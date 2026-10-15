//! Browser observations derived from the actor forest and embedded model owners.

use std::collections::{BTreeMap, HashMap};

use exomonad_actor::{ActorExitKind, ActorGraphNode, ActorRef, ActorWorkbenchPosture};
use harness::{
    embedding::{EmbeddedRoundId, HostIdentity, HostInferenceState},
    model::AgentPath,
    server::{
        browser_contract::{ConversationProjection, ConversationState},
        HostActorKind, HostActorLifecycle, HostActorProjection, ServerControl,
    },
};
use tokio::sync::watch;

fn workbench_observation(
    posture: &ActorWorkbenchPosture,
) -> harness::server::browser_contract::HostWorkbenchObservation {
    use harness::server::browser_contract::{
        HostWorkbenchObservation as Wire, HostWorkbenchTransfer,
    };
    match posture {
        ActorWorkbenchPosture::Idle => Wire::Idle,
        ActorWorkbenchPosture::RunningUnit {
            input_unit_index,
            total,
        } => Wire::RunningUnit {
            input_unit_index: (*input_unit_index as u64).into(),
            total: (*total as u64).into(),
        },
        ActorWorkbenchPosture::AwaitingEffect {
            input_unit_index,
            total,
            effect,
        } => Wire::AwaitingEffect {
            input_unit_index: (*input_unit_index as u64).into(),
            total: (*total as u64).into(),
            operation: effect.clone(),
        },
        ActorWorkbenchPosture::TerminalTransfer { transfer } => Wire::TerminalTransfer {
            transfer: match transfer {
                exomonad_actor::ActorWorkbenchTransfer::Reply => HostWorkbenchTransfer::Reply,
                exomonad_actor::ActorWorkbenchTransfer::CancellationAcknowledgement => {
                    HostWorkbenchTransfer::CancellationAcknowledgement
                }
            },
        },
        ActorWorkbenchPosture::Failed => Wire::Failed,
    }
}

fn form_waits(node: &ActorGraphNode) -> harness::server::browser_contract::HostFormWaits {
    use harness::server::browser_contract::{ActorOutputExecution, HostFormWait, HostFormWaits};
    const MAX_WAITS: usize = 64;
    HostFormWaits {
        omitted: (node.form_waits.len().saturating_sub(MAX_WAITS) as u64).into(),
        waits: node
            .form_waits
            .iter()
            .take(MAX_WAITS)
            .map(|wait| HostFormWait {
                mount_id: wait.mount_id.clone(),
                execution: match &wait.operation {
                    Some(operation) => ActorOutputExecution::Notebook {
                        execution: operation.execution.to_string(),
                        input_unit_index: (operation.input_unit_index as u64).into(),
                        effect_ordinal: (operation.effect_ordinal as u64).into(),
                    },
                    None => ActorOutputExecution::ActorProgram,
                },
                since_unix_ms: wait.since_unix_ms.into(),
            })
            .collect(),
    }
}

pub(super) type LifecycleState = BTreeMap<ActorRef, HostActorLifecycle>;

/// Keep the latest state for each model actor when several drivers update
/// before the host processes the watch notification.
#[derive(Clone)]
pub(super) struct LifecycleSender(watch::Sender<LifecycleState>);

pub(super) trait LifecyclePublisher: Send {
    fn publish(&self, actor: ActorRef, lifecycle: HostActorLifecycle);
}

impl LifecycleSender {
    pub(super) fn channel() -> (Self, watch::Receiver<LifecycleState>) {
        let (sender, receiver) = watch::channel(BTreeMap::new());
        (Self(sender), receiver)
    }
}

impl LifecyclePublisher for LifecycleSender {
    fn publish(&self, actor: ActorRef, lifecycle: HostActorLifecycle) {
        self.0.send_modify(|states| {
            let previous = states.get(&actor).copied();
            if matches!(
                previous,
                Some(HostActorLifecycle::Retired | HostActorLifecycle::Lost)
            ) {
                return;
            }
            if previous == Some(HostActorLifecycle::Retiring)
                && matches!(
                    lifecycle,
                    HostActorLifecycle::Running | HostActorLifecycle::Waiting
                )
            {
                return;
            }
            states.insert(actor, lifecycle);
        });
    }
}

