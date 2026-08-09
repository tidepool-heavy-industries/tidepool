//! The turn engine — the conversation driver that turns a `ModelProvider`
//! into a resident-session turn loop, plus the hole classification and
//! transcript store that the golden path threads through.
//!
//! # The golden path this drives
//!
//! A node is forced → a resident session bootstraps → the turn engine drives
//! the calling model: assemble a prompt (transcript prefix + system framing),
//! call the provider, extract the LAST fenced ```haskell block, compile + run
//! it as a resident turn. The turn either COMPLETES (the node is done) or
//! SUSPENDS at an `AskWith` — which the engine classifies from the request
//! payload:
//!
//! - `{typedSite, fork:true}` → `runLLMTurnFork`: PARK. The parent stays
//!   suspended; a child answerer node is registered (transcript forked at the
//!   checkpoint) and, once forced, drives its own turn loop to produce a typed
//!   answer that `run_child`s against the parent and resumes it.
//! - `{typedSite}` (no fork) → `runLLMTurn`: the SAME model answers in
//!   context by evaluating `resume expr :: T`.
//! - `AskUserWith spec` (own constructor, answerer-only) → `askUserRaw`:
//!   OPERATOR routing — the typed `FormSpec` renders as a form; the
//!   operator's submission resumes the turn.
//!
//! Answer validation is GHC end-to-end: an ill-typed `resume expr` fails to
//! compile, and the compiler error is fed back verbatim as the retry prompt —
//! the continuation is never consumed by a bad attempt.
//!
//! # What this module does NOT own
//!
//! The event log, node tree, and registry lifecycle live in [`crate::forcing`]
//! / [`crate::registry`]; the engine calls them. Compilation lives in
//! [`crate::compile`]. The provider boundary is [`crate::provider`]. The web
//! protocol / SSE is `tidepool-web`. This module is the glue that sequences
//! them into a turn loop.

use std::path::PathBuf;
use std::sync::Arc;

use serde_json::Value as Json;
use tidepool_eval::value::Value;
use tidepool_repr::DataConTable;

use crate::compile::AsksSidecar;
use crate::provider::{
    DynModelProvider, Message, ProviderError, ReasoningItem, Role, StreamSink, TurnRequest,
    TurnResponse, Usage,
};
use crate::tree::FanBadge;

/// How a suspended request routes — decoded from its constructor name +
/// payload: `Ask`, `RunLLMTurn`, and `Finalize`
/// are each their own GADT/union-tag, see [`classify_hole`].
///
/// `PartialEq` only (not `Eq`): [`HoleRouting::AskUser`] carries a
/// [`crate::selfharness::operator::FormSpec`], which derives `PartialEq` but
/// not `Eq` (the frozen `operator.rs` contract) — do not add `Eq` back there.
#[derive(Debug, Clone, PartialEq)]
pub enum HoleRouting {
    /// `runLLMTurn @T` — the same calling model answers in context.
    RunLLMTurn { site: u32, ty: Option<String> },
    /// Park a suspension into a bounded fan-out with a join. Produced by two
    /// sources that share this routing: the `Fork` effect (`ForkWith` →
    /// `fan: None`, one child; `ForkAllWith` → `fan: Some(_)`, N children —
    /// `Tidepool.Fork`'s `fork`/`forkAll`), and the general Agent stack's
    /// `runLLMTurn` fork payload (`runLLMTurnFork`/`runLLMTurnFanout`). `ty` is
    /// the RENDERED answer type: the element type `T` for a plain fork, the
    /// LIST type `[T]` for a fanout (`engine::strip_list_type` recovers `T`).
    /// `prompts` carries the per-child prompt text, one per fanout child, in
    /// declaration order (empty for a plain fork).
    Fork {
        site: u32,
        ty: Option<String>,
        fan: Option<FanBadge>,
        prompts: Vec<String>,
    },
    /// `dialogAsk ui` — operator routing; the `Ui` value renders in the form
    /// pane.
    Dialog { ui: Json },
    /// `askUserRaw spec` — a typed form
    /// suspends to a HUMAN OPERATOR, routed by CONSTRUCTOR NAME
    /// (`AskUserWith`), not JSON-key probing. `spec` is the decoded
    /// [`crate::selfharness::operator::FormSpec`] the operator gate renders.
    AskUser {
        spec: crate::selfharness::operator::FormSpec,
    },
    /// `finalize @T x` — an Agent turn hands a
    /// typed value UP to the parent `runLLMTurn` hole and TERMINATES its own
    /// turn loop, rather than resuming in context like [`HoleRouting::RunLLMTurn`]
    /// does. Its own GADT/union-tag (`Finalize`/`FinalizeWith`), decoded by
    /// [`classify_hole`] from `FinalizeWith`'s own wire shape (`Con(_, [site
    /// :: Int, value])`) — NOT `AskWith`'s `typedSite` field. `site`/`ty`
    /// mirror `RunLLMTurn`'s shape (same asks.json sidecar lookup); the raw
    /// finalized VALUE is not carried here (it crosses in-heap, may be
    /// non-serializable) — the caller recovers it from the original
    /// suspended request `Value`.
    Finalize { site: u32, ty: Option<String> },
    /// A plain `ask schema prompt` (structured operator elicitation) or an
    /// unrecognized payload — operator routing with the raw payload attached.
    Ask { payload: Json },
}

/// A classified suspension: the routing plus the human-facing prompt text.
#[derive(Debug, Clone)]
pub struct ClassifiedHole {
    pub routing: HoleRouting,
    pub prompt: String,
}

/// Decode a suspended request `Value` into a [`ClassifiedHole`] — the ONE
/// shared classify path `Ask`, `RunLLMTurn`, and `Finalize` all go through.
/// Each is its own GADT/union-tag, so this
/// dispatches on the request Con's CONSTRUCTOR NAME first, then decodes that
/// constructor's own wire shape:
///
/// - `RunLLMTurnWith` (prompt, payload) — the `typedSite`/`fork`/`fan`/
///   `prompts` payload shape carried on the `RunLLMTurn` constructor: `fork` →
///   [`HoleRouting::Fork`] (the general Agent stack's `runLLMTurnFork`/
///   `runLLMTurnFanout`), else [`HoleRouting::RunLLMTurn`]. `asks` resolves
///   `typedSite` to its rendered answer type.
/// - `ForkWith` (site, brief) / `ForkAllWith` (site, prompts) — the `Fork`
///   effect (`Tidepool.Fork`'s `fork`/`forkAll`), routed by CONSTRUCTOR NAME
///   to [`HoleRouting::Fork`] (`fan: None` for one child, `fan: Some(_)` for a
///   batch). The site id is a constructor field, not a payload key; `asks`
///   resolves it to the rendered answer type the same way.
/// - `FinalizeWith` (site, value) — [`HoleRouting::Finalize`]. The VALUE field
///   is never JSON-decoded here (it crosses in-heap, may be non-serializable —
///   e.g. a closure); only the leading `Int` site id is. `asks` resolves the
///   site the same way as `RunLLMTurn`. The raw value itself is recovered from
///   the original request `Value` by the caller (`Harness` retains it), not
///   through this JSON-shaped `ClassifiedHole`.
/// - `AskUserWith` (spec) — a real constructor arm, routed by CONSTRUCTOR
///   NAME (no JSON-key probing): the request's sole field decodes as a
///   [`crate::selfharness::operator::FormSpec`] → [`HoleRouting::AskUser`].
///   A malformed spec (the decode fails) falls through to the plain-Ask
///   fallback below instead of hanging, so a bad payload surfaces loudly at
///   the driver.
/// - `AskWith` (prompt, payload) — plain [`HoleRouting::Ask`] (a structured
///   `ask schema prompt`). A Dialog hole is never PRODUCED here — no
///   `payload` probe routes to it — though the `HoleRouting::Dialog` variant
///   and its consumers still exist.
/// - anything else (an unrecognized Con) — treated as a bare Ask with an empty
///   prompt/`Null` payload, same fallback `decode_askwith` always had.
pub fn classify_hole(request: &Value, table: &DataConTable, asks: &AsksSidecar) -> ClassifiedHole {
    let hole = match con_name(request, table) {
        Some("RunLLMTurnWith") => {
            let (prompt, payload) = decode_prompt_payload(request, table);
            ClassifiedHole {
                routing: classify_runllmturn_payload(&payload, asks),
                prompt,
            }
        }
        Some("FinalizeWith") => {
            let (site, ty) = decode_finalize_site(request, table, asks);
            ClassifiedHole {
                routing: HoleRouting::Finalize { site, ty },
                prompt: String::new(),
            }
        }
        Some("ForkWith") => {
            let (site, brief) = decode_fork_one(request, table);
            ClassifiedHole {
                routing: HoleRouting::Fork {
                    site,
                    ty: asks.type_of(site).map(str::to_string),
                    fan: None,
                    prompts: Vec::new(),
                },
                prompt: brief,
            }
        }
        Some("ForkAllWith") => {
            let (site, prompts) = decode_fork_all(request, table);
            ClassifiedHole {
                prompt: prompts.join("\n"),
                routing: HoleRouting::Fork {
                    site,
                    ty: asks.type_of(site).map(str::to_string),
                    fan: Some(FanBadge::Exact {
                        n: prompts.len() as u32,
                    }),
                    prompts,
                },
            }
        }
        Some("AskUserWith") => match decode_askuser_spec(request, table) {
            Some(spec) => ClassifiedHole {
                routing: HoleRouting::AskUser { spec },
                prompt: String::new(),
            },
            None => {
                let (prompt, payload) = decode_askwith(request, table);
                ClassifiedHole {
                    routing: HoleRouting::Ask { payload },
                    prompt,
                }
            }
        },
        _ => {
            let (prompt, payload) = decode_askwith(request, table);
            ClassifiedHole {
                routing: HoleRouting::Ask { payload },
                prompt,
            }
        }
    };
    tracing::info!(routing = ?hole.routing, prompt = %hole.prompt, "suspension classified");
    hole
}

