//! Rust-owned substrate for self-writing Haskell actors.
//!
//! Owns exact actor identity and lifecycle, typed live-value mailboxes,
//! resident agent sessions, Haskell actor startup, and supervision. Machine
//! execution remains in `tidepool-runtime`;
//! provider transport remains behind `exomonad-model`'s seams.
//!
//! A resident root or child is a persistent actor application, not one model
//! round. Ordinary provider output termination makes it idle; reply settlement
//! completes one typed request; only supervision terminates the actor.
//!
//! Cache-preserving context unfold is admitted here as one atomic sibling
//! group. The caller's active provider thread and immutable Haskell snapshot
//! are shared as information, while [`ActorCapabilities`], exact
//! [`ActorEffectKey`] membership, opaque grants, workspace placement, and
//! descendant limits independently define each child's authority. Children
//! are admitted dormant and released after the enclosing hosted tool block's
//! real result is durable. They inherit its final committed Haskell scope.
//! Admission failure aborts its own group; subsequent statement failure preserves
//! earlier admissions. Unacknowledged groups are cancelled on host reattachment
//! or owner shutdown. Published children remain independently addressable.

pub(crate) mod after_tool;
pub mod agent_spec;
mod call_timing;
mod cell_context;
mod cell_model;
pub use cell_context::{CellExit, CellExitCause, ContextReq, HostedContextBinding};
pub use cell_model::{CellModelBinding, CellModelFactory, ModelBoundaryError, ModelReq};
mod conversation;
mod descriptor;
mod external_application;
mod fork_workspace;
mod generated;
mod hosted_lifecycle;
mod identity;
mod interactive_session;
mod jev;
pub use jev::{unconfigured_jev, JevBackend, JevBackendHandle, JevCallFailure};
mod kernel;
mod lineage;
mod local_actor;
mod lookup;
pub(crate) mod lookup_tool;
mod mailbox;
mod mount;
mod notification;
mod profile;
mod prompt_catalog;
pub(crate) mod reload_helpers_tool;
pub(crate) mod reload_spec_tool;
mod request;
pub use request::sources::SourceDelivery;
mod recovery;
mod request_effect;
mod resident_actor;
mod resident_interactive;
mod resident_tools;
mod resident_workbench;
mod role;
pub(crate) mod tool_contract;
pub use tool_contract::ToolContractError;
mod runtime_observation;
mod start;
pub(crate) mod status_tool;
mod termination;
pub use hosted_lifecycle::{
    CleanupComponentOutcome, ForestRootShutdown, HostedWorkSeal, ResidentCleanupOutcome,
    ResidentShutdown,
};
mod typed_request;
mod usage_pointer;
pub use usage_pointer::UsagePointerTable;
mod wait;
mod workbench_display;
pub use after_tool::AFTER_TOOL_WAIT_ENV;
pub use workbench_display::bounded_output as bound_workbench_display;

