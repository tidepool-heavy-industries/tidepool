//! Host-owned completion policy, distinct from actor cleanup and HTTP drain.

/// Only the host lifecycle owner selects the abort boundary. Native successful
/// completion/release is not implemented by the current service controller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CompletionBoundary {
    AwaitingNativeDecision,
    AbortForShutdown,
}
