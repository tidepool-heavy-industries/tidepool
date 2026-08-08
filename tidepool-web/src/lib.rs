//! tidepool-web — the minimal operator GUI for the self-iterating harness.
//!
//! A single clean form page served over HTTP + Datastar SSE. The harness
//! driver blocks on an [`OperatorGate`](tidepool_harness::selfharness::operator::OperatorGate);
//! [`server::WebGate`] implements that gate over a web round trip:
//! `present_form` publishes a [`FormSpec`](tidepool_harness::selfharness::operator::FormSpec)
//! (rendered by [`render`]) and parks a channel resolved by `POST /submit`;
//! `await_continue` parks a channel resolved by `POST /continue`.
//!
//! Three modules, one seam:
//! - [`render`] — a [`FormSpec`] → maud form (enum/int/text/bool + a Submit /
//!   Continue button); the `id="panel"` fragment patched over SSE.
//! - [`shell`] — the full HTML document (inline Swiss-minimal CSS + the
//!   vendored Datastar patch-apply / form-collection JS; no CDN, no build step).
//! - [`server`] — axum routes (`GET /`, `GET /sse`, `POST /submit`,
//!   `POST /continue`), the SSE broadcast stream, and [`server::WebGate`].
//!
//! Loopback bind only: reachability is the authorization boundary.

pub mod render;
pub mod server;
pub mod shell;

pub use render::{panel, View};
pub use server::{router, AppState, WebGate};

use std::sync::Arc;

/// Boot the operator HTTP server on `127.0.0.1:<port>` and return the
/// [`WebGate`] a self-iterating harness driver blocks on for its two
/// operator interactions. The server runs on a spawned background task;
/// the caller stays responsible for keeping the process alive (e.g. by
/// driving a blocking `run_loop` on another thread of the same runtime).
pub async fn spawn_operator_server(port: u16) -> std::io::Result<Arc<WebGate>> {
    let state = AppState::new();
    let gate = Arc::new(WebGate::new(state.clone()));
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    eprintln!("[boot] operator GUI on http://{addr}");
    tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, router(state)).await {
            eprintln!("[operator server] error: {e}");
        }
    });
    Ok(gate)
}
