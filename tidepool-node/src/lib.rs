//! Backend-neutral substrate for durable interactive-actor delivery and process ownership.

#![warn(clippy::unwrap_used, clippy::expect_used)]

mod inbox;
#[cfg(target_os = "linux")]
mod mount_namespace;
mod process_boundary;
mod process_supervisor;
pub mod systemd_slice;
mod tmux;

pub use inbox::{
    DeliveryAttempt, DeliveryPhase, DurableEnvelope, DurableInbox, InboxError, InboxWriteOperation,
    ReceiptEvidence, ReceiptLookup, MAX_RECEIPT_CONTEXT_BYTES, MAX_RETAINED_RECEIPTS,
    MAX_TOTAL_RECEIPT_CONTEXT_BYTES,
};
#[cfg(target_os = "linux")]
pub use mount_namespace::{
    copy_overlay_root_metadata, MountNamespace, NamespaceEntry, OverlayRecovery, OverlayRotation,
    OverlayRotationOutcome, PreparedOverlayRotation,
};
pub use process_boundary::service_scope::{
    LaunchRelease, LaunchReservation, PreparedServiceScope, RetainedProcessView, ScopeCapability,
    ScopeObservation, ServiceEnvironment, ServiceScope, ServiceScopeCleanup, ServiceScopeError,
    ServiceStdio,
};
pub use process_boundary::{
    ProcessBoundaryError, ProcessInvocation, ProcessMountBoundary, BUBBLEWRAP_PROGRAM,
};
pub use process_supervisor::{
    run_process_supervisor, ProcessSupervisorClient, ProcessSupervisorError,
    ProcessSupervisorManifest, ProcessSupervisorObservation, ProcessSupervisorRecovery,
    PROCESS_SUPERVISOR_CHECKPOINT, PROCESS_SUPERVISOR_MANIFEST, PROCESS_SUPERVISOR_SOCKET,
    PROCESS_SUPERVISOR_VERSION,
};
pub use tmux::{
    TmuxLaunch, TmuxNodeError, TmuxPaneId, TmuxPaneStatus, TmuxSession, TmuxSessionName,
};

pub mod command_resources;
