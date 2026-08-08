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
//! - PIN: `Finalize` is TYPE-INDEXED by its answer type (like `State s`), so
//!   the turn compiles against a ROW instantiated at the hole's type —
//!   `Member (Finalize T) effs` is the pin, not a shimmed/shadowed binding.
//!   A wrong-typed answer is a GHC error naming the row, fed back by the
//!   corrective-retry loop ([`wrong_typed_finalize_is_a_compile_error`]).
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
/// turn is answering a typed hole — the row is instantiated at it
/// (`Finalize <ty>`, importing `imports`); `None` compiles at the config's
/// default row (`Finalize NoAnswer`).
fn compile_turn(
    code: &str,
    imports: &str,
    finalize_ty: Option<&str>,
) -> Result<compile::CompiledTurn, compile::CompileError> {
    let cfg = answerer_cfg();
    let row_imports: Vec<String> = if imports.trim().is_empty() {
        Vec::new()
    } else {
        vec![imports.to_string()]
    };
    let target = cfg
        .turn_target(finalize_ty.map(|ty| (ty, row_imports.as_slice())))
        .expect("turn target");
    let src = template_turn_for(&cfg.decls, &target.stack, code, imports, "");
    compile::compile_turn(
        &cfg.extract_bin,
        &src,
        "result",
        &target.include,
        tidepool_harness::timing::NO_NODE,
        tidepool_harness::timing::NO_ROUND,
    )
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

/// The control. Under row-indexing, "unpinned" is no longer an expressible
/// state — `Member (Finalize v)` is satisfiable by exactly the ONE type
/// applied to `Finalize` in the row, never by an unconstrained `v` — so the
/// control is not "no pin", it is a DIFFERENT pin: the same wrong-typed-for-
/// `Decision` block compiles fine when the row instead names `Finalize Text`.
/// This proves `wrong_typed_finalize_is_a_compile_error`'s rejections come
/// from the row PARAMETER selecting which type is admitted, not from some
/// unrelated compile breakage that would reject the block regardless of what
/// the row names.
#[test]
fn wrong_typed_finalize_compiles_when_the_row_names_text() {
    if !extract_available() {
        eprintln!("Skipping: tidepool-extract not available (set TIDEPOOL_EXTRACT)");
        return;
    }
    let result = compile_turn(
        "(finalize @Text (\"oops\" :: Text) :: M ())",
        "",
        Some("Text"),
    );
    assert!(
        result.is_ok(),
        "finalize @Text must compile when the row is instantiated at Finalize \
         Text — the same block that is rejected above against a Decision-pinned \
         row, got: {:?}",
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

/// The effects staging dir is content-addressed on the GENERATED
/// `Tidepool.Effects` source — which only ever says `import AuthorType`, never
/// the type's actual constructors. Two compiles that pin the SAME row
/// (`Finalize Foo`, importing `AuthorType`) therefore hash to the SAME staging
/// dir and reuse it (`ensure_effects_module_at` writes SOURCE ONLY, no
/// `.hi`/`.o`), so an edit to `AuthorType.hs` BETWEEN those two compiles must
/// still be picked up by the second — there is no compiled artifact for the
/// dir's content hash to have to cover, and this pins that the extract compile
/// itself isn't caching stale bytecode for the author module either.
#[test]
fn author_module_edit_between_compiles_is_picked_up_by_the_second() {
    if !extract_available() {
        eprintln!("Skipping: tidepool-extract not available (set TIDEPOOL_EXTRACT)");
        return;
    }
    let dir = tempfile::tempdir().expect("temp dir for the author module");
    let module_path = dir.path().join("AuthorType.hs");

    let mut cfg = answerer_cfg();
    cfg.include.push(dir.path().to_path_buf());

    let compile_at = |code: &str| -> Result<compile::CompiledTurn, compile::CompileError> {
        let row_imports = vec!["AuthorType".to_string()];
        let target = cfg
            .turn_target(Some(("Foo", row_imports.as_slice())))
            .expect("turn target");
        let src = template_turn_for(&cfg.decls, &target.stack, code, "AuthorType", "");
        compile::compile_turn(
            &cfg.extract_bin,
            &src,
            "result",
            &target.include,
            tidepool_harness::timing::NO_NODE,
            tidepool_harness::timing::NO_ROUND,
        )
    };

    std::fs::write(
        &module_path,
        "module AuthorType where\ndata Foo = MkFooOld deriving (Show)\n",
    )
    .expect("write v1 author module");
    let first = compile_at("(finalize @Foo MkFooOld :: M ())");
    assert!(
        first.is_ok(),
        "first compile against the v1 author module must succeed, got: {:?}",
        first.err().map(|e| e.to_string())
    );

    std::fs::write(
        &module_path,
        "module AuthorType where\ndata Foo = MkFooNew deriving (Show)\n",
    )
    .expect("rewrite the author module with a different constructor set");

    let stale = compile_at("(finalize @Foo MkFooOld :: M ())");
    assert!(
        stale.is_err(),
        "MkFooOld no longer exists in the rewritten author module — a second \
         compile that still accepts it would mean the staging dir served a \
         stale AuthorType"
    );

    let second = compile_at("(finalize @Foo MkFooNew :: M ())");
    assert!(
        second.is_ok(),
        "the second compile must see the NEW definition (MkFooNew), got: {:?}",
        second.err().map(|e| e.to_string())
    );
}
