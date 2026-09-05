//! Independent ABI pins for Shoal control and fork configuration.
use tidepool_protocol::effects::{
    agent_control::agent_control, agent_inspection::agent_inspection,
};
use tidepool_protocol::hs::HsType;

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
fn fork_effort_is_optional_at_the_existing_launch_boundary() {
    let forks = tidepool_protocol::effects::forks::forks();
    let launch = forks
        .verbs
        .iter()
        .find(|verb| verb.ctor == "ForksStartWith")
        .unwrap();
    assert_eq!(launch.args.len(), 10);
    assert_eq!(launch.args[9].name, "effort");
    assert_eq!(
        launch.args[9].ty,
        HsType::maybe(HsType::Named("ForkEffort"))
    );
    // Preserve the entry closure's position: the actor capture owner claims
    // its live custody by this field, independently of configuration decoding.
    assert_eq!(launch.args[1].name, "entry");
}

#[test]
fn cleanup_execution_carries_exact_incarnations_and_activity_revisions() {
    let control = agent_control();
    let execute = control
        .verbs
        .iter()
        .find(|verb| verb.ctor == "AgentControlExecuteCleanupWith")
        .unwrap();
    assert_eq!(
        execute
            .args
            .iter()
            .map(|arg| arg.ty.clone())
            .collect::<Vec<_>>(),
        vec![
            HsType::Int,
            HsType::list(HsType::Tuple(vec![HsType::Int, HsType::Int, HsType::Int])),
        ]
    );
    assert_eq!(execute.ret, HsType::Named("CleanupReceipt"));
    assert!(!control
        .verbs
        .iter()
        .any(|verb| verb.ret == HsType::Named("CleanupPlan")));
    let inspection = agent_inspection();
    let plan = inspection
        .verbs
        .iter()
        .find(|verb| verb.ctor == "AgentInspectCleanupWith")
        .unwrap();
    assert_eq!(plan.ret, HsType::Named("CleanupPlan"));
}

#[test]
fn group_inspection_uses_an_identity_and_can_report_unavailability() {
    let inspection = agent_inspection();
    let group = inspection
        .verbs
        .iter()
        .find(|verb| verb.ctor == "AgentGroupListWith")
        .unwrap();
    assert_eq!(group.args.len(), 1);
    assert_eq!(group.args[0].ty, HsType::Int);
    assert_eq!(
        group.ret,
        HsType::maybe(HsType::list(HsType::Named("AgentRosterEntry")))
    );
}
