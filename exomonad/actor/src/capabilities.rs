//! Narrow services available to resident application consumers.

use crate::resident_workbench::CompilerCloseOwner;
use crate::termination::CompilerWorkTicket;
use crate::ResidentActorWorkbenchError;

/// Register compiler work under an explicit captured lifetime.
///
/// Admission is synchronous metadata work: it starts no compiler or source IO.
/// Success records Pending at every required recipient before returning one
/// affine ticket. Refusal runs no action and settles partial registration as
/// NotStarted. Unwinding admission records Abandoned instead. Every call rechecks
/// the owner's live lifecycle/publication fences.
/// The service grants no owner identity or source authority.
///
/// The ticket's native owner settles independently of its async waiter. A stop
/// requests interruption and revokes post-close host admission; it is not proof
/// of cleanup. Shared preparation observers cannot stop their producer. Dropping
/// an unconsumed ticket records Abandoned, never confirmed cleanup.
pub(crate) trait CompilerAdmission: Send + Sync {
    fn admit_work(
        &self,
        owner: &CompilerCloseOwner,
    ) -> Result<CompilerWorkTicket, ResidentActorWorkbenchError>;

    /// Apply the exact owner's existing publication admission policy.
    /// This never transitions the owner or reopens its closed fences.
    fn admit_publication(
        &self,
        owner: &CompilerCloseOwner,
    ) -> Result<CompilerWorkTicket, ResidentActorWorkbenchError>;
}

/// A consumer declares only the service it uses, independently of bundle wiring.
pub(crate) trait HasCompilerAdmission {
    type Compiler: CompilerAdmission + ?Sized;

    fn compiler_admission(&self) -> &Self::Compiler;
}

/// The resident application's service view, shared by its machine entry points.
/// Per-operation lifetime and source authority travel separately.
#[derive(Clone)]
pub(crate) struct WorkbenchServices {
    compiler: CompilerAdmissionService,
}

impl WorkbenchServices {
    pub(crate) fn new() -> Self {
        Self {
            compiler: CompilerAdmissionService,
        }
    }
}

#[derive(Clone)]
pub(crate) struct CompilerAdmissionService;

impl CompilerAdmission for CompilerAdmissionService {
    fn admit_work(
        &self,
        owner: &CompilerCloseOwner,
    ) -> Result<CompilerWorkTicket, ResidentActorWorkbenchError> {
        owner.register_work()
    }

    fn admit_publication(
        &self,
        owner: &CompilerCloseOwner,
    ) -> Result<CompilerWorkTicket, ResidentActorWorkbenchError> {
        owner.register_publication_work()
    }
}

impl HasCompilerAdmission for WorkbenchServices {
    type Compiler = CompilerAdmissionService;

    fn compiler_admission(&self) -> &Self::Compiler {
        &self.compiler
    }
}

/// Run an action after checkout/source admission, using the captured exact owner.
/// Action success and compiler close are separate; this caller retains the
/// owner's live close observation while the action returns its original value.
pub(crate) fn run_compiler_action<C: HasCompilerAdmission + ?Sized, T>(
    capabilities: &C,
    owner: &CompilerCloseOwner,
    action: impl FnOnce(&mut dyn FnMut(tidepool_runtime::CompilerTransactionClose)) -> T,
) -> Result<T, ResidentActorWorkbenchError> {
    Ok(capabilities
        .compiler_admission()
        .admit_work(owner)?
        .run(action))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::termination::CompilerWorkClose;

    #[test]
    fn generic_action_preserves_failure_and_rechecks_captured_owner() {
        let capabilities = WorkbenchServices::new();
        let retained = crate::RetainedActorExit::new();
        let owner = CompilerCloseOwner::ActorLifecycle(retained.clone());
        assert_eq!(
            run_compiler_action(&capabilities, &owner, |_| Err::<(), _>("original refusal"))
                .unwrap(),
            Err("original refusal")
        );
        assert_eq!(
            retained.compiler_close_observations(),
            vec![CompilerWorkClose::Settled(
                tidepool_runtime::CompilerTransactionClose::NotStarted
            )]
        );
        retained.request_shutdown(crate::ActorTerminal::new(
            crate::ActorExitKind::Cancelled,
            "close captured owner",
        ));
        let mut entered = false;
        assert!(matches!(
            run_compiler_action(&capabilities, &owner, |_| entered = true),
            Err(ResidentActorWorkbenchError::CompilerCleanupAdmissionClosed)
        ));
        assert!(!entered);
        assert_eq!(retained.compiler_close_observations().len(), 1);
    }

    #[test]
    fn generic_action_compensates_a_shared_observers_partial_refusal() {
        let capabilities = WorkbenchServices::new();
        let producer = crate::RetainedActorExit::new();
        let observer = crate::RetainedActorExit::new();
        observer.request_shutdown(crate::ActorTerminal::new(
            crate::ActorExitKind::Cancelled,
            "observer already closed",
        ));
        let owner = CompilerCloseOwner::ActorLifecycle(observer.clone())
            .shared_preparation(producer.clone());
        let mut entered = false;
        assert!(matches!(
            run_compiler_action(&capabilities, &owner, |_| entered = true),
            Err(ResidentActorWorkbenchError::CompilerCleanupAdmissionClosed)
        ));
        assert!(!entered);
        assert_eq!(
            producer.compiler_close_observations(),
            vec![CompilerWorkClose::Settled(
                tidepool_runtime::CompilerTransactionClose::NotStarted
            )]
        );
        assert!(observer.compiler_close_observations().is_empty());
    }

    #[test]
    fn issued_control_stops_only_its_work_before_and_after_compiler_close() {
        for stop_before in [false, true] {
            let services = WorkbenchServices::new();
            let retained = crate::RetainedActorExit::new();
            let owner = CompilerCloseOwner::ActorLifecycle(retained.clone());
            let ticket = services.compiler_admission().admit_work(&owner).unwrap();
            let control = ticket.control();
            if stop_before {
                control.cancel();
            }
            let settled =
                ticket.run_with_close(|_| tidepool_extract_cmd::compiler_host_checkpoint());
            if stop_before {
                assert_eq!(
                    settled.action().as_ref().unwrap_err().kind(),
                    std::io::ErrorKind::Interrupted
                );
            } else {
                settled.action().as_ref().unwrap();
                control.cancel();
            }
            assert_eq!(
                settled
                    .host_admission_cancellation()
                    .unwrap()
                    .checkpoint()
                    .unwrap_err()
                    .kind(),
                std::io::ErrorKind::Interrupted
            );
            assert_eq!(
                *settled.close(),
                tidepool_runtime::CompilerTransactionClose::NotStarted
            );
            let fresh = CompilerCloseOwner::ActorLifecycle(crate::RetainedActorExit::new());
            run_compiler_action(&services, &fresh, |_| {
                tidepool_extract_cmd::compiler_host_checkpoint()
            })
            .unwrap()
            .unwrap();
        }
    }

    #[test]
    fn admitted_observation_retains_abandonment_after_owners_are_released() {
        let capabilities = WorkbenchServices::new();
        let retained = crate::RetainedActorExit::new();
        let owner = CompilerCloseOwner::ActorLifecycle(retained.clone());
        let ticket = capabilities
            .compiler_admission()
            .admit_work(&owner)
            .unwrap();
        let observation = ticket.control().observation();
        assert_eq!(observation.observation(), CompilerWorkClose::Pending);
        drop(retained);
        drop(owner);
        drop(capabilities);
        drop(ticket);
        assert_eq!(observation.observation(), CompilerWorkClose::Abandoned);
    }
}
