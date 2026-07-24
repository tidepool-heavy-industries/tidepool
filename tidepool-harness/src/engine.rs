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
    DynModelProvider, Message, ProviderError, Role, TurnRequest, TurnResponse, Usage,
};

/// How a suspended `AskWith` request routes — decoded from its payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HoleRouting {
    /// `returnControl @T` — the same calling model answers in context.
    ReturnControl { site: u32, ty: Option<String> },
    /// `returnControlFork @T` — park; a forked child answerer produces the
    /// typed value.
    Fork { site: u32, ty: Option<String> },
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
/// [`HoleRouting::Fork`], `typedSite` alone → [`HoleRouting::ReturnControl`],
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
            HoleRouting::Fork { site, ty }
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
To SUSPEND for a typed answer, evaluate `returnControl @T \"prompt\"` (answered in your \
own context) or `returnControlFork @T \"prompt\"` (answered by a forked sub-agent). To \
elicit an operator form, `dialogAsk (toJSON someUi)` (needs `import Tidepool.Ui`).\n\
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
/// `imports` (e.g. `Tidepool.Ui`).
pub fn template_turn(cfg: &EngineConfig, code: &str, imports: &str, helpers: &str) -> String {
    let decls = tidepool_mcp::standard_decls();
    let preamble = tidepool_mcp::build_preamble(&decls, false);
    let stack = cfg.effect_stack_type();
    tidepool_mcp::template_haskell(&preamble, &stack, code, imports, helpers, None, None)
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
    pub block: Option<String>,
}

/// Call the provider once with the assembled transcript and extract the block.
pub async fn drive_model_turn(
    provider: &dyn DynModelProvider,
    transcript: &[Message],
    max_tokens: Option<u32>,
) -> Result<DrivenTurn, EngineError> {
    let req = assemble_request(transcript, max_tokens);
    let TurnResponse { text, usage } = provider.complete_boxed(req).await?;
    let block = extract_last_haskell_block(&text);
    Ok(DrivenTurn {
        reply: text,
        usage,
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

/// Shared handle to a provider, so the engine and its forked answerers all use
/// the same signed-in client.
pub type SharedProvider = Arc<dyn DynModelProvider>;
