//! Measures persistent-root growth cost: whether the JIT's tenure-on-bind
//! path leaks roots (and the old-space bytes they retain) unboundedly as a
//! session accumulates binds.
//!
//! Drives ONE session machine through N successive value-plane bind
//! fragments (`add_function` + `run_pure_and_bind`) and reports, at N in
//! {1, 8, 64}:
//! `persistent_roots_count()`, `heap_stats()` (nursery high-water bytes +
//! GC count), and `old_space_bytes_used()` (the retained-bytes number
//! `heap_stats().live_bytes` does NOT capture, since it resets to 0 across a
//! collection while old-space only ever grows).
//!
//! Run under GC poison + heap verify (this touches the heap across real
//! minor collections — the nursery is sized small enough to force several).

use tidepool_codegen::emit::ExternalEnv;
use tidepool_codegen::jit_machine::JitEffectMachine;
use tidepool_repr::datacon::DataCon;
use tidepool_repr::DataConTable;

use serial_test::serial;

use crate::session_scaffold;
use crate::session_scaffold_value;
use session_scaffold::C1;
use session_scaffold_value::build_value_fragment;

fn table_with_c1() -> DataConTable {
    let mut table = DataConTable::new();
    table.insert(DataCon {
        id: C1,
        name: "C1".to_string(),
        tag: 1,
        rep_arity: 1,
        field_bangs: vec![],
        qualified_name: None,
        type_name: String::new(),
    });
    table
}

/// N in {1, 8, 64} value-plane binds against one session machine. Reports
/// the root/heap numbers at each checkpoint and asserts two claims:
/// (1) `persistent_roots_count` grows STRICTLY
/// monotonically (one root per bind, no dedup, no shrink short of drop),
/// and (2) `old_space_bytes_used` grows with it (the retained bytes a
/// growing root set keeps alive).
#[test]
#[serial]
fn realm_root_growth_persistent_roots_and_heap() {
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            tidepool_codegen::host_fns::set_gc_poison(true);
            tidepool_codegen::host_fns::set_heap_verify(true);

            let table = table_with_c1();
            // Small nursery (2 KiB): each bind's Con(C1,[Lit]) is ~56 bytes
            // pre-tenure, so 64 rounds forces several real minor collections
            // rather than fitting in one nursery's worth of bump allocation.
            let dummy = build_value_fragment(0);
            let mut machine = JitEffectMachine::compile_session(&dummy, &table, 2048)
                .expect("compile_session");

            let checkpoints = [1usize, 8, 64];
            let mut round = 0usize;
            let mut samples: Vec<(usize, usize, usize, u64, usize)> = Vec::new();
            for target in checkpoints {
                while round < target {
                    round += 1;
                    let frag = machine
                        .add_function(
                            &format!("bind_{round}"),
                            &build_value_fragment(round as i64),
                            &table,
                            &ExternalEnv::new(),
                        )
                        .unwrap_or_else(|e| panic!("add_function bind_{round}: {e}"));
                    machine
                        .run_pure_and_bind(frag)
                        .unwrap_or_else(|e| panic!("run_pure_and_bind bind_{round}: {e}"));
                }
                let roots = machine.persistent_roots_count();
                let stats = machine.heap_stats();
                let old_space = machine.old_space_bytes_used();
                println!(
                    "[realm_root_growth] N={round} persistent_roots_count={roots} \
                     nursery_bytes={} live_bytes={} gc_count={} old_space_bytes_used={old_space}",
                    stats.nursery_bytes, stats.live_bytes, stats.gc_count
                );
                samples.push((round, roots, stats.nursery_bytes, stats.gc_count, old_space));
            }

            // Claim 1: persistent_roots_count == N exactly (one root per bind).
            for &(n, roots, ..) in &samples {
                assert_eq!(
                    roots, n,
                    "persistent_roots_count must equal bind count N={n} exactly \
                     (no per-slot deregister exists — machine_state.rs registers \
                     one slot per tenure and clears only at machine drop)"
                );
            }

            // Claim 2: strictly monotonic growth, checkpoint over checkpoint.
            for w in samples.windows(2) {
                assert!(
                    w[1].1 > w[0].1,
                    "persistent_roots_count must grow strictly: {:?} -> {:?}",
                    w[0],
                    w[1]
                );
            }

            // Claim 3: retained old-space bytes grow with the root count —
            // this is the number `heap_stats().live_bytes` cannot show, since
            // a minor GC resets the nursery high-water mark to whatever
            // survives (frequently ~0 here) while old-space is append-only.
            for w in samples.windows(2) {
                assert!(
                    w[1].4 > w[0].4,
                    "old_space_bytes_used must grow with the root count: {:?} -> {:?}",
                    w[0],
                    w[1]
                );
            }

            // At least one real minor collection fired across 64 small binds
            // through a 2 KiB nursery — otherwise this run says nothing about
            // GC-poison/heap-verify safety under root growth.
            assert!(
                samples.last().unwrap().3 > 0,
                "expected at least one real GC across 64 rounds through a 2 KiB nursery, got gc_count={}",
                samples.last().unwrap().3
            );

            drop(machine);

            tidepool_codegen::host_fns::set_gc_poison(false);
            tidepool_codegen::host_fns::set_heap_verify(false);
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
#[serial]
fn retirement_major_collection_rewrites_survivor_and_reclaims_old_space() {
    use tidepool_heap::layout::{CON_FIELDS_OFFSET, LIT_VALUE_OFFSET};

    let table = table_with_c1();
    let dummy = build_value_fragment(0);
    let mut machine =
        JitEffectMachine::compile_session(&dummy, &table, 2048).expect("compile_session");
    let mut roots = Vec::new();
    for round in 1..=3 {
        let frag = machine
            .add_function(
                &format!("retire_{round}"),
                &build_value_fragment(round),
                &table,
                &ExternalEnv::new(),
            )
            .expect("add_function");
        roots.push(machine.run_pure_and_bind(frag).expect("bind"));
    }
    let before = machine.old_space_bytes_used();
    assert!(before > 0);

    machine.retire_scope_root(roots[0]);
    machine.retire_scope_root(roots[1]);
    let survivor_bytes = machine.old_space_bytes_used();
    assert!(survivor_bytes > 0 && survivor_bytes < before);
    assert_eq!(machine.persistent_roots_count(), 1);

    // The surviving RootSlot address is stable while its value is rewritten
    // to the compacted arena. Read the fixture's C1 (LitInt 3) shape directly.
    unsafe {
        let con = roots[2].current();
        let lit = *(con.add(CON_FIELDS_OFFSET) as *const *mut u8);
        assert_eq!(*(lit.add(LIT_VALUE_OFFSET) as *const i64), 3);
    }

    machine.retire_scope_root(roots[2]);
    assert_eq!(machine.persistent_roots_count(), 0);
    assert_eq!(machine.old_space_bytes_used(), 0);
}
