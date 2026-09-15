//! Rung 5 of the acceptance ladder ("retiring an incarnation releases its
//! parked frame; a different incarnation cannot resume it"), proven through
//! `tidepool-actor`'s own types against the prepared-STG engine.
//!
//! `ActorRunTarget::retire_placement` (added alongside
//! `install_actor_execution` in `tidepool-actor/src/mount.rs`) is the only
//! way the actor crate retires a placement without reaching for an
//! engine-specific inherent method. This test hosts a real `PreparedRuntime`
//! in a `SessionRegistry` (`SingleSlot`, exactly as
//! `tidepool-runtime/tests/prepared_resident_composite.rs` does -- read that
//! file first, it is the model this one follows for the checkout/settle
//! protocol and the freer-resume parkable-continuation fixture) and drives
//! two actor incarnations' placements through it: retiring incarnation 1's
//! placement must release exactly what incarnation 1 held, and must not
//! disturb incarnation 2's still-parked continuation, which then resumes and
//! runs to completion.
//!
//! Rust integration test files are separate crates, so small helpers
//! (`requirements`, `tops`, `top_named`, `take_managed`, `take_scalar`, the
//! freer-resume fixture and resume loop) are copied from the composite test
//! rather than shared.
//!
//! The request-layer half of rung 5 ("a stale incarnation is refused") is
//! NOT exercised here: `tidepool-actor::request::RequestRegistry` and its
//! `reserve`/`mark_queued`/`present`/`begin_reply` methods (the types
//! `stale_incarnation_cannot_settle_a_request` in `request.rs` around line
//! 2348 uses) are all `pub(crate)`, unreachable from a `tests/` integration
//! crate, and nothing else in `tidepool-actor`'s public surface constructs a
//! request against a target actor. Exercising that half would need either a
//! `pub(crate)` visibility widening (out of this task's scope, and not one
//! of the owned files) or driving the whole `ResidentActorWorkbench`/kernel
//! actor-mailbox path, which is far more than the "small helpers" a
//! standalone integration test is meant to copy. This test instead asserts
//! the machine-layer half in full: exactly incarnation 1's realm/scope
//! receipt, incarnation 2's hole untouched, and incarnation 2 resumable to
//! completion.

use tidepool_actor::{ActorId, ActorPlacement, ActorRef, ActorRunTarget, Incarnation};
use tidepool_codegen::scope::ScopeId;
use tidepool_repr::execution_schema::{
    parse_program, Architecture, DecodeLimits, Endianness, Group, MachineImports, PreparedProgram,
    ProgramRequirements, SymbolIdentity, TargetDescriptor, TopBinding, EXECUTION_ABI_VERSION,
    SCHEMA_VERSION,
};
use tidepool_repr::{DataConId, Generation, SessionId};
use tidepool_runtime::prepared_execution::{
    PreparedArgument, PreparedHole, PreparedOuter, PreparedRuntime, PreparedValue,
    PreparedValueResult,
};
use tidepool_runtime::session::registry::SingleSlot;

const IMPORT_PRODUCER_ARTIFACT: &[u8] =
    include_bytes!("../../haskell/test-prepared-stg/fixtures/import-producer.cbor");
const IMPORT_CONSUMER_ARTIFACT: &[u8] =
    include_bytes!("../../haskell/test-prepared-stg/fixtures/import-consumer.cbor");
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

fn producer_identity(occurrence: &str) -> SymbolIdentity {
    SymbolIdentity {
        unit: "main".to_owned(),
        module: "ImportProducer".to_owned(),
        namespace: "value".to_owned(),
        occurrence: occurrence.to_owned(),
        record_parent: None,
    }
}

fn freer_resume_top(prepared: &PreparedProgram, occurrence: &str) -> TopBinding {
    tops(prepared)
        .into_iter()
        .find(|top| top.identity.module == "FreerResume" && top.identity.occurrence == occurrence)
        .unwrap_or_else(|| panic!("FreerResume artifact has no top named {occurrence}"))
}

