use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Weak};

use parking_lot::Mutex;
use tidepool_repr::MonotonicIdIssuer;

use crate::{
    ActorEvent, ActorEventRecord, ActorExitKind, ActorId, ActorRef, EventCausality, StartInitiator,
};

/// Immutable attributes selected before an actor begins initialization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActorDescriptor {
    pub label: String,
    pub effect_stack: Vec<String>,
}

/// A private initialization capability. No callable [`ActorRef`] is exposed
/// until [`ActorRegistry::publish_ready`] consumes the readiness transition.
pub struct StartingActor {
    actor: ActorRef,
    registry: Weak<RegistryInner>,
    armed: bool,
}

impl std::fmt::Debug for StartingActor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StartingActor").finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActorLifecycle {
    Initializing,
    Ready,
    Exited,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActorTerminal {
    pub kind: ActorExitKind,
    pub summary: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActorTurnKind {
    Haskell,
    Provider,
    Advisory,
    Mailbox,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ActorRegistryError {
    #[error("actor {0:?} is unknown")]
    Unknown(ActorRef),
    #[error("actor reference {given:?} is stale; current incarnation is {current:?}")]
    Stale { given: ActorRef, current: ActorRef },
    #[error("actor {0:?} is still initializing")]
    Initializing(ActorRef),
    #[error("actor {0:?} is already ready")]
    AlreadyReady(ActorRef),
    #[error("actor {0:?} has exited")]
    Exited(ActorRef),
    #[error("actor {actor:?} already has an active {active:?} turn")]
    Busy {
        actor: ActorRef,
        active: ActorTurnKind,
    },
    #[error("startup token belongs to another actor registry")]
    ForeignStartup,
}

/// Thread-safe ownership and lifecycle registry. It intentionally does not
/// own a machine session: turn admission is a guard above the existing runtime
/// checkout mechanism.
#[derive(Clone)]
pub struct ActorRegistry {
    inner: Arc<RegistryInner>,
}

struct RegistryInner {
    ids: MonotonicIdIssuer,
    state: Mutex<RegistryState>,
}

#[derive(Default)]
struct RegistryState {
    actors: HashMap<ActorId, ActorEntry>,
    next_stream_sequence: u64,
    events: Vec<ActorEventRecord>,
}

struct ActorEntry {
    reference: ActorRef,
    owner: Option<ActorRef>,
    children: BTreeSet<ActorRef>,
    lifecycle: ActorLifecycle,
    active_turn: Option<ActorTurnKind>,
    terminal: Option<ActorTerminal>,
    next_event_sequence: u64,
}

impl ActorRegistry {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RegistryInner {
                ids: MonotonicIdIssuer::new("actor"),
                state: Mutex::new(RegistryState::default()),
            }),
        }
    }

    /// Allocate an initializing actor. The returned token is deliberately not
    /// an `ActorRef`; callers publish a usable reference only after authored
    /// startup and installation have completed.
    pub fn begin_start(
        &self,
        owner: Option<ActorRef>,
        descriptor: ActorDescriptor,
        initiator: StartInitiator,
    ) -> Result<StartingActor, ActorRegistryError> {
        let reference = ActorRef::first(ActorId(self.inner.ids.next_raw()));
        let mut state = self.inner.state.lock();
        if let Some(owner) = owner {
            let owner_entry = entry(&state, owner)?;
            match owner_entry.lifecycle {
                ActorLifecycle::Initializing => {
                    return Err(ActorRegistryError::Initializing(owner));
                }
                ActorLifecycle::Exited => return Err(ActorRegistryError::Exited(owner)),
                ActorLifecycle::Ready => {}
            }
        }

        state.actors.insert(
            reference.id,
            ActorEntry {
                reference,
                owner,
                children: BTreeSet::new(),
                lifecycle: ActorLifecycle::Initializing,
                active_turn: None,
                terminal: None,
                next_event_sequence: 0,
            },
        );
        if let Some(owner) = owner {
            entry_mut(&mut state, owner)?.children.insert(reference);
        }
        record(
            &mut state,
            reference,
            EventCausality {
                owner,
                ..EventCausality::default()
            },
            ActorEvent::Created {
                owner,
                label: descriptor.label,
                effect_stack: descriptor.effect_stack,
            },
        )?;
        record(
            &mut state,
            reference,
            EventCausality::default(),
            ActorEvent::Started { initiator },
        )?;

        Ok(StartingActor {
            actor: reference,
            registry: Arc::downgrade(&self.inner),
            armed: true,
        })
    }

    /// Linearize readiness and reveal the exact-incarnation reference.
    pub fn publish_ready(
        &self,
        mut starting: StartingActor,
    ) -> Result<ActorRef, ActorRegistryError> {
        self.validate_starting(&starting)?;
        let mut state = self.inner.state.lock();
        let actor = starting.actor;
        match entry(&state, actor)?.lifecycle {
            ActorLifecycle::Initializing => {}
            ActorLifecycle::Ready => return Err(ActorRegistryError::AlreadyReady(actor)),
            ActorLifecycle::Exited => return Err(ActorRegistryError::Exited(actor)),
        }
        entry_mut(&mut state, actor)?.lifecycle = ActorLifecycle::Ready;
        record(
            &mut state,
            actor,
            EventCausality::default(),
            ActorEvent::Ready,
        )?;
        starting.armed = false;
        Ok(actor)
    }

    /// Terminate an actor whose startup failed before reference publication.
    /// The terminal fact remains observable in the journal, but no callable
    /// handle is returned to the starter.
    pub fn abort_start(
        &self,
        mut starting: StartingActor,
        terminal: ActorTerminal,
    ) -> Result<(), ActorRegistryError> {
        self.validate_starting(&starting)?;
        let mut state = self.inner.state.lock();
        let result = match entry(&state, starting.actor)?.lifecycle {
            ActorLifecycle::Initializing => exit_subtree(&mut state, starting.actor, terminal),
            ActorLifecycle::Ready => Err(ActorRegistryError::AlreadyReady(starting.actor)),
            ActorLifecycle::Exited => Err(ActorRegistryError::Exited(starting.actor)),
        };
        if result.is_ok() {
            starting.armed = false;
        }
        result
    }

    /// Admit one serialized actor turn. Dropping the lease restores admission
    /// even during unwinding; machine ownership is still separately fenced by
    /// the runtime checkout API.
    pub fn begin_turn(
        &self,
        actor: ActorRef,
        kind: ActorTurnKind,
    ) -> Result<TurnLease, ActorRegistryError> {
        let mut state = self.inner.state.lock();
        let actor_entry = entry_mut(&mut state, actor)?;
        match actor_entry.lifecycle {
            ActorLifecycle::Initializing => return Err(ActorRegistryError::Initializing(actor)),
            ActorLifecycle::Exited => return Err(ActorRegistryError::Exited(actor)),
            ActorLifecycle::Ready => {}
        }
        if let Some(active) = actor_entry.active_turn {
            return Err(ActorRegistryError::Busy { actor, active });
        }
        actor_entry.active_turn = Some(kind);
        Ok(TurnLease {
            actor,
            kind,
            registry: Arc::downgrade(&self.inner),
            released: false,
        })
    }

    /// Retain a terminal outcome and recursively cancel descendants. Ordinary
    /// child failure never changes its owner.
    pub fn finish(
        &self,
        actor: ActorRef,
        terminal: ActorTerminal,
    ) -> Result<(), ActorRegistryError> {
        let mut state = self.inner.state.lock();
        entry(&state, actor)?;
        exit_subtree(&mut state, actor, terminal)
    }

    pub fn lifecycle(&self, actor: ActorRef) -> Result<ActorLifecycle, ActorRegistryError> {
        Ok(entry(&self.inner.state.lock(), actor)?.lifecycle)
    }

    /// Terminal results are immutable and repeatably observable after runtime
    /// execution resources have been reaped.
    pub fn terminal(&self, actor: ActorRef) -> Result<Option<ActorTerminal>, ActorRegistryError> {
        Ok(entry(&self.inner.state.lock(), actor)?.terminal.clone())
    }

    pub fn children(&self, actor: ActorRef) -> Result<Vec<ActorRef>, ActorRegistryError> {
        Ok(entry(&self.inner.state.lock(), actor)?
            .children
            .iter()
            .copied()
            .collect())
    }

    pub fn owner(&self, actor: ActorRef) -> Result<Option<ActorRef>, ActorRegistryError> {
        Ok(entry(&self.inner.state.lock(), actor)?.owner)
    }

    pub fn events(&self) -> Vec<ActorEventRecord> {
        self.inner.state.lock().events.clone()
    }

    pub(crate) fn record_event(
        &self,
        actor: ActorRef,
        causality: EventCausality,
        event: ActorEvent,
    ) -> Result<(), ActorRegistryError> {
        let mut state = self.inner.state.lock();
        match entry(&state, actor)?.lifecycle {
            ActorLifecycle::Initializing => return Err(ActorRegistryError::Initializing(actor)),
            ActorLifecycle::Exited => return Err(ActorRegistryError::Exited(actor)),
            ActorLifecycle::Ready => {}
        }
        record(&mut state, actor, causality, event)
    }

    fn validate_starting(&self, starting: &StartingActor) -> Result<(), ActorRegistryError> {
        let Some(registry) = starting.registry.upgrade() else {
            return Err(ActorRegistryError::ForeignStartup);
        };
        if Arc::ptr_eq(&registry, &self.inner) {
            Ok(())
        } else {
            Err(ActorRegistryError::ForeignStartup)
        }
    }
}