/// Decode an `AskUserWith`-shaped request (`Con(_, [spec])`) into a
/// [`crate::selfharness::operator::FormSpec`]. `None` on any shape/decode
/// mismatch — the caller falls back to the plain-Ask routing.
///
/// TWO Haskell surfaces ride this one constructor, and they are told apart
/// by decode rather than by a second routing arm: `askUser @T`
/// (`Tidepool.Form`) sends a bare
/// [`crate::selfharness::operator::FormShape`] — exactly the JSON
/// `selfharness::operator`'s module docs specify — and the de-advertised
/// applicative builder sends a flat `{"fields": [...]}` spec. A bare shape
/// cannot decode as a `FormSpec` (`fields` is required there), so trying the
/// flat wire first is unambiguous; a shape is lifted into
/// [`crate::selfharness::operator::FormSpec::shape`] for the gate to render.
fn decode_askuser_spec(
    request: &Value,
    table: &DataConTable,
) -> Option<crate::selfharness::operator::FormSpec> {
    use crate::selfharness::operator::{FormShape, FormSpec};

    let Value::Con(_, fields) = request else {
        return None;
    };
    let field = fields.first()?;
    let json = tidepool_runtime::value_to_json(field, table, 0);
    if let Ok(spec) = serde_json::from_value::<FormSpec>(json.clone()) {
        return Some(spec);
    }
    let shape: FormShape = serde_json::from_value(json).ok()?;
    Some(FormSpec {
        fields: Vec::new(),
        shape: Some(shape),
    })
}

/// The `typedSite`/`fork`/`fan`/`prompts` payload classification a
/// `RunLLMTurnWith` request carries — factored out of [`classify_hole`] so
/// this shape is documented once rather than at every call site.
fn classify_runllmturn_payload(payload: &Json, asks: &AsksSidecar) -> HoleRouting {
    let site = payload.get("typedSite").and_then(Json::as_u64).unwrap_or(0) as u32;
    let ty = asks.type_of(site).map(str::to_string);
    if payload.get("fork").and_then(Json::as_bool).unwrap_or(false) {
        let fan = payload
            .get("fan")
            .and_then(Json::as_u64)
            .map(|n| FanBadge::Exact { n: n as u32 });
        let prompts = payload
            .get("prompts")
            .and_then(Json::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        HoleRouting::Fork {
            site,
            ty,
            fan,
            prompts,
        }
    } else {
        HoleRouting::RunLLMTurn { site, ty }
    }
}

/// Strip one layer of `[...]` from a rendered type string — the FANOUT
/// element-type derivation: a `runLLMTurnFanout` site's recorded
/// asks.json type is the LIST type `[T]`; the harness recovers the
/// per-child element type `T` by stripping the outer brackets. `None` if
/// `ty` isn't bracket-wrapped.
pub fn strip_list_type(ty: &str) -> Option<&str> {
    ty.strip_prefix('[').and_then(|s| s.strip_suffix(']'))
}

/// The request `Value`'s constructor name, when it is a `Con` — `None` for
/// any other `Value` shape (a suspended Ask/RunLLMTurn/Finalize request is
/// always a `Con`, by construction of their `*With` GADT constructors).
fn con_name<'a>(request: &Value, table: &'a DataConTable) -> Option<&'a str> {
    let Value::Con(con_id, _) = request else {
        return None;
    };
    table.name_of(*con_id)
}

/// Pull `(prompt, payload)` out of a `Con(_, [prompt :: Text, payload ::
/// Value])`-shaped request — the wire shape `AskWith` and `RunLLMTurnWith`
/// both use (this is the shared decode `Ask` and `RunLLMTurn` consume; only
/// the constructor NAME differs, checked by the caller via [`con_name`]
/// before dispatching here). `Finalize`'s wire shape is different (its value
/// field is never JSON-decoded) and has its own decode, [`decode_finalize_site`].
fn decode_prompt_payload(request: &Value, table: &DataConTable) -> (String, Json) {
    let Value::Con(_, fields) = request else {
        return (String::new(), Json::Null);
    };
    let prompt = fields
        .first()
        .map(|p| tidepool_runtime::value_to_json(p, table, 0))
        .and_then(|j| j.as_str().map(str::to_string))
        .unwrap_or_default();
    let payload = fields
        .get(1)
        .map(|p| tidepool_runtime::value_to_json(p, table, 0))
        .unwrap_or(Json::Null);
    (prompt, payload)
}

/// Pull `(site, ty)` out of a `FinalizeWith`-shaped request (`Con(_, [site ::
/// Int, value])`). Only the leading `Int` site id is JSON-decoded — the
/// value field crosses in-heap and is deliberately left untouched here (see
/// [`classify_hole`]'s doc); `asks` resolves the site to its rendered type
/// the same way [`classify_runllmturn_payload`] does.
fn decode_finalize_site(
    request: &Value,
    table: &DataConTable,
    asks: &AsksSidecar,
) -> (u32, Option<String>) {
    let Value::Con(_, fields) = request else {
        return (0, None);
    };
    let site = fields
        .first()
        .map(|p| tidepool_runtime::value_to_json(p, table, 0))
        .and_then(|j| j.as_u64())
        .unwrap_or(0) as u32;
    let ty = asks.type_of(site).map(str::to_string);
    (site, ty)
}

/// Pull `(site, brief)` out of a `ForkWith`-shaped request (`Con(_, [site ::
/// Int, brief :: Text])`) — a single `fork @T brief` suspension. The site id
/// selects the recorded answer type; the brief is the child's task text.
fn decode_fork_one(request: &Value, table: &DataConTable) -> (u32, String) {
    let Value::Con(_, fields) = request else {
        return (0, String::new());
    };
    let site = fields
        .first()
        .map(|p| tidepool_runtime::value_to_json(p, table, 0))
        .and_then(|j| j.as_u64())
        .unwrap_or(0) as u32;
    let brief = fields
        .get(1)
        .map(|p| tidepool_runtime::value_to_json(p, table, 0))
        .and_then(|j| j.as_str().map(str::to_string))
        .unwrap_or_default();
    (site, brief)
}

/// Pull `(site, prompts)` out of a `ForkAllWith`-shaped request (`Con(_, [site
/// :: Int, prompts :: [Text]])`) — a `forkAll @T briefs` suspension. `prompts`
/// is the per-child brief list in declaration order; a non-`Text` element is
/// silently dropped, so a shorter result than the `fan` count is a cardinality
/// error the caller catches (`Harness::answer_fanout`).
fn decode_fork_all(request: &Value, table: &DataConTable) -> (u32, Vec<String>) {
    let Value::Con(_, fields) = request else {
        return (0, Vec::new());
    };
    let site = fields
        .first()
        .map(|p| tidepool_runtime::value_to_json(p, table, 0))
        .and_then(|j| j.as_u64())
        .unwrap_or(0) as u32;
    let prompts = fields
        .get(1)
        .map(|p| tidepool_runtime::value_to_json(p, table, 0))
        .and_then(|j| j.as_array().cloned())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    (site, prompts)
}

/// Pull the prompt (Text) and payload (JSON object) out of an `AskWith` Con.
/// Mirrors `tidepool_repl::ask::extract_ask_request`, but keeps the payload as
/// structured JSON (not opaque) so the engine can classify it. A non-`AskWith`
/// Con (or any other request shape) decodes to an empty prompt / `Null`
/// payload — the same fallback [`classify_hole`] uses for an unrecognized Con.
fn decode_askwith(request: &Value, table: &DataConTable) -> (String, Json) {
    if con_name(request, table) != Some("AskWith") {
        return (String::new(), Json::Null);
    }
    decode_prompt_payload(request, table)
}

// ---------------------------------------------------------------------------
// Prompt assembly
// ---------------------------------------------------------------------------

