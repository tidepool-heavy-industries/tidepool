//! Browser observations derived from the actor forest and embedded model owners.

use std::collections::{BTreeMap, HashMap};

use exomonad_actor::{ActorExitKind, ActorGraphNode, ActorRef, ActorWorkbenchPosture};
use harness::{
    embedding::{EmbeddedRoundId, HostIdentity},
    model::AgentPath,
    server::{HostActorKind, HostActorLifecycle, HostActorProjection, ServerControl},
};
use tokio::sync::watch;

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
}

struct ConversationObservation {
    identity: HostIdentity,
    head: Option<String>,
    frozen: bool,
}

impl EmbeddedProjection {
    pub(super) fn attached(&mut self, actor: ActorRef, identity: &HostIdentity) {
        self.conversations
            .entry(actor)
            .or_insert_with(|| ConversationObservation {
                identity: identity.clone(),
                head: None,
                frozen: false,
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
    ) -> Result<(Vec<HostActorProjection>, Vec<serde_json::Value>), harness::store::StoreError>
    {
        let mut actors = Vec::new();
        let mut conversations = Vec::new();
        for node in nodes {
            if node.model_actor && !self.conversations.contains_key(&node.actor) {
                continue;
            }
            let identity = self.identity(run, node.actor);
            let lifecycle = match node.terminal.as_ref().map(|terminal| terminal.kind) {
                Some(ActorExitKind::Completed | ActorExitKind::Cancelled) => {
                    HostActorLifecycle::Retired
                }
                Some(ActorExitKind::Failed) => HostActorLifecycle::Lost,
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
            let (conversation, model_head_request) = if let Some(observation) = observation {
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
                )
            } else {
                (None, None)
            };
            actors.push(HostActorProjection {
                identity,
                output_origin: Some(harness::store::actor_output::ActorOutputOrigin {
                    run: run.to_owned(),
                    native_actor: node.actor.id.0,
                    incarnation: node.actor.incarnation.0,
                }),
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
                conversations.push(serde_json::json!({
                    "id": path,
                    "path": path,
                    "state": conversation_state(lifecycle),
                }));
            }
        }
        Ok((actors, conversations))
    }

    pub(super) fn publish(
        &mut self,
        control: &ServerControl,
        run: &str,
        nodes: &[ActorGraphNode],
        states: &LifecycleState,
        active_round_for: impl Fn(ActorRef) -> Option<EmbeddedRoundId>,
        head_for: impl Fn(&HostIdentity) -> Result<Option<String>, harness::store::StoreError>,
        history_finished_for: impl Fn(ActorRef) -> bool,
    ) -> Result<(), harness::store::StoreError> {
        let (actors, conversations) = self.projection(
            run,
            nodes,
            states,
            active_round_for,
            head_for,
            history_finished_for,
        )?;
        control.update_host_projection(run.to_owned(), actors, conversations);
        Ok(())
    }
}

fn conversation_state(lifecycle: HostActorLifecycle) -> &'static str {
    match lifecycle {
        HostActorLifecycle::Running => "requesting",
        HostActorLifecycle::Waiting => "idle",
        HostActorLifecycle::Retiring => "paused",
        HostActorLifecycle::Retired | HostActorLifecycle::Lost => "cancelled",
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
            terminal: None,
            workbench: ActorWorkbenchPosture::Idle,
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
        root.terminal = Some(ActorTerminal {
            kind: ActorExitKind::Completed,
            summary: "root completed".into(),
            diagnostic: None,
        });
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
            conversation["id"] == "/root" && conversation["state"] == "cancelled"
        }));
        assert!(conversations.iter().any(|conversation| {
            conversation["id"] == "/root/1" && conversation["state"] == "idle"
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
        root.terminal = Some(ActorTerminal {
            kind: ActorExitKind::Cancelled,
            summary: "retired".into(),
            diagnostic: None,
        });
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
        for kind in [ActorExitKind::Cancelled, ActorExitKind::Failed] {
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
            root.terminal = Some(ActorTerminal {
                kind,
                summary: "terminal".into(),
                diagnostic: None,
            });
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
            assert!(matches!(
                actors[0].lifecycle,
                HostActorLifecycle::Retired | HostActorLifecycle::Lost
            ));
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
        root.terminal = Some(ActorTerminal {
            kind: ActorExitKind::Cancelled,
            summary: "cancelled".into(),
            diagnostic: None,
        });
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