fn freer_resume_constructor_identity(prepared: &PreparedProgram, occurrence: &str) -> DataConId {
    prepared
        .constructors()
        .iter()
        .find(|constructor| constructor.identity.occurrence == occurrence)
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
            e_id: freer_resume_constructor_identity(&prepared, "E"),
            val_id: freer_resume_constructor_identity(&prepared, "Val"),
            union_id: freer_resume_constructor_identity(&prepared, "Union"),
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

/// `prepared_resident_composite.rs`'s own `drive_to_val_in`, copied
/// verbatim (integration test crates cannot share code across files).
fn drive_to_val_in(
    runtime: &mut PreparedRuntime,
    program: tidepool_codegen::prepared_program::ProgramId,
    fixture: &FreerResumeFixture,
    realm: tidepool_codegen::suspension::RealmId,
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

/// Rung 5: retiring incarnation 1's placement through `ActorRunTarget`
/// releases exactly incarnation 1's realm/scope receipt, leaves incarnation
/// 2's parked hole live, and incarnation 2 still resumes to completion.
#[test]
fn retiring_one_incarnations_placement_does_not_disturb_a_sibling_incarnation() {
    // ---- setup: parse S6's producer/consumer pair (source of at least one
    // lease under incarnation 1's realm) and the freer-resume fixture
    // (source of at least one parked handle per incarnation), before either
    // program moves into the runtime.
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

    let fixture = FreerResumeFixture::load();

    let runtime = PreparedRuntime::from_prepared(producer_prepared, MachineImports::default())
        .expect("producer links closed");

    let session_id = SessionId(920_001);
    let slot: SingleSlot<PreparedRuntime, PreparedHole> = SingleSlot::new();
    slot.install(session_id, runtime)
        .unwrap_or_else(|_| panic!("install must succeed against a freshly constructed slot"));

    // Two incarnations of the SAME actor lineage, each with its own
    // resource-scope realm -- `ActorPlacement`/`ActorRef`/`Incarnation` are
    // `tidepool-actor`'s own identity types, not stand-ins.
    let actor_id = ActorId(77);
    let incarnation_1 = ActorRef {
        id: actor_id,
        incarnation: Incarnation::FIRST,
    };
    let incarnation_2 = ActorRef {
        id: actor_id,
        incarnation: Incarnation(2),
    };
    assert_ne!(
        incarnation_1, incarnation_2,
        "a retirement must be able to distinguish these two incarnations"
    );

    // ==== Turn 1 -- both incarnations set up their placements =============
    let checkout = slot
        .checkout_run()
        .expect("the freshly installed session is Idle");
    let (mut runtime, receipt) = checkout.into_parts();

    let realm_1 = runtime.open_realm();
    let scope_1 = ScopeId(101);
    let placement_1 = ActorPlacement {
        session: session_id,
        resource_scope: realm_1,
        lexical_scope: scope_1,
    };

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

    // Installing the consumer under realm_1 is incarnation 1's "at least one
    // lease": both imports are leased for the consumer program's lifetime,
    // released together only when realm_1 closes.
    runtime
        .install_prepared_in(
            consumer_prepared,
            &[
                (value_identity.clone(), bound_value),
                (fn_identity.clone(), bound_fn),
            ],
            realm_1,
        )
        .expect("consumer links against both generation-11 bindings and installs under realm_1");
    assert_eq!(runtime.bindings().lease_count(bound_value), 1);
    assert_eq!(runtime.bindings().lease_count(bound_fn), 1);

    // A second, independent program (freer-resume, no imports), shared by
    // both incarnations, run to first suspension under realm_1: incarnation
    // 1's "at least one parked handle".
    let freer_program = runtime
        .install(
            FREER_RESUME_ARTIFACT,
            &requirements(),
            DecodeLimits::default(),
            &[],
        )
        .expect("freer-resume artifact installs as a second program, no imports");
    let first_1 = runtime
        .run_entry_retained_in(
            freer_program,
            fixture.program_top.binding.id,
            &[],
            true,
            realm_1,
        )
        .expect("incarnation 1's `program` run suspends on its first Ask");
    let Some(PreparedValueResult::Managed(outer_1)) = first_1.values.into_iter().next() else {
        panic!("`program` must return one managed `Eff` outer value");
    };
    let hole_1 = runtime.hole_for(&outer_1, realm_1);

    // Incarnation 2: its own realm, its own parked handle over the SAME
    // shared freer-resume program -- entirely unrelated work, standing in
    // for a sibling incarnation that must survive incarnation 1's later
    // retirement.
    let realm_2 = runtime.open_realm();
    let scope_2 = ScopeId(202);
    let placement_2 = ActorPlacement {
        session: session_id,
        resource_scope: realm_2,
        lexical_scope: scope_2,
    };
    let first_2 = runtime
        .run_entry_retained_in(
            freer_program,
            fixture.program_top.binding.id,
            &[],
            true,
            realm_2,
        )
        .expect("incarnation 2's own unrelated `program` run suspends on its own first Ask");
    let Some(PreparedValueResult::Managed(outer_2)) = first_2.values.into_iter().next() else {
        panic!("`program` must return one managed `Eff` outer value");
    };
    let hole_2 = runtime.hole_for(&outer_2, realm_2);

    assert_eq!(runtime.parked_realm(&hole_1), Some(realm_1));
    assert_eq!(runtime.parked_realm(&hole_2), Some(realm_2));

    // `ActorSessionContext::run_context` is the seam an actor's compiled
    // turns actually run under; confirm each placement addresses its own
    // realm/scope pair distinctly before either is retired.
    assert_ne!(
        placement_1.resource_scope, placement_2.resource_scope,
        "each incarnation owns its own resource realm"
    );

    slot.settle_suspended(receipt, runtime, vec![hole_1, hole_2]);

    // ==== Turn 2 -- retire incarnation 1's placement through
    //      `ActorRunTarget`, the seam `resident_workbench.rs`'s
    //      `retire_root_placement` now goes through for either engine =======
    let checkout = slot
        .checkout_run()
        .expect("turn 2 checks out the idle session");
    let (mut runtime, receipt) = checkout.into_parts();

    let retirement =
        runtime.retire_placement(placement_1.resource_scope, placement_1.lexical_scope);
    assert_eq!(
        retirement.frames, 0,
        "the prepared engine never parks a continuation as a frame"
    );
    assert_eq!(
        retirement.handles, 1,
        "realm_1 owned exactly outer_1's still-parked handle"
    );
    assert_eq!(
        retirement.leases, 2,
        "realm_1 leased exactly the two S6 imports (producerValue, producerFn)"
    );
    assert_eq!(
        retirement.scope_roots, 0,
        "the prepared engine has no lexical-scope frames of its own"
    );

    // Incarnation 1's parked frame is gone...
    assert_eq!(
        runtime.parked_realm(&hole_1),
        None,
        "retiring incarnation 1's placement released its parked continuation"
    );
    // ...but a different incarnation (2) cannot resume it: its own hole is
    // untouched, exactly rung 5's statement.
    assert_eq!(
        runtime.parked_realm(&hole_2),
        Some(realm_2),
        "incarnation 2's placement is untouched by incarnation 1's retirement"
    );

    // The leases released with realm_1 did not free the underlying producer
    // bindings themselves -- `bind_top` roots are session-level.
    assert_eq!(runtime.bindings().lease_count(bound_value), 0);
    assert_eq!(runtime.bindings().lease_count(bound_fn), 0);

    // Incarnation 2 is still fully live: drive its own parked continuation
    // to completion, exactly as if nothing had happened to incarnation 1.
    let value_2 = drive_to_val_in(&mut runtime, freer_program, &fixture, realm_2, outer_2);
    assert_eq!(
        value_2,
        expected_program_value(),
        "incarnation 2's computation completes unaffected by incarnation 1's retirement"
    );

    slot.settle_suspended(receipt, runtime, Vec::new());

    // ==== Turn 3 -- retire incarnation 2 as well and tear the session down
    let checkout = slot
        .checkout_run()
        .expect("final checkout for incarnation 2's own retirement");
    let (mut runtime, receipt) = checkout.into_parts();

    let retirement_2 =
        runtime.retire_placement(placement_2.resource_scope, placement_2.lexical_scope);
    assert_eq!(retirement_2.frames, 0);
    assert_eq!(
        retirement_2.handles, 0,
        "incarnation 2's own parked continuation was already fully driven and released above"
    );
    assert_eq!(retirement_2.leases, 0);
    assert_eq!(retirement_2.scope_roots, 0);

    runtime
        .release_binding(bound_value)
        .expect("bound_value is no longer leased once realm_1 has closed");
    runtime
        .release_binding(bound_fn)
        .expect("bound_fn is no longer leased once realm_1 has closed");

    assert_eq!(
        runtime.retained_handle_count(),
        0,
        "every value either incarnation produced, plus both session-level bindings, is released"
    );

    slot.settle_retire(receipt);
    assert_eq!(
        slot.current_id(),
        None,
        "settle_retire clears SingleSlot's current entry"
    );
}
