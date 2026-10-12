//! Per-incarnation startup ownership, independent of the workbench mailbox.
//!
//! The coordinator starts external journal work and handles its completion as
//! a message. A pending append never prevents retirement from closing admission.

use super::*;
use crate::initial_context::InitialContextEvent;
use std::collections::VecDeque;
use ractor::{Actor, ActorProcessingErr};

#[derive(Clone, Default)]
struct Observation {
    closed: bool,
    initial_context: Option<crate::InitialContextObservation>,
}

#[derive(Clone)]
pub(super) struct ActorLifecycle {
    address: ractor::ActorRef<Command>,
    observation: tokio::sync::watch::Receiver<Arc<Observation>>,
}

pub(super) struct InitialContextTransition {
    pub observation: crate::InitialContextObservation,
    pub admitted: bool,
}

type Reply = tokio::sync::oneshot::Sender<Result<InitialContextTransition, String>>;

struct Mutation {
    event: InitialContextEvent,
    begin: bool,
    reply: Reply,
}

enum Command {
    Mutate(Mutation),
    Completed {
        result: Result<crate::InitialContextObservation, String>,
        reply: Reply,
        admitted: bool,
    },
    Retire,
}

struct Coordinator;
struct State {
    actor: ActorRef,
    journal: Option<Arc<crate::ActorRecoveryJournal>>,
    observation: Observation,
    publisher: tokio::sync::watch::Sender<Arc<Observation>>,
    pending: bool,
    queued: VecDeque<Mutation>,
}

impl ActorLifecycle {
    pub(super) async fn start(
        actor: ActorRef,
        journal: Option<Arc<crate::ActorRecoveryJournal>>,
    ) -> Result<Self, String> {
        let (publisher, observation) = tokio::sync::watch::channel(Arc::new(Observation::default()));
        let (address, _task) = Actor::spawn(None, Coordinator, State {
            actor,
            journal,
            observation: Observation::default(),
            publisher,
            pending: false,
            queued: VecDeque::new(),
        }).await.map_err(|error| error.to_string())?;
        Ok(Self { address, observation })
    }

    /// Immutable observation grants no authority to execute or publish.
    pub(super) fn initial_context(&self) -> Option<crate::InitialContextObservation> {
        self.observation.borrow().initial_context.clone()
    }

    pub(super) fn is_closed(&self) -> bool {
        self.observation.borrow().closed
    }

    /// Queue closure at the same owning mailbox that admits mutations.
    /// A projection may lag this command and never authorizes a new mutation.
    pub(super) fn retire(&self) {
        let _ = self.address.cast(Command::Retire);
    }

    pub(super) fn abandon_initial_context(&self) {
        let (reply, _answer) = tokio::sync::oneshot::channel();
        let _ = self.address.cast(Command::Mutate(Mutation {
            event: InitialContextEvent::Fail { detail: "initial context mailbox admission was abandoned".into(),
                cancelled: true, receipts: serde_json::json!([]) },
            begin: false, reply,
        }));
    }

    pub(super) async fn begin_initial_context(
        &self,
        event: InitialContextEvent,
    ) -> Result<InitialContextTransition, String> {
        self.mutate(event, true).await
    }

    pub(super) async fn change_initial_context(
        &self,
        event: InitialContextEvent,
    ) -> Result<crate::InitialContextObservation, String> {
        self.mutate(event, false).await.map(|transition| transition.observation)
    }

    async fn mutate(&self, event: InitialContextEvent, begin: bool) -> Result<InitialContextTransition, String> {
        let (reply, answer) = tokio::sync::oneshot::channel();
        self.address.cast(Command::Mutate(Mutation { event, begin, reply }))
            .map_err(|error| error.to_string())?;
        answer.await.map_err(|_| "actor lifecycle ended before mutation settlement".to_owned())?
    }
}

impl State {
    fn advance(&mut self, address: &ractor::ActorRef<Command>) {
        if self.pending { return; }
        while let Some(mutation) = self.queued.pop_front() {
            let current = self.observation.clone();
            if mutation.begin {
                if let Some(observation) = current.initial_context {
                    let _ = mutation.reply.send(Ok(InitialContextTransition { observation, admitted: false }));
                    continue;
                }
            }
            self.pending = true;
            let journal = self.journal.clone();
            let actor = self.actor;
            let address = address.clone();
            tokio::spawn(async move {
                let result = tokio::task::spawn_blocking(move || {
                    if let Some(journal) = journal {
                        if mutation.begin {
                            if let Some(restored) = journal.initial_context(actor).map_err(|error| error.to_string())? {
                                return Ok((restored, false));
                            }
                        }
                        let owner = current.initial_context.as_ref()
                            .and_then(|value| value.identity.as_ref()).map_or(actor, |identity| identity.actor);
                        journal.record_initial_context(owner, mutation.event).map(|value| (value, true)).map_err(|error| error.to_string())
                    } else {
                        let mut next = current.initial_context;
                        crate::initial_context::apply_event(&mut next, mutation.event).map_err(|error| error.to_string())?;
                        next.map(|value| (value, true)).ok_or_else(|| "initial context transition produced no observation".into())
                    }
                }).await.map_err(|error| error.to_string()).and_then(|result| result);
                let (result, admitted) = match result {
                    Ok((observation, admitted)) => (Ok(observation), admitted),
                    Err(error) => (Err(error), false),
                };
                let _ = address.cast(Command::Completed { result, reply: mutation.reply, admitted });
            });
            break;
        }
        // A Preparing initializer has an admitted completion still outstanding.
        let observation = &self.observation;
        if observation.closed && !self.pending && self.queued.is_empty()
            && !observation.initial_context.as_ref().is_some_and(|value|
                matches!(value.state, crate::InitialContextState::Preparing)) {
            address.stop(None);
        }
    }
}

impl Actor for Coordinator {
    type Msg = Command;
    type State = State;
    type Arguments = State;

    async fn pre_start(&self, _: ractor::ActorRef<Command>, state: State) -> Result<State, ActorProcessingErr> {
        Ok(state)
    }

    async fn handle(&self, address: ractor::ActorRef<Command>, command: Command, state: &mut State) -> Result<(), ActorProcessingErr> {
        match command {
            Command::Mutate(mutation) => {
                let completion = matches!(&mutation.event,
                    InitialContextEvent::Retain { .. } | InitialContextEvent::Fail { .. }
                    | InitialContextEvent::Uncertain { .. })
                    && state.observation.initial_context.as_ref().is_some_and(|value|
                        matches!(value.state, crate::InitialContextState::Preparing));
                if state.observation.closed && !completion {
                    let _ = mutation.reply.send(Err("actor lifecycle admission is closed".into()));
                } else {
                    state.queued.push_back(mutation);
                }
            },
            Command::Completed { result, reply, admitted } => {
                state.pending = false;
                let answer = result.map(|observation| {
                    state.observation.initial_context = Some(observation.clone());
                    InitialContextTransition { observation, admitted }
                });
                state.publisher.send_replace(Arc::new(state.observation.clone()));
                // Retention commits even if the requester dropped its observer.
                let _ = reply.send(answer);
            }
            Command::Retire => state.observation.closed = true,
        }
        state.publisher.send_replace(Arc::new(state.observation.clone()));
        state.advance(&address);
        Ok(())
    }
}
