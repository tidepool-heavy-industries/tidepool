use exomonad_worktree::{AgentRef, BindingState, BindingTable, WorkspaceAccess, WorktreeId};
use proptest::prelude::*;
use proptest::test_runner::{Config, FileFailurePersistence};
use std::collections::BTreeMap;
use tidepool_atomic_write::DirectoryAnchor;

#[derive(Clone, Copy, Debug)]
struct Active {
    worktree: usize,
    access: WorkspaceAccess,
    authorized: bool,
    predecessor: Option<usize>,
}

fn config() -> Config {
    let mut config = Config::default();
    if std::env::var_os("PROPTEST_CASES").is_none() {
        config.cases = 96;
    }
    if std::env::var_os("PROPTEST_MAX_SHRINK_ITERS").is_none() {
        config.max_shrink_iters = 4096;
    }
    if let Some(path) = option_env!("TIDEPOOL_PROPTEST_REGRESSIONS") {
        config.failure_persistence = Some(Box::new(FileFailurePersistence::Direct(path)));
    }
    config
}

fn worktree(index: usize) -> WorktreeId {
    WorktreeId::from_raw(format!("wt-{index}"))
}

fn agent(index: usize) -> AgentRef {
    AgentRef::exact_actor("property-run", index as u64, 0)
}

fn check_model(table: &BindingTable, active: &BTreeMap<usize, Active>) {
    for tree in 0..3 {
        let expected: Vec<_> = active
            .iter()
            .filter(|(_, membership)| membership.worktree == tree)
            .map(|(actor, membership)| (*actor, membership.access))
            .collect();
        let mut actual: Vec<_> = table
            .participants(&worktree(tree))
            .expect("diagnostic participants")
            .map(|binding| {
                let actor = (0..4)
                    .find(|candidate| agent(*candidate) == *binding.agent())
                    .expect("participant belongs to the generated actor domain");
                assert_eq!(binding.state(), BindingState::Active);
                (actor, binding.access())
            })
            .collect();
        actual.sort_unstable_by_key(|(actor, _)| *actor);
        assert_eq!(actual, expected, "participants for worktree {tree}");
    }
    for actor_index in 0..4 {
        let actor = agent(actor_index);
        match active.get(&actor_index) {
            Some(membership) if membership.authorized => {
                let tree = worktree(membership.worktree);
                let row = table
                    .membership(&tree, &actor)
                    .expect("issued exact membership is authority");
                assert_eq!(row.access(), membership.access);
                assert_eq!(table.active_for_agent(&actor), Some(&tree));
            }
            Some(membership) => {
                assert!(table
                    .membership(&worktree(membership.worktree), &actor)
                    .is_none());
                assert!(table.active_for_agent(&actor).is_none());
            }
            None => {
                assert!(table.active_for_agent(&actor).is_none());
                for tree in 0..3 {
                    assert!(table.membership(&worktree(tree), &actor).is_none());
                }
            }
        }
        for stale in [
            AgentRef::exact_actor("another-run", actor_index as u64, 0),
            AgentRef::exact_actor("property-run", actor_index as u64, 1),
        ] {
            assert!(table.active_for_agent(&stale).is_none());
            for tree in 0..3 {
                assert!(table.membership(&worktree(tree), &stale).is_none());
            }
        }
    }
}

