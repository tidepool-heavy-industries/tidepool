//! MCP (Model Context Protocol) server library for Tidepool.
//!
//! Wraps `tidepool-runtime` in an MCP server exposing `run_haskell`,
//! `compile_haskell`, and `eval` tools. Generic over effect handler stacks
//! via `TidepoolMcpServer<H>`.

use dyn_clone::{clone_trait_object, DynClone};
use rmcp::{
    model::*, service::RequestContext, ErrorData as McpError, RoleServer, ServerHandler, ServiceExt,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tidepool_bridge::{FromCore, ToCore};
use tidepool_runtime::DispatchEffect;
use tokio::io::{stdin, stdout};

// ---------------------------------------------------------------------------
// Effect metadata — lives next to the handler, discovered via trait
// ---------------------------------------------------------------------------

/// Static metadata describing a Haskell effect type.
///
/// Each effect handler that wants to participate in the MCP templating system
/// implements `DescribeEffect` to provide its Haskell-side type declaration.
#[derive(Debug, Clone, Copy)]
pub struct EffectDecl {
    /// Haskell GADT type name, e.g. `"Console"`.
    pub type_name: &'static str,
    /// Human-readable description of what this effect does.
    pub description: &'static str,
    /// Haskell GADT constructor declarations (one per line inside `data T a where`).
    pub constructors: &'static [&'static str],
    /// Extra Haskell type/function definitions emitted before the GADT.
    /// Use for supporting types (e.g. `data Lang = ...`) and helper functions.
    pub type_defs: &'static [&'static str],
    /// Thin curried helper definitions emitted after the `type M` alias.
    /// Each string is one or more lines of Haskell (signature + definition).
    pub helpers: &'static [&'static str],
}

/// Trait for effect handlers that can describe their Haskell-side type.
pub trait DescribeEffect {
    fn effect_decl() -> EffectDecl;
}

/// Trait for collecting effect declarations from an HList of handlers.
pub trait CollectEffectDecls {
    fn collect_decls() -> Vec<EffectDecl>;
}

impl CollectEffectDecls for frunk::HNil {
    fn collect_decls() -> Vec<EffectDecl> {
        Vec::new()
    }
}

impl<H, T> CollectEffectDecls for frunk::HCons<H, T>
where
    H: DescribeEffect,
    T: CollectEffectDecls,
{
    fn collect_decls() -> Vec<EffectDecl> {
        let mut decls = vec![H::effect_decl()];
        decls.extend(T::collect_decls());
        decls
    }
}

// ---------------------------------------------------------------------------
// Standard effect declarations
// ---------------------------------------------------------------------------

/// Console effect: print text output.
pub fn console_decl() -> EffectDecl {
    EffectDecl {
        type_name: "Console",
        description: "Print text output.",
        constructors: &["Print :: Text -> Console ()"],
        type_defs: &[],
        helpers: &[
            "say :: Text -> M ()\nsay = send . Print",
        ],
    }
}

/// Key-value store effect.
pub fn kv_decl() -> EffectDecl {
    EffectDecl {
        type_name: "KV",
        description:
            "Persistent key-value store. State survives across calls within one server session.",
        constructors: &[
            "KvGet :: Text -> KV (Maybe Text)",
            "KvSet :: Text -> Text -> KV ()",
            "KvDelete :: Text -> KV ()",
            "KvKeys :: KV [Text]",
        ],
        type_defs: &[],
        helpers: &[
            "kvGet :: Text -> M (Maybe Text)\nkvGet = send . KvGet",
            "kvSet :: Text -> Text -> M ()\nkvSet k v = send (KvSet k v)",
            "kvDel :: Text -> M ()\nkvDel = send . KvDelete",
            "kvKeys :: M [Text]\nkvKeys = send KvKeys",
        ],
    }
}

/// File I/O effect (sandboxed).
pub fn fs_decl() -> EffectDecl {
    EffectDecl {
        type_name: "Fs",
        description: "Read and write files (sandboxed to server working directory).",
        constructors: &[
            "FsRead :: Text -> Fs Text",
            "FsWrite :: Text -> Text -> Fs ()",
        ],
        type_defs: &[],
        helpers: &[
            "fsRead :: Text -> M Text\nfsRead = send . FsRead",
            "fsWrite :: Text -> Text -> M ()\nfsWrite f c = send (FsWrite f c)",
        ],
    }
}

