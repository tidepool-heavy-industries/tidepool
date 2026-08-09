//! REALM CYCLE-SCOPING — does reclamation-by-drop actually reclaim?
//!
//! The realm design's answer to unbounded growth is CYCLE-SCOPED machines: one
//! machine per loop cycle, dropped at the loop boundary after State serializes,
//! with no retirement machinery. That only works if dropping a machine that
//! held K parked continuations and M `add_function` fragments actually gives the
//! memory back.
//!
//! Three things could grow. Two are reasoned about from `JitEffectMachine::drop`:
//! the root registries (`MachineState` is owned by the machine, and drop retires
//! every old-space arena then calls `free_session_heap`) and the session heap
//! `Vec`. The third — JITModule executable memory — is the open question, and it
//! is the one this file measures.
//!
//! MEASUREMENT, NOT ASSUMPTION. `report_cycle_scoped_drop_footprint` creates and
//! drops 32 machines and reads `/proc/self/statm` at baseline, peak, and after.
//! It reports BOTH resident (RSS) and total address space (VSZ), because the two
//! answer differently — see the assertions below. Run it with `--no-capture` to
//! see the table:
//!
//! ```text
//! cargo nextest run -p tidepool-codegen \
//!   -E 'binary(realm_cycle_scoped_drop)' --no-capture
//! ```
//!
//! A sibling lane ('lifetime') is measuring the JITModule question
//! independently. If its numbers disagree with these, that disagreement is a
//! finding, not something to reconcile away.

use tidepool_codegen::emit::ExternalEnv;
use tidepool_codegen::jit_machine::{
    JitEffectMachine, ParkKind, ParkedOutcome, RealmId, ResumeInput,
};
use tidepool_effect::dispatch::{DispatchEffect, EffectContext};
use tidepool_effect::error::EffectError;
use tidepool_effect::Response;
use tidepool_eval::value::Value;
use tidepool_repr::datacon::DataCon;
use tidepool_repr::datacon_table::DataConTable;
use tidepool_repr::frame::CoreFrame;
use tidepool_repr::types::*;
use tidepool_repr::{CoreExpr, Literal, TreeBuilder};

use serial_test::serial;

// The shared scaffold carries helpers this file does not need; `#[path]`
// inclusion makes them look dead here.
#[allow(dead_code)]
#[path = "support/session_scaffold.rs"]
mod session_scaffold;
use session_scaffold::{build_value_fragment, C1};

const VAL_ID: DataConId = DataConId(10);
const E_ID: DataConId = DataConId(11);
const UNION_ID: DataConId = DataConId(12);
const LEAF_ID: DataConId = DataConId(13);
const NODE_ID: DataConId = DataConId(14);
const PAIR_ID: DataConId = DataConId(2);
const ASK_TAG: u64 = 0;

/// How many machines the create-and-drop loop runs. The spec's number.
const CYCLES: usize = 32;
/// Parked continuations per machine.
const PARKS_PER_MACHINE: usize = 4;
/// Extra `add_function` fragments per machine (JIT code that must be reclaimed
/// with the module, on top of the entry and the parks' own fragments).
const FRAGMENTS_PER_MACHINE: usize = 8;
/// The virtual reservation each `CodegenPipeline` makes for its JIT arena
/// (`pipeline.rs`: `ArenaMemoryProvider::new_with_size(256 * 1024 * 1024)`).
const JIT_ARENA_BYTES: usize = 256 * 1024 * 1024;

