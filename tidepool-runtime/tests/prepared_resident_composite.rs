//! B1: the real prepared engine driven through `SessionRegistry` (via its
//! [`SingleSlot`] facade), proving rungs 2-5 of the acceptance ladder work
//! TOGETHER through the real session/registry substrate rather than each in
//! isolation the way `prepared_execution.rs`'s own tests pin them one at a
//! time:
//!
//! - rung 2 (bind/import/cross-program call): S6's own producer/consumer
//!   setup (`retained_import_end_to_end_links_consumer_against_bound_producer_tops`),
//!   run inside a checkout instead of directly on a bare machine.
//! - rung 3 (park/resume out of order across a collection): C0's own
//!   pattern (`c0_two_installed_programs_park_and_resume_out_of_order_with_a_collection_between`),
//!   but with the machine actually leaving and re-entering the registry
//!   between every park and every resume.
//! - rung 4/5 (realm-scoped cancellation, actor-shaped independence): C1's
//!   own pattern (`two_realms_share_one_machine_cancel_reset_close_independently_of_each_other`),
//!   with each realm standing in for one actor incarnation's own resource
//!   scope — retiring one via `close_realm` must never disturb the other's
//!   still-live state.
//!
//! `SingleSlot<M, H>`'s own checkout/settle protocol is already tested
//! generically in `tidepool-runtime/src/session/registry.rs` against a
//! `FakeMachine`; this file's whole point is proving the REAL production
//! machine type (`tidepool_codegen::prepared_program::PreparedMachine` — the
//! inner engine `PreparedEngine`, the deleted `PreparedRuntime`'s own real
//! wrapped type, is built on) survives that protocol under Send-correctness
//! and simulated park/resume across a "moved to another thread and back"
//! cycle.
//!
//! `H` here is a plain `(RealmId, PreparedHandle)` tuple: this test never
//! calls `PreparedMachine::park` (that mechanism belongs to a suspended
//! *effect* continuation, not to a retained Send value moving across a
//! checkout boundary) — the "hole" carried by `SingleSlot` across a turn is
//! just a Send-safe handle correlated with the realm it is live under, which
//! a `Copy` tuple already satisfies (`SingleSlot`'s own bound is `H: Clone +
//! PartialEq + Debug`).
//!
//! Helpers below duplicate small pieces of `prepared_execution.rs`
//! (`requirements`, `top_named`, `take_managed`, the freer-resume resume
//! loop, ...): Rust integration test files are separate crates, so nothing
//! private (or even public but un-re-exported test-local) can be `use`d
//! across them.

use tidepool_bridge::Value;
use tidepool_codegen::host_fns::RuntimeError;
use tidepool_codegen::machine::MachineDisposition;
use tidepool_codegen::prepared_program::{
    CompiledProgram, ExecutionError, PreparedCallOptions, PreparedHandle,
    PreparedInput as CodegenInput, PreparedMachine, PreparedMachineOptions,
    PreparedOuter as CodegenOuter, PreparedResult, ProgramId,
};
use tidepool_codegen::suspension::RealmId;
use tidepool_repr::execution_schema::{
    link_program, parse_program, Architecture, DecodeLimits, Endianness, Group, HeapRhs,
    ImportedValue, MachineImports, PreparedProgram, ProgramRequirements, RuntimeRep,
    SymbolIdentity, TargetDescriptor, TopBinding, EXECUTION_ABI_VERSION, SCHEMA_VERSION,
};
use tidepool_repr::freer_names::{
    find_declared, E_DEFINING_MODULE, UNION_DEFINING_MODULE, VAL_DEFINING_MODULE,
};
use tidepool_repr::{DataConId, SessionId};
use tidepool_runtime::session::registry::SingleSlot;

// ---- fixtures -------------------------------------------------------------

const IMPORT_PRODUCER_ARTIFACT: &[u8] =
    include_bytes!("../../haskell/test-prepared-stg/fixtures/import-producer.cbor");
