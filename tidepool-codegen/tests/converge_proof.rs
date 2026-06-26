//! Wave 1.B — THE CONVERGE PROOF: live-heap value persistence end-to-end.
//!
//! Assembles the full value-plane re-entry path on ONE live session machine:
//!
//!   1. `compile_session` a machine (dummy entry).
//!   2. `add_function` fragment-1 that builds a `Con` value (the "bound value").
//!   3. `run_pure_and_bind` it: run → `deep_force`-to-NF (K) → tenure into
//!      old-space (E) → `register_persistent_root` (D) → stable `RootSlot`.
//!   4. Seed `ExternalEnv[stableVarId] = slot` (the GC-safe slot indirection).
//!   5. `add_function` fragment-2 (`case x of C n -> n`) referencing that id.
//!   6. `run_fragment_pure` it against the RETAINED session heap → resolves the
//!      tenured value from fragment-1. Assert it reads back correctly.
//!
//! VARIANT: force a REAL GC (a heavy filler fragment that overflows the small
//! nursery) BETWEEN the bind and the read, then read again — proving the tenured
//! value + its persistent root survive a collection and fragment-2 still
//! resolves correctly (the value lives in old-space, outside the minor-GC
//! from-range; its slot is a persistent root the GC traces but never relocates).
//!
//! This is the mechanical smoke proof of the §4 1.B seam — necessary, not the
//! full acceptance proof (that is the Wave-2/3 multi-turn real-entry-point test).

use tidepool_codegen::emit::ExternalEnv;
use tidepool_codegen::jit_machine::JitEffectMachine;
use tidepool_repr::datacon::DataCon;
use tidepool_repr::types::{AltCon, Alt, DataConId, Literal, VarId};
use tidepool_repr::{CoreExpr, CoreFrame, DataConTable, TreeBuilder};

use serial_test::serial;

/// High-byte tag a real Option-C session binder carries (`stableVarId`,
/// 0xFE-tagged external). Resolution is keyed on `ExternalEnv` membership, not
/// the tag — the tag only makes the fixture faithful.
const EXTERNAL_TAG: u64 = 0xFE;

fn external_var_id(key: u64) -> VarId {
    VarId((EXTERNAL_TAG << 56) | (key & ((1u64 << 56) - 1)))
}

/// The data constructor `C1 :: Int -> T` (arity 1) shared by all fragments.
const C1: DataConId = DataConId(1);

/// A DataConTable with `C1`. (ConTags/EffCont entries are unnecessary — every
/// fragment here runs through the *pure* path, which never consults them.)
fn table_with_c1() -> DataConTable {
    let mut table = DataConTable::new();
    table.insert(DataCon {
        id: C1,
        name: "C1".to_string(),
        tag: 1,
        rep_arity: 1,
        field_bangs: vec![],
        qualified_name: None,
    });
    table
}

/// fragment-1: `C1 n` — builds the bound value (a Con wrapping a boxed Int).
fn build_value_fragment(n: i64) -> CoreExpr {
    let mut b = TreeBuilder::new();
    let lit = b.push(CoreFrame::Lit(Literal::LitInt(n)));
    b.push(CoreFrame::Con {
        tag: C1,
        fields: vec![lit],
    });
    b.build()
}

/// fragment-2: `case x of C1 n -> n` — x is the seeded external session binder.
/// Resolves the tenured value built by fragment-1 and projects its field.
fn build_reference_fragment(x: VarId) -> CoreExpr {
    let mut b = TreeBuilder::new();
    let body = b.push(CoreFrame::Var(VarId(11))); // the bound field `n`
    let scrut = b.push(CoreFrame::Var(x)); // the session binder `x`
    b.push(CoreFrame::Case {
        scrutinee: scrut,
        binder: VarId(10), // case-scrutinee binder (unused)
        alts: vec![Alt {
            con: AltCon::DataAlt(C1),
            binders: vec![VarId(11)],
            body,
        }],
    });
    b.build()
}

/// A heavy allocator: `let rec f = \x -> let g1 = C1 x; g2 = C1 g1 in C1 x
/// in f (f (… (42)))` applied `depth` times — eagerly builds ~3·depth Cons,
/// overflowing a small session nursery and forcing a real minor GC. (Mirrors
/// the `make_gc_forcing_setup` helper used by the 1.A seam test.)
fn build_gc_forcing_fragment(depth: usize) -> CoreExpr {
    let mut b = TreeBuilder::new();
    let var_x = b.push(CoreFrame::Var(VarId(0)));
    let g1_rhs = b.push(CoreFrame::Con {
        tag: C1,
        fields: vec![var_x],
    });
    let var_g1 = b.push(CoreFrame::Var(VarId(1)));
    let g2_rhs = b.push(CoreFrame::Con {
        tag: C1,
        fields: vec![var_g1],
    });
    let final_con = b.push(CoreFrame::Con {
        tag: C1,
        fields: vec![var_x],
    });
    let let_g2 = b.push(CoreFrame::LetNonRec {
        binder: VarId(2),
        rhs: g2_rhs,
        body: final_con,
    });
    let let_g1 = b.push(CoreFrame::LetNonRec {
        binder: VarId(1),
        rhs: g1_rhs,
        body: let_g2,
    });
    let lam_x = b.push(CoreFrame::Lam {
        binder: VarId(0),
        body: let_g1,
    });
    let mut current = b.push(CoreFrame::Lit(Literal::LitInt(42)));
    for _ in 0..depth {
        let f_var = b.push(CoreFrame::Var(VarId(99)));
        current = b.push(CoreFrame::App {
            fun: f_var,
            arg: current,
        });
    }
    b.push(CoreFrame::LetRec {
        bindings: vec![(VarId(99), lam_x)],
        body: current,
    });
    b.build()
}