impl Default for ActorRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for StartingActor {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let Some(registry) = self.registry.upgrade() else {
            return;
        };
        let mut state = registry.state.lock();
        if matches!(
            entry(&state, self.actor).map(|entry| entry.lifecycle),
            Ok(ActorLifecycle::Initializing)
        ) {
            let _ = exit_subtree(
                &mut state,
                self.actor,
                ActorTerminal {
                    kind: ActorExitKind::Cancelled,
                    summary: "startup capability dropped before readiness".into(),
                },
            );
        }
    }
}

/// Releases actor turn admission on drop.
pub struct TurnLease {
    actor: ActorRef,
    kind: ActorTurnKind,
    registry: Weak<RegistryInner>,
    released: bool,
}

impl TurnLease {
    pub fn release(mut self) {
        self.release_inner();
    }

    fn release_inner(&mut self) {
        if self.released {
            return;
        }
        if let Some(registry) = self.registry.upgrade() {
            let mut state = registry.state.lock();
            if let Ok(actor) = entry_mut(&mut state, self.actor) {
                if actor.active_turn == Some(self.kind) {
                    actor.active_turn = None;
                }
            }
        }
        self.released = true;
    }
}

impl Drop for TurnLease {
    fn drop(&mut self) {
        self.release_inner();
    }
}

