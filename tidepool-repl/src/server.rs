//! The `tidepool-repl` MCP server — a SEPARATE server/binary from the `tidepool`
//! eval server (whose request path is untouched). It exposes the session tool
//! surface and routes each tool to a [`SessionCommand`] on the resident worker.
//!
//! Tools:
//! - `session_open` — spawn a named session worker (N concurrent sessions supported).
//! - `session_run` — run a list of GHCi-capable items (decls, binds, exprs, :commands).
//! - `session_close` — drop the machine, free the heap.
//! - `session_resume` / `session_abort` — answer/abort an in-turn `ask`
//!   (the parked-thread mechanism reused from the eval server).

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

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
use crate::command::{BlockItem, DeclText, ExprText, MetaCommand, SessionCommand};
use crate::session::{SessionConfig, DEFAULT_NURSERY_SIZE};
use crate::state::{SessionState, SharedState, Suspension};
use crate::worker::{
    empty_cancel_slot, spawn_worker, CancelSlot, SessionManager, WorkerHandle, WorkerJob,
};

/// Per-turn window before a turn is declared timed out. A session is one
/// resident thread, so a runaway wedges the session (MVP); the window keeps a
/// single MCP call from hanging forever.
const TURN_TIMEOUT_SECS: u64 = 120;

/// After a turn times out and is cancelled, how long to wait for the worker to
/// abort at a JIT safepoint before declaring the session `Wedged`. Allocating /
/// tail-recursive runaways abort within milliseconds of `cancel()`; this margin
/// only covers scheduling. A turn that doesn't abort in this window is treated
/// as genuinely uninterruptible (the reaper reclaims it).
const ABORT_GRACE_SECS: u64 = 3;

/// The manager-side handles for the session whose turn [`TidepoolReplServer::drive`]
/// awaits: its lifecycle [`SharedState`] and the [`CancelSlot`] read on timeout to
/// abort a runaway at a JIT safepoint. Bundled so `drive` stays within the
/// argument-count budget.
struct DriveCtl {
    state: SharedState,
    cancel: CancelSlot,
}