fn replay(history: &[(u8, u8, u8)]) {
    let storage = tempfile::tempdir().expect("temporary binding storage");
    let anchor = DirectoryAnchor::open_existing(storage.path()).expect("storage anchor");
    let mut table = BindingTable::open(&anchor, "bindings").expect("open binding table");
    let mut active = BTreeMap::<usize, Active>::new();
    let mut leases = BTreeMap::new();
    let mut support = [0usize; 7]; // six operations and maximum peers on one tree

    // Every generated history begins with a witness for shared membership,
    // the per-actor workspace limit, settlement isolation, and restart custody.
    let mut ops = vec![
        (0, 0, 3), // actor 0 attaches read-write to tree 0
        (0, 1, 0), // actor 1 joins the same tree read-only
        (0, 0, 1), // actor 0 cannot acquire a different active workspace
        (5, 1, 2), // transfer actor 1 to actor 2; actor 0 remains attached
        (3, 0, 0), // restart revokes process-local memberships
        (4, 2, 0), // reclaim transfer only with exact predecessor provenance
        (4, 0, 0), // independently recover the surviving peer
        (1, 2, 0), // release transferred membership
        (2, 0, 0), // complete surviving peer
    ];
    ops.extend_from_slice(history);

    for (op, actor_index, raw_tree_access) in ops {
        support[(op % 6) as usize] += 1;
        let actor_index = actor_index as usize % 4;
        let tree_index = raw_tree_access as usize % 3;
        let access = if raw_tree_access % 2 == 0 {
            WorkspaceAccess::ReadOnly
        } else {
            WorkspaceAccess::ReadWrite
        };
        let tree = worktree(tree_index);
        let actor = agent(actor_index);
        match op % 6 {
            0 => {
                if active.contains_key(&actor_index) {
                    assert!(table.bind(&tree, &actor, access, 100).is_err());
                } else {
                    let lease = table.bind(&tree, &actor, access, 100).expect("attach");
                    active.insert(
                        actor_index,
                        Active {
                            worktree: tree_index,
                            access,
                            authorized: true,
                            predecessor: None,
                        },
                    );
                    leases.insert(actor_index, lease);
                }
            }
            1 | 2 => {
                if active
                    .get(&actor_index)
                    .is_some_and(|membership| membership.authorized)
                {
                    let membership = active.remove(&actor_index).expect("active actor");
                    let lease = leases.remove(&actor_index).expect("issued lease");
                    if op % 6 == 1 {
                        lease.release(&mut table).expect("release membership");
                    } else {
                        lease.complete(&mut table).expect("complete membership");
                    }
                    assert!(table
                        .membership(&worktree(membership.worktree), &actor)
                        .is_none());
                }
            }
            3 => {
                leases.clear();
                for membership in active.values_mut() {
                    membership.authorized = false;
                }
                drop(table);
                table = BindingTable::open(&anchor, "bindings").expect("reopen binding table");
            }
            4 => match active.get_mut(&actor_index) {
                Some(membership) if !membership.authorized => {
                    let predecessor_index = membership.predecessor.unwrap_or(actor_index);
                    let wrong_index = (0..4)
                        .find(|candidate| {
                            *candidate != predecessor_index && *candidate != actor_index
                        })
                        .expect("actor domain has a distinct incorrect predecessor");
                    let wrong_predecessor = agent(wrong_index);
                    assert!(table
                        .recover_active(
                            &worktree(membership.worktree),
                            &wrong_predecessor,
                            &actor,
                            99,
                        )
                        .is_err());
                    let lease = table
                        .recover_active(
                            &worktree(membership.worktree),
                            &agent(predecessor_index),
                            &actor,
                            100,
                        )
                        .expect("recover retained exact actor");
                    membership.authorized = true;
                    leases.insert(actor_index, lease);
                }
                Some(membership) => {
                    assert!(table
                        .recover_active(&worktree(membership.worktree), &actor, &actor, 100)
                        .is_err());
                }
                None => assert!(table.recover_active(&tree, &actor, &actor, 100).is_err()),
            },
            5 => {
                let successor_index = raw_tree_access as usize % 4;
                let Some(membership) = active.get(&actor_index).copied() else {
                    check_model(&table, &active);
                    continue;
                };
                if !membership.authorized {
                    check_model(&table, &active);
                    continue;
                }
                if active.contains_key(&successor_index) {
                    let lease = leases.get_mut(&actor_index).expect("authorized lease");
                    assert!(table.transfer(lease, &agent(successor_index), 100).is_err());
                } else {
                    let mut lease = leases.remove(&actor_index).expect("authorized lease");
                    table
                        .transfer(&mut lease, &agent(successor_index), 100)
                        .expect("transfer membership to available exact actor");
                    active.remove(&actor_index);
                    active.insert(
                        successor_index,
                        Active {
                            worktree: membership.worktree,
                            access: membership.access,
                            authorized: true,
                            predecessor: Some(actor_index),
                        },
                    );
                    leases.insert(successor_index, lease);
                }
            }
        }
        support[6] = support[6].max(
            (0..3)
                .map(|tree| table.participants(&worktree(tree)).unwrap().count())
                .max()
                .unwrap_or(0),
        );
        check_model(&table, &active);
    }
    assert!(support[0] > 0 && support[1] > 0 && support[2] > 0);
    assert!(support[3] > 0 && support[4] > 0 && support[5] > 0);
    assert!(
        support[6] >= 2,
        "the history must exercise shared membership"
    );
}

proptest! {
    #![proptest_config(config())]

    #[test]
    fn generated_membership_history_matches_independent_model(
        history in proptest::collection::vec((0u8..6, 0u8..4, 0u8..6), 1..80)
    ) {
        replay(&history);
    }
}
