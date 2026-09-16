//! B1: `PreparedRuntime` driven through `SessionRegistry` (via its
//! [`SingleSlot`] facade), proving rungs 2-5 of the acceptance ladder work
//! TOGETHER through the real session/registry substrate rather than each in
//! isolation the way `prepared_execution.rs`'s own tests pin them one at a
//! time:
//!
//! - rung 2 (bind/import/cross-program call): S6's own producer/consumer
//!   setup (`retained_import_end_to_end_links_consumer_against_bound_producer_tops`),
//!   run inside a checkout instead of directly on a bare `PreparedRuntime`.
//! - rung 3 (park/resume out of order across a collection): C0's own
//!   pattern (`c0_two_installed_programs_park_and_resume_out_of_order_with_a_collection_between`),
//!   but with the machine actually leaving and re-entering the registry
//!   between every park and every resume.
//! - rung 4/5 (realm-scoped cancellation, actor-shaped independence): C1's
//!   own pattern (`two_realms_share_one_machine_cancel_reset_close_independently_of_each_other`),
//!   with each realm standing in for one actor incarnation's own resource
//!   scope — retiring one via `close_realm_report` must never disturb the
//!   other's still-live state.
//!
//! This is the first test that ever puts a `PreparedRuntime` inside a
//! `SessionRegistry`/`SingleSlot`. Every turn below is a separate
//! `checkout_run`/`checkout_resume` -> `into_parts` -> ... -> `settle_*`
//! cycle, exactly the shape a real frontend would use, settled EXACTLY ONCE
//! per checkout so no `Checkout`/`CheckoutReceipt` is ever dropped
//! unsettled.
//!
//! Helpers below duplicate small pieces of `prepared_execution.rs`
//! (`requirements`, `top_named`, `take_managed`, the freer-resume resume
//! loop, ...): Rust integration test files are separate crates, so nothing
//! private (or even public but un-re-exported test-local) can be `use`d
//! across them.

use tidepool_bridge::Value;
use tidepool_codegen::prepared_program::ProgramId;
use tidepool_repr::execution_schema::{
    parse_program, Architecture, DecodeLimits, Endianness, Group, HeapRhs, MachineImports,
    PreparedProgram, ProgramRequirements, RuntimeRep, SymbolIdentity, TargetDescriptor, TopBinding,
    EXECUTION_ABI_VERSION, SCHEMA_VERSION,
};
use tidepool_repr::freer_names::{
    find_declared, E_DEFINING_MODULE, UNION_DEFINING_MODULE, VAL_DEFINING_MODULE,
};
use tidepool_repr::{DataConId, Generation, SessionId};
use tidepool_runtime::prepared_execution::{
    PreparedArgument, PreparedHole, PreparedOuter, PreparedRuntime, PreparedRuntimeError,
    PreparedValue, PreparedValueResult, RealmId,
};
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

fn take_managed(fields: &mut [PreparedValueResult], index: usize) -> PreparedValue {
    match std::mem::replace(&mut fields[index], PreparedValueResult::Void) {
        PreparedValueResult::Managed(value) => value,
        PreparedValueResult::Void | PreparedValueResult::Scalar(_) => {
            panic!("field {index} expected a managed value")
        }
    }
}

fn take_scalar(fields: &[PreparedValueResult], index: usize) -> u64 {
    match fields[index] {
        PreparedValueResult::Scalar(word) => word,
        PreparedValueResult::Void => panic!("field {index} expected a scalar value, got Void"),
        PreparedValueResult::Managed(_) => {
            panic!("field {index} expected a scalar value, got Managed")
        }
    }
}