/// Structural grep (ast-grep) effect.
pub fn sg_decl() -> EffectDecl {
    EffectDecl {
        type_name: "SG",
        description: concat!(
            "Structural code search and rewrite via ast-grep. ",
            "Use patterns with $VAR for single-node captures and $$$VAR for multi-node. ",
            "Paths are relative to server working directory.",
        ),
        type_defs: &[
            "data Lang = Rust | Python | TypeScript | JavaScript | Go | Java | C | Cpp | Haskell | Nix | Html | Css | Json | Yaml | Toml",
            "data Match = Match { mText :: Text, mFile :: Text, mLine :: Int, mVars :: [(Text, Text)], mReplacement :: Text }",
            "var :: Match -> Text -> Text",
            "var (Match _ _ _ vs _) k = case [v | (k', v) <- vs, k' == k] of { (x:_) -> x; _ -> \"\" }",
        ],
        constructors: &[
            "SgFind :: Lang -> Text -> [Text] -> SG [Match]",
            "SgPreview :: Lang -> Text -> Text -> [Text] -> SG [Match]",
            "SgReplace :: Lang -> Text -> Text -> [Text] -> SG Int",
        ],
        helpers: &[
            "sgFind :: Lang -> Text -> [Text] -> M [Match]\nsgFind l p fs = send (SgFind l p fs)",
            "sgPreview :: Lang -> Text -> Text -> [Text] -> M [Match]\nsgPreview l p r fs = send (SgPreview l p r fs)",
            "sgReplace :: Lang -> Text -> Text -> [Text] -> M Int\nsgReplace l p r fs = send (SgReplace l p r fs)",
        ],
    }
}

/// Http effect: fetch JSON from HTTP endpoints.
pub fn http_decl() -> EffectDecl {
    EffectDecl {
        type_name: "Http",
        description: "Fetch JSON from HTTP endpoints. Returns response body as Value.",
        constructors: &["HttpGet :: Text -> Http Value"],
        type_defs: &[],
        helpers: &[
            "httpGet :: Text -> M Value\nhttpGet = send . HttpGet",
        ],
    }
}

/// Ask effect: suspend execution to ask the calling LLM a question.
pub fn ask_decl() -> EffectDecl {
    EffectDecl {
        type_name: "Ask",
        description: "Suspend execution and ask the calling LLM a question. The LLM calls the resume tool with an answer, and execution continues.",
        constructors: &["Ask :: Text -> Ask Text"],
        type_defs: &[],
        helpers: &[
            "ask :: Text -> M Text\nask = send . Ask",
        ],
    }
}

/// All standard effects in canonical order.
pub fn standard_decls() -> Vec<EffectDecl> {
    vec![
        console_decl(),
        kv_decl(),
        fs_decl(),
        sg_decl(),
        http_decl(),
        ask_decl(),
    ]
}

// ---------------------------------------------------------------------------
// Request types
// ---------------------------------------------------------------------------

/// Request parameters for the `eval` tool.
///
/// Provide a Haskell do-block as a single string. The server wraps it in a
/// full module with the effect stack type, LANGUAGE pragmas, and imports.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct EvalRequest {
    /// Haskell do-notation code. Each line is indented into a do-block.
    /// Use `pure x` as the last line to return a value.
    /// Use `send (Constructor args)` to invoke effects.
    /// Accepts either a single string (preferred) or array of lines (legacy).
    #[serde(deserialize_with = "deserialize_code")]
    pub code: Vec<String>,
    /// Additional Haskell imports, one per line (e.g. "Data.List (sort)").
    /// Accepts a string (one import per line) or array of strings.
    #[serde(default, deserialize_with = "deserialize_string_or_vec")]
    pub imports: Vec<String>,
    /// Top-level helper definitions placed before the main do-block.
    /// Accepts a string (raw Haskell) or array of definition strings.
    #[serde(default, deserialize_with = "deserialize_string_or_vec")]
    pub helpers: Vec<String>,
    /// Optional JSON input injected as `input :: Aeson.Value` binding.
    #[serde(default)]
    pub input: Option<serde_json::Value>,
}

/// Deserialize `code` from either a string (split on newlines) or array of strings.
fn deserialize_code<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<String>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum StringOrVec {
        Str(String),
        Vec(Vec<String>),
    }
    match StringOrVec::deserialize(d)? {
        StringOrVec::Str(s) => Ok(s.lines().map(String::from).collect()),
        StringOrVec::Vec(v) => Ok(v),
    }
}

/// Deserialize from either a string (split on newlines, empty lines filtered) or array of strings.
fn deserialize_string_or_vec<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<String>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum StringOrVec {
        Str(String),
        Vec(Vec<String>),
    }
    match StringOrVec::deserialize(d)? {
        StringOrVec::Str(s) => Ok(s.lines().filter(|l| !l.trim().is_empty()).map(String::from).collect()),
        StringOrVec::Vec(v) => Ok(v),
    }
}

/// Request parameters for the `resume` tool.
///
/// Used to continue a suspended evaluation that hit an `Ask` effect.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ResumeRequest {
    /// The continuation ID returned by a suspended eval call.
    pub continuation_id: String,
    /// The response text to feed back to the suspended Haskell program.
    pub response: String,
}

// ---------------------------------------------------------------------------
// Templating
// ---------------------------------------------------------------------------