fn entry(state: &RegistryState, actor: ActorRef) -> Result<&ActorEntry, ActorRegistryError> {
    let Some(found) = state.actors.get(&actor.id) else {
        return Err(ActorRegistryError::Unknown(actor));
    };
    if found.reference != actor {
        return Err(ActorRegistryError::Stale {
            given: actor,
            current: found.reference,
        });
    }
    Ok(found)
}

fn entry_mut(
    state: &mut RegistryState,
    actor: ActorRef,
) -> Result<&mut ActorEntry, ActorRegistryError> {
    let Some(found) = state.actors.get_mut(&actor.id) else {
        return Err(ActorRegistryError::Unknown(actor));
    };
    if found.reference != actor {
        return Err(ActorRegistryError::Stale {
            given: actor,
            current: found.reference,
        });
    }
    Ok(found)
}

fn record(
    state: &mut RegistryState,
    actor: ActorRef,
    causality: EventCausality,
    event: ActorEvent,
) -> Result<(), ActorRegistryError> {
    let stream_sequence = state.next_stream_sequence;
    state.next_stream_sequence += 1;
    let actor_entry = entry_mut(state, actor)?;
    let actor_sequence = actor_entry.next_event_sequence;
    actor_entry.next_event_sequence += 1;
    state.events.push(ActorEventRecord {
        stream_sequence,
        actor_sequence,
        actor,
        causality,
        event,
    });
    Ok(())
}

