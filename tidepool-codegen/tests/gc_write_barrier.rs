//! Regression suite for the generational write barrier (see `old_space.rs`'s
//! module doc for the invariant it enforces): before the fix, `OldSpace::tenure`
//! registered a persistent root for the tenured object itself, but old-space
//! was never rescanned by a minor GC (`raw::cheney_copy`'s from-range is the
//! nursery only) — a `writeSmallArray#`/`WriteArray` store into an
//! already-tenured array's external payload buffer was invisible to every
//! later minor collection, so a fresh nursery value written there had no path
//! back to a GC root once the array itself was off-nursery.
//!
//! Every test runs with `TIDEPOOL_GC_POISON`/`TIDEPOOL_HEAP_VERIFY` on so a
//! dangling read is deterministic (poison tag 0xDD) rather than
//! sometimes-works, except the two tests whose own doc comments explain why
//! they turn one of those off.

use serial_test::serial;
use tidepool_codegen::emit::ExternalEnv;
use tidepool_codegen::host_fns::{
    gc_trigger_call_count, heap_verify_run_count, reset_test_counters, set_gc_poison,
    set_heap_verify,
};
use tidepool_codegen::jit_machine::JitEffectMachine;
use tidepool_repr::datacon::DataCon;
use tidepool_repr::types::{Alt, AltCon, DataConId, Literal, PrimOpKind, VarId};
use tidepool_repr::{CoreExpr, CoreFrame, DataConTable, TreeBuilder};

#[path = "support/session_scaffold.rs"]
mod session_scaffold;
#[path = "support/session_scaffold_expect.rs"]
mod session_scaffold_expect;
#[path = "support/session_scaffold_gc_forcing.rs"]
mod session_scaffold_gc_forcing;
#[path = "support/session_scaffold_value.rs"]
mod session_scaffold_value;
use session_scaffold::C1;
use session_scaffold_expect::expect_int;
use session_scaffold_gc_forcing::build_gc_forcing_fragment;
use session_scaffold_value::build_value_fragment;

const I_HASH: DataConId = DataConId(7);

/// The stable external id `write_new`/`read_back` resolve the tenured array
/// through — an ordinary `ExternalEnv` member, not tag-dependent (see
/// `emit/expr.rs`'s Var-miss handling: resolution is keyed on `ExternalEnv`
/// membership, the 0xFE tag convention is incidental).
const ARR_EXT: VarId = VarId(0xFE00_0000_0000_1001);

/// The stable external id `force_thunk`/`read_after_gc` resolve the tenured
/// thunk-holding closure through (see `build_mk_thunk_holder`).
const THUNK_EXT: VarId = VarId(0xFE00_0000_0000_2002);

thread_local! {
    static VAR_CTR: std::cell::Cell<u64> = const { std::cell::Cell::new(2000) };
}
fn fresh_var() -> VarId {
    VAR_CTR.with(|c| {
        let v = c.get();
        c.set(v + 1);
        VarId(v)
    })
}
fn reset_ctr() {
    VAR_CTR.with(|c| c.set(2000));
}

/// Ensure `root` is the tree's last node (the emitter's root convention) by
/// wrapping it in a trivial `let` if some later push left it stranded
/// mid-tree.
fn fixup_root(tree: &mut CoreExpr, root: usize) -> CoreExpr {
    if root == tree.nodes.len() - 1 {
        return tree.clone();
    }
    let binder = fresh_var();
    let var_idx = tree.nodes.len();
    tree.nodes.push(CoreFrame::Var(binder));
    tree.nodes.push(CoreFrame::LetNonRec {
        binder,
        rhs: root,
        body: var_idx,
    });
    tree.clone()
}

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
        id: I_HASH,
        name: "I#".to_string(),
        tag: 7,
        rep_arity: 1,
        field_bangs: vec![],
        qualified_name: None,
        type_name: String::new(),
    });
    t
}

