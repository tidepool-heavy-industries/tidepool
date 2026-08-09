//! tidepool-web — the minimal operator GUI for the self-iterating harness.
//!
//! A single clean form page served over HTTP + Datastar SSE. The harness
//! driver blocks on an [`OperatorGate`](tidepool_harness::selfharness::operator::OperatorGate);
//! [`server::WebGate`] implements that gate over a web round trip:
//! `present_form` publishes a [`FormSpec`](tidepool_harness::selfharness::operator::FormSpec)
//! (rendered by [`render`]) and parks a channel resolved by `POST /submit`;
//! `await_continue` parks a channel resolved by `POST /continue`.
//!
//! Four modules, one seam:
//! - [`render`] — a [`FormSpec`] → maud form (enum/int/text/bool + a Submit /
//!   Continue button); the `id="panel"` fragment patched over SSE.
//! - [`shell`] — the full HTML document (inline Swiss-minimal CSS + the
//!   vendored Datastar patch-apply / form-collection JS; no CDN, no build step).
//! - [`server`] — axum routes (`GET /`, `GET /sse`, `POST /submit`,
//!   `POST /continue`), the SSE broadcast stream, and [`server::WebGate`].
//! - [`formapi`] — a DISABLED-BY-DEFAULT testing-convenience surface
//!   (`GET`/`POST /api/form`) mounted onto the same router when
//!   `TIDEPOOL_FORM_API=1`; see that module's docs for the hardening story.
//!
//! Loopback bind only: reachability is the authorization boundary.

pub mod formapi;
pub mod render;
pub mod server;
pub mod shell;

pub use formapi::FormApiConfig;
pub use render::{panel, View};
pub use server::{router, router_with_form_api, AppState, WebGate};

use std::net::SocketAddr;
use std::sync::Arc;

/// The operator server's ONE bind address family — loopback only, any port.
/// Pulled out so it's pinned by a unit test independent of a real bind: the
/// form-api testing surface (`formapi`) mounts onto this SAME listener and
/// never opens one of its own, so pinning this literal pins its reachability
/// too.
fn bind_addr(port: u16) -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], port))
}

/// Boot the operator HTTP server on `127.0.0.1:<port>` and return the
/// [`WebGate`] a self-iterating harness driver blocks on for its two
/// operator interactions. The server runs on a spawned background task;
/// the caller stays responsible for keeping the process alive (e.g. by
/// driving a blocking `run_loop` on another thread of the same runtime).
///
/// Also mounts the [`formapi`] testing surface when `TIDEPOOL_FORM_API=1` is
/// set in the process environment — disabled otherwise.
pub async fn spawn_operator_server(port: u16) -> std::io::Result<Arc<WebGate>> {
    let state = AppState::new();
    let gate = Arc::new(WebGate::new(state.clone()));
    let addr = bind_addr(port);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let form_api = FormApiConfig::from_env();
    if form_api.enabled {
        eprintln!(
            "[boot] form-api ENABLED (TIDEPOOL_FORM_API=1) — testing-convenience surface on \
             GET/POST /api/form, loopback-only, not for browser/production use"
        );
    }
    eprintln!("[boot] operator GUI on http://{addr}");
    tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, router_with_form_api(state, form_api)).await {
            eprintln!("[operator server] error: {e}");
        }
    });
    Ok(gate)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The operator server's ONE bind literal is loopback, never the
    /// unspecified/all-interfaces address — the form-api surface rides this
    /// same listener, so this pins its reachability boundary too (see
    /// `bind_addr`'s doc comment).
    #[test]
    fn loopback_only() {
        let addr = bind_addr(4601);
        assert!(addr.ip().is_loopback(), "{addr} is not loopback");
        assert_ne!(
            addr.ip(),
            std::net::IpAddr::from(std::net::Ipv4Addr::UNSPECIFIED)
        );
    }
}
