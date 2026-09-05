//! Independent ABI pins for inspected cleanup and exact group inspection.
use tidepool_protocol::effects::{
    agent_control::agent_control, agent_inspection::agent_inspection,
};
use tidepool_protocol::hs::HsType;

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
