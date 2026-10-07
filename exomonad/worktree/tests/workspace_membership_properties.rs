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

#[derive(Clone, Copy, Debug)]
enum Operation {
    Attach {
        actor: usize,
        tree: usize,
        access: WorkspaceAccess,
    },
    Release {
        actor: usize,
    },
    Complete {
        actor: usize,
    },
    Reopen,
    Recover {
        actor: usize,
    },
    Transfer {
        actor: usize,
        successor: usize,
    },
}

impl Operation {
    fn support_index(self) -> usize {
        match self {
            Self::Attach { .. } => 0,
            Self::Release { .. } => 1,
            Self::Complete { .. } => 2,
            Self::Reopen => 3,
            Self::Recover { .. } => 4,
            Self::Transfer { .. } => 5,
        }
    }
}

fn operation() -> impl Strategy<Value = Operation> {
    prop_oneof![
        (0usize..4, 0usize..3, any::<bool>()).prop_map(|(actor, tree, writable)| {
            Operation::Attach {
                actor,
                tree,
                access: if writable {
                    WorkspaceAccess::ReadWrite
                } else {
                    WorkspaceAccess::ReadOnly
                },
            }
        }),
        (0usize..4).prop_map(|actor| Operation::Release { actor }),
        (0usize..4).prop_map(|actor| Operation::Complete { actor }),
        Just(Operation::Reopen),
        (0usize..4).prop_map(|actor| Operation::Recover { actor }),
        (0usize..4, 0usize..4)
            .prop_map(|(actor, successor)| Operation::Transfer { actor, successor }),
    ]
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

fn replay(history: &[Operation]) {
    let storage = tempfile::tempdir().expect("temporary binding storage");
    let anchor = DirectoryAnchor::open_existing(storage.path()).expect("storage anchor");
    let mut table = BindingTable::open(&anchor, "bindings").expect("open binding table");
    let mut active = BTreeMap::<usize, Active>::new();
    let mut leases = BTreeMap::new();
    let mut support = [0usize; 7]; // six operations and maximum peers on one tree

    // Every generated history begins with a witness for shared membership,
    // the per-actor workspace limit, settlement isolation, and restart custody.
    let mut ops = vec![
        Operation::Attach {
            actor: 0,
            tree: 0,
            access: WorkspaceAccess::ReadWrite,
        },
        Operation::Attach {
            actor: 1,
            tree: 0,
            access: WorkspaceAccess::ReadOnly,
        },
        Operation::Attach {
            actor: 0,
            tree: 1,
            access: WorkspaceAccess::ReadWrite,
        },
        Operation::Transfer {
            actor: 1,
            successor: 2,
        },
        Operation::Reopen,
        Operation::Recover { actor: 2 },
        Operation::Recover { actor: 0 },
        Operation::Release { actor: 2 },
        Operation::Complete { actor: 0 },
    ];
    ops.extend_from_slice(history);

    for operation in ops {
        support[operation.support_index()] += 1;
        match operation {
            Operation::Attach {
                actor: actor_index,
                tree: tree_index,
                access,
            } => {
                let actor = agent(actor_index);
                let tree = worktree(tree_index);
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
            Operation::Release { actor: actor_index }
            | Operation::Complete { actor: actor_index } => {
                let actor = agent(actor_index);
                if active
                    .get(&actor_index)
                    .is_some_and(|membership| membership.authorized)
                {
                    let membership = active.remove(&actor_index).expect("active actor");
                    let lease = leases.remove(&actor_index).expect("issued lease");
                    if matches!(operation, Operation::Release { .. }) {
                        lease.release(&mut table).expect("release membership");
                    } else {
                        lease.complete(&mut table).expect("complete membership");
                    }
                    assert!(table
                        .membership(&worktree(membership.worktree), &actor)
                        .is_none());
                }
            }
            Operation::Reopen => {
                leases.clear();
                for membership in active.values_mut() {
                    membership.authorized = false;
                }
                drop(table);
                table = BindingTable::open(&anchor, "bindings").expect("reopen binding table");
            }
            Operation::Recover { actor: actor_index } => {
                let actor = agent(actor_index);
                match active.get_mut(&actor_index) {
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
                    None => assert!(table
                        .recover_active(&worktree(0), &actor, &actor, 100)
                        .is_err()),
                }
            }
            Operation::Transfer {
                actor: actor_index,
                successor: successor_index,
            } => {
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
        history in proptest::collection::vec(operation(), 1..80)
    ) {
        replay(&history);
    }
}
