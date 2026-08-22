//! tidepool-web — the operator GUI for the self-iterating harness, built on
//! the minimal node model: N REGISTERED NODES in a tree (slash-separated
//! `node_id` paths), each one lifecycle — a SEED prompt in, an append-only
//! TIMELINE of notes and asks, a FINAL VALUE (or failure) out — rendered as
//! one always-visible outline and served over HTTP + Datastar SSE.
//!
//! The harness driver blocks on an
//! [`OperatorGate`](tidepool_harness::selfharness::operator::OperatorGate);
//! [`server::WebGate`] implements that gate over a web round trip, bound to
//! one registered node: `present_form` publishes a
//! [`FormShape`](tidepool_harness::selfharness::operator::FormShape)
//! (rendered by [`render`]) onto that node's timeline and parks a channel
//! resolved by `POST /node/{node}/submit/{interaction}` — this covers the
//! self-iterating harness's between-loops gate too: it is an ordinary
//! driver-authored form, not a second mechanism. The node-lifecycle
//! extensions (`node_seeded`/`node_finalized`/
//! `node_failed`/`retire_node`) store what the driver sends across the seam.
//! Publishing never supersedes an existing pending ask; resolving keeps the
//! answered ask in place.
//!
//! Five modules, one seam:
//! - [`render`] — [`render::NodeView`] → one node's section markup (header +
//!   status, seed, timeline, final value/failure); the `id="panel-<node_id>"`
//!   fragment patched over SSE (also served standalone at `GET
//!   /node/{node}/panel` for the tree view's side pane).
//! - [`shell`] — `/legacy`'s full HTML document (inline Swiss-minimal CSS +
//!   the vendored Datastar patch-apply / form-collection / tree-mount JS; no
//!   CDN, no build step) — the ORIGINAL outline page, moved verbatim off
//!   `/`.
//! - [`tree`] — `/`'s full HTML document: a d3-hierarchy tree canvas (pan,
//!   zoom, status-colored nodes) with a side pane for a clicked node's full
//!   panel. Vendors d3 v7 as a served static asset (no CDN) and shares
//!   [`shell::CORE_JS`]'s form/SSE-patch plumbing with `/legacy` — one copy,
//!   two views.
//! - [`server`] — axum routes (`GET /`, `GET /legacy`, `GET /api/tree`, `GET
//!   /node/{node}/panel`, `GET /sse`, `POST /node/{node}/submit/{interaction}`),
//!   the SSE broadcast stream, and [`server::WebGate`].
//! - [`formapi`] — a DISABLED-BY-DEFAULT testing-convenience surface
//!   (`GET`/`POST /node/{node}/api/form`) mounted onto the same router when
//!   `TIDEPOOL_FORM_API=1`; see that module's docs for the hardening story.
//!
//! Loopback bind by default: reachability is the authorization boundary.
//! There is still no auth token on the HTTP surface itself — see
//! `TIDEPOOL_WEB_BIND_HOST` on [`bind_addr`] for the one, deliberate,
//! opt-in exception.

#![warn(clippy::unwrap_used, clippy::expect_used)]
pub mod formapi;
pub mod render;
pub mod server;
pub mod shell;
pub mod tree;

pub use render::{node_panel, NodeView};
pub use server::{router, router_with_form_api, AppState, WebGate};

use std::net::SocketAddr;
use std::sync::Arc;

/// The node id [`spawn_operator_server_multi`] registers for its caller —
/// the TREE ROOT, shared by the driver's default gate and a harness's own
/// root window (labeled `root` by convention, children `root/…`). See this
/// crate's `CLAUDE.md` ("Revival") for why that sharing is safe.
pub const DEFAULT_NODE_ID: &str = "root";

