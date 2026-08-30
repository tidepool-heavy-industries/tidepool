use std::collections::{BTreeSet, HashMap};

use crate::{ActorEvent, ActorEventRecord, ActorRef, ActorTerminal};

/// Presentation-neutral observability state folded from authoritative actor
/// events. It is a view, never the source used for runtime admission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActorTimeline {
    pub actor: ActorRef,
    pub owner: Option<ActorRef>,
    pub label: String,
    pub lifecycle: TimelineLifecycle,
    pub model_messages: u64,
    pub haskell_batches: u64,
    pub active_suspensions: BTreeSet<String>,
    pub terminal: Option<ActorTerminal>,
    pub last_stream_sequence: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimelineLifecycle {
    Initializing,
    Ready,
    Exited,
}

#[derive(Debug, Clone, Default)]
pub struct ActorTimelines {
    actors: HashMap<ActorRef, ActorTimeline>,
    next_actor_sequence: HashMap<ActorRef, u64>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum TimelineError {
    #[error("actor event stream regressed from {previous} to {next}")]
    StreamRegression { previous: u64, next: u64 },
    #[error("actor {actor:?} expected event sequence {expected}, found {found}")]
    ActorSequenceGap {
        actor: ActorRef,
        expected: u64,
        found: u64,
    },
    #[error("actor {actor:?} emitted {event} before creation")]
    EventBeforeCreation {
        actor: ActorRef,
        event: &'static str,
    },
    #[error("actor {0:?} was created twice")]
    DuplicateCreation(ActorRef),
}

impl ActorTimelines {
    pub fn fold<'a>(
        records: impl IntoIterator<Item = &'a ActorEventRecord>,
    ) -> Result<Self, TimelineError> {
        let mut timelines = Self::default();
        let mut last_stream_sequence = None;
        for record in records {
            if let Some(previous) = last_stream_sequence {
                if record.stream_sequence <= previous {
                    return Err(TimelineError::StreamRegression {
                        previous,
                        next: record.stream_sequence,
                    });
                }
            }
            last_stream_sequence = Some(record.stream_sequence);
            timelines.apply(record)?;
        }
        Ok(timelines)
    }

    pub fn get(&self, actor: ActorRef) -> Option<&ActorTimeline> {
        self.actors.get(&actor)
    }

    pub fn iter(&self) -> impl Iterator<Item = &ActorTimeline> {
        self.actors.values()
    }

    fn apply(&mut self, record: &ActorEventRecord) -> Result<(), TimelineError> {
        if let ActorEvent::Created { owner, label, .. } = &record.event {
            if self.actors.contains_key(&record.actor) {
                return Err(TimelineError::DuplicateCreation(record.actor));
            }
            if record.actor_sequence != 0 {
                return Err(TimelineError::ActorSequenceGap {
                    actor: record.actor,
                    expected: 0,
                    found: record.actor_sequence,
                });
            }
            self.actors.insert(
                record.actor,
                ActorTimeline {
                    actor: record.actor,
                    owner: *owner,
                    label: label.clone(),
                    lifecycle: TimelineLifecycle::Initializing,
                    model_messages: 0,
                    haskell_batches: 0,
                    active_suspensions: BTreeSet::new(),
                    terminal: None,
                    last_stream_sequence: record.stream_sequence,
                },
            );
            self.next_actor_sequence.insert(record.actor, 1);
            return Ok(());
        }

        let Some(timeline) = self.actors.get_mut(&record.actor) else {
            return Err(TimelineError::EventBeforeCreation {
                actor: record.actor,
                event: event_name(&record.event),
            });
        };
        let expected = self.next_actor_sequence.get_mut(&record.actor).ok_or(
            TimelineError::EventBeforeCreation {
                actor: record.actor,
                event: event_name(&record.event),
            },
        )?;
        if record.actor_sequence != *expected {
            return Err(TimelineError::ActorSequenceGap {
                actor: record.actor,
                expected: *expected,
                found: record.actor_sequence,
            });
        }
        *expected += 1;
        timeline.last_stream_sequence = record.stream_sequence;
        match &record.event {
            ActorEvent::Created { .. } => {}
            ActorEvent::Started { .. }
            | ActorEvent::HaskellCompiled { .. }
            | ActorEvent::EffectSettled { .. }
            | ActorEvent::SuspensionAnswerAttempt { .. }
            | ActorEvent::ConversationForked { .. }
            | ActorEvent::MailboxAccepted { .. }
            | ActorEvent::MailboxDequeued { .. }
            | ActorEvent::CallSettled { .. }
            | ActorEvent::WaitRegistered { .. }
            | ActorEvent::WaitSettled { .. } => {}
            ActorEvent::Ready => timeline.lifecycle = TimelineLifecycle::Ready,
            ActorEvent::Exited { kind, summary, .. } => {
                timeline.lifecycle = TimelineLifecycle::Exited;
                timeline.terminal = Some(ActorTerminal {
                    kind: *kind,
                    summary: summary.clone(),
                });
            }
            ActorEvent::ModelMessage { .. } => timeline.model_messages += 1,
            ActorEvent::HaskellBatchStarted { .. } => timeline.haskell_batches += 1,
            ActorEvent::Suspended { suspension, .. } => {
                timeline.active_suspensions.insert(suspension.clone());
            }
            ActorEvent::Resumed { suspension } => {
                timeline.active_suspensions.remove(suspension);
            }
        }
        Ok(())
    }
}

