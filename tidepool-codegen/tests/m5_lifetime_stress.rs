use serial_test::serial;
use tidepool_codegen::emit::ExternalEnv;
use tidepool_codegen::jit_machine::JitEffectMachine;
use tidepool_heap::execution_descriptor::{EntryMetadata, ObjectDescriptor, ObjectKind};
use tidepool_repr::datacon::DataCon;
use tidepool_repr::execution_schema::{
    Architecture, Endianness, ResultContract, RuntimeRep, Signature, StorageLayout,
    TargetDescriptor,
};
use tidepool_repr::{DataConTable, VarId};

use crate::session_scaffold::C1;
use crate::session_scaffold_expect::expect_int;
use crate::session_scaffold_gc_forcing::build_gc_forcing_fragment;
use crate::session_scaffold_reference::build_reference_fragment;
use crate::session_scaffold_value::build_value_fragment;

const EXTERNAL_TAG: u64 = 0xFE;

fn external_var(key: u64) -> VarId {
    VarId((EXTERNAL_TAG << 56) | (key & ((1u64 << 56) - 1)))
}

fn table() -> DataConTable {
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

fn target() -> TargetDescriptor {
    TargetDescriptor {
        architecture: Architecture::X86_64,
        endianness: Endianness::Little,
        pointer_width: 64,
        word_width: 64,
        abi: "system-v".into(),
        features: Vec::new(),
    }
}

/// The new descriptor contract records PAP semantic positions independently
/// from storage. The retained JIT consumer then proves that a compiled global
/// slot, not a retired source root, keeps the captured value and code usable
/// across a moving collection and source-root retirement.
#[test]
#[serial]
fn pap_void_prefix_and_code_global_survive_collection_then_retire() {
    let pap_signature = Signature {
        arguments: vec![RuntimeRep::Void, RuntimeRep::Word(8), RuntimeRep::LiftedRef],
        results: ResultContract::Returns(vec![RuntimeRep::LiftedRef]),
    };
    let pap_payload =
        StorageLayout::for_reps(&target(), &pap_signature.arguments).expect("PAP storage layout");
    let pap = ObjectDescriptor::new(
        ObjectKind::Pap,
        pap_payload,
        Some(EntryMetadata::new(pap_signature, 1)),
    )
    .expect("PAP descriptor");
    assert_eq!(pap.payload().logical_to_stored(), &[None, Some(0), Some(1)]);
    assert_eq!(pap.trace_offsets(), &[16]);

    tidepool_codegen::host_fns::set_gc_poison(true);
    tidepool_codegen::host_fns::set_heap_verify(true);
    tidepool_codegen::host_fns::reset_test_counters();

    let table = table();
    let mut machine = JitEffectMachine::compile_session(&build_value_fragment(0), &table, 2048)
        .expect("compile session");
    let value = machine
        .add_function(
            "m5_bind_global",
            &build_value_fragment(42),
            &table,
            &ExternalEnv::new(),
        )
        .expect("compile bound value");
    let root = machine.run_pure_and_bind(value).expect("bind value");

    let global = external_var(0x505);
    let mut env = ExternalEnv::new();
    env.insert(global, root.addr());
    let read = machine
        .add_function(
            "m5_read_global",
            &build_reference_fragment(global),
            &table,
            &env,
        )
        .expect("compile global reader");
    assert_eq!(
        expect_int(
            &machine
                .run_fragment_pure(read)
                .expect("initial global read")
        ),
        42
    );

    let collections_before = tidepool_codegen::host_fns::gc_trigger_call_count();
    let filler = machine
        .add_function(
            "m5_force_collection",
            &build_gc_forcing_fragment(160),
            &table,
            &ExternalEnv::new(),
        )
        .expect("compile collection filler");
    let _ = machine
        .run_fragment_pure(filler)
        .expect("execute collection filler");
    assert!(
        tidepool_codegen::host_fns::gc_trigger_call_count() > collections_before,
        "fixture must execute a real moving collection"
    );
    assert_eq!(
        expect_int(
            &machine
                .run_fragment_pure(read)
                .expect("post-GC global read")
        ),
        42
    );

    machine.retire_scope_root(root);
    assert_eq!(machine.persistent_roots_count(), 0);
    assert_eq!(
        expect_int(
            &machine
                .run_fragment_pure(read)
                .expect("code-root read after source-root retirement")
        ),
        42,
        "the compiled reader's code root must outlive the retired source root"
    );

    drop(machine);
    tidepool_codegen::host_fns::set_gc_poison(false);
    tidepool_codegen::host_fns::set_heap_verify(false);
}