pub fn build_preamble(effects: &[EffectDecl]) -> String {
    let mut out = String::new();
    out.push_str("{-# LANGUAGE NoImplicitPrelude, OverloadedStrings, DataKinds, TypeOperators, FlexibleContexts, GADTs, PartialTypeSignatures, ScopedTypeVariables #-}\n");
    out.push_str("module Expr where\n");
    out.push_str("import Tidepool.Prelude\n");
    out.push_str("import qualified Data.Text as T\n");
    out.push_str("import Control.Monad.Freer\n");
    out.push_str("default (Int, Text)\n");
    out.push('\n');

    for eff in effects {
        for td in eff.type_defs {
            out.push_str(td);
            out.push('\n');
        }
        out.push_str(&format!("data {} a where\n", eff.type_name));
        for ctor in eff.constructors {
            out.push_str(&format!("  {}\n", ctor));
        }
        out.push('\n');
    }

    // Type alias so helpers can write `M a` instead of `Eff '[Console, KV, Fs] a`
    if !effects.is_empty() {
        let names: Vec<&str> = effects.iter().map(|e| e.type_name).collect();
        out.push_str(&format!("type M = Eff '[{}]\n\n", names.join(", ")));
    }

    // Emit thin effect helpers
    let has_helpers = effects.iter().any(|e| !e.helpers.is_empty());
    if has_helpers {
        for eff in effects {
            for h in eff.helpers {
                out.push_str(h);
                out.push('\n');
            }
        }
        out.push('\n');
    }

    out
}

/// Qualified aeson imports for MCP eval. Unqualified symbols now come from Tidepool.Prelude.
/// These provide `Aeson.` prefix (used by json_to_haskell for input injection) and
/// qualified access to KeyMap/Vector for power users.
pub fn aeson_imports() -> Vec<String> {
    vec![
        "qualified Tidepool.Aeson as Aeson".into(),
        "qualified Tidepool.Aeson.KeyMap as KM".into(),
    ]
}

pub fn build_effect_stack_type(effects: &[EffectDecl]) -> String {
    if effects.is_empty() {
        "'[]".to_string()
    } else {
        let names: Vec<&str> = effects.iter().map(|e| e.type_name).collect();
        format!("'[{}]", names.join(", "))
    }
}

fn build_eval_tool_description(effects: &[EffectDecl]) -> String {
    let mut desc = String::from(concat!(
        "Write Haskell do-notation in `code`. The server wraps it in a module ",
        "with the effect stack, pragmas, and imports. ",
        "Use `pure x` as the last line to return a value. ",
        "Use `send (Constructor args)` to invoke effects. ",
        "First call is slow (~2s). Subsequent calls are cached.\n",
        "Return values are automatically rendered to JSON by the Rust runtime \u{2014} ",
        "Int becomes a number, [Char] becomes a string, Bool becomes true/false, ",
        "lists become arrays, etc. Prefer `pure x` over `send (Print (show x))` ",
        "for returning results.",
    ));

    if !effects.is_empty() {
        desc.push_str("\nAvailable effects (use `send` to invoke):\n");
        for eff in effects {
            desc.push_str(&format!("\n{}: {}\n", eff.type_name, eff.description));
            for ctor in eff.constructors {
                desc.push_str(&format!("  {}\n", ctor));
            }
        }

        // List built-in helpers
        let has_helpers = effects.iter().any(|e| !e.helpers.is_empty());
        if has_helpers {
            desc.push_str("\nBuilt-in helpers (always available, no need to define):\n");
            for eff in effects {
                for h in eff.helpers {
                    // Extract just the type signature line
                    if let Some(sig) = h.lines().next() {
                        desc.push_str(&format!("  {}\n", sig));
                    }
                }
            }
            desc.push_str("\nPrefer helpers over raw `send`: `say \"hi\"` not `send (Print \"hi\")`.\n");
            desc.push_str("Use `>>=` chains and `<$>`/`<*>` for dense composition. Named bindings as escape hatch.\n");
        }
    }

    desc
}

pub fn template_haskell(
    preamble: &str,
    effect_stack: &str,
    code: &[String],
    imports: &[String],
    helpers: &[String],
    input: Option<&serde_json::Value>,
) -> String {
    let mut out = String::new();

    // Preamble contains: pragmas, module header, standard imports, default decl,
    // data declarations, type alias. User imports must go after standard imports
    // (after "import Control.Monad.Freer\n") and before "default".
    if !imports.is_empty() {
        let insert_point = preamble.find("default (Int").unwrap_or(preamble.len());
        out.push_str(&preamble[..insert_point]);
        for imp in imports {
            out.push_str(&format!("import {}\n", imp));
        }
        out.push_str(&preamble[insert_point..]);
    } else {
        out.push_str(preamble);
    }

    for helper in helpers {
        out.push_str(helper);
        out.push('\n');
    }
    if !helpers.is_empty() {
        out.push('\n');
    }

    // Inject input binding if provided
    if let Some(val) = input {
        out.push_str("input :: Aeson.Value\n");
        out.push_str(&format!("input = {}\n\n", json_to_haskell(val)));
    }

    out.push_str(&format!("result :: Eff {} _\n", effect_stack));
    out.push_str("result = do\n");
    for line in code {
        out.push_str(&format!("  {}\n", line));
    }

    out
}