/// Extract an `Int` from a pure-run result Value (Lit or 1-arg Con wrapping one).
fn expect_int(v: &tidepool_eval::value::Value) -> i64 {
    use tidepool_eval::value::Value;
    match v {
        Value::Lit(Literal::LitInt(n)) => *n,
        Value::Con(_, fields) if fields.len() == 1 => expect_int(&fields[0]),
        other => panic!("expected an Int result, got {other:?}"),
    }
}

/// Basic converge proof: a second fragment JITed into a live session machine
/// resolves a tenured value built by the first fragment.
#[test]
#[serial]
fn converge_second_fragment_resolves_first_fragments_tenured_value() {
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            tidepool_codegen::host_fns::clear_persistent_roots();
            let table = table_with_c1();

            // 1. Session machine with a dummy entry (never run).
            let dummy = build_value_fragment(0);
            let mut machine = JitEffectMachine::compile_session(&dummy, &table, 1 << 16)
                .expect("compile_session");

            // 2/3. add_function fragment-1, bind it (run → deep_force → tenure → root).
            let frag1 = machine
                .add_function("frag1", &build_value_fragment(7), &table, &ExternalEnv::new())
                .expect("add_function frag1");
            let slot = machine
                .run_pure_and_bind(frag1)
                .expect("run_pure_and_bind frag1");

            // The slot must be a registered persistent root, holding a live Con(C1,[7]).
            assert_eq!(
                tidepool_codegen::host_fns::persistent_roots_count(),
                1,
                "tenure must register exactly one persistent root"
            );

            // 4. Seed the ExternalEnv: stableVarId -> the tenured root slot.
            let x = external_var_id(0x5151);
            let mut env = ExternalEnv::new();
            env.insert(x, slot.addr());

            // 5. add_function fragment-2 referencing x.
            let frag2 = machine
                .add_function("frag2", &build_reference_fragment(x), &table, &env)
                .expect("add_function frag2");

            // 6. Run it against the retained session heap.
            let result = machine
                .run_fragment_pure(frag2)
                .expect("run_fragment_pure frag2");
            assert_eq!(
                expect_int(&result),
                7,
                "fragment-2 must resolve fragment-1's tenured value (7)"
            );

            drop(machine);
            assert_eq!(
                tidepool_codegen::host_fns::persistent_roots_count(),
                0,
                "machine drop clears persistent roots"
            );
        })
        .unwrap()
        .join()
        .unwrap();
}

/// GC-between-runs variant: a REAL minor GC fires between the bind and the read
/// (a heavy filler fragment overflows a tiny nursery). The tenured value + its
/// persistent root must survive, and fragment-2 must still resolve correctly.
#[test]
#[serial]
fn converge_survives_real_gc_between_runs() {
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            tidepool_codegen::host_fns::clear_persistent_roots();
            tidepool_codegen::host_fns::reset_test_counters();
            let table = table_with_c1();

            // Tiny 2 KiB nursery so the filler fragment forces a collection.
            let dummy = build_value_fragment(0);
            let mut machine =
                JitEffectMachine::compile_session(&dummy, &table, 2048).expect("compile_session");

            // Bind x = C1 13.
            let frag1 = machine
                .add_function("frag1", &build_value_fragment(13), &table, &ExternalEnv::new())
                .expect("add_function frag1");
            let slot = machine.run_pure_and_bind(frag1).expect("bind frag1");
            assert_eq!(tidepool_codegen::host_fns::persistent_roots_count(), 1);

            // Record what the slot points at BEFORE the GC.
            // SAFETY: slot is a live, registered persistent root.
            let tenured_before = unsafe { slot.current() };

            let x = external_var_id(0x7373);
            let mut env = ExternalEnv::new();
            env.insert(x, slot.addr());
            let frag2 = machine
                .add_function("frag2", &build_reference_fragment(x), &table, &env)
                .expect("add_function frag2");

            // Read once (no GC yet).
            assert_eq!(
                expect_int(&machine.run_fragment_pure(frag2).expect("read 1")),
                13
            );

            // --- Force a REAL GC: a heavy filler fragment over the 2 KiB nursery. ---
            let gc_before = tidepool_codegen::host_fns::gc_trigger_call_count();
            let filler = machine
                .add_function("filler", &build_gc_forcing_fragment(80), &table, &ExternalEnv::new())
                .expect("add_function filler");
            let _ = machine.run_fragment_pure(filler).expect("run filler");
            let gc_after = tidepool_codegen::host_fns::gc_trigger_call_count();
            assert!(
                gc_after > gc_before,
                "filler fragment must have triggered at least one real GC \
                 (before={gc_before}, after={gc_after})"
            );

            // Tenured value lives in old-space (outside the nursery from-range),
            // so a minor GC neither moves it nor changes its slot.
            let tenured_after = unsafe { slot.current() };
            assert_eq!(
                tenured_after, tenured_before,
                "tenured value must not be relocated by a minor GC"
            );
            assert_eq!(
                tidepool_codegen::host_fns::persistent_roots_count(),
                1,
                "persistent root must survive the collection"
            );

            // --- Read AGAIN after the GC: fragment-2 still resolves correctly. ---
            assert_eq!(
                expect_int(&machine.run_fragment_pure(frag2).expect("read 2 (post-GC)")),
                13,
                "fragment-2 must still resolve the tenured value after a real GC"
            );

            drop(machine);
            assert_eq!(tidepool_codegen::host_fns::persistent_roots_count(), 0);
        })
        .unwrap()
        .join()
        .unwrap();
}