/// The operator server's bind address — loopback by default, any port.
/// Pulled out so the default is pinned by a unit test independent of a real
/// bind: the form-api testing surface (`formapi`) mounts onto this SAME
/// listener and never opens one of its own, so pinning this literal pins its
/// reachability too.
///
/// **`TIDEPOOL_WEB_BIND_HOST`** is a deliberate, opt-in escape hatch (operator
/// decision, 2026-08-18): when set to a valid IP, the server binds there
/// instead of loopback — e.g. a box's own Tailscale interface address, so the
/// operator GUI is reachable from another machine on the tailnet without an
/// SSH port-forward. This surface has NO AUTH TOKEN; loopback reachability is
/// its whole authorization boundary in the default case, so setting this
/// trades that boundary for whatever access control the target network
/// provides (a tailnet's own ACLs, in the Tailscale case) — never set it to
/// `0.0.0.0` or a publicly-routable address. An unparseable value falls back
/// to loopback with a loud warning rather than failing to bind.
fn bind_addr(port: u16) -> SocketAddr {
    match std::env::var("TIDEPOOL_WEB_BIND_HOST") {
        Ok(host) => match host.parse::<std::net::IpAddr>() {
            Ok(ip) => SocketAddr::from((ip, port)),
            Err(e) => {
                eprintln!(
                    "[boot] TIDEPOOL_WEB_BIND_HOST={host:?} is not a valid IP ({e}); \
                     falling back to loopback"
                );
                SocketAddr::from(([127, 0, 0, 1], port))
            }
        },
        Err(_) => SocketAddr::from(([127, 0, 0, 1], port)),
    }
}

/// Boot the operator HTTP server on `127.0.0.1:<port>` and return both the
/// [`AppState`] (register additional nodes on it via
/// [`AppState::register_node`] for a multi-tab page) and the [`WebGate`] for
/// the default single-tab registration ([`DEFAULT_NODE_ID`]) — the one-tab
/// case existing callers use unchanged. The server runs on a spawned
/// background task; the caller stays responsible for keeping the process
/// alive (e.g. by driving a blocking `run_loop` on another thread of the
/// same runtime).
///
/// Also mounts the [`formapi`] testing surface when `TIDEPOOL_FORM_API=1` is
/// set in the process environment — disabled otherwise.
pub async fn spawn_operator_server_multi(port: u16) -> std::io::Result<(AppState, Arc<WebGate>)> {
    let state = AppState::new();
    let gate = state.register_node(DEFAULT_NODE_ID);
    let addr = bind_addr(port);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let form_api_enabled = std::env::var("TIDEPOOL_FORM_API").as_deref() == Ok("1");
    if form_api_enabled {
        eprintln!(
            "[boot] form-api ENABLED (TIDEPOOL_FORM_API=1) — testing-convenience surface on \
             GET/POST /node/{{node}}/api/form, loopback-only, not for browser/production use"
        );
    }
    eprintln!("[boot] operator GUI on http://{addr}");
    let serve_state = state.clone();
    tokio::spawn(async move {
        if let Err(e) = axum::serve(
            listener,
            router_with_form_api(serve_state, form_api_enabled),
        )
        .await
        {
            eprintln!("[operator server] error: {e}");
        }
    });
    Ok((state, gate))
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
        // SAFETY (env mutation in a test): this crate's suite runs one test
        // per OS process under the project's mandated `cargo-nextest` runner
        // (root CLAUDE.md), so no other test observes this process's env —
        // still explicitly ensured absent first, defensively, for a plain
        // `cargo test` run sharing one process.
        std::env::remove_var("TIDEPOOL_WEB_BIND_HOST");
        let addr = bind_addr(4601);
        assert!(addr.ip().is_loopback(), "{addr} is not loopback");
        assert_ne!(
            addr.ip(),
            std::net::IpAddr::from(std::net::Ipv4Addr::UNSPECIFIED)
        );
    }

    /// The opt-in override binds where told, and falls back to loopback
    /// (never panics, never silently binds nothing) on an unparseable value.
    #[test]
    fn bind_host_override() {
        std::env::set_var("TIDEPOOL_WEB_BIND_HOST", "100.84.124.37");
        let addr = bind_addr(4602);
        assert_eq!(addr, "100.84.124.37:4602".parse().unwrap());

        std::env::set_var("TIDEPOOL_WEB_BIND_HOST", "not-an-ip");
        let addr = bind_addr(4602);
        assert!(addr.ip().is_loopback(), "{addr} is not loopback");

        std::env::remove_var("TIDEPOOL_WEB_BIND_HOST");
    }
}
