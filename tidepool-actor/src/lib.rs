//! Rust-owned substrate for self-writing Haskell actors.
//!
//! Owns exact actor identity and lifecycle, typed live-value mailboxes,
//! resident agent sessions, Haskell actor startup, and supervision. Machine
//! execution remains in `tidepool-runtime`;
//! provider transport remains behind `tidepool-model`'s seams.
//!
//! A resident root or child is a persistent actor application, not one model
//! round. Ordinary provider output termination makes it idle; reply settlement
//! completes one typed request; only supervision terminates the actor.
//!
//! Cache-preserving context unfold is admitted here as one atomic sibling
//! group. The caller's active provider thread and immutable Haskell snapshot
//! are shared as information, while [`EffectiveRole`], exact
//! [`ActorEffectKey`] membership, opaque grants, workspace placement, and
//! descendant limits independently define each child's authority. Children
//! are not published until every provider binding is queue-ready and the final
//! hosted effect boundary commits. Pre-publication failure aborts the group;
//! published children remain independently addressable for typed follow-up.

#![warn(clippy::unwrap_used, clippy::expect_used)]

mod descriptor;
mod external_application;
mod fork_workspace;
mod generated;
mod identity;
mod interactive_session;
mod kernel;
mod lineage;
mod local_actor;
mod mailbox;
mod mount;
mod profile;
mod prompt_catalog;
mod request;
mod request_effect;
mod resident_actor;
mod resident_interactive;
mod resident_tools;
mod resident_workbench;
mod role;
mod runtime_observation;
mod start;
mod termination;
mod typed_request;
mod wait;

pub use descriptor::ActorDescriptor;
pub use external_application::{
    ExternalApplicationFailure, ExternalApplicationFailureClass, ExternalFailureDisposition,
};
pub use fork_workspace::{ForkWorkspaceAdmission, ForkWorkspaceAdmissionError, ForkWorkspaceSeed};
pub use identity::{ActorId, ActorRef, Incarnation};
pub use interactive_session::{
    ActivationId, InteractiveSessionCaptureError, InteractiveSessionRequest, ResidentActivation,
    ResidentInteractiveSession,
};
pub use kernel::{
    CallAncestry, KernelCallFailure, KernelCallReply, KernelInvocationFailure,
    KernelInvocationReply, KernelMessage, KernelWorkbenchFailure, KernelWorkbenchReply,
    LocalActorRef,
};
pub use lineage::{
    ActorLineageRegistry, ActorPathReservation, ForkGroupCleanupOutcome, ForkGroupError,
    ForkGroupGate, ForkGroupId, ForkGroupPhase, ForkGroupRegistry,
};
pub use local_actor::{
    spawn_local_actor, ChildExitNotice, KernelBehavior, KernelBehaviorError, KernelContext,
    KernelStep, LocalActor, LocalActorArguments, LocalActorDirectory, LocalActorState,
};
pub use mailbox::MailboxValue;
pub use mount::{
    ActorCompileView, ActorCompileViewError, ActorPlacement, ActorRunTarget, ActorSessionContext,
    ActorSourceImports,
};
pub use profile::ActorEffectProfile;
pub use request::{
    AbandonResponseOutcome, ActorEventSequence, CancelRequestOutcome, CancellationReason,
    DeadlineUnit, ForgetResponseOutcome, ForgetWatchOutcome, ReplyError, ReplyObservation,
    RequestCancellationNotification, RequestDeadline, RequestId, ResponseFailure,
    ResponseObservation, WatchId, WatchNotification, WatchObservation, WatchStateProjection,
    WatchTransition,
};
pub use resident_actor::{
    spawn_resident_root, spawn_resident_root_with_fork_admission, LocalResidentDeployment,
    LocalResidentInstallation, ResidentActorRoot, ResidentKernelBehavior,
};
pub use resident_interactive::{ResidentInteractivePolicy, HASKELL_TOOL};
pub use resident_tools::{
    ResidentToolEndpoint, ResidentToolError, ResidentToolFuture, ResidentToolPolicy,
};
pub use resident_workbench::{
    ActorMachineRegistry, ActorWorkbenchSource, ResidentActorRunner, ResidentActorWorkbench,
    ResidentActorWorkbenchError,
};
pub use role::{
    ActorEffectKey, ActorRole, DescendantBudget, EffectiveRole, NativeToolClass, WorkspaceAccess,
};
pub use runtime_observation::{
    ActorActivationKind, ActorRuntimeObservation, ActorRuntimeObservationHandle,
    CacheBoundaryReason, ProviderUsageSample, ProviderUsageScope,
};
pub use start::{
    ActorEffectKeyWire, ActorEffectProfileWire, ActorLaunchRoleWire, ActorStartCaptureError,
    ResidentActorStart,
};
pub use termination::{ActorExitAlreadyPublished, ActorExitKind, ActorTerminal, RetainedActorExit};
pub use tidepool_repr::{ActorPath, ActorPathError, ActorPathSegment};
pub use typed_request::{RequestSignatureError, ResponseExpectation};
pub use wait::{actor_terminal_value, ActorWaitError};
