//! Independent ABI pins for Exomonad control and agent admission.
use tidepool_protocol::effects::{
    agent_control::agent_control, agent_inspection::agent_inspection,
    agent_launch::agent_launch,
};
use tidepool_protocol::{hs::HsType, schema::RustBinding};

#[test]
fn usage_observation_has_one_shared_record_and_distinct_history_endpoints() {
    use tidepool_protocol::schema::TypeShape;
    let context = tidepool_protocol::effects::actor_context::actor_context();
    let observation = context
        .type_defs
        .iter()
        .find(|ty| ty.name == "ProviderUsageObservation")
        .unwrap();
    let TypeShape::Record { fields } = &observation.shape else {
        panic!("usage record")
    };
    assert_eq!(
        fields.iter().map(|field| field.hs_name).collect::<Vec<_>>(),
        [
            "usageObservationId",
            "usageTimestamp",
            "usageCachedInputTokens",
            "usageUncachedInputTokens"
        ]
    );
    for (effect, name, expected) in [
        (
            context,
            "ActorContextInfo",
            ["contextFirstUsage", "contextLatestUsage"],
        ),
        (
            agent_inspection(),
            "AgentRosterEntry",
            ["rosterFirstUsage", "rosterLatestUsage"],
        ),
    ] {
        let record = effect.type_defs.iter().find(|ty| ty.name == name).unwrap();
        let TypeShape::Record { fields } = &record.shape else {
            panic!("inspection record")
        };
        let usage_fields = fields
            .iter()
            .filter(|field| field.ty == HsType::maybe(HsType::Named("ProviderUsageObservation")))
            .map(|field| field.hs_name)
            .collect::<Vec<_>>();
        assert_eq!(usage_fields, expected);
    }
}

#[test]
fn usage_summaries_have_provider_scope_completeness_and_shared_inspection_fields() {
    use tidepool_protocol::schema::TypeShape;
    let context = tidepool_protocol::effects::actor_context::actor_context();
    let summary = context
        .type_defs
        .iter()
        .find(|ty| ty.name == "ProviderUsageSummary")
        .unwrap();
    let TypeShape::Record { fields } = &summary.shape else {
        panic!("summary record")
    };
    assert_eq!(
        fields.iter().map(|field| field.hs_name).collect::<Vec<_>>(),
        [
            "usageSummaryScope",
            "usageSummaryCompleteness",
            "usageSummaryObservations",
            "usageSummaryCachedInputTokens",
            "usageSummaryUncachedInputTokens",
            "usageSummaryOutputTokens",
            "usageSummaryReasoningTokens",
            "usageSummaryTotalTokens",
        ]
    );
    for (effect, name, expected) in [
        (
            context,
            "ActorContextInfo",
            ["contextUsageSummary", "contextLatestTurnUsage"],
        ),
        (
            agent_inspection(),
            "AgentRosterEntry",
            ["rosterUsageSummary", "rosterLatestTurnUsage"],
        ),
    ] {
        let record = effect.type_defs.iter().find(|ty| ty.name == name).unwrap();
        let TypeShape::Record { fields } = &record.shape else {
            panic!("inspection record")
        };
        assert_eq!(
            fields
                .iter()
                .filter(|field| field.ty == HsType::maybe(HsType::Named("ProviderUsageSummary")))
                .map(|field| field.hs_name)
                .collect::<Vec<_>>(),
            expected
        );
    }
}

#[test]
fn spawn_uses_typed_context_workspace_and_live_spec_with_optional_configuration() {
    let effect = agent_launch();
    assert!(effect.validate().is_ok());
    let spawn = effect
        .verbs
        .iter()
        .find(|verb| verb.ctor == "AgentLaunchSpawnWith")
        .unwrap();
    let argument = |name| spawn.args.iter().find(|arg| arg.name == name).unwrap();
    assert_eq!(
        spawn.args.iter().map(|arg| arg.name).collect::<Vec<_>>(),
        [
            "context",
            "install",
            "workspace",
            "effects",
            "label",
            "model",
            "effort",
            "instructions",
            "lifetime",
            "limits",
        ]
    );
    assert_eq!(argument("context").ty, HsType::Named("SpawnContextWire"));
    assert_eq!(argument("workspace").ty, HsType::Named("SpawnWorkspaceWire"));
    assert_eq!(
        argument("effects").ty,
        HsType::list(HsType::Named("ActorEffectKey"))
    );
    assert_eq!(
        argument("install").ty,
        HsType::func(
            HsType::Int,
            HsType::app(
                HsType::app(HsType::Named("Eff"), HsType::Var("childEffs")),
                HsType::Unit,
            ),
        )
    );
    assert_eq!(argument("install").rust, RustBinding::HaskellValue);
    assert_eq!(
        argument("label").ty,
        HsType::maybe(HsType::Text)
    );
    assert_eq!(
        argument("model").ty,
        HsType::maybe(HsType::Named("Model"))
    );
    assert_eq!(
        argument("effort").ty,
        HsType::maybe(HsType::Named("ForkEffort"))
    );
    assert_eq!(
        argument("instructions").ty,
        HsType::maybe(HsType::Text)
    );
    assert_eq!(
        argument("limits").ty,
        HsType::maybe(HsType::Tuple(vec![HsType::Int, HsType::Int]))
    );
    assert_eq!(argument("lifetime").ty, HsType::Named("WorkerLifetime"));
    assert_eq!(
        spawn.ret,
        HsType::either(
            HsType::Named("SpawnErrorWire"),
            HsType::Tuple(vec![
                HsType::Int,
                HsType::Int,
                HsType::maybe(HsType::Named("WorktreeHandle")),
            ]),
        )
    );
    let errors = effect
        .type_defs
        .iter()
        .find(|ty| ty.name == "SpawnErrorWire")
        .unwrap();
    let tidepool_protocol::schema::TypeShape::Sum { variants } = &errors.shape else {
        panic!("spawn error is a typed sum")
    };
    assert_eq!(
        variants.iter().map(|variant| variant.ctor).collect::<Vec<_>>(),
        ["SpawnRefused", "SpawnPartialFailure"]
    );
    let tidepool_protocol::schema::VariantFields::Positional(refused) = &variants[0].fields else {
        panic!("spawn refusal carries a reason")
    };
    assert_eq!(refused, &[HsType::Text]);
    let tidepool_protocol::schema::VariantFields::Positional(partial) = &variants[1].fields else {
        panic!("partial spawn failure carries retained resources and cleanup")
    };
    assert_eq!(
        partial,
        &[
            HsType::Named("SpawnRetainedResourcesWire"),
            HsType::Named("SpawnCleanup"),
            HsType::Text,
        ]
    );
}

