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
//! Each child is admitted independently with an explicit context, workspace,
//! installer, and lifetime. [`ActorCapabilities`], exact [`ActorEffectKey`]
//! membership, opaque resource grants, and retained sponsor budgets authorize
//! its work. A captured checkpoint keeps information available without
//! changing the child's caller-selected tool surface. Readiness acknowledges
//! the installed actor before the caller receives its live typed handle.

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
mod forms;
pub use forms::{FormHost, FormPublication};
mod generated;
mod hosted_lifecycle;
mod identity;
mod interactive_session;
mod jev;
pub use jev::{
    failed_jev_client_setup, unconfigured_jev, JevBackend, JevBackendHandle, JevCallFailure,
};
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
// installs one has to be able to name what it yields. Actor capabilities belong
// to this crate; a conversation's speaker is the
// provider's, hence the alias.
pub use agent_spec::preparation::{
    BuiltinDeploymentProgram, BuiltinToolsetPolicy, ToolsetAcquisition, ToolsetProgramRecipe,
    ToolsetProgramSelection,
};
pub use descriptor::{ActorDescriptor, ActorPersistencePolicy};
pub use exomonad_model::{
    ConversationTurn, ConversationTurnState, Role as ConversationRole, TurnItem,
};
pub use exomonad_worktree::WorkspaceAccess;
pub use external_application::{
    ExternalApplicationFailure, ExternalApplicationFailureClass, ExternalFailureDisposition,
};

pub use fork_workspace::{
    PreparedWorkspaceAttachment, SpawnWorkspaceWire, WorkspaceAdmission, WorkspaceAdmissionError,
    WorkspaceAdmissionFuture, WorkspaceCustody, WorkspaceSeedWire, WorkspaceSelection,
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
    ActorAdmissionRegistry, CheckpointLease, CheckpointRefusal, SpawnAdmission,
    SpawnAdmissionOutcome, SpawnCleanupOutcome,
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
    spawn_resident_root_with_workspace_admission, ActorDisplayAdmission, ActorGraphNode,
    ActorProviderAdmission, DisplayConversationIdentity, DisplayPublication,
    DisplayPublicationHostContext, DisplayPublicationOutcome, LocalResidentDeployment,
    LocalResidentInstallation, ReleaseAwait, ResidentActorRoot, ResidentForest,
    ResidentKernelBehavior, ResidentRootEntry, ResourceRelease, RootStartupRelease,
};
pub use resident_interactive::{ResidentInteractivePolicy, HASKELL_TOOL};
pub use resident_tools::{
    HostedCheckpointAttachment, HostedCheckpointCapture, HostedCheckpointCaptureError,
    HostedCheckpointContext, HostedOperationFinalization, HostedOperationSettlement,
    HostedOperationTerminal, ProviderFinalizationKind, ResidentToolDispatchFuture,
    ResidentToolEndpoint, ResidentToolError, ResidentToolFuture, ResidentToolPolicy,
    ResidentToolResponse, WorkbenchBoundaryReconciliation, WorkbenchCancellationOutcome,
    WorkbenchExecutionControl,
};
pub use resident_workbench::{
    ActivationCompileStage, ActorMachineRegistry, ActorWorkbenchSource, ChildSessionFactory,
    InstalledToolLease, PreparedSourceToolset, RequestWorkbenchScope, ResidentActorRunner,
    ResidentActorWorkbench, ResidentActorWorkbenchError, ResidentMachineMeasurement,
    SourceToolsetRecipe, SpecReplacementDefinition, SpecReplacementError, ToolDispatchError,
    ToolDispatchReply,
};
pub use role::{render_child_budget, ActorCapabilities, ActorEffectKey, DescendantBudget};
pub use runtime_observation::{
    ActorActivationKind, ActorRuntimeObservation, ActorRuntimeObservationHandle,
    ActorSourceDriftObservation, ActorWorkbenchPosture, ActorWorkbenchTransfer,
    ActorWorkspaceObservation, CacheBoundaryReason, CheckoutGitDrift, FrozenSourceDrift,
    InboundDeliveryObservation, InboundNext, InboxDelivery, ObservedSource, ProviderUsageSample,
    SourceDriftTargets, SourceLayerDrift, SourceObservation, TrackedMessageObservation,
    TrackedMessageState,
};
pub use start::{
    ActorEffectKeyWire, ActorEffectProfileWire, ActorReplacementDefinition, ActorStartCaptureError,
    ForkEffort, Model, ResidentActorStart, SpawnContextWire, SpawnError, SpawnRetainedResources,
    WorkerLifetime,
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
