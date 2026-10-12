//! One run owns actor membership, identity claims and preparation admission.
use super::*;
use tokio::sync::oneshot;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DirectoryError {
    #[error("run control is unavailable")]
    Unavailable,
    #[error("actor forest admission is closed")]
    Closed,
    #[error("logical actor {actor:?} is already claimed")]
    IdentityConflict { actor: crate::ActorId },
    #[error("directory operation does not own actor {actor:?}")]
    Unauthorized { actor: ActorRef },
}

/// Registration and preparation share this phase, rather than separate gates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RunPhase {
    Running,
    RetiringPreparations,
    RetiringResources,
    Retired,
}

pub(super) struct RunState {
    phase: RunPhase,
    entries: HashMap<ActorRef, DirectoryEntry>,
    sessions: HashMap<ActorRef, crate::ActorSessionContext>,
    identities: DirectoryIdentities,
    resources: HashMap<ractor::ActorId, ResourceChild>,
}
impl Default for RunState {
    fn default() -> Self {
        Self {
            phase: RunPhase::Running,
            entries: HashMap::new(),
            sessions: HashMap::new(),
            identities: DirectoryIdentities::default(),
            resources: HashMap::new(),
        }
    }
}

pub(super) enum RunControlMessage {
    Reserve {
        incarnation: crate::Incarnation,
        reply: oneshot::Sender<Result<ActorRef, DirectoryError>>,
    },
    ClaimExact {
        actor: ActorRef,
        reply: oneshot::Sender<Result<(), DirectoryError>>,
    },
    Fence {
        actors: Vec<crate::ActorId>,
        reply: oneshot::Sender<Result<(), DirectoryError>>,
    },
    Register {
        actor: LocalActorRef,
        context: std::sync::Weak<KernelContext>,
        ownership: SpawnOwnership,
        reply: oneshot::Sender<Result<(), DirectoryError>>,
    },
    Resolve {
        actor: ActorRef,
        reply: oneshot::Sender<Option<LocalActorRef>>,
    },
    RuntimeIdentity {
        actor: ractor::ActorId,
        reply: oneshot::Sender<Option<ActorRef>>,
    },
    Context {
        actor: ActorRef,
        reply: oneshot::Sender<Option<Arc<KernelContext>>>,
    },
    Session {
        actor: ActorRef,
        reply: oneshot::Sender<Option<crate::ActorSessionContext>>,
    },
    InstallSession {
        context: crate::ActorSessionContext,
        reply: oneshot::Sender<Result<(), DirectoryError>>,
    },
    Forget {
        actor: ActorRef,
        reply: oneshot::Sender<bool>,
    },
    Seal {
        reply: oneshot::Sender<SealedActorMembership>,
    },
}

pub(super) struct RunControl;
impl Actor for RunControl {
    type Msg = RunControlMessage;
    type State = RunState;
    type Arguments = ();
    async fn pre_start(
        &self,
        _myself: RactorRef<Self::Msg>,
        (): (),
    ) -> Result<Self::State, ActorProcessingErr> {
        Ok(RunState::default())
    }
    async fn handle(
        &self,
        _myself: RactorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            RunControlMessage::Reserve { incarnation, reply } => {
                let result = if state.phase != RunPhase::Running {
                    Err(DirectoryError::Closed)
                } else {
                    loop {
                        let id = crate::ActorId(NEXT_ACTOR_ID.fetch_add(1, Ordering::Relaxed));
                        if !state.identities.fenced.contains(&id)
                            && !state.identities.claimed.contains_key(&id)
                        {
                            let actor = ActorRef { id, incarnation };
                            state.identities.claimed.insert(id, actor);
                            break Ok(actor);
                        }
                    }
                };
                let _ = reply.send(result);
            }
            RunControlMessage::ClaimExact { actor, reply } => {
                let result = if state.phase != RunPhase::Running {
                    Err(DirectoryError::Closed)
                } else if state
                    .identities
                    .claimed
                    .get(&actor.id)
                    .is_some_and(|previous| {
                        actor.incarnation <= previous.incarnation
                            || state
                                .entries
                                .get(previous)
                                .is_none_or(|entry| entry.actor.terminal().get().is_none())
                    })
                {
                    Err(DirectoryError::IdentityConflict { actor: actor.id })
                } else {
                    advance_actor_ids_past(actor.id);
                    state.identities.fenced.remove(&actor.id);
                    state.identities.claimed.insert(actor.id, actor);
                    Ok(())
                };
                let _ = reply.send(result);
            }
            RunControlMessage::Fence { actors, reply } => {
                let result = if state.phase != RunPhase::Running {
                    Err(DirectoryError::Closed)
                } else if let Some(actor) = actors
                    .iter()
                    .find(|actor| state.identities.claimed.contains_key(actor))
                {
                    Err(DirectoryError::IdentityConflict { actor: *actor })
                } else {
                    for actor in actors {
                        state.identities.fenced.insert(actor);
                        advance_actor_ids_past(actor);
                    }
                    Ok(())
                };
                let _ = reply.send(result);
            }
            RunControlMessage::Register {
                actor,
                context,
                ownership,
                reply,
            } => {
                let identity = actor.identity();
                let result = if state.phase != RunPhase::Running {
                    Err(DirectoryError::Closed)
                } else if state.identities.claimed.get(&identity.id) != Some(&identity) {
                    Err(DirectoryError::Unauthorized { actor: identity })
                } else {
                    state
                        .identities
                        .runtime
                        .insert(actor.address().get_id(), identity);
                    state.entries.insert(
                        identity,
                        DirectoryEntry {
                            actor,
                            context,
                            spawn_ownership: ownership,
                        },
                    );
                    Ok(())
                };
                let _ = reply.send(result);
            }
            RunControlMessage::Resolve { actor, reply } => {
                let _ = reply.send(state.entries.get(&actor).map(|entry| entry.actor.clone()));
            }
            RunControlMessage::RuntimeIdentity { actor, reply } => {
                let _ = reply.send(state.identities.runtime.get(&actor).copied());
            }
            RunControlMessage::Context { actor, reply } => {
                let _ = reply.send(
                    state
                        .entries
                        .get(&actor)
                        .and_then(|entry| entry.context.upgrade()),
                );
            }
            RunControlMessage::Session { actor, reply } => {
                let _ = reply.send(state.sessions.get(&actor).cloned());
            }
            RunControlMessage::InstallSession { context, reply } => {
                let result = if state.entries.contains_key(&context.actor) {
                    state.sessions.insert(context.actor, context);
                    Ok(())
                } else {
                    Err(DirectoryError::Unauthorized {
                        actor: context.actor,
                    })
                };
                let _ = reply.send(result);
            }
            RunControlMessage::Forget { actor, reply } => {
                let terminal = state
                    .entries
                    .get(&actor)
                    .is_some_and(|entry| entry.actor.terminal().get().is_some());
                if terminal {
                    if let Some(entry) = state.entries.remove(&actor) {
                        state
                            .identities
                            .runtime
                            .remove(&entry.actor.address().get_id());
                    }
                    state.sessions.remove(&actor);
                }
                let _ = reply.send(terminal);
            }
            RunControlMessage::Seal { reply } => {
                if state.phase == RunPhase::Running {
                    state.phase = RunPhase::RetiringPreparations;
                }
                let _ = reply.send(SealedActorMembership {
                    actors: state
                        .entries
                        .values()
                        .map(|entry| (entry.actor.clone(), entry.spawn_ownership))
                        .collect(),
                });
            }
        }
        Ok(())
    }
}

