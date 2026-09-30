//! Provider ancestry for a fresh conversation admitted by the actor kernel.

use std::collections::{HashMap, HashSet};

use exomonad_actor::{ActorGraphNode, ActorRef};
use harness::{embedding::HostIdentity, model::AgentPath};

pub(super) struct SelectedProviderParent(HostIdentity);

impl SelectedProviderParent {
    pub(super) fn identity(&self) -> &HostIdentity {
        &self.0
    }

    pub(super) fn child_path(&self, actor: ActorRef) -> AgentPath {
        AgentPath(format!(
            "{}/a{}_i{}",
            self.0.actor.0, actor.id.0, actor.incarnation.0
        ))
    }
}

/// The provider tree may skip a Haskell creator, while the host graph retains
/// that exact creator. A retained identity supplies provenance, not authority
/// to operate the ancestor's live actor.
pub(super) fn selected_provider_parent(
    run: &str,
    creator: Option<ActorRef>,
    nodes: &[ActorGraphNode],
    conversations: &HashMap<ActorRef, HostIdentity>,
) -> Result<SelectedProviderParent, String> {
    let mut actor = creator.ok_or("selected child has no admitted creator")?;
    let mut seen = HashSet::new();
    loop {
        if !seen.insert(actor) {
            return Err("selected provider ancestry contains a cycle".into());
        }
        let mut matching = nodes.iter().filter(|node| node.actor == actor);
        let node = matching
            .next()
            .ok_or("selected provider ancestor is absent from the host graph")?;
        if matching.next().is_some() {
            return Err("selected provider ancestor is ambiguous".into());
        }
        if let Some(identity) = conversations.get(&actor) {
            if !node.model_actor
                || identity.run != run
                || identity.incarnation != actor.incarnation.0.to_string()
            {
                return Err(
                    "selected provider ancestor identity does not match its exact host incarnation"
                        .into(),
                );
            }
            return Ok(SelectedProviderParent(identity.clone()));
        }
        actor = node
            .creator
            .or(node.supervisor_parent)
            .ok_or("selected child has no registered provider ancestor")?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use exomonad_actor::{ActorId, ActorWorkbenchPosture};

    fn actor(id: u64) -> ActorRef {
        ActorRef::first(ActorId(id))
    }

    fn node(id: u64, creator: Option<ActorRef>, model_actor: bool) -> ActorGraphNode {
        ActorGraphNode {
            actor: actor(id),
            label: format!("actor-{id}"),
            model_actor,
            creator,
            supervisor_parent: creator,
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
    fn selected_child_keeps_haskell_creator_and_uses_nearest_provider_ancestor() {
        let nodes = vec![
            node(1, None, true),
            node(2, Some(actor(1)), false),
            node(3, Some(actor(2)), true),
        ];
        let identities = HashMap::from([(
            actor(1),
            HostIdentity {
                run: "run".into(),
                actor: AgentPath("/root".into()),
                incarnation: "1".into(),
            },
        )]);
        let parent =
            selected_provider_parent("run", nodes[2].creator, &nodes, &identities).unwrap();
        assert_eq!(parent.identity().actor.0, "/root");
        assert_eq!(parent.child_path(actor(3)).0, "/root/a3_i1");
        assert_eq!(nodes[2].creator, Some(actor(2)));
    }

    #[test]
    fn selected_child_refuses_stale_missing_and_cyclic_provider_ancestry() {
        let mut nodes = vec![node(1, None, true), node(2, Some(actor(1)), false)];
        let mut identities = HashMap::from([(
            actor(1),
            HostIdentity {
                run: "run".into(),
                actor: AgentPath("/root".into()),
                incarnation: "stale".into(),
            },
        )]);
        assert!(selected_provider_parent("run", Some(actor(2)), &nodes, &identities).is_err());
        identities.get_mut(&actor(1)).unwrap().incarnation = "1".into();
        assert!(
            selected_provider_parent("other-run", Some(actor(2)), &nodes, &identities).is_err()
        );
        assert!(selected_provider_parent("run", Some(actor(4)), &nodes, &identities).is_err());
        identities.clear();
        assert!(selected_provider_parent("run", Some(actor(2)), &nodes, &identities).is_err());
        nodes[0].creator = Some(actor(2));
        assert!(selected_provider_parent("run", Some(actor(2)), &nodes, &identities).is_err());
    }
}
