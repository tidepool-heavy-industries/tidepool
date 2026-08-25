//! `Effect::validate`'s enforcement of [`Polymorphism::ArgBound`]/
//! [`Polymorphism::ResultBound`]'s own tyvar-placement invariants.
//!
//! Neither variant has a PRODUCTION effect whose validation failure this
//! test could observe by mutating a real, wrongly-shaped schema: `Finalize`
//! (`ArgBound`) is correct as declared, and no migrated effect uses
//! `ResultBound` at all — see [`run_llm_turn`]/[`fork`]'s own module docs on
//! why their `@T` polymorphism lives in a `HelperBody::OpaqueSited` helper's
//! signature, never in the GADT/row `ResultBound` describes (`Fork`/
//! `RunLLMTurn`'s constructors return concrete `Value`, not a bare tyvar).
//! So this test provokes both variants' enforcement directly, mutating a
//! `Polymorphism` field onto a real effect's otherwise-valid shape — the
//! smallest way to prove the checks in `Effect::validate` actually fire
//! rather than sitting as declared-but-never-exercised code.

use tidepool_protocol::effects::finalize::finalize;
use tidepool_protocol::schema::Polymorphism;

/// `ArgBound`'s positive case: `Finalize`'s real, declared shape (`v` a real
/// GADT parameter, appearing as `FinalizeWith`'s `value` argument type).
#[test]
fn arg_bound_accepts_finalizes_real_shape() {
    assert!(finalize().validate().is_ok());
}

/// `ArgBound`'s two enforced invariants, each provoked independently by
/// mutating `Finalize`'s otherwise-valid schema.
#[test]
fn arg_bound_rejects_a_tyvar_not_in_type_params() {
    let mut eff = finalize();
    // `"a"` is FinalizeWith's other, unconstrained tyvar (its `ret`) — never
    // an applied GADT parameter, so this must fail the "must appear in
    // type_params" check.
    eff.polymorphism = Polymorphism::ArgBound { tyvar: "a" };
    let errs = eff
        .validate()
        .expect_err("a tyvar absent from type_params must be rejected");
    assert!(
        errs.iter()
            .any(|e| e.contains("must appear in type_params")),
        "got: {errs:?}"
    );
}

#[test]
fn arg_bound_rejects_a_tyvar_absent_from_every_verb_argument() {
    let mut eff = finalize();
    eff.type_params = &["v", "ghost"];
    eff.default_row_args = &["Void", "Void"];
    // "ghost" now satisfies the type_params check but appears in no verb's
    // argument list, so it must fail the "argument-bound, not result-bound"
    // check instead.
    eff.polymorphism = Polymorphism::ArgBound { tyvar: "ghost" };
    let errs = eff
        .validate()
        .expect_err("a tyvar absent from every verb argument must be rejected");
    assert!(
        errs.iter()
            .any(|e| e.contains("must appear as some verb's argument type")),
        "got: {errs:?}"
    );
}

/// `ResultBound`'s positive case: `Finalize`'s own `a` (its `ret` tyvar,
/// never an applied GADT parameter and never a verb argument) satisfies
/// EVERY `ResultBound` invariant, even though `Finalize` itself is declared
/// `ArgBound { tyvar: "v" }` in production — this only proves the checks
/// accept a genuinely valid `ResultBound` shape when asked to.
#[test]
fn result_bound_accepts_a_phantom_result_tyvar() {
    let mut eff = finalize();
    eff.polymorphism = Polymorphism::ResultBound { tyvar: "a" };
    assert!(eff.validate().is_ok());
}

/// `ResultBound`'s three enforced invariants.
#[test]
fn result_bound_rejects_a_tyvar_that_is_an_applied_type_param() {
    let mut eff = finalize();
    // `"v"` IS an applied GADT parameter (`type_params`), which `ResultBound`
    // forbids — it describes a PHANTOM tyvar.
    eff.polymorphism = Polymorphism::ResultBound { tyvar: "v" };
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
    let mut eff = finalize();
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
    let mut eff = finalize();
    // `"v"` is both an applied type param AND a verb argument type
    // (`FinalizeWith`'s `value`) — retarget `type_params` off it so only the
    // argument-position violation is exercised.
    eff.type_params = &[];
    eff.default_row_args = &[];
    eff.polymorphism = Polymorphism::ResultBound { tyvar: "v" };
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