/// Exact run-control endpoint. Startup is completed before this handle is issued.
#[derive(Clone)]
pub struct RunControlClient {
    owner: RactorRef<RunControlMessage>,
}
impl RunControlClient {
    pub async fn start() -> Result<(Self, ractor::concurrency::JoinHandle<()>), ractor::SpawnErr> {
        let (owner, task) = Actor::spawn(None, RunControl, ()).await?;
        Ok((Self { owner }, task))
    }
    async fn ask<T: Send + 'static>(
        &self,
        message: impl FnOnce(oneshot::Sender<T>) -> RunControlMessage,
    ) -> Result<T, DirectoryError> {
        let (reply, receiver) = oneshot::channel();
        self.owner
            .send_message(message(reply))
            .map_err(|_| DirectoryError::Unavailable)?;
        receiver.await.map_err(|_| DirectoryError::Unavailable)
    }
    pub(super) async fn reserve(
        &self,
        incarnation: crate::Incarnation,
    ) -> Result<ActorRef, DirectoryError> {
        self.ask(|reply| RunControlMessage::Reserve { incarnation, reply })
            .await?
    }
    pub(super) async fn claim_exact(&self, actor: ActorRef) -> Result<(), DirectoryError> {
        self.ask(|reply| RunControlMessage::ClaimExact { actor, reply })
            .await?
    }
    pub(crate) async fn fence_logical_ids(
        &self,
        actors: impl IntoIterator<Item = crate::ActorId>,
    ) -> Result<(), DirectoryError> {
        let actors = actors.into_iter().collect();
        self.ask(|reply| RunControlMessage::Fence { actors, reply })
            .await?
    }
    pub(super) async fn runtime_identity(
        &self,
        actor: ractor::ActorId,
    ) -> Result<Option<ActorRef>, DirectoryError> {
        self.ask(|reply| RunControlMessage::RuntimeIdentity { actor, reply })
            .await
    }
    pub async fn resolve(&self, actor: ActorRef) -> Result<Option<LocalActorRef>, DirectoryError> {
        self.ask(|reply| RunControlMessage::Resolve { actor, reply })
            .await
    }
    pub async fn session_context(
        &self,
        actor: ActorRef,
    ) -> Result<Option<crate::ActorSessionContext>, DirectoryError> {
        self.ask(|reply| RunControlMessage::Session { actor, reply })
            .await
    }
    pub(super) async fn context(
        &self,
        actor: ActorRef,
    ) -> Result<Option<Arc<KernelContext>>, DirectoryError> {
        self.ask(|reply| RunControlMessage::Context { actor, reply })
            .await
    }
    pub(super) async fn install_session_context(
        &self,
        context: crate::ActorSessionContext,
    ) -> Result<(), DirectoryError> {
        self.ask(|reply| RunControlMessage::InstallSession { context, reply })
            .await?
    }
    pub(super) async fn insert(
        &self,
        actor: LocalActorRef,
        context: std::sync::Weak<KernelContext>,
        ownership: SpawnOwnership,
    ) -> Result<(), DirectoryError> {
        self.ask(|reply| RunControlMessage::Register {
            actor,
            context,
            ownership,
            reply,
        })
        .await?
    }
    pub(crate) async fn seal(&self) -> Result<SealedActorMembership, DirectoryError> {
        self.ask(|reply| RunControlMessage::Seal { reply }).await
    }
    pub(super) async fn forget_terminal(&self, actor: ActorRef) -> Result<bool, DirectoryError> {
        self.ask(|reply| RunControlMessage::Forget { actor, reply })
            .await
    }
}
