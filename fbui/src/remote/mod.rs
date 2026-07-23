//! The **remote console** (feature `remote`): every fbui app becomes remotely
//! observable and operable over HTTP — a live view of the screen in any
//! browser, input injection through the exact live event path, a widget-tree
//! inspector, and Prometheus metrics. No X11, no VNC, no extra dependencies:
//! the server is hand-rolled over `std::net` and the console is one embedded
//! HTML file.
//!
//! This is a fleet tool: a kiosk in the field has no keyboard and no second
//! screen, but it usually has a network. `ssh -L 8433:localhost:8433 kiosk`
//! then <http://localhost:8433> shows you what the device shows and lets you
//! drive it.
//!
//! ## Enabling
//!
//! Nothing runs unless the operator asks. With the `remote` + `platform`
//! features compiled in, the runner reads:
//!
//! * `FBUI_REMOTE` — a port (`8433`, binds `127.0.0.1`) or a full socket
//!   address (`0.0.0.0:8433`). Loopback unless you explicitly bind wider.
//! * `FBUI_REMOTE_TOKEN` — optional bearer token; when set, every request
//!   must carry `?token=…` or `Authorization: Bearer …`.
//!
//! A failed bind is a hard startup error — a session you believe is remotely
//! reachable but isn't is worse than one that fails loudly.
//!
//! ## Endpoints
//!
//! | Endpoint | What |
//! |---|---|
//! | `GET /` | the embedded web console |
//! | `GET /screen.png` | current frame as PNG (works while idle) |
//! | `GET /stream` | multipart PNG stream, damage-driven |
//! | `GET /tree` | widget-tree snapshot as JSON ([`Ui::inspect`](fbui_widgets::Ui::inspect)) |
//! | `GET /metrics` | Prometheus text format |
//! | `POST /input` | inject input (`type=tap&x=…&y=…`, `type=key&key=Enter`, `type=text&text=…`, …) |
//!
//! Injected input flows through the same path as live input — gestures fire,
//! focus moves, `FBUI_RECORD` captures it, and `Escape` exits the app just as
//! it does on the device.
//!
//! **Security**: input injection is remote control. The server binds loopback
//! by default; anything wider should sit behind the token *and* a network you
//! trust (or an SSH tunnel). See `docs/remote-console.md`.
//!
//! The pieces are headless-testable: the [`Hub`] carries frames, commands and
//! metrics between the UI thread and the server without knowing either side.

mod http;
mod hub;
mod json;

use std::net::SocketAddr;
use std::sync::Arc;

pub use hub::{Command, FrameImage, Hub, MetricsSnapshot, RemoteButton};
pub use json::tree_json;

/// Where and how to serve. Built from the environment by
/// [`RemoteConfig::from_env`], or directly for embedding/tests.
#[derive(Debug, Clone)]
pub struct RemoteConfig {
    pub addr: SocketAddr,
    /// Required on every request when set.
    pub token: Option<String>,
}

impl RemoteConfig {
    /// Read `FBUI_REMOTE` / `FBUI_REMOTE_TOKEN`. `Ok(None)` when unset (the
    /// normal case); `Err` when set but unparseable — the operator asked for
    /// remote access and must not silently not get it.
    pub fn from_env() -> Result<Option<RemoteConfig>, String> {
        let Some(spec) = std::env::var_os("FBUI_REMOTE") else {
            return Ok(None);
        };
        let spec = spec.to_string_lossy();
        let addr: SocketAddr = if let Ok(port) = spec.parse::<u16>() {
            SocketAddr::from(([127, 0, 0, 1], port))
        } else {
            spec.parse()
                .map_err(|_| format!("FBUI_REMOTE={spec:?}: expected a port or host:port"))?
        };
        let token = std::env::var("FBUI_REMOTE_TOKEN")
            .ok()
            .filter(|t| !t.is_empty());
        Ok(Some(RemoteConfig { addr, token }))
    }

    /// Start the server, returning the hub the embedder feeds and the actual
    /// bound address (port `0` resolves here).
    pub fn spawn(&self) -> std::io::Result<(Arc<Hub>, SocketAddr)> {
        let hub = Hub::new();
        let addr = http::spawn(hub.clone(), self.addr, self.token.clone())?;
        Ok((hub, addr))
    }
}
