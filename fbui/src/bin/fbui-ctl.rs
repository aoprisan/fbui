//! `fbui-ctl` — drive a live fbui app from a shell over the remote console.
//!
//! ```sh
//! export FBUI_CTL=http://localhost:8433    # token via FBUI_REMOTE_TOKEN
//! fbui-ctl tree                            # the widget tree as text
//! fbui-ctl shot out.png                    # a screenshot
//! fbui-ctl tap '#inc'                      # resolve via /tree, then POST /input
//! fbui-ctl type "milk" && fbui-ctl key Enter
//! fbui-ctl run flows/add-item.txt          # the third executor
//! fbui-ctl trace --follow
//! ```
//!
//! This is the **third executor** of `docs/tooling.md`: the same flow file
//! that a `cargo test` runs in-process and CI runs headless can be pointed at
//! a kiosk in the field. Reproduce there, run the flow headless in CI, commit
//! it.
//!
//! Everything it needs already exists server-side: `/tree` to resolve
//! references, `/input` to inject, `/screen.png`, `/tree.txt`, `/trace`. The
//! client is `std::net` only, like the console it talks to.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use fbui::remote::parse_tree;
use fbui::script::{self, Act, Executor, Snapshot, Target};

const USAGE: &str = "\
fbui-ctl — drive a live fbui app over the remote console

USAGE:
    fbui-ctl <command> [args]

COMMANDS:
    tree                  print the widget tree as text (GET /tree.txt)
    json                  print the widget tree as JSON (GET /tree)
    shot <file.png>       save a screenshot (GET /screen.png)
    tap <ref>             tap a widget: '#name', 'Kind \"text\"', or '@x,y'
    press|release <ref>   one half of a contact
    move <ref>            move the pointer
    wheel <ref> <n>       scroll n notches
    type <text>           type a string
    key <spec>            press a key: Enter, Ctrl+C, Shift+Tab, a
    run <flow.txt>        run a flow script against the device
    trace [--follow]      print the event trace (GET /trace)
    metrics               print the Prometheus metrics

ENVIRONMENT:
    FBUI_CTL              base URL (default http://127.0.0.1:8433)
    FBUI_REMOTE_TOKEN     token, when the console requires one
    FBUI_CTL_STEP         ms between flow steps (default 120)
";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() || args[0] == "-h" || args[0] == "--help" {
        print!("{USAGE}");
        std::process::exit(if args.is_empty() { 2 } else { 0 });
    }
    if let Err(e) = run(&args) {
        eprintln!("fbui-ctl: {e}");
        std::process::exit(1);
    }
}

fn run(args: &[String]) -> Result<(), String> {
    let client = Client::from_env()?;
    match args[0].as_str() {
        "tree" => print!("{}", client.get_text("/tree.txt")?),
        "json" => println!("{}", client.get_text("/tree")?),
        "metrics" => print!("{}", client.get_text("/metrics")?),
        "shot" => {
            let path = arg(args, 1, "shot needs a file name")?;
            let png = client.get("/screen.png")?;
            std::fs::write(path, png).map_err(|e| format!("{path}: {e}"))?;
            eprintln!("wrote {path}");
        }
        "trace" => {
            let follow = args.iter().any(|a| a == "--follow" || a == "-f");
            follow_trace(&client, follow)?;
        }
        "tap" | "press" | "release" | "move" => {
            let r = parse_ref(arg(args, 1, "needs a widget reference")?)?;
            let (x, y) = client.resolve(&r)?;
            let kind = if args[0] == "move" { "move" } else { &args[0] };
            client.input(&[("type", kind), ("x", &fmt(x)), ("y", &fmt(y))])?;
        }
        "wheel" => {
            let r = parse_ref(arg(args, 1, "wheel needs a widget reference")?)?;
            let n: f32 = arg(args, 2, "wheel needs a notch count")?
                .parse()
                .map_err(|_| "wheel needs a number of notches".to_string())?;
            let (x, y) = client.resolve(&r)?;
            // The console's `dy` is browser-style (positive scrolls content
            // down); a flow's notches are wheel-style, like the platform's.
            client.input(&[
                ("type", "wheel"),
                ("x", &fmt(x)),
                ("y", &fmt(y)),
                ("dy", &fmt(-n)),
            ])?;
        }
        "type" => {
            let text = arg(args, 1, "type needs a string")?;
            client.input(&[("type", "text"), ("text", text)])?;
        }
        "key" => {
            let spec = arg(args, 1, "key needs a key name")?;
            client.input(&[("type", "key"), ("key", spec)])?;
        }
        "run" => run_flow(&client, arg(args, 1, "run needs a flow file")?)?,
        other => return Err(format!("unknown command {other:?}\n\n{USAGE}")),
    }
    Ok(())
}