/// The framing that teaches the model its ONE tool (an eval block) and how to
/// answer a hole. Deliberately terse — the API is the prompt.
pub const SYSTEM_FRAMING: &str = "\
You drive a resident Haskell (tidepool) session. Your ONLY output that runs is a \
single fenced ```haskell code block containing ONE expression of type `M a` — the \
same effect-monad surface as tidepool eval (verbs like `run`, `grepGlob`, `readGlob`, \
`llm`, `runLLMTurn`, `runLLMTurnFork`). The LAST such block in your \
reply is compiled and run against the session; prose around it is ignored by the runtime.\n\
\n\
Verbs return typed DATA you unwrap — failures are `Either`, NOT exceptions. PREFER a typed \
verb over shelling out with `run` (there is a verb for files, http, git, kv):\n\
- `run :: Text -> M (Either ExecError Proc)` — a shell command. `Right p <- run \"cmd\"`, then \
`p.stdout` / `p.exitCode` (record-dot). `run` does NOT return `Text`.\n\
- `readFile :: FilePath -> M (Either FsError Text)` / `writeFile :: FilePath -> Text -> M (Either FsError ())` \
(mkdir-p) — read/write ONE file (do NOT `run \"cat/awk …\"` or `run \"… > file\"`); process text with `lines`, \
`T.` fns. `readGlob :: Text -> M [FileRead]` for a glob (each `.path`, `.contents`). To edit, `update path old new`.\n\
- `grepGlob :: Text -> FilePath -> M (Either FsError [Hit])` — regex FIRST, path-glob SECOND (each `.path`/`.line`/`.text`).\n\
- `httpGet :: Text -> M (Either HttpError Value)` — HTTP GET → JSON (do NOT `run \"curl …\"`); \
extract with `v ^? key \"f\" . _Int` / `_String`.\n\
- Git (not `run \"git …\"`): `gitLog`, `gitStatus`, `gitShow \"HEAD\" :: M (Either GitError Commit)` \
(`.sha`/`.subject`/`.author`/`.files`).\n\
- KV store: `kvSet key (toJSON v)`, `kvGet key :: M (Maybe Value)`.\n\
- JSON: `object [\"k\" .= v]`, `toJSON`; extract with `v ^? key \"f\" . _String`.\n\
Unwrap an `Either` via `Right x <- verb …` or `verb … >>= liftEither`. Avoid `read`-parsing — \
use the typed verbs + optics.\n\
\n\
To SUSPEND for a typed answer, evaluate `runLLMTurn @T \"prompt\"` (answered in your \
own context) or `runLLMTurnFork @T \"prompt\"` (answered by a forked sub-agent).\n\
\n\
The session PERSISTS across turns like GHCi: a value you bind with `x <- …` this turn \
— a `runLLMTurn`/`runLLMTurnFork` answer — is a LIVE binding in your NEXT turn, so \
you can BRANCH on it. A branching dialogue is \
exactly that: bind a choice, then next turn pick the follow-up from it. E.g. turn 1 \
`lane <- runLLMTurn @Text \"which lane — alpha or beta?\"`; turn 2 reads `lane` and \
presents the form for that branch. Bind what you'll need later instead of re-asking.\n\
\n\
When you are answering a HOLE, your block's value IS the answer: write `resume expr` \
where `expr :: T` matches the hole's declared type. `resume` is the identity here — \
`resume Approve` just yields `Approve`.";

/// The answerer's `resume :: a -> M a` helper — the identity, so a fork/return
/// answerer writes `resume expr` and the block's value is `expr`. Injected into
/// the answerer turn's `helpers` so `resume` is in scope.
pub const RESUME_HELPER: &str = "resume :: a -> M a\nresume = pure";

/// Assemble the provider request from a transcript and per-node framing. The
/// system message is always first; the transcript follows in order.
///
/// `framing` is the node's own system message — the self-iterating harness's
/// per-loop answerer session passes `render`'s output here (wired end-to-end
/// so the distilled `render` conditional actually reaches the model, not just
/// the observational `CycleOutcome`). `None` falls back to the default
/// [`SYSTEM_FRAMING`] that teaches the full eval surface — the shape every
/// ordinary Agent node still uses.
pub fn assemble_request(
    transcript: &[Message],
    max_tokens: Option<u32>,
    framing: Option<&str>,
) -> TurnRequest {
    let mut messages = Vec::with_capacity(transcript.len() + 1);
    messages.push(Message {
        role: Role::System,
        content: framing.unwrap_or(SYSTEM_FRAMING).to_string(),
        reasoning_items: Vec::new(), // the synthetic system message never carries any
    });
    messages.extend_from_slice(transcript);
    TurnRequest {
        messages,
        max_tokens,
    }
}

/// A names-only type-shape line to append to a hole card, or an empty string
/// when there is nothing to show: no table (the caller couldn't reach one —
/// see call sites), or [`crate::uiof::type_synopsis`] degraded all the way to
/// the bare type name (already stated elsewhere in the card, so repeating it
/// here would add nothing). See `plans/post-restart/dev/hole-card-type-synopsis.md`:
/// names only, never an invented/partial shape.
fn type_shape_line(ty: &str, table: Option<&DataConTable>) -> String {
    match table.map(|t| crate::uiof::type_synopsis(t, ty)) {
        Some(synopsis) if synopsis != ty => format!("Its shape: `{synopsis}`\n\n"),
        _ => String::new(),
    }
}

/// Render a hole card as a user-turn message: the prompt plus a `resume :: T`
/// signature the answerer fills. This is what a fork/return answerer sees as
/// its task. `table` is the [`DataConTable`] the hole's type was classified
/// from (when the caller has one in hand) — used only to render a names-only
/// shape synopsis, never to invent field types.
pub fn hole_card(prompt: &str, ty: Option<&str>, table: Option<&DataConTable>) -> String {
    match ty {
        Some(ty) => {
            let shape = type_shape_line(ty, table);
            format!(
                "A parent computation is suspended and needs a typed answer.\n\n\
                 {prompt}\n\n\
                 Answer by evaluating `resume expr` where:\n\n\
                 ```haskell\nresume :: {ty} -> M {ty}\n```\n\n\
                 {shape}Your ```haskell block's value must be of type `{ty}`."
            )
        }
        None => format!(
            "A parent computation is suspended and needs an answer.\n\n{prompt}\n\n\
             Answer by evaluating `resume expr` in a ```haskell block."
        ),
    }
}

/// The hole card for a SELF-ITERATING-HARNESS answerer (scoped `[AskUser,
/// Finalize]` stack), which can ONLY resolve a hole via `finalize @T` — it has
/// no `resume` (the generic [`hole_card`] tells the model to write `resume
/// expr`, which does not compile against this scoped stack and costs a needless
/// compile-error/retry round). This card names
/// `finalize @T` directly.
/// `imports` are the author modules the turn already imports
/// (`crate::harness::AnswerContract`) — say so, because a model that believes
/// `{ty}` is out of scope stops trying to build one and finalizes whatever does
/// compile instead (e.g. a `Text`/tuple) rather than the real type.
///
/// Prescribes bare `finalize @{ty} value`, with no outer `:: M {ty}`
/// annotation — the earlier "annotate the WHOLE expression" wording was a
/// stopgap for the ambiguous-`a0` defect; `__anchor` (`template_turn_for`)
/// fixed that at the source, and
/// `finalize_type_pinning::bare_finalize_with_no_annotation_compiles_when_pinned`
/// proves the bare shape compiles for a pinned `Finalize T` row. The
/// annotated form still compiles too (a relaxation, not a prohibition) — it
/// is simply no longer necessary to prescribe.
///
/// `table` is the [`DataConTable`] the hole's answer type was resolved from,
/// when the caller has one in hand — used only to render a names-only shape
/// synopsis (see [`hole_card`]), never to invent field types.
pub fn answerer_hole_card(
    prompt: &str,
    ty: Option<&str>,
    imports: &[String],
    table: Option<&DataConTable>,
) -> String {
    let ty = ty.unwrap_or("A");
    let shape = type_shape_line(ty, table);
    let scope = if imports.is_empty() {
        String::new()
    } else {
        format!(
            " `{ty}` is already in scope (this turn imports {}) — and this turn's \
             row only admits `finalize @{ty}`, so a wrong-typed value is a compile \
             error naming the row, not a value that silently crosses. Construct a \
             real `{ty}`, do not substitute a tuple or `Text`.",
            imports.join(", ")
        )
    };
    format!(
        "The loop needs a typed answer of type `{ty}`.\n\n\
         {prompt}\n\n\
         {shape}Answer by evaluating `finalize @{ty} value` in a single \
         ```haskell block — this ends your turn and hands the value back to the \
         loop.{scope} (To gather operator input first, evaluate `askUser @T` for \
         a type in scope; bind its result, then `finalize`.)"
    )
}

// ---------------------------------------------------------------------------
// Eval-block extraction
// ---------------------------------------------------------------------------

