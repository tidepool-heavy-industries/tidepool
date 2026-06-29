//! The `tidepool-repl` MCP server — a SEPARATE server/binary from the `tidepool`
//! eval server (whose request path is untouched). It exposes the session tool
//! surface and routes each tool to a [`SessionCommand`] on the resident worker.
//!
//! Tools:
//! - `session_open` — spawn a named session worker (N concurrent sessions supported).
//! - `session_def` — append a declaration (Lane A).
//! - `session_eval` — run an `M a` expression on the resident machine.
//! - `session_cmd` — `:bindings` / `:reset` (and stubbed `:t` / `:i`).
//! - `session_close` — drop the machine, free the heap.
//! - `session_resume` / `session_abort` — answer/abort an in-turn `ask`
//!   (the parked-thread mechanism reused from the eval server).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use parking_lot::Mutex;
use rmcp::{
    model::*, service::RequestContext, ErrorData as McpError, RoleServer, ServerHandler, ServiceExt,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tidepool_effect::dispatch::DispatchEffect;
use tidepool_mcp::{CapturedOutput, EffectDecl};
use tidepool_repr::SessionId;
use tidepool_runtime::session::ModuleEnv;
use tokio::io::{stdin, stdout};
use tokio::time::{timeout, Duration};

use crate::ask::{PauseGate, ResumeMsg, WorkerMessage};
use crate::command::{DeclText, ExprText, MetaCommand, SessionCommand};
use crate::session::{SessionConfig, DEFAULT_NURSERY_SIZE};
use crate::worker::{spawn_worker, SessionManager, WorkerHandle, WorkerJob};

/// Per-turn window before a turn is declared timed out. A session is one
/// resident thread, so a runaway wedges the session (MVP); the window keeps a
/// single MCP call from hanging forever.
const TURN_TIMEOUT_SECS: u64 = 120;

// ---------------------------------------------------------------------------
// Request types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct SessionOpenRequest {
    /// Session name. Omit to use `"default"` (back-compat). Multiple agents
    /// can each open a distinct named session; the name is used as the key for
    /// subsequent session_def/eval/cmd/close/resume/abort calls.
    #[serde(default)]
    pub session: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SessionDefRequest {
    /// One or more top-level Haskell declarations (functions, types, classes).
    pub decl: String,
    /// Session name (default: `"default"`).
    #[serde(default)]
    pub session: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SessionEvalRequest {
    /// A single Haskell expression of type `M a` (as in the `eval` tool). Its
    /// value is the turn's result. Declarations from prior `session_def` turns
    /// are in scope.
    pub code: String,
    /// Optional payload available as `input :: Aeson.Value` in the evaluated
    /// code. Pass large or quote-heavy content here instead of escaping it
    /// inside `code`. Mirrors the stateless `eval` tool's `input` lane.
    #[serde(default)]
    pub input: Option<serde_json::Value>,
    /// Session name (default: `"default"`).
    #[serde(default)]
    pub session: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SessionCmdRequest {
    /// A meta-command: `:bindings`, `:reset`, `:t <expr>`, `:i <name>` (leading
    /// colon optional).
    pub command: String,
    /// Session name (default: `"default"`).
    #[serde(default)]
    pub session: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct SessionCloseRequest {
    /// Session name (default: `"default"`).
    #[serde(default)]
    pub session: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SessionResumeRequest {
    pub continuation_id: String,
    #[serde(default)]
    pub response: serde_json::Value,
    /// Session name (default: `"default"`).
    #[serde(default)]
    pub session: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SessionAbortRequest {
    pub continuation_id: String,
    #[serde(default)]
    pub reason: Option<String>,
    /// Session name (default: `"default"`).
    #[serde(default)]
    pub session: Option<String>,
}

// ---------------------------------------------------------------------------
// Session name helpers
// ---------------------------------------------------------------------------

/// Resolve an optional session name to its canonical form. `None` yields
/// `"default"` for back-compat with single-session callers.
fn resolve_session(session: Option<String>) -> String {
    session.unwrap_or_else(|| "default".to_string())
}

// ---------------------------------------------------------------------------
// Continuation registry (in-turn ask suspend/resume)
// ---------------------------------------------------------------------------

/// A parked in-turn `ask`: the worker is blocked on `response_tx`'s peer, the
/// async side holds the live `session_rx` to await the rest of the turn.
struct ReplContinuation {
    response_tx: std::sync::mpsc::Sender<ResumeMsg>,
    session_rx: tokio::sync::mpsc::UnboundedReceiver<WorkerMessage>,
    gate: Arc<PauseGate>,
    captured: CapturedOutput,
    /// The `ask`'s schema (from the `AskWith` meta), used to validate AND
    /// canonicalize the resume reply before it reaches the continuation — so
    /// `ask` returns a structured, optic-extractable `Value` (a reply that
    /// arrives as a JSON string is parsed into the canonical shape). `None` ⇒
    /// accept any JSON. Mirrors the eval server's `AwaitingAnswer`.
    expected_schema: Option<serde_json::Value>,
    created_at: Instant,
}

// ---------------------------------------------------------------------------
// Server config + inner
// ---------------------------------------------------------------------------

/// Static config for the server (everything but the per-session root, which is
/// minted per `session_open`).
pub struct ReplServerConfig {
    pub decls: Vec<EffectDecl>,
    pub ask_tag: u64,
    /// Base GHC include dirs (generated `Tidepool.Effects` dir + prelude/stdlib).
    pub base_include: Vec<PathBuf>,
    /// Import/pragma surface for generated `Lib.G<g>` decl modules.
    pub module_env: ModuleEnv,
    /// Parent dir under which per-session include trees are created.
    pub session_root_base: PathBuf,
    /// Session nursery size in bytes. `None` ⇒ [`DEFAULT_NURSERY_SIZE`] (64 MiB).
    /// Tests shrink it to force an organic GC between turns.
    pub nursery_size: Option<usize>,
}

/// The non-generic server core (H is erased into the `spawn` closure).
struct ReplServerInner {
    manager: SessionManager,
    /// Keyed by (session_id, continuation_id) so resume/abort route to the
    /// right session's continuation.
    continuations: Mutex<HashMap<(String, String), ReplContinuation>>,
    next_cont_id: AtomicU64,
    next_session_id: AtomicU64,
    /// Spawns a worker for a `SessionConfig` (captures the base handler + ask_tag).
    spawn: Box<dyn Fn(SessionConfig) -> WorkerHandle + Send + Sync>,
    preamble: String,
    effect_stack: String,
    cfg: ReplServerConfig,
    tool_description: String,
}

/// The `tidepool-repl` MCP server. `Clone` is cheap (Arc); the HTTP transport
/// clones it per connection.
#[derive(Clone)]
pub struct TidepoolReplServer {
    inner: Arc<ReplServerInner>,
}

impl TidepoolReplServer {
    /// Build a server over the given base effect handler stack `base` and config.
    pub fn new<H>(base: H, cfg: ReplServerConfig) -> TidepoolReplServer
    where
        H: DispatchEffect<CapturedOutput> + Clone + Send + Sync + 'static,
    {
        let preamble = tidepool_mcp::build_preamble_non_interactive(&cfg.decls, false);
        let effect_stack = tidepool_mcp::build_effect_stack_type(&cfg.decls);
        let ask_tag = cfg.ask_tag;
        // Erase H: the spawn closure owns a clone of `base`.
        let spawn: Box<dyn Fn(SessionConfig) -> WorkerHandle + Send + Sync> =
            Box::new(move |sc| spawn_worker(sc, base.clone(), ask_tag));
        TidepoolReplServer {
            inner: Arc::new(ReplServerInner {
                manager: SessionManager::new(),
                continuations: Mutex::new(HashMap::new()),
                next_cont_id: AtomicU64::new(1),
                next_session_id: AtomicU64::new(1),
                spawn,
                preamble,
                effect_stack,
                tool_description: build_tool_description(&cfg.decls),
                cfg,
            }),
        }
    }

    /// Start on stdio transport.
    pub async fn serve_stdio(self) -> Result<(), Box<dyn std::error::Error>> {
        self.serve((stdin(), stdout())).await?.waiting().await?;
        Ok(())
    }

    /// Start on streamable HTTP transport.
    pub async fn serve_http(
        self,
        addr: std::net::SocketAddr,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use rmcp::transport::streamable_http_server::{
            session::local::LocalSessionManager, StreamableHttpServerConfig, StreamableHttpService,
        };
        let template = self;
        let config = StreamableHttpServerConfig::default();
        let cancel = config.cancellation_token.clone();
        let service = StreamableHttpService::new(
            move || Ok(template.clone()),
            Arc::new(LocalSessionManager::default()),
            config,
        );
        async fn health() -> axum::Json<serde_json::Value> {
            axum::Json(serde_json::json!({"status": "ok"}))
        }
        let router = axum::Router::new()
            .route("/health", axum::routing::get(health))
            .nest_service("/mcp", service);
        let listener = tokio::net::TcpListener::bind(addr).await?;
        eprintln!(
            "tidepool-repl v{} listening on http://{}/mcp",
            env!("CARGO_PKG_VERSION"),
            addr
        );
        axum::serve(listener, router)
            .with_graceful_shutdown(async move {
                tokio::signal::ctrl_c().await.ok();
                cancel.cancel();
            })
            .await?;
        Ok(())
    }

    fn next_continuation_id(&self) -> String {
        format!(
            "scont_{}",
            self.inner.next_cont_id.fetch_add(1, Ordering::Relaxed)
        )
    }

    // -- tool handlers -----------------------------------------------------

    /// The shared tool-dispatch entry point. `call_tool` (the MCP `ServerHandler`
    /// method) delegates here; tests drive this directly to exercise the exact
    /// production path without constructing a `RequestContext`.
    pub async fn dispatch_tool(
        &self,
        name: &str,
        args: serde_json::Map<String, serde_json::Value>,
    ) -> Result<CallToolResult, McpError> {
        let parse =
            |args: serde_json::Map<String, serde_json::Value>| serde_json::Value::Object(args);
        match name {
            "session_open" => {
                let req: SessionOpenRequest = serde_json::from_value(parse(args))
                    .map_err(|e| McpError::invalid_params(format!("invalid params: {e}"), None))?;
                let sid = resolve_session(req.session);
                Ok(self.session_open(&sid).await)
            }
            "session_def" => {
                let req: SessionDefRequest = serde_json::from_value(parse(args))
                    .map_err(|e| McpError::invalid_params(format!("invalid params: {e}"), None))?;
                let sid = resolve_session(req.session);
                Ok(self
                    .run_command(
                        "session_def",
                        SessionCommand::Def(DeclText(req.decl)),
                        None,
                        &sid,
                    )
                    .await)
            }
            "session_eval" => {
                let req: SessionEvalRequest = serde_json::from_value(parse(args))
                    .map_err(|e| McpError::invalid_params(format!("invalid params: {e}"), None))?;
                let sid = resolve_session(req.session);
                Ok(self
                    .run_command(
                        "session_eval",
                        SessionCommand::Eval(ExprText(req.code)),
                        req.input,
                        &sid,
                    )
                    .await)
            }
            "session_cmd" => {
                let req: SessionCmdRequest = serde_json::from_value(parse(args))
                    .map_err(|e| McpError::invalid_params(format!("invalid params: {e}"), None))?;
                let sid = resolve_session(req.session);
                let meta = match MetaCommand::parse(&req.command) {
                    Ok(m) => m,
                    Err(e) => return Ok(CallToolResult::error(vec![Content::text(e)])),
                };
                Ok(self
                    .run_command("session_cmd", SessionCommand::Cmd(meta), None, &sid)
                    .await)
            }
            "session_close" => {
                let req: SessionCloseRequest = serde_json::from_value(parse(args))
                    .map_err(|e| McpError::invalid_params(format!("invalid params: {e}"), None))?;
                let sid = resolve_session(req.session);
                Ok(self.session_close(&sid).await)
            }
            "session_resume" => {
                let req: SessionResumeRequest = serde_json::from_value(parse(args))
                    .map_err(|e| McpError::invalid_params(format!("invalid params: {e}"), None))?;
                self.session_resume(req).await
            }
            "session_abort" => {
                let req: SessionAbortRequest = serde_json::from_value(parse(args))
                    .map_err(|e| McpError::invalid_params(format!("invalid params: {e}"), None))?;
                self.session_abort(req).await
            }
            other => Err(McpError {
                code: ErrorCode::METHOD_NOT_FOUND,
                message: format!("Tool not found: {other}").into(),
                data: None,
            }),
        }
    }

    async fn session_open(&self, session_name: &str) -> CallToolResult {
        let sid = SessionId(self.inner.next_session_id.fetch_add(1, Ordering::Relaxed));
        let root = self
            .inner
            .cfg
            .session_root_base
            .join(format!("session-{}", sid.0));
        let cfg = SessionConfig {
            id: sid,
            root,
            base_include: self.inner.cfg.base_include.clone(),
            decls: self.inner.cfg.decls.clone(),
            preamble: self.inner.preamble.clone(),
            effect_stack: self.inner.effect_stack.clone(),
            ask_tag: self.inner.cfg.ask_tag,
            module_env: self.inner.cfg.module_env.clone(),
            nursery_size: self.inner.cfg.nursery_size.unwrap_or(DEFAULT_NURSERY_SIZE),
        };
        let handle = (self.inner.spawn)(cfg);
        match self.inner.manager.install(session_name, handle) {
            Ok(()) => CallToolResult::success(vec![Content::text(
                serde_json::json!({"opened": true, "session_id": sid.0, "session": session_name})
                    .to_string(),
            )]),
            Err(rejected) => {
                rejected.shutdown();
                CallToolResult::error(vec![Content::text(format!(
                    "session '{}' is already open; call session_close first",
                    session_name
                ))])
            }
        }
    }

    /// Send a `SessionCommand` to the named session worker and await its reply.
    /// `eval_input` is forwarded to the worker so `input :: Aeson.Value` is in
    /// scope for `session_eval` turns; pass `None` for `session_def`/`session_cmd`.
    async fn run_command(
        &self,
        op: &str,
        cmd: SessionCommand,
        eval_input: Option<serde_json::Value>,
        session_name: &str,
    ) -> CallToolResult {
        let Some(sender) = self.inner.manager.get_sender(session_name) else {
            return CallToolResult::error(vec![Content::text(format!(
                "no session '{}' open; call session_open first",
                session_name
            ))]);
        };
        let (session_tx, session_rx) = tokio::sync::mpsc::unbounded_channel::<WorkerMessage>();
        let (response_tx, response_rx) = std::sync::mpsc::channel::<ResumeMsg>();
        let gate = PauseGate::new();
        let captured = CapturedOutput::new();
        let job = WorkerJob {
            cmd,
            session_tx,
            response_rx,
            gate: Arc::clone(&gate),
            captured: captured.clone(),
            eval_input,
        };
        if sender.send(job).is_err() {
            return CallToolResult::error(vec![Content::text("session worker is gone")]);
        }
        self.drive(op, session_rx, response_tx, gate, captured, session_name)
            .await
    }

    async fn session_close(&self, session_name: &str) -> CallToolResult {
        let Some(handle) = self.inner.manager.remove(session_name) else {
            return CallToolResult::error(vec![Content::text(format!(
                "no session '{}' open",
                session_name
            ))]);
        };
        let (session_tx, mut session_rx) = tokio::sync::mpsc::unbounded_channel::<WorkerMessage>();
        let (_response_tx, response_rx) = std::sync::mpsc::channel::<ResumeMsg>();
        let gate = PauseGate::new();
        let captured = CapturedOutput::new();
        let job = WorkerJob {
            cmd: SessionCommand::Close,
            session_tx,
            response_rx,
            gate,
            captured,
            eval_input: None,
        };
        let _ = handle.sender().send(job);
        // Await the Closed ack, then reap the worker thread.
        let _ = timeout(Duration::from_secs(30), session_rx.recv()).await;
        handle.shutdown();
        CallToolResult::success(vec![Content::text(
            serde_json::json!({"closed": true, "session": session_name}).to_string(),
        )])
    }

    async fn session_resume(&self, req: SessionResumeRequest) -> Result<CallToolResult, McpError> {
        // Validate + canonicalize the reply against the suspension's schema
        // BEFORE consuming the continuation. This (a) makes `ask` return a
        // STRUCTURED, optic-extractable Value — a reply that arrived as a JSON
        // string is parsed into the canonical shape (BUG-9) — and (b) leaves an
        // invalid reply's continuation un-consumed so the caller can retry.
        // Mirrors the eval server's resume (tidepool-mcp/src/server.rs).
        let session_name = resolve_session(req.session);
        let cont_key = (session_name.clone(), req.continuation_id.clone());
        let cont = {
            let mut conts = self.inner.continuations.lock();
            let Some(c) = conts.get(&cont_key) else {
                return Err(McpError::invalid_params(
                    format!(
                        "Unknown or expired continuation_id: {} (session '{}')",
                        req.continuation_id, session_name
                    ),
                    None,
                ));
            };
            let schema = c.expected_schema.clone();
            match tidepool_mcp::validate::validate_response(schema.as_ref(), &req.response) {
                tidepool_mcp::validate::Outcome::Invalid(violations) => {
                    // Anti-starvation: a retrying continuation must not become the
                    // oldest-first eviction victim while its caller fixes the reply.
                    if let Some(c) = conts.get_mut(&cont_key) {
                        c.created_at = Instant::now();
                    }
                    let body = serde_json::json!({
                        "validation_failed": true,
                        "violations": violations
                            .iter()
                            .map(tidepool_mcp::validate::Violation::to_json)
                            .collect::<Vec<_>>(),
                        "schema": schema,
                        "continuation_id": req.continuation_id,
                        "continuation_not_consumed": true,
                    });
                    return Ok(CallToolResult::error(vec![Content::text(format!(
                        "Response does not match the suspension's schema. Call session_resume \
                         again with the same continuation_id and a corrected response (or \
                         session_abort).\n{body}"
                    ))]));
                }
                tidepool_mcp::validate::Outcome::Valid(canonical) => {
                    let cont = conts
                        .remove(&cont_key)
                        .expect("continuation present: checked under the same lock");
                    if cont.response_tx.send(ResumeMsg::Answer(canonical)).is_err() {
                        return Err(McpError::internal_error(
                            "session worker is no longer running",
                            None,
                        ));
                    }
                    cont
                }
            }
        };
        Ok(self
            .drive(
                "session_resume",
                cont.session_rx,
                cont.response_tx,
                cont.gate,
                cont.captured,
                &session_name,
            )
            .await)
    }

    async fn session_abort(&self, req: SessionAbortRequest) -> Result<CallToolResult, McpError> {
        let session_name = resolve_session(req.session);
        let cont_key = (session_name.clone(), req.continuation_id.clone());
        let cont = {
            let mut conts = self.inner.continuations.lock();
            conts.remove(&cont_key)
        };
        let Some(cont) = cont else {
            return Err(McpError::invalid_params(
                format!(
                    "Unknown or expired continuation_id: {} (session '{}')",
                    req.continuation_id, session_name
                ),
                None,
            ));
        };
        let reason = req
            .reason
            .unwrap_or_else(|| "aborted by caller".to_string());
        if cont.response_tx.send(ResumeMsg::Abort(reason)).is_err() {
            return Err(McpError::internal_error(
                "session worker is no longer running",
                None,
            ));
        }
        Ok(self
            .drive(
                "session_abort",
                cont.session_rx,
                cont.response_tx,
                cont.gate,
                cont.captured,
                &session_name,
            )
            .await)
    }

    /// Await the next worker message for an in-flight turn, mapping it to an MCP
    /// result (parking a continuation on `Suspended`).
    async fn drive(
        &self,
        op: &str,
        mut session_rx: tokio::sync::mpsc::UnboundedReceiver<WorkerMessage>,
        response_tx: std::sync::mpsc::Sender<ResumeMsg>,
        gate: Arc<PauseGate>,
        captured: CapturedOutput,
        session_name: &str,
    ) -> CallToolResult {
        let received =
            match timeout(Duration::from_secs(TURN_TIMEOUT_SECS), session_rx.recv()).await {
                Ok(r) => r,
                Err(_) => {
                    // The worker is still computing; ask it to park at the next
                    // effect boundary (best effort — a pure runaway can't be).
                    gate.request_pause();
                    return CallToolResult::error(vec![Content::text(format!(
                        "{op} timed out after {TURN_TIMEOUT_SECS}s (the resident session may be \
                     wedged on a long/pure computation)"
                    ))]);
                }
            };
        match received {
            Some(WorkerMessage::Completed { result }) => {
                let out = captured.drain();
                CallToolResult::success(vec![Content::text(with_output(&out, &result))])
            }
            Some(WorkerMessage::Error { error }) => {
                let out = captured.snapshot();
                CallToolResult::error(vec![Content::text(with_output(&out, &error))])
            }
            Some(WorkerMessage::Suspended { prompt, meta }) => {
                let cont_id = self.next_continuation_id();
                let mut json_obj = serde_json::json!({
                    "suspended": true,
                    "continuation_id": cont_id,
                    "prompt": prompt,
                });
                let mut expected_schema = None;
                if let Some(serde_json::Value::Object(mut m)) = meta {
                    if let Some(schema) = m.remove("schema") {
                        // Keep the schema to validate + canonicalize the reply on
                        // resume (BUG-9); also surface it to the caller.
                        expected_schema = Some(schema.clone());
                        if let Some(o) = json_obj.as_object_mut() {
                            o.insert("schema".into(), schema);
                        }
                    }
                    if !m.is_empty() {
                        if let Some(o) = json_obj.as_object_mut() {
                            o.insert("meta".into(), serde_json::Value::Object(m));
                        }
                    }
                }
                self.inner.continuations.lock().insert(
                    (session_name.to_string(), cont_id),
                    ReplContinuation {
                        response_tx,
                        session_rx,
                        gate,
                        captured,
                        expected_schema,
                        created_at: Instant::now(),
                    },
                );
                CallToolResult::success(vec![Content::text(json_obj.to_string())])
            }
            Some(WorkerMessage::Closed) => CallToolResult::success(vec![Content::text(
                serde_json::json!({"closed": true}).to_string(),
            )]),
            None => CallToolResult::error(vec![Content::text(format!(
                "{op}: session worker thread crashed (likely a JIT signal — exhausted case \
                 branch or invalid memory access)"
            ))]),
        }
    }
}

/// Prepend captured output (if any) to a result body.
fn with_output(output: &[String], body: &str) -> String {
    if output.is_empty() {
        return body.to_string();
    }
    let mut s = String::from("## Output\n");
    for line in output {
        s.push_str(line);
        s.push('\n');
    }
    s.push_str("\n## Result\n");
    s.push_str(body);
    s
}

fn build_tool_description(decls: &[EffectDecl]) -> String {
    let names: Vec<&str> = decls.iter().map(|d| d.type_name).collect();
    format!(
        "tidepool-repl — a GHCi-style stateful Haskell session. Each named session holds one \
         resident JIT machine whose value heap persists across `session_eval` turns; \
         `session_def` accumulates declarations. Multiple agents can open distinct named sessions \
         in parallel (omit `session` to use `\"default\"`). \
         Lifecycle: session_open → (session_def | session_eval | session_cmd)* → session_close. \
         `session_eval` code is a single `M a` expression (effects: {effects}). Prior \
         declarations are in scope. An in-turn `ask` suspends with a continuation_id; answer it \
         with session_resume or drop it with session_abort.",
        effects = names.join(", "),
    )
}

// ---------------------------------------------------------------------------
// ServerHandler
// ---------------------------------------------------------------------------

impl ServerHandler for TidepoolReplServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            instructions: Some(self.inner.tool_description.clone()),
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            ..Default::default()
        }
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        self.dispatch_tool(request.name.as_ref(), request.arguments.unwrap_or_default())
            .await
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        fn schema_to_map(
            schema: schemars::Schema,
        ) -> Result<Arc<serde_json::Map<String, serde_json::Value>>, McpError> {
            match serde_json::to_value(&schema).map_err(|e| {
                McpError::internal_error(format!("schema serialize failed: {e}"), None)
            })? {
                serde_json::Value::Object(o) => Ok(Arc::new(o)),
                _ => Ok(Arc::new(serde_json::Map::new())),
            }
        }
        fn tool(
            name: &str,
            desc: &str,
            schema: Arc<serde_json::Map<String, serde_json::Value>>,
        ) -> Tool {
            Tool {
                name: name.to_string().into(),
                title: None,
                description: Some(desc.to_string().into()),
                input_schema: schema,
                output_schema: None,
                annotations: None,
                icons: None,
                meta: None,
                execution: None,
            }
        }
        let tools = vec![
            tool(
                "session_open",
                "Open a named session (one live JIT machine per name). Omit `session` to use the \
                 default session. Call before any session_def/eval/cmd.",
                schema_to_map(schemars::schema_for!(SessionOpenRequest))?,
            ),
            tool(
                "session_def",
                "Append one or more top-level Haskell declarations to the session library. They \
                 stay in scope for later session_eval turns.",
                schema_to_map(schemars::schema_for!(SessionDefRequest))?,
            ),
            tool(
                "session_eval",
                "Evaluate a Haskell expression of type `M a` on the resident machine (the heap \
                 persists across turns). Prior session_def declarations are in scope.",
                schema_to_map(schemars::schema_for!(SessionEvalRequest))?,
            ),
            tool(
                "session_cmd",
                "Run a session meta-command: :bindings, :reset (:t / :i are stubbed). Leading \
                 colon optional.",
                schema_to_map(schemars::schema_for!(SessionCmdRequest))?,
            ),
            tool(
                "session_close",
                "Close the session: drop the resident machine and free its heap.",
                schema_to_map(schemars::schema_for!(SessionCloseRequest))?,
            ),
            tool(
                "session_resume",
                "Answer an in-turn `ask` suspension (continuation_id from a {\"suspended\":true} \
                 result) and run the turn to completion.",
                schema_to_map(schemars::schema_for!(SessionResumeRequest))?,
            ),
            tool(
                "session_abort",
                "Abort an in-turn `ask` suspension without answering it.",
                schema_to_map(schemars::schema_for!(SessionAbortRequest))?,
            ),
        ];
        Ok(ListToolsResult {
            tools,
            next_cursor: None,
            meta: None,
        })
    }
}