fn table() -> DataConTable {
    let mut t = DataConTable::new();
    t.insert(DataCon {
        id: C1,
        name: "C1".to_string(),
        tag: 1,
        rep_arity: 1,
        field_bangs: vec![],
        qualified_name: None,
        type_name: String::new(),
    });
    t.insert(DataCon {
        id: PAIR_ID,
        name: "Pair".to_string(),
        tag: 2,
        rep_arity: 2,
        field_bangs: vec![],
        qualified_name: None,
        type_name: String::new(),
    });
    for (id, name, qual, arity) in [
        (VAL_ID, "Val", "Control.Monad.Freer.Val", 1u32),
        (E_ID, "E", "Control.Monad.Freer.E", 2),
        (UNION_ID, "Union", "Data.OpenUnion.Union", 2),
        (LEAF_ID, "Leaf", "Data.FTCQueue.Leaf", 1),
        (NODE_ID, "Node", "Data.FTCQueue.Node", 2),
    ] {
        t.insert(DataCon {
            id,
            name: name.to_string(),
            tag: 0,
            rep_arity: arity,
            field_bangs: vec![],
            qualified_name: Some(qual.to_string()),
            type_name: String::new(),
        });
    }
    t
}

/// A suspending entry closing over `captured` (same shape as the falsifier's).
fn build_suspending(captured_n: i64, req: i64) -> CoreExpr {
    let mut b = TreeBuilder::new();
    let cap_lit = b.push(CoreFrame::Lit(Literal::LitInt(captured_n)));
    let captured = b.push(CoreFrame::Con {
        tag: C1,
        fields: vec![cap_lit],
    });
    let var_v = b.push(CoreFrame::Var(VarId(0)));
    let c1_v = b.push(CoreFrame::Con {
        tag: C1,
        fields: vec![var_v],
    });
    let var_captured = b.push(CoreFrame::Var(VarId(1)));
    let pair = b.push(CoreFrame::Con {
        tag: PAIR_ID,
        fields: vec![var_captured, c1_v],
    });
    let val = b.push(CoreFrame::Con {
        tag: VAL_ID,
        fields: vec![pair],
    });
    let lam = b.push(CoreFrame::Lam {
        binder: VarId(0),
        body: val,
    });
    let leaf = b.push(CoreFrame::Con {
        tag: LEAF_ID,
        fields: vec![lam],
    });
    let tag_word = b.push(CoreFrame::Lit(Literal::LitWord(ASK_TAG)));
    let request = b.push(CoreFrame::Lit(Literal::LitInt(req)));
    let union = b.push(CoreFrame::Con {
        tag: UNION_ID,
        fields: vec![tag_word, request],
    });
    let e = b.push(CoreFrame::Con {
        tag: E_ID,
        fields: vec![union, leaf],
    });
    b.push(CoreFrame::LetNonRec {
        binder: VarId(1),
        rhs: captured,
        body: e,
    });
    b.build()
}

struct NoDispatch;
impl DispatchEffect<()> for NoDispatch {
    fn dispatch(
        &mut self,
        tag: u64,
        _request: &Value,
        _cx: &EffectContext<'_, ()>,
    ) -> Result<Response, EffectError> {
        panic!("handler dispatched tag {tag} — the ask should have suspended");
    }
}

// ─── /proc/self/statm ──────────────────────────────────────────────────────

/// Total address space and resident set, in BYTES, from `/proc/self/statm`
/// (field 0 = program size, field 1 = resident, both in pages).
fn mem_bytes() -> (usize, usize) {
    let page = 4096usize;
    let s = std::fs::read_to_string("/proc/self/statm").expect("/proc/self/statm");
    let mut it = s.split_whitespace();
    let vsz: usize = it.next().unwrap().parse().unwrap();
    let rss: usize = it.next().unwrap().parse().unwrap();
    (vsz * page, rss * page)
}

/// Number of VMA entries in `/proc/self/maps`. The kernel caps this at
/// `vm.max_map_count` (65530 by default), so it — not the 128 TiB address space
/// — is the practical ceiling on how many leaked JIT arenas a process tolerates.
fn map_count() -> usize {
    std::fs::read_to_string("/proc/self/maps")
        .expect("/proc/self/maps")
        .lines()
        .count()
}

/// The kernel's default `vm.max_map_count`, for scale in the report. Read the
/// live value where available rather than assuming the default.
fn max_map_count() -> usize {
    std::fs::read_to_string("/proc/sys/vm/max_map_count")
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(65530)
}