/// Extract the LAST fenced ```haskell block from a model reply. Returns `None`
/// when the reply has no fenced haskell block (a pure-prose turn — the engine
/// treats that as "no eval to run", loops or completes per policy).
///
/// Matches ```haskell / ```hs (case-insensitive) opening fences; a bare ```
/// fence is NOT treated as haskell (avoids grabbing a shell/text block). The
/// LAST block wins so a model can think in earlier blocks and commit in the
/// final one.
pub fn extract_last_haskell_block(reply: &str) -> Option<String> {
    let mut blocks = Vec::new();
    let mut lines = reply.lines().peekable();
    while let Some(line) = lines.next() {
        let trimmed = line.trim_start();
        let lang = trimmed
            .strip_prefix("```")
            .map(|l| l.trim().to_ascii_lowercase());
        let is_haskell_open = matches!(lang.as_deref(), Some("haskell") | Some("hs"));
        if is_haskell_open {
            let mut body = String::new();
            for inner in lines.by_ref() {
                if inner.trim_start().starts_with("```") {
                    break;
                }
                body.push_str(inner);
                body.push('\n');
            }
            let body = body.trim_end().to_string();
            if !body.is_empty() {
                blocks.push(body);
            }
        }
    }
    blocks.pop()
}

/// Split a model-written block into (imports, expression). A model sometimes
/// puts `import Foo` lines at the top of its ```haskell block; those are NOT
/// legal inside the templated `M a` EXPRESSION position, so they are peeled off
/// and routed to `template_haskell`'s `imports` field.
///
/// No implicit import is added here: `Tidepool.Form` is already
/// auto-imported by the turn preamble when `AskUser` is in the compiling
/// stack (`preamble::pragmas_and_imports`), and an Agent-stack turn (which
/// has no `AskUser`) must NOT get it force-imported — `Tidepool.Form` would
/// fail to resolve there (it depends on `askUserRaw`).
pub fn split_imports(block: &str) -> (String, String) {
    let mut imports: Vec<String> = Vec::new();
    let mut body = Vec::new();
    let mut in_body = false;
    for line in block.lines() {
        let trimmed = line.trim_start();
        if !in_body && trimmed.starts_with("import ") {
            // `import Qualified.Mod (names)` — keep everything after `import `.
            let rest = trimmed.trim_start_matches("import ").trim().to_string();
            if !rest.is_empty() {
                imports.push(rest);
            }
        } else if !in_body && trimmed.is_empty() {
            // Blank lines before the body are skipped (don't start the body).
        } else {
            in_body = true;
            body.push(line);
        }
    }
    (imports.join("\n"), body.join("\n"))
}

// ---------------------------------------------------------------------------
// Include-path resolution + effect-stack config
// ---------------------------------------------------------------------------

/// Everything the engine needs to compile + run turns: the extract binary, the
/// include search paths (prelude + effects module + optional project lib), the
/// effect decls, the Ask tag, and the effect-row names.
pub struct EngineConfig {
    pub extract_bin: String,
    pub include: Vec<PathBuf>,
    pub effect_names: Vec<String>,
    /// The full [`EffectDecl`]s this config was built from — the SOURCE of both
    /// the turn preamble (the effect verb helpers) and `effect_names`. Carried
    /// so a turn templates its preamble against the config's ACTUAL effect set,
    /// not a hardcoded Agent stack: the self-iterating harness's answerer
    /// session is built from [`crate::selfharness::driver::answerer_decls`]
    /// (gui + finalize only), and its turns must NOT advertise verbs
    /// (`run`/`runLLMTurn`/…) it cannot compile.
    pub decls: Vec<tidepool_mcp::EffectDecl>,
    /// The suspend THRESHOLD: the tag (position) of the FIRST interposed
    /// effect (`Ask`|`RunLLMTurn`|`Finalize`) in [`Self::decls`] — every effect
    /// at or past this tag suspends the machine rather than dispatching to a
    /// handler. Named for what it is, the suspend threshold, not just
    /// `Ask`, since `RunLLMTurn`/`Finalize` share the same suspend path.
    pub suspend_tag: u64,
    /// The stdlib include dir this config was built from (`include[0]`,
    /// carried separately so a caller building a NARROWER decls list against
    /// the same stdlib — e.g. the self-iterating harness's outer `Eff
    /// '[RunLLMTurn]` compile — doesn't have to reverse-engineer it out
    /// of `include`).
    pub prelude_dir: PathBuf,
    /// The project-lib dir this config was built from, if any (see
    /// `prelude_dir`'s doc).
    pub project_lib: Option<PathBuf>,
    /// The config's own default effects-module dir (the entry
    /// [`Self::from_decls`] pushed into [`Self::include`], at the config's
    /// default row). Tracked separately — not just "the last entry of
    /// `include`" — because callers routinely append MORE dirs to `include`
    /// after construction (a project source dir, `examples/harness` in
    /// tests): [`Self::turn_target`] finds and replaces THIS specific entry
    /// for a pinned turn, so it stays correct regardless of what else got
    /// appended later.
    effects_dir: PathBuf,
    /// Per-node turn cap — a model that never emits a runnable/answering block
    /// is stopped after this many turns (config, default small).
    pub max_turns: u32,
    /// Per-CHILD turn cap for a `runLLMTurnFanout` answerer: each of the N
    /// children gets this budget independently, so one
    /// pathological child can't consume the whole node's turn allowance the
    /// way a single shared cap would. Plain fork/return-control answerers
    /// still use `max_turns`.
    pub max_child_turns: u32,
    /// Per-turn output-token cap handed to the provider.
    pub max_tokens: Option<u32>,
    /// The context-window budget (in tokens) the runtime watches for MID-LOOP
    /// emergency compaction (self-iterating-harness). DISTINCT from
    /// [`Self::max_tokens`], which is the ~2048
    /// per-turn *output* cap — this is the whole answerer session's
    /// accumulated *context* size, summed across its turns. At ~80% of this,
    /// the driver forces a compact-to-text summary of the answerer transcript
    /// and replaces its context IN PLACE so the loop continues under a smaller
    /// window ([`crate::selfharness::driver::SelfHarnessDriver`]'s
    /// `maybe_compact_answerer`). `None` disables the emergency trigger
    /// (structural compaction alone).
    pub context_window_tokens: Option<u32>,
}

/// Default context-window budget the emergency-compaction trigger watches.
/// A representative small-model context window;
/// distinct from [`EngineConfig::max_tokens`] (the 2048 per-turn output cap).
/// The driver's `compaction_threshold_percent` (~80%) is taken against THIS.
pub const DEFAULT_CONTEXT_WINDOW_TOKENS: u32 = 128_000;

/// The Agent turn engine's decl list: `standard_decls()` (base9 + Ask +
/// RunLLMTurn) with `Finalize` appended last —
/// its own interposed effect/tag, sharing `Ask`/`RunLLMTurn`'s suspend path
/// (see `jit_machine::drive_effect_loop`'s `suspend_tag` threshold: every tag
/// from the FIRST interposed effect onward suspends, so appending a third
/// interposed effect here needs no further Rust-side dispatch change).
/// `Agent` isn't a literal Haskell type anywhere — it's this decl list, used
/// wherever an Agent turn (an Agent node driven by `Harness::run_to_hole_or_done`,
/// including the self-iterating-harness's nested Agent sessions answering a
/// `runLLMTurn` hole via `finalize`) is compiled.
fn agent_decls() -> Vec<tidepool_mcp::EffectDecl> {
    let mut decls = tidepool_mcp::standard_decls();
    decls.push(tidepool_mcp::finalize_decl());
    decls
}

impl EngineConfig {
    /// The canonical effect stack's decls + ask tag + effect names, resolving
    /// the extract binary from `TIDEPOOL_EXTRACT` (falling back to
    /// `tidepool-extract` on PATH) and the include paths from `prelude_dir`
    /// (the stdlib `haskell/lib`) plus a freshly-materialized effects module.
    ///
    /// `project_lib` (a `.tidepool/lib` dir) is appended when present so evals
    /// can `import Library` verbs; `None` for the bare stack.
    pub fn standard(
        prelude_dir: PathBuf,
        project_lib: Option<PathBuf>,
    ) -> Result<Self, EngineError> {
        Self::from_decls(agent_decls(), prelude_dir, project_lib)
    }

    /// A config for unit tests that never compile a turn: no extract binary,
    /// no includes, and an effects dir that is never read. `effect_names` is
    /// the one field such a test does read — `Harness::flush_effects` maps an
    /// effect's stack tag through it.
    #[cfg(test)]
    pub(crate) fn inert(effect_names: Vec<String>) -> Self {
        EngineConfig {
            extract_bin: "unused".to_string(),
            include: Vec::new(),
            effect_names,
            decls: Vec::new(),
            suspend_tag: 0,
            prelude_dir: PathBuf::from("."),
            project_lib: None,
            effects_dir: PathBuf::from("."),
            max_turns: 1,
            max_child_turns: 1,
            max_tokens: None,
            context_window_tokens: None,
        }
    }

