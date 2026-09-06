//! Staged host/process custody pairing. No production launch selects this path
//! before the native pin and host-work quiescence contracts are integrated.

use std::sync::Arc;

use tidepool_node::{ServiceScope, ServiceScopeCleanup};

use super::ActorWorkspaceCustody;

/// Captures the installed lease and its own spawn result, never a caller-supplied
/// cleanup receipt. Construction must consume a one-shot claim on that lease.
struct ScopedCustodyOwner {
    custody: Arc<ActorWorkspaceCustody>,
    scope: ServiceScope,
}

/// Reporting only: this copyable status does not authorize binding settlement.
enum ScopedCleanupObservation {
    ProcessStoppedHostWorkPending(ServiceScopeCleanup),
}

/// Intentionally uninhabited until the owning HTTP/resident-work mechanisms can
/// supply an exact quiescence contract. No bool or Drop success substitutes for it.
enum HostWorkQuiescence {}
