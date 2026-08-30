//! Lossless semantic projection from the historical harness journal into the
//! neutral actor-event vocabulary. The old file remains the durable wire
//! format during migration; this adapter lets observability consumers move
//! without dual-writing or changing replay behavior.

use std::collections::HashMap;
use std::path::Path;

use tidepool_actor::{
    ActorEvent, ActorEventRecord, ActorExitKind, ActorId, ActorRef, ActorRole, AnswerDisposition,
    EventCausality, ModelUsage, StartInitiator,
};

use super::{Actor, AnswerOutcome, Event, EventRecord, LogHeader, LogReader, ReadError};
use crate::provider::{Role, Usage};
use crate::tree::NodeId;

/// Stateful because the historical journal has one file-global sequence while
/// actor observability additionally requires a sequence per exact actor.
#[derive(Debug, Default)]
pub struct ActorEventAdapter {
    next_actor_sequence: HashMap<ActorRef, u64>,
}

impl ActorEventAdapter {
    pub fn adapt(&mut self, record: &EventRecord) -> ActorEventRecord {
        let actor = actor_ref(event_node(&record.event));
        let actor_sequence = self.next_actor_sequence.entry(actor).or_default();
        let sequence = *actor_sequence;
        *actor_sequence += 1;

        let (causality, event) = adapt_event(&record.event);
        ActorEventRecord {
            stream_sequence: record.seq,
            actor_sequence: sequence,
            actor,
            causality,
            event,
        }
    }
}

/// Read a historical harness journal as the neutral actor event stream. This
/// is the migration seam for timelines and UI consumers; replay continues to
/// read the original schema directly.
pub fn read_actor_events(
    path: impl AsRef<Path>,
) -> Result<(LogHeader, Vec<ActorEventRecord>), ReadError> {
    let (header, records) = LogReader::open(path)?;
    let mut adapter = ActorEventAdapter::default();
    let events = records
        .map(|record| record.map(|record| adapter.adapt(&record)))
        .collect::<Result<Vec<_>, _>>()?;
    Ok((header, events))
}

fn actor_ref(node: NodeId) -> ActorRef {
    ActorRef::first(ActorId(node.0))
}

fn event_node(event: &Event) -> NodeId {
    match event {
        Event::NodeCreated { node, .. }
        | Event::Forced { node, .. }
        | Event::TurnStart { node, .. }
        | Event::TurnExtracted { node, .. }
        | Event::Effect { node, .. }
        | Event::HolePublished { node, .. }
        | Event::HoleAnswerAttempt { node, .. }
        | Event::HoleConsumed { node, .. }
        | Event::NodeDone { node, .. }
        | Event::NodeCancelled { node, .. }
        | Event::TurnDelta { node, .. }
        | Event::TurnForked { node, .. }
        | Event::TurnSpliced { node, .. } => *node,
    }
}