    /// Build a config for an EXPLICIT decls list — not necessarily the full
    /// Agent stack `standard()` hardcodes. The self-iterating harness's outer
    /// driver uses this for its `Eff '[RunLLMTurn]`-only compile
    /// (`vec![tidepool_mcp::runllmturn_decl()]`), so `Harness = M` resolves
    /// to that literal single-effect row rather than the full Agent stack.
    pub fn from_decls(
        decls: Vec<tidepool_mcp::EffectDecl>,
        prelude_dir: PathBuf,
        project_lib: Option<PathBuf>,
    ) -> Result<Self, EngineError> {
        // `suspend_tag` is the suspend THRESHOLD: the index of the first
        // interposed effect. For the full Agent stack that's `Ask`; for the
        // answerer stack it's `AskUser`; for a narrower stack (e.g.
        // RunLLMTurn-only) there is no `Ask`/`AskUser` entry at all, so fall
        // back to the first of the other interposed effects — found by name,
        // not by position, since none of them is necessarily the list's last
        // entry.
        let suspend_tag = decls
            .iter()
            .position(|d| {
                matches!(
                    d.type_name,
                    "Ask" | "AskUser" | "RunLLMTurn" | "Fork" | "Finalize"
                )
            })
            .unwrap_or(decls.len()) as u64;
        let effect_names = decls.iter().map(|d| d.type_name.to_string()).collect();
        let effects_dir = tidepool_mcp::ensure_effects_module(&decls)
            .map_err(|e| EngineError::Setup(format!("materialize effects module: {e}")))?;
        let mut include = vec![prelude_dir.clone()];
        if let Some(lib) = &project_lib {
            include.push(lib.clone());
        }
        include.push(effects_dir.clone());
        let extract_bin =
            std::env::var("TIDEPOOL_EXTRACT").unwrap_or_else(|_| "tidepool-extract".to_string());
        Ok(EngineConfig {
            extract_bin,
            include,
            effect_names,
            decls,
            suspend_tag,
            prelude_dir,
            project_lib,
            effects_dir,
            max_turns: 8,
            max_child_turns: 4,
            max_tokens: Some(2048),
            context_window_tokens: Some(DEFAULT_CONTEXT_WINDOW_TOKENS),
        })
    }

    /// The promoted-list effect-stack string (`'[Console, KV, …, Finalize
    /// NoAnswer]`) for `template_haskell` at the config's default row — every
    /// decl including Ask, each parameterized effect applied to its
    /// [`EffectDecl::default_row_args`]. Routes through
    /// [`tidepool_mcp::build_effect_stack_type`] (not a bare join of
    /// `effect_names`, which carries no type arguments and would emit a
    /// bare `Finalize` — a kind error in the promoted list, since every
    /// sibling entry has kind `* -> *`).
    fn effect_stack_type(&self) -> String {
        tidepool_mcp::build_effect_stack_type(&self.decls)
    }

    /// Resolve ONE turn's compile target: the include search path and the
    /// promoted effect-row string it must be compiled against — both derived
    /// from the SAME [`tidepool_mcp::RowArgs`], so they cannot name different
    /// rows.
    ///
    /// `finalize`, when `Some((ty, imports))`, instantiates the row's
    /// `Finalize` entry at `ty` (the hole's answer type) — importing
    /// `imports` (the author modules that define it) — and materializes ITS
    /// OWN effects-module dir via [`tidepool_mcp::ensure_effects_module_at`],
    /// swapping it in for [`Self::effects_dir`] wherever it sits in
    /// [`Self::include`] (found by VALUE, not by position — a caller may have
    /// appended more dirs after construction, e.g. a project source dir).
    /// The dir is content-addressed on the generated source, so repeats of
    /// the same answer type are free and two answer types can never be
    /// served each other's module.
    ///
    /// `None` keeps the config's own default row (`Finalize NoAnswer`) and
    /// `include` unchanged — the shape every turn that isn't answering a
    /// typed hole compiles against.
    pub fn turn_target(
        &self,
        finalize: Option<(&str, &[String])>,
    ) -> Result<TurnTarget, EngineError> {
        let Some((ty, imports)) = finalize else {
            return Ok(TurnTarget {
                include: self.include.clone(),
                stack: self.effect_stack_type(),
            });
        };
        let row = tidepool_mcp::RowArgs::at("Finalize", [ty]).importing(imports.iter().cloned());
        let effects_dir = tidepool_mcp::ensure_effects_module_at(&self.decls, &row)
            .map_err(|e| EngineError::Setup(format!("materialize effects module: {e}")))?;
        let mut include = self.include.clone();
        match include.iter().position(|p| p == &self.effects_dir) {
            Some(pos) => include[pos] = effects_dir,
            None => include.push(effects_dir),
        }
        Ok(TurnTarget {
            include,
            stack: tidepool_mcp::build_effect_stack_type_at(&self.decls, &row),
        })
    }
}

/// One turn's compile target — the include search path and the promoted
/// effect-row string, resolved together by [`EngineConfig::turn_target`] so
/// they can never disagree.
#[derive(Debug, Clone)]
pub struct TurnTarget {
    pub include: Vec<PathBuf>,
    pub stack: String,
}

// ---------------------------------------------------------------------------
// Turn templating
// ---------------------------------------------------------------------------

/// Wrap a model-written `M a` block as a full templated module the extract can
/// compile, with optional extra `helpers` (e.g. the answerer's `resume`) and
/// `imports` (e.g. `Tidepool.Ui`). The result is `toJSON`'d — the JSON-render
/// contract of a NORMAL turn (its terminal value is displayed).
///
/// `stack` is the promoted effect-row string this turn compiles against —
/// callers answering a typed `finalize` hole resolve it (and the matching
/// include dir) via [`EngineConfig::turn_target`], so `finalize`'s pin lives
/// in the ROW (`Member (Finalize T) stack`), not in a shimmed/shadowed
/// binding: the ordinary [`tidepool_mcp::build_preamble`] is used unconditionally.
pub fn template_turn(
    cfg: &EngineConfig,
    stack: &str,
    code: &str,
    imports: &str,
    helpers: &str,
) -> String {
    template_turn_for(&cfg.decls, stack, code, imports, helpers)
}

/// Like [`template_turn`], but for an EXPLICIT decls list rather than the
/// hardcoded Agent stack — the self-iterating harness driver's outer `Eff
/// '[RunLLMTurn]` compile needs a preamble matching ITS OWN (narrower)
/// decls, not the Agent's. `stack` must be rendered from the SAME `decls` —
/// see [`EngineConfig::turn_target`].
///
/// `stack` pinned to a real (non-`NoAnswer`) `Finalize T` entry routes
/// through [`tidepool_mcp::template_haskell_anchored`] instead of the plain
/// [`tidepool_mcp::template_haskell`] — see [`finalize_pin_active`]'s doc for
/// why, and `template_haskell_anchored`'s doc (`eval_prep.rs`) for the
/// mechanism. Every other row (no `Finalize` entry, or the `NoAnswer`
/// default) compiles exactly as before.
pub fn template_turn_for(
    decls: &[tidepool_mcp::EffectDecl],
    stack: &str,
    code: &str,
    imports: &str,
    helpers: &str,
) -> String {
    let preamble = tidepool_mcp::build_preamble(decls, false);
    if finalize_pin_active(stack) {
        tidepool_mcp::template_haskell_anchored(
            &preamble, stack, code, imports, helpers, None, None,
        )
    } else {
        tidepool_mcp::template_haskell(&preamble, stack, code, imports, helpers, None, None)
    }
}

/// Whether `stack` (the promoted row string a turn compiles against, e.g.
/// `'[AskUser, Finalize Decision]`) pins `Finalize` to a REAL author type —
/// `false` for the uninhabited default `Finalize NoAnswer` (a turn not
/// currently answering a typed hole — every non-answerer turn, and an
/// answerer turn before its first `AnswerContract` is set) or a row with no
/// `Finalize` entry at all (the general Agent stack never carries one).
///
/// A presence check, not a type extraction: `template_turn_for` only needs a
/// boolean (route this turn's `_r` through the anchor, or don't), never the
/// concrete `T` itself — `EngineConfig::turn_target`'s caller already has `T`
/// in hand were it needed for anything else, so there is nothing to recover
/// from this string, only whether to flip the anchor on.
fn finalize_pin_active(stack: &str) -> bool {
    stack.contains("Finalize ") && !stack.contains("Finalize NoAnswer")
}

/// Wrap an ANSWERER block as a module whose `result` returns the RAW value —
/// NOT `toJSON`'d. A fork/return answerer's block is `resume expr :: M T`, and
/// the value fed to the parent's continuation must be the raw `T` (an `Int`,
/// an ADT — whatever the hole's type is), because `runLLMTurn`/`Fork`'s
/// `unsafeCoerce` relabels the SAME runtime bytes back to `T`. `template_turn`'s
/// `toJSON _r` would instead hand back an Aeson `Value` (a `Number`, an
/// `Object`), which the parent's `T`-typed continuation then case-traps on.
///
/// So this emits `result :: Eff stack a; result = <block>` — GHC infers `a`
/// from the block's `resume :: T -> M T` (or the polymorphic identity), and the
/// value stays in its native `T` representation.
pub fn template_answer_turn(
    cfg: &EngineConfig,
    code: &str,
    imports: &str,
    helpers: &str,
) -> String {
    let preamble = tidepool_mcp::build_preamble(&cfg.decls, false);
    let stack = cfg.effect_stack_type();

    let mut out = String::new();
    // Insert user imports right before the `default` decl (same insertion point
    // template_haskell uses), else append the preamble whole.
    if imports.trim().is_empty() {
        out.push_str(&preamble);
    } else {
        let insert = preamble.find("default (Int").unwrap_or(preamble.len());
        out.push_str(&preamble[..insert]);
        for imp in imports.lines().map(str::trim).filter(|l| !l.is_empty()) {
            out.push_str("import ");
            out.push_str(imp);
            out.push('\n');
        }
        out.push_str(&preamble[insert..]);
    }
    out.push_str("-- [user]\n");
    if !helpers.trim().is_empty() {
        out.push_str(helpers);
        if !helpers.ends_with('\n') {
            out.push('\n');
        }
        out.push('\n');
    }
    // The RAW-result binding: no toJSON, no paginate. No explicit type
    // signature — GHC infers the result type from the block (the answerer's
    // `resume :: T -> M T` helper fixes `T` when the hole type is known). The
    // block is embedded verbatim inside an explicit let-bracket (same
    // layout-suspension trick as template_haskell) so unindented multi-line
    // blocks stay valid. `stack` is unused in the raw binding (the type is
    // inferred), so silence it deliberately.
    let _ = &stack;
    out.push_str("result = let {\n __b =\n");
    out.push_str(code);
    if !code.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(" } in __b\n");
    out
}

