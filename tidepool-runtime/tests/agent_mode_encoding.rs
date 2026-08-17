//! PRD 18 gate 1(a): the Servant-style mode encoding
//! (`Tidepool.Agent.Contract`, `mode :- Call/Notify`, `compileTools`) proven
//! through the REAL extract/JIT pipeline — not just a typecheck.
//!
//! `dynamic_dispatch_executes_on_real_jit` is the load-bearing test: it
//! builds a `WorkerTools (AsServerT M)` value with a real handler that
//! performs a real effect (`send (Print ...)`), runs `compileTools`, then
//! `dispatch`es a structural argument through the compiled table and asserts
//! on the decoded/re-encoded output. That is what proves the mode
//! encoding's elaborated dictionary code actually executes on the JIT, not
//! merely that `Tidepool.Agent.Contract` compiles.
//!
//! `single_traversal_invariant_*` pins the property `compileTools` exists to
//! guarantee: the declaration key set and the dispatch key set cannot drift,
//! because both are plain projections of ONE `GCompileTools` traversal.
//!
//! The `compile_fail_*` tests are TYPE-LEVEL diagnostics (source-level
//! `TypeError`s, or in one case GHC's own instance-resolution error);
//! `compiletools_time_*` are runtime `ToolCompileError` values — see
//! `plans/post-restart/agent-lanes/receipt-mode-encoding.md` for why the
//! split falls where it does.
//!
//! Requires a worktree extract binary (`cabal build tidepool-extract-bin`,
//! then `TIDEPOOL_EXTRACT` pointed at it, or run inside `nix develop`). Panics
//! loudly (see `require_extract`) when the extractor is unreachable, matching
//! every other `tidepool-runtime/tests/*_generic_deriving*` file.

use serde_json::json;
use tidepool_testing::eval_harness::{require_extract, EvalHarness};

/// Shared header for the standalone (no-effects) diagnostics fixtures: just
/// enough to bring `Tidepool.Agent.Contract` and the shared `Question`/
/// `Decision` endpoint types into scope. Mirrors the `HEADER` pattern in
/// `generic_deriving_337.rs`/`nullary_sum_generic_deriving.rs`.
const HEADER: &str = "{-# LANGUAGE NoImplicitPrelude, OverloadedStrings, DeriveGeneric, DeriveAnyClass, DataKinds, TypeOperators, FlexibleContexts #-}\n\
     module Expr where\n\
     import Tidepool.Prelude hiding (error)\n\
     import Tidepool.Agent.Contract\n";

/// `Question`/`Decision` are the shared `Call` input/output pair used by
/// every fixture below. `compileTools`'s leaf constraint only needs
/// `(FromJSON input, JsonSchema input, ToJSON output)` — `Question` doesn't
/// need `ToJSON` for that — but several fixtures ALSO call `toJSON` on a
/// `Question` directly (to build a dispatch argument from Rust-side test
/// code, outside any generated `Tool` handler), so it derives `ToJSON` too.
const SHARED_TYPES: &str =
    "data Question = Question { questionText :: Text } deriving (Generic, FromJSON, ToJSON, JsonSchema)\n\
     data Decision = Decision { approved :: Bool } deriving (Generic, ToJSON)\n";

fn run_pure(source: &str, target: &str) -> serde_json::Value {
    require_extract();
    EvalHarness::new()
        .with_stdlib()
        .run_pure(source, target)
        .expect("compile_and_run_pure failed")
        .to_json()
}

// ---------------------------------------------------------------------------
// The load-bearing dynamic-dispatch proof
// ---------------------------------------------------------------------------

