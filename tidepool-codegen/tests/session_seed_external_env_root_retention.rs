//! D9 integration proof: narrowing `BindingTable::seed_external_env` to only
//! the VarIds a fragment actually references must NOT narrow which bindings
//! survive GC as persistent roots — those are two different jobs that used to
//! be served by one loop. Binds two values, compiles+runs a fragment that
//! seeds (and references) only ONE of them, forces a REAL minor GC via a
//! nursery-overflowing filler fragment, then reads the OTHER value back —
//! despite it never having appeared in any `ExternalEnv` before the GC.
//!
//! Root retention happens entirely at bind time (`OldSpace::tenure` ->
//! `register_persistent_root`, see `old_space.rs`), independent of anything a
//! later `seed_external_env` call chooses to seed. If narrowing the seeded set
//! had accidentally narrowed root retention too, `y` below would be a
//! use-after-collection: `TIDEPOOL_GC_POISON`/`TIDEPOOL_HEAP_VERIFY` make that
//! deterministic (a bad tag / verify panic) rather than a sometimes-works race.

use serial_test::serial;

use tidepool_codegen::binding_table::{BindingEntry, BindingTable, BoundValue};
use tidepool_codegen::emit::ExternalEnv;
use tidepool_codegen::jit_machine::JitEffectMachine;
use tidepool_codegen::old_space::RootSlot;
use tidepool_repr::datacon::DataCon;
use tidepool_repr::types::VarId;
use tidepool_repr::{BindingName, DataConTable, Generation, SessionModule, SessionVarId};

#[path = "support/session_scaffold.rs"]
mod session_scaffold;
use session_scaffold::{build_gc_forcing_fragment, build_reference_fragment, build_value_fragment};
use session_scaffold::{expect_int, C1};

/// High-byte tag a real Option-C session binder carries (`stableVarId`,
/// 0xFE-tagged external) — mirrors `converge_proof.rs`'s fixture helper.
const EXTERNAL_TAG: u64 = 0xFE;

fn external_var_id(key: u64) -> VarId {
    VarId((EXTERNAL_TAG << 56) | (key & ((1u64 << 56) - 1)))
}

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

fn binding_entry(name: &str, var: VarId, gen: u64, slot: RootSlot) -> BindingEntry {
    BindingEntry {
        defining_expr: None,
        name: BindingName(name.to_string()),
        id: SessionVarId::from_var(var),
        module: SessionModule::val(Generation(gen)),
        value: BoundValue::Tier0Forced(slot),
        type_display: Some("Int".to_string()),
    }
}

#[test]
#[serial]
fn unreferenced_bindings_survive_a_real_gc_after_narrowed_seeding() {
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            // Poison from-space so a missed root is a deterministic bad tag,
            // not a sometimes-works race.
            tidepool_codegen::host_fns::set_gc_poison(true);
            tidepool_codegen::host_fns::set_heap_verify(true);
            tidepool_codegen::host_fns::reset_test_counters();

            let table = table_with_c1();

            // Tiny nursery so the filler fragment below forces a real minor GC.
            let dummy = build_value_fragment(0);
            let mut machine =
                JitEffectMachine::compile_session(&dummy, &table, 2048).expect("compile_session");

            let x = external_var_id(0x1111);
            let y = external_var_id(0x2222);

            // Bind x = C1 41, y = C1 42 — both tenured + persistent-rooted at
            // BIND time (`run_pure_and_bind` -> `OldSpace::tenure`), entirely
            // before either is ever considered for seeding into an env.
            let frag_x = machine
                .add_function(
                    "bind_x",
                    &build_value_fragment(41),
                    &table,
                    &ExternalEnv::new(),
                )
                .expect("add_function bind_x");
            let slot_x = machine.run_pure_and_bind(frag_x).expect("bind x");

            let frag_y = machine
                .add_function(
                    "bind_y",
                    &build_value_fragment(42),
                    &table,
                    &ExternalEnv::new(),
                )
                .expect("add_function bind_y");
            let slot_y = machine.run_pure_and_bind(frag_y).expect("bind y");

            assert_eq!(
                machine.persistent_roots_count(),
                2,
                "both binds must be registered as persistent GC roots"
            );

            let mut bindings = BindingTable::new();
            bindings.bind(binding_entry("x", x, 1, slot_x));
            bindings.bind(binding_entry("y", y, 1, slot_y));

            // Only `x` is referenced by the fragment about to be compiled —
            // the narrowed env must contain exactly that, not `y` too.
            let env_x = bindings.seed_external_env(&[x]);
            assert_eq!(
                env_x.len(),
                1,
                "narrowed env must seed only the referenced var"
            );
            assert!(env_x.get(x).is_some());
            assert!(
                env_x.get(y).is_none(),
                "y must NOT be seeded — this fragment doesn't reference it"
            );

            let frag_read_x = machine
                .add_function("read_x", &build_reference_fragment(x), &table, &env_x)
                .expect("add_function read_x");
            assert_eq!(
                expect_int(&machine.run_fragment_pure(frag_read_x).expect("read x")),
                41,
                "x must resolve via the narrowed env"
            );

            // --- Force a REAL GC. `y` has never appeared in any `ExternalEnv`
            // up to this point — only its persistent root (registered at bind
            // time, above) is what could keep it alive across this.
            let gc_before = tidepool_codegen::host_fns::gc_trigger_call_count();
            let filler = machine
                .add_function(
                    "filler",
                    &build_gc_forcing_fragment(80),
                    &table,
                    &ExternalEnv::new(),
                )
                .expect("add_function filler");
            let _ = machine.run_fragment_pure(filler).expect("run filler");
            let gc_after = tidepool_codegen::host_fns::gc_trigger_call_count();
            assert!(
                gc_after > gc_before,
                "filler fragment must have triggered at least one real GC \
                 (before={gc_before}, after={gc_after})"
            );

            assert_eq!(
                machine.persistent_roots_count(),
                2,
                "both roots must survive the collection regardless of narrowed seeding"
            );

            // NOW seed+reference y for the first time — AFTER the GC it was
            // never seeded across. If narrowing seeding had accidentally
            // narrowed root retention, this would read a relocated/freed
            // from-space pointer (a poisoned 0xDD tag under GC_POISON).
            let env_y = bindings.seed_external_env(&[y]);
            assert_eq!(env_y.len(), 1);
            assert!(env_y.get(x).is_none());
            assert!(env_y.get(y).is_some());

            let frag_read_y = machine
                .add_function("read_y", &build_reference_fragment(y), &table, &env_y)
                .expect("add_function read_y");
            assert_eq!(
                expect_int(
                    &machine
                        .run_fragment_pure(frag_read_y)
                        .expect("read y (post-GC)")
                ),
                42,
                "y must still resolve correctly after surviving a GC it was \
                 never seeded across"
            );

            tidepool_codegen::host_fns::set_gc_poison(false);
            tidepool_codegen::host_fns::set_heap_verify(false);
            drop(machine);
        })
        .unwrap()
        .join()
        .unwrap();
}