/// `let seed = I# seed_val in newSmallArray# 1# seed` — builds a length-1
/// boxed array whose only element is a heap `Con`. The whole expression is
/// the fragment's result, so `run_pure_and_bind` tenures the array's `Lit`
/// wrapper (and, transitively, `seed`) into old-space.
fn build_mk_array(seed_val: i64) -> CoreExpr {
    reset_ctr();
    let mut b = TreeBuilder::new();
    let seed_lit = b.push(CoreFrame::Lit(Literal::LitInt(seed_val)));
    let seed_con = b.push(CoreFrame::Con {
        tag: I_HASH,
        fields: vec![seed_lit],
    });
    let seed_var = fresh_var();
    let one = b.push(CoreFrame::Lit(Literal::LitInt(1)));
    let seed_var_ref = b.push(CoreFrame::Var(seed_var));
    let arr_rhs = b.push(CoreFrame::PrimOp {
        op: PrimOpKind::NewSmallArray,
        args: vec![one, seed_var_ref],
    });
    let let_seed = b.push(CoreFrame::LetNonRec {
        binder: seed_var,
        rhs: seed_con,
        body: arr_rhs,
    });
    let mut tree = b.build();
    fixup_root(&mut tree, let_seed)
}

/// `let payload = I# payload_val in writeSmallArray# arr 0# payload` — `arr`
/// is the EXTERNAL (already-tenured) array binding; `payload` is a fresh
/// nursery `Con` this fragment allocates and stores into the array's payload
/// slot. The `write` PrimOp is the fragment's OWN tail expression (its
/// `SsaVal::Raw(0, LIT_TAG_INT)` result is what the fragment returns) —
/// deliberately NOT routed through a further `let _ = write in body` wrapper,
/// because `emit_node_impl`'s `LetNonRec` handling skips emitting the RHS
/// entirely when its binder is dead in `body` (`emit/expr.rs`'s "Dead code
/// elimination: skip RHS if binder is unused" — a plain liveness check with
/// no notion that this particular RHS has an observable side effect). Binding
/// the write to an unused binder would silently drop the store.
fn build_write_new(payload_val: i64) -> CoreExpr {
    reset_ctr();
    let mut b = TreeBuilder::new();
    let payload_lit = b.push(CoreFrame::Lit(Literal::LitInt(payload_val)));
    let payload_con = b.push(CoreFrame::Con {
        tag: I_HASH,
        fields: vec![payload_lit],
    });
    let payload_var = fresh_var();
    let arr_ref = b.push(CoreFrame::Var(ARR_EXT));
    let idx0 = b.push(CoreFrame::Lit(Literal::LitInt(0)));
    let payload_ref = b.push(CoreFrame::Var(payload_var));
    let write = b.push(CoreFrame::PrimOp {
        op: PrimOpKind::WriteSmallArray,
        args: vec![arr_ref, idx0, payload_ref],
    });
    let let_payload = b.push(CoreFrame::LetNonRec {
        binder: payload_var,
        rhs: payload_con,
        body: write,
    });
    let mut tree = b.build();
    fixup_root(&mut tree, let_payload)
}

/// `case indexSmallArray# arr 0# of I# n -> n` — DEEP-inspects the element:
/// this forces a real load through the payload slot AND a tag-dispatch case
/// match on the loaded pointer, not just a non-null check. A dangling slot
/// either loads a stale/poisoned pointer whose tag matches no alt (clean
/// `CaseTrap`) or, if the address has since been reused, a wrong value.
fn build_read_back() -> CoreExpr {
    reset_ctr();
    let mut b = TreeBuilder::new();
    let arr_ref = b.push(CoreFrame::Var(ARR_EXT));
    let idx0 = b.push(CoreFrame::Lit(Literal::LitInt(0)));
    let idx_read = b.push(CoreFrame::PrimOp {
        op: PrimOpKind::IndexSmallArray,
        args: vec![arr_ref, idx0],
    });
    let n = fresh_var();
    let n_ref = b.push(CoreFrame::Var(n));
    let case_binder = fresh_var();
    let done = b.push(CoreFrame::Case {
        scrutinee: idx_read,
        binder: case_binder,
        alts: vec![Alt {
            con: AltCon::DataAlt(I_HASH),
            binders: vec![n],
            body: n_ref,
        }],
    });
    let mut tree = b.build();
    fixup_root(&mut tree, done)
}