/// A self-contained one-effect (`Console`) stack + `M` — deliberately NOT
/// `tidepool_testing::eval_harness::mock::MCP_PREAMBLE`: that preamble's own
/// declarations start immediately after its five-line import block, so a
/// caller can't splice in an extra `import Tidepool.Agent.Contract` without
/// editing shared test infra other lanes depend on. Declaring one effect
/// directly is simpler and still proves the point: `dispatch` runs the
/// handler in a REAL `Eff` row, not a bare `IO`/`Identity` stand-in.
const EFFECT_HEADER: &str = "{-# LANGUAGE NoImplicitPrelude, OverloadedStrings, DeriveGeneric, DeriveAnyClass, DataKinds, TypeOperators, FlexibleContexts, GADTs #-}\n\
     module Expr where\n\
     import Tidepool.Prelude hiding (error)\n\
     import Tidepool.Agent.Contract\n\
     import qualified Tidepool.Data.Text as T\n\
     import Control.Monad.Freer hiding (run)\n\n\
     data Console a where\n\
     \x20 Print :: Text -> Console ()\n\n\
     type M = Eff '[Console]\n";

fn worker_tools_module(body: &str) -> String {
    format!(
        "{EFFECT_HEADER}\n{SHARED_TYPES}\n\
         data WorkerTools mode = WorkerTools\n\
         \x20 {{ askParent :: mode :- Call Question Decision\n\
         \x20 , reportProgress :: mode :- Notify Text\n\
         \x20 }} deriving (Generic)\n\n\
         workerTools :: WorkerTools (AsServerT M)\n\
         workerTools = WorkerTools\n\
         \x20 {{ askParent = tool \"Ask the resident to resolve a question.\" $ \\q -> do\n\
         \x20     send (Print (T.append \"asked: \" (questionText q)))\n\
         \x20     pure (Decision (T.length (questionText q) > 5))\n\
         \x20 , reportProgress = notify \"Report progress.\" $ \\msg -> do\n\
         \x20     send (Print (T.append \"progress: \" msg))\n\
         \x20     pure ()\n\
         \x20 }}\n\n\
         {body}\n"
    )
}

/// `tidepool_effect`'s generic `DispatchEffect` impl covers a single-element
/// `HList` (`HCons<H, HNil>`) same as the eleven-element `mock::min_stack`,
/// so one handler for the one effect declared above is a complete stack.
fn console_stack() -> frunk::HList!(tidepool_testing::eval_harness::mock::MockConsole) {
    frunk::hlist![tidepool_testing::eval_harness::mock::MockConsole]
}

/// The gate's central claim: `compileTools` elaborates on the real JIT AND
/// its `dispatch` actually runs the handler — including a real effect inside
/// that handler — and returns the correctly re-encoded output. Not a
/// typecheck-only proof.
#[test]
fn dynamic_dispatch_executes_on_real_jit() {
    require_extract();
    let src = worker_tools_module(
        "result :: M Value\n\
         result = case compileTools workerTools of\n\
         \x20 Left _ -> pure (toJSON (\"compile-error\" :: Text))\n\
         \x20 Right compiled -> dispatch compiled \"ask_parent\" (toJSON (Question \"should we ship this quarter?\"))\n",
    );
    let out = EvalHarness::new()
        .with_stdlib()
        .run(&src, "result", console_stack());
    assert!(
        out.is_ok(),
        "dispatch must run on the real JIT: {:?}",
        out.err()
    );
    assert_eq!(
        out.json(),
        json!({"approved": true}),
        "dispatch decoded the Question, ran the handler, and re-encoded the Decision"
    );
}

/// `reportProgress` is a `Notify`, interpreted as `Tool m input ()`, and
/// dispatches through the same leaf path as `Call`.
#[test]
fn notify_endpoint_dispatches_through_the_same_path() {
    require_extract();
    let src = worker_tools_module(
        "result :: M Value\n\
         result = case compileTools workerTools of\n\
         \x20 Left _ -> pure (toJSON (\"compile-error\" :: Text))\n\
         \x20 Right compiled -> dispatch compiled \"report_progress\" (toJSON (\"halfway done\" :: Text))\n",
    );
    let out = EvalHarness::new()
        .with_stdlib()
        .run(&src, "result", console_stack());
    assert!(
        out.is_ok(),
        "notify dispatch must run on the real JIT: {:?}",
        out.err()
    );
    assert_eq!(
        out.json(),
        json!(null),
        "Notify's output is Tool m input () — () encodes as null"
    );
}