const IMPORT_CONSUMER_ARTIFACT: &[u8] =
    include_bytes!("../../haskell/test-prepared-stg/fixtures/import-consumer.cbor");
/// `consumerValueAt 0#`'s GHC-computed value. See `prepared_execution.rs`'s
/// own `IMPORT_CONSUMER_EXPECTATIONS` doc: `import-consumer.cbor`'s pinned
/// entries are `consumerValueAt`/`consumerEntries`, never `consumerResult`/
/// `consumerResultAt` (a direct call to an import is not yet admitted, per
/// `s6_direct_global_call_is_not_yet_admitted` there) -- this test only ever
/// drives `consumerValueAt`.
const IMPORT_CONSUMER_EXPECTATIONS: &str =
    include_str!("../../haskell/test-prepared-stg/ImportConsumerExpectations.json");
const FREER_RESUME_ARTIFACT: &[u8] =
    include_bytes!("../../haskell/test-prepared-stg/fixtures/freer-resume.cbor");
const FREER_RESUME_EXPECTATIONS: &str =
    include_str!("../../haskell/test-prepared-stg/FreerResumeExpectations.json");

fn requirements() -> ProgramRequirements {
    ProgramRequirements {
        schema_version: SCHEMA_VERSION,
        projection_profile: "ghc-9.12-prepared-stg".into(),
        toolchain: "ghc-9.12.2".into(),
        execution_abi_version: EXECUTION_ABI_VERSION,
        target: TargetDescriptor {
            architecture: Architecture::X86_64,
            endianness: Endianness::Little,
            pointer_width: 64,
            word_width: 64,
            abi: "sysv64".into(),
            features: vec![],
        },
    }
}

fn machine_options() -> PreparedMachineOptions {
    PreparedMachineOptions {
        nursery_bytes: RunOptionsNurseryBytes::default_bytes(),
    }
}

/// `RunOptions::default().nursery_bytes` without pulling in the whole
/// `RunOptions` call-options type just for its capacity default.
struct RunOptionsNurseryBytes;
impl RunOptionsNurseryBytes {
    fn default_bytes() -> usize {
        tidepool_codegen::prepared_program::RunOptions::default().nursery_bytes
    }
}

fn call_options(collect_before_observation: bool) -> PreparedCallOptions {
    PreparedCallOptions {
        observation_budget: tidepool_codegen::prepared_program::RunOptions::default()
            .observation_budget,
        collect_before_observation,
    }
}

fn tops(prepared: &PreparedProgram) -> Vec<TopBinding> {
    prepared
        .bindings()
        .iter()
        .flat_map(|group| match group {
            Group::NonRecursive(top) => std::slice::from_ref(top).to_vec(),
            Group::Recursive(tops) => tops.clone(),
        })
        .collect()
}

fn top_named(prepared: &PreparedProgram, module: &str, occurrence: &str) -> TopBinding {
    tops(prepared)
        .into_iter()
        .find(|top| top.identity.module == module && top.identity.occurrence == occurrence)
        .unwrap_or_else(|| panic!("artifact has no top {module}.{occurrence}"))
}

/// The first of `occurrences` present as a top of `module`: GHC's
/// worker/wrapper split may leave only the `$w`-prefixed worker as a top.
fn top_named_any(prepared: &PreparedProgram, module: &str, occurrences: &[&str]) -> TopBinding {
    occurrences
        .iter()
        .find_map(|occurrence| {
            tops(prepared)
                .into_iter()
                .find(|top| top.identity.module == module && top.identity.occurrence == *occurrence)
        })
        .unwrap_or_else(|| panic!("artifact has no top {module}.{occurrences:?}"))
}

/// How many physical arguments a function top's projected signature takes.
fn top_arity(prepared: &PreparedProgram, top: &TopBinding) -> usize {
    match &top.binding.rhs {
        HeapRhs::Function { signature, .. } => prepared.signatures()[signature.0 as usize]
            .arguments
            .iter()
            .filter(|rep| !matches!(rep, RuntimeRep::Void))
            .count(),
        _ => 0,
    }
}