// ---------------------------------------------------------------------------
// Request types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct SessionOpenRequest {
    /// Session name. Omit to use `"default"` (back-compat). Multiple agents
    /// can each open a distinct named session; the name is used as the key for
    /// subsequent session_run/close/resume/abort calls.
    #[serde(default)]
    pub session: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SessionBlockRequest {
    /// List of GHCi-capable items to run in sequence. Each item is one of:
    /// a top-level declaration (`data Foo = …`, `f x = …`), a bind statement
    /// (`x <- e` / `let x = e`), a bare expression, or a `:command`
    /// (`:bindings`, `:reset`, `:t <expr>`, `:i <name>`, `:vocab`).
    /// Items are classified automatically; execution stops on the first error.
    /// Each declaration item is its own module, so a type signature and its
    /// binding (and all equations of a multi-clause function) must share ONE
    /// newline-separated item.
    pub items: Vec<String>,
    /// Optional payload available as `input :: Aeson.Value` to every evaluated
    /// item in the block (binds, `let`s, and bare expressions). Pass large or
    /// quote-heavy content here to avoid Haskell string escaping. Mirrors the
    /// stateless `eval` tool's `input` lane.
    #[serde(default)]
    pub input: Option<serde_json::Value>,
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
// Item classifier
// ---------------------------------------------------------------------------

/// Classify one `session_run` item string into a [`BlockItem`].
///
/// Classification strategy (try-cascade, approach (a) from the plan):
/// - `:` prefix → [`BlockItem::Meta`] via `MetaCommand::parse`.
/// - Keyword-initiated declarations (`data`, `newtype`, `type`, `class`,
///   `instance`, …) → [`BlockItem::Decl`] (unambiguous; skip cascade).
/// - Everything else → [`BlockItem::Auto`]: `run_block` will attempt the item
///   as a declaration via `run_def` first; on a GHC parse error it falls back
///   to [`BlockItem::Stmt`]/`run_eval`.
///
/// Misclassification fails LOUD — the wrong handler's GHC error surfaces
/// immediately rather than silently producing a wrong result.
pub fn classify_item(text: &str) -> Result<BlockItem, String> {
    let s = text.trim();
    // An empty/whitespace-only item is a NO-OP (RE-1): route it to run_def,
    // which returns Ok without bumping the generation (matches the legacy
    // `session_def ""` contract). Erroring here would fail the whole block.
    if s.is_empty() {
        return Ok(BlockItem::Decl(DeclText(String::new())));
    }

    // :commands → Meta (unambiguous)
    if s.starts_with(':') {
        return MetaCommand::parse(s).map(BlockItem::Meta);
    }

    // Keyword-initiated declarations are unambiguous — skip the cascade.
    const DECL_KEYWORDS: &[&str] = &[
        "data ",
        "newtype ",
        "type ",
        "class ",
        "instance ",
        "infixl ",
        "infixr ",
        "infix ",
        "foreign ",
        "import ",
        "default ",
        "{-# ",
    ];
    for kw in DECL_KEYWORDS {
        if s.starts_with(kw) {
            return Ok(BlockItem::Decl(DeclText(s.to_string())));
        }
    }

    // Everything else needs the try-cascade (function equations, type sigs,
    // bind stmts, bare expressions — all routed through run_def first).
    Ok(BlockItem::Auto(ExprText(s.to_string())))
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
    /// How long an abandoned suspension (a parked `ask` that is never resumed or
    /// aborted) — or a `Wedged` session — may linger before the reaper reclaims
    /// it. `None` ⇒ no reaper (the historical behavior). `main.rs` sets a sane
    /// default (~30 min); tests shrink it to exercise reaping fast.
    pub continuation_ttl: Option<Duration>,
    /// Wall-clock budget for a single turn before it is cancelled at a JIT
    /// safepoint (see [`drive`]). `None` ⇒ [`TURN_TIMEOUT_SECS`] (120 s). Tests
    /// shrink it to exercise the timeout/self-heal path fast.
    pub turn_timeout: Option<Duration>,
}

/// Spawns a worker for a `(session_name, SessionConfig)` — the erased,
/// per-session handler-stack builder (H is hidden behind this boxed closure).
type SessionSpawn = Box<dyn Fn(&str, SessionConfig) -> WorkerHandle + Send + Sync>;

/// Whether a project/global `Library` facade is on the include path. When true
/// the preamble emits `import Library` so `.tidepool/lib` verbs are in scope
/// (parity with the eval server). Derived from `base_include` so no extra config
/// field / test-harness churn: `main.rs` puts the lib dirs on `base_include`.
fn has_user_library(cfg: &ReplServerConfig) -> bool {
    cfg.base_include
        .iter()
        .any(|d| d.join("Library.hs").exists())
}

/// The non-generic server core (H is erased into the `spawn` closure).
struct ReplServerInner {
    manager: SessionManager,
    next_cont_id: AtomicU64,
    next_session_id: AtomicU64,
    /// Spawns a worker for `(session_name, SessionConfig)` (captures handler builder + ask_tag).
    spawn: SessionSpawn,
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
        let preamble =
            tidepool_mcp::build_preamble_non_interactive(&cfg.decls, has_user_library(&cfg));
        let effect_stack = tidepool_mcp::build_effect_stack_type(&cfg.decls);
        let ask_tag = cfg.ask_tag;
        // Erase H: the spawn closure owns a clone of `base`; session name is ignored (shared stack).
        let spawn: SessionSpawn =
            Box::new(move |_: &str, sc| spawn_worker(sc, base.clone(), ask_tag));
        let server = TidepoolReplServer {
            inner: Arc::new(ReplServerInner {
                manager: SessionManager::new(),
                next_cont_id: AtomicU64::new(1),
                next_session_id: AtomicU64::new(1),
                spawn,
                preamble,
                effect_stack,
                tool_description: build_tool_description(&cfg.decls),
                cfg,
            }),
        };
        server.spawn_reaper();
        server
    }

    /// Build a server where each named session gets its own handler stack from `builder`.
    ///
    /// `builder` is invoked once per `session_open` with the session name, producing a
    /// fresh base stack for that session. Use this to give each session an isolated KV
    /// namespace (e.g. a per-session backing file) while sharing all other construction.
    ///
    /// The `cfg` must already carry the correct `decls` and `ask_tag` (derived from a
    /// representative stack before calling this constructor).
    pub fn new_with_session_builder<H, F>(builder: F, cfg: ReplServerConfig) -> TidepoolReplServer
    where
        H: DispatchEffect<CapturedOutput> + Clone + Send + Sync + 'static,
        F: Fn(&str) -> H + Send + Sync + 'static,
    {
        let preamble =
            tidepool_mcp::build_preamble_non_interactive(&cfg.decls, has_user_library(&cfg));
        let effect_stack = tidepool_mcp::build_effect_stack_type(&cfg.decls);
        let ask_tag = cfg.ask_tag;
        let spawn: SessionSpawn = Box::new(move |session_name: &str, sc| {
            let base = builder(session_name);
            spawn_worker(sc, base, ask_tag)
        });
        let server = TidepoolReplServer {
            inner: Arc::new(ReplServerInner {
                manager: SessionManager::new(),
                next_cont_id: AtomicU64::new(1),
                next_session_id: AtomicU64::new(1),
                spawn,
                preamble,
                effect_stack,
                tool_description: build_tool_description(&cfg.decls),
                cfg,
            }),
        };
        server.spawn_reaper();
        server
    }

    /// Spawn the background reaper: periodically reclaim abandoned suspensions
    /// (a parked `ask` never resumed/aborted, H2) and `Wedged` sessions (a
    /// timed-out turn, H3), so neither leaks a worker thread + JIT machine
    /// indefinitely. No-op when `continuation_ttl` is `None` or there is no
    /// tokio runtime (e.g. a unit test that constructs the server off-runtime).
    fn spawn_reaper(&self) {
        let Some(ttl) = self.inner.cfg.continuation_ttl else {
            return;
        };
        if tokio::runtime::Handle::try_current().is_err() {
            return;
        }
        // Weak so the reaper does not keep the server alive — it exits the first
        // tick after the last `TidepoolReplServer` is dropped.
        let weak = Arc::downgrade(&self.inner);
        // Sweep several times per TTL so effective lateness is ≤ ~TTL.
        let tick = (ttl / 4).max(Duration::from_millis(50));
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(tick);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                interval.tick().await;
                let Some(inner) = weak.upgrade() else {
                    break;
                };
                reap_once(&inner, ttl);
            }
        });
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
            "session_run" => {
                let req: SessionBlockRequest = serde_json::from_value(parse(args))
                    .map_err(|e| McpError::invalid_params(format!("invalid params: {e}"), None))?;
                let sid = resolve_session(req.session);
                let mut block_items: Vec<BlockItem> = Vec::with_capacity(req.items.len());
                for item_text in &req.items {
                    match classify_item(item_text) {
                        Ok(item) => block_items.push(item),
                        Err(e) => {
                            return Ok(CallToolResult::error(vec![Content::text(format!(
                                "session_run: failed to classify item {item_text:?}: {e}"
                            ))]))
                        }
                    }
                }
                // MCP clients stringify the `input` param (a JSON object/array
                // arrives double-encoded as a String); unwrap one level so
                // `input :: Aeson.Value` is the structured value, matching the
                // stateless `eval` tool exactly.
                let input = req.input.as_ref().map(tidepool_mcp::normalize_input);
                Ok(self
                    .run_command(
                        "session_run",
                        SessionCommand::Block(block_items),
                        input,
                        &sid,
                    )
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
        let handle = (self.inner.spawn)(session_name, cfg);
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
    /// scope for the first eval item in a `session_run` block.
    async fn run_command(
        &self,
        op: &str,
        cmd: SessionCommand,
        eval_input: Option<serde_json::Value>,
        session_name: &str,
    ) -> CallToolResult {
        let Some(state) = self.inner.manager.state(session_name) else {
            return CallToolResult::error(vec![Content::text(format!(
                "no session '{}' open; call session_open first",
                session_name
            ))]);
        };
        // Busy-guard (M5): only an Idle session accepts a new turn. A turn that
        // is running, suspended on an `ask`, wedged, or closing must be resolved
        // first — otherwise a second run would queue behind the parked worker and
        // later mutate state against a dropped listener.
        {
            let mut st = state.lock();
            if !st.is_idle() {
                let label = st.busy_label();
                return CallToolResult::error(vec![Content::text(format!(
                    "session '{session_name}' is {label}; resume/abort it (or close) \
                     before running again"
                ))]);
            }
            *st = SessionState::Busy;
        }
        let Some(sender) = self.inner.manager.get_sender(session_name) else {
            *state.lock() = SessionState::Idle;
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
            *state.lock() = SessionState::Idle;
            return CallToolResult::error(vec![Content::text("session worker is gone")]);
        }
        let cancel = self
            .inner
            .manager
            .cancel_slot(session_name)
            .unwrap_or_else(empty_cancel_slot);
        self.drive(
            op,
            session_rx,
            response_tx,
            gate,
            captured,
            DriveCtl { state, cancel },
        )
        .await
    }

    async fn session_close(&self, session_name: &str) -> CallToolResult {
        // H1 FIX: if the session is parked on an `ask`, RELEASE its suspension
        // first. Setting `Closing` drops the old `SessionState` — and with it the
        // `Suspension`'s `response_tx` — so the worker blocked on `response_rx`
        // (inside `handle.run`, NOT reading `cmd_tx`) unblocks, unwinds the turn,
        // and returns to its command loop where it can observe the `Close` below.
        // Without this, the `Close` job is never read, the ack times out, and the
        // unbounded `join()` in `shutdown()` hangs a tokio thread forever.
        if let Some(state) = self.inner.manager.state(session_name) {
            *state.lock() = SessionState::Closing;
        }
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
        // Await the Closed ack. If it arrives the worker reached its command loop
        // and exited cleanly → join it. If it does NOT (a pure-compute runaway
        // that never reads `Close`), DETACH instead of joining — `shutdown`'s
        // `join()` would hang forever on an uninterruptible thread.
        let acked = timeout(Duration::from_secs(30), session_rx.recv())
            .await
            .is_ok();
        if acked {
            handle.shutdown();
        } else {
            handle.detach();
        }
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
        let unknown = || {
            McpError::invalid_params(
                format!(
                    "Unknown or expired continuation_id: {} (session '{}')",
                    req.continuation_id, session_name
                ),
                None,
            )
        };
        let Some(state) = self.inner.manager.state(&session_name) else {
            return Err(unknown());
        };
        // All under the per-session state lock; we extract the owned `Suspension`
        // and DROP the lock before `drive().await` (never hold it across await).
        let suspension = {
            let mut st = state.lock();
            // Must be Suspended on the matching continuation.
            let schema = match &*st {
                SessionState::Suspended(s) if s.cont_id == req.continuation_id => {
                    s.expected_schema.clone()
                }
                _ => return Err(unknown()),
            };
            match tidepool_mcp::validate::validate_response(schema.as_ref(), &req.response) {
                tidepool_mcp::validate::Outcome::Invalid(violations) => {
                    // Anti-starvation: a retrying continuation must not become the
                    // reaper's oldest-first eviction victim while its caller fixes
                    // the reply. Stays Suspended (un-consumed).
                    if let SessionState::Suspended(s) = &mut *st {
                        s.since = Instant::now();
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
                    // Take the suspension out → Busy (the turn is resuming).
                    let SessionState::Suspended(s) =
                        std::mem::replace(&mut *st, SessionState::Busy)
                    else {
                        unreachable!("checked Suspended under the same lock")
                    };
                    let s = *s;
                    if s.response_tx.send(ResumeMsg::Answer(canonical)).is_err() {
                        *st = SessionState::Idle;
                        return Err(McpError::internal_error(
                            "session worker is no longer running",
                            None,
                        ));
                    }
                    s
                }
            }
        };
        let cancel = self
            .inner
            .manager
            .cancel_slot(&session_name)
            .unwrap_or_else(empty_cancel_slot);
        Ok(self
            .drive(
                "session_resume",
                suspension.session_rx,
                suspension.response_tx,
                suspension.gate,
                suspension.captured,
                DriveCtl { state, cancel },
            )
            .await)
    }

    async fn session_abort(&self, req: SessionAbortRequest) -> Result<CallToolResult, McpError> {
        let session_name = resolve_session(req.session);
        let unknown = || {
            McpError::invalid_params(
                format!(
                    "Unknown or expired continuation_id: {} (session '{}')",
                    req.continuation_id, session_name
                ),
                None,
            )
        };
        let Some(state) = self.inner.manager.state(&session_name) else {
            return Err(unknown());
        };
        let reason = req
            .reason
            .unwrap_or_else(|| "aborted by caller".to_string());
        let suspension = {
            let mut st = state.lock();
            match &*st {
                SessionState::Suspended(s) if s.cont_id == req.continuation_id => {}
                _ => return Err(unknown()),
            }
            let SessionState::Suspended(s) = std::mem::replace(&mut *st, SessionState::Busy) else {
                unreachable!("checked Suspended under the same lock")
            };
            let s = *s;
            if s.response_tx.send(ResumeMsg::Abort(reason)).is_err() {
                *st = SessionState::Idle;
                return Err(McpError::internal_error(
                    "session worker is no longer running",
                    None,
                ));
            }
            s
        };
        let cancel = self
            .inner
            .manager
            .cancel_slot(&session_name)
            .unwrap_or_else(empty_cancel_slot);
        Ok(self
            .drive(
                "session_abort",
                suspension.session_rx,
                suspension.response_tx,
                suspension.gate,
                suspension.captured,
                DriveCtl { state, cancel },
            )
            .await)
    }

    /// Await the next worker message for an in-flight turn, mapping it to an MCP
    /// result AND driving the session's [`SessionState`] transition. The state
    /// arrived `Busy` (set by the caller); this resolves it to `Idle` (turn
    /// finished), `Suspended` (parked an `ask` — the suspension payload, incl.
    /// `response_tx`, is stored IN the state), or `Wedged` (timeout / crash).
    async fn drive(
        &self,
        op: &str,
        mut session_rx: tokio::sync::mpsc::UnboundedReceiver<WorkerMessage>,
        response_tx: std::sync::mpsc::Sender<ResumeMsg>,
        gate: Arc<PauseGate>,
        captured: CapturedOutput,
        ctl: DriveCtl,
    ) -> CallToolResult {
        let DriveCtl { state, cancel } = ctl;
        let turn_timeout = self
            .inner
            .cfg
            .turn_timeout
            .unwrap_or(Duration::from_secs(TURN_TIMEOUT_SECS));
        let to_secs = turn_timeout.as_secs();
        let received = match timeout(turn_timeout, session_rx.recv()).await {
            Ok(r) => r,
            Err(_) => {
                // H3: the worker is still computing past the budget. Abort it
                // cooperatively on two fronts:
                //   (1) `request_abort` unwinds an `ask`-parked turn at the
                //       effect boundary;
                //   (2) the resident machine's `CancelHandle` aborts an
                //       allocating / tail-recursive runaway at its next JIT
                //       safepoint (`YieldError::Cancelled`).
                // Then a bounded grace re-wait: if the worker aborts promptly
                // the session SELF-HEALS back to `Idle` (handle reset, ready
                // for the next turn); only a genuinely-uninterruptible turn
                // (or a session whose first-ever turn ran away before any
                // machine was published) stays `Wedged` for the reaper.
                gate.request_abort(format!("{op} timed out after {to_secs}s"));
                let handle = cancel.lock().as_ref().cloned();
                if let Some(h) = handle {
                    h.cancel();
                    if let Ok(Some(_)) =
                        timeout(Duration::from_secs(ABORT_GRACE_SECS), session_rx.recv()).await
                    {
                        // Worker aborted at a safepoint — clear the flag and
                        // return the session to Idle (self-healed).
                        h.reset();
                        *state.lock() = SessionState::Idle;
                        return CallToolResult::error(vec![Content::text(format!(
                            "{op} timed out after {to_secs}s and was aborted; the \
                                 session recovered and is ready for the next turn"
                        ))]);
                    }
                }
                // No handle (first-turn runaway) or no prompt abort → wedged.
                *state.lock() = SessionState::Wedged {
                    since: Instant::now(),
                };
                return CallToolResult::error(vec![Content::text(format!(
                    "{op} timed out after {to_secs}s (the resident session is \
                         wedged on an uninterruptible computation; close it)"
                ))]);
            }
        };
        match received {
            Some(WorkerMessage::Completed { result }) => {
                *state.lock() = SessionState::Idle;
                let out = captured.drain();
                CallToolResult::success(vec![Content::text(with_output(&out, &result))])
            }
            Some(WorkerMessage::Error { error }) => {
                *state.lock() = SessionState::Idle;
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
                // The suspension payload lives IN the state — a parked `ask` can't
                // exist untracked, so teardown (close/reaper) is forced to release
                // its `response_tx`.
                *state.lock() = SessionState::Suspended(Box::new(Suspension {
                    cont_id: cont_id.clone(),
                    response_tx,
                    session_rx,
                    gate,
                    captured,
                    expected_schema,
                    since: Instant::now(),
                }));
                CallToolResult::success(vec![Content::text(json_obj.to_string())])
            }
            Some(WorkerMessage::Closed) => {
                *state.lock() = SessionState::Idle;
                CallToolResult::success(vec![Content::text(
                    serde_json::json!({"closed": true}).to_string(),
                )])
            }
            None => {
                // The worker thread died (JIT signal). Mark Wedged so the reaper
                // removes + reaps the dead worker.
                *state.lock() = SessionState::Wedged {
                    since: Instant::now(),
                };
                CallToolResult::error(vec![Content::text(format!(
                    "{op}: session worker thread crashed (likely a JIT signal — exhausted case \
                     branch or invalid memory access)"
                ))])
            }
        }
    }
}

/// One reaper sweep: reclaim suspensions / wedges older than `ttl`.
///
/// - An abandoned `Suspended` (never resumed/aborted) → `Idle`: dropping the
///   `Suspension` drops its `response_tx`, so the parked worker's
///   `response_rx.recv()` errors, the turn unwinds, and the worker returns to
///   its command loop — the session stays alive and usable (H2).
/// - A stale `Wedged` (timed-out turn) → removed, freeing the session name.
///   The worker is DETACHED, not joined: a pure-compute runaway can't be joined
///   without hanging.
fn reap_once(inner: &ReplServerInner, ttl: Duration) {
    let now = Instant::now();
    for (id, state) in inner.manager.snapshot_states() {
        let remove_wedged = {
            let mut st = state.lock();
            match &*st {
                SessionState::Suspended(s) if now.duration_since(s.since) >= ttl => {
                    *st = SessionState::Idle;
                    false
                }
                SessionState::Wedged { since } if now.duration_since(*since) >= ttl => {
                    *st = SessionState::Closing;
                    true
                }
                _ => false,
            }
        };
        if remove_wedged {
            if let Some(handle) = inner.manager.remove(&id) {
                handle.detach();
            }
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
         resident JIT machine whose value heap and module scope persist across turns; \
         declarations accumulate across `session_run` calls.\n\n\
         PRIMARY TOOL: session_run\n\
         Pass a list of items run in sequence: top-level declarations (`data Foo = …`, \
         `f x = …`), bind statements (`x <- e` / `let x = e`), bare expressions, or \
         :commands (`:bindings`, `:reset`, `:t <expr>`, `:i <name>`, `:vocab`). \
         Items are classified automatically. Execution stops on the first error. \
         Returns per-item results and the last expression's value.\n\n\
         PREFERRED IDIOM — define then call in one block:\n\
         Put helper definitions and type aliases in the early items, then call them in the \
         final expression. One block with a clean definition + its caller beats cramming all \
         logic into a single expression.\n\n\
         ONE DECLARATION PER ITEM: each declaration item compiles as its own module, so a \
         type signature and its binding — and all equations of a multi-clause function — must \
         live in the SAME item (newline-separated), not split across items.\n\n\
         BINDS vs VALUE: end a block with a bare EXPRESSION to populate the top-level `value` \
         — a block ending in a bind (`x <- e` / `let x = e`) leaves `value` null (read \
         `items[].result`). A bare `x = 5` (no `let`) is a top-level DECLARATION; use \
         `let x = 5` to bind a value into the heap.\n\n\
         JSON OUTPUT: opt-in — return an `Aeson.Value` to get structured JSON instead of \
         Show output.\n\n\
         Available effects: {effects}.\n\n\
         Lifecycle: session_open → session_run* → session_close. \
         Multiple agents can open distinct named sessions in parallel \
         (omit `session` to use `\"default\"`). \
         An in-turn `ask` suspends with a continuation_id; answer it with session_resume or \
         drop it with session_abort.",
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
                 default session. Call before session_run.",
                schema_to_map(schemars::schema_for!(SessionOpenRequest))?,
            ),
            tool(
                "session_run",
                "Run a list of GHCi-capable items in sequence on the resident machine. Each item \
                 is a declaration (`data Foo = …`, `f x = …`), a bind statement (`x <- e` / \
                 `let x = e`), a bare expression, or a `:command` (`:bindings`, `:reset`, \
                 `:t <expr>`, `:i <name>`, `:vocab`). Items are classified automatically; \
                 execution stops on the first error. Returns per-item results and the last \
                 expression's value. \
                 PREFERRED IDIOM: define helpers and types in early items, then invoke them in \
                 the final expression — one block with a clean definition plus its caller beats \
                 one cramped expression. Each declaration item is its own module, so a type \
                 signature and its binding (and a multi-clause function's equations) must share \
                 ONE newline-separated item. \
                 JSON OUTPUT: return an `Aeson.Value` to get structured JSON instead of Show \
                 output. \
                 An in-turn `ask` suspends with a continuation_id; resume with session_resume \
                 or drop with session_abort.",
                schema_to_map(schemars::schema_for!(SessionBlockRequest))?,
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