fn arg<'a>(args: &'a [String], i: usize, msg: &str) -> Result<&'a str, String> {
    args.get(i)
        .map(String::as_str)
        .ok_or_else(|| msg.to_string())
}

/// Trim a float for a query parameter: `240` rather than `240.00000`.
fn fmt(v: f32) -> String {
    if v == v.trunc() {
        format!("{}", v as i64)
    } else {
        format!("{v:.2}")
    }
}

/// Parse a reference the way a flow does, so the shell and a flow file agree.
fn parse_ref(text: &str) -> Result<script::Ref, String> {
    // Reuse the flow parser rather than a second syntax: a one-step flow.
    let s = script::parse(&format!("tap {text}\n")).map_err(|e| e.to_string())?;
    match s.steps.into_iter().next() {
        Some(script::Step::Tap(r)) => Ok(r),
        _ => Err(format!("{text:?} is not a widget reference")),
    }
}

// ---- the flow executor over HTTP ------------------------------------------

fn run_flow(client: &Client, path: &str) -> Result<(), String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
    let script = script::parse(&text).map_err(|e| format!("{path}: {e}"))?;
    if script.has_raw {
        return Err(format!(
            "{path} contains raw `@ms` event lines, which only the runner can \
             replay (run it with FBUI_REPLAY on the device)"
        ));
    }
    let step_ms: u64 = std::env::var("FBUI_CTL_STEP")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(120);
    let mut exec = Executor::new(script);
    loop {
        // The lint pass runs inside the `Ui`, and the console has no endpoint
        // for it. Say so rather than passing the expectation vacuously — a
        // green flow that checked nothing is worse than a red one.
        if exec.wants_lints() {
            return Err(format!(
                "{path}: `expect no-lints` needs the lint pass, which runs on the \
                 device; run this flow with FBUI_REPLAY there, or headless in CI"
            ));
        }
        let tree = client.tree()?;
        let Some(act) = exec.advance(Snapshot::new(Some(&tree))) else {
            break;
        };
        perform(client, act, step_ms)?;
        std::thread::sleep(Duration::from_millis(step_ms));
    }
    let failures = exec.failures();
    if failures.is_empty() {
        let (done, total) = exec.position();
        eprintln!("{path}: {done}/{total} steps ok");
        return Ok(());
    }
    for f in failures {
        eprintln!("{f}");
    }
    eprintln!("\ntree at failure:\n{}", client.get_text("/tree.txt")?);
    Err(format!("{path}: flow failed"))
}