/// Render a serde_json::Value as a Haskell aeson literal expression.
fn json_to_haskell(val: &serde_json::Value) -> String {
    match val {
        serde_json::Value::Null => "Aeson.Null".into(),
        serde_json::Value::Bool(b) => {
            format!("Aeson.Bool {}", if *b { "True" } else { "False" })
        }
        serde_json::Value::Number(n) => {
            format!("Aeson.Number (fromIntegral ({} :: Int))", n)
        }
        serde_json::Value::String(s) => {
            let escaped = s.replace('\\', "\\\\").replace('"', "\\\"");
            format!("Aeson.String \"{}\"", escaped)
        }
        serde_json::Value::Array(arr) => {
            let elems: Vec<String> = arr.iter().map(json_to_haskell).collect();
            format!("toJSON [{}]", elems.join(", "))
        }
        serde_json::Value::Object(map) => {
            let pairs: Vec<String> = map
                .iter()
                .map(|(k, v)| {
                    let escaped_k = k.replace('\\', "\\\\").replace('"', "\\\"");
                    format!("\"{}\" .= {}", escaped_k, json_to_haskell(v))
                })
                .collect();
            format!("object [{}]", pairs.join(", "))
        }
    }
}

// ---------------------------------------------------------------------------
// Error formatting
// ---------------------------------------------------------------------------

fn format_panic_payload(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else if let Some(s) = payload.downcast_ref::<&str>() {
        s.to_string()
    } else {
        "unknown panic".to_string()
    }
}

fn format_error_with_source(title: &str, error: &str, source: &str) -> String {
    format!(
        "## {}\n{}\n\n## Compiled Source\n```haskell\n{}\n```",
        title, error, source
    )
}

// ---------------------------------------------------------------------------
// Output capture
// ---------------------------------------------------------------------------

/// Captured output from effect handlers (e.g., Console Print).
///
/// Clone is cheap (Arc-backed). Thread-safe for use across spawn_blocking.
#[derive(Clone, Default)]
pub struct CapturedOutput {
    lines: Arc<std::sync::Mutex<Vec<String>>>,
}

impl CapturedOutput {
    pub fn new() -> Self {
        Self::default()
    }

    /// Push a line of output.
    pub fn push(&self, line: String) {
        self.lines.lock().unwrap().push(line);
    }

    /// Drain all captured lines, returning them and clearing the buffer.
    pub fn drain(&self) -> Vec<String> {
        let mut lines = self.lines.lock().unwrap();
        std::mem::take(&mut *lines)
    }
}

// ---------------------------------------------------------------------------
// Ask effect — channel-based suspension
// ---------------------------------------------------------------------------

/// Messages from the eval thread to the MCP server.
enum SessionMessage {
    /// The program hit an Ask effect and is waiting for a response.
    Suspended { prompt: String },
    /// The program completed successfully.
    Completed { result: String, output: Vec<String> },
    /// The program encountered an error.
    Error { error: String },
}

/// A suspended evaluation session, waiting for a resume call.
struct EvalSession {
    /// Send a response string to unblock the eval thread's Ask handler.
    response_tx: std::sync::mpsc::Sender<String>,
    /// Receive the next message (Completed, Suspended, or Error) from the eval thread.
    session_rx: tokio::sync::mpsc::UnboundedReceiver<SessionMessage>,
    /// The Haskell source code, for error formatting on resume.
    source: Arc<str>,
    /// When this session was created, for TTL cleanup.
    created_at: std::time::Instant,
}

/// Wraps an existing effect dispatcher and intercepts the Ask effect tag.
///
/// When the Ask tag is hit, sends a `Suspended` message via the session channel
/// and blocks the current thread until a response arrives.
struct AskDispatcher {
    inner: Box<dyn McpEffectHandler>,
    ask_tag: u64,
    session_tx: tokio::sync::mpsc::UnboundedSender<SessionMessage>,
    response_rx: std::sync::mpsc::Receiver<String>,
}

impl DispatchEffect<CapturedOutput> for AskDispatcher {
    fn dispatch(
        &mut self,
        tag: u64,
        request: &tidepool_eval::value::Value,
        cx: &tidepool_effect::dispatch::EffectContext<'_, CapturedOutput>,
    ) -> Result<tidepool_eval::value::Value, tidepool_effect::error::EffectError> {
        if tag == self.ask_tag {
            // Extract prompt from Ask constructor: Con(Ask, [prompt_val])
            let prompt = extract_ask_prompt(request, cx.table());

            // Signal suspension to the MCP server
            let _ = self.session_tx.send(SessionMessage::Suspended { prompt });

            // Block until the MCP server sends a response via the resume tool
            let response = self.response_rx.recv().map_err(|_| {
                tidepool_effect::error::EffectError::Handler(
                    "Ask session closed (timeout or client disconnected)".into(),
                )
            })?;

            // Convert response string to a Haskell Text value
            response
                .to_value(cx.table())
                .map_err(tidepool_effect::error::EffectError::Bridge)
        } else {
            self.inner.dispatch(tag, request, cx)
        }
    }
}