fn mib(bytes: usize) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}

/// Signed MiB delta — growth can be negative when the allocator returns pages.
fn mib_delta(from: usize, to: usize) -> f64 {
    (to as f64 - from as f64) / (1024.0 * 1024.0)
}

// ─── the cycle under test ──────────────────────────────────────────────────

/// Build one cycle's machine: a session machine with `PARKS_PER_MACHINE`
/// continuations parked in the registry and `FRAGMENTS_PER_MACHINE` extra JIT
/// functions defined, then DROP it. This is the cycle-scoped shape: everything
/// the cycle allocated dies with the machine, with no retirement machinery.
///
/// `resume_one` resumes a single park before the drop, so the cycle exercises
/// both the park and resume paths rather than only the park path.
///
/// Returns `(vsz, rss)` sampled at the cycle's HIGH-WATER point — with the
/// machine fully built and still alive, immediately before the drop. Sampling
/// only after each drop would report a peak that never includes a live machine.
fn one_cycle(table: &DataConTable, resume_one: bool) -> (usize, usize) {
    let mut machine = JitEffectMachine::compile_session(&build_suspending(1, 1), table, 4096)
        .expect("compile_session");

    // Park the entry.
    let first = match machine
        .run_suspendable_parked(table, &mut NoDispatch, &(), ASK_TAG, RealmId(0))
        .expect("park entry")
    {
        ParkedOutcome::Suspended { id, .. } => id,
        ParkedOutcome::Completed { .. } => panic!("entry should suspend"),
    };

    // Park the rest as suspending fragments.
    for i in 1..PARKS_PER_MACHINE {
        let f = machine
            .add_function(
                &format!("park_{i}"),
                &build_suspending(100 + i as i64, 200 + i as i64),
                table,
                &ExternalEnv::new(),
            )
            .expect("add suspending fragment");
        match machine
            .run_fragment_suspendable_parked(
                f,
                table,
                &mut NoDispatch,
                &(),
                ASK_TAG,
                RealmId(i as u64),
                ParkKind::Plain,
            )
            .expect("park fragment")
        {
            ParkedOutcome::Suspended { .. } => {}
            ParkedOutcome::Completed { .. } => panic!("fragment should suspend"),
        }
    }
    assert_eq!(machine.parked_count(), PARKS_PER_MACHINE);
    assert_eq!(machine.stowed_roots_count(), PARKS_PER_MACHINE);

    // Accrete plain JIT functions — the module growth cycle-scoping must reclaim.
    for i in 0..FRAGMENTS_PER_MACHINE {
        let f = machine
            .add_function(
                &format!("frag_{i}"),
                &build_value_fragment(1000 + i as i64),
                table,
                &ExternalEnv::new(),
            )
            .expect("add plain fragment");
        let _ = machine.run_fragment_pure(f).expect("plain fragment runs");
    }

    if resume_one {
        let _ = machine
            .resume_parked(
                first,
                &mut NoDispatch,
                &(),
                ResumeInput::Answer(Value::Lit(Literal::LitInt(1))),
            )
            .expect("resume one park before the drop");
        assert_eq!(machine.parked_count(), PARKS_PER_MACHINE - 1);
    }

    // High-water: the machine is fully built and still alive.
    let high_water = mem_bytes();

    // THE CYCLE BOUNDARY. Everything above dies here.
    drop(machine);
    high_water
}

// ───────────────────────────────────────────────────────────────────────────
// The report. Report-only on RSS (whose reading is allocator-dependent);
// ASSERTS on VSZ, where the JIT arena leak is unambiguous.
// ───────────────────────────────────────────────────────────────────────────

