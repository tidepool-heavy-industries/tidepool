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
//! - `{typedSite, fork:true}` → `returnControlFork`: PARK. The parent stays
//!   suspended; a child answerer node is registered (transcript forked at the
//!   checkpoint) and, once forced, drives its own turn loop to produce a typed
//!   answer that `run_child`s against the parent and resumes it.
//! - `{typedSite}` (no fork) → `returnControl`: the SAME model answers in
//!   context by evaluating `resume expr :: T`.
//! - `{ui}` → `dialogAsk`: OPERATOR routing — the `Ui` renders in the form
//!   pane; the operator's submission resumes the turn.
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
    DynModelProvider, Message, ProviderError, Role, StreamSink, TurnRequest, TurnResponse, Usage,
};
use crate::tree::FanBadge;

/// How a suspended `AskWith` request routes — decoded from its payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HoleRouting {
    /// `returnControl @T` — the same calling model answers in context.
    ReturnControl { site: u32, ty: Option<String> },
    /// `returnControlFork @T` (`fan: None`) — park; a forked child answerer
    /// produces the typed value. `returnControlFanout @T` (B1 widen,
    /// `fan: Some(_)`) — same payload-classification scheme (F3: "fan"
    /// joins additively), park; N thunk children each answer the element
    /// type. `ty` is the RENDERED answer type: the element type `T` for a
    /// plain fork, the LIST type `[T]` for a fanout (`engine::strip_list_type`
    /// recovers `T` from it). `prompts` carries the per-child prompt text,
    /// one per fanout child, in declaration order (empty for a plain fork).
    Fork {
        site: u32,
        ty: Option<String>,
        fan: Option<FanBadge>,
        prompts: Vec<String>,
    },
    /// `dialogAsk ui` — operator routing; the `Ui` value renders in the form
    /// pane.
    Dialog { ui: Json },
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

/// Decode a suspended `AskWith` request Value into a [`ClassifiedHole`].
///
/// The request is `Con(AskWith, [prompt :: Text, payload :: Value])`. The
/// payload object's fields decide the routing: `fork` + `typedSite` →
/// [`HoleRouting::Fork`] (additionally `fan` + `prompts` for a
/// `returnControlFanout` site), `typedSite` alone → [`HoleRouting::ReturnControl`],
/// `ui` → [`HoleRouting::Dialog`], else [`HoleRouting::Ask`]. `asks` resolves a
/// `typedSite` to its rendered answer type.
pub fn classify_hole(request: &Value, table: &DataConTable, asks: &AsksSidecar) -> ClassifiedHole {
    let (prompt, payload) = decode_askwith(request, table);
    let routing = if let Some(site) = payload.get("typedSite").and_then(Json::as_u64) {
        let site = site as u32;
        let ty = asks.type_of(site).map(str::to_string);
        if payload
            .get("fork")
            .and_then(Json::as_bool)
            .unwrap_or(false)
        {
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
            HoleRouting::ReturnControl { site, ty }
        }
    } else if let Some(ui) = payload.get("ui") {
        HoleRouting::Dialog { ui: ui.clone() }
    } else {
        HoleRouting::Ask { payload }
    };
    ClassifiedHole { routing, prompt }
}

/// Strip one layer of `[...]` from a rendered type string — the FANOUT
/// element-type derivation (F3: a `returnControlFanout` site's recorded
/// asks.json type is the LIST type `[T]`; the harness recovers the
/// per-child element type `T` by stripping the outer brackets). `None` if
/// `ty` isn't bracket-wrapped.
pub fn strip_list_type(ty: &str) -> Option<&str> {
    ty.strip_prefix('[').and_then(|s| s.strip_suffix(']'))
}