/// Best-effort extraction of the prompt string from an Ask request Value.
///
/// The request is `Con(Ask, [prompt_val])` where `prompt_val` is a Text value.
fn extract_ask_prompt(
    request: &tidepool_eval::value::Value,
    table: &tidepool_repr::DataConTable,
) -> String {
    use tidepool_eval::value::Value;

    if let Value::Con(_, fields) = request {
        if let Some(prompt_val) = fields.first() {
            // Try using FromCore (handles Text, LitString, [Char])
            if let Ok(s) = String::from_value(prompt_val, table) {
                return s;
            }
        }
    }
    // Fallback: debug representation
    format!("{:?}", request)
}

/// TTL for parked continuations (5 minutes).
const CONTINUATION_TTL: std::time::Duration = std::time::Duration::from_secs(300);

// ---------------------------------------------------------------------------
// Server internals
// ---------------------------------------------------------------------------

/// Trait combining effect dispatch with cloning for the MCP server.
pub trait McpEffectHandler:
    DispatchEffect<CapturedOutput> + DynClone + Send + Sync + 'static
{
}
clone_trait_object!(McpEffectHandler);

impl<T> McpEffectHandler for T where
    T: DispatchEffect<CapturedOutput> + Clone + Send + Sync + 'static
{
}

/// Generic MCP server wrapper that compiles and runs Haskell via Tidepool.
#[derive(Clone)]
pub struct TidepoolMcpServer<H> {
    inner: TidepoolMcpServerImpl,
    _phantom: PhantomData<H>,
}

/// Non-generic internal implementation to satisfy trait requirements.
#[derive(Clone)]
pub struct TidepoolMcpServerImpl {
    handler_factory: Arc<dyn McpEffectHandler>,
    include: Vec<PathBuf>,
    haskell_preamble: String,
    effect_stack_type: String,
    eval_tool_description: String,
    // Ask effect support
    ask_tag: u64,
    continuations: Arc<std::sync::Mutex<HashMap<String, EvalSession>>>,
    next_cont_id: Arc<AtomicU64>,
}

impl TidepoolMcpServerImpl {
    fn next_continuation_id(&self) -> String {
        let id = self.next_cont_id.fetch_add(1, Ordering::Relaxed);
        format!("cont_{}", id)
    }

    fn cleanup_stale_continuations(&self) {
        let mut conts = self.continuations.lock().unwrap();
        let now = std::time::Instant::now();
        conts.retain(|_, session| now.duration_since(session.created_at) < CONTINUATION_TTL);
    }

    async fn eval(&self, req: EvalRequest) -> Result<CallToolResult, McpError> {
        tracing::info!(lines = req.code.len(), "eval request");
        self.cleanup_stale_continuations();

        let mut all_imports = aeson_imports();
        all_imports.extend(req.imports);
        let source: Arc<str> = template_haskell(
            &self.haskell_preamble,
            &self.effect_stack_type,
            &req.code,
            &all_imports,
            &req.helpers,
            req.input.as_ref(),
        )
        .into();

        let handlers = dyn_clone::clone_box(&*self.handler_factory);
        let include_refs: Vec<PathBuf> = self.include.clone();
        let source_for_blocking = Arc::clone(&source);
        let captured = CapturedOutput::new();
        let captured_for_blocking = captured.clone();
        let ask_tag = self.ask_tag;

        // Create channels for Ask effect communication
        let (session_tx, mut session_rx) =
            tokio::sync::mpsc::unbounded_channel::<SessionMessage>();
        let (response_tx, response_rx) = std::sync::mpsc::channel::<String>();

        // Spawn eval thread — does NOT join; communicates via channels
        let thread_session_tx = session_tx;
        let _handle = std::thread::Builder::new()
            .name("tidepool-eval".into())
            .stack_size(8 * 1024 * 1024)
            .spawn(move || {
                let include_paths: Vec<&Path> =
                    include_refs.iter().map(|p| p.as_path()).collect();
                let mut ask_dispatcher = AskDispatcher {
                    inner: handlers,
                    ask_tag,
                    session_tx: thread_session_tx.clone(),
                    response_rx,
                };

                let result =
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        tidepool_runtime::compile_and_run(
                            &source_for_blocking,
                            "result",
                            &include_paths,
                            &mut ask_dispatcher,
                            &captured_for_blocking,
                        )
                    }));