// Focused driver tests observe a single conversation directly.
#[cfg(test)]
impl LifecyclePublisher for watch::Sender<(Option<ActorRef>, HostActorLifecycle)> {
    fn publish(&self, actor: ActorRef, lifecycle: HostActorLifecycle) {
        self.send_replace((Some(actor), lifecycle));
    }
}

#[derive(Default)]
pub(super) struct EmbeddedProjection {
    // Conversation identity is immutable. Keep it after its Engine task leaves
    // so a terminal actor remains linked to its exact history head in the snapshot.
    // Cold restart cannot reconstruct these associations from logical paths alone.
    conversations: HashMap<ActorRef, ConversationObservation>,
    published_actors: Option<watch::Sender<Vec<HostActorProjection>>>,
}

struct ConversationObservation {
    identity: HostIdentity,
    head: Option<String>,
    frozen: bool,
    inference: Option<HostInferenceState>,
}

impl EmbeddedProjection {
    pub(super) fn attached(&mut self, actor: ActorRef, identity: &HostIdentity) {
        self.conversations
            .entry(actor)
            .or_insert_with(|| ConversationObservation {
                identity: identity.clone(),
                head: None,
                frozen: false,
                inference: None,
            });
    }

    pub(super) fn resolve_identity(
        &self,
        run: &str,
        target: &HostIdentity,
        nodes: &[ActorGraphNode],
    ) -> Option<ActorRef> {
        if target.run != run {
            return None;
        }
        let mut matches = nodes
            .iter()
            .filter(|node| !node.model_actor || self.conversations.contains_key(&node.actor))
            .filter(|node| self.identity(run, node.actor) == *target)
            .map(|node| node.actor);
        let actor = matches.next()?;
        matches.next().is_none().then_some(actor)
    }

    fn identity(&self, run: &str, actor: ActorRef) -> HostIdentity {
        self.conversations
            .get(&actor)
            .map(|conversation| conversation.identity.clone())
            .unwrap_or_else(|| HostIdentity {
                run: run.to_owned(),
                actor: AgentPath(format!("/actors/a{}_i{}", actor.id.0, actor.incarnation.0)),
                incarnation: actor.incarnation.0.to_string(),
            })
    }

    fn projection(
        &mut self,
        run: &str,
        nodes: &[ActorGraphNode],
        states: &LifecycleState,
        active_round_for: impl Fn(ActorRef) -> Option<EmbeddedRoundId>,
        head_for: impl Fn(&HostIdentity) -> Result<Option<String>, harness::store::StoreError>,
        history_finished_for: impl Fn(ActorRef) -> bool,
    ) -> Result<(Vec<HostActorProjection>, Vec<ConversationProjection>), harness::store::StoreError>
    {
        let mut actors = Vec::new();
        let mut conversations = Vec::new();
        for node in nodes {
            if node.model_actor && !self.conversations.contains_key(&node.actor) {
                continue;
            }
            let identity = self.identity(run, node.actor);
            let lifecycle = match node.terminal().map(|terminal| terminal.kind) {
                Some(ActorExitKind::Completed | ActorExitKind::Cancelled) => {
                    HostActorLifecycle::Retired
                }
                Some(ActorExitKind::Failed | ActorExitKind::Interrupted) => {
                    HostActorLifecycle::Lost
                }
                None => states.get(&node.actor).copied().unwrap_or_else(|| {
                    if matches!(
                        node.workbench,
                        ActorWorkbenchPosture::RunningUnit { .. }
                            | ActorWorkbenchPosture::AwaitingEffect { .. }
                    ) {
                        HostActorLifecycle::Running
                    } else {
                        HostActorLifecycle::Waiting
                    }
                }),
            };
            let terminal = matches!(
                lifecycle,
                HostActorLifecycle::Retired | HostActorLifecycle::Lost
            );
            let observation = self.conversations.get_mut(&node.actor);
            let (conversation, model_head_request, inference) =
                if let Some(observation) = observation {
                    if !observation.frozen {
                        match head_for(&observation.identity) {
                            Ok(head) => {
                                if head.is_some() {
                                    observation.head = head;
                                }
                            }
                            Err(harness::store::StoreError::InvalidEmbeddedBinding) => {
                                observation.frozen = true;
                            }
                            Err(error) => return Err(error),
                        }
                        // Kernel retirement can precede the driver committing its last request.
                        observation.frozen |= terminal && history_finished_for(node.actor);
                    }
                    (
                        Some(observation.identity.actor.0.clone()),
                        observation.head.clone(),
                        observation.inference.clone(),
                    )
                } else {
                    (None, None, None)
                };
            actors.push(HostActorProjection {
                workbench: Some(workbench_observation(&node.workbench)),
                form_waits: Some(form_waits(node)),
                identity,
                output_origin: Some(
                    (&harness::store::actor_output::ActorOutputOrigin {
                        run: run.to_owned(),
                        native_actor: node.actor.id.0,
                        incarnation: node.actor.incarnation.0,
                    })
                        .into(),
                ),
                parent: node
                    .supervisor_parent
                    .or(node.creator)
                    .map(|parent| self.identity(run, parent)),
                kind: if node.model_actor {
                    HostActorKind::Model
                } else {
                    HostActorKind::Workflow
                },
                lifecycle,
                inference: inference.as_ref().map(Into::into),
                active_round: if matches!(
                    lifecycle,
                    HostActorLifecycle::Retired | HostActorLifecycle::Lost
                ) {
                    None
                } else {
                    active_round_for(node.actor)
                },
                model_conversation: conversation.clone(),
                model_head_request,
            });
            if let Some(path) = conversation {
                conversations.push(ConversationProjection {
                    id: path.clone(),
                    path,
                    parent_id: None,
                    fork_source_request_id: None,
                    state: conversation_state(lifecycle),
                    version: None,
                });
            }
        }
        Ok((actors, conversations))
    }

