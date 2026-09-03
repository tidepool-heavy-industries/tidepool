//! Backend-neutral substrate for durable interactive-actor delivery and process ownership.

#![warn(clippy::unwrap_used, clippy::expect_used)]

mod inbox;
mod process_boundary;
mod tmux;

pub use inbox::{DurableEnvelope, DurableInbox, InboxError};
pub use process_boundary::{
    ProcessBoundaryError, ProcessInvocation, ProcessMountBoundary, BUBBLEWRAP_PROGRAM,
};
pub use tmux::{TmuxLaunch, TmuxNodeError, TmuxPaneId, TmuxSession, TmuxSessionName};