/// `let mkBox = \n -> I# n in let x = mkBox boxed_val in let f = \_ -> x in f`
/// — `x`'s RHS is an `App` (GHC Core `let` is non-strict; `emit_node_impl`
/// thunkifies any non-trivial RHS — App/Case/Let/Jump — so `x` stays an
/// UNEVALUATED thunk rather than being evaluated eagerly), and `f`'s closure
/// captures that thunk (a bare `Var` reference — `heap_force` never touches
/// a Lam body until the closure is actually CALLED). The fragment's root is
/// `f` itself: `run_pure_and_bind`'s `deep_force` forces it to WHNF (identity
/// for a `Closure` — Tier1, not descended), so `x`'s thunk survives tenuring
/// unevaluated, exactly the shape `OldSpace::tenure`'s thunk-indirection loop
/// exists for.
fn build_mk_thunk_holder(boxed_val: i64) -> CoreExpr {
    reset_ctr();
    let mut b = TreeBuilder::new();
    let n_var = fresh_var();
    let n_ref = b.push(CoreFrame::Var(n_var));
    let box_body = b.push(CoreFrame::Con {
        tag: I_HASH,
        fields: vec![n_ref],
    });
    let mk_box = b.push(CoreFrame::Lam {
        binder: n_var,
        body: box_body,
    });
    let mk_box_var = fresh_var();
    let mk_box_ref = b.push(CoreFrame::Var(mk_box_var));
    let arg = b.push(CoreFrame::Lit(Literal::LitInt(boxed_val)));
    let x_rhs = b.push(CoreFrame::App {
        fun: mk_box_ref,
        arg,
    });
    let x_var = fresh_var();
    let x_ref = b.push(CoreFrame::Var(x_var));
    let ignored = fresh_var();
    let f_lam = b.push(CoreFrame::Lam {
        binder: ignored,
        body: x_ref,
    });
    let f_var = fresh_var();
    let f_ref = b.push(CoreFrame::Var(f_var));
    let let_f = b.push(CoreFrame::LetNonRec {
        binder: f_var,
        rhs: f_lam,
        body: f_ref,
    });
    let let_x = b.push(CoreFrame::LetNonRec {
        binder: x_var,
        rhs: x_rhs,
        body: let_f,
    });
    let let_mkbox = b.push(CoreFrame::LetNonRec {
        binder: mk_box_var,
        rhs: mk_box,
        body: let_x,
    });
    let mut tree = b.build();
    fixup_root(&mut tree, let_mkbox)
}

/// `case f 0# of I# n -> n` — calls the tenured closure `f` (via
/// `THUNK_EXT`), which returns `x`'s (possibly still-unevaluated) thunk
/// pointer; the `Case` forces it to WHNF (a data-case dispatch must know the
/// scrutinee's tag), running the thunk's body the first time this is
/// evaluated and following its `THUNK_EVALUATED` indirection every time
/// after. Deep-inspects: forces AND tag-matches, not just a non-null check.
fn build_force_via_call() -> CoreExpr {
    reset_ctr();
    let mut b = TreeBuilder::new();
    let f_ref = b.push(CoreFrame::Var(THUNK_EXT));
    let arg0 = b.push(CoreFrame::Lit(Literal::LitInt(0)));
    let call = b.push(CoreFrame::App {
        fun: f_ref,
        arg: arg0,
    });
    let n = fresh_var();
    let n_ref = b.push(CoreFrame::Var(n));
    let case_binder = fresh_var();
    let done = b.push(CoreFrame::Case {
        scrutinee: call,
        binder: case_binder,
        alts: vec![Alt {
            con: AltCon::DataAlt(I_HASH),
            binders: vec![n],
            body: n_ref,
        }],
    });
    let mut tree = b.build();
    fixup_root(&mut tree, done)
}

