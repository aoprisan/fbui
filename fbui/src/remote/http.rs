//! A deliberately small HTTP/1.1 server for the remote console.
//!
//! Hand-rolled over `std::net` — no async runtime, no HTTP crate — because the
//! whole surface is six endpoints serving one operator, and a UI framework
//! must not drag a server stack into every kiosk build. One thread accepts;
//! each connection gets a short-lived thread (bounded by
//! [`MAX_CONNECTIONS`]); every response closes the connection except the
//! multipart frame stream.
//!
//! Security model: **off unless configured**, binds loopback unless the
//! operator explicitly gives an address, and an optional bearer token
//! (`FBUI_REMOTE_TOKEN`) is required on every request when set. Input
//! injection is real input — treat the port like an SSH port.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::hub::{Command, Hub, RemoteButton};
use super::json::metrics_text;

/// Open connections beyond this are refused with 503 — the console is an
/// operator tool, not a public site, and fd exhaustion on a kiosk is fatal.
const MAX_CONNECTIONS: usize = 16;
/// Bytes of request line + headers we are willing to read.
const MAX_HEAD: usize = 16 * 1024;
/// Bytes of request body we are willing to read.
const MAX_BODY: usize = 64 * 1024;
/// How long a `/screen.png` or `/tree` request waits for the UI thread.
const UI_REPLY_TIMEOUT: Duration = Duration::from_secs(2);
/// Frame stream pacing: encode at most this often per client.
const STREAM_MIN_INTERVAL: Duration = Duration::from_millis(66);

/// The embedded single-file web console served at `/`.
const CONSOLE_HTML: &str = include_str!("console.html");

/// Start serving on `addr`. Returns the bound address (so port `0` works in
/// tests). The listener thread holds only weak-ish state: everything it needs
/// lives in the shared [`Hub`].
pub fn spawn(
    hub: Arc<Hub>,
    addr: SocketAddr,
    token: Option<String>,
) -> std::io::Result<SocketAddr> {
    let listener = TcpListener::bind(addr)?;
    let local = listener.local_addr()?;
    let token = Arc::new(token);
    std::thread::Builder::new()
        .name("fbui-remote".into())
        .spawn(move || {
            for conn in listener.incoming() {
                let Ok(stream) = conn else { continue };
                let hub = hub.clone();
                let token = token.clone();
                if hub.clients.fetch_add(1, Ordering::Relaxed) >= MAX_CONNECTIONS {
                    hub.clients.fetch_sub(1, Ordering::Relaxed);
                    let _ = respond_simple(&stream, 503, "busy");
                    continue;
                }
                let _ = std::thread::Builder::new()
                    .name("fbui-remote-conn".into())
                    .spawn(move || {
                        let _ = serve_connection(&hub, stream, token.as_deref());
                        hub.clients.fetch_sub(1, Ordering::Relaxed);
                    });
            }
        })?;
    Ok(local)
}

struct Request {
    method: String,
    path: String,
    /// Decoded query parameters, in order.
    query: Vec<(String, String)>,
    /// Lower-cased header names.
    headers: Vec<(String, String)>,
}

impl Request {
    fn param(&self, name: &str) -> Option<&str> {
        self.query
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
}

fn serve_connection(hub: &Hub, stream: TcpStream, token: Option<&str>) -> std::io::Result<()> {
    // A stalled or vanished client must not pin the thread forever.
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    stream.set_write_timeout(Some(Duration::from_secs(10)))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let Some(req) = parse_request(&mut reader)? else {
        return respond_simple(&stream, 400, "bad request");
    };

    if !authorized(&req, token) {
        return respond_simple(&stream, 401, "missing or wrong token (FBUI_REMOTE_TOKEN)");
    }

    match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/") | ("GET", "/index.html") => respond(
            &stream,
            200,
            "text/html; charset=utf-8",
            CONSOLE_HTML.as_bytes(),
        ),
        ("GET", "/screen.png") => screen_png(hub, &stream),
        ("GET", "/stream") => stream_frames(hub, &stream),
        ("GET", "/tree") => tree(hub, &stream),
        ("GET", "/metrics") => {
            let text = metrics_text(&hub.metrics());
            respond(&stream, 200, "text/plain; version=0.0.4", text.as_bytes())
        }
        ("POST", "/input") => input(hub, &req, &stream),
        _ => respond_simple(&stream, 404, "not found"),
    }
}