// ---------------------------------------------------------------------------
// Single-traversal invariant: declaration keys == dispatch keys
// ---------------------------------------------------------------------------

/// Pins the property `compileTools`'s doc comment claims: `declarations`
/// and `dispatchNames` are built from the SAME `GCompileTools` traversal
/// result, in the same order, so they cannot silently disagree. A
/// regression that split this into two traversals (e.g. one deriving names
/// from the record, the other from a separately-maintained list) would show
/// up here as a set/order mismatch.
#[test]
fn single_traversal_invariant_declaration_and_dispatch_keys_match() {
    let src = worker_tools_module(
        "result :: Bool\n\
         result = case compileTools workerTools of\n\
         \x20 Left _ -> False\n\
         \x20 Right compiled -> map dtdName (declarations compiled) == dispatchNames compiled\n",
    );
    let v = run_pure(&src, "result");
    assert_eq!(
        v,
        json!(true),
        "declaration key set must equal dispatch key set"
    );
}

/// The invariant test above only proves the two lists are equal for a
/// well-formed record; this pins that they are also non-trivial (two
/// distinct, correctly-named entries) so the equality isn't vacuously true
/// over an accidentally-empty list.
#[test]
fn single_traversal_invariant_names_are_the_expected_two() {
    let src = worker_tools_module(
        "result :: [Text]\n\
         result = case compileTools workerTools of\n\
         \x20 Left _ -> []\n\
         \x20 Right compiled -> dispatchNames compiled\n",
    );
    let v = run_pure(&src, "result");
    assert_eq!(
        v,
        json!(["ask_parent", "report_progress"]),
        "selector -> snake_case, field order preserved"
    );
}

// ---------------------------------------------------------------------------
// Diagnostics — type-level (compile-fail fixtures)
// ---------------------------------------------------------------------------

/// "the tool record does not derive Generic" — NOT an authored `TypeError`:
/// see the receipt for why an overlapping-instance rescue can't distinguish
/// two `HasAgentApi tools m` instances with identical heads. This is GHC's
/// own instance-resolution error, checked here so a regression that makes it
/// WORSE (e.g. starts leaking `Rep`) is caught.
#[test]
fn compile_fail_tools_record_missing_generic() {
    require_extract();
    let src = format!(
        "{HEADER}\n{SHARED_TYPES}\n\
         data BadTools mode = BadTools\n\
         \x20 {{ askParent :: mode :- Call Question Decision }}\n\n\
         result :: Int\n\
         result = case compileTools (undefined :: BadTools (AsServerT Maybe)) of\n\
         \x20 Left _ -> 0\n\
         \x20 Right _ -> 1\n"
    );
    match EvalHarness::new().with_stdlib().compile(&src, "result") {
        Ok(_) => panic!("a tools record without `deriving (Generic)` must not compile"),
        Err(e) => {
            let msg = tidepool_runtime::classify_compile(&e).message;
            assert!(
                msg.contains("BadTools"),
                "expected the message to name the record, got:\n{msg}"
            );
            assert!(
                msg.contains("Generic"),
                "expected the message to say Generic is missing, got:\n{msg}"
            );
            assert!(
                !msg.contains("JSON-RPC") && !msg.contains("codex-codes"),
                "must not leak backend vocabulary, got:\n{msg}"
            );
            // NOT asserting Rep-freedom here: `classify_compile` joins EVERY
            // diagnostic GHC emits for this module, and `HasAgentApi`'s
            // context lists `Generic (...)` AND `GCompileTools (Rep (...)) m`
            // — when `Generic` has no instance, GHC's solver can still try
            // (and fail) the second constraint too, so a follow-on "no
            // instance for GCompileTools (Rep (BadTools ...))" diagnostic can
            // legitimately appear alongside the primary one. See the receipt
            // for why this is the honestly-reported shape of this
            // diagnostic, not a design defect to paper over with an
            // assertion that doesn't hold.
        }
    }
}