/// Control: write then read back through the SAME tenured array with NO GC
/// forced in between. Isolates the write/read mechanism itself (arg passing,
/// `ExternalEnv` slot resolution, `unbox_bytearray`) from the GC-rescan
/// question `tenured_array_write_survives_gc_g1` targets — if this control
/// failed, a G1 failure would prove nothing about the write barrier.
#[test]
#[serial]
fn tenured_array_write_visible_without_intervening_gc() {
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            reset_test_counters();
            set_heap_verify(true);
            set_gc_poison(true);
            let table = table();

            let dummy = build_value_fragment(0);
            let mut machine = JitEffectMachine::compile_session(&dummy, &table, 64 * 1024)
                .expect("compile_session");

            let mk_array = machine
                .add_function(
                    "mk_array",
                    &build_mk_array(111),
                    &table,
                    &ExternalEnv::new(),
                )
                .expect("add_function mk_array");
            let slot_arr = machine
                .run_pure_and_bind(mk_array)
                .expect("run_pure_and_bind mk_array");

            let mut env = ExternalEnv::new();
            env.insert(ARR_EXT, slot_arr.addr());
            let write_fn = machine
                .add_function("write_new", &build_write_new(777), &table, &env)
                .expect("add_function write_new");
            let _ = machine
                .run_fragment_pure(write_fn)
                .expect("run_fragment_pure write_new");

            let mut env2 = ExternalEnv::new();
            env2.insert(ARR_EXT, slot_arr.addr());
            let read_fn = machine
                .add_function("read_back", &build_read_back(), &table, &env2)
                .expect("add_function read_back");
            let result = machine
                .run_fragment_pure(read_fn)
                .expect("run_fragment_pure read_back");
            assert_eq!(
                expect_int(&result),
                777,
                "write must be visible to an immediately-following read with no GC in between"
            );

            set_gc_poison(false);
            set_heap_verify(false);
            drop(machine);
        })
        .unwrap()
        .join()
        .unwrap();
}

/// Reachability spike: `tenured_array_write_survives_gc_g1` drives the bug
/// through the most public path available.
///
/// G1: tenure a boxed `SmallArray#`, have a LATER fragment `writeSmallArray#`
/// a fresh nursery `Con` into it, force minor + doubling GC under a tiny
/// nursery, then deep-read the element back.
#[test]
#[serial]
fn tenured_array_write_survives_gc_g1() {
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            reset_test_counters();
            set_heap_verify(true);
            set_gc_poison(true);
            let verify_before = heap_verify_run_count();
            let table = table();

            // Tiny 2 KiB nursery — same recipe `nested_child_gc_rooting.rs`'s
            // `child_heap_doubling_then_parent_resumes` documents as tripping
            // the doubling re-evacuate (`live*4 > size*3`) under a depth-200
            // filler.
            let dummy = build_value_fragment(0);
            let mut machine =
                JitEffectMachine::compile_session(&dummy, &table, 2048).expect("compile_session");

            // Fragment 1: build + tenure the array (E — bind primitive tenures
            // the NF result; the array Lit wrapper has no thunk cells, so
            // exactly one persistent root — the top-level binding itself —
            // gets registered).
            let mk_array = machine
                .add_function(
                    "mk_array",
                    &build_mk_array(111),
                    &table,
                    &ExternalEnv::new(),
                )
                .expect("add_function mk_array");
            let slot_arr = machine
                .run_pure_and_bind(mk_array)
                .expect("run_pure_and_bind mk_array");
            assert_eq!(
                machine.persistent_roots_count(),
                1,
                "tenuring the array must register exactly one persistent root"
            );

            // Fragment 2: a LATER fragment resolves the tenured array via
            // ExternalEnv and writes a FRESH nursery Con into its payload slot.
            let mut env = ExternalEnv::new();
            env.insert(ARR_EXT, slot_arr.addr());
            let write_fn = machine
                .add_function("write_new", &build_write_new(777), &table, &env)
                .expect("add_function write_new");
            let write_result = machine
                .run_fragment_pure(write_fn)
                .expect("run_fragment_pure write_new");
            assert_eq!(
                expect_int(&write_result),
                0,
                "writeSmallArray# returns its 0 state token"
            );

            // Force >=1 minor GC (and, per the documented recipe, a doubling
            // pass) between the write and the read.
            let gc_before = gc_trigger_call_count();
            let filler = machine
                .add_function(
                    "filler",
                    &build_gc_forcing_fragment(200),
                    &table,
                    &ExternalEnv::new(),
                )
                .expect("add_function filler");
            let _ = machine
                .run_fragment_pure(filler)
                .expect("run_fragment_pure filler");
            let gc_after = gc_trigger_call_count();
            assert!(
                gc_after > gc_before,
                "filler fragment must have triggered at least one real GC \
                 (before={gc_before}, after={gc_after})"
            );

            // Fragment 3: resolve the SAME tenured array again and deep-read
            // element 0 back.
            let mut env2 = ExternalEnv::new();
            env2.insert(ARR_EXT, slot_arr.addr());
            let read_fn = machine
                .add_function("read_back", &build_read_back(), &table, &env2)
                .expect("add_function read_back");
            let result = machine.run_fragment_pure(read_fn);

            match result {
                Ok(v) => assert_eq!(
                    expect_int(&v),
                    777,
                    "array element must read back intact after minor + doubling GC"
                ),
                Err(e) => panic!(
                    "read_back fragment failed (this IS the predicted dangling-element \
                     corruption if the barrier is absent/disabled): {e:?}"
                ),
            }

            let verify_after = heap_verify_run_count();
            assert!(
                verify_after > verify_before,
                "heap_verify_run_count did not increase ({verify_before} -> {verify_after}) — \
                 the verifier never ran, so this test guarded nothing"
            );

            set_gc_poison(false);
            set_heap_verify(false);
            drop(machine);
        })
        .unwrap()
        .join()
        .unwrap();
}

