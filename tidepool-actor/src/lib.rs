//! Rust-owned substrate for self-writing Haskell actors.
//!
//! The first landed boundary is deliberately small: exact-incarnation
//! identity and the neutral event vocabulary consumed by journals, timelines,
//! and compatibility adapters. Machine execution remains in
//! `tidepool-runtime`; provider transport remains behind its existing seams.

#![warn(clippy::unwrap_used, clippy::expect_used)]

mod agent_session;
mod event;
mod generated;
mod identity;
mod mailbox;
mod mount;
mod registry;
mod sequence;
mod timeline;
mod wait;

pub use agent_session::{ActorAgentSession, AssistantTurn, PendingProviderTurn};
pub use event::{
    ActorEvent, ActorEventRecord, ActorExitKind, ActorRole, AnswerDisposition, CallDisposition,
    EventCausality, MailboxMessageKind, ModelUsage, StartInitiator, WaitDisposition,
};
pub use identity::{ActorId, ActorRef, Incarnation};
pub use mailbox::{
    CallFailure, CallId, CallStatus, CallTicket, ExitObservation, MailboxFailure, MailboxValue,
    MessageId, ParkedObligation, WaitError, WaitId, WaitTicket,
};
pub use mount::{
    mount_actor_turn, ActorCompileView, ActorCompileViewError, ActorPlacement, ActorRunTarget,
    ActorSessionContext, ActorSourceImports, MountActorTurnError,
};
pub use registry::{
    ActorDescriptor, ActorLifecycle, ActorRegistry, ActorRegistryError, ActorTerminal,
    ActorTurnKind, CallDelivery, CastDelivery, MailboxDelivery, StartingActor, TurnLease,
};
pub use sequence::{
    run_block_sequence, BlockExecution, BlockSequenceOutcome, CommittedBlock, ParsedBlock,
};
pub use timeline::{ActorTimeline, ActorTimelines, TimelineError, TimelineLifecycle};
pub use wait::{actor_terminal_value, ActorWait, ActorWaitError};