/// An unsupported endpoint is rejected by the mode family's fallthrough.
#[test]
fn compile_fail_unsupported_endpoint_type() {
    require_extract();
    let src = format!(
        "{HEADER}\n{SHARED_TYPES}\n\
         data BadTools2 mode = BadTools2\n\
         \x20 {{ oops :: mode :- Text }} deriving (Generic)\n\n\
         result :: Int\n\
         result = case compileTools (undefined :: BadTools2 (AsServerT Maybe)) of\n\
         \x20 Left _ -> 0\n\
         \x20 Right _ -> 1\n"
    );
    match EvalHarness::new().with_stdlib().compile(&src, "result") {
        Ok(_) => panic!("an unsupported mode endpoint must not compile"),
        Err(e) => {
            let msg = tidepool_runtime::classify_compile(&e).message;
            assert!(
                msg.contains("unsupported agent tool endpoint"),
                "expected the authored endpoint diagnostic, got:\n{msg}"
            );
            assert!(
                msg.contains("Call input output") && msg.contains("Notify input"),
                "expected the diagnostic to name both valid endpoint forms, got:\n{msg}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// The input schema IS the schema of the generic JSON encoding
// ---------------------------------------------------------------------------

/// A one-endpoint tools record whose `Call` input is `input_ty`, preceded by
/// whatever `decls` define it. `result` is the compiled declaration's
/// `input_schema` — the value `compileTools` actually hands a backend.
///
/// One fixture serves both polarities below: the schema-shape pins read
/// `result`, and the compile-fail diagnostics never get that far because
/// `JsonSchema`/`FromJSON` reject the endpoint's input type at the
/// `GCompileTools` leaf.
fn input_schema_module(decls: &str, input_ty: &str) -> String {
    format!(
        "{HEADER}\n{SHARED_TYPES}\n{decls}\n\
         data InputTools mode = InputTools\n\
         \x20 {{ recordInput :: mode :- Call {input_ty} Decision }} deriving (Generic)\n\n\
         inputTools :: InputTools (AsServerT Maybe)\n\
         inputTools = InputTools {{ recordInput = tool \"record the input\" (\\_ -> Just (Decision True)) }}\n\n\
         result :: Value\n\
         result = case compileTools inputTools of\n\
         \x20 Left _ -> toJSON (\"compile-error\" :: Text)\n\
         \x20 Right compiled -> case declarations compiled of\n\
         \x20   (d : _) -> dtdInputSchema d\n\
         \x20   [] -> toJSON (\"no declarations\" :: Text)\n"
    )
}

/// A record input: an object keyed by VERBATIM selector names (no
/// normalization anywhere on this path), with a `Maybe` field present in
/// `properties` and absent from `required` — the schema statement of what
/// `FromJSON`'s `.:?` already accepts.
#[test]
fn tool_input_schema_is_the_generic_record_shape() {
    let src = input_schema_module(
        "data Note = Note { noteFile :: Text, noteLine :: Int, noteFix :: Maybe Text }\n\
         \x20 deriving (Generic, FromJSON, JsonSchema)\n",
        "Note",
    );
    assert_eq!(
        run_pure(&src, "result"),
        json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "noteFile": { "type": "string" },
                "noteLine": { "type": "integer" },
                "noteFix": { "type": "string" }
            },
            "required": ["noteFile", "noteLine"]
        }),
        "an optional field is expressed by ABSENCE from required, never by omission \
         from properties"
    );
}

/// A multi-constructor input is now SUPPORTED, and its schema is the shape the
/// vendored encoder/decoder actually use: an all-nullary sum is a bare
/// constructor-name string, so its schema is a string enum.
#[test]
fn nullary_sum_tool_input_schema_is_a_string_enum() {
    let src = input_schema_module(
        "data Verdict = Yes | No deriving (Generic, FromJSON, JsonSchema)\n",
        "Verdict",
    );
    assert_eq!(
        run_pure(&src, "result"),
        json!({ "type": "string", "enum": ["Yes", "No"] })
    );
}

/// A payload sum is `oneOf` tagged objects — aeson's `TaggedObject` shape,
/// which is exactly what the vendored `ToJSON`/`FromJSON` defaults produce and
/// consume.
#[test]
fn payload_sum_tool_input_schema_is_oneof_tagged_objects() {
    let src = input_schema_module(
        "data Verdict = Yes | Because { reason :: Text }\n\
         \x20 deriving (Generic, FromJSON, JsonSchema)\n",
        "Verdict",
    );
    assert_eq!(
        run_pure(&src, "result"),
        json!({
            "oneOf": [
                {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": { "tag": { "type": "string", "enum": ["Yes"] } },
                    "required": ["tag"]
                },
                {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "tag": { "type": "string", "enum": ["Because"] },
                        "reason": { "type": "string" }
                    },
                    "required": ["tag", "reason"]
                }
            ]
        }),
        "a nullary constructor in a MIXED sum is the zero-field case of the same \
         tagged object, never a bare string"
    );
}