fn event_name(event: &ActorEvent) -> &'static str {
    match event {
        ActorEvent::Created { .. } => "created",
        ActorEvent::Started { .. } => "started",
        ActorEvent::Ready => "ready",
        ActorEvent::Exited { .. } => "exited",
        ActorEvent::ModelMessage { .. } => "model_message",
        ActorEvent::MailboxAccepted { .. } => "mailbox_accepted",
        ActorEvent::MailboxDequeued { .. } => "mailbox_dequeued",
        ActorEvent::CallSettled { .. } => "call_settled",
        ActorEvent::WaitRegistered { .. } => "wait_registered",
        ActorEvent::WaitSettled { .. } => "wait_settled",
        ActorEvent::ConversationForked { .. } => "conversation_forked",
        ActorEvent::HaskellBatchStarted { .. } => "haskell_batch_started",
        ActorEvent::HaskellCompiled { .. } => "haskell_compiled",
        ActorEvent::EffectSettled { .. } => "effect_settled",
        ActorEvent::Suspended { .. } => "suspended",
        ActorEvent::SuspensionAnswerAttempt { .. } => "suspension_answer_attempt",
        ActorEvent::Resumed { .. } => "resumed",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ActorDescriptor, ActorExitKind, ActorPlacement, ActorRegistry, ActorTerminal,
        StartInitiator,
    };
    use tidepool_codegen::{scope::ScopeId, suspension::RealmId};

    #[test]
    fn folds_registry_events_into_a_retained_timeline() {
        let registry = ActorRegistry::new();
        let starting = registry
            .begin_start(
                None,
                ActorDescriptor {
                    label: "reviewer".into(),
                    effect_stack: vec!["Deliberate".into()],
                    placement: ActorPlacement {
                        session: tidepool_repr::SessionId(1),
                        resource_scope: RealmId::ROOT,
                        lexical_scope: ScopeId::ROOT,
                    },
                },
                StartInitiator::Runtime,
            )
            .expect("start actor");
        let actor = registry.publish_ready(starting).expect("ready actor");
        registry
            .finish(
                actor,
                ActorTerminal {
                    kind: ActorExitKind::Completed,
                    summary: "review complete".into(),
                },
            )
            .expect("finish actor");

        let events = registry.events();
        let timelines = ActorTimelines::fold(&events).expect("fold events");
        let timeline = timelines.get(actor).expect("actor timeline");
        assert_eq!(timeline.label, "reviewer");
        assert_eq!(timeline.lifecycle, TimelineLifecycle::Exited);
        assert_eq!(
            timeline.terminal,
            Some(ActorTerminal {
                kind: ActorExitKind::Completed,
                summary: "review complete".into(),
            })
        );
    }
}