fn producer_identity(occurrence: &str) -> SymbolIdentity {
    SymbolIdentity {
        unit: "main".to_owned(),
        module: "ImportProducer".to_owned(),
        namespace: "value".to_owned(),
        occurrence: occurrence.to_owned(),
        record_parent: None,
    }
}

fn expected_consumer_value() -> Vec<i64> {
    let parsed: serde_json::Value = serde_json::from_str(IMPORT_CONSUMER_EXPECTATIONS)
        .expect("ImportConsumerExpectations.json parses as JSON");
    parsed["expectations"]["consumerValueAt"]["value"]
        .as_array()
        .expect("consumerValue expectation is a list")
        .iter()
        .map(|element| {
            element
                .as_i64()
                .expect("consumerValue elements are integers")
        })
        .collect()
}

/// Flatten an observed `[Int]`: a cons cell is `Con(_, [head, tail])`, nil
/// is `Con(_, [])`, and each head is a bare literal or an `I#` box around one.
fn observed_int_list(value: &Value) -> Vec<i64> {
    let mut out = Vec::new();
    let mut cursor = value;
    loop {
        match cursor {
            Value::Con(_, fields) if fields.is_empty() => return out,
            Value::Con(_, fields) if fields.len() == 2 => {
                let head = match &fields[0] {
                    Value::Lit(tidepool_repr::Literal::LitInt(n)) => *n,
                    Value::Con(_, boxed) => match boxed.as_slice() {
                        [Value::Lit(tidepool_repr::Literal::LitInt(n))] => *n,
                        other => panic!("unexpected boxed list head {other:?}"),
                    },
                    other => panic!("unexpected list head {other:?}"),
                };
                out.push(head);
                cursor = &fields[1];
            }
            other => panic!("unexpected list shape {other:?}"),
        }
    }
}

fn freer_resume_top(prepared: &PreparedProgram, occurrence: &str) -> TopBinding {
    tops(prepared)
        .into_iter()
        .find(|top| top.identity.module == "FreerResume" && top.identity.occurrence == occurrence)
        .unwrap_or_else(|| panic!("FreerResume artifact has no top named {occurrence}"))
}

fn freer_resume_constructor_identity(
    prepared: &PreparedProgram,
    module: &str,
    occurrence: &str,
) -> DataConId {
    find_declared(prepared.constructors(), module, occurrence)
        .unwrap_or_else(|| panic!("freer-resume artifact has no constructor named {occurrence}"))
        .host_id
}

fn expected_program_value() -> i64 {
    let parsed: serde_json::Value = serde_json::from_str(FREER_RESUME_EXPECTATIONS)
        .expect("FreerResumeExpectations.json parses as JSON");
    parsed["expectations"]["program"]["value"]
        .as_i64()
        .expect("FreerResumeExpectations.json's program expectation is an integer")
}

struct FreerResumeFixture {
    program_top: TopBinding,
    resume_int_top: TopBinding,
    ask_argument_top: TopBinding,
    val_result_top: TopBinding,
    e_id: DataConId,
    val_id: DataConId,
    union_id: DataConId,
}

impl FreerResumeFixture {
    fn load() -> Self {
        let prepared = parse_program(
            FREER_RESUME_ARTIFACT,
            &requirements(),
            DecodeLimits::default(),
        )
        .expect("freer-resume artifact parses");
        Self {
            program_top: freer_resume_top(&prepared, "program"),
            resume_int_top: freer_resume_top(&prepared, "resumeInt"),
            ask_argument_top: freer_resume_top(&prepared, "askArgument"),
            val_result_top: freer_resume_top(&prepared, "valResult"),
            e_id: freer_resume_constructor_identity(&prepared, E_DEFINING_MODULE, "E"),
            val_id: freer_resume_constructor_identity(&prepared, VAL_DEFINING_MODULE, "Val"),
            union_id: freer_resume_constructor_identity(&prepared, UNION_DEFINING_MODULE, "Union"),
        }
    }
}

