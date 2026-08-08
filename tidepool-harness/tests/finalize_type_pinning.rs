//! `finalize` is monomorphic to the hole's answer type — the guarantee
//! "GHC validates the answer against `T`" made TRUE for `finalize`.
//!
//! `Tidepool.Effects`' `finalize :: forall v a effs. Member Finalize effs => v
//! -> Eff effs a` leaves `v` unconstrained, so an answerer servicing a
//! `runLLMTurn @T` hole could `finalize` a value of ANY type. It compiled; the
//! wrong-typed value then crossed in-heap into a `T`-typed continuation and
//! trapped on a constructor tag, past every check the pipeline has.
//!
//! Observed live, three consecutive dogfood runs against the wizard harness
//! (`runLLMTurn @Contribution`): the answerer tried `finalize @Contribution`,
//! got `Not in scope: type constructor or class 'Contribution'`, and then —
//! because anything typechecks — settled for `finalize @Text`, `finalize
//! @String`, and `finalize @([Text], Text, Bool)`. Each crossed and trapped.
//!
//! Both halves of the fix are covered here, because either alone leaves the
//! answerer unable to answer:
//!
//! - PIN: with the hole's type known, the turn compiles against a `finalize`
//!   whose `v` is pinned to it, so a wrong-typed answer is a GHC error the
//!   corrective-retry loop feeds back ([`wrong_typed_finalize_is_a_compile_error`]).
//! - SCOPE: the answer type is an AUTHOR type, so the turn also imports the
//!   module defining it — otherwise the model cannot name the type it is being
//!   asked for ([`pinned_finalize_needs_the_type_in_scope`] pins the failure
//!   mode; [`answer_contract_puts_the_type_in_scope`] pins the driver-side
//!   contract that supplies it).
//!
//! Compile-level, so each case is one deterministic `tidepool-extract` call
//! with no model in the loop. Needs `TIDEPOOL_EXTRACT` and the with-packages
//! GHC on PATH (`--ignore-default-filter` to run; see `tests/golden_path.rs`
//! for the env recipe).

use tidepool_harness::compile;
use tidepool_harness::engine::{template_turn_for, EngineConfig};
use tidepool_harness::{answerer_decls, load_harness_source};

fn extract_available() -> bool {
    std::env::var("TIDEPOOL_EXTRACT").is_ok()
        || std::process::Command::new("tidepool-extract")
            .arg("--help")
            .output()
            .is_ok()
}

fn repo_root() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("tidepool-harness has a parent (the repo root)")
        .to_path_buf()
}

/// The answerer's real compile setup: its scoped `[AskUser, Fork, Finalize]`
/// row plus `examples/harness` on the include path, so `HarnessTypes` (and its
/// `Decision`) resolves — exactly what `tidepool-selfharness` wires.
fn answerer_cfg() -> EngineConfig {
    let mut cfg = EngineConfig::from_decls(answerer_decls(), repo_root().join("haskell/lib"), None)
        .expect("answerer engine config");
    cfg.include.push(repo_root().join("examples/harness"));
    cfg
}

/// Compile one answerer turn. `finalize_ty` is the hole's answer type when the
/// turn is answering a typed hole (the pin); `None` is the unpinned turn.
fn compile_turn(
    code: &str,
    imports: &str,
    finalize_ty: Option<&str>,
) -> Result<compile::CompiledTurn, compile::CompileError> {
    let cfg = answerer_cfg();
    let src = template_turn_for(&cfg.decls, &cfg, code, imports, "", finalize_ty);
    compile::compile_turn(&cfg.extract_bin, &src, "result", &cfg.include)
}

const GOOD_DECISION: &str = "(finalize @Decision (Decision { action = \"observe\", \
     rationale = \"because\", confidence = High }) :: M ())";