/// UNIFICATION — the pre-existing thunk-indirection tenure path (a Tier1
/// closure tenured unforced, later forced, mutating its indirection cell to
/// point at a nursery result) is now routed through the SAME `write_barrier`
/// API as array writes — not a parallel mechanism. Checks this two ways:
/// (1) `remembered_slots_count()` increases at tenure time, proving the
/// thunk-indirection cell registered through the barrier's own tracking; (2)
/// forcing the thunk, then forcing real GC, then re-forcing (following the
/// now-`THUNK_EVALUATED` indirection) still returns the correct value — the
/// pre-existing "forced tenured thunk's result survives GC" behavior must not
/// regress now that it is barrier-routed instead of a special-case
/// `register_persistent_root` call.
#[test]
#[serial]
fn tenured_thunk_indirection_uses_write_barrier_and_survives_gc() {
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            reset_test_counters();
            set_heap_verify(true);
            set_gc_poison(true);
            let table = table();

            let dummy = build_value_fragment(0);
            let mut machine =
                JitEffectMachine::compile_session(&dummy, &table, 2048).expect("compile_session");

            let before = machine.remembered_slots_count();
            let mk_thunk = machine
                .add_function(
                    "mk_thunk_holder",
                    &build_mk_thunk_holder(555),
                    &table,
                    &ExternalEnv::new(),
                )
                .expect("add_function mk_thunk_holder");
            let slot = machine
                .run_pure_and_bind(mk_thunk)
                .expect("run_pure_and_bind mk_thunk_holder");
            let after_tenure = machine.remembered_slots_count();
            assert!(
                after_tenure > before,
                "tenure's thunk-indirection loop must register through the SAME \
                 write_barrier API remembered_slots_count tracks (before={before}, \
                 after={after_tenure})"
            );

            let mut env = ExternalEnv::new();
            env.insert(THUNK_EXT, slot.addr());
            let force_fn = machine
                .add_function("force_thunk", &build_force_via_call(), &table, &env)
                .expect("add_function force_thunk");
            let forced = machine
                .run_fragment_pure(force_fn)
                .expect("run_fragment_pure force_thunk");
            assert_eq!(
                expect_int(&forced),
                555,
                "forcing the thunk must run mkBox 555 and return 555"
            );

            let gc_before = gc_trigger_call_count();
            let filler = machine
                .add_function(
                    "filler",
                    &build_gc_forcing_fragment(200),
                    &table,
                    &ExternalEnv::new(),
                )
                .expect("add_function filler");
            let _ = machine
                .run_fragment_pure(filler)
                .expect("run_fragment_pure filler");
            assert!(
                gc_trigger_call_count() > gc_before,
                "filler fragment must have triggered at least one real GC"
            );

            let mut env2 = ExternalEnv::new();
            env2.insert(THUNK_EXT, slot.addr());
            let reread_fn = machine
                .add_function("read_after_gc", &build_force_via_call(), &table, &env2)
                .expect("add_function read_after_gc");
            let result = machine
                .run_fragment_pure(reread_fn)
                .expect("run_fragment_pure read_after_gc");
            assert_eq!(
                expect_int(&result),
                555,
                "the forced thunk's nursery result must survive GC via its remembered slot"
            );

            set_gc_poison(false);
            set_heap_verify(false);
            drop(machine);
        })
        .unwrap()
        .join()
        .unwrap();
}

