//! Explicit fallback costs include real observation mutations and complete
//! exact inventory requests. Stable bookkeeping slots are not native values.

use super::{cost_tests, persistent_name_cost_tests::Scenario, *};
use std::hint::black_box;

fn emit(
    steps: usize,
    unrelated: usize,
    mode: &str,
    operation: &str,
    measured: (u128, cost_tests::AllocationCounts),
) {
    eprintln!(
        "binding_membership_cost {}",
        serde_json::json!({
            "schema": 1,
            "steps": steps,
            "inherited_baseline": steps,
            "unrelated_baseline": unrelated,
            "native_source_instances": 0,
            "mode": mode,
            "operation": operation,
            "elapsed_ns": measured.0,
            "current_thread_allocations": measured.1,
        }),
    );
}

#[test]
fn observation_mutation_capture_fallback_cost_matrix_preserves_frozen_readers() {
    for steps in [1, 10, 100] {
        for unrelated in [0, 100] {
            for mode in ["append_observation", "cross_owner_cycle_resave"] {
                let mut scenario = Scenario::new();
                let b = scenario.tree.mint_isolated();
                let c = scenario.tree.mint_isolated();
                let mut cycle = None;
                emit(
                    steps,
                    unrelated,
                    mode,
                    "construct_baseline_and_observations",
                    cost_tests::measure(|| {
                        for index in 0..steps {
                            scenario.bind(scenario.origin, format!("base_{index:03}"));
                        }
                        for index in 0..unrelated {
                            scenario.bind(scenario.unrelated, format!("other_{index:03}"));
                        }
                        scenario.table.seed_detached_scope(
                            &scenario.tree,
                            scenario.origin,
                            scenario.owner,
                        );
                        if mode == "cross_owner_cycle_resave" {
                            let aid = scenario.bind(scenario.owner, "probe".into());
                            let bid = scenario.bind(b, "b".into());
                            let cid = scenario.bind(c, "c".into());
                            assert!(scenario.table.save_observation(bid, &[], 1).is_empty());
                            assert!(scenario
                                .table
                                .save_observation(aid, &[bid.var()], 1)
                                .is_empty());
                            assert!(scenario
                                .table
                                .save_observation(cid, &[aid.var()], 1)
                                .is_empty());
                            cycle = Some((bid, cid));
                        }
                    }),
                );
                let mut first = None;
                let mut latest = None;
                let mut first_id = None;
                let mut last_id = None;
                let mut previous_observation = None;
                emit(
                    steps,
                    unrelated,
                    mode,
                    "mutate_capture_and_superseded_drain",
                    cost_tests::measure(|| {
                        for index in 0..steps {
                            let id = scenario.bind(scenario.owner, format!("step_{index:03}"));
                            first_id.get_or_insert(id);
                            last_id = Some(id);
                            if let Some((bid, cid)) = cycle {
                                let deps = if index % 2 == 0 {
                                    vec![cid.var()]
                                } else {
                                    vec![]
                                };
                                assert!(scenario.table.save_observation(bid, &deps, 1).is_empty());
                            } else {
                                let deps = previous_observation
                                    .into_iter()
                                    .map(SessionVarId::var)
                                    .collect::<Vec<_>>();
                                assert!(scenario.table.save_observation(id, &deps, 1).is_empty());
                                previous_observation = Some(id);
                            }
                            let snapshot = scenario.tree.mint_isolated();
                            scenario.table.seed_detached_scope(
                                &scenario.tree,
                                scenario.owner,
                                snapshot,
                            );
                            first.get_or_insert(snapshot);
                            if let Some(old) = latest.replace(snapshot) {
                                if Some(old) != first {
                                    assert!(scenario.drain(old).bindings.is_empty());
                                }
                            }
                        }
                    }),
                );
                let first = first.unwrap();
                let latest = latest.unwrap();
                let first_ids = scenario
                    .table
                    .scope_reachable_binding_ids(&scenario.tree, first);
                assert!(first_ids.contains(&first_id.unwrap()));
                if steps > 1 {
                    assert!(!first_ids.contains(&last_id.unwrap()));
                }
                let latest_ids = scenario
                    .table
                    .scope_reachable_binding_ids(&scenario.tree, latest);
                assert!(latest_ids.contains(&last_id.unwrap()));
                if let Some((_, cid)) = cycle {
                    assert!(first_ids.contains(&cid));
                    if steps % 2 == 0 {
                        assert!(
                            !latest_ids.contains(&cid),
                            "new membership retracts foreign cyclic dependency"
                        );
                    }
                }
                emit(
                    steps,
                    unrelated,
                    mode,
                    "latest_exact_inventory_128_requests",
                    cost_tests::measure(|| {
                        for _ in 0..128 {
                            black_box(
                                scenario
                                    .table
                                    .scope_reachable_binding_ids(&scenario.tree, latest),
                            );
                        }
                    }),
                );
                emit(
                    steps,
                    unrelated,
                    mode,
                    "drain_all_owners_and_readers",
                    cost_tests::measure(|| {
                        for scope in [scenario.origin, scenario.owner, scenario.unrelated, b, c] {
                            scenario.drain(scope);
                        }
                        if first != latest {
                            scenario.drain(latest);
                        }
                        scenario.drain(first);
                    }),
                );
                assert!(scenario.table.is_empty());
            }
        }
    }
}