/// Wrap a value-plane BIND turn (`x <- e`) as a session module whose `__result`
/// runs the bind statement and yields the bound name — the shape
/// `compile_session_turn` expects (target `__result`, `Eff <stack> _` so GHC
/// infers the bound type from the block). Mirrors the repl's `wrap_bind_source`;
/// the harness preamble/effect-stack differ, the `__result`/session-bind contract
/// is identical. `stmt` is the raw `x <- e` block; `binder` is the bound name.
pub fn template_session_bind(
    cfg: &EngineConfig,
    stmt: &str,
    binder: &str,
    imports: &str,
    helpers: &str,
) -> String {
    let preamble = tidepool_mcp::build_preamble(&cfg.decls, false);
    let stack = cfg.effect_stack_type();

    let mut out = String::new();
    // Insert user imports right before the `default` decl (the same insertion
    // point `template_haskell`/`template_answer_turn` use).
    if imports.trim().is_empty() {
        out.push_str(&preamble);
    } else {
        let insert = preamble.find("default (Int").unwrap_or(preamble.len());
        out.push_str(&preamble[..insert]);
        for imp in imports.lines().map(str::trim).filter(|l| !l.is_empty()) {
            out.push_str("import ");
            out.push_str(imp);
            out.push('\n');
        }
        out.push_str(&preamble[insert..]);
    }
    out.push_str("-- [user]\n");
    if !helpers.trim().is_empty() {
        out.push_str(helpers);
        if !helpers.ends_with('\n') {
            out.push('\n');
        }
        out.push('\n');
    }
    out.push_str(&format!("__result :: Eff {stack} _\n"));
    out.push_str("__result = do {\n");
    push_braced_stmt(&mut out, stmt);
    out.push_str(&format!(" ; pure {binder}\n }}\n"));
    out
}

/// Embed a turn statement verbatim inside an explicit `do { }` block (mirrors the
/// repl's `push_braced_stmt`): a bare `let x = e` gets explicit `let { }`
/// brackets so an unindented continuation is legal; every other statement is
/// embedded as-is. Explicit brackets suspend the layout algorithm so multi-line
/// / quasiquote payloads keep byte fidelity.
fn push_braced_stmt(out: &mut String, turn_text: &str) {
    let trimmed = turn_text.trim_start();
    let let_rest = trimmed
        .strip_prefix("let")
        .filter(|r| r.starts_with(|c: char| c.is_whitespace()));
    match let_rest {
        Some(rest) if !rest.trim_start().starts_with('{') => {
            out.push_str("let {");
            out.push_str(rest);
            if !rest.ends_with('\n') {
                out.push('\n');
            }
            out.push_str(" }\n");
        }
        _ => {
            out.push_str(turn_text);
            if !turn_text.ends_with('\n') {
                out.push('\n');
            }
        }
    }
}

/// Build the EXPR turn template a [`tidepool_runtime::session::TurnRequest`]
/// carries: [`template_turn`] with a one-line `{{TURN}}` placeholder standing
/// in for the real block (the verdict — and so which template applies — is
/// not known until `run_turn` returns), with its `-- [user-lines] S:E`
/// annotation repaired to the range the REAL block would occupy, and its
/// compile target renamed to `__result` (see [`retarget_result_binder`]).
///
/// `template_haskell` computes the `[user-lines]` annotation's END line from
/// the spliced code's own newline count, so a one-line placeholder yields a
/// CORRECT start line (it depends only on content BEFORE the placeholder) but
/// a WRONG end line for any block that isn't itself exactly one line. Left
/// unrepaired, every GHC diagnostic the corrective-retry loop feeds back for a
/// multi-line block would cite the wrong line range.
///
/// Byte-exactness is the contract these repairs buy: splicing `block` back in
/// via [`tidepool_runtime::session::render_template`] must reproduce
/// [`template_turn`] called directly on `block`, up to the deliberate binder
/// rename — see `expr_turn_template_byte_identical_to_template_turn` for the
/// pin.
///
/// Requires `block` not to end in `\n` — `{{TURN}}`'s splice is a dumb
/// VERBATIM substitution (no newline normalization of its own), so this
/// relies on `template_haskell`'s fixed one-newline-after-code padding
/// (baked in once, at build time, from the one-line placeholder) being the
/// SAME padding a trailing-newline-free `block` needs; a `block` that already
/// ended in `\n` would double up. `run_block`'s only source of turn text,
/// `extract_last_haskell_block`, always `trim_end()`s what it extracts, so
/// this holds for every real caller.
pub fn expr_turn_template(
    cfg: &EngineConfig,
    stack: &str,
    block: &str,
    imports: &str,
    helpers: &str,
) -> String {
    let placeholder = template_turn(cfg, stack, "{{TURN}}", imports, helpers);
    let repaired = repair_user_lines_end(&placeholder, content_line_count(block));
    retarget_result_binder(&repaired)
}

/// Build a BIND/BINDDISCARD turn template: the same preamble/imports/helpers/
/// `__result` scaffolding [`template_session_bind`] builds around a REAL
/// statement, but with the bare `{{TURN_STMT}}` marker placed DIRECTLY —
/// deliberately NOT through [`push_braced_stmt`].
///
/// `push_braced_stmt` (and `render_template`'s own `place_turn_stmt`, which
/// mirrors it) ends its output in exactly one trailing newline, ADDING one
/// when its input doesn't already have one — correct for a REAL statement,
/// but the 13-character marker token `"{{TURN_STMT}}"` itself never ends in
/// `\n`, so routing the MARKER through the same function bakes an extra
/// trailing newline into the template. At splice time `render_template`
/// substitutes the marker with `place_turn_stmt`'s OWN newline-terminated
/// output, so that baked-in newline becomes a genuine duplicate — a blank
/// line between the turn statement and `; pure …` that `template_session_bind`
/// called directly on the same text never produces. Placing the marker bare
/// leaves supplying the separator entirely to the splice's own normalization,
/// which is what makes the two agree — see
/// `bind_template_byte_identical_to_template_session_bind` for the pin.
///
/// `binder` is `"{{BINDERS}}"` for a real bind (its names are comma-joined
/// and spliced in later) or the literal `"()"` for a discarding bind
/// (`TemplateSelector::BindDiscard` — no `{{BINDERS}}` placeholder at all,
/// "splicing no binder").
pub fn session_bind_template(
    cfg: &EngineConfig,
    binder: &str,
    imports: &str,
    helpers: &str,
) -> String {
    let preamble = tidepool_mcp::build_preamble(&cfg.decls, false);
    let stack = cfg.effect_stack_type();

    let mut out = String::new();
    if imports.trim().is_empty() {
        out.push_str(&preamble);
    } else {
        let insert = preamble.find("default (Int").unwrap_or(preamble.len());
        out.push_str(&preamble[..insert]);
        for imp in imports.lines().map(str::trim).filter(|l| !l.is_empty()) {
            out.push_str("import ");
            out.push_str(imp);
            out.push('\n');
        }
        out.push_str(&preamble[insert..]);
    }
    out.push_str("-- [user]\n");
    if !helpers.trim().is_empty() {
        out.push_str(helpers);
        if !helpers.ends_with('\n') {
            out.push('\n');
        }
        out.push('\n');
    }
    out.push_str(&format!("__result :: Eff {stack} _\n"));
    out.push_str("__result = do {\n");
    out.push_str("{{TURN_STMT}}");
    out.push_str(&format!(" ; pure {binder}\n }}\n"));
    out
}

