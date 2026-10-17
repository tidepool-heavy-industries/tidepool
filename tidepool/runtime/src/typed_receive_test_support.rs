//! Typed receive assertions over an already installed complete program closure.
use super::runtime;
use tidepool_codegen::prepared_program::ProgramId;
use tidepool_repr::execution_schema::{DefinitionsView, JsonLayout};

pub(super) fn json_layout(
    program: DefinitionsView<'_>,
    owner: &str,
) -> JsonLayout<tidepool_repr::DataConId> {
    program
        .json_layout()
        .unwrap_or_else(|| panic!("compiler-issued Value layout missing from {owner}"))
        .try_map(|id| {
            program
                .constructors()
                .get(id.0 as usize)
                .map(|constructor| constructor.host_id)
                .ok_or(())
        })
        .expect("declared canonical JSON constructors")
}

pub(super) fn assert_typed_receive_json(
    engine: &mut runtime::prepared_execution::PreparedEngine,
    program: ProgramId,
    table: tidepool_repr::DataConTable,
    expected_site: u64,
    payload: (
        &mut runtime::prepared_execution::PreparedEngine,
        ProgramId,
        JsonLayout<tidepool_repr::DataConId>,
    ),
    assert_schema: impl Fn(
        &runtime::prepared_execution::PreparedEngine,
        &runtime::prepared_execution::PreparedParked,
        &JsonLayout<tidepool_repr::DataConId>,
    ),
) {
    use runtime::prepared_execution::{
        ParkPolicy, PreparedRuntimeError, PreparedSettlement, RealmId,
    };
    use tidepool_effect::{EffectRunPolicy, LivePayloadPolicy};
    let (producer, payload_program, layout) = payload;
    let table = table.with_json_layout(Some(layout));
    let PreparedSettlement::Done { value: pair } = producer
        .run_settled(payload_program, RealmId::ROOT)
        .expect("execute Haskell typed payload producer")
    else {
        panic!("pure payload producer must complete")
    };
    producer
        .observe(payload_program, pair)
        .expect("force the genuine typed payload pair");
    let answers = producer
        .fields(pair, RealmId::ROOT, 2)
        .expect("two typed payloads");
    assert!(producer.release(pair));
    let mut parked = Vec::new();
    for _ in 0..2 {
        let PreparedSettlement::Suspended {
            request,
            continuation,
        } = engine
            .run_settled(program, RealmId::ROOT)
            .expect("run typed receive")
        else {
            panic!("typed receive must suspend")
        };
        let frame = engine
            .park_suspension(
                program,
                RealmId::ROOT,
                ParkPolicy {
                    principal: tidepool_repr::PrincipalId::SYSTEM,
                    effect_policy: EffectRunPolicy::SuspendAll,
                    live_payload: LivePayloadPolicy::None,
                },
                request,
                continuation,
                &table,
            )
            .expect("park with the compiler-issued carrier");
        let runtime::HaskellValue::Con(constructor, _) = &frame.request else {
            panic!("typed receive parks its actual constructor request")
        };
        let identity = &table
            .get(*constructor)
            .expect("admitted request constructor")
            .identity
            .symbol;
        assert_eq!(identity.module, "Tidepool.Effects.Core");
        assert_eq!(identity.occurrence, "ActorReceiveWith");
        assert_eq!(identity.namespace, "constructor");
        assert_eq!(engine.parked_site(frame.id), Some(expected_site));
        // Native handle delivery is trusted and does not prove Haskell type
        // equality. Check the receiver owner's schema before delivering it.
        assert_schema(engine, &frame, &layout);
        parked.push(frame);
    }
    assert_eq!(engine.parked_count(), 2);
    engine
        .quiesce_and_collect_now()
        .expect("collect while typed continuations remain parked");
    for (index, (parked, answer)) in parked.into_iter().rev().zip(answers).enumerate() {
        let payload = serde_json::json!({"index": index, "mixed": [true, false, null, 42, "text", {"nested": []}]});
        assert!(matches!(
            engine.resume_with_structural_answer(parked.id, &payload, &table),
            Err(PreparedRuntimeError::AnswerDelivery {
                delivery: tidepool_repr::execution_schema::SiteDelivery::LiveReentry,
                ..
            })
        ));
        assert_eq!(engine.parked_count(), 2 - index);
        let produced = producer
            .observe(payload_program, answer)
            .expect("observe typed Haskell payload");
        assert_eq!(runtime::value_to_json(&produced, &table, 0), payload);
        let parcel = producer
            .export_parcel(answer.raw())
            .expect("export genuine typed value");
        let (arrived, imports) = engine
            .import_parcel(parcel, RealmId::ROOT)
            .expect("import value and its native owner into the receiver realm");
        assert!(producer.release(answer));
        engine
            .quiesce_and_collect_now()
            .expect("collect imported live value and parked continuations");
        let resumed = engine
            .resume_with_handle(parked.id, arrived)
            .expect("resume with the receiver-owned live typed value");
        assert!(engine.discard_handle(arrived));
        engine.release_all(imports.into_iter().map(|(_, handle)| handle));
        let PreparedSettlement::Done { value } = resumed.settlement else {
            panic!("typed reply must complete")
        };
        let observed = engine.observe(program, value).expect("observe typed reply");
        let decoded = runtime::value_to_json(&observed, &table, 0);
        assert_eq!(decoded, payload);
        assert!(engine.release(value));
        assert_eq!(engine.parked_count(), 1 - index);
    }
}
