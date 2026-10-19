//! Affine fresh-root issuance and the exact handoff into directory supervision.

use super::*;

pub(crate) enum RootStartupCode {
    Installed(tidepool_runtime::session::PreparedStartupEntry),
    Recovered(Arc<tidepool_runtime::session::CompiledTurn>),
}

impl RootStartupCode {
    pub(crate) fn compile_input_identity(&self) -> Option<&str> {
        match self {
            Self::Installed(entry) => entry.compile_input_identity(),
            Self::Recovered(program) => program
                .ordinary_original_compile_input()?
                .replay_eligible_identity(),
        }
    }
}

pub(crate) struct RootPreparationDraft<H, O, T> {
    pub descriptor: ActorDescriptor,
    pub machine: ResidentSession<H, O>,
    pub program: Arc<tidepool_runtime::session::CompiledTurn>,
    pub predecessor: Option<tidepool_runtime::session::RecoveryPublicOwner>,
    pub metadata: T,
    pub startup: Option<RootStartupCode>,
}

impl<H, O, T> RootPreparationDraft<H, O, T> {
    pub(crate) fn issue(
        self,
        preparation: crate::CompilerPreparationOwner,
        cancellation: tidepool_runtime::HostAdmissionCancellation,
        installation_cancellation: Option<tidepool_runtime::HostAdmissionCancellation>,
    ) -> (ResidentActorRoot<H, O>, T) {
        let Self {
            descriptor,
            machine,
            startup,
            predecessor,
            metadata,
            ..
        } = self;
        (
            ResidentActorRoot {
                descriptor,
                machine,
                startup: startup.expect("successful root phases retain startup"),
                predecessor,
                preparation,
                cancellation,
                installation_cancellation,
            },
            metadata,
        )
    }
}

/// Only the tracked directory producer issues an already evaluated root.
/// Source provenance and physical placement move with its affine outcome.
pub(crate) struct PreparedRootExecution {
    pub(crate) publication: crate::local_actor::run_control::PublishedRootPlacement,
    pub(crate) outcome: ResidentOutcome,
    pub(crate) dependency: crate::resident_workbench::generation_admission::SourceDependency,
}

/// Failed preparation retains the same compiler owner and any produced machine.
/// Dropping the report is not a claim that an uncertain compiler END succeeded.
pub struct ResidentRootPreparationFailure<H, O, T> {
    pub(crate) cause: Box<dyn std::error::Error>,
    pub(crate) close: Option<crate::CompilerWorkClose>,
    pub(crate) draft: Option<Box<RootPreparationDraft<H, O, T>>>,
    pub(crate) preparation: crate::CompilerPreparationOwner,
}

impl<H, O, T> ResidentRootPreparationFailure<H, O, T> {
    pub fn cleanup(&self) -> crate::CompilerPreparationCleanup {
        self.preparation.cleanup()
    }
}

impl<H, O, T> std::fmt::Debug for ResidentRootPreparationFailure<H, O, T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResidentRootPreparationFailure")
            .field("cause", &self.cause.to_string())
            .field("close", &self.close)
            .field("retains_machine", &self.draft.is_some())
            .finish()
    }
}
impl<H, O, T> std::fmt::Display for ResidentRootPreparationFailure<H, O, T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}; compiler close: {:?}", self.cause, self.close)
    }
}
impl<H: 'static, O: 'static, T: 'static> std::error::Error
    for ResidentRootPreparationFailure<H, O, T>
{
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.cause.as_ref())
    }
}

/// Configuration can borrow the forest while this token still owns its machine.
/// Only the matching directory may consume it and insert that machine.
pub struct ResidentRootAdmission<H, O> {
    pub(super) root: ResidentActorRoot<H, O>,
    pub(super) directory: crate::LocalActorDirectory,
}
