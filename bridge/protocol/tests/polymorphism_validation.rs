//! Result-bound validation against the active typed agent session protocol.
use tidepool_protocol::effects::agent_session::agent_session;
use tidepool_protocol::schema::{Polymorphism, TypeParam};

#[test]
fn result_bound_accepts_a_phantom_result_tyvar() {
    let mut eff = agent_session();
    eff.polymorphism = Polymorphism::ResultBound { tyvar: "output" };
    assert!(eff.validate().is_ok());
}

/// `ResultBound`'s three enforced invariants.
#[test]
fn result_bound_rejects_a_tyvar_that_is_an_applied_type_param() {
    let mut eff = agent_session();
    eff.type_params = &[TypeParam::value("input")];
    eff.default_row_args = &["Void"];
    eff.polymorphism = Polymorphism::ResultBound { tyvar: "input" };
    let errs = eff
        .validate()
        .expect_err("a type_params member must be rejected as ResultBound");
    assert!(
        errs.iter()
            .any(|e| e.contains("must NOT appear in type_params")),
        "got: {errs:?}"
    );
}

#[test]
fn result_bound_rejects_a_tyvar_absent_from_every_verb_ret() {
    let mut eff = agent_session();
    eff.polymorphism = Polymorphism::ResultBound { tyvar: "nowhere" };
    let errs = eff
        .validate()
        .expect_err("a tyvar naming no verb's ret must be rejected");
    assert!(
        errs.iter()
            .any(|e| e.contains("must appear as some verb's `ret`")),
        "got: {errs:?}"
    );
}

#[test]
fn result_bound_rejects_a_tyvar_that_is_also_a_verb_argument() {
    let mut eff = agent_session();
    eff.type_params = &[];
    eff.default_row_args = &[];
    eff.polymorphism = Polymorphism::ResultBound { tyvar: "input" };
    let errs = eff
        .validate()
        .expect_err("a tyvar used as a verb argument must be rejected as ResultBound");
    assert!(
        errs.iter().any(|e| e.contains(
            "must not appear as any verb's argument type (that would make it argument-bound"
        )),
        "got: {errs:?}"
    );
}