/// MUTATION CHECK. With the write barrier force-disabled via
/// `set_write_barrier_disabled_for_test`, re-run the exact G1 scenario. This
/// MUST go red with the same predicted dangling-element corruption
/// (`CaseTrap`, gc-poison tag 0xDD/221) the reachability spike above
/// predicts — if it stayed green, the test would prove nothing.
///
/// Heap-verify is deliberately OFF here, unlike every other test in this
/// file. This test's claim is that the BARRIER is load-bearing: remove it and
/// the stranded element is read back as poison at the dereference. With
/// heap-verify on, `verify_tenured_graph` now detects the unrecorded store
/// EARLIER — at the collection that strands it — and aborts the process
/// before the read is ever reached, which would test the verifier rather than
/// the barrier. `heap_verify_catches_unrecorded_store_at_the_stranding_collection`
/// is the test for that second, separate claim.
#[test]
#[serial]
fn tenured_array_write_g1_reproduces_without_barrier() {
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            reset_test_counters();
            set_heap_verify(false);
            set_gc_poison(true);
            tidepool_codegen::host_fns::set_write_barrier_disabled_for_test(true);
            let table = table();

            let dummy = build_value_fragment(0);
            let mut machine =
                JitEffectMachine::compile_session(&dummy, &table, 2048).expect("compile_session");

            let mk_array = machine
                .add_function(
                    "mk_array",
                    &build_mk_array(111),
                    &table,
                    &ExternalEnv::new(),
                )
                .expect("add_function mk_array");
            let slot_arr = machine
                .run_pure_and_bind(mk_array)
                .expect("run_pure_and_bind mk_array");

            let mut env = ExternalEnv::new();
            env.insert(ARR_EXT, slot_arr.addr());
            let write_fn = machine
                .add_function("write_new", &build_write_new(777), &table, &env)
                .expect("add_function write_new");
            let _ = machine
                .run_fragment_pure(write_fn)
                .expect("run_fragment_pure write_new");

            let gc_before = gc_trigger_call_count();
            let filler = machine
                .add_function(
                    "filler",
                    &build_gc_forcing_fragment(200),
                    &table,
                    &ExternalEnv::new(),
                )
                .expect("add_function filler");
            let _ = machine
                .run_fragment_pure(filler)
                .expect("run_fragment_pure filler");
            assert!(
                gc_trigger_call_count() > gc_before,
                "filler fragment must have triggered at least one real GC"
            );

            let mut env2 = ExternalEnv::new();
            env2.insert(ARR_EXT, slot_arr.addr());
            let read_fn = machine
                .add_function("read_back", &build_read_back(), &table, &env2)
                .expect("add_function read_back");
            let result = machine.run_fragment_pure(read_fn);

            tidepool_codegen::host_fns::set_write_barrier_disabled_for_test(false);
            set_gc_poison(false);
            set_heap_verify(false);

            match result {
                Ok(v) => panic!(
                    "EXPECTED this to fail with the barrier disabled — instead got a value \
                     ({v:?}) with no corruption. A green-either-way test proves nothing; \
                     something else in the change is masking the bug."
                ),
                Err(e) => {
                    // This IS the mutation evidence: with the barrier disabled, the
                    // scenario reproduces the exact pre-fix corruption.
                    eprintln!(
                        "mutation check: barrier disabled -> G1 reproduces corruption: {e:?}"
                    );
                }
            }

            drop(machine);
        })
        .unwrap()
        .join()
        .unwrap();
}