                let output_lines = captured_for_blocking.drain();
                match result {
                    Ok(Ok(eval_result)) => {
                        let _ = thread_session_tx.send(SessionMessage::Completed {
                            result: eval_result.to_string_pretty(),
                            output: output_lines,
                        });
                    }
                    Ok(Err(e)) => {
                        let _ = thread_session_tx.send(SessionMessage::Error {
                            error: e.to_string(),
                        });
                    }
                    Err(panic_payload) => {
                        let _ = thread_session_tx.send(SessionMessage::Error {
                            error: format_panic_payload(panic_payload),
                        });
                    }
                }
            })
            .map_err(|e| {
                McpError::internal_error(format!("thread spawn error: {}", e), None)
            })?;

        // Await first message from the eval thread
        match session_rx.recv().await {
            Some(SessionMessage::Completed { result, output }) => {
                tracing::info!("eval completed");
                let mut response = String::new();
                if !output.is_empty() {
                    response.push_str("## Output\n");
                    for line in &output {
                        response.push_str(line);
                        response.push('\n');
                    }
                    response.push_str("\n## Result\n");
                }
                response.push_str(&result);
                Ok(CallToolResult::success(vec![Content::text(response)]))
            }
            Some(SessionMessage::Suspended { prompt }) => {
                tracing::info!(prompt = %prompt, "eval suspended on Ask");
                let cont_id = self.next_continuation_id();
                let json = serde_json::json!({
                    "suspended": true,
                    "continuation_id": cont_id,
                    "prompt": prompt,
                });
                self.continuations.lock().unwrap().insert(
                    cont_id.clone(),
                    EvalSession {
                        response_tx,
                        session_rx,
                        source: Arc::clone(&source),
                        created_at: std::time::Instant::now(),
                    },
                );
                Ok(CallToolResult::success(vec![Content::text(
                    json.to_string(),
                )]))
            }
            Some(SessionMessage::Error { error }) => {
                let error_msg = format_error_with_source("Error", &error, &source);
                tracing::error!("eval failed: {}", error);
                Ok(CallToolResult::error(vec![Content::text(error_msg)]))
            }
            None => Err(McpError::internal_error(
                "eval thread died unexpectedly",
                None,
            )),
        }
    }

    async fn resume(&self, req: ResumeRequest) -> Result<CallToolResult, McpError> {
        tracing::info!(continuation_id = %req.continuation_id, "resume request");
        self.cleanup_stale_continuations();

        let mut session = {
            let mut conts = self.continuations.lock().unwrap();
            conts.remove(&req.continuation_id).ok_or_else(|| {
                McpError::invalid_params(
                    format!(
                        "Unknown or expired continuation_id: {}",
                        req.continuation_id
                    ),
                    None,
                )
            })?
        };

        // Send the response to the blocked eval thread
        session.response_tx.send(req.response).map_err(|_| {
            McpError::internal_error("eval thread is no longer running", None)
        })?;

        let source = session.source.clone();
        let response_tx = session.response_tx.clone();

        // Await the next message from the eval thread
        match session.session_rx.recv().await {
            Some(SessionMessage::Completed { result, output }) => {
                tracing::info!("resumed eval completed");
                let mut response = String::new();
                if !output.is_empty() {
                    response.push_str("## Output\n");
                    for line in &output {
                        response.push_str(line);
                        response.push('\n');
                    }
                    response.push_str("\n## Result\n");
                }
                response.push_str(&result);
                Ok(CallToolResult::success(vec![Content::text(response)]))
            }
            Some(SessionMessage::Suspended { prompt }) => {
                tracing::info!(prompt = %prompt, "resumed eval suspended again");
                let cont_id = self.next_continuation_id();
                let json = serde_json::json!({
                    "suspended": true,
                    "continuation_id": cont_id,
                    "prompt": prompt,
                });
                self.continuations.lock().unwrap().insert(
                    cont_id.clone(),
                    EvalSession {
                        response_tx,
                        session_rx: session.session_rx,
                        source,
                        created_at: std::time::Instant::now(),
                    },
                );
                Ok(CallToolResult::success(vec![Content::text(
                    json.to_string(),
                )]))
            }
            Some(SessionMessage::Error { error }) => {
                let error_msg = format_error_with_source("Error", &error, &source);
                tracing::error!("resumed eval failed: {}", error);
                Ok(CallToolResult::error(vec![Content::text(error_msg)]))
            }
            None => Err(McpError::internal_error(
                "eval thread died unexpectedly",
                None,
            )),
        }
    }
}

impl ServerHandler for TidepoolMcpServerImpl {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            instructions: Some(self.eval_tool_description.clone()),
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            ..Default::default()
        }
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let args = request.arguments.unwrap_or_default();
        match request.name.as_ref() {
            "eval" => {
                let req: EvalRequest = serde_json::from_value(serde_json::Value::Object(args))
                    .map_err(|e| {
                        McpError::invalid_params(format!("invalid params: {}", e), None)
                    })?;
                self.eval(req).await
            }
            "resume" => {
                let req: ResumeRequest =
                    serde_json::from_value(serde_json::Value::Object(args)).map_err(|e| {
                        McpError::invalid_params(format!("invalid params: {}", e), None)
                    })?;
                self.resume(req).await
            }
            _ => Err(McpError {
                code: ErrorCode::METHOD_NOT_FOUND,
                message: format!("Tool not found: {}", request.name).into(),
                data: None,
            }),
        }
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        fn schema_to_map(
            schema: schemars::Schema,
        ) -> Result<Arc<serde_json::Map<String, serde_json::Value>>, McpError> {
            let json = serde_json::to_value(&schema).map_err(|e| {
                McpError::internal_error(format!("Failed to serialize schema: {}", e), None)
            })?;
            match json {
                serde_json::Value::Object(o) => Ok(Arc::new(o)),
                _ => Ok(Arc::new(serde_json::Map::new())),
            }
        }

        let tools = vec![
            Tool {
                name: "eval".into(),
                title: None,
                description: Some(self.eval_tool_description.clone().into()),
                input_schema: schema_to_map(schemars::schema_for!(EvalRequest))?,
                output_schema: None,
                annotations: None,
                icons: None,
                meta: None,
                execution: None,
            },
            Tool {
                name: "resume".into(),
                title: None,
                description: Some(
                    "Resume a suspended Haskell evaluation. When eval returns \
                     {\"suspended\": true, \"continuation_id\": \"...\", \"prompt\": \"...\"}, \
                     call this tool with the continuation_id and your response to the prompt."
                        .into(),
                ),
                input_schema: schema_to_map(schemars::schema_for!(ResumeRequest))?,
                output_schema: None,
                annotations: None,
                icons: None,
                meta: None,
                execution: None,
            },
        ];

        Ok(ListToolsResult {
            tools,
            next_cursor: None,
            meta: None,
        })
    }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