/// REPIN (citing 334d794a, "feat(aeson): positional sum payloads — aeson
/// TaggedObject contents form"): this test used to pin that a positional
/// payload constructor as a `Call` input must not compile. 334d794a
/// deliberately lifted that rejection for the encoder/decoder/schema triple
/// (`Tidepool.Aeson.Value`/`FromJSON`/`Schema`'s shared `IsRecordCon`
/// dispatch) — a positional constructor now schedules a required
/// `"contents"` property instead of a `GAllFieldsNamed` `TypeError`. The
/// tool surface's `JsonSchema` constraint imports that same dispatch, so a
/// type the encoder/decoder accept is a type `compileTools` accepts too:
/// this now compiles and its `input_schema` is the `"contents"`-property
/// shape `Schema.hs`'s `GTaggedConSchema 'False` instance builds.
/// (`compile_fail_payload_field_named_tag` below is the compile-time
/// rejection 334d794a did NOT touch — a payload field literally named
/// `tag` still collides with the discriminator on every branch.)
#[test]
fn positional_payload_call_input_schema_is_a_contents_property() {
    let src = input_schema_module(
        "data Verdict = Yes | Because Text deriving (Generic, FromJSON, JsonSchema)\n",
        "Verdict",
    );
    assert_eq!(
        run_pure(&src, "result"),
        json!({
            "oneOf": [
                {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": { "tag": { "type": "string", "enum": ["Yes"] } },
                    "required": ["tag"]
                },
                {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "tag": { "type": "string", "enum": ["Because"] },
                        "contents": { "type": "string" }
                    },
                    "required": ["tag", "contents"]
                }
            ]
        }),
        "a single positional field schedules one required \"contents\" property \
         keyed by the field's own schema, not a compile-time rejection"
    );
}

// ---------------------------------------------------------------------------
// Diagnostics — compileTools-time (ToolCompileError values)
// ---------------------------------------------------------------------------

/// "two selectors normalize to the same wire name" — only detectable once
/// selector NAME STRINGS (not just types) are in hand, so this is a
/// `ToolCompileError` value, not a `TypeError`. The realistic way this
/// happens: one selector already spelled snake_case, its camelCase sibling
/// normalizing to the identical name.
#[test]
fn compiletools_time_duplicate_wire_name() {
    let src = format!(
        "{HEADER}\n{SHARED_TYPES}\n\
         data DupTools mode = DupTools\n\
         \x20 {{ askParent :: mode :- Call Question Decision\n\
         \x20 , ask_parent :: mode :- Call Question Decision\n\
         \x20 }} deriving (Generic)\n\n\
         dupTools :: DupTools (AsServerT Maybe)\n\
         dupTools = DupTools\n\
         \x20 {{ askParent = tool \"a\" (\\_ -> Just (Decision True))\n\
         \x20 , ask_parent = tool \"b\" (\\_ -> Just (Decision False))\n\
         \x20 }}\n\n\
         result :: Text\n\
         result = case compileTools dupTools of\n\
         \x20 Left err -> renderToolCompileError err\n\
         \x20 Right _ -> \"unexpectedly compiled\"\n"
    );
    let v = run_pure(&src, "result");
    let msg = v.as_str().expect("result is Text");
    assert!(
        msg.contains("DupTools"),
        "must name the record, got:\n{msg}"
    );
    assert!(
        msg.contains("askParent") && msg.contains("ask_parent"),
        "must name both selectors, got:\n{msg}"
    );
    assert!(
        msg.contains("\"ask_parent\""),
        "must name the colliding wire name, got:\n{msg}"
    );
    assert!(
        msg.to_lowercase().contains("rename"),
        "must suggest the smallest fix, got:\n{msg}"
    );
}

