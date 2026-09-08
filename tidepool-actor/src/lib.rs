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
//! are admitted dormant and released after the enclosing hosted tool block's
//! real result is durable. They inherit its final committed Haskell scope.
//! Admission failure aborts its own group; subsequent statement failure preserves
//! earlier admissions. Unacknowledged groups are cancelled on host reattachment
//! or owner shutdown. Published children remain independently addressable.

#![warn(clippy::unwrap_used, clippy::expect_used)]

mod descriptor;
mod external_application;
mod fork_workspace;
mod generated;
mod hosted_lifecycle;
mod identity;
mod interactive_session;
mod kernel;
mod lineage;
mod local_actor;
mod mailbox;
mod mount;
mod notification;
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
pub use hosted_lifecycle::{
    CleanupComponentOutcome, HostedWorkSeal, ResidentCleanupOutcome, ResidentShutdown,
};
mod typed_request;
mod wait;
mod workbench_display;

pub use descriptor::ActorDescriptor;
pub use external_application::{
    ExternalApplicationFailure, ExternalApplicationFailureClass, ExternalFailureDisposition,
};
pub use fork_workspace::{
    ForkWorkspaceAdmission, ForkWorkspaceAdmissionError, ForkWorkspaceAdmissionFuture,
    ForkWorkspaceCustody, ForkWorkspaceSeed, PreparedForkWorkspace,
};
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
    spawn_local_actor, spawn_local_actor_in_incarnation, ChildExitNotice, KernelBehavior,
    KernelBehaviorError, KernelContext, KernelStep, LocalActor, LocalActorArguments,
    LocalActorDirectory, LocalActorState,
};
pub use mailbox::MailboxValue;
pub use mount::{
    ActorCompileView, ActorCompileViewError, ActorPlacement, ActorRunTarget, ActorSessionContext,
    ActorSourceImports,
};
pub use notification::{
    NotificationError, NotificationPoll, NotificationReceipt, NotificationSend, NotificationState,
};
pub use profile::ActorEffectProfile;
pub use prompt_catalog::hosted_prompt_fingerprint as shoal_hosted_prompt_fingerprint;
pub use request::{
    AbandonResponseOutcome, ActorEventSequence, CancelRequestOutcome, CancellationReason,
    DeadlineUnit, ForgetResponseOutcome, ForgetWatchOutcome, ReplyError, ReplyObservation,
    RequestCancellationNotification, RequestDeadline, RequestId, RequestUpdateDelivery,
    RequestUpdateId, RequestUpdatePresentation, RequestUpdateState, ResponseFailure,
    ResponseObservation, WatchId, WatchNotification, WatchObservation, WatchStateProjection,
    WatchTransition,
};
pub use resident_actor::{
    spawn_resident_root, spawn_resident_root_in_incarnation,
    spawn_resident_root_with_fork_admission, ActorGraphNode, LocalResidentDeployment,
    LocalResidentInstallation, ResidentActorRoot, ResidentForest, ResidentKernelBehavior,
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
    ActorEffectKey, ActorRole, DescendantBudget, EffectiveRole, NativeToolClass, ResearchPolicy,
    WorkspaceAccess,
};
pub use runtime_observation::{
    ActorActivationKind, ActorRuntimeObservation, ActorRuntimeObservationHandle,
    ActorWorkbenchPosture, ActorWorkbenchTransfer, ActorWorkspaceObservation, CacheBoundaryReason,
    ProviderUsageSample,
};
pub use start::{
    ActorEffectKeyWire, ActorEffectProfileWire, ActorLaunchRoleWire, ActorStartCaptureError,
    ForkContext, ForkEffort, ResidentActorStart, WorkerLaunchPreview, WorkerLaunchRequest,
    WorkerLaunchResolver, WorkerLifetime,
};
pub use termination::{ActorExitAlreadyPublished, ActorExitKind, ActorTerminal, RetainedActorExit};
pub use tidepool_repr::{ActorPath, ActorPathError, ActorPathSegment};
pub use typed_request::{RequestSignatureError, ResponseExpectation};
pub use wait::{actor_terminal_value, ActorWaitError};