impl<H> TidepoolMcpServer<H>
where
    H: DispatchEffect<CapturedOutput> + Clone + Send + Sync + 'static + CollectEffectDecls,
{
    /// Create a new server with the given effect handler stack.
    ///
    /// Effect declarations are collected automatically from handlers that
    /// implement `DescribeEffect`.
    pub fn new(handler: H) -> Self {
        let mut decls = H::collect_decls();
        let ask_tag = decls.len() as u64;
        decls.push(ask_decl());
        Self {
            inner: TidepoolMcpServerImpl {
                handler_factory: Arc::new(handler),
                include: Vec::new(),
                haskell_preamble: build_preamble(&decls),
                effect_stack_type: build_effect_stack_type(&decls),
                eval_tool_description: build_eval_tool_description(&decls),
                ask_tag,
                continuations: Arc::new(std::sync::Mutex::new(HashMap::new())),
                next_cont_id: Arc::new(AtomicU64::new(1)),
            },
            _phantom: PhantomData,
        }
    }

    /// Add include paths for Haskell module resolution.
    pub fn with_include(mut self, paths: Vec<PathBuf>) -> Self {
        self.inner.include = paths;
        self
    }

    /// Add the bundled Tidepool prelude to the include paths.
    ///
    /// Looks for the prelude in this order:
    /// 1. `TIDEPOOL_PRELUDE_DIR` environment variable
    /// 2. The provided fallback path
    ///
    /// The prelude provides source definitions for common Prelude functions
    /// (reverse, splitAt, sort, etc.) whose GHC base library workers lack
    /// unfoldings in .hi files.
    pub fn with_prelude(mut self, fallback: PathBuf) -> Self {
        let prelude_dir = std::env::var_os("TIDEPOOL_PRELUDE_DIR")
            .map(PathBuf::from)
            .unwrap_or(fallback);
        self.inner.include.push(prelude_dir);
        self
    }

    /// Start the MCP server on stdio transport.
    pub async fn serve_stdio(self) -> Result<(), Box<dyn std::error::Error>> {
        self.inner
            .serve((stdin(), stdout()))
            .await?
            .waiting()
            .await?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_eval_request_string_code() {
        let json = serde_json::json!({"code": "let x = 1\npure x"});
        let req: EvalRequest = serde_json::from_value(json).unwrap();
        assert_eq!(req.code, vec!["let x = 1", "pure x"]);
        assert!(req.imports.is_empty());
        assert!(req.helpers.is_empty());
    }

    #[test]
    fn test_eval_request_array_code() {
        let json = serde_json::json!({"code": ["pure 42"]});
        let req: EvalRequest = serde_json::from_value(json).unwrap();
        assert_eq!(req.code, vec!["pure 42"]);
    }

    #[test]
    fn test_eval_request_string_imports() {
        let json = serde_json::json!({"code": "pure 42", "imports": "Data.List (sort)\nData.Char"});
        let req: EvalRequest = serde_json::from_value(json).unwrap();
        assert_eq!(req.imports, vec!["Data.List (sort)", "Data.Char"]);
    }

    #[test]
    fn test_build_preamble() {
        let effects = vec![
            EffectDecl {
                type_name: "Console",
                description: "Print output",
                constructors: &["Print :: Text -> Console ()"],
                type_defs: &[],
                helpers: &[],
            },
            EffectDecl {
                type_name: "KV",
                description: "Key-value store",
                constructors: &[
                    "KvGet :: Text -> KV (Maybe Text)",
                    "KvSet :: Text -> Text -> KV ()",
                ],
                type_defs: &[],
                helpers: &[],
            },
        ];
        let preamble = build_preamble(&effects);
        assert!(preamble.contains("data Console a where"));
        assert!(preamble.contains("  Print :: Text -> Console ()"));
        assert!(preamble.contains("data KV a where"));
    }

    #[test]
    fn test_build_effect_stack_type() {
        let effects = vec![
            EffectDecl {
                type_name: "Console",
                description: "",
                constructors: &[],
                type_defs: &[],
                helpers: &[],
            },
            EffectDecl {
                type_name: "KV",
                description: "",
                constructors: &[],
                type_defs: &[],
                helpers: &[],
            },
            EffectDecl {
                type_name: "Fs",
                description: "",
                constructors: &[],
                type_defs: &[],
                helpers: &[],
            },
        ];
        assert_eq!(build_effect_stack_type(&effects), "'[Console, KV, Fs]");
        assert_eq!(build_effect_stack_type(&[]), "'[]");
    }

    #[test]
    fn test_template_haskell() {
        let effects = vec![EffectDecl {
            type_name: "Console",
            description: "",
            constructors: &["Print :: Text -> Console ()"],
            type_defs: &[],
            helpers: &[],
        }];
        let preamble = build_preamble(&effects);
        let stack = build_effect_stack_type(&effects);
        let source = vec!["let x = 42".into(), "pure x".into()];

        let result = template_haskell(&preamble, &stack, &source, &[], &[], None);

        assert!(result.contains("module Expr where"));
        assert!(result.contains("import Control.Monad.Freer"));
        assert!(result.contains("data Console a where"));
        assert!(result.contains("result :: Eff '[Console] _"));
        assert!(result.contains("result = do"));
        assert!(result.contains("  let x = 42"));
        assert!(result.contains("  pure x"));
    }

    #[test]
    fn test_eval_tool_description_includes_effects() {
        let effects = vec![EffectDecl {
            type_name: "Console",
            description: "Print to console",
            constructors: &["Print :: Text -> Console ()"],
            type_defs: &[],
            helpers: &["say :: Text -> M ()\nsay = send . Print"],
        }];
        let desc = build_eval_tool_description(&effects);
        assert!(desc.contains("Console: Print to console"));
        assert!(desc.contains("Print :: Text -> Console ()"));
        assert!(desc.contains("say :: Text -> M ()"));
        assert!(desc.contains("Built-in helpers"));
    }

    #[test]
    fn test_preamble_includes_helpers() {
        let decls = standard_decls();
        let preamble = build_preamble(&decls);
        assert!(preamble.contains("say :: Text -> M ()\nsay = send . Print"));
        assert!(preamble.contains("kvGet :: Text -> M (Maybe Text)\nkvGet = send . KvGet"));
        assert!(preamble.contains("fsRead :: Text -> M Text\nfsRead = send . FsRead"));
        assert!(preamble.contains("httpGet :: Text -> M Value\nhttpGet = send . HttpGet"));
        assert!(preamble.contains("ask :: Text -> M Text\nask = send . Ask"));
    }

    #[test]
    fn test_format_panic_payload() {
        use std::any::Any;

        let s = "string panic".to_string();
        let payload: Box<dyn Any + Send> = Box::new(s);
        assert_eq!(format_panic_payload(payload), "string panic");

        let s = "str panic";
        let payload: Box<dyn Any + Send> = Box::new(s);
        assert_eq!(format_panic_payload(payload), "str panic");

        let payload: Box<dyn Any + Send> = Box::new(42);
        assert_eq!(format_panic_payload(payload), "unknown panic");
    }

    #[test]
    fn test_format_error_with_source() {
        let title = "Error";
        let error = "Type mismatch";
        let source = "main = pure ()";
        let formatted = format_error_with_source(title, error, source);

        assert!(formatted.contains("## Error"));
        assert!(formatted.contains("Type mismatch"));
        assert!(formatted.contains("## Compiled Source"));
        assert!(formatted.contains("```haskell\nmain = pure ()\n```"));
    }

    #[test]
    fn test_ask_decl() {
        let decl = ask_decl();
        assert_eq!(decl.type_name, "Ask");
        assert_eq!(decl.constructors.len(), 1);
        assert!(decl.constructors[0].contains("Ask :: Text -> Ask Text"));
    }

    #[test]
    fn test_standard_decls_includes_ask() {
        let decls = standard_decls();
        assert_eq!(decls.len(), 6);
        assert_eq!(decls[4].type_name, "Http");
        assert_eq!(decls[5].type_name, "Ask");
    }

    #[test]
    fn test_resume_request_parse() {
        let json = serde_json::json!({
            "continuation_id": "cont_1",
            "response": "hello"
        });
        let req: ResumeRequest = serde_json::from_value(json).unwrap();
        assert_eq!(req.continuation_id, "cont_1");
        assert_eq!(req.response, "hello");
    }

    #[test]
    fn test_ask_in_preamble() {
        let decls = standard_decls();
        let preamble = build_preamble(&decls);
        assert!(preamble.contains("data Ask a where"));
        assert!(preamble.contains("  Ask :: Text -> Ask Text"));
        assert!(preamble.contains("type M = Eff '[Console, KV, Fs, SG, Http, Ask]"));
    }

    #[test]
    fn test_ask_in_effect_stack_type() {
        let decls = standard_decls();
        let stack = build_effect_stack_type(&decls);
        assert_eq!(stack, "'[Console, KV, Fs, SG, Http, Ask]");
    }
}
