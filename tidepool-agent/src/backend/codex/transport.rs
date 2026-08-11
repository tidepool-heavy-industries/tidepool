//! The one seam between the [`Session`](super::process::Session) pump and the
//! bytes it pumps.
//!
//! # Why this exists
//!
//! It exists so the pump is TESTABLE, not as a general plugin point. The
//! surface is therefore exactly the four operations `Session` already performed
//! on [`RawAsyncClient`] — read a line, write a frame, name the child pid,
//! shut down — and nothing more. Anything wider would be a place for a second
//! implementation to diverge from the live one, which is the failure mode the
//! whole record/replay policy exists to avoid.
//!
//! # Generic, not `Box<dyn>`
//!
//! [`Session`](super::process::Session) is generic over this trait with
//! `RawAsyncClient` as its DEFAULT type parameter, rather than holding a boxed
//! trait object. Two reasons, in order:
//!
//! 1. **The live path stays allocation-identical.** `Session<RawAsyncClient>`
//!    monomorphizes to the same code the pump compiled to before the trait
//!    existed: no box, no vtable, and no per-frame virtual call on the read
//!    loop. A `dyn` version would additionally need boxed futures — `async fn`
//!    is not dyn-compatible — so every `next_line` on the hot path would
//!    allocate. Making a test seam cost the production path an allocation per
//!    frame is exactly the wrong trade.
//! 2. **The default type parameter means existing code is unedited.** Every
//!    `Session` mention elsewhere (`driver.rs`'s `Option<Session>`, its
//!    `&Session` helper arguments) keeps resolving to the live session with no
//!    change, so introducing the seam moved no live behavior.
//!
//! # Errors
//!
//! The error type is `codex_codes::Error`, unchanged from what
//! `RawAsyncClient` already returned, so `SessionError` and its projection in
//! `driver.rs` needed no new variant. A replay-side mismatch surfaces as
//! `codex_codes::Error::Protocol` — the variant whose meaning ("the peer did
//! something the protocol does not allow") is exactly what a transcript
//! mismatch is.

use std::future::Future;

use codex_codes::RawAsyncClient;
use serde_json::Value;

/// A newline-delimited JSON-RPC byte pipe with a peer on the other end.
pub trait Transport: Send {
    /// The next frame the peer sent, or `None` once the peer is done.
    ///
    /// `None` is a CLEAN end: the pump reports it as
    /// [`SessionError::Closed`](super::process::SessionError::Closed) rather
    /// than hanging, which is what makes an exhausted recording a diagnosable
    /// failure instead of a test that never finishes.
    fn next_line(
        &mut self,
    ) -> impl Future<Output = Result<Option<String>, codex_codes::Error>> + Send;

    /// Write one frame to the peer.
    fn send(
        &mut self,
        frame: &Value,
    ) -> impl Future<Output = Result<(), codex_codes::Error>> + Send;

    /// The peer's OS pid, when there is a process and the platform reports one.
    fn pid(&self) -> Option<u32>;

    /// Tear the peer down.
    fn shutdown(self) -> impl Future<Output = Result<(), codex_codes::Error>> + Send;
}

/// The live transport: the `codex app-server` child's stdio.
///
/// Pure delegation — every method is the inherent `RawAsyncClient` method the
/// pump used to call directly, so this impl adds no behavior of its own.
impl Transport for RawAsyncClient {
    fn next_line(
        &mut self,
    ) -> impl Future<Output = Result<Option<String>, codex_codes::Error>> + Send {
        RawAsyncClient::next_line(self)
    }

    fn send(
        &mut self,
        frame: &Value,
    ) -> impl Future<Output = Result<(), codex_codes::Error>> + Send {
        RawAsyncClient::send(self, frame)
    }

    fn pid(&self) -> Option<u32> {
        RawAsyncClient::pid(self)
    }

    fn shutdown(self) -> impl Future<Output = Result<(), codex_codes::Error>> + Send {
        RawAsyncClient::shutdown(self)
    }
}