fn exit_subtree(
    state: &mut RegistryState,
    actor: ActorRef,
    terminal: ActorTerminal,
) -> Result<(), ActorRegistryError> {
    let children: Vec<_> = entry(state, actor)?.children.iter().copied().collect();
    {
        let actor_entry = entry_mut(state, actor)?;
        if actor_entry.lifecycle == ActorLifecycle::Exited {
            return Ok(());
        }
        actor_entry.lifecycle = ActorLifecycle::Exited;
        actor_entry.active_turn = None;
        actor_entry.terminal = Some(terminal.clone());
    }
    record(
        state,
        actor,
        EventCausality::default(),
        ActorEvent::Exited {
            kind: terminal.kind,
            summary: terminal.summary,
        },
    )?;
    for child in children {
        exit_subtree(
            state,
            child,
            ActorTerminal {
                kind: ActorExitKind::Cancelled,
                summary: format!("owner {actor:?} exited"),
            },
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn descriptor(label: &str) -> ActorDescriptor {
        ActorDescriptor {
            label: label.into(),
            effect_stack: vec!["Deliberate".into()],
        }
    }

    fn ready_root(registry: &ActorRegistry) -> ActorRef {
        let starting = registry
            .begin_start(None, descriptor("root"), StartInitiator::Runtime)
            .expect("begin root startup");
        registry.publish_ready(starting).expect("publish root")
    }

    #[test]
    fn reference_is_published_only_after_readiness() {
        let registry = ActorRegistry::new();
        let starting = registry
            .begin_start(None, descriptor("root"), StartInitiator::Runtime)
            .expect("begin startup");
        let actor = registry.publish_ready(starting).expect("publish ready");
        assert_eq!(registry.lifecycle(actor), Ok(ActorLifecycle::Ready));
        assert!(matches!(
            registry.events().last().map(|record| &record.event),
            Some(ActorEvent::Ready)
        ));
    }

    #[test]
    fn one_actor_never_admits_two_turns() {
        let registry = ActorRegistry::new();
        let actor = ready_root(&registry);
        let lease = registry
            .begin_turn(actor, ActorTurnKind::Provider)
            .expect("first turn");
        assert_eq!(
            registry.begin_turn(actor, ActorTurnKind::Haskell).err(),
            Some(ActorRegistryError::Busy {
                actor,
                active: ActorTurnKind::Provider,
            })
        );
        drop(lease);
        registry
            .begin_turn(actor, ActorTurnKind::Haskell)
            .expect("turn after release");
    }

    #[test]
    fn owner_exit_cancels_subtree_but_child_exit_does_not_kill_owner() {
        let registry = ActorRegistry::new();
        let root = ready_root(&registry);
        let child_starting = registry
            .begin_start(Some(root), descriptor("child"), StartInitiator::Runtime)
            .expect("begin child");
        let child = registry
            .publish_ready(child_starting)
            .expect("publish child");

        registry
            .finish(
                child,
                ActorTerminal {
                    kind: ActorExitKind::Failed,
                    summary: "child failed".into(),
                },
            )
            .expect("finish child");
        assert_eq!(registry.lifecycle(root), Ok(ActorLifecycle::Ready));
        assert_eq!(
            registry.terminal(child),
            Ok(Some(ActorTerminal {
                kind: ActorExitKind::Failed,
                summary: "child failed".into(),
            }))
        );

        let sibling_starting = registry
            .begin_start(Some(root), descriptor("sibling"), StartInitiator::Runtime)
            .expect("begin sibling");
        let sibling = registry
            .publish_ready(sibling_starting)
            .expect("publish sibling");
        registry
            .finish(
                root,
                ActorTerminal {
                    kind: ActorExitKind::Completed,
                    summary: "done".into(),
                },
            )
            .expect("finish root");
        assert_eq!(registry.lifecycle(sibling), Ok(ActorLifecycle::Exited));
        assert_eq!(
            registry
                .terminal(sibling)
                .expect("retained sibling exit")
                .map(|exit| exit.kind),
            Some(ActorExitKind::Cancelled)
        );
    }

    #[test]
    fn terminal_result_is_repeatable() {
        let registry = ActorRegistry::new();
        let actor = ready_root(&registry);
        let terminal = ActorTerminal {
            kind: ActorExitKind::Completed,
            summary: "result root live-value id 7".into(),
        };
        registry.finish(actor, terminal.clone()).expect("finish");
        assert_eq!(registry.terminal(actor), Ok(Some(terminal.clone())));
        assert_eq!(registry.terminal(actor), Ok(Some(terminal)));
    }

    #[test]
    fn failed_start_never_publishes_a_callable_reference() {
        let registry = ActorRegistry::new();
        let starting = registry
            .begin_start(None, descriptor("broken"), StartInitiator::Runtime)
            .expect("begin startup");
        registry
            .abort_start(
                starting,
                ActorTerminal {
                    kind: ActorExitKind::Failed,
                    summary: "initializer failed".into(),
                },
            )
            .expect("abort startup");
        assert!(matches!(
            registry.events().last().map(|record| &record.event),
            Some(ActorEvent::Exited {
                kind: ActorExitKind::Failed,
                ..
            })
        ));
    }

    #[test]
    fn dropped_startup_capability_cannot_leak_initializing_actor() {
        let registry = ActorRegistry::new();
        let starting = registry
            .begin_start(None, descriptor("abandoned"), StartInitiator::Runtime)
            .expect("begin startup");
        drop(starting);
        assert!(matches!(
            registry.events().last().map(|record| &record.event),
            Some(ActorEvent::Exited {
                kind: ActorExitKind::Cancelled,
                summary,
            }) if summary.contains("startup capability dropped")
        ));
    }
}