fn perform(client: &Client, act: Act, step_ms: u64) -> Result<(), String> {
    let at = |x: f32, y: f32| [("x".to_string(), fmt(x)), ("y".to_string(), fmt(y))];
    match act {
        Act::Tap(p) => client.input_owned("tap", &at(p.x, p.y), &[])?,
        Act::Press(p) => client.input_owned("down", &at(p.x, p.y), &[])?,
        Act::Release(p) => {
            let p = p.ok_or("a bare `release` needs a preceding position over HTTP")?;
            client.input_owned("up", &at(p.x, p.y), &[])?;
        }
        Act::Move(p) => client.input_owned("move", &at(p.x, p.y), &[])?,
        Act::LongPress(p) => {
            client.input_owned("down", &at(p.x, p.y), &[])?;
            // The device's own recognizer times the hold, so wait it out.
            std::thread::sleep(Duration::from_millis(700));
            client.input_owned("up", &at(p.x, p.y), &[])?;
        }
        Act::Drag { from, dx, dy, fast } => {
            client.input_owned("down", &at(from.x, from.y), &[])?;
            let steps = 8;
            for i in 1..=steps {
                let f = i as f32 / steps as f32;
                client.input_owned("move", &at(from.x + dx * f, from.y + dy * f), &[])?;
                if !fast {
                    std::thread::sleep(Duration::from_millis(40));
                }
            }
            if !fast {
                // Pause before lifting, so the device reads a stop, not a fling.
                std::thread::sleep(Duration::from_millis(200));
            }
            client.input_owned("up", &at(from.x + dx, from.y + dy), &[])?;
        }
        Act::Wheel { at: p, notches } => {
            client.input_owned("wheel", &at(p.x, p.y), &[("dy".to_string(), fmt(-notches))])?
        }
        Act::Text(t) => client.input_owned("text", &[], &[("text".to_string(), t)])?,
        Act::Key(spec) => client.input_owned("key", &[], &[("key".to_string(), key_name(spec))])?,
        // There is no frame clock to advance from here; the device has its
        // own, so a settle is a wall-clock wait long enough for a transition.
        Act::WaitSettle => std::thread::sleep(Duration::from_millis(400)),
        Act::WaitMs(ms) => std::thread::sleep(Duration::from_millis(ms)),
        Act::Shot(path) => {
            std::thread::sleep(Duration::from_millis(step_ms));
            let png = client.get("/screen.png")?;
            std::fs::write(&path, png).map_err(|e| format!("{}: {e}", path.display()))?;
        }
        Act::Tree(path) => {
            std::thread::sleep(Duration::from_millis(step_ms));
            let text = client.get_text("/tree.txt")?;
            std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))?;
        }
        Act::Raw { .. } => return Err("raw event lines need the device's runner".into()),
    }
    Ok(())
}

/// The console's key-name vocabulary for one parsed key spec. Modifiers are
/// not expressible over `/input` today, so a chord is reported rather than
/// silently sent without them.
fn key_name(spec: script::KeySpec) -> String {
    use fbui::Key;
    let name = match spec.key {
        Key::Enter => "Enter".to_string(),
        Key::Tab => "Tab".to_string(),
        Key::Escape => "Escape".to_string(),
        Key::Space => "Space".to_string(),
        Key::Backspace => "Backspace".to_string(),
        Key::Delete => "Delete".to_string(),
        Key::Home => "Home".to_string(),
        Key::End => "End".to_string(),
        Key::Left => "Left".to_string(),
        Key::Right => "Right".to_string(),
        Key::Up => "Up".to_string(),
        Key::Down => "Down".to_string(),
        Key::Char(c) => c.to_string(),
        other => format!("{other:?}"),
    };
    if spec.mods.ctrl || spec.mods.alt || spec.mods.shift {
        eprintln!(
            "fbui-ctl: warning: the console has no modifier channel; sending {name:?} \
             without them"
        );
    }
    name
}