fn adapt_event(event: &Event) -> (EventCausality, ActorEvent) {
    let mut causality = EventCausality::default();
    let event = match event {
        Event::NodeCreated {
            parent,
            teaser,
            effect_row,
            ..
        } => {
            let owner = parent.map(actor_ref);
            causality.owner = owner;
            ActorEvent::Created {
                owner,
                label: teaser.clone(),
                effect_stack: effect_row.clone(),
            }
        }
        Event::Forced { actor, .. } => ActorEvent::Started {
            initiator: match actor {
                Actor::Operator => StartInitiator::Operator,
                Actor::Policy => StartInitiator::Policy,
            },
        },
        Event::TurnStart { source, input, .. } => ActorEvent::HaskellBatchStarted {
            source: source.clone(),
            input: input.clone(),
        },
        Event::TurnExtracted { asks, bound, .. } => ActorEvent::HaskellCompiled {
            asks: asks.clone(),
            bound: bound.clone(),
        },
        Event::Effect {
            seq,
            tag,
            req,
            resp,
            ..
        } => ActorEvent::EffectSettled {
            sequence: *seq,
            tag: tag.clone(),
            request: req.clone(),
            response: resp.clone(),
        },
        Event::HolePublished {
            hole,
            site,
            ty,
            prompt,
            fork,
            ..
        } => {
            causality.operation = Some(hole.0.clone());
            ActorEvent::Suspended {
                suspension: hole.0.clone(),
                site: site.map(|site| site.get()),
                answer_type: ty.clone(),
                prompt: prompt.clone(),
                fork: *fork,
            }
        }
        Event::HoleAnswerAttempt {
            hole,
            source,
            outcome,
            ..
        } => {
            causality.operation = Some(hole.0.clone());
            ActorEvent::SuspensionAnswerAttempt {
                suspension: hole.0.clone(),
                source: source.clone(),
                disposition: match outcome {
                    AnswerOutcome::Consumed => AnswerDisposition::Consumed,
                    AnswerOutcome::Rejected { error } => AnswerDisposition::Rejected {
                        error: error.clone(),
                    },
                },
            }
        }
        Event::HoleConsumed { hole, .. } => {
            causality.operation = Some(hole.0.clone());
            ActorEvent::Resumed {
                suspension: hole.0.clone(),
            }
        }
        Event::NodeDone {
            result_rendered, ..
        } => ActorEvent::Exited {
            kind: ActorExitKind::Completed,
            summary: result_rendered.clone(),
            owner_observing: false,
        },
        Event::NodeCancelled { reason, .. } => ActorEvent::Exited {
            kind: ActorExitKind::Cancelled,
            summary: reason.clone(),
            owner_observing: false,
        },
        Event::TurnDelta {
            turn,
            role,
            content,
            usage,
            reasoning,
            ..
        } => {
            causality.turn = Some(*turn);
            ActorEvent::ModelMessage {
                turn: *turn,
                role: adapt_role(role),
                content: content.clone(),
                usage: usage.map(adapt_usage),
                reasoning: reasoning.clone(),
                injected: false,
            }
        }
        Event::TurnForked {
            parent,
            parent_turn,
            ..
        } => {
            causality.owner = Some(actor_ref(*parent));
            causality.turn = Some(*parent_turn);
            ActorEvent::ConversationForked {
                parent: actor_ref(*parent),
                parent_turn: *parent_turn,
            }
        }
        Event::TurnSpliced {
            turn,
            role,
            content,
            ..
        } => {
            causality.turn = Some(*turn);
            ActorEvent::ModelMessage {
                turn: *turn,
                role: adapt_role(role),
                content: content.clone(),
                usage: None,
                reasoning: None,
                injected: true,
            }
        }
    };
    (causality, event)
}

fn adapt_role(role: &Role) -> ActorRole {
    match role {
        Role::System => ActorRole::System,
        Role::Developer => ActorRole::Developer,
        Role::User => ActorRole::User,
        Role::Assistant => ActorRole::Assistant,
    }
}

fn adapt_usage(usage: Usage) -> ModelUsage {
    ModelUsage {
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        cached_input_tokens: usage.cached_input_tokens,
        cache_write_tokens: usage.cache_write_tokens,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::log::Event;

    #[test]
    fn preserves_global_order_and_assigns_per_actor_order() {
        let records = [
            EventRecord {
                seq: 4,
                event: Event::NodeCreated {
                    node: NodeId(1),
                    parent: None,
                    teaser: "root".into(),
                    effect_row: vec!["Llm".into()],
                    fan: crate::tree::FanBadge::Exact { n: 0 },
                    price: crate::tree::PriceClass::Frontier,
                },
            },
            EventRecord {
                seq: 5,
                event: Event::NodeCreated {
                    node: NodeId(2),
                    parent: Some(NodeId(1)),
                    teaser: "child".into(),
                    effect_row: vec![],
                    fan: crate::tree::FanBadge::Exact { n: 0 },
                    price: crate::tree::PriceClass::Zero,
                },
            },
            EventRecord {
                seq: 6,
                event: Event::Forced {
                    node: NodeId(1),
                    actor: Actor::Operator,
                },
            },
        ];

        let mut adapter = ActorEventAdapter::default();
        let adapted: Vec<_> = records.iter().map(|record| adapter.adapt(record)).collect();
        assert_eq!(adapted[0].stream_sequence, 4);
        assert_eq!(adapted[1].stream_sequence, 5);
        assert_eq!(adapted[2].stream_sequence, 6);
        assert_eq!(adapted[0].actor_sequence, 0);
        assert_eq!(adapted[1].actor_sequence, 0);
        assert_eq!(adapted[2].actor_sequence, 1);
        assert_eq!(adapted[1].causality.owner, Some(actor_ref(NodeId(1))));
    }
}
