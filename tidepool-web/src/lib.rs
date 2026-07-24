//! tidepool-web — protocol server + observatory.
//!
//! R0 scaffold. Segment 30 C4 adds axum + the SSE event stream + the
//! protocol verbs (force / answer / cancel / eval-in-binding, snapshot
//! endpoints paginated, loopback bind only); segment 50 adds the maud
//! `Ui` → Datastar fragment renderer and the D1 tree view. Depends on
//! tidepool-harness only through its protocol/contract types — no private
//! APIs (E1: the web UI is a client of the documented protocol).

pub mod render;
pub mod server;
pub mod shell;

pub use render::{fragment, render_with_answer_url};
pub use server::{router, AppState};
