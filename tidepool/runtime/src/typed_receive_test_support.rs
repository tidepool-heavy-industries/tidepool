//! Typed receive assertions over an already installed complete program closure.
use super::runtime;
use tidepool_codegen::prepared_program::ProgramId;
use tidepool_repr::execution_schema::{parse_program, DecodeLimits, DefinitionsView};

pub(super) fn assert_typed_receive_json(
    engine: &mut runtime::prepared_execution::PreparedEngine,
    program: ProgramId,
    table: tidepool_repr::DataConTable,
    receive: DefinitionsView<'_>,
) {
    use runtime::prepared_execution::{
        ParkPolicy, PreparedEngine, PreparedRuntimeError, PreparedSettlement, RealmId,
    };
    use tidepool_effect::{EffectRunPolicy, LivePayloadPolicy};
    use tidepool_repr::execution_schema::{ConstructorId, ConstructorReply};

    let receiver = receive
        .constructors()
        .iter()
        .position(|constructor| {
            constructor.identity.symbol.module == "Tidepool.Effects.Core"
                && constructor.identity.symbol.occurrence == "ActorReceiveWith"
        })
        .expect("compiler retained the genuine receive constructor");
    assert!(receive
        .constructor_replies()
        .contains(&(ConstructorId(receiver as u32), ConstructorReply::AtSite,)));
    assert!(receive.json_layout().is_some());
    assert!(receive.sites().iter().any(|site| {
        site.delivery == tidepool_repr::execution_schema::SiteDelivery::LiveReentry
    }));
    let payload_artifact = tidepool_test_data::prepared_resources::read_target(
        "TIDEPOOL_TYPED_RECEIVE_FIXTURE_DIR",
        "__payload",
    );
    let payload_prepared = parse_program(
        &payload_artifact,
        &tidepool_toolchain::prepared_artifact::production_requirements()
            .expect("production compiler requirements"),
        DecodeLimits::default(),
    )
    .expect("genuine compiler-produced typed payloads");
    let json_layout = |program: DefinitionsView<'_>| {
        program
            .json_layout()
            .expect("compiler-issued Value layout")
            .try_map(|id| {
                program
                    .constructors()
                    .get(id.0 as usize)
                    .map(|constructor| constructor.host_id)
                    .ok_or(())
            })
            .expect("declared canonical JSON constructors")
    };
    let layout = json_layout(receive);
    assert_eq!(layout, json_layout(payload_prepared.definitions()));
    let table = table.with_json_layout(Some(layout));
    let (mut producer, payload_program) =
        PreparedEngine::bootstrap_with_nursery_bytes(payload_prepared, 4096)
            .expect("bootstrap Haskell payload producer");
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
        parked.push(
            engine
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
                .expect("park with the compiler-issued carrier"),
        );
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