/// Rename the compiled EXPR module's top-level binder from `result`
/// (`template_haskell`'s fixed name — shared with the stateless eval server,
/// so that function can't change it) to `__result`, the name every OTHER
/// template `run_turn` carries here already uses:
/// [`template_session_bind`]'s fixed `__result`, and `run_turn`'s own default
/// target when no `--target` is supplied. `--target` is ONE flag for the
/// whole `--turn` spawn, shared across whichever verdict GHC actually picks
/// — a caller supplying a `result`-targeted expr template alongside an
/// `__result`-targeted bind template has no single `--target` value that
/// works for both. Confirmed empirically against the built extract:
/// `--target result` against a module that only declares `__result` fails
/// with `translateModule: exported top-level binding 'result' not found`.
/// So every compiling template here shares `__result`, and `run_turn` is
/// called with no `--target` override at all.
///
/// Scoped to the text AFTER the `[user-lines]` marker (`template_haskell`'s
/// own `result ::`/`result = do` lines always follow it) so a `result ::`/
/// `result =` line inside caller-supplied `helpers`/`imports` (which precede
/// the marker) is never touched.
fn retarget_result_binder(src: &str) -> String {
    const MARKER: &str = " -- [user-lines] ";
    let Some(marker_pos) = src.find(MARKER) else {
        return src.to_string();
    };
    let (head, tail) = src.split_at(marker_pos);
    let tail = tail
        .replacen("\nresult :: Eff ", "\n__result :: Eff ", 1)
        .replacen("\nresult = do\n", "\n__result = do\n", 1);
    format!("{head}{tail}")
}

/// The 1-based inclusive line count `code` occupies once embedded — mirrors
/// `tidepool_mcp::eval_prep`'s `template_haskell_impl` end-line computation
/// exactly (an empty block is 1 line; a trailing newline doesn't count as an
/// extra line).
pub(crate) fn content_line_count(code: &str) -> usize {
    if code.is_empty() {
        1
    } else if code.ends_with('\n') {
        code.matches('\n').count()
    } else {
        code.matches('\n').count() + 1
    }
}

/// Rewrite a `-- [user-lines] S:E` annotation's END line to
/// `start + content_lines - 1`, leaving the START line untouched. `src` is
/// expected to contain exactly one such marker (as every [`template_turn`]
/// output does); a src without one is returned unchanged rather than panicking
/// — a caller error surfaces downstream as an unrepaired annotation, not here.
fn repair_user_lines_end(src: &str, content_lines: usize) -> String {
    const MARKER: &str = " -- [user-lines] ";
    let Some(pos) = src.find(MARKER) else {
        return src.to_string();
    };
    let after = &src[pos + MARKER.len()..];
    let Some(colon) = after.find(':') else {
        return src.to_string();
    };
    let start_str = &after[..colon];
    let Ok(start) = start_str.parse::<usize>() else {
        return src.to_string();
    };
    let rest = &after[colon + 1..];
    let end_digits = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    let real_end = start + content_lines - 1;

    let mut out = String::with_capacity(src.len());
    out.push_str(&src[..pos + MARKER.len()]);
    out.push_str(start_str);
    out.push(':');
    out.push_str(&real_end.to_string());
    out.push_str(&rest[end_digits..]);
    out
}

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("engine setup failed: {0}")]
    Setup(String),
    #[error("provider call failed: {0}")]
    Provider(#[from] ProviderError),
    #[error("turn compile failed:\n{0}")]
    Compile(String),
    #[error("resident turn failed: {0}")]
    Run(String),
    #[error("the model produced no runnable ```haskell block after {turns} turns")]
    NoBlock { turns: u32 },
}

/// The outcome of driving ONE model turn against a resident session.
pub enum TurnOutcome {
    /// The compiled block ran to completion; the node is done.
    Completed { rendered: String },
    /// The compiled block suspended at an `AskWith`; the hole is classified and
    /// the machine's continuation id is `hole`.
    Suspended {
        hole: String,
        classified: ClassifiedHole,
        /// The compiled turn's table — kept so an in-context/fork answer's
        /// value can be bridged against the SAME constructor set the parent
        /// suspended with.
        table: DataConTable,
    },
    /// The model replied with no runnable block — the caller decides whether to
    /// loop (feed a nudge) or stop.
    NoBlock { reply: String },
}

/// One assistant turn's provider result plus its extracted block (if any).
pub struct DrivenTurn {
    pub reply: String,
    pub usage: Usage,
    /// The turn's reasoning-summary ("thinking"), when the provider surfaced one.
    pub reasoning: Option<String>,
    /// The encrypted reasoning items the provider surfaced for this turn (see
    /// [`ReasoningItem`]) — distinct from `reasoning` above.
    pub reasoning_items: Vec<ReasoningItem>,
    pub block: Option<String>,
}

/// Call the provider once with the assembled transcript and extract the block.
/// `framing` is the node's per-turn system message (see [`assemble_request`]);
/// `sink`, when `Some`, receives streaming deltas as the provider reads them.
pub async fn drive_model_turn(
    provider: &dyn DynModelProvider,
    transcript: &[Message],
    max_tokens: Option<u32>,
    framing: Option<&str>,
    sink: Option<StreamSink>,
) -> Result<DrivenTurn, EngineError> {
    let req = assemble_request(transcript, max_tokens, framing);
    let TurnResponse {
        text,
        usage,
        reasoning,
        reasoning_items,
    } = provider.complete_boxed(req, sink).await?;
    let block = extract_last_haskell_block(&text);
    if let Some(r) = reasoning.as_deref().filter(|r| !r.is_empty()) {
        tracing::info!("model reasoning:\n{r}");
    }
    Ok(DrivenTurn {
        reply: text,
        usage,
        reasoning,
        reasoning_items,
        block,
    })
}

/// Bridge a JSON answer (from a form submission or an in-context resume value)
/// to a Core `Value` against `table`, for feeding to `ResidentSession::resume`.
pub fn json_answer_to_value(answer: &Json, table: &DataConTable) -> Result<Value, EngineError> {
    use tidepool_bridge::ToCore;
    answer
        .to_value(table)
        .map_err(|e| EngineError::Run(format!("bridge answer to Value: {e}")))
}

/// Assemble N raw per-child answer `Value`s into a genuine `[T]` list
/// `Value` for a `runLLMTurnFanout` resume — the same "hand back the native
/// representation, not an Aeson wrapper" discipline a single fork's
/// `unsafeCoerce` relies on. `items` must
/// already be in declaration order; `table` only needs to know the
/// always-wired-in `:`/`[]` constructors (any `DataConTable` from the same
/// compiled program qualifies — `DataConId`s are stable hashes, not
/// table-local indices, so a table from a DIFFERENT compile of the same
/// program resolves to the same ids, exactly how a single fork's answer
/// already crosses from the child's compiled table into the parent's heap).
pub fn build_list_value(items: Vec<Value>, table: &DataConTable) -> Result<Value, EngineError> {
    let nil_id = tidepool_bridge::get_resilient(table, "[]", 0).ok_or_else(|| {
        EngineError::Run("build_list_value: no [] constructor in table".to_string())
    })?;
    let cons_id = tidepool_bridge::get_resilient(table, ":", 2).ok_or_else(|| {
        EngineError::Run("build_list_value: no : constructor in table".to_string())
    })?;
    let mut result = Value::Con(nil_id, vec![]);
    for item in items.into_iter().rev() {
        result = Value::Con(cons_id, vec![item, result]);
    }
    Ok(result)
}