#[test]
#[serial]
fn report_cycle_scoped_drop_footprint() {
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            let table = table();

            // Warm up: one full cycle absorbs the process's one-time costs
            // (signal handlers, the cranelift ISA, log init) so the baseline
            // measures a steady state rather than startup.
            one_cycle(&table, true);
            let (vsz0, rss0) = mem_bytes();
            let maps0 = map_count();

            let mut peak_vsz = vsz0;
            let mut peak_rss = rss0;
            for i in 0..CYCLES {
                let (hv, hr) = one_cycle(&table, i % 2 == 0);
                peak_vsz = peak_vsz.max(hv);
                peak_rss = peak_rss.max(hr);
            }
            let (vsz1, rss1) = mem_bytes();
            let maps1 = map_count();

            println!(
                "\n=== cycle-scoped drop: {CYCLES} machines, each with \
                      {PARKS_PER_MACHINE} parked continuations + \
                      {FRAGMENTS_PER_MACHINE} fragments ==="
            );
            println!("{:<12} {:>12} {:>12}", "", "VSZ (MiB)", "RSS (MiB)");
            println!("{:<12} {:>12.1} {:>12.1}", "baseline", mib(vsz0), mib(rss0));
            println!(
                "{:<12} {:>12.1} {:>12.1}",
                "peak",
                mib(peak_vsz),
                mib(peak_rss)
            );
            println!("{:<12} {:>12.1} {:>12.1}", "after", mib(vsz1), mib(rss1));
            println!(
                "{:<12} {:>12.1} {:>12.1}",
                "retained",
                mib_delta(vsz0, vsz1),
                mib_delta(rss0, rss1)
            );
            println!(
                "{:<12} {:>12.2} {:>12.2}",
                "per machine",
                mib_delta(vsz0, vsz1) / CYCLES as f64,
                mib_delta(rss0, rss1) / CYCLES as f64
            );
            println!(
                "\nJIT arena reservation per machine: {:.0} MiB \
                 (pipeline.rs ArenaMemoryProvider::new_with_size)",
                mib(JIT_ARENA_BYTES)
            );
            // The VMA count, not the address space, is the practical ceiling.
            let maps_per_machine = (maps1 - maps0) as f64 / CYCLES as f64;
            let cap = max_map_count();
            println!(
                "/proc/self/maps entries: {maps0} -> {maps1} ({maps_per_machine:.2}/machine); \
                 vm.max_map_count = {cap}"
            );
            if maps_per_machine > 0.0 {
                println!(
                    "=> a process leaking at this rate exhausts vm.max_map_count after \
                     ~{:.0} cycles\n",
                    (cap - maps1) as f64 / maps_per_machine
                );
            } else {
                println!("=> no VMA growth per cycle\n");
            }

            // THE FINDING, PINNED AS AN ASSERTION.
            //
            // `ArenaMemoryProvider::drop` (cranelift-jit 0.129.1,
            // src/memory/arena.rs) frees its reservation ONLY if no segment was
            // finalized — "otherwise leak it since JIT memory may still be in
            // use". Every machine finalizes (`CodegenPipeline::finalize` in
            // `compile_inner`), and nothing calls `free_memory()`, so every
            // dropped machine leaks its whole 256 MiB reservation of address
            // space. Cycle-scoped drop does NOT reclaim the JIT module.
            //
            // This assertion is a FINDING GATE, not a wish: if someone later
            // makes the pipeline call `free_memory()` on drop, this fails and
            // should be REPLACED by its opposite, not relaxed.
            let vsz_per_machine = (vsz1 - vsz0) / CYCLES;
            assert!(
                vsz_per_machine >= JIT_ARENA_BYTES * 3 / 4,
                "expected each dropped machine to LEAK its ~{:.0} MiB JIT arena \
                 reservation (cranelift-jit leaks a finalized arena on drop); \
                 measured {:.1} MiB/machine. If this dropped because the arena \
                 is now actually freed, that is good news — rewrite this \
                 assertion, do not loosen it.",
                mib(JIT_ARENA_BYTES),
                mib(vsz_per_machine)
            );
        })
        .unwrap()
        .join()
        .unwrap();
}