/// `prepared_execution.rs`'s own `drive_freer_program_to_val_in`, but
/// parameterized on `realm` instead of hardcoding `RealmId::ROOT`: this
/// composite test drives freer-resume continuations under two different
/// non-root realms (one per "incarnation").
fn drive_to_val_in(
    runtime: &mut PreparedRuntime,
    program: ProgramId,
    fixture: &FreerResumeFixture,
    realm: RealmId,
    mut outer: PreparedValue,
) -> i64 {
    loop {
        let PreparedOuter::Constructor {
            identity,
            mut fields,
        } = runtime
            .inspect_outer(&outer, realm)
            .expect("the retained Eff value survives its collection and inspects");

        if identity == fixture.val_id {
            assert_eq!(fields.len(), 1, "Val has exactly one field");
            let boxed = take_managed(&mut fields, 0);
            assert!(runtime.release(boxed));

            let value_result = runtime
                .run_entry_retained_in(
                    program,
                    fixture.val_result_top.binding.id,
                    &[PreparedArgument::Managed(&outer)],
                    true,
                    realm,
                )
                .expect("valResult (Val (I# n) -> n) forces the settled Int");
            let mut value_values = value_result.values.into_iter();
            let Some(PreparedValueResult::Scalar(word)) = value_values.next() else {
                panic!("valResult must return one scalar Int#");
            };
            assert!(value_values.next().is_none());
            assert!(runtime.release(outer));
            return word as i64;
        }

        assert_eq!(
            identity, fixture.e_id,
            "an Eff value at WHNF is either Val or E"
        );
        assert_eq!(fields.len(), 2, "E has exactly two fields: Union and Arrs");
        let union = take_managed(&mut fields, 0);
        let k = take_managed(&mut fields, 1);
        assert!(runtime.release(outer));

        let PreparedOuter::Constructor {
            identity: union_identity,
            fields: mut union_fields,
        } = runtime
            .inspect_outer(&union, realm)
            .expect("Union inspects");
        assert_eq!(union_identity, fixture.union_id);
        assert_eq!(union_fields.len(), 2);
        let tag = take_scalar(&union_fields, 0);
        assert_eq!(tag, 0, "the only effect in '[Req] is index 0");
        let payload = take_managed(&mut union_fields, 1);
        assert!(runtime.release(union));

        let ask_result = runtime
            .run_entry_retained_in(
                program,
                fixture.ask_argument_top.binding.id,
                &[PreparedArgument::Managed(&payload)],
                true,
                realm,
            )
            .expect("askArgument (Ask (I# n) -> n) forces the Ask request's Int#");
        let mut ask_values = ask_result.values.into_iter();
        let Some(PreparedValueResult::Scalar(n)) = ask_values.next() else {
            panic!("askArgument must return one scalar Int#");
        };
        assert!(ask_values.next().is_none());
        assert!(runtime.release(payload));

        let resumed = runtime
            .run_entry_retained_in(
                program,
                fixture.resume_int_top.binding.id,
                &[PreparedArgument::Managed(&k), PreparedArgument::Scalar(n)],
                true,
                realm,
            )
            .expect("resumeInt (qApp k (I# n)) applies the retained continuation");
        assert!(runtime.release(k));
        let mut resumed_values = resumed.values.into_iter();
        let Some(PreparedValueResult::Managed(next_outer)) = resumed_values.next() else {
            panic!("resumeInt must return one managed `Eff` outer value");
        };
        assert!(resumed_values.next().is_none());
        outer = next_outer;
    }
}

