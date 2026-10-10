use exomonad_worktree::{
    AgentRef, BindingState, BindingTable, WorkspaceAccess, WorktreeError, WorktreeId,
};
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
    StaleSettle {
        receipt: usize,
        completed: bool,
    },
    StaleTransfer {
        receipt: usize,
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
            Self::StaleSettle { .. } => 6,
            Self::StaleTransfer { .. } => 7,
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
        (0usize..8, any::<bool>())
            .prop_map(|(receipt, completed)| Operation::StaleSettle { receipt, completed }),
        (0usize..8, 0usize..4)
            .prop_map(|(receipt, successor)| Operation::StaleTransfer { receipt, successor }),
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

fn durable_bytes(root: &std::path::Path) -> Vec<Option<Vec<u8>>> {
    (0..3)
        .map(
            |tree| match std::fs::read(root.join(format!("bindings/wt-{tree}.json"))) {
                Ok(bytes) => Some(bytes),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => panic!("observe durable binding rows: {error}"),
            },
        )
        .collect()
}

#[test]
fn stale_receipts_cannot_mutate_reopened_memberships() {
    #[derive(Clone, Copy, Debug)]
    enum Custody {
        FreshPeer,
        RecoverPredecessor,
        ReclaimSuccessor,
    }
    #[derive(Clone, Copy, Debug)]
    enum Action {
        Release,
        Complete,
        Transfer,
    }
    for recovery in [
        Custody::FreshPeer,
        Custody::RecoverPredecessor,
        Custody::ReclaimSuccessor,
    ] {
        for transferred in [false, true] {
            if matches!(recovery, Custody::ReclaimSuccessor) && !transferred {
                continue;
            }
            for action in [Action::Release, Action::Complete, Action::Transfer] {
                let storage = tempfile::tempdir().unwrap();
                let anchor = DirectoryAnchor::open_existing(storage.path()).unwrap();
                let tree = worktree(0);
                let mut table = BindingTable::open(&anchor, "bindings").unwrap();
                let mut stale = table
                    .bind(&tree, &agent(0), WorkspaceAccess::ReadOnly, 1)
                    .unwrap();
                if transferred {
                    table.transfer(&mut stale, &agent(1), 2).unwrap();
                }
                let predecessor = agent(if matches!(recovery, Custody::ReclaimSuccessor) {
                    0
                } else {
                    usize::from(transferred)
                });
                let successor = agent(if matches!(recovery, Custody::ReclaimSuccessor) {
                    1
                } else {
                    2
                });
                drop(table);
                let mut table = BindingTable::open(&anchor, "bindings").unwrap();
                // Align the new owner's generation with a transferred old lease.
                let peer = transferred.then(|| {
                    table
                        .bind(&worktree(1), &agent(4), WorkspaceAccess::ReadOnly, 3)
                        .unwrap()
                });
                let current = match recovery {
                    Custody::FreshPeer => {
                        table.bind(&tree, &successor, WorkspaceAccess::ReadWrite, 3)
                    }
                    Custody::RecoverPredecessor | Custody::ReclaimSuccessor => {
                        table.recover_active(&tree, &predecessor, &successor, 3)
                    }
                }
                .unwrap();
                let path = storage.path().join("bindings/wt-0.json");
                let bytes = std::fs::read(&path).unwrap();
                let rows: Vec<_> = table.participants(&tree).unwrap().cloned().collect();
                let result = match action {
                    Action::Release => stale.release(&mut table),
                    Action::Complete => stale.complete(&mut table),
                    Action::Transfer => table.transfer(&mut stale, &agent(3), 4),
                };
                assert!(
                    matches!(result, Err(WorktreeError::StorageFailure { .. })),
                    "foreign receipt must refuse: recovery={recovery:?}, transferred={transferred}, action={action:?}"
                );
                assert_eq!(std::fs::read(&path).unwrap(), bytes);
                assert_eq!(
                    table
                        .participants(&tree)
                        .unwrap()
                        .cloned()
                        .collect::<Vec<_>>(),
                    rows
                );
                assert!(table.membership(&tree, &successor).is_some());
                current.release(&mut table).unwrap();
                assert!(table.membership(&tree, &successor).is_none());
                if let Some(peer) = peer {
                    peer.complete(&mut table).unwrap();
                }
            }
        }
    }
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
    let mut stale = Vec::new();
    let mut support = [0usize; 9]; // eight operations and maximum peers on one tree

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
        Operation::StaleTransfer {
            receipt: 0,
            successor: 3,
        },
        Operation::StaleSettle {
            receipt: 0,
            completed: false,
        },
        Operation::Recover { actor: 0 },
        Operation::StaleSettle {
            receipt: 0,
            completed: true,
        },
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
                    let before = durable_bytes(storage.path());
                    assert!(table.bind(&tree, &actor, access, 100).is_err());
                    assert_eq!(durable_bytes(storage.path()), before);
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
                stale.extend(std::mem::take(&mut leases).into_values());
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
                        let before = durable_bytes(storage.path());
                        assert!(table
                            .recover_active(
                                &worktree(membership.worktree),
                                &wrong_predecessor,
                                &actor,
                                99,
                            )
                            .is_err());
                        assert_eq!(durable_bytes(storage.path()), before);
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
                        let before = durable_bytes(storage.path());
                        assert!(table
                            .recover_active(&worktree(membership.worktree), &actor, &actor, 100)
                            .is_err());
                        assert_eq!(durable_bytes(storage.path()), before);
                    }
                    None => {
                        let before = durable_bytes(storage.path());
                        assert!(table
                            .recover_active(&worktree(0), &actor, &actor, 100)
                            .is_err());
                        assert_eq!(durable_bytes(storage.path()), before);
                    }
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
                    let before = durable_bytes(storage.path());
                    let lease = leases.get_mut(&actor_index).expect("authorized lease");
                    assert!(table.transfer(lease, &agent(successor_index), 100).is_err());
                    assert_eq!(durable_bytes(storage.path()), before);
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
            Operation::StaleSettle { receipt, completed } => {
                if !stale.is_empty() {
                    let before = durable_bytes(storage.path());
                    let lease = stale.remove(receipt % stale.len());
                    let result = if completed {
                        lease.complete(&mut table)
                    } else {
                        lease.release(&mut table)
                    };
                    assert!(matches!(result, Err(WorktreeError::StorageFailure { .. })));
                    assert_eq!(durable_bytes(storage.path()), before);
                }
            }
            Operation::StaleTransfer { receipt, successor } => {
                if !stale.is_empty() {
                    let before = durable_bytes(storage.path());
                    let index = receipt % stale.len();
                    assert!(matches!(
                        table.transfer(&mut stale[index], &agent(successor), 100),
                        Err(WorktreeError::StorageFailure { .. })
                    ));
                    assert_eq!(durable_bytes(storage.path()), before);
                }
            }
        }
        support[8] = support[8].max(
            (0..3)
                .map(|tree| table.participants(&worktree(tree)).unwrap().count())
                .max()
                .unwrap_or(0),
        );
        check_model(&table, &active);
    }
    assert!(support[0] > 0 && support[1] > 0 && support[2] > 0);
    assert!(support[3] > 0 && support[4] > 0 && support[5] > 0);
    assert!(support[6] > 0 && support[7] > 0);
    assert!(
        support[8] >= 2,
        "the history must exercise shared membership"
    );
}

#[test]
fn transferred_successor_recovery_preserves_a_reattached_predecessor_peer() {
    let mut history = [
        Operation::Attach {
            actor: 0,
            tree: 0,
            access: WorkspaceAccess::ReadOnly,
        },
        Operation::Transfer {
            actor: 0,
            successor: 3,
        },
        Operation::Reopen,
        Operation::Attach {
            actor: 0,
            tree: 0,
            access: WorkspaceAccess::ReadOnly,
        },
        Operation::Attach {
            actor: 0,
            tree: 0,
            access: WorkspaceAccess::ReadOnly,
        },
        Operation::Recover { actor: 3 },
    ];
    // Replay the minimized failing history, then distinguish the independent
    // peer's access from the transferred successor's retained read-only grant.
    replay(&history);
    history[3] = Operation::Attach {
        actor: 0,
        tree: 0,
        access: WorkspaceAccess::ReadWrite,
    };
    replay(&history);
    let mut reopened_peer = history[..5].to_vec();
    reopened_peer.extend([
        Operation::Reopen,
        Operation::Recover { actor: 3 },
        Operation::Recover { actor: 0 },
    ]);
    replay(&reopened_peer);
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