/// The write barrier's own verifier gate: with the barrier force-disabled AND
/// `TIDEPOOL_HEAP_VERIFY` on, the unrecorded old-to-young store is caught by
/// `verify_tenured_graph` at the collection that strands the target — not
/// later, at whatever next dereferences it.
///
/// This is the claim that matters about the verifier pass: it is INDEPENDENT
/// of the barrier. A verifier that walked the barrier's remembered set could
/// only ever inspect stores the barrier already caught, so it would be blind
/// to precisely this failure. Walking the tenured object graph instead means
/// a slot the barrier never recorded is still found.
///
/// Run in a SUBPROCESS because the detection aborts rather than unwinds:
/// `gc_trigger` is `extern "C"`, so a panic raised inside a JIT-triggered
/// collection hits `panic_cannot_unwind` and becomes SIGABRT. That is the
/// pre-existing behavior of `verify_heap_post_gc` too — this pass is simply
/// the first thing to exercise it through the JIT path — and it is acceptable
/// for an opt-in diagnostic whose contract is to fail loudly, but it does
/// mean the failure cannot be observed with `catch_unwind`.
#[test]
#[serial]
fn heap_verify_catches_unrecorded_store_at_the_stranding_collection() {
    const CHILD_ENV: &str = "TIDEPOOL_TEST_VERIFIER_ABORT_CHILD";
    const CHILD_TEST: &str = "heap_verify_catches_unrecorded_store_at_the_stranding_collection";

    if std::env::var(CHILD_ENV).is_ok() {
        // ── child role: provoke the abort ────────────────────────────────
        run_g1_scenario_with_barrier_disabled(true);
        // Reaching here means no collection detected the stranded slot.
        eprintln!("CHILD-REACHED-END-WITHOUT-DETECTION");
        return;
    }

    let exe = std::env::current_exe().expect("current_exe");
    let out = std::process::Command::new(exe)
        .args(["--exact", CHILD_TEST, "--nocapture", "--test-threads=1"])
        .env(CHILD_ENV, "1")
        .output()
        .expect("spawn child test process");

    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert!(
        !stdout.contains("CHILD-REACHED-END-WITHOUT-DETECTION")
            && !stderr.contains("CHILD-REACHED-END-WITHOUT-DETECTION"),
        "the child ran the whole scenario without any collection detecting the \
         unrecorded store — verify_tenured_graph did not fire.\n\
         stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stderr.contains("[HEAP VERIFY] tenured-graph violation after GC"),
        "expected the tenured-graph verifier to name the violation on stderr.\n\
         stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stderr.contains("dangling old-to-young reference"),
        "expected the diagnostic to identify this as an unrecorded old-to-young \
         store.\nstderr:\n{stderr}"
    );
    assert!(
        !out.status.success(),
        "child must have died on the verifier abort, got {:?}",
        out.status
    );
}

/// The G1 scenario with the write barrier force-disabled: tenure a boxed
/// array, have a later fragment write a freshly allocated nursery `Con` into
/// its payload, then force collections. With `heap_verify` on, a collection
/// is expected to detect the stranded slot and abort the process, so this
/// returns only when nothing detected it.
fn run_g1_scenario_with_barrier_disabled(heap_verify: bool) {
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(move || {
            reset_test_counters();
            set_heap_verify(heap_verify);
            set_gc_poison(true);
            tidepool_codegen::host_fns::set_write_barrier_disabled_for_test(true);
            let table = table();

            let dummy = build_value_fragment(0);
            let mut machine =
                JitEffectMachine::compile_session(&dummy, &table, 2048).expect("compile_session");

            let mk_array = machine
                .add_function(
                    "mk_array",
                    &build_mk_array(111),
                    &table,
                    &ExternalEnv::new(),
                )
                .expect("add_function mk_array");
            let slot_arr = machine
                .run_pure_and_bind(mk_array)
                .expect("run_pure_and_bind mk_array");

            let mut env = ExternalEnv::new();
            env.insert(ARR_EXT, slot_arr.addr());
            let write_fn = machine
                .add_function("write_new", &build_write_new(777), &table, &env)
                .expect("add_function write_new");
            let _ = machine
                .run_fragment_pure(write_fn)
                .expect("run_fragment_pure write_new");

            let filler = machine
                .add_function(
                    "filler",
                    &build_gc_forcing_fragment(200),
                    &table,
                    &ExternalEnv::new(),
                )
                .expect("add_function filler");
            let _ = machine.run_fragment_pure(filler);

            tidepool_codegen::host_fns::set_write_barrier_disabled_for_test(false);
            set_gc_poison(false);
            set_heap_verify(false);
            drop(machine);
        })
        .unwrap()
        .join()
        .unwrap();
}