/// B1: rungs 2-5 driven TOGETHER through `SingleSlot<PreparedRuntime,
/// PreparedHole>` -- the composite proof no single-rung test above attempts.
#[test]
fn session_registry_drives_prepared_runtime_through_bind_import_park_resume_cancel_and_retire() {
    // ---- setup: parse both S6 artifacts and locate their tops BEFORE
    // either program is moved into the runtime (mirrors S6's own ordering:
    // `top_named` needs `&PreparedProgram` before `PreparedRuntime::from_prepared`
    // consumes it).
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

    let runtime = PreparedRuntime::from_prepared(producer_prepared, MachineImports::default())
        .expect("producer links closed");

    // `SessionId` has no `fresh()` constructor (it is a bare `pub struct
    // SessionId(pub u64)`, per `tidepool-repr::session_ids`) -- every other
    // integration test in this crate that needs one just picks a literal.
    let session_id = SessionId(910_001);
    let slot: SingleSlot<PreparedRuntime, PreparedHole> = SingleSlot::new();
    slot.install(session_id, runtime)
        .unwrap_or_else(|_| panic!("install must succeed against a freshly constructed slot"));

    // ==== Turn 1 -- incarnation A: bind + import + cross-program call =====
    // Everything this turn leases (the two S6 imports) is scoped to
    // `realm_a`, the resource scope standing in for incarnation A's own
    // actor-shaped lifetime.
    let checkout = slot
        .checkout_run()
        .expect("the freshly installed session is Idle");
    let (mut runtime, receipt) = checkout.into_parts();

    let realm_a = runtime.open_realm();

    let producer_program = runtime.first_program().expect("producer installs");
    runtime
        .set_val_gen(Generation(11))
        .expect("start the generation the consumer is projected against");
    let bound_value = runtime
        .bind_top(
            producer_program,
            producer_value_top.binding.id,
            "producerValue",
        )
        .expect("producerValue binds at generation 11");
    let bound_fn = runtime
        .bind_top(producer_program, producer_fn_top.binding.id, "producerFn")
        .expect("producerFn binds at generation 11");

    let consumer_program = runtime
        .install_prepared_in(
            consumer_prepared,
            &[
                (value_identity.clone(), bound_value),
                (fn_identity.clone(), bound_fn),
            ],
            realm_a,
        )
        .expect("consumer links against both generation-11 bindings and installs under realm_a");
    assert_eq!(runtime.bindings().lease_count(bound_value), 1);
    assert_eq!(runtime.bindings().lease_count(bound_fn), 1);

    let expected = expected_consumer_value();
    let observed = runtime
        .run_entry_in(
            consumer_program,
            consumer_value_top.binding.id,
            &scalar_args,
            true,
            realm_a,
        )
        .expect("consumerValueAt reads producerValue through its import slot");
    assert_eq!(observed.values.len(), 1);
    assert_eq!(observed_int_list(&observed.values[0]), expected);

    slot.settle_suspended(receipt, runtime, Vec::new());
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
    let (mut runtime, receipt) = checkout.into_parts();

    let freer_program = runtime
        .install(
            FREER_RESUME_ARTIFACT,
            &requirements(),
            DecodeLimits::default(),
            &[],
        )
        .expect("freer-resume artifact installs as a second program, no imports");
    let first_a = runtime
        .run_entry_retained_in(
            freer_program,
            fixture.program_top.binding.id,
            &[],
            true,
            realm_a,
        )
        .expect("incarnation A's `program` run suspends on its first Ask");
    let Some(PreparedValueResult::Managed(outer_a)) = first_a.values.into_iter().next() else {
        panic!("`program` must return one managed `Eff` outer value");
    };
    let hole_a = runtime.hole_for(&outer_a, realm_a);

    slot.settle_suspended(receipt, runtime, vec![hole_a]);

    // ==== Turn 3 -- incarnation B: a second, independent realm ============
    // `checkout_run` (not `checkout_resume`) on purpose: incarnation B does
    // UNRELATED work, not resuming A's hole. The registry's own doc on
    // `checkout_run` says a fresh turn over parked frames is ordinary.
    let checkout = slot
        .checkout_run()
        .expect("checkout_run also admits a turn over a Suspended slot");
    let (mut runtime, receipt) = checkout.into_parts();

    let realm_b = runtime.open_realm();
    let first_b = runtime
        .run_entry_retained_in(
            freer_program,
            fixture.program_top.binding.id,
            &[],
            true,
            realm_b,
        )
        .expect("incarnation B's own unrelated `program` run suspends on its own first Ask");
    let Some(PreparedValueResult::Managed(outer_b)) = first_b.values.into_iter().next() else {
        panic!("`program` must return one managed `Eff` outer value");
    };
    let mut hole_b = runtime.hole_for(&outer_b, realm_b);

    // A's hole survived B's entirely unrelated turn.
    assert_eq!(runtime.parked_realm(&hole_a), Some(realm_a));

    slot.settle_suspended(receipt, runtime, vec![hole_a, hole_b]);

    // ==== Turn 4 -- resume A's hole out of order, to completion ===========
    let checkout = slot
        .checkout_resume(&hole_a)
        .expect("hole_a is a member of the suspended slot's parked holes");
    let (mut runtime, receipt) = checkout.into_parts();

    let value_a = drive_to_val_in(&mut runtime, freer_program, &fixture, realm_a, outer_a);
    assert_eq!(value_a, expected_program_value());

    slot.settle_suspended(receipt, runtime, vec![hole_b]);

    // ==== Turn 5 -- realm-scoped cancellation on B, then resume to
    //      completion =======================================================
    let checkout = slot
        .checkout_run()
        .expect("checkout_run also admits a turn over hole_b's suspended slot");
    let (mut runtime, receipt) = checkout.into_parts();

    // Split B's parked suspension into Union/continuation (mirrors
    // `two_realms_share_one_machine_cancel_reset_close_independently_of_each_other`).
    // `inspect_outer` never consumes `outer_b` itself (it mints fresh
    // handles for the constructor's fields), so `hole_b` -- minted against
    // `outer_b` -- stays valid across this split.
    let PreparedOuter::Constructor {
        identity: id_b,
        fields: mut fields_b,
    } = runtime
        .inspect_outer(&outer_b, realm_b)
        .expect("B's parked Eff value inspects cleanly under its own realm");
    assert_eq!(id_b, fixture.e_id);
    assert_eq!(fields_b.len(), 2);
    let union_b = take_managed(&mut fields_b, 0);
    let k_cont_b = take_managed(&mut fields_b, 1);
    assert!(runtime.release(outer_b));
    // `outer_b`'s own root is spent now; re-mint `hole_b` against the
    // continuation actually being resumed below, `k_cont_b`, so the
    // liveness check after the cancelled attempt names the thing that must
    // survive it.
    hole_b = runtime.hole_for(&k_cont_b, realm_b);

    let PreparedOuter::Constructor {
        identity: union_id_b,
        fields: mut union_fields_b,
    } = runtime
        .inspect_outer(&union_b, realm_b)
        .expect("B's Union inspects");
    assert_eq!(union_id_b, fixture.union_id);
    let payload_b = take_managed(&mut union_fields_b, 1);
    assert!(runtime.release(union_b));

    let ask_b = runtime
        .run_entry_retained_in(
            freer_program,
            fixture.ask_argument_top.binding.id,
            &[PreparedArgument::Managed(&payload_b)],
            true,
            realm_b,
        )
        .expect("askArgument forces B's Ask request's Int# before any cancellation is requested");
    let Some(PreparedValueResult::Scalar(n_b)) = ask_b.values.into_iter().next() else {
        panic!("askArgument must return one scalar Int#");
    };
    assert!(runtime.release(payload_b));

    // Cancel ONLY realm_b, after k_cont_b is already parked and its Ask
    // answer already forced -- the same "flag set after the parked state is
    // reached" discipline C1's own two-realm test documents.
    let cancel_b = runtime
        .cancel_handle(realm_b)
        .expect("the machine is already installed by this point");
    cancel_b.cancel();

    let cancelled = runtime.run_entry_retained_in(
        freer_program,
        fixture.resume_int_top.binding.id,
        &[
            PreparedArgument::Managed(&k_cont_b),
            PreparedArgument::Scalar(n_b),
        ],
        true,
        realm_b,
    );
    assert!(
        matches!(cancelled, Err(PreparedRuntimeError::Cancelled)),
        "a cancelled realm must refuse the call before it runs"
    );
    assert_eq!(
        runtime.parked_realm(&hole_b),
        Some(realm_b),
        "a cancelled resumeInt call must not consume k_cont_b's parked continuation"
    );

    cancel_b.reset();

    let resumed_b = runtime
        .run_entry_retained_in(
            freer_program,
            fixture.resume_int_top.binding.id,
            &[
                PreparedArgument::Managed(&k_cont_b),
                PreparedArgument::Scalar(n_b),
            ],
            true,
            realm_b,
        )
        .expect(
            "k_cont_b remains valid after a cancellation that committed nothing, and now proceeds",
        );
    assert!(runtime.release(k_cont_b));
    let Some(PreparedValueResult::Managed(next_outer_b)) = resumed_b.values.into_iter().next()
    else {
        panic!("resumeInt must return one managed `Eff` outer value");
    };

    let value_b = drive_to_val_in(&mut runtime, freer_program, &fixture, realm_b, next_outer_b);
    assert_eq!(
        value_b, value_a,
        "both incarnations run the same deterministic freer-resume computation"
    );

    // Nothing parked anymore: both incarnations' continuations are fully
    // driven to their settled `Val`.
    slot.settle_suspended(receipt, runtime, Vec::new());

    // ==== Turn 6 -- retirement: close each realm independently ============
    let checkout = slot
        .checkout_run()
        .expect("final checkout for realm retirement");
    let (mut runtime, receipt) = checkout.into_parts();

    let report_a = runtime.close_realm_report(realm_a);
    assert_eq!(
        report_a.leases_released, 2,
        "realm_a leased exactly the two S6 imports (producerValue, producerFn)"
    );
    assert_eq!(
        report_a.handles_released, 0,
        "realm_a's own parked continuation was already fully driven and released in turn 4"
    );
    assert_eq!(
        report_a.frames, 0,
        "the prepared engine never parks a continuation as a frame"
    );

    // Releasing realm_a's leases must not free the underlying producer
    // bindings themselves -- `bind_top` roots are session-level (tracked
    // under `RealmId::ROOT`), so they only stop being LEASED here.
    assert_eq!(runtime.bindings().lease_count(bound_value), 0);
    assert_eq!(runtime.bindings().lease_count(bound_fn), 0);

    let report_b = runtime.close_realm_report(realm_b);
    assert_eq!(
        report_b.leases_released, 0,
        "realm_b installed no program of its own -- it only ran realm_a's installed program's entries"
    );
    assert_eq!(
        report_b.handles_released, 0,
        "realm_b's own parked continuation was already fully driven and released in turn 5"
    );
    assert_eq!(report_b.frames, 0);

    // The two incarnations' retirements did not disturb each other: both
    // reports above are independent of order (realm_a closed first here,
    // but neither touches the other's state).
    runtime
        .release_binding(bound_value)
        .expect("bound_value is no longer leased once realm_a has closed");
    runtime
        .release_binding(bound_fn)
        .expect("bound_fn is no longer leased once realm_a has closed");

    assert_eq!(
        runtime.retained_handle_count(),
        0,
        "every value either incarnation produced, plus both session-level bindings, is released"
    );
    assert_eq!(
        runtime.disposition(),
        tidepool_runtime::prepared_execution::MachineDisposition::Reusable
    );

    // Tear the session down fully: nothing is left to resume.
    slot.settle_retire(receipt);
    assert_eq!(
        slot.current_id(),
        None,
        "settle_retire clears SingleSlot's current entry"
    );
}