/// Pull the prompt (Text) and payload (JSON object) out of an `AskWith` Con.
/// Mirrors `tidepool_repl::ask::extract_ask_request`, but keeps the payload as
/// structured JSON (not opaque) so the engine can classify it.
fn decode_askwith(request: &Value, table: &DataConTable) -> (String, Json) {
    let Value::Con(con_id, fields) = request else {
        return (String::new(), Json::Null);
    };
    let name = table.name_of(*con_id).unwrap_or("<unknown>");
    if name != "AskWith" {
        return (String::new(), Json::Null);
    }
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

// ---------------------------------------------------------------------------
// Prompt assembly
// ---------------------------------------------------------------------------

/// The framing that teaches the model its ONE tool (an eval block) and how to
/// answer a hole. Deliberately terse — the API is the prompt.
pub const SYSTEM_FRAMING: &str = "\
You drive a resident Haskell (tidepool) session. Your ONLY output that runs is a \
single fenced ```haskell code block containing ONE expression of type `M a` — the \
same effect-monad surface as tidepool eval (verbs like `run`, `grepGlob`, `readGlob`, \
`llm`, `returnControl`, `returnControlFork`, `dialogAsk`). The LAST such block in your \
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
To SUSPEND for a typed answer, evaluate `returnControl @T \"prompt\"` (answered in your \
own context) or `returnControlFork @T \"prompt\"` (answered by a forked sub-agent).\n\
\n\
To elicit an operator form, PREFER a TYPED form (`import Tidepool.Form`): build a \
`Form a` applicatively and answer with `dialogForm form :: M (Either FormError a)` — \
e.g. `dialogForm ((,) <$> choiceField \"Lane\" [(\"a\", Alpha), (\"b\", Beta)] <*> textField \
\"Notes\")` renders ONE multi-field form and returns the decoded typed value (a missing/ \
ill-typed field is `Left FormError`). Field constructors: `textField`/`multilineField` \
(→ `Text`), `choiceField label [(key, value)]` (a radio → the selected value), \
`boolField` (→ `Bool`), `intField` (→ `Int`). For a raw untyped form, `dialogAsk ui` \
(`import Tidepool.Ui`) returns the raw `Value` submission — pass the `Ui` directly (e.g. \
`dialogAsk (textIn \"note?\" True)`), NOT `toJSON` of it.\n\
\n\
When you are answering a HOLE, your block's value IS the answer: write `resume expr` \
where `expr :: T` matches the hole's declared type. `resume` is the identity here — \
`resume Approve` just yields `Approve`.";

/// The answerer's `resume :: a -> M a` helper — the identity, so a fork/return
/// answerer writes `resume expr` and the block's value is `expr`. Injected into
/// the answerer turn's `helpers` so `resume` is in scope.
pub const RESUME_HELPER: &str = "resume :: a -> M a\nresume = pure";

/// Assemble the provider request from a transcript and framing. The system
/// message is always first; the transcript follows in order.
pub fn assemble_request(transcript: &[Message], max_tokens: Option<u32>) -> TurnRequest {
    let mut messages = Vec::with_capacity(transcript.len() + 1);
    messages.push(Message {
        role: Role::System,
        content: SYSTEM_FRAMING.to_string(),
    });
    messages.extend_from_slice(transcript);
    TurnRequest {
        messages,
        max_tokens,
    }
}

/// Render a hole card as a user-turn message: the prompt plus a `resume :: T`
/// signature the answerer fills. This is what a fork/return answerer sees as
/// its task.
pub fn hole_card(prompt: &str, ty: Option<&str>) -> String {
    match ty {
        Some(ty) => format!(
            "A parent computation is suspended and needs a typed answer.\n\n\
             {prompt}\n\n\
             Answer by evaluating `resume expr` where:\n\n\
             ```haskell\nresume :: {ty} -> M {ty}\n```\n\n\
             Your ```haskell block's value must be of type `{ty}`."
        ),
        None => format!(
            "A parent computation is suspended and needs an answer.\n\n{prompt}\n\n\
             Answer by evaluating `resume expr` in a ```haskell block."
        ),
    }
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
/// and routed to `template_haskell`'s `imports` field. `Tidepool.Ui` is always
/// added (harmless if unused) so `dialogAsk (card …)` resolves without the model
/// having to remember the import (`Tidepool.Form`, for typed `dialogForm`, is
/// likewise auto-imported by the turn preamble — see `preamble::pragmas_and_imports`).
/// `prose`/`code` are HIDDEN from this import: `Tidepool.Form` (also always
/// in scope) exports its own `prose`/`code` :: `Text -> Form ()` display
/// combinators of the same bare name, and both being unqualified-imported
/// would make either one an "Ambiguous occurrence" the instant a turn
/// actually references it. `Tidepool.Ui.prose`/`.code` stay reachable
/// qualified for a turn building a raw `dialogAsk` `Ui` tree by hand.
pub fn split_imports(block: &str) -> (String, String) {
    let mut imports = vec!["Tidepool.Ui hiding (prose, code)".to_string()];
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
    pub ask_tag: u64,
    /// Per-node turn cap — a model that never emits a runnable/answering block
    /// is stopped after this many turns (config, default small).
    pub max_turns: u32,
    /// Per-CHILD turn cap for a `returnControlFanout` answerer (B1 widen):
    /// each of the N children gets this budget independently, so one
    /// pathological child can't consume the whole node's turn allowance the
    /// way a single shared cap would. Plain fork/return-control answerers
    /// still use `max_turns`.
    pub max_child_turns: u32,
    /// Per-turn output-token cap handed to the provider.
    pub max_tokens: Option<u32>,
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
        let decls = tidepool_mcp::standard_decls();
        // Ask is last in standard_decls (index len-1).
        let ask_tag = (decls.len() as u64) - 1;
        let effect_names = decls.iter().map(|d| d.type_name.to_string()).collect();
        let effects_dir = tidepool_mcp::ensure_effects_module(&decls)
            .map_err(|e| EngineError::Setup(format!("materialize effects module: {e}")))?;
        let mut include = vec![prelude_dir];
        if let Some(lib) = project_lib {
            include.push(lib);
        }
        include.push(effects_dir);
        let extract_bin = std::env::var("TIDEPOOL_EXTRACT")
            .unwrap_or_else(|_| "tidepool-extract".to_string());
        Ok(EngineConfig {
            extract_bin,
            include,
            effect_names,
            ask_tag,
            max_turns: 8,
            max_child_turns: 4,
            max_tokens: Some(2048),
        })
    }

    /// The promoted-list effect-stack string (`'[Console, KV, …, Ask]`) for
    /// `template_haskell` — every decl including Ask.
    fn effect_stack_type(&self) -> String {
        let names: Vec<&str> = self.effect_names.iter().map(String::as_str).collect();
        if names.is_empty() {
            "'[]".to_string()
        } else {
            format!("'[{}]", names.join(", "))
        }
    }
}

// ---------------------------------------------------------------------------
// Turn templating
// ---------------------------------------------------------------------------

/// Wrap a model-written `M a` block as a full templated module the extract can
/// compile, with optional extra `helpers` (e.g. the answerer's `resume`) and
/// `imports` (e.g. `Tidepool.Ui`). The result is `toJSON`'d — the JSON-render
/// contract of a NORMAL turn (its terminal value is displayed).
pub fn template_turn(cfg: &EngineConfig, code: &str, imports: &str, helpers: &str) -> String {
    let decls = tidepool_mcp::standard_decls();
    let preamble = tidepool_mcp::build_preamble(&decls, false);
    let stack = cfg.effect_stack_type();
    tidepool_mcp::template_haskell(&preamble, &stack, code, imports, helpers, None, None)
}

/// Wrap an ANSWERER block as a module whose `result` returns the RAW value —
/// NOT `toJSON`'d. A fork/return answerer's block is `resume expr :: M T`, and
/// the value fed to the parent's continuation must be the raw `T` (an `Int`,
/// an ADT — whatever the hole's type is), because `returnControl`/`Fork`'s
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
    let decls = tidepool_mcp::standard_decls();
    let preamble = tidepool_mcp::build_preamble(&decls, false);
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
    let decls = tidepool_mcp::standard_decls();
    let preamble = tidepool_mcp::build_preamble(&decls, false);
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
    pub block: Option<String>,
}