    pub(super) fn publish(
        &mut self,
        control: &ServerControl,
        observation: &tokio::sync::mpsc::UnboundedSender<super::ActorHostReadiness>,
        run: &str,
        nodes: &[ActorGraphNode],
        states: &LifecycleState,
        active_round_for: impl Fn(ActorRef) -> Option<EmbeddedRoundId>,
        head_for: impl Fn(&HostIdentity) -> Result<Option<String>, harness::store::StoreError>,
        inference_for: impl Fn(&HostIdentity) -> Result<HostInferenceState, harness::store::StoreError>,
        history_finished_for: impl Fn(ActorRef) -> bool,
    ) -> Result<(), harness::store::StoreError> {
        for node in nodes {
            if let Some(observation) = self.conversations.get_mut(&node.actor) {
                observation.inference = if node.terminal().is_some() || observation.frozen {
                    None
                } else {
                    match inference_for(&observation.identity) {
                        Ok(inference) => Some(inference),
                        Err(harness::store::StoreError::InvalidEmbeddedBinding) => None,
                        Err(error) => return Err(error),
                    }
                };
            }
        }
        let (actors, conversations) = self.projection(
            run,
            nodes,
            states,
            active_round_for,
            head_for,
            history_finished_for,
        )?;
        if let Some(published) = &self.published_actors {
            published.send_if_modified(|previous| {
                if *previous == actors {
                    return false;
                }
                previous.clone_from(&actors);
                true
            });
        } else {
            let (published, actors) = watch::channel(actors.clone());
            observation
                .send(super::ActorHostReadiness::ActorsObserved { actors })
                .ok();
            self.published_actors = Some(published);
        }
        control.update_host_projection(run.to_owned(), actors, conversations);
        Ok(())
    }
}