#[test]
fn agent_control_uses_exact_incarnation_and_typed_retention_and_stop_results() {
    let control = agent_control();
    assert!(control.validate().is_ok());
    let retain = control
        .verbs
        .iter()
        .find(|verb| verb.ctor == "AgentControlRetainWith")
        .unwrap();
    assert_eq!(
        retain
            .args
            .iter()
            .map(|arg| arg.ty.clone())
            .collect::<Vec<_>>(),
        vec![
            HsType::Tuple(vec![HsType::Int, HsType::Int]),
            HsType::Named("WorkerLifetime"),
        ]
    );
    assert_eq!(
        retain.ret,
        HsType::either(HsType::Named("AgentRetentionError"), HsType::Unit)
    );
    let retention_error = control
        .type_defs
        .iter()
        .find(|ty| ty.name == "AgentRetentionError")
        .unwrap();
    let tidepool_protocol::schema::TypeShape::Sum { variants } = &retention_error.shape else {
        panic!("retention refusal is a typed sum")
    };
    assert_eq!(
        variants.iter().map(|variant| variant.ctor).collect::<Vec<_>>(),
        [
            "AgentRetainUnavailable",
            "AgentRetainUnauthorized",
            "AgentRetainOwnerUnavailable",
            "AgentRetainOwnerClosed",
        ]
    );
    let stop = control
        .verbs
        .iter()
        .find(|verb| verb.ctor == "AgentControlStopWith")
        .unwrap();
    assert_eq!(
        stop.args[0].ty,
        HsType::Tuple(vec![HsType::Int, HsType::Int])
    );
    assert_eq!(stop.ret, HsType::Named("AgentStopControlOutcome"));
    assert!(!control.verbs.iter().any(|verb| {
        verb.ctor.contains("Cleanup") || verb.ret == HsType::Named("CleanupPlan")
    }));
}

#[test]
fn agent_inspection_uses_an_exact_actor_incarnation() {
    let inspection = agent_inspection();
    let inspect = inspection
        .verbs
        .iter()
        .find(|verb| verb.ctor == "AgentInspectWith")
        .unwrap();
    assert_eq!(inspect.args.len(), 1);
    assert_eq!(
        inspect.args[0].ty,
        HsType::Tuple(vec![HsType::Int, HsType::Int])
    );
    assert_eq!(
        inspect.ret,
        HsType::maybe(HsType::Named("AgentRosterEntry"))
    );
}

#[test]
fn notifications_have_one_way_admission_and_owner_receipt_observation() {
    let effect = tidepool_protocol::effects::notifications::notifications();
    assert!(effect.validate().is_ok());
    assert_eq!(
        effect
            .verbs
            .iter()
            .map(|verb| verb.ctor)
            .collect::<Vec<_>>(),
        ["NotifyWith", "PollNotificationWith"]
    );
    let address = HsType::Tuple(vec![HsType::Int, HsType::Int]);
    let receipt = HsType::Tuple(vec![
        address.clone(),
        HsType::Tuple(vec![
            address.clone(),
            HsType::Tuple(vec![HsType::Text, HsType::Int]),
        ]),
    ]);
    assert_eq!(effect.verbs[0].args[0].ty, address);
    assert_eq!(
        effect.verbs[0].ret,
        HsType::either(HsType::Named("NotificationError"), receipt.clone())
    );
    assert_eq!(effect.verbs[1].args[0].ty, receipt);
    assert_eq!(
        effect.verbs[1].ret,
        HsType::either(
            HsType::Named("NotificationError"),
            HsType::Named("NotificationState")
        )
    );
    assert!(!format!("{effect:?}").contains("Reply "));
}

#[test]
fn command_observation_preserves_independent_output_failure_in_both_wire_languages() {
    let commands = tidepool_protocol::effects::commands::commands();
    let wire = tidepool_protocol::gen::wire_rs::file(&commands).contents;
    assert!(wire.contains("pub result: CommandResult,"));
    assert!(wire.contains("pub output: Result<CommandOutput, CommandError>,"));
    assert!(!wire.contains("pub output: Result<CommandError, CommandOutput>,"));
    let haskell = tidepool_protocol::gen::decl_rs::file(&commands).contents;
    assert!(haskell.contains("observedCommandResult :: CommandResult"));
    assert!(haskell.contains("observedCommandOutput :: Either CommandError CommandOutput"));
}