/// Parse the request head (and drain any body — we never need bodies, input
/// arrives as query parameters, but the socket must be read past it).
fn parse_request(reader: &mut BufReader<TcpStream>) -> std::io::Result<Option<Request>> {
    let mut line = String::new();
    let mut head_bytes = 0usize;
    if read_head_line(reader, &mut line, &mut head_bytes)? == 0 {
        return Ok(None);
    }
    let mut parts = line.split_whitespace();
    let (Some(method), Some(target)) = (parts.next(), parts.next()) else {
        return Ok(None);
    };
    let (path, query_str) = match target.split_once('?') {
        Some((p, q)) => (p, q),
        None => (target, ""),
    };
    let query = query_str
        .split('&')
        .filter(|s| !s.is_empty())
        .map(|pair| {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            (percent_decode(k), percent_decode(v))
        })
        .collect();
    let (method, path) = (method.to_string(), path.to_string());

    let mut headers = Vec::new();
    let mut content_len = 0usize;
    loop {
        line.clear();
        if read_head_line(reader, &mut line, &mut head_bytes)? == 0 {
            return Ok(None);
        }
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            break;
        }
        if let Some((k, v)) = trimmed.split_once(':') {
            let k = k.trim().to_ascii_lowercase();
            let v = v.trim().to_string();
            if k == "content-length" {
                content_len = v.parse().unwrap_or(0);
            }
            headers.push((k, v));
        }
    }
    if content_len > MAX_BODY {
        return Ok(None);
    }
    // Drain the body so the connection is in a sane state to respond on.
    let mut body = vec![0u8; content_len];
    reader.read_exact(&mut body)?;

    Ok(Some(Request {
        method,
        path,
        query,
        headers,
    }))
}

/// Read one CRLF-terminated head line, enforcing the total head budget.
fn read_head_line(
    reader: &mut BufReader<TcpStream>,
    line: &mut String,
    total: &mut usize,
) -> std::io::Result<usize> {
    let n = reader.read_line(line)?;
    *total += n;
    if *total > MAX_HEAD {
        return Ok(0);
    }
    Ok(n)
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
                match hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                    Some(b) => {
                        out.push(b);
                        i += 3;
                    }
                    None => {
                        out.push(b'%');
                        i += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn authorized(req: &Request, token: Option<&str>) -> bool {
    let Some(token) = token else { return true };
    if req.param("token") == Some(token) {
        return true;
    }
    req.header("authorization")
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::trim)
        == Some(token)
}

// ---- endpoint handlers ---------------------------------------------------

fn screen_png(hub: &Hub, stream: &TcpStream) -> std::io::Result<()> {
    // Ask the UI thread for current pixels even if the app is idle, then wait
    // briefly; fall back to whatever was last published.
    let before = hub.latest_frame().map(|f| f.seq).unwrap_or(0);
    hub.push(Command::RefreshFrame);
    let frame = hub
        .wait_frame(before, UI_REPLY_TIMEOUT)
        .or_else(|| hub.latest_frame());
    let Some(f) = frame else {
        return respond_simple(stream, 503, "no frame yet (is the app painting?)");
    };
    match fbui_render::encode_png_rgba(f.width, f.height, &f.rgba) {
        Ok(png) => respond(stream, 200, "image/png", &png),
        Err(e) => respond_simple(stream, 500, &e),
    }
}

fn stream_frames(hub: &Hub, stream: &TcpStream) -> std::io::Result<()> {
    // `multipart/x-mixed-replace`: the MJPEG trick, with PNG parts — every
    // browser renders it in a plain <img>, and damage-driven apps mean parts
    // arrive only when something changed.
    hub.add_watcher();
    let r = stream_frames_inner(hub, stream);
    hub.remove_watcher();
    r
}

fn stream_frames_inner(hub: &Hub, mut stream: &TcpStream) -> std::io::Result<()> {
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: multipart/x-mixed-replace; boundary=fbuiframe\r\n\
         Cache-Control: no-store\r\nConnection: close\r\n\r\n"
    )?;
    // Kick the UI thread so the first frame arrives without waiting for damage.
    hub.push(Command::RefreshFrame);
    let mut seq = 0u64;
    let mut last_sent = Instant::now() - STREAM_MIN_INTERVAL;
    loop {
        // Wake periodically even with no frames so a vanished client is
        // noticed (the write below fails) instead of pinned forever.
        let Some(f) = hub.wait_frame(seq, Duration::from_secs(2)) else {
            // Heartbeat: an empty part keeps proxies from timing out and
            // detects a closed socket.
            write!(stream, "--fbuiframe\r\nContent-Length: 0\r\n\r\n")?;
            stream.flush()?;
            continue;
        };
        // Pace the encode: a fast animation would otherwise burn a core
        // PNG-encoding every frame per client.
        let since = last_sent.elapsed();
        if since < STREAM_MIN_INTERVAL {
            std::thread::sleep(STREAM_MIN_INTERVAL - since);
        }
        // Encode the *latest* frame at send time, skipping any backlog.
        let f = hub.latest_frame().unwrap_or(f);
        seq = f.seq;
        let png = match fbui_render::encode_png_rgba(f.width, f.height, &f.rgba) {
            Ok(p) => p,
            Err(_) => continue,
        };
        write!(
            stream,
            "--fbuiframe\r\nContent-Type: image/png\r\nContent-Length: {}\r\n\r\n",
            png.len()
        )?;
        stream.write_all(&png)?;
        write!(stream, "\r\n")?;
        stream.flush()?;
        last_sent = Instant::now();
    }
}

