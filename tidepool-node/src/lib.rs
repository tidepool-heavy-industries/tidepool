//! Backend-neutral substrate for durable interactive-actor delivery and process ownership.

#![warn(clippy::unwrap_used, clippy::expect_used)]

mod inbox;
mod process_boundary;
mod tmux;

pub use inbox::{
    DeliveryAttempt, DeliveryPhase, DurableEnvelope, DurableInbox, InboxError, InboxWriteOperation,
    ReceiptEvidence, ReceiptLookup, MAX_RECEIPT_CONTEXT_BYTES, MAX_RETAINED_RECEIPTS,
    MAX_TOTAL_RECEIPT_CONTEXT_BYTES,
};
pub use process_boundary::service_scope::{
    PreparedServiceScope, ServiceEnvironment, ServiceScope, ServiceScopeCleanup, ServiceScopeError,
};
pub use process_boundary::{
    ProcessBoundaryError, ProcessInvocation, ProcessMountBoundary, BUBBLEWRAP_PROGRAM,
};
pub use tmux::{
    TmuxLaunch, TmuxNodeError, TmuxPaneId, TmuxPaneStatus, TmuxSession, TmuxSessionName,
};