fn follow_trace(client: &Client, follow: bool) -> Result<(), String> {
    let mut seen = 0usize;
    loop {
        let text = client.get_text("/trace")?;
        let lines: Vec<&str> = text.lines().collect();
        // The tail is bounded, so it can shrink from the front; print only
        // what is new by count, which is right unless the tail overflowed
        // between polls.
        for l in lines.iter().skip(seen) {
            println!("{l}");
        }
        seen = lines.len();
        if !follow {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

// ---- the HTTP client ------------------------------------------------------

struct Client {
    host: String,
    port: u16,
    token: Option<String>,
}

impl Client {
    fn from_env() -> Result<Self, String> {
        let base = std::env::var("FBUI_CTL").unwrap_or_else(|_| "http://127.0.0.1:8433".into());
        let rest = base
            .strip_prefix("http://")
            .ok_or_else(|| format!("FBUI_CTL {base:?}: expected http://host:port"))?;
        let rest = rest.trim_end_matches('/');
        let (host, port) = match rest.rsplit_once(':') {
            Some((h, p)) => (
                h.to_string(),
                p.parse()
                    .map_err(|_| format!("FBUI_CTL {base:?}: bad port"))?,
            ),
            None => (rest.to_string(), 80),
        };
        Ok(Client {
            host,
            port,
            token: std::env::var("FBUI_REMOTE_TOKEN")
                .ok()
                .filter(|t| !t.is_empty()),
        })
    }

    fn connect(&self) -> Result<TcpStream, String> {
        let s = TcpStream::connect((self.host.as_str(), self.port)).map_err(|e| {
            format!(
                "connect {}:{}: {e} (is the app running with FBUI_REMOTE set?)",
                self.host, self.port
            )
        })?;
        let _ = s.set_read_timeout(Some(Duration::from_secs(15)));
        let _ = s.set_write_timeout(Some(Duration::from_secs(15)));
        Ok(s)
    }

    fn request(&self, method: &str, path: &str) -> Result<Vec<u8>, String> {
        let mut s = self.connect()?;
        let auth = match &self.token {
            Some(t) => format!("Authorization: Bearer {t}\r\n"),
            None => String::new(),
        };
        let req = format!(
            "{method} {path} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n{auth}\r\n",
            self.host
        );
        s.write_all(req.as_bytes()).map_err(|e| e.to_string())?;
        let mut reader = BufReader::new(s);
        let mut status = String::new();
        reader.read_line(&mut status).map_err(|e| e.to_string())?;
        let code: u16 = status
            .split_whitespace()
            .nth(1)
            .and_then(|c| c.parse().ok())
            .ok_or_else(|| format!("bad response {status:?}"))?;
        // Drain headers.
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).map_err(|e| e.to_string())? == 0 {
                break;
            }
            if line.trim_end().is_empty() {
                break;
            }
        }
        let mut body = Vec::new();
        reader.read_to_end(&mut body).map_err(|e| e.to_string())?;
        // `/input` answers 204; anything 2xx is success.
        if !(200..300).contains(&code) {
            return Err(format!(
                "{method} {path} → {code}: {}",
                String::from_utf8_lossy(&body).trim()
            ));
        }
        Ok(body)
    }

    fn get(&self, path: &str) -> Result<Vec<u8>, String> {
        self.request("GET", path)
    }

    fn get_text(&self, path: &str) -> Result<String, String> {
        Ok(String::from_utf8_lossy(&self.get(path)?).to_string())
    }

    fn input(&self, params: &[(&str, &str)]) -> Result<(), String> {
        let owned: Vec<(String, String)> = params
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        self.post_input(&owned)
    }

    fn input_owned(
        &self,
        kind: &str,
        a: &[(String, String)],
        b: &[(String, String)],
    ) -> Result<(), String> {
        let mut params = vec![("type".to_string(), kind.to_string())];
        params.extend_from_slice(a);
        params.extend_from_slice(b);
        self.post_input(&params)
    }

    fn post_input(&self, params: &[(String, String)]) -> Result<(), String> {
        let query: Vec<String> = params
            .iter()
            .map(|(k, v)| format!("{}={}", encode(k), encode(v)))
            .collect();
        self.request("POST", &format!("/input?{}", query.join("&")))?;
        Ok(())
    }

    /// The live tree, parsed back from `/tree`.
    fn tree(&self) -> Result<fbui::InspectNode, String> {
        let doc = self.get_text("/tree")?;
        parse_tree(&doc).ok_or_else(|| "the device returned no widget tree".to_string())
    }

    /// A reference resolved against the live tree, as logical coordinates.
    fn resolve(&self, r: &script::Ref) -> Result<(f32, f32), String> {
        if let script::Ref::At(p) = r {
            return Ok((p.x, p.y));
        }
        let tree = self.tree()?;
        let found = script::matches(&tree, r);
        match found.first() {
            Some(n) if !n.visible => Err(format!(
                "{r} is in the tree but not on screen (clipped or off-surface)"
            )),
            Some(n) => {
                let p = Target::Node(n).point();
                Ok((p.x, p.y))
            }
            None => Err(format!("{r} matches no widget")),
        }
    }
}

/// Percent-encode a query value (the console decodes `%XX` and `+`).
fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}