/// A correctly-typed `finalize` against a pinned turn still compiles — the pin
/// constrains, it does not break the working path. `@Decision` must keep
/// binding the FIRST tyvar (`v`) and leave the result type free, so the
/// `:: M ()` spelling every existing answerer writes survives.
#[test]
fn correctly_typed_finalize_still_compiles_when_pinned() {
    if !extract_available() {
        eprintln!("Skipping: tidepool-extract not available (set TIDEPOOL_EXTRACT)");
        return;
    }
    let result = compile_turn(GOOD_DECISION, "HarnessTypes", Some("Decision"));
    assert!(
        result.is_ok(),
        "a correctly-typed finalize must compile against a pinned turn, got: {:?}",
        result.err().map(|e| e.to_string())
    );
}

/// THE fix. Each of these is a wrong-typed `finalize` the UNPINNED verb accepts;
/// against a `Decision`-pinned turn every one must be a COMPILE ERROR — caught
/// by GHC, fed back by the retry loop, never crossing into a `Decision`-typed
/// continuation to trap there.
///
/// The three shapes are the ones the live dogfood actually produced, not
/// invented ones: a `Text`, a `String`, and a 3-tuple structurally echoing the
/// record's fields (the tuple is the nastiest — its arity MATCHES the record's,
/// so the trap reported a plausible-looking field count).
#[test]
fn wrong_typed_finalize_is_a_compile_error() {
    if !extract_available() {
        eprintln!("Skipping: tidepool-extract not available (set TIDEPOOL_EXTRACT)");
        return;
    }
    let wrong = [
        ("Text", "(finalize @Text (\"oops\" :: Text) :: M ())"),
        (
            "inferred Text (no type application)",
            "(finalize (\"oops\" :: Text) :: M ())",
        ),
        (
            "arity-matching 3-tuple",
            "(finalize @([Text], Text, Bool) ([\"a\"], \"b\", False) :: M ())",
        ),
    ];
    for (label, code) in wrong {
        let result = compile_turn(code, "HarnessTypes", Some("Decision"));
        assert!(
            result.is_err(),
            "finalize of a {label} must NOT compile against a Decision-pinned \
             turn — it would cross in-heap and case-trap"
        );
    }
}

/// The control: the SAME wrong-typed block compiles fine when the turn is not
/// pinned. This is the hole as it stood — `v` unconstrained, so the value
/// crosses and traps at the far side. Pinning is what closes it; without this
/// case the test above could pass for an unrelated reason.
#[test]
fn wrong_typed_finalize_compiles_when_unpinned() {
    if !extract_available() {
        eprintln!("Skipping: tidepool-extract not available (set TIDEPOOL_EXTRACT)");
        return;
    }
    let result = compile_turn("(finalize @Text (\"oops\" :: Text) :: M ())", "", None);
    assert!(
        result.is_ok(),
        "unpinned finalize accepts any type (the hole this pins shut), got: {:?}",
        result.err().map(|e| e.to_string())
    );
}

/// The pin alone is not enough: it names the answer type, so the type must be
/// IN SCOPE or the turn cannot compile at all. This is why the answer contract
/// carries imports as well as a type — and it is the failure the answerer hit
/// live, before it gave up and substituted a tuple.
#[test]
fn pinned_finalize_needs_the_type_in_scope() {
    if !extract_available() {
        eprintln!("Skipping: tidepool-extract not available (set TIDEPOOL_EXTRACT)");
        return;
    }
    let err = compile_turn(GOOD_DECISION, "", Some("Decision"))
        .err()
        .map(|e| e.to_string())
        .expect("a pinned turn without the type imported cannot compile");
    assert!(
        err.contains("Decision"),
        "the error must name the unresolved type so the retry prompt is actionable, got: {err}"
    );
}

/// The driver side of SCOPE: the answerer's imports are derived from the
/// modules the harness itself imports, so the reference harness offers
/// `HarnessTypes` — and never itself. Importing the harness module would not
/// work: it defines `loop`, whose `runLLMTurn` is absent from the answerer's
/// row, and GHC compiles an imported module whole.
#[test]
fn answer_contract_puts_the_type_in_scope() {
    let source = load_harness_source(&repo_root().join("examples/harness/Harness.hs"))
        .expect("reference harness resolves");
    assert_eq!(source.answerer_imports, vec!["HarnessTypes".to_string()]);
    assert!(!source.answerer_imports.contains(&source.module_name));
}
