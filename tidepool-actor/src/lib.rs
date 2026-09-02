//! Rust-owned substrate for self-writing Haskell actors.
//!
//! Owns exact actor identity and lifecycle, typed live-value mailboxes,
//! resident agent sessions, Haskell actor startup, supervision, and the
//! neutral event stream. Machine execution remains in `tidepool-runtime`;
//! provider transport remains behind `tidepool-model`'s seams.

#![warn(clippy::unwrap_used, clippy::expect_used)]

mod agent_session;
mod authorization;
mod completion;
mod event;
mod executor;
mod generated;
mod host;
mod identity;
mod interactive_session;
mod mailbox;
mod mount;
mod profile;
mod registry;
mod resident_interactive;
mod resident_lifecycle;
mod resident_mailbox;
mod resident_mcp;
mod resident_workbench;
mod start;
mod termination;
mod timeline;
mod wait;

pub use agent_session::{
    ActorAgentSession, AdmittedAgentSession, AgentSessionError, AssistantTurn, PendingProviderRound,
};
pub use authorization::{ActorEffectRefusal, ActorOperationClass, ActorProfileHandler};
pub use completion::{
    CompletionCaptureError, CompletionRequest, CompletionRequestError, ResidentCompletion,
    ResidentCompletionError, ResidentCompletionExecutor,
};
pub use event::{
    ActorEvent, ActorEventRecord, ActorExitKind, ActorRole, AnswerDisposition, CallDisposition,
    EventCausality, MailboxMessageKind, ModelUsage, StartInitiator, WaitDisposition,
};
pub use executor::{
    run_result_session, AgentBlockStop, AgentExecutionError, AgentWorkbench, CompletionExpectation,
};
pub use host::{
    ExternalApplicationFailure, ExternalApplicationFailureClass, ExternalFailureDisposition,
    ResidentActorDeployment, ResidentActorHost, ResidentActorHostControl,
    ResidentActorHostControlError, ResidentActorHostError, ResidentActorRoot,
    ResidentHostParkedKind, ResidentHostRunReport, ResidentHostShutdownReport,
    ResidentHostTaskError, ResidentMcpInstallation,
};
pub use identity::{ActorId, ActorRef, Incarnation};
pub use interactive_session::{
    InteractiveSessionCaptureError, InteractiveSessionRequest, ResidentInteractiveSession,
};
pub use mailbox::{
    CallFailure, CallId, CallStatus, CallTicket, ExitObservation, MailboxFailure, MailboxValue,
    MessageId, ParkedObligation, WaitError, WaitId, WaitTicket,
};
pub use mount::{
    mount_actor_turn, ActorCompileView, ActorCompileViewError, ActorPlacement, ActorRunTarget,
    ActorSessionContext, ActorSourceImports, MountActorTurnError,
};
pub use profile::ActorEffectProfile;
pub use registry::{
    ActorDescriptor, ActorLifecycle, ActorRegistry, ActorRegistryError, ActorRuntimeWake,
    ActorRuntimeWakes, ActorTurnKind, CallDelivery, CastDelivery, MailboxDelivery, StartingActor,
    TurnLease,
};
pub use resident_interactive::ResidentInteractivePolicy;
pub use resident_lifecycle::{
    ResidentActorLifecycle, ResidentLifecycleError, ResidentLifecyclePolicy,
};
pub use resident_mailbox::{
    OutboundSettlement, ResidentActorMailbox, ResidentCall, ResidentCallPoll, ResidentMailboxError,
    ResidentWait, ResidentWaitPoll,
};
pub use resident_mcp::{ResidentMcpEndpoint, ResidentMcpError, ResidentMcpPolicy};
pub use resident_workbench::{
    ActorMachineRegistry, ActorWorkbenchSource, ResidentActorRunner, ResidentActorWorkbench,
    ResidentActorWorkbenchError,
};
pub use start::{
    ActorStartCaptureError, ResidentActorStart, ResidentActorStartError, ResidentActorStarter,
};
pub use termination::{ActorExitAlreadyPublished, ActorTerminal, RetainedActorExit};
pub use timeline::{ActorTimeline, ActorTimelines, TimelineError, TimelineLifecycle};
pub use wait::{actor_terminal_value, ActorWait, ActorWaitError};