/// Shared handle to a provider, so the engine and its forked answerers all use
/// the same signed-in client.
pub type SharedProvider = Arc<dyn DynModelProvider>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{Message, Role};

    fn user(content: &str) -> Message {
        Message {
            role: Role::User,
            content: content.to_string(),
            reasoning_items: Vec::new(),
        }
    }

    /// A `Some(framing)` becomes the request's System message verbatim,
    /// NOT the default `SYSTEM_FRAMING` — the self-iterating harness's
    /// `render` output must reach the model as-is.
    #[test]
    fn assemble_request_uses_framing_as_system_message() {
        let framing = "RENDERED: you are in Deciding mode, loop 3.";
        let transcript = [user("answer me")];
        let req = assemble_request(&transcript, Some(2048), Some(framing));

        assert_eq!(req.messages[0].role, Role::System);
        assert_eq!(
            req.messages[0].content, framing,
            "the framing must be the System message verbatim"
        );
        assert_ne!(
            req.messages[0].content, SYSTEM_FRAMING,
            "a Some(framing) must OVERRIDE the default SYSTEM_FRAMING"
        );
        // The transcript follows the system message in order.
        assert_eq!(req.messages[1].content, "answer me");
    }

    /// A `None` framing falls back to the default full-surface `SYSTEM_FRAMING`
    /// — the shape every ordinary Agent node still uses.
    #[test]
    fn assemble_request_none_framing_falls_back_to_default() {
        let req = assemble_request(&[user("hi")], Some(2048), None);
        assert_eq!(req.messages[0].role, Role::System);
        assert_eq!(req.messages[0].content, SYSTEM_FRAMING);
    }

    // -- hole card type synopsis --------------------------------------------

    use tidepool_repr::{DataCon, DataConId};

    fn nullary_dc(id: u64, name: &str, tag: u32, type_name: &str) -> DataCon {
        DataCon {
            id: DataConId(id),
            name: name.to_string(),
            tag,
            rep_arity: 0,
            field_bangs: vec![],
            qualified_name: Some(format!("{type_name}.{name}")),
            type_name: type_name.to_string(),
        }
    }

    fn record_table() -> DataConTable {
        let mut table = DataConTable::new();
        let dc = DataCon {
            id: DataConId(1),
            name: "Contribution".to_string(),
            tag: 1,
            rep_arity: 3,
            field_bangs: vec![],
            qualified_name: Some("Contribution.Contribution".to_string()),
            type_name: "Contribution".to_string(),
        };
        table.insert(dc.clone());
        table.set_field_labels(
            dc.id,
            vec![
                "addedIdeas".to_string(),
                "draftDelta".to_string(),
                "advance".to_string(),
            ],
        );
        table
    }

    fn nullary_sum_table() -> DataConTable {
        let mut table = DataConTable::new();
        table.insert(nullary_dc(3, "Abort", 3, "Verdict"));
        table.insert(nullary_dc(1, "Advance", 1, "Verdict"));
        table.insert(nullary_dc(2, "Hold", 2, "Verdict"));
        table
    }

    #[test]
    fn hole_card_renders_record_selector_names() {
        let table = record_table();
        let card = hole_card("answer this", Some("Contribution"), Some(&table));
        assert!(
            card.contains("Contribution { addedIdeas, draftDelta, advance }"),
            "{card}"
        );
    }

    #[test]
    fn answerer_hole_card_renders_nullary_sum_in_tag_order() {
        let table = nullary_sum_table();
        let card = answerer_hole_card("decide", Some("Verdict"), &[], Some(&table));
        assert!(card.contains("Advance | Hold | Abort"), "{card}");
    }

    /// Mutation-close the degrade path at the hole-card level: an unsupported
    /// shape (here, a type the table has no constructors for) must NOT add a
    /// partial/invented shape line — the card falls back to naming the type
    /// alone (already stated elsewhere in the card text).
    #[test]
    fn hole_card_unsupported_shape_adds_no_shape_line() {
        let table = DataConTable::new();
        let card = hole_card("answer this", Some("Mystery"), Some(&table));
        assert!(
            !card.contains("Its shape:"),
            "an unsupported shape must not render a shape line: {card}"
        );
    }

    #[test]
    fn hole_card_with_no_table_adds_no_shape_line() {
        let card = hole_card("answer this", Some("Contribution"), None);
        assert!(!card.contains("Its shape:"), "{card}");
    }

    /// Derived from the table, not hardcoded: a renamed field changes what
    /// the hole card shows.
    #[test]
    fn hole_card_shape_follows_table_field_rename() {
        let table = record_table();
        let card = hole_card("answer this", Some("Contribution"), Some(&table));
        assert!(card.contains("addedIdeas"), "{card}");

        let mut renamed = DataConTable::new();
        let dc = DataCon {
            id: DataConId(1),
            name: "Contribution".to_string(),
            tag: 1,
            rep_arity: 3,
            field_bangs: vec![],
            qualified_name: Some("Contribution.Contribution".to_string()),
            type_name: "Contribution".to_string(),
        };
        renamed.insert(dc.clone());
        renamed.set_field_labels(
            dc.id,
            vec![
                "ideasAdded".to_string(),
                "deltaDraft".to_string(),
                "advance".to_string(),
            ],
        );
        let renamed_card = hole_card("answer this", Some("Contribution"), Some(&renamed));
        assert!(renamed_card.contains("ideasAdded"), "{renamed_card}");
        assert!(!renamed_card.contains("addedIdeas"), "{renamed_card}");
    }

    #[test]
    fn finalize_pin_active_true_for_a_real_answer_type() {
        assert!(finalize_pin_active("'[AskUser, Finalize Decision]"));
        assert!(finalize_pin_active("'[Finalize (Int -> Int)]"));
    }

    #[test]
    fn finalize_pin_active_false_for_the_noanswer_sentinel() {
        assert!(!finalize_pin_active("'[AskUser, Finalize NoAnswer]"));
    }

    #[test]
    fn finalize_pin_active_false_with_no_finalize_entry() {
        assert!(!finalize_pin_active("'[Console, KV]"));
    }

    // -- run_turn template byte-identity ------------------------------------
    //
    // `expr_turn_template`/`session_bind_template` are what `run_block` hands
    // `run_turn` as its `--turn-template` sources. These pin that splicing
    // `block`/`stmt` back into the built template (via
    // `tidepool_runtime::session::render_template`, the same substitution
    // `--turn` performs at runtime) reproduces exactly what calling the
    // underlying builder directly on the real text would produce — the only
    // thing standing between the `[user-lines]`/binder repairs and a silently
    // wrong error-line mapping or a stray placeholder reaching GHC.
    //
    // Every fixture below deliberately does NOT end in `\n`: `run_block`'s
    // only source of turn text, `extract_last_haskell_block`, always
    // `trim_end()`s the fenced block it extracts, so a turn's raw text never
    // carries a trailing newline in production. That invariant is load-bearing
    // for `expr_turn_template` specifically — its `{{TURN}}` placement is a
    // dumb VERBATIM splice with no newline normalization of its own, so it
    // relies on `template_haskell`'s fixed one-newline-after-code padding
    // (computed once, at template-build time, from the one-line placeholder)
    // matching what a trailing-newline-free real block needs. A block that
    // DID end in `\n` would double up — not exercised here because it can't
    // reach this code from `run_block`.

    use tidepool_runtime::session::render_template;

    const STACK: &str = "'[]";

    fn multiline_block() -> &'static str {
        "let x = 1\n    y = 2\nin x + y"
    }

    fn quasiquote_block() -> &'static str {
        "let s = [fmt|line one\nline two|]\nin s"
    }

    #[test]
    fn expr_turn_template_byte_identical_to_template_turn() {
        let cfg = EngineConfig::inert(vec![]);
        for block in ["1 + 1", multiline_block(), quasiquote_block()] {
            let tmpl = expr_turn_template(&cfg, STACK, block, "", "");
            let spliced = render_template(&tmpl, block, &[]);
            // `expr_turn_template` deliberately renames the compiled binder
            // from `result` to `__result` (see `retarget_result_binder`) —
            // apply the same rename to the direct-call reference so the pin
            // still catches any OTHER divergence (imports, helpers,
            // `[user-lines]` repair, splice placement).
            let direct = retarget_result_binder(&template_turn(&cfg, STACK, block, "", ""));
            assert_eq!(spliced, direct, "block: {block:?}");
        }
    }

    #[test]
    fn expr_turn_template_targets_underscore_result_not_result() {
        let cfg = EngineConfig::inert(vec![]);
        let tmpl = expr_turn_template(&cfg, STACK, "1 + 1", "", "");
        assert!(tmpl.contains("\n__result :: Eff "));
        assert!(tmpl.contains("\n__result = do\n"));
        assert!(
            !tmpl.contains("\nresult :: Eff ") && !tmpl.contains("\nresult = do\n"),
            "the retargeted template must not also carry the original `result` binder:\n{tmpl}"
        );
    }

    #[test]
    fn bind_template_byte_identical_to_template_session_bind() {
        let cfg = EngineConfig::inert(vec![]);
        for (stmt, name) in [
            ("x <- pure 1", "x"),
            ("let y = 2", "y"),
            (multiline_block(), "z"),
        ] {
            let tmpl = session_bind_template(&cfg, "{{BINDERS}}", "", "");
            let spliced = render_template(&tmpl, stmt, &[name.to_string()]);
            let direct = template_session_bind(&cfg, stmt, name, "", "");
            assert_eq!(spliced, direct, "stmt: {stmt:?}");
        }
    }

    /// The discarding-bind template is [`session_bind_template`] with the
    /// literal binder `"()"` (no `{{BINDERS}}` placeholder at all — "splicing
    /// no binder", per `plans/one-spawn-turn-protocol.md`'s four-shape note) —
    /// pinned the same way as the bind template.
    #[test]
    fn binddiscard_template_byte_identical_to_template_session_bind_with_unit_binder() {
        let cfg = EngineConfig::inert(vec![]);
        let stmt = "_ <- pure ()";
        let tmpl = session_bind_template(&cfg, "()", "", "");
        assert!(
            !tmpl.contains("{{BINDERS}}"),
            "binddiscard template must splice no binder placeholder:\n{tmpl}"
        );
        let spliced = render_template(&tmpl, stmt, &[]);
        let direct = template_session_bind(&cfg, stmt, "()", "", "");
        assert_eq!(spliced, direct);
    }

    #[test]
    fn repair_user_lines_end_fixes_only_the_end_line() {
        let src = "head\n } in __b  -- [user-lines] 5:5\ntail";
        let out = repair_user_lines_end(src, 3);
        assert_eq!(out, "head\n } in __b  -- [user-lines] 5:7\ntail");
    }

    #[test]
    fn content_line_count_matches_template_haskell_impl_convention() {
        assert_eq!(content_line_count(""), 1);
        assert_eq!(content_line_count("a"), 1);
        assert_eq!(content_line_count("a\n"), 1);
        assert_eq!(content_line_count("a\nb"), 2);
        assert_eq!(content_line_count("a\nb\n"), 2);
    }
}
