//! Rust-owned substrate for self-writing Haskell actors.
//!
//! Owns exact actor identity and lifecycle, typed live-value mailboxes,
//! resident agent sessions, Haskell actor startup, and supervision. Machine
//! execution remains in `tidepool-runtime`;
//! provider transport remains behind `tidepool-model`'s seams.

#![warn(clippy::unwrap_used, clippy::expect_used)]

mod agent_session;
mod completion;
mod descriptor;
mod executor;
mod external_application;
mod generated;
mod identity;
mod interactive_session;
mod kernel;
mod local_actor;
mod mailbox;
mod mount;
mod profile;
mod prompt_catalog;
mod resident_actor;
mod resident_interactive;
mod resident_tools;
mod resident_workbench;
mod start;
mod termination;
mod wait;

pub use agent_session::{
    ActorAgentSession, AdmittedAgentSession, AgentSessionError, AssistantTurn, PendingProviderRound,
};
pub use completion::{
    CompletionCaptureError, CompletionRequest, CompletionRequestError, ResidentCompletion,
    ResidentCompletionError, ResidentCompletionExecutor,
};
pub use descriptor::ActorDescriptor;
pub use executor::{
    run_result_session, AgentBlockStop, AgentExecutionError, AgentWorkbench, CompletionExpectation,
};
pub use external_application::{
    ExternalApplicationFailure, ExternalApplicationFailureClass, ExternalFailureDisposition,
};
pub use identity::{ActorId, ActorRef, Incarnation};
pub use interactive_session::{
    InteractiveSessionCaptureError, InteractiveSessionRequest, ResidentInteractiveSession,
};
pub use kernel::{
    CallAncestry, KernelCallFailure, KernelCallReply, KernelInvocationFailure,
    KernelInvocationReply, KernelMessage, KernelWorkbenchReply, LocalActorRef,
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
pub use resident_actor::{
    spawn_resident_root, LocalResidentDeployment, LocalResidentInstallation, ResidentActorRoot,
    ResidentKernelBehavior,
};
pub use resident_interactive::{ResidentInteractivePolicy, HASKELL_TOOL};
pub use resident_tools::{
    ResidentToolEndpoint, ResidentToolError, ResidentToolFuture, ResidentToolPolicy,
};
pub use resident_workbench::{
    ActorMachineRegistry, ActorWorkbenchSource, ResidentActorRunner, ResidentActorWorkbench,
    ResidentActorWorkbenchError,
};
pub use start::{ActorEffectProfileWire, ActorStartCaptureError, ResidentActorStart};
pub use termination::{ActorExitAlreadyPublished, ActorExitKind, ActorTerminal, RetainedActorExit};
pub use wait::{actor_terminal_value, ActorWaitError};