fn tree(hub: &Hub, stream: &TcpStream) -> std::io::Result<()> {
    let (tx, rx) = mpsc::sync_channel(1);
    hub.push(Command::Inspect { reply: tx });
    match rx.recv_timeout(UI_REPLY_TIMEOUT) {
        Ok(json) => respond(stream, 200, "application/json", json.as_bytes()),
        Err(_) => respond_simple(stream, 504, "UI thread did not reply"),
    }
}

fn input(hub: &Hub, req: &Request, stream: &TcpStream) -> std::io::Result<()> {
    let xy = || -> Option<(f32, f32)> {
        Some((req.param("x")?.parse().ok()?, req.param("y")?.parse().ok()?))
    };
    let button = match req.param("button").unwrap_or("left") {
        "left" => RemoteButton::Left,
        "middle" => RemoteButton::Middle,
        "right" => RemoteButton::Right,
        other => return respond_simple(stream, 400, &format!("unknown button {other:?}")),
    };
    let cmds: Vec<Command> = match req.param("type") {
        Some("move") => match xy() {
            Some((x, y)) => vec![Command::PointerMove { x, y }],
            None => return bad_coords(stream),
        },
        Some("down") => match xy() {
            Some((x, y)) => vec![Command::PointerDown { x, y, button }],
            None => return bad_coords(stream),
        },
        Some("up") => match xy() {
            Some((x, y)) => vec![Command::PointerUp { x, y, button }],
            None => return bad_coords(stream),
        },
        Some("tap") => match xy() {
            Some((x, y)) => vec![
                Command::PointerDown { x, y, button },
                Command::PointerUp { x, y, button },
            ],
            None => return bad_coords(stream),
        },
        Some("wheel") => {
            let (Some((x, y)), Some(dy)) = (xy(), req.param("dy").and_then(|d| d.parse().ok()))
            else {
                return respond_simple(stream, 400, "wheel needs x, y, dy");
            };
            vec![Command::Wheel { x, y, dy }]
        }
        Some("key") => match req.param("key") {
            Some(k) if !k.is_empty() => vec![Command::Key {
                name: k.to_string(),
            }],
            _ => return respond_simple(stream, 400, "key needs key=<char or name>"),
        },
        Some("text") => match req.param("text") {
            Some(t) => vec![Command::Text {
                text: t.to_string(),
            }],
            None => return respond_simple(stream, 400, "text needs text=..."),
        },
        other => {
            return respond_simple(
                stream,
                400,
                &format!("unknown input type {other:?} (move|down|up|tap|wheel|key|text)"),
            )
        }
    };
    for c in cmds {
        hub.push(c);
    }
    respond_simple(stream, 204, "")
}

fn bad_coords(stream: &TcpStream) -> std::io::Result<()> {
    respond_simple(stream, 400, "needs numeric x and y")
}

// ---- response helpers ----------------------------------------------------

fn status_text(code: u16) -> &'static str {
    match code {
        200 => "OK",
        204 => "No Content",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        _ => "",
    }
}

fn respond(mut stream: &TcpStream, code: u16, ctype: &str, body: &[u8]) -> std::io::Result<()> {
    write!(
        stream,
        "HTTP/1.1 {code} {}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\n\
         Cache-Control: no-store\r\nConnection: close\r\n\r\n",
        status_text(code),
        body.len()
    )?;
    stream.write_all(body)?;
    stream.flush()
}

fn respond_simple(stream: &TcpStream, code: u16, msg: &str) -> std::io::Result<()> {
    if code == 204 {
        let mut s = stream;
        write!(
            s,
            "HTTP/1.1 204 No Content\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n"
        )?;
        return s.flush();
    }
    respond(stream, code, "text/plain; charset=utf-8", msg.as_bytes())
}