// ───────────────────────────────────────────────────────────────────────────
// Separating the arena from everything else: a machine that compiles but never
// parks, never adds a fragment, and never runs. If its per-machine VSZ growth
// matches the full cycle's, the leak is the arena reservation alone and parks +
// fragments cost nothing extra at the address-space level.
// ───────────────────────────────────────────────────────────────────────────

#[test]
#[serial]
fn report_bare_machine_drop_footprint() {
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            let table = table();
            // Warm up.
            drop(
                JitEffectMachine::compile_session(&build_suspending(1, 1), &table, 4096)
                    .expect("compile_session"),
            );
            let (vsz0, rss0) = mem_bytes();
            for _ in 0..CYCLES {
                drop(
                    JitEffectMachine::compile_session(&build_suspending(1, 1), &table, 4096)
                        .expect("compile_session"),
                );
            }
            let (vsz1, rss1) = mem_bytes();
            println!(
                "\n=== bare compile_session x{CYCLES} (no parks, no fragments, no runs) ===\n\
                 VSZ retained: {:.1} MiB ({:.2} MiB/machine)\n\
                 RSS retained: {:.1} MiB ({:.2} MiB/machine)\n",
                mib_delta(vsz0, vsz1),
                mib_delta(vsz0, vsz1) / CYCLES as f64,
                mib_delta(rss0, rss1),
                mib_delta(rss0, rss1) / CYCLES as f64
            );
        })
        .unwrap()
        .join()
        .unwrap();
}

// ───────────────────────────────────────────────────────────────────────────
// Dropping a machine that still holds parked continuations must be clean: the
// stowed roots are deregistered before their `Box` cells are freed, and the
// session heap goes with it. Under poison + verify, a botched teardown ordering
// would surface as a corrupt read on the NEXT machine's collections.
// ───────────────────────────────────────────────────────────────────────────

#[test]
#[serial]
fn dropping_with_live_parks_is_clean_and_the_next_machine_is_unaffected() {
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            tidepool_codegen::host_fns::set_gc_poison(true);
            tidepool_codegen::host_fns::set_heap_verify(true);
            let table = table();

            for _ in 0..8 {
                // Never resume: every machine dies with all its parks live.
                let _ = one_cycle(&table, false);
            }

            // A fresh machine's registries are its own — a leaked stowed slot
            // from a dead machine could only corrupt through shared state, and
            // there is none: `MachineState` is per-machine.
            let mut machine =
                JitEffectMachine::compile_session(&build_suspending(4321, 9), &table, 2048)
                    .expect("compile_session");
            assert_eq!(machine.stowed_roots_count(), 0);
            assert_eq!(machine.parked_count(), 0);

            let id = match machine
                .run_suspendable_parked(&table, &mut NoDispatch, &(), ASK_TAG, RealmId(0))
                .expect("park")
            {
                ParkedOutcome::Suspended { id, .. } => id,
                ParkedOutcome::Completed { .. } => panic!("should suspend"),
            };
            match machine
                .resume_parked(
                    id,
                    &mut NoDispatch,
                    &(),
                    ResumeInput::Answer(Value::Lit(Literal::LitInt(9))),
                )
                .expect("resume")
            {
                ParkedOutcome::Completed { value, .. } => match &value {
                    Value::Con(cid, fields) if cid.0 == PAIR_ID.0 => {
                        assert_eq!(session_scaffold::expect_int(&fields[0]), 4321);
                        assert_eq!(session_scaffold::expect_int(&fields[1]), 9);
                    }
                    other => panic!("expected Pair, got {other:?}"),
                },
                ParkedOutcome::Suspended { .. } => panic!("should complete"),
            }

            tidepool_codegen::host_fns::set_gc_poison(false);
            tidepool_codegen::host_fns::set_heap_verify(false);
        })
        .unwrap()
        .join()
        .unwrap();
}
