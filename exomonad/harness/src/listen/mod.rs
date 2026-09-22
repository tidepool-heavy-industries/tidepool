//! A durable outbound message feed from the resident harness to an operator's
//! terminal. The durable frame queue ([`queue::FrameQueue`]) mints the `seq`
//! carried on the wire, so an acknowledgement advances the queue's own cursor
//! without a second bookkeeping layer.
//!
//! Delivery is **print-then-ack**: a frame is written to the client and
//! flushed there BEFORE the client acks, and this server only advances its
//! persisted cursor once that ack lands ([`server::drain_pending`]) — so a
//! frame published while no client is attached, or to a client that dies
//! mid-delivery, simply stays queued and is redelivered, in order, the next
//! time a client connects. **Latest-wins**: a newer connection always
//! replaces an older one ([`server::ListenerSlot::install`]) — the server
//! cannot otherwise distinguish a genuine re-arm from a stale/zombie
//! predecessor, so a new client always wins and the old one sees EOF.
//!
//! [`ListenServer`] is the constructed handle: [`ListenServer::start`] binds
//! the socket, opens the durable queue, and spawns the accept/drain
//! background tasks; [`ListenServer::publish`] is the ONE way anything in
//! this process emits a frame. This module has no dependency on
//! [`crate::selfharness`] or the driver internals — the composition root
//! constructs it at boot from a run/session identifier
//! ([`server::ListenPaths::for_run`]) and hands the returned handle to
//! whatever wants to publish.
//!
//! The client half (a `tidepool listen` subcommand) lives in the `tidepool`
//! binary crate, not here — it depends on this module only for the wire
//! types ([`Frame`], [`Ack`]) and [`server::ListenPaths`].

pub mod queue;
pub mod server;

use serde::{Deserialize, Serialize};

/// Server → client: one delivered message. `seq` is the durable frame
/// sequence minted by [`queue::FrameQueue::publish`] — also the ack
/// correlation key AND the queue's own cursor unit, so an ack directly tells
/// the server how far it may durably advance. `text` is the rendered payload
/// the client writes to stdout verbatim (may contain embedded newlines —
/// JSON-escaped inside the single wire line).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Frame {
    pub seq: u64,
    pub text: String,
}

/// Client → server: "I have written frame `seq` to stdout and flushed it."
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct Ack {
    pub seq: u64,
}

pub use queue::{FrameQueue, QueueError};
pub use server::{
    drain_pending, DeliverError, ListenPaths, ListenServer, ListenServerError, ListenerSlot,
};

#[cfg(test)]
mod wire_tests {
    use super::*;

    #[test]
    fn frame_roundtrips_with_newlines() {
        let f = Frame {
            seq: 7,
            text: "line one\nline two".into(),
        };
        let json = serde_json::to_string(&f).unwrap();
        // The frame itself must stay one wire line.
        assert!(!json.contains('\n'));
        assert_eq!(serde_json::from_str::<Frame>(&json).unwrap(), f);
    }

    #[test]
    fn ack_roundtrips() {
        let a = Ack { seq: 42 };
        let json = serde_json::to_string(&a).unwrap();
        assert_eq!(serde_json::from_str::<Ack>(&json).unwrap(), a);
    }
}