fn take_managed(fields: &mut [PreparedResult], index: usize) -> PreparedHandle {
    match std::mem::replace(&mut fields[index], PreparedResult::Void) {
        PreparedResult::Managed(handle) => handle,
        PreparedResult::Void | PreparedResult::Scalar(_) => {
            panic!("field {index} expected a managed value")
        }
    }
}

fn take_scalar(fields: &[PreparedResult], index: usize) -> u64 {
    match fields[index] {
        PreparedResult::Scalar(word) => word,
        PreparedResult::Void => panic!("field {index} expected a scalar value, got Void"),
        PreparedResult::Managed(_) => {
            panic!("field {index} expected a scalar value, got Managed")
        }
    }
}

/// Build one consumer program's `MachineImports` from its own declared
/// globals plus a by-identity handle map: `entry_signature`/`generation` are
/// read directly off the consumer's own declaration (there is no separate
/// session-level generation ledger at this layer any more — `retain_top`
/// mints a handle with no generation of its own), and `evaluated` is read
/// live off the machine the handles actually live on.
fn import_bindings_for(
    consumer: &PreparedProgram,
    machine: &PreparedMachine<'static>,
    handles: &[(SymbolIdentity, PreparedHandle)],
) -> Result<MachineImports, ExecutionError> {
    let mut imports = MachineImports::default();
    for declaration in consumer.globals() {
        let Some((_, handle)) = handles.iter().find(|(id, _)| *id == declaration.identity) else {
            continue;
        };
        let entry_signature = declaration
            .entry_signature
            .map(|signature| consumer.signatures()[signature.0 as usize].clone());
        let imported = ImportedValue {
            identity: declaration.identity.clone(),
            rep: handle.rep(),
            entry_signature,
            evaluated: machine.handle_is_evaluated(*handle)?,
            generation: declaration.required_generation.unwrap_or(0),
        };
        imports
            .values
            .insert(declaration.identity.clone(), imported);
    }
    Ok(imports)
}

/// `prepared_execution.rs`'s own `drive_freer_program_to_val_in`, but
/// parameterized on `realm` instead of hardcoding `RealmId::ROOT`: this
/// composite test drives freer-resume continuations under two different
/// non-root realms (one per "incarnation").
fn drive_to_val_in(
    machine: &mut PreparedMachine<'static>,
    program: ProgramId,
    fixture: &FreerResumeFixture,
    realm: RealmId,
    mut outer: PreparedHandle,
) -> i64 {
    loop {
        let CodegenOuter::Constructor {
            identity,
            mut fields,
        } = machine
            .inspect_outer(outer, realm)
            .expect("the retained Eff value survives its collection and inspects");

        if identity == fixture.val_id {
            assert_eq!(fields.len(), 1, "Val has exactly one field");
            let boxed = take_managed(&mut fields, 0);
            assert!(machine.release(boxed));

            let value_result = machine
                .run_entry_retained(
                    program,
                    fixture.val_result_top.binding.id,
                    &[CodegenInput::Managed(outer)],
                    call_options(true),
                    realm,
                )
                .expect("valResult (Val (I# n) -> n) forces the settled Int");
            let mut value_values = value_result.values.into_iter();
            let Some(PreparedResult::Scalar(word)) = value_values.next() else {
                panic!("valResult must return one scalar Int#");
            };
            assert!(value_values.next().is_none());
            assert!(machine.release(outer));
            return word as i64;
        }

        assert_eq!(
            identity, fixture.e_id,
            "an Eff value at WHNF is either Val or E"
        );
        assert_eq!(fields.len(), 2, "E has exactly two fields: Union and Arrs");
        let union = take_managed(&mut fields, 0);
        let k = take_managed(&mut fields, 1);
        assert!(machine.release(outer));

        let CodegenOuter::Constructor {
            identity: union_identity,
            fields: mut union_fields,
        } = machine.inspect_outer(union, realm).expect("Union inspects");
        assert_eq!(union_identity, fixture.union_id);
        assert_eq!(union_fields.len(), 2);
        let tag = take_scalar(&union_fields, 0);
        assert_eq!(tag, 0, "the only effect in '[Req] is index 0");
        let payload = take_managed(&mut union_fields, 1);
        assert!(machine.release(union));

        let ask_result = machine
            .run_entry_retained(
                program,
                fixture.ask_argument_top.binding.id,
                &[CodegenInput::Managed(payload)],
                call_options(true),
                realm,
            )
            .expect("askArgument (Ask (I# n) -> n) forces the Ask request's Int#");
        let mut ask_values = ask_result.values.into_iter();
        let Some(PreparedResult::Scalar(n)) = ask_values.next() else {
            panic!("askArgument must return one scalar Int#");
        };
        assert!(ask_values.next().is_none());
        assert!(machine.release(payload));

        let resumed = machine
            .run_entry_retained(
                program,
                fixture.resume_int_top.binding.id,
                &[CodegenInput::Managed(k), CodegenInput::Scalar(n)],
                call_options(true),
                realm,
            )
            .expect("resumeInt (qApp k (I# n)) applies the retained continuation");
        assert!(machine.release(k));
        let mut resumed_values = resumed.values.into_iter();
        let Some(PreparedResult::Managed(next_outer)) = resumed_values.next() else {
            panic!("resumeInt must return one managed `Eff` outer value");
        };
        assert!(resumed_values.next().is_none());
        outer = next_outer;
    }
}