fn conversation_state(lifecycle: HostActorLifecycle) -> ConversationState {
    match lifecycle {
        HostActorLifecycle::Running => ConversationState::Requesting,
        HostActorLifecycle::Waiting => ConversationState::Idle,
        HostActorLifecycle::Retiring => ConversationState::Paused,
        HostActorLifecycle::Retired | HostActorLifecycle::Lost => ConversationState::Cancelled,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use exomonad_actor::{ActorId, ActorTerminal, Incarnation};

    fn actor(id: u64) -> ActorRef {
        ActorRef {
            id: ActorId(id),
            incarnation: Incarnation::FIRST,
        }
    }

    fn node(id: u64, parent: Option<ActorRef>, model_actor: bool) -> ActorGraphNode {
        ActorGraphNode {
            actor: actor(id),
            label: format!("actor-{id}"),
            model_actor,
            creator: parent,
            supervisor_parent: parent,
            context_parent: None,
            lifecycle: Some(exomonad_actor::ActorLifecycle::Live),
            workbench: ActorWorkbenchPosture::Idle,
            form_waits: vec![],
            provider_thread: None,
            provider_turn: None,
            provider_observation_stale: false,
            bound_worktree: None,
            active_requests: Vec::new(),
            queued_requests: Vec::new(),
        }
    }

    #[test]
    fn coalesced_driver_updates_preserve_siblings_and_terminal_fence() {
        let (sender, receiver) = LifecycleSender::channel();
        sender.publish(actor(1), HostActorLifecycle::Running);
        sender.publish(actor(2), HostActorLifecycle::Retired);
        sender.publish(actor(2), HostActorLifecycle::Waiting);
        assert_eq!(receiver.borrow().len(), 2);
        assert_eq!(receiver.borrow()[&actor(1)], HostActorLifecycle::Running);
        assert_eq!(receiver.borrow()[&actor(2)], HostActorLifecycle::Retired);
    }

    #[test]
    fn host_observation_reports_actor_loss_and_suppresses_unchanged_projection() {
        let (_, control, _) = harness::server::server(std::path::PathBuf::new());
        let (observations, mut received) = tokio::sync::mpsc::unbounded_channel();
        let mut projection = EmbeddedProjection::default();
        let identity = HostIdentity {
            run: "run".into(),
            actor: AgentPath("/root".into()),
            incarnation: "1".into(),
        };
        projection.attached(actor(1), &identity);
        let mut root = node(1, None, true);
        for _ in 0..2 {
            projection
                .publish(
                    &control,
                    &observations,
                    "run",
                    &[root.clone()],
                    &LifecycleState::default(),
                    |_| None,
                    |_| Ok(Some("retained-head".into())),
                    |_| Ok(HostInferenceState::Idle),
                    |_| false,
                )
                .unwrap();
        }
        let super::super::ActorHostReadiness::ActorsObserved { mut actors } =
            received.try_recv().unwrap()
        else {
            panic!("the projection must publish its typed actor observation");
        };
        assert_eq!(actors.borrow()[0].lifecycle, HostActorLifecycle::Waiting);
        assert_eq!(
            actors.borrow()[0].inference,
            Some((&HostInferenceState::Idle).into())
        );
        assert!(!actors.has_changed().unwrap());
        assert!(received.try_recv().is_err());
        let block = harness::embedding::InferenceBlock {
            request: harness::model::RequestId("retained-head".into()),
            generation: 1,
            reason: harness::embedding::InferenceBlockReason::ContinuityUnproven,
            continuation_refusal: None,
        };
        projection
            .publish(
                &control,
                &observations,
                "run",
                &[root.clone()],
                &LifecycleState::default(),
                |_| None,
                |_| Ok(Some("retained-head".into())),
                |_| {
                    Ok(HostInferenceState::Blocked {
                        block: block.clone(),
                    })
                },
                |_| false,
            )
            .unwrap();
        assert!(received.try_recv().is_err());
        assert!(actors.has_changed().unwrap());
        assert_eq!(
            actors.borrow_and_update()[0].lifecycle,
            HostActorLifecycle::Waiting
        );
        assert_eq!(
            actors.borrow()[0].inference,
            Some((&HostInferenceState::Blocked { block }).into())
        );
        root.lifecycle = Some(exomonad_actor::ActorLifecycle::Exited(ActorTerminal {
            kind: ActorExitKind::Failed,
            summary: "runtime failed".into(),
            diagnostic: None,
        }));
        projection
            .publish(
                &control,
                &observations,
                "run",
                &[root],
                &LifecycleState::default(),
                |_| None,
                |_| Ok(Some("retained-head".into())),
                |_| Ok(HostInferenceState::Idle),
                |_| true,
            )
            .unwrap();
        assert!(received.try_recv().is_err());
        assert!(actors.has_changed().unwrap());
        let actors = actors.borrow_and_update();
        assert_eq!(actors[0].lifecycle, HostActorLifecycle::Lost);
        assert_eq!(actors[0].inference, None);
        assert_eq!(
            actors[0].model_head_request.as_deref(),
            Some("retained-head")
        );
    }

    #[test]
    fn projects_model_children_and_workflow_parent_without_conversation() {
        let mut projection = EmbeddedProjection::default();
        let child_identity = HostIdentity {
            run: "run".into(),
            actor: AgentPath("/root/child".into()),
            incarnation: "1".into(),
        };
        projection.attached(actor(2), &child_identity);
        let (actors, conversations) = projection
            .projection(
                "run",
                &[node(1, None, false), node(2, Some(actor(1)), true)],
                &LifecycleState::new(),
                |_| None,
                |_| Ok(None),
                |_| true,
            )
            .unwrap();
        assert_eq!(actors.len(), 2);
        assert_eq!(actors[0].kind, HostActorKind::Workflow);
        assert_eq!(actors[0].model_conversation, None);
        assert_eq!(actors[1].identity, child_identity);
        assert_eq!(actors[1].parent, Some(actors[0].identity.clone()));
        assert_eq!(conversations.len(), 1);
    }

    #[test]
    fn paused_lifecycle_preserves_browser_projection_classification() {
        for lifecycle in [
            None,
            Some(exomonad_actor::ActorLifecycle::Live),
            Some(exomonad_actor::ActorLifecycle::Paused(
                "retained input".into(),
            )),
        ] {
            let mut root = node(1, None, false);
            root.lifecycle = lifecycle;
            let mut projection = EmbeddedProjection::default();
            let mut states = LifecycleState::new();
            for (state, expected) in [
                (None, HostActorLifecycle::Waiting),
                (
                    Some(HostActorLifecycle::Running),
                    HostActorLifecycle::Running,
                ),
            ] {
                if let Some(state) = state {
                    states.insert(root.actor, state);
                }
                let (actors, _) = projection
                    .projection(
                        "run",
                        &[root.clone()],
                        &states,
                        |_| None,
                        |_| Ok(None),
                        |_| true,
                    )
                    .unwrap();
                assert_eq!(actors[0].lifecycle, expected);
            }
        }
    }

    #[test]
    fn terminal_root_remains_projected_with_a_live_child() {
        let mut projection = EmbeddedProjection::default();
        let root_identity = HostIdentity {
            run: "run".into(),
            actor: AgentPath("/root".into()),
            incarnation: "1".into(),
        };
        let child_identity = HostIdentity {
            run: "run".into(),
            actor: AgentPath("/root/1".into()),
            incarnation: "1".into(),
        };
        projection.attached(actor(1), &root_identity);
        projection.attached(actor(2), &child_identity);

        let mut root = node(1, None, true);
        root.lifecycle = Some(exomonad_actor::ActorLifecycle::Exited(ActorTerminal {
            kind: ActorExitKind::Completed,
            summary: "root completed".into(),
            diagnostic: None,
        }));
        let child = node(2, Some(actor(1)), true);
        let (actors, conversations) = projection
            .projection(
                "run",
                &[root, child],
                &LifecycleState::default(),
                |_| None,
                |_| Ok(None),
                |_| true,
            )
            .unwrap();

        assert_eq!(actors.len(), 2);
        let projected_root = actors
            .iter()
            .find(|projected| projected.identity == root_identity)
            .expect("terminal root remains visible in the host graph");
        assert_eq!(projected_root.lifecycle, HostActorLifecycle::Retired);
        let projected_child = actors
            .iter()
            .find(|projected| projected.identity == child_identity)
            .expect("live child remains visible after its parent terminates");
        assert_eq!(projected_child.parent, Some(root_identity));
        assert_eq!(projected_child.lifecycle, HostActorLifecycle::Waiting);
        assert_eq!(conversations.len(), 2);
        assert!(conversations.iter().any(|conversation| {
            conversation.id == "/root" && conversation.state == ConversationState::Cancelled
        }));
        assert!(conversations.iter().any(|conversation| {
            conversation.id == "/root/1" && conversation.state == ConversationState::Idle
        }));
    }

    #[test]
    fn active_round_is_observed_from_its_owner_and_terminal_state_clears_it() {
        let mut projection = EmbeddedProjection::default();
        let identity = HostIdentity {
            run: "run".into(),
            actor: AgentPath("/root".into()),
            incarnation: "1".into(),
        };
        projection.attached(actor(1), &identity);
        let round = EmbeddedRoundId(uuid::Uuid::new_v4());
        let mut root = node(1, None, true);
        let (actors, _) = projection
            .projection(
                "run",
                std::slice::from_ref(&root),
                &LifecycleState::default(),
                |_| Some(round),
                |_| Ok(None),
                |_| true,
            )
            .unwrap();
        assert_eq!(actors[0].active_round, Some(round));
        let (actors, _) = projection
            .projection(
                "run",
                std::slice::from_ref(&root),
                &LifecycleState::default(),
                |_| None,
                |_| Ok(None),
                |_| true,
            )
            .unwrap();
        assert_eq!(actors[0].active_round, None);
        root.lifecycle = Some(exomonad_actor::ActorLifecycle::Exited(ActorTerminal {
            kind: ActorExitKind::Cancelled,
            summary: "retired".into(),
            diagnostic: None,
        }));
        let (actors, _) = projection
            .projection(
                "run",
                &[root],
                &LifecycleState::default(),
                |_| Some(round),
                |_| Ok(None),
                |_| true,
            )
            .unwrap();
        assert_eq!(actors[0].active_round, None);
    }

    #[test]
    fn terminal_head_is_retained_and_observed_only_once() {
        for kind in [
            ActorExitKind::Cancelled,
            ActorExitKind::Failed,
            ActorExitKind::Interrupted,
        ] {
            let mut projection = EmbeddedProjection::default();
            let identity = HostIdentity {
                run: "run".into(),
                actor: AgentPath("/root/old".into()),
                incarnation: "1".into(),
            };
            projection.attached(actor(1), &identity);
            let mut root = node(1, None, true);
            let (actors, _) = projection
                .projection(
                    "run",
                    std::slice::from_ref(&root),
                    &LifecycleState::default(),
                    |_| None,
                    |_| Ok(None),
                    |_| true,
                )
                .unwrap();
            assert_eq!(actors[0].model_head_request, None);
            let (actors, _) = projection
                .projection(
                    "run",
                    std::slice::from_ref(&root),
                    &LifecycleState::default(),
                    |_| None,
                    |observed| {
                        assert_eq!(observed, &identity);
                        Ok(Some("first-head".into()))
                    },
                    |_| true,
                )
                .unwrap();
            assert_eq!(actors[0].model_head_request.as_deref(), Some("first-head"));
            root.lifecycle = Some(exomonad_actor::ActorLifecycle::Exited(ActorTerminal {
                kind,
                summary: "terminal".into(),
                diagnostic: None,
            }));
            let (actors, _) = projection
                .projection(
                    "run",
                    std::slice::from_ref(&root),
                    &LifecycleState::default(),
                    |_| None,
                    |_| Ok(Some("final-head".into())),
                    |_| true,
                )
                .unwrap();
            assert_eq!(actors[0].model_head_request.as_deref(), Some("final-head"));
            assert_eq!(
                actors[0].lifecycle,
                match kind {
                    ActorExitKind::Cancelled => HostActorLifecycle::Retired,
                    ActorExitKind::Failed | ActorExitKind::Interrupted => HostActorLifecycle::Lost,
                    ActorExitKind::Completed => unreachable!(),
                }
            );
            // Subsequent publications do no Store query for the immutable terminal head.
            for _ in 0..130 {
                let (actors, _) = projection
                    .projection(
                        "run",
                        std::slice::from_ref(&root),
                        &LifecycleState::default(),
                        |_| None,
                        |_| panic!("terminal head was queried again"),
                        |_| true,
                    )
                    .unwrap();
                assert_eq!(actors[0].model_head_request.as_deref(), Some("final-head"));
            }
        }
    }

    #[test]
    fn terminal_actor_waits_for_driver_cleanup_before_freezing_head() {
        let mut projection = EmbeddedProjection::default();
        let identity = HostIdentity {
            run: "run".into(),
            actor: AgentPath("/root".into()),
            incarnation: "1".into(),
        };
        projection.attached(actor(1), &identity);
        let mut root = node(1, None, true);
        root.lifecycle = Some(exomonad_actor::ActorLifecycle::Exited(ActorTerminal {
            kind: ActorExitKind::Cancelled,
            summary: "cancelled".into(),
            diagnostic: None,
        }));
        let (actors, _) = projection
            .projection(
                "run",
                std::slice::from_ref(&root),
                &LifecycleState::default(),
                |_| None,
                |_| Ok(Some("previous-head".into())),
                |_| false,
            )
            .unwrap();
        assert_eq!(
            actors[0].model_head_request.as_deref(),
            Some("previous-head")
        );
        let (actors, _) = projection
            .projection(
                "run",
                std::slice::from_ref(&root),
                &LifecycleState::default(),
                |_| None,
                |_| Ok(Some("cancelled-final-head".into())),
                |_| true,
            )
            .unwrap();
        assert_eq!(
            actors[0].model_head_request.as_deref(),
            Some("cancelled-final-head")
        );
        let (actors, _) = projection
            .projection(
                "run",
                &[root],
                &LifecycleState::default(),
                |_| None,
                |_| panic!("finished driver queried again"),
                |_| true,
            )
            .unwrap();
        assert_eq!(
            actors[0].model_head_request.as_deref(),
            Some("cancelled-final-head")
        );
    }

    #[test]
    fn binding_loss_freezes_previous_head_and_attachment_identity() {
        let mut projection = EmbeddedProjection::default();
        let old = HostIdentity {
            run: "run".into(),
            actor: AgentPath("/root/old".into()),
            incarnation: "1".into(),
        };
        projection.attached(actor(1), &old);
        let root = node(1, None, true);
        projection
            .projection(
                "run",
                std::slice::from_ref(&root),
                &LifecycleState::default(),
                |_| None,
                |_| Ok(Some("old-head".into())),
                |_| true,
            )
            .unwrap();
        let successor = HostIdentity {
            incarnation: "2".into(),
            ..old.clone()
        };
        projection.attached(actor(1), &successor);
        let (actors, _) = projection
            .projection(
                "run",
                std::slice::from_ref(&root),
                &LifecycleState::default(),
                |_| None,
                |identity| {
                    assert_eq!(identity, &old);
                    Err(harness::store::StoreError::InvalidEmbeddedBinding)
                },
                |_| true,
            )
            .unwrap();
        assert_eq!(actors[0].identity, old);
        assert_eq!(actors[0].model_head_request.as_deref(), Some("old-head"));
        let (actors, _) = projection
            .projection(
                "run",
                &[root],
                &LifecycleState::default(),
                |_| None,
                |_| panic!("lost binding queried successor"),
                |_| true,
            )
            .unwrap();
        assert_eq!(actors[0].model_head_request.as_deref(), Some("old-head"));
    }

    #[test]
    fn unavailable_binding_without_observation_has_no_head_and_store_failure_propagates() {
        let mut projection = EmbeddedProjection::default();
        let identity = HostIdentity {
            run: "run".into(),
            actor: AgentPath("/root".into()),
            incarnation: "1".into(),
        };
        projection.attached(actor(1), &identity);
        let root = node(1, None, true);
        assert!(matches!(
            projection.projection(
                "run",
                std::slice::from_ref(&root),
                &LifecycleState::default(),
                |_| None,
                |_| Err(harness::store::StoreError::MissingRequest("corrupt".into())),
                |_| true
            ),
            Err(harness::store::StoreError::MissingRequest(_))
        ));
        let (actors, _) = projection
            .projection(
                "run",
                &[root],
                &LifecycleState::default(),
                |_| None,
                |_| Err(harness::store::StoreError::InvalidEmbeddedBinding),
                |_| true,
            )
            .unwrap();
        assert_eq!(actors[0].model_head_request, None);
    }

    #[test]
    fn resolves_only_the_exact_live_graph_identity() {
        let mut projection = EmbeddedProjection::default();
        let identity = HostIdentity {
            run: "run".into(),
            actor: AgentPath("/root/child".into()),
            incarnation: "1".into(),
        };
        projection.attached(actor(2), &identity);
        let nodes = [node(2, None, true)];
        assert_eq!(
            projection.resolve_identity("run", &identity, &nodes),
            Some(actor(2))
        );

        let mut stale = identity.clone();
        stale.incarnation = "2".into();
        assert_eq!(projection.resolve_identity("run", &stale, &nodes), None);
        assert_eq!(
            projection.resolve_identity("other-run", &identity, &nodes),
            None
        );
        assert_eq!(projection.resolve_identity("run", &identity, &[]), None);

        let unattached_identity = projection.identity("run", actor(3));
        assert_eq!(
            projection.resolve_identity("run", &unattached_identity, &[node(3, None, true)]),
            None
        );
    }
}
