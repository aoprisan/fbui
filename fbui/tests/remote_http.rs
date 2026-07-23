//! Headless end-to-end tests for the remote console server: a real TCP
//! listener on an ephemeral port, a fake "UI thread" servicing the hub, and a
//! raw `TcpStream` client — no platform, no display, no browser.
#![cfg(feature = "remote")]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

use fbui::remote::{Command, Hub, RemoteConfig};

/// Spawn a server plus a service thread that plays the runner's role:
/// woken by the hub, it drains commands, answers Inspect with a canned JSON,
/// publishes a frame on RefreshFrame, and counts injected input.
fn server(token: Option<&str>) -> (Arc<Hub>, SocketAddr, Arc<AtomicU64>) {
    let cfg = RemoteConfig {
        addr: "127.0.0.1:0".parse().unwrap(),
        token: token.map(String::from),
    };
    let (hub, addr) = cfg.spawn().expect("bind ephemeral port");
    let inputs = Arc::new(AtomicU64::new(0));

    let (tx, rx) = mpsc::channel();
    hub.set_waker(move || {
        let _ = tx.send(());
    });
    let h = hub.clone();
    let n = inputs.clone();
    std::thread::spawn(move || {
        // Exit when the hub is dropped by the test (sender gone → recv errs
        // only when the waker is dropped; bound the wait instead).
        while rx.recv_timeout(Duration::from_secs(30)).is_ok() {
            let _ = h.take_refresh();
            for cmd in h.take_commands() {
                match cmd {
                    Command::Inspect { reply } => {
                        let _ = reply.send(r#"{"scale":1,"tree":{"name":"Root"}}"#.to_string());
                    }
                    Command::RefreshFrame => {
                        // A 2x1 frame: red pixel then blue pixel.
                        h.publish_frame(2, 1, vec![255, 0, 0, 255, 0, 0, 255, 255]);
                    }
                    Command::Text { text } => {
                        n.fetch_add(text.chars().count() as u64, Ordering::SeqCst);
                    }
                    _ => {
                        n.fetch_add(1, Ordering::SeqCst);
                    }
                }
            }
        }
    });
    (hub, addr, inputs)
}

/// Issue one HTTP request, returning (status, headers, body).
fn request(addr: SocketAddr, method: &str, target: &str) -> (u16, String, Vec<u8>) {
    let mut s = TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    write!(
        s,
        "{method} {target} HTTP/1.1\r\nHost: t\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    let mut r = BufReader::new(s);
    let mut status_line = String::new();
    r.read_line(&mut status_line).unwrap();
    let status: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .expect("status code");
    let mut headers = String::new();
    loop {
        let mut line = String::new();
        r.read_line(&mut line).unwrap();
        if line.trim_end().is_empty() {
            break;
        }
        headers.push_str(&line);
    }
    let mut body = Vec::new();
    r.read_to_end(&mut body).unwrap();
    (status, headers, body)
}

#[test]
fn console_html_is_served_at_root() {
    let (_hub, addr, _) = server(None);
    let (status, headers, body) = request(addr, "GET", "/");
    assert_eq!(status, 200);
    assert!(headers.to_lowercase().contains("text/html"));
    assert!(String::from_utf8_lossy(&body).contains("fbui remote console"));
}

#[test]
fn metrics_endpoint_reports_counters() {
    let (hub, addr, _) = server(None);
    hub.record_frame(3.5);
    hub.set_size(320, 240);
    let (status, _, body) = request(addr, "GET", "/metrics");
    assert_eq!(status, 200);
    let text = String::from_utf8_lossy(&body).to_string();
    assert!(text.contains("fbui_frames_total 1\n"), "{text}");
    assert!(text.contains("fbui_paint_milliseconds 3.5\n"));
    assert!(text.contains("fbui_surface_pixels{axis=\"width\"} 320\n"));
}

#[test]
fn screen_png_round_trips_the_published_frame() {
    let (_hub, addr, _) = server(None);
    let (status, headers, body) = request(addr, "GET", "/screen.png");
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
    assert!(headers.to_lowercase().contains("image/png"));
    let img = image::load_from_memory(&body).unwrap().to_rgba8();
    assert_eq!(img.dimensions(), (2, 1));
    assert_eq!(img.get_pixel(0, 0).0, [255, 0, 0, 255]);
    assert_eq!(img.get_pixel(1, 0).0, [0, 0, 255, 255]);
}

#[test]
fn tree_endpoint_returns_the_ui_threads_json() {
    let (_hub, addr, _) = server(None);
    let (status, headers, body) = request(addr, "GET", "/tree");
    assert_eq!(status, 200);
    assert!(headers.to_lowercase().contains("application/json"));
    assert_eq!(
        String::from_utf8_lossy(&body),
        r#"{"scale":1,"tree":{"name":"Root"}}"#
    );
}

#[test]
fn input_injection_queues_commands() {
    let (hub, addr, inputs) = server(None);
    let (status, _, _) = request(addr, "POST", "/input?type=tap&x=10&y=20.5");
    assert_eq!(status, 204);
    let (status, _, _) = request(addr, "POST", "/input?type=key&key=Enter");
    assert_eq!(status, 204);
    let (status, _, _) = request(addr, "POST", "/input?type=text&text=hi%20there");
    assert_eq!(status, 204);
    // tap = down+up (2) + key (1) + "hi there" (8 chars).
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while inputs.load(Ordering::SeqCst) < 11 && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(inputs.load(Ordering::SeqCst), 11);
    let _ = hub;

    // Malformed requests are rejected, not silently dropped.
    let (status, _, body) = request(addr, "POST", "/input?type=tap&x=abc&y=1");
    assert_eq!(status, 400, "{}", String::from_utf8_lossy(&body));
    let (status, _, _) = request(addr, "POST", "/input?type=warp");
    assert_eq!(status, 400);
}

#[test]
fn token_gates_every_endpoint() {
    let (_hub, addr, inputs) = server(Some("s3cret"));
    let (status, _, _) = request(addr, "GET", "/metrics");
    assert_eq!(status, 401);
    let (status, _, _) = request(addr, "POST", "/input?type=key&key=a");
    assert_eq!(status, 401);
    assert_eq!(
        inputs.load(Ordering::SeqCst),
        0,
        "rejected input never runs"
    );

    let (status, _, _) = request(addr, "GET", "/metrics?token=s3cret");
    assert_eq!(status, 200);

    // Bearer-header form.
    let mut s = TcpStream::connect(addr).unwrap();
    write!(
        s,
        "GET /metrics HTTP/1.1\r\nHost: t\r\nAuthorization: Bearer s3cret\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    let mut line = String::new();
    BufReader::new(s).read_line(&mut line).unwrap();
    assert!(line.contains("200"), "{line}");
}

#[test]
fn unknown_path_is_404() {
    let (_hub, addr, _) = server(None);
    let (status, _, _) = request(addr, "GET", "/nope");
    assert_eq!(status, 404);
}

#[test]
fn stream_delivers_frames_as_multipart_parts() {
    let (hub, addr, _) = server(None);
    let mut s = TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    write!(s, "GET /stream HTTP/1.1\r\nHost: t\r\n\r\n").unwrap();

    let mut r = BufReader::new(s.try_clone().unwrap());
    let mut head = String::new();
    loop {
        let mut line = String::new();
        r.read_line(&mut line).unwrap();
        if line.trim_end().is_empty() {
            break;
        }
        head.push_str(&line);
    }
    assert!(head.contains("multipart/x-mixed-replace"), "{head}");

    // The connect kicked a RefreshFrame; the service thread published. Read
    // until the first PNG part header appears.
    let mut seen = String::new();
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !seen.contains("Content-Type: image/png") {
        assert!(
            std::time::Instant::now() < deadline,
            "no frame part: {seen}"
        );
        let mut line = String::new();
        if r.read_line(&mut line).unwrap() == 0 {
            panic!("stream closed early: {seen}");
        }
        seen.push_str(&line);
    }
    assert_eq!(hub.watchers(), 1, "stream client counted as watcher");
    drop(r);
    drop(s);
}