/// B1: rungs 2-5 driven TOGETHER through `SingleSlot<PreparedMachine,
/// (RealmId, PreparedHandle)>` -- the composite proof no single-rung test
/// above attempts.
#[test]
fn session_registry_drives_prepared_runtime_through_bind_import_park_resume_cancel_and_retire() {
    // ---- setup: parse both S6 artifacts and locate their tops BEFORE
    // either program is moved into the machine (mirrors S6's own ordering:
    // `top_named` needs `&PreparedProgram` before installing consumes it).
    let producer_prepared = parse_program(
        IMPORT_PRODUCER_ARTIFACT,
        &requirements(),
        DecodeLimits::default(),
    )
    .expect("import-producer artifact parses");
    let producer_value_top = top_named(&producer_prepared, "ImportProducer", "producerValue");
    let producer_fn_top = top_named(&producer_prepared, "ImportProducer", "producerFn");

    let consumer_prepared = parse_program(
        IMPORT_CONSUMER_ARTIFACT,
        &requirements(),
        DecodeLimits::default(),
    )
    .expect("import-consumer artifact parses");
    let value_identity = producer_identity("producerValue");
    let fn_identity = producer_identity("producerFn");
    let consumer_value_top = top_named_any(
        &consumer_prepared,
        "ImportConsumer",
        &["consumerValueAt", "$wconsumerValueAt"],
    );
    let scalar_args = vec![0_u64; top_arity(&consumer_prepared, &consumer_value_top)];

    let fixture = FreerResumeFixture::load();

    let producer_linked = link_program(producer_prepared, &MachineImports::default())
        .expect("producer has no imports of its own, so linking against an empty snapshot closes");
    let producer_compiled = CompiledProgram::compile(&producer_linked).expect("producer compiles");
    let (machine, producer_program) =
        PreparedMachine::new(producer_compiled, machine_options()).expect("producer installs");

    // `SessionId` has no `fresh()` constructor (it is a bare `pub struct
    // SessionId(pub u64)`, per `tidepool-repr::session_ids`) -- every other
    // integration test in this crate that needs one just picks a literal.
    let session_id = SessionId(910_001);
    let slot: SingleSlot<PreparedMachine<'static>, (RealmId, PreparedHandle)> = SingleSlot::new();
    slot.install(session_id, machine)
        .unwrap_or_else(|_| panic!("install must succeed against a freshly constructed slot"));

    // ==== Turn 1 -- incarnation A: bind + import + cross-program call =====
    // Everything this turn leases (the two S6 imports) is scoped to
    // `realm_a`, the resource scope standing in for incarnation A's own
    // actor-shaped lifetime.
    let checkout = slot
        .checkout_run()
        .expect("the freshly installed session is Idle");
    let (mut machine, receipt) = checkout.into_parts();

    let realm_a = RealmId::fresh();

    let bound_value = machine
        .retain_top(producer_program, producer_value_top.binding.id)
        .expect("producerValue retains as a session-level (ROOT-realm) handle");
    let bound_fn = machine
        .retain_top(producer_program, producer_fn_top.binding.id)
        .expect("producerFn retains as a session-level (ROOT-realm) handle");

    let handles = [
        (value_identity.clone(), bound_value),
        (fn_identity.clone(), bound_fn),
    ];
    let imports = import_bindings_for(&consumer_prepared, &machine, &handles)
        .expect("both imports resolve their evaluatedness against the live machine");
    let consumer_linked =
        link_program(consumer_prepared, &imports).expect("consumer links against both bindings");
    let consumer_compiled = machine
        .compile_for_install(&consumer_linked)
        .expect("consumer compiles against the machine's shared descriptor interner");
    let mut bindings = tidepool_codegen::prepared_program::ImportBindings::new();
    bindings.insert(value_identity.clone(), bound_value);
    bindings.insert(fn_identity.clone(), bound_fn);
    let consumer_program = machine
        .install_program(consumer_compiled, bindings)
        .expect("consumer installs under realm_a's own leases");

    let expected = expected_consumer_value();
    let observed = machine
        .run_entry(
            consumer_program,
            consumer_value_top.binding.id,
            &scalar_args,
            call_options(true),
            realm_a,
        )
        .expect("consumerValueAt reads producerValue through its import slot");
    assert_eq!(observed.values.len(), 1);
    assert_eq!(observed_int_list(&observed.values[0]), expected);

    slot.settle_suspended(receipt, machine, Vec::new());
    assert_eq!(
        slot.kind(),
        Some(tidepool_runtime::session::registry::SlotKind::Idle),
        "turn 1 parked nothing, so the slot settles back to Idle"
    );

    // ==== Turn 2 -- incarnation A parks a continuation ====================
    // A second, independent program (freer-resume, no imports) installed on
    // the SAME machine, run to its first suspension under `realm_a`.
    let checkout = slot
        .checkout_run()
        .expect("turn 2 checks out the idle session");
    let (mut machine, receipt) = checkout.into_parts();

    let freer_linked = link_program(
        parse_program(
            FREER_RESUME_ARTIFACT,
            &requirements(),
            DecodeLimits::default(),
        )
        .expect("freer-resume artifact parses"),
        &MachineImports::default(),
    )
    .expect("freer-resume artifact has no imports of its own");
    let freer_compiled = machine
        .compile_for_install(&freer_linked)
        .expect("freer-resume compiles");
    let freer_program = machine
        .install_program(
            freer_compiled,
            tidepool_codegen::prepared_program::ImportBindings::new(),
        )
        .expect("freer-resume artifact installs as a second program, no imports");
    let first_a = machine
        .run_entry_retained(
            freer_program,
            fixture.program_top.binding.id,
            &[],
            call_options(true),
            realm_a,
        )
        .expect("incarnation A's `program` run suspends on its first Ask");
    let Some(PreparedResult::Managed(outer_a)) = first_a.values.into_iter().next() else {
        panic!("`program` must return one managed `Eff` outer value");
    };
    let hole_a = (realm_a, outer_a);

    slot.settle_suspended(receipt, machine, vec![hole_a]);

    // ==== Turn 3 -- incarnation B: a second, independent realm ============
    // `checkout_run` (not `checkout_resume`) on purpose: incarnation B does
    // UNRELATED work, not resuming A's hole. The registry's own doc on
    // `checkout_run` says a fresh turn over parked frames is ordinary.
    let checkout = slot
        .checkout_run()
        .expect("checkout_run also admits a turn over a Suspended slot");
    let (mut machine, receipt) = checkout.into_parts();

    let realm_b = RealmId::fresh();
    let first_b = machine
        .run_entry_retained(
            freer_program,
            fixture.program_top.binding.id,
            &[],
            call_options(true),
            realm_b,
        )
        .expect("incarnation B's own unrelated `program` run suspends on its own first Ask");
    let Some(PreparedResult::Managed(outer_b)) = first_b.values.into_iter().next() else {
        panic!("`program` must return one managed `Eff` outer value");
    };
    let mut hole_b = (realm_b, outer_b);

    // A's hole survived B's entirely unrelated turn.
    assert_eq!(machine.handle_realm(outer_a), Some(realm_a));

    slot.settle_suspended(receipt, machine, vec![hole_a, hole_b]);

    // ==== Turn 4 -- resume A's hole out of order, to completion ===========
    let checkout = slot
        .checkout_resume(&hole_a)
        .expect("hole_a is a member of the suspended slot's parked holes");
    let (mut machine, receipt) = checkout.into_parts();

    let value_a = drive_to_val_in(&mut machine, freer_program, &fixture, realm_a, outer_a);
    assert_eq!(value_a, expected_program_value());

    slot.settle_suspended(receipt, machine, vec![hole_b]);

    // ==== Turn 5 -- realm-scoped cancellation on B, then resume to
    //      completion =======================================================
    let checkout = slot
        .checkout_run()
        .expect("checkout_run also admits a turn over hole_b's suspended slot");
    let (mut machine, receipt) = checkout.into_parts();

    // Split B's parked suspension into Union/continuation (mirrors
    // `two_realms_share_one_machine_cancel_reset_close_independently_of_each_other`).
    // `inspect_outer` never consumes `outer_b` itself (it mints fresh
    // handles for the constructor's fields), so `hole_b` -- minted against
    // `outer_b` -- stays valid across this split.
    let CodegenOuter::Constructor {
        identity: id_b,
        fields: mut fields_b,
    } = machine
        .inspect_outer(outer_b, realm_b)
        .expect("B's parked Eff value inspects cleanly under its own realm");
    assert_eq!(id_b, fixture.e_id);
    assert_eq!(fields_b.len(), 2);
    let union_b = take_managed(&mut fields_b, 0);
    let k_cont_b = take_managed(&mut fields_b, 1);
    assert!(machine.release(outer_b));
    // `outer_b`'s own root is spent now; re-mint `hole_b` against the
    // continuation actually being resumed below, `k_cont_b`, so the
    // liveness check after the cancelled attempt names the thing that must
    // survive it.
    hole_b = (realm_b, k_cont_b);

    let CodegenOuter::Constructor {
        identity: union_id_b,
        fields: mut union_fields_b,
    } = machine
        .inspect_outer(union_b, realm_b)
        .expect("B's Union inspects");
    assert_eq!(union_id_b, fixture.union_id);
    let payload_b = take_managed(&mut union_fields_b, 1);
    assert!(machine.release(union_b));

    let ask_b = machine
        .run_entry_retained(
            freer_program,
            fixture.ask_argument_top.binding.id,
            &[CodegenInput::Managed(payload_b)],
            call_options(true),
            realm_b,
        )
        .expect("askArgument forces B's Ask request's Int# before any cancellation is requested");
    let Some(PreparedResult::Scalar(n_b)) = ask_b.values.into_iter().next() else {
        panic!("askArgument must return one scalar Int#");
    };
    assert!(machine.release(payload_b));

    // Cancel ONLY realm_b, after k_cont_b is already parked and its Ask
    // answer already forced -- the same "flag set after the parked state is
    // reached" discipline C1's own two-realm test documents.
    let cancel_b = machine.realm_cancel_handle(realm_b);
    cancel_b.cancel();

    let cancelled = machine.run_entry_retained(
        freer_program,
        fixture.resume_int_top.binding.id,
        &[CodegenInput::Managed(k_cont_b), CodegenInput::Scalar(n_b)],
        call_options(true),
        realm_b,
    );
    assert!(
        matches!(
            &cancelled,
            Err(ExecutionError::Runtime(failure)) if failure.cause == RuntimeError::Cancelled
        ),
        "a cancelled realm must refuse the call before it runs, got {cancelled:?}"
    );
    assert_eq!(
        machine.handle_realm(k_cont_b),
        Some(realm_b),
        "a cancelled resumeInt call must not consume k_cont_b's parked continuation"
    );

    cancel_b.reset();

    let resumed_b = machine
        .run_entry_retained(
            freer_program,
            fixture.resume_int_top.binding.id,
            &[CodegenInput::Managed(k_cont_b), CodegenInput::Scalar(n_b)],
            call_options(true),
            realm_b,
        )
        .expect(
            "k_cont_b remains valid after a cancellation that committed nothing, and now proceeds",
        );
    assert!(machine.release(k_cont_b));
    let Some(PreparedResult::Managed(next_outer_b)) = resumed_b.values.into_iter().next() else {
        panic!("resumeInt must return one managed `Eff` outer value");
    };

    let value_b = drive_to_val_in(&mut machine, freer_program, &fixture, realm_b, next_outer_b);
    assert_eq!(
        value_b, value_a,
        "both incarnations run the same deterministic freer-resume computation"
    );
    let _ = hole_b;

    // Nothing parked anymore: both incarnations' continuations are fully
    // driven to their settled `Val`.
    slot.settle_suspended(receipt, machine, Vec::new());

    // ==== Turn 6 -- retirement: close each realm independently ============
    let checkout = slot
        .checkout_run()
        .expect("final checkout for realm retirement");
    let (mut machine, receipt) = checkout.into_parts();

    let (frames_a, handles_a) = machine.close_realm(realm_a);
    assert_eq!(
        handles_a, 0,
        "realm_a's own parked continuation was already fully driven and released in turn 4"
    );
    assert_eq!(
        frames_a, 0,
        "the prepared engine never parks a continuation as a frame in this test (no `park` call)"
    );
    // NOTE: leasing/lease-count bookkeeping is not asserted here. The
    // deleted `PreparedRuntime` wrapper's own `BindingTable`
    // `acquire_leases`/`lease_count`/`release_leases` calls were never wired
    // to the production `PreparedMachine`/`PreparedEngine` path -- they are
    // dead bookkeeping on the prepared route (only the Core-route
    // continuation-capture path in `tidepool-runtime/src/session/resident.rs`
    // actually calls `acquire_leases`/`release_leases`). `bound_value`/
    // `bound_fn` are session-level (`RealmId::ROOT`) handles from
    // `retain_top`, entirely unaffected by closing `realm_a`.

    let (frames_b, handles_b) = machine.close_realm(realm_b);
    assert_eq!(
        handles_b, 0,
        "realm_b's own parked continuation was already fully driven and released in turn 5"
    );
    assert_eq!(frames_b, 0);

    // The two incarnations' retirements did not disturb each other: both
    // calls above are independent of order (realm_a closed first here, but
    // neither touches the other's state).
    assert!(
        machine.release(bound_value),
        "bound_value is still a live session-level handle after both realms closed"
    );
    assert!(
        machine.release(bound_fn),
        "bound_fn is still a live session-level handle after both realms closed"
    );

    assert_eq!(
        machine.handle_count(),
        0,
        "every value either incarnation produced, plus both session-level bindings, is released"
    );
    assert_eq!(machine.disposition(), MachineDisposition::Reusable);

    // Tear the session down fully: nothing is left to resume.
    slot.settle_retire(receipt);
    assert_eq!(
        slot.current_id(),
        None,
        "settle_retire clears SingleSlot's current entry"
    );
}