/// Call the provider once with the assembled transcript and extract the block.
/// `sink`, when `Some`, receives streaming deltas as the provider reads them.
pub async fn drive_model_turn(
    provider: &dyn DynModelProvider,
    transcript: &[Message],
    max_tokens: Option<u32>,
    sink: Option<StreamSink>,
) -> Result<DrivenTurn, EngineError> {
    let req = assemble_request(transcript, max_tokens);
    let TurnResponse {
        text,
        usage,
        reasoning,
    } = provider.complete_boxed(req, sink).await?;
    let block = extract_last_haskell_block(&text);
    Ok(DrivenTurn {
        reply: text,
        usage,
        reasoning,
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
/// `Value` (F3's RAW-value rule for a `returnControlFanout` resume — the
/// same "hand back the native representation, not an Aeson wrapper"
/// discipline a single fork's `unsafeCoerce` relies on). `items` must
/// already be in declaration order; `table` only needs to know the
/// always-wired-in `:`/`[]` constructors (any `DataConTable` from the same
/// compiled program qualifies — `DataConId`s are stable hashes, not
/// table-local indices, so a table from a DIFFERENT compile of the same
/// program resolves to the same ids, exactly how a single fork's answer
/// already crosses from the child's compiled table into the parent's heap).
pub fn build_list_value(items: Vec<Value>, table: &DataConTable) -> Result<Value, EngineError> {
    let nil_id = tidepool_bridge::get_resilient(table, "[]", 0)
        .ok_or_else(|| EngineError::Run("build_list_value: no [] constructor in table".to_string()))?;
    let cons_id = tidepool_bridge::get_resilient(table, ":", 2)
        .ok_or_else(|| EngineError::Run("build_list_value: no : constructor in table".to_string()))?;
    let mut result = Value::Con(nil_id, vec![]);
    for item in items.into_iter().rev() {
        result = Value::Con(cons_id, vec![item, result]);
    }
    Ok(result)
}

/// Shared handle to a provider, so the engine and its forked answerers all use
/// the same signed-in client.
pub type SharedProvider = Arc<dyn DynModelProvider>;