pub use conversation::{ConversationFuture, ConversationReader, ConversationUnavailable};
// A `ConversationReader` resolves to `Vec<ConversationTurn>`, so anyone who
// installs one has to be able to name what it yields. `ActorRole` and
// `ActorCapabilities` are this crate's own; a conversation's speaker is the
// provider's, hence the alias.
pub use agent_spec::preparation::ToolsetAcquisition;
pub use descriptor::{ActorDescriptor, ActorPersistencePolicy};
pub use exomonad_model::{ConversationTurn, Role as ConversationRole, TurnItem};
pub use external_application::{
    ExternalApplicationFailure, ExternalApplicationFailureClass, ExternalFailureDisposition,
};
pub use fork_workspace::{
    ForkWorkspaceAdmission, ForkWorkspaceAdmissionError, ForkWorkspaceAdmissionFuture,
    ForkWorkspaceCustody, ForkWorkspacePolicy, ForkWorkspaceSeed, PreparedForkWorkspace,
};
pub use identity::{ActorId, ActorRef, Incarnation};
pub use interactive_session::{
    ActivationId, InteractiveSessionCaptureError, InteractiveSessionRequest, ResidentActivation,
    ResidentInteractiveSession, SiblingPreview,
};
pub use kernel::{
    ActorAdmissionLease, ActorWorkbenchInvocation, CallAncestry, KernelCallFailure,
    KernelCallReply, KernelInvocationFailure, KernelInvocationReply, KernelMessage, KernelResume,
    KernelWorkbenchFailure, KernelWorkbenchReply, LocalActorRef, NativeProviderAdmission,
    NativeProviderStartError, NativeProviderTurnLease, WorkbenchStepKey,
};
pub use lineage::{
    ActorLineageRegistry, ActorPathReservation, CheckpointLease, CheckpointRefusal,
    ForkGroupCleanupOutcome, ForkGroupError, ForkGroupGate, ForkGroupId, ForkGroupPhase,
    ForkGroupPublication, ForkGroupRegistry,
};
pub use local_actor::{
    spawn_local_actor, spawn_local_actor_in_incarnation, ActorAbandonGuard, ActorAdvance,
    ChildExitNotice, KernelBehavior, KernelBehaviorError, KernelContext, KernelStep, LocalActor,
    LocalActorArguments, LocalActorDirectory, LocalActorState, OwnedActorCompletion,
    OwnedActorTask, OwnedWorkbenchCompletion, OwnedWorkbenchTask, WorkbenchAbandonGuard,
    WorkbenchAdvance, WorkbenchDispatch,
};
pub use mailbox::MailboxValue;
pub use mount::{
    ActorCompileView, ActorCompileViewError, ActorPlacement, ActorRunTarget, ActorSessionContext,
    ActorSourceImports, ActorSourceLayerResolver, ActorSourceLayers, CheckpointSourceLayer,
    RetainedSourceLayer, SourceEntryStorage, SourceLayerIssuer, SourceLayerReload,
    StagedActorSourceReload,
};
pub use notification::{
    NotificationError, NotificationPoll, NotificationReceipt, NotificationSend, NotificationState,
};
pub use profile::ActorEffectProfile;
pub use prompt_catalog::hosted_prompt_fingerprint as exomonad_hosted_prompt_fingerprint;
pub use recovery::{
    ActorRecoveryJournal, ApplicationConversation, DurableActorAdmission, DurableActorApplication,
    DurableActorRecord, DurableActorTerminal, DurableRootSuccessorAdmission, RootRecoveryPlacement,
    RootStartupIntent, RootStartupManifestPin,
};
pub use request::{
    AbandonResponseOutcome, ActorEventSequence, CancelRequestOutcome, CancellationReason,
    DeadlineUnit, ForgetResponseOutcome, ForgetWatchOutcome, LateUpdateEvidence, PendingProgress,
    ReplyError, ReplyObservation, RequestCancellationNotification, RequestDeadline, RequestId,
    RequestUpdateCorrelation, RequestUpdateDelivery, RequestUpdateId, RequestUpdatePresentation,
    RequestUpdateReconciler, RequestUpdateState, ResponseFailure, ResponseObservation,
    SettlementNotification, SettlementTransition, UpdateReconciliationError, WatchId,
    WatchNotification, WatchObservation, WatchStateProjection, WatchTransition,
};
pub use resident_actor::{
    spawn_resident_root, spawn_resident_root_in_incarnation,
    spawn_resident_root_with_fork_admission, ActorDisplayAdmission, ActorGraphNode,
    ActorProviderAdmission, DisplayConversationIdentity, DisplayPublication,
    DisplayPublicationHostContext, DisplayPublicationOutcome, ForkChildRelease,
    LocalResidentDeployment, LocalResidentInstallation, ReleaseAwait, ResidentActorRoot,
    ResidentForest, ResidentKernelBehavior, ResidentRootEntry, ResourceRelease, RootStartupRelease,
};
pub use resident_interactive::{ResidentInteractivePolicy, HASKELL_TOOL};
pub use resident_tools::{
    HostedCheckpointAttachment, HostedCheckpointCapture, HostedCheckpointCaptureError,
    HostedCheckpointContext, ResidentToolDispatchFuture, ResidentToolEndpoint, ResidentToolError,
    ResidentToolFuture, ResidentToolPolicy, ResidentToolResponse, WorkbenchBoundaryReconciliation,
    WorkbenchCancellationOutcome, WorkbenchExecutionControl,
};
pub use resident_workbench::{
    ActivationCompileStage, ActorMachineRegistry, ActorWorkbenchSource, ChildSessionFactory,
    InstalledToolLease, PreparedSourceToolset, RequestWorkbenchScope, ResidentActorRunner,
    ResidentActorWorkbench, ResidentActorWorkbenchError, ResidentMachineMeasurement,
    SourceToolsetRecipe, ToolDispatchError, ToolDispatchReply,
};
pub use role::{
    render_child_budget, ActorCapabilities, ActorEffectKey, DescendantBudget,
};
pub use runtime_observation::{
    ActorActivationKind, ActorRuntimeObservation, ActorRuntimeObservationHandle,
    ActorSourceDriftObservation, ActorWorkbenchPosture, ActorWorkbenchTransfer,
    ActorWorkspaceObservation, CacheBoundaryReason, CheckoutGitDrift, FrozenSourceDrift,
    InboundDeliveryObservation, InboundNext, InboxDelivery, ObservedSource, ProviderUsageSample,
    SourceDriftTargets, SourceLayerDrift, SourceObservation, TrackedMessageObservation,
    TrackedMessageState,
};
pub use start::{
    ActorEffectKeyWire, ActorEffectProfileWire, ActorLaunchRoleWire, ActorReplacementDefinition,
    ActorStartCaptureError, ForkContext, ForkEffort, Model, ResidentActorStart,
    WorkerLaunchPreview, WorkerLaunchRequest, WorkerLaunchResolver, WorkerLifetime,
};
pub use termination::{
    ActorExitAlreadyPublished, ActorExitKind, ActorLifecycle, ActorLifecycleConnection,
    ActorTerminal, CompilerPreparationCleanup, CompilerPreparationCleanupObservation,
    CompilerPreparationOutcome, CompilerPreparationOwner, CompilerWorkClose, RetainedActorExit,
};
pub use tidepool_repr::{ActorPath, ActorPathError, ActorPathSegment};
pub use typed_request::{RequestSignatureError, ResponseExpectation};
pub use wait::ActorWaitError;

pub mod command_jobs;

pub(crate) use runtime_observation::ProviderTurnLease;