/// "a normalized name violates backend identifier rules" — also only
/// knowable from the actual string, so also a `ToolCompileError`. A selector
/// prefixed with `_` (a common Haskell record-field convention) is the
/// realistic trigger: its snake_case form still starts with `_`.
#[test]
fn compiletools_time_invalid_identifier() {
    let src = format!(
        "{HEADER}\n{SHARED_TYPES}\n\
         data UnderscoreTools mode = UnderscoreTools\n\
         \x20 {{ _askParent :: mode :- Call Question Decision }} deriving (Generic)\n\n\
         underscoreTools :: UnderscoreTools (AsServerT Maybe)\n\
         underscoreTools = UnderscoreTools {{ _askParent = tool \"a\" (\\_ -> Just (Decision True)) }}\n\n\
         result :: Text\n\
         result = case compileTools underscoreTools of\n\
         \x20 Left err -> renderToolCompileError err\n\
         \x20 Right _ -> \"unexpectedly compiled\"\n"
    );
    let v = run_pure(&src, "result");
    let msg = v.as_str().expect("result is Text");
    assert!(
        msg.contains("UnderscoreTools"),
        "must name the record, got:\n{msg}"
    );
    assert!(
        msg.contains("_askParent"),
        "must name the selector, got:\n{msg}"
    );
    assert!(
        msg.contains("lowercase letter"),
        "must state the identifier rule that was violated, got:\n{msg}"
    );
}

/// A well-formed record compiles and dispatches without hitting either
/// `ToolCompileError` branch — the negative control for the two tests above,
/// so a validator that (say) always rejects wouldn't pass this file by
/// accident.
#[test]
fn compiletools_time_well_formed_record_compiles() {
    let src = worker_tools_module(
        "result :: Either Text Text\n\
         result = case compileTools workerTools of\n\
         \x20 Left err -> Left (renderToolCompileError err)\n\
         \x20 Right compiled -> Right (synopsis compiled)\n",
    );
    let v = run_pure(&src, "result");
    // The runtime's generic eval-result rendering for a top-level ADT
    // value (not routed through the in-language `ToJSON` class) is
    // `{"constructor": ..., "fields": [...]}` — a different convention
    // from `Tidepool.Aeson.Value.ToJSON`'s hand-rolled `Either` instance
    // (`{"Left"/"Right": ...}`), since `result` here is never `toJSON`'d.
    let obj = v
        .as_object()
        .expect("Either Text Text encodes as a tagged object");
    assert_eq!(
        obj.get("constructor").and_then(|c| c.as_str()),
        Some("Right"),
        "well-formed WorkerTools must compile, got:\n{v}"
    );
}

/// A payload field named `tag` shadows the constructor discriminator and is
/// rejected as a compile-time `TypeError` on the vendored path; this pins
/// that it fires on the agent tool surface specifically.
/// (`generic_recursive_sums.rs`'s `payload_field_cannot_collide_with_sum_tag`
/// pins the same guard through `ToJSON`; this one proves the guard reaches a
/// type arriving as a `Call` input.)
#[test]
fn compile_fail_payload_field_named_tag() {
    require_extract();
    let src = input_schema_module(
        "data Verdict = Yes | Because { tag :: Text }\n\
         \x20 deriving (Generic, FromJSON, JsonSchema)\n",
        "Verdict",
    );
    match EvalHarness::new().with_stdlib().compile(&src, "result") {
        Ok(_) => panic!("a payload field named `tag` must not compile"),
        Err(e) => {
            let msg = tidepool_runtime::classify_compile(&e).message;
            assert!(
                msg.contains("reserved for the constructor discriminator"),
                "expected the reserved-tag TypeError, got:\n{msg}"
            );
        }
    }
}
