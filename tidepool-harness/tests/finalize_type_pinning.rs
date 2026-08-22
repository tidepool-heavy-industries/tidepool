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

mod support;

use tidepool_harness::answerer_decls;
use tidepool_harness::engine::{
    self, answerer_hole_card, template_turn_for, CompiledTurn, EngineConfig,
};
use tidepool_runtime::CompileError;

fn repo_root() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("tidepool-harness has a parent (the repo root)")
        .to_path_buf()
}

/// The answerer's real compile setup: its scoped `[AskUser, Fork, ReadState, Green, Finalize]`
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
/// default row (`Finalize Void`).
fn compile_turn(
    code: &str,
    imports: &str,
    finalize_ty: Option<&str>,
) -> Result<CompiledTurn, CompileError> {
    let cfg = answerer_cfg();
    let row_imports: Vec<String> = if imports.trim().is_empty() {
        Vec::new()
    } else {
        vec![imports.to_string()]
    };
    // `turn_target` itself can fail here (not just IO): a pinned row whose
    // applied type has no resolving import fails the ROW at this point, with
    // GHC's own "Not in scope" diagnostic, rather than materializing a
    // target that would blow up confusingly downstream — see
    // `engine::validate_finalize_row`'s doc. Propagated as a `CompileError`
    // like every other compile failure below, not `.expect()`'d, since
    // `pinned_finalize_needs_the_type_in_scope` exercises exactly this path.
    let target = match cfg.turn_target(finalize_ty.map(|ty| (ty, row_imports.as_slice()))) {
        Ok(t) => t,
        Err(e) => return Err(CompileError::ExtractFailed(e.to_string())),
    };
    let src = template_turn_for(
        &cfg.decls,
        &target.stack,
        code,
        imports,
        "",
        cfg.delegate_wrap,
    );
    engine::compile_turn(
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
    support::require_extract();
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
    support::require_extract();
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
/// Asserts `wrong_typed_finalize_is_a_compile_error`'s rejections come
/// from the row PARAMETER selecting which type is admitted, not from some
/// unrelated compile breakage that would reject the block regardless of what
/// the row names.
#[test]
fn wrong_typed_finalize_compiles_when_the_row_names_text() {
    support::require_extract();
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
    support::require_extract();
    let err = compile_turn(GOOD_DECISION, "", Some("Decision"))
        .err()
        .map(|e| match e {
            // `CompileError::Diagnostics`' own `Display` is only a count
            // ("Haskell compilation failed (N diagnostic(s))") — assert
            // against GHC's own message text instead, same as
            // `classify_compile`/`compile_error_to_session_error` do.
            CompileError::Diagnostics(diags) => diags
                .iter()
                .map(|d| d.message.as_str())
                .collect::<Vec<_>>()
                .join("\n\n"),
            other => other.to_string(),
        })
        .expect("a pinned turn without the type imported cannot compile");
    assert!(
        err.contains("Decision"),
        "the error must name the unresolved type so the retry prompt is actionable, got: {err}"
    );
}

/// The driver side of SCOPE, on the CURRENT mechanism: extract itself
/// resolves `Decision`'s defining module from the real type environment at
/// the `finalize @Decision` call site and reports it in `asks.json`
/// (`AsksSidecar::modules_of`) — `SelfHarnessDriver::answer_contract` pins
/// its imports from that lookup, not from a scan of `Harness.hs`'s own
/// import lines (the retired `HarnessSource::answerer_imports`). Pinned
/// to `HarnessTypes` here exactly like the retired scrape-based test was,
/// but via the mechanism that also resolves a MODEL-declared decl-plane
/// type, which no import scan of a static file could ever see.
#[test]
fn answer_contract_puts_the_type_in_scope() {
    support::require_extract();
    let result = compile_turn(GOOD_DECISION, "HarnessTypes", Some("Decision"))
        .expect("a correctly-typed finalize compiles");
    let (_, _, modules) = result
        .asks
        .iter()
        .next()
        .expect("the finalize call site has an asks.json entry");
    assert_eq!(
        modules,
        &["HarnessTypes".to_string()],
        "Decision's defining module must be resolved from the type itself, not \
         scraped from Harness.hs"
    );
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
    support::require_extract();
    let dir = tempfile::tempdir().expect("temp dir for the author module");
    let module_path = dir.path().join("AuthorType.hs");

    let mut cfg = answerer_cfg();
    cfg.include.push(dir.path().to_path_buf());

    let compile_at = |code: &str| -> Result<CompiledTurn, CompileError> {
        let row_imports = vec!["AuthorType".to_string()];
        let target = cfg
            .turn_target(Some(("Foo", row_imports.as_slice())))
            .expect("turn target");
        let src = template_turn_for(
            &cfg.decls,
            &target.stack,
            code,
            "AuthorType",
            "",
            cfg.delegate_wrap,
        );
        engine::compile_turn(
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

// ---------------------------------------------------------------------------
// finalize-template-pin: the answerer prompt prescribes a `finalize` shape
// that must actually compile against a pinned row with NO annotation the
// model has to discover. See `plans/post-restart/dev/finalize-template-pin.md`.
//
// `_r <- __user; paginateResult 4096 (toJSON _r)` is the shared eval template
// (`tidepool-mcp/src/eval_prep.rs`); `finalize`'s free result tyvar `a`
// (`finalize :: forall v a effs. Member (Finalize v) effs => v -> Eff effs
// a`) left `_r` ambiguous there — `toJSON`/`ToWire` are ordinary library
// classes with no superclass, and GHC's defaulting (even under
// `ExtendedDefaultRules`, even with the explicit `default (Int, Double,
// Text)` already in the preamble) only fires when the ambiguous variable's
// constraint set carries at least one class from GHC's own fixed "standard"
// set (the GHC User's Guide's `ExtendedDefaultRules` section states rule 3
// as relaxed to "at least one of the classes Ci is numeric, or is Show, Eq,
// or Ord" — a relaxation of the anchor requirement, never its removal). A
// solitary `ToJSON a0` never qualifies. Confirmed empirically, not by
// reasoning about the docs: a minimal `IO`-do-block repro and a real
// `freer-simple` `Eff`-row repro with an identical custom class fail
// IDENTICALLY under identical pragmas — ruling out the `Eff`-row/
// `MonoLocalBinds`/implication explanation a working hypothesis had assumed
// — and adding a bare `Num` constraint on the SAME otherwise-ambiguous tyvar,
// nothing else changed, makes defaulting fire. `template_turn_for` supplies
// that missing anchor (`__anchor :: P.Show a => a -> a; __anchor = P.id`,
// additive — it never forces `_r`'s type) only when compiling against a real
// (non-`Void`) `Finalize T` row.

/// Reuse the KNOWN-VALID `Decision` record literal from [`GOOD_DECISION`]
/// (rather than re-deriving `Decision`'s field names) as the value plugged
/// into shapes recovered from the answerer prompt below.
const A_DECISION: &str =
    "Decision { action = \"observe\", rationale = \"because\", confidence = High }";

/// Pull the backtick-quoted `finalize` shape out of
/// [`answerer_hole_card`]'s "Answer by evaluating `...`" sentence — the
/// prompt text an answerer turn actually receives for a pinned hole. This is
/// a DERIVATION (parse the live prompt), not a retyped copy: if
/// `answerer_hole_card`'s template ever changes shape (drops the inner `::
/// {ty}`, adds an outer `:: M {ty}`, anything), this function reflects it
/// and the caller's `assert_eq!` against the last-known shape (not this
/// function) is what tracks the drift instead of silently going stale.
fn prescribed_finalize_shape(ty: &str, imports: &[String]) -> String {
    let card = answerer_hole_card("answer the loop's request", Some(ty), imports, None, &[]);
    const MARKER: &str = "evaluating `";
    let start = card.find(MARKER).unwrap_or_else(|| {
        panic!("answerer_hole_card must prescribe a `finalize` shape via \"evaluating `...`\", got: {card}")
    }) + MARKER.len();
    let rest = &card[start..];
    let end = rest
        .find('`')
        .unwrap_or_else(|| panic!("unterminated backtick-quoted shape in: {card}"));
    let shape = rest[..end].to_string();
    assert!(
        shape.contains("finalize"),
        "expected a `finalize`-shaped prescription, got: {shape:?}"
    );
    shape
}

/// Assertion 1 — drift-proofing: the shape `answerer_hole_card` (the per-hole
/// answerer prompt) actually prescribes today, DERIVED from the live prompt
/// text rather than retyped, must compile against the pinned row it is
/// prescribed for. This is what keeps prompt and template from silently
/// diverging again — it stays green as long as whatever the prompt currently
/// says compiles, whatever that shape is.
///
/// The `assert_eq!` against today's known shape is the drift SIGNAL (a
/// failure here means the prompt's wording changed — go re-read it and
/// update this literal, not just make the assertion pass); the compile
/// below is the actual claim, applied to whatever shape is live right now.
#[test]
fn prompts_prescribed_hole_card_shape_compiles_when_pinned() {
    support::require_extract();
    let imports = vec!["HarnessTypes".to_string()];
    let shape = prescribed_finalize_shape("Decision", &imports);
    assert_eq!(
        shape, "finalize @Decision value",
        "the answerer prompt's prescribed shape changed — re-read \
         `engine::answerer_hole_card` and update this pinned literal"
    );
    // `value` is the prompt's placeholder identifier for "a real Decision" —
    // substitute a real one, keeping the REST of the derived snippet
    // byte-for-byte what the prompt actually says.
    let code = shape.replacen("value", &format!("({A_DECISION})"), 1);
    let result = compile_turn(&code, "HarnessTypes", Some("Decision"));
    assert!(
        result.is_ok(),
        "the answerer prompt's own prescribed shape ({code:?}) must compile \
         against the Decision-pinned row it names — got: {:?}",
        result.err().map(|e| e.to_string())
    );
}

/// Assertion 2 — the fix's actual claim, mutation-closed: bare `finalize @T
/// value`, with NO annotation of any kind (no inner `:: T` on the argument,
/// no outer `:: M T` on the whole expression — the shape the ORIGINAL
/// dogfood-recovered prompt text prescribed verbatim, and the one no
/// argument-side annotation can ever fix, since it is `finalize`'s RESULT
/// tyvar that's ambiguous, not its argument's), compiles through the real
/// turn path against a pinned `Finalize T` row.
///
/// Pinned regardless of what the prompt currently prescribes (today's tree
/// still has the inner `(value :: T)` annotation — see the shape above) —
/// this is the claim the fix must hold even if the prompt's own wording
/// drifts. Revert `template_turn_for`'s anchor routing
/// (`tidepool-harness/src/engine.rs`) or `TurnTemplate::render`'s
/// `anchor_result` handling (`tidepool-mcp/src/eval_prep.rs`) and this test
/// goes RED with GHC's "Ambiguous type variable 'a0' ... arising from a use
/// of 'toJSON' ... (ToJSON a0)" — the exact defect this pins.
#[test]
fn bare_finalize_with_no_annotation_compiles_when_pinned() {
    support::require_extract();
    let code = format!("finalize @Decision ({A_DECISION})");
    let result = compile_turn(&code, "HarnessTypes", Some("Decision"));
    assert!(
        result.is_ok(),
        "bare `finalize @T value`, with no annotation of any kind, must \
         compile against a pinned Finalize row — got: {:?}",
        result.err().map(|e| e.to_string())
    );
}

/// The case that distinguishes `__anchor` from the REJECTED hard pin (`_r ::
/// T`). A bare, non-bind `askUser @T` turn — gathering operator input with
/// no `finalize` in the same block, a real shape (elicit now, finalize on a
/// LATER turn) — leaves the block's result concretely `M Confidence` (the
/// asked-for type itself), which already has both `Show` and `ToJSON` instances and needs
/// no defaulting at all. `__anchor` is `id` under an ADDITIVE `Show`
/// constraint, so it resolves trivially against that concrete `Int` and
/// changes nothing observable.
///
/// A hard pin would instead unify the block's result type against the row's
/// `Decision` (`_r :: Decision`) and reject this compile outright —
/// `Confidence` is not `Decision` — even though the turn never touches
/// `finalize`. Compiled
/// against a `Decision`-pinned row specifically (not `Finalize Void`) so
/// the anchor is actually active for this compile, proving the additive
/// claim rather than a compile that never exercised it.
#[test]
fn bare_non_bind_askuser_form_compiles_when_pinned() {
    support::require_extract();
    let code = "askUser @Confidence";
    let result = compile_turn(code, "HarnessTypes", Some("Decision"));
    assert!(
        result.is_ok(),
        "a bare non-bind `askUser @T` turn (no finalize in the block) must \
         still compile against a Decision-pinned row — got: {:?}",
        result.err().map(|e| e.to_string())
    );
}
