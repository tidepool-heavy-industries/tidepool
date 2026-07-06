//! The `tidepool-repl` MCP server — a SEPARATE server/binary from the `tidepool`
//! eval server (whose request path is untouched). It exposes ONE implicit
//! session over three tools and one live-state resource.
//!
//! Tools:
//! - `session_run` — run a list of GHCi-capable items (decls, binds, exprs,
//!   :commands). Auto-opens the session on first use.
//! - `session_resume` — answer an in-turn `ask` suspension (the parked-thread
//!   mechanism reused from the eval server).
//! - `session_reset` — drop the resident machine and open a fresh one; also
//!   drops any pending `ask` continuation (abort folds into reset).
//!
//! Resource:
//! - `tidepool://session/bindings` — read-only JSON over live session state.

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
use tidepool_mcp::{describe_effects_index, CapturedOutput, EffectDecl};
use tidepool_repr::SessionId;
use tidepool_runtime::session::ModuleEnv;
use tokio::io::{stdin, stdout};
use tokio::time::{timeout, Duration};
use tokio_util::sync::CancellationToken;

use crate::ask::{PauseGate, ResumeMsg, WorkerMessage};
use crate::command::{BlockItem, DeclText, ExprText, MetaCommand, SessionCommand};
use crate::session::{SessionConfig, DEFAULT_NURSERY_SIZE};
use crate::state::{take_suspension, ContinuationId, SessionState, SharedState, Suspension};
use crate::worker::{
    empty_cancel_slot, spawn_worker, CancelSlot, SessionManager, WorkerHandle, WorkerJob,
};

/// The `tidepool://session/bindings` resource URI: read-only JSON over the live
/// session environment (decl plane + value/pure binds).
const SESSION_BINDINGS_URI: &str = "tidepool://session/bindings";

/// Per-turn window before a turn is declared timed out. A session is one
/// resident thread, so a runaway wedges the session (MVP); the window keeps a
/// single MCP call from hanging forever.
const TURN_TIMEOUT_SECS: u64 = 600;

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

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SessionBlockRequest {
    /// List of GHCi-capable items to run in sequence. Each item is one of:
    /// a top-level declaration (`data Foo = …`, `f x = …`), a bind statement
    /// (`x <- e` / `let x = e`), a bare expression, or a `:command`
    /// (`:bindings`, `:reset`, `:t <expr>`, `:i <name>`, `:vocab`, `:stub <n>`, `:program`).
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
    /// Set `true` to get the full diagnostic shape: per-item `index` and
    /// double-encoded `result` string, plus top-level `generation` /
    /// `valGeneration` counters. Default (`false`): the slim shape with inline
    /// JSON, no generation counters, and final-expression value at top level only.
    #[serde(default)]
    pub verbose: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SessionResumeRequest {
    pub continuation_id: ContinuationId,
    #[serde(default)]
    pub response: serde_json::Value,
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
/// minted per session open).
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
    /// How long a parked `ask` suspension may linger before the reaper flips it
    /// back to `Idle`. `None` ⇒ parked continuations never expire — the
    /// production default (`main.rs`): a parked ask holds one worker thread +
    /// JIT machine, an acceptable cost for long-parked knots. Tests set it
    /// small to exercise the reap path (H2).
    pub continuation_ttl: Option<Duration>,
    /// How long a `Wedged` session (a timed-out turn, H3) may linger before the
    /// reaper closes and removes it. `None` ⇒ no wedged sweep. `main.rs` keeps
    /// ~30 min — a wedged session is dead weight, unlike a parked ask.
    pub wedged_ttl: Option<Duration>,
    /// Wall-clock budget for a single turn before it is cancelled at a JIT
    /// safepoint (see [`drive`]). `None` ⇒ [`TURN_TIMEOUT_SECS`] (600 s). Tests
    /// shrink it to exercise the timeout/self-heal path fast.
    pub turn_timeout: Option<Duration>,
}

/// Spawns a worker for a [`SessionConfig`] — the erased handler-stack builder
/// (H is hidden behind this boxed closure).
type SessionSpawn = Box<dyn Fn(SessionConfig) -> WorkerHandle + Send + Sync>;

/// Whether a project/global `Library` facade is on the include path. When true
/// the preamble emits `import Library` so `.tidepool/lib` verbs are in scope
/// (parity with the eval server). Derived from `base_include` so no extra config
/// field / test-harness churn: `main.rs` puts the lib dirs on `base_include`.
fn has_user_library(cfg: &ReplServerConfig) -> bool {
    cfg.base_include
        .iter()
        .any(|d| d.join("Library.hs").exists())
}

/// The repl's eval preamble: non-interactive pagination, with the Haskell-side
/// `paginateResult` truncation patched to a pass-through so oversized results
/// reach Rust in full and are truncated by [`crate::truncate::truncate_result`]
/// instead — which can stash the elided subtrees for `:stub <n>` (the Haskell
/// truncation discarded them, leaving `stub_N` markers nothing could fetch).
fn repl_preamble(cfg: &ReplServerConfig) -> String {
    crate::truncate::passthrough_paginate(&tidepool_mcp::build_preamble_non_interactive(
        &cfg.decls,
        has_user_library(cfg),
    ))
}

/// The non-generic server core (H is erased into the `spawn` closure).
struct ReplServerInner {
    manager: SessionManager,
    next_cont_id: AtomicU64,
    next_session_id: AtomicU64,
    /// Spawns a worker for a [`SessionConfig`] (captures handler builder + ask_tag).
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
        let preamble = repl_preamble(&cfg);
        let effect_stack = tidepool_mcp::build_effect_stack_type(&cfg.decls);
        let ask_tag = cfg.ask_tag;
        // Erase H: the spawn closure owns a clone of `base`.
        let spawn: SessionSpawn = Box::new(move |sc| spawn_worker(sc, base.clone(), ask_tag));
        Self::from_spawn(spawn, cfg, preamble, effect_stack)
    }

    /// Build a server where the single session's handler stack comes from
    /// `builder`, invoked once per session open (`session_run` auto-open /
    /// `session_reset`). Use this to give the session its own KV namespace
    /// (e.g. a per-session backing file) while sharing all other construction.
    ///
    /// The `cfg` must already carry the correct `decls` and `ask_tag` (derived
    /// from a representative stack before calling this constructor).
    pub fn new_with_session_builder<H, F>(builder: F, cfg: ReplServerConfig) -> TidepoolReplServer
    where
        H: DispatchEffect<CapturedOutput> + Clone + Send + Sync + 'static,
        F: Fn() -> H + Send + Sync + 'static,
    {
        let preamble = repl_preamble(&cfg);
        let effect_stack = tidepool_mcp::build_effect_stack_type(&cfg.decls);
        let ask_tag = cfg.ask_tag;
        let spawn: SessionSpawn = Box::new(move |sc| spawn_worker(sc, builder(), ask_tag));
        Self::from_spawn(spawn, cfg, preamble, effect_stack)
    }

    /// Shared constructor: assemble the inner from an erased spawn closure.
    fn from_spawn(
        spawn: SessionSpawn,
        cfg: ReplServerConfig,
        preamble: String,
        effect_stack: String,
    ) -> TidepoolReplServer {
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

    /// Spawn the background reaper: periodically reclaim an abandoned suspension
    /// (a parked `ask` never resumed, H2 — only if `continuation_ttl` is set)
    /// and a `Wedged` session (a timed-out turn, H3 — only if `wedged_ttl` is
    /// set). No-op when both TTLs are `None` or there is no tokio runtime (e.g. a
    /// unit test that constructs the server off-runtime).
    fn spawn_reaper(&self) {
        let suspended_ttl = self.inner.cfg.continuation_ttl;
        let wedged_ttl = self.inner.cfg.wedged_ttl;
        let Some(min_ttl) = [suspended_ttl, wedged_ttl].into_iter().flatten().min() else {
            return;
        };
        if tokio::runtime::Handle::try_current().is_err() {
            return;
        }
        // Weak so the reaper does not keep the server alive — it exits the first
        // tick after the last `TidepoolReplServer` is dropped.
        let weak = Arc::downgrade(&self.inner);
        // Sweep several times per (shortest) TTL so effective lateness is ≤ ~TTL.
        let tick = (min_ttl / 4).max(Duration::from_millis(50));
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(tick);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                interval.tick().await;
                let Some(inner) = weak.upgrade() else {
                    break;
                };
                reap_once(&inner, suspended_ttl, wedged_ttl);
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

    fn next_continuation_id(&self) -> ContinuationId {
        ContinuationId(tidepool_mcp::server_common::mint_id(
            &self.inner.next_cont_id,
            "scont",
        ))
    }

    // -- session lifecycle -------------------------------------------------

    /// Spawn a fresh resident worker for the implicit session (a new session id
    /// ⇒ a new include-tree root). Does NOT install it into the manager.
    fn spawn_session(&self) -> WorkerHandle {
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
        (self.inner.spawn)(cfg)
    }

    /// The implicit session's lifecycle state, auto-opening it on first use
    /// (`session_run` needs no explicit open). If a concurrent caller wins the
    /// install race, our freshly-spawned worker is dropped and the winner's
    /// state is returned.
    fn ensure_session(&self) -> Result<SharedState, String> {
        if let Some(s) = self.inner.manager.state() {
            return Ok(s);
        }
        let handle = self.spawn_session();
        if let Err(rejected) = self.inner.manager.install(handle) {
            // Lost the race — someone else installed first. Drop ours cleanly.
            rejected.shutdown();
        }
        self.inner
            .manager
            .state()
            .ok_or_else(|| "session worker vanished immediately after install".to_string())
    }

    // -- tool handlers -----------------------------------------------------

    /// The shared tool-dispatch entry point, with no client cancel signal — a
    /// convenience over [`Self::dispatch_tool_ct`] passing a never-cancelled
    /// token. Tests drive this directly to exercise the exact production path
    /// without constructing a `RequestContext`.
    pub async fn dispatch_tool(
        &self,
        name: &str,
        args: serde_json::Map<String, serde_json::Value>,
    ) -> Result<CallToolResult, McpError> {
        self.dispatch_tool_ct(name, args, CancellationToken::new())
            .await
    }

    /// The work-carrying tool-dispatch entry point. `call_tool` (the MCP
    /// `ServerHandler` method) delegates here, forwarding the request's
    /// `RequestContext.ct` so a client cancel (rmcp cancels this token; it does
    /// NOT drop the handler future) aborts the in-flight turn at a safepoint.
    pub async fn dispatch_tool_ct(
        &self,
        name: &str,
        args: serde_json::Map<String, serde_json::Value>,
        ct: CancellationToken,
    ) -> Result<CallToolResult, McpError> {
        let parse =
            |args: serde_json::Map<String, serde_json::Value>| serde_json::Value::Object(args);
        match name {
            "session_run" => {
                let req: SessionBlockRequest = serde_json::from_value(parse(args))
                    .map_err(|e| McpError::invalid_params(format!("invalid params: {e}"), None))?;
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
                let verbose = req.verbose.unwrap_or(false);
                Ok(self
                    .run_command(
                        "session_run",
                        SessionCommand::Block {
                            items: block_items,
                            verbose,
                        },
                        input,
                        ct,
                    )
                    .await)
            }
            "session_resume" => {
                let req: SessionResumeRequest = serde_json::from_value(parse(args))
                    .map_err(|e| McpError::invalid_params(format!("invalid params: {e}"), None))?;
                self.session_resume(req, ct).await
            }
            "session_reset" => Ok(self.session_reset().await),
            other => Err(McpError {
                code: ErrorCode::METHOD_NOT_FOUND,
                message: format!("Tool not found: {other}").into(),
                data: None,
            }),
        }
    }

    /// Send a `SessionCommand` to the resident worker and await its reply,
    /// auto-opening the session on first use. `eval_input` is forwarded so
    /// `input :: Aeson.Value` is in scope for the first eval item in a block.
    async fn run_command(
        &self,
        op: &str,
        cmd: SessionCommand,
        eval_input: Option<serde_json::Value>,
        ct: CancellationToken,
    ) -> CallToolResult {
        let state = match self.ensure_session() {
            Ok(s) => s,
            Err(e) => return CallToolResult::error(vec![Content::text(e)]),
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
                    "session is {label}; resume it (or session_reset) before running again"
                ))]);
            }
            *st = SessionState::Busy;
        }
        let Some(sender) = self.inner.manager.get_sender() else {
            *state.lock() = SessionState::Idle;
            return CallToolResult::error(vec![Content::text("session worker is gone")]);
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
            .cancel_slot()
            .unwrap_or_else(empty_cancel_slot);
        self.drive_detached(
            op,
            session_rx,
            response_tx,
            gate,
            captured,
            DriveCtl { state, cancel },
            ct,
        )
        .await
    }

    /// `session_reset`: tear down the current session (releasing a parked `ask`
    /// and aborting a runaway) and open a fresh resident machine. The universal
    /// get-unstuck button — abort folds into it, so resetting while suspended
    /// drops the pending continuation.
    async fn session_reset(&self) -> CallToolResult {
        self.teardown_current().await;
        match self.ensure_session() {
            Ok(_) => CallToolResult::success(vec![Content::text(
                serde_json::json!({"reset": true}).to_string(),
            )]),
            Err(e) => CallToolResult::error(vec![Content::text(e)]),
        }
    }

    /// Tear down the current session if one is present: abort a runaway at a JIT
    /// safepoint, RELEASE a parked `ask` suspension (setting `Closing` drops the
    /// old `SessionState` — and with it the `Suspension`'s `response_tx` — so a
    /// worker blocked on `response_rx` unblocks, unwinds the turn, and returns to
    /// its command loop where it observes the `Close`), then close the worker.
    /// The `Close` ack is awaited: on a clean ack the worker is joined; a
    /// pure-compute runaway that never reads `Close` is DETACHED (`join()` would
    /// hang forever on an uninterruptible thread).
    async fn teardown_current(&self) {
        // Abort a runaway turn at a JIT safepoint so a Busy session tears down
        // promptly instead of waiting out the ack timeout (no-op if idle).
        if let Some(cancel) = self.inner.manager.cancel_slot() {
            if let Some(h) = cancel.lock().as_ref().cloned() {
                h.cancel();
            }
        }
        if let Some(state) = self.inner.manager.state() {
            *state.lock() = SessionState::Closing;
        }
        let Some(handle) = self.inner.manager.remove() else {
            return;
        };
        let (session_tx, mut session_rx) = tokio::sync::mpsc::unbounded_channel::<WorkerMessage>();
        let (_response_tx, response_rx) = std::sync::mpsc::channel::<ResumeMsg>();
        let job = WorkerJob {
            cmd: SessionCommand::Close,
            session_tx,
            response_rx,
            gate: PauseGate::new(),
            captured: CapturedOutput::new(),
            eval_input: None,
        };
        let _ = handle.sender().send(job);
        let acked = timeout(Duration::from_secs(30), session_rx.recv())
            .await
            .is_ok();
        if acked {
            handle.shutdown();
        } else {
            handle.detach();
        }
    }

    async fn session_resume(
        &self,
        req: SessionResumeRequest,
        ct: CancellationToken,
    ) -> Result<CallToolResult, McpError> {
        // Validate + canonicalize the reply against the suspension's schema
        // BEFORE consuming the continuation. This (a) makes `ask` return a
        // STRUCTURED, optic-extractable Value — a reply that arrived as a JSON
        // string is parsed into the canonical shape (BUG-9) — and (b) leaves an
        // invalid reply's continuation un-consumed so the caller can retry.
        // Mirrors the eval server's resume (tidepool-mcp/src/server.rs).
        let Some(state) = self.inner.manager.state() else {
            return Err(McpError::invalid_params(
                format!(
                    "no session is running; continuation_id {} cannot be resumed \
                     (run session_run to start one)",
                    req.continuation_id
                ),
                None,
            ));
        };
        // All under the per-session state lock; we extract the owned `Suspension`
        // and DROP the lock before `drive().await` (never hold it across await).
        let suspension = {
            let mut st = state.lock();
            // Must be Suspended on the matching continuation. Three distinguishable
            // causes on mismatch: suspended on a DIFFERENT continuation, or not
            // suspended at all (already spent or never existed).
            let schema = match &*st {
                SessionState::Suspended(s) if s.cont_id == req.continuation_id => {
                    s.expected_schema.clone()
                }
                SessionState::Suspended(s) => {
                    return Err(McpError::invalid_params(
                        format!(
                            "session is suspended on continuation {}, not {}; resume the \
                             pending one (or session_reset to drop it)",
                            s.cont_id, req.continuation_id
                        ),
                        None,
                    ));
                }
                other => {
                    return Err(McpError::invalid_params(
                        format!(
                            "session is not awaiting a resume (state: {}); continuation_id {} \
                             is already spent or never existed",
                            other.busy_label(),
                            req.continuation_id
                        ),
                        None,
                    ));
                }
            };
            match tidepool_mcp::validate::validate_response(schema.as_ref(), &req.response) {
                tidepool_mcp::validate::Outcome::Invalid(violations) => {
                    // Anti-starvation: a retrying continuation must not become the
                    // reaper's oldest-first eviction victim while its caller fixes
                    // the reply. Stays Suspended (un-consumed).
                    if let SessionState::Suspended(s) = &mut *st {
                        s.since = Instant::now();
                    }
                    let msg = tidepool_mcp::server_common::validation_failed_body(
                        "session_resume",
                        "session_reset",
                        &violations,
                        schema.as_ref(),
                        &req.continuation_id.0,
                    );
                    return Ok(CallToolResult::error(vec![Content::text(msg)]));
                }
                tidepool_mcp::validate::Outcome::Valid(canonical) => {
                    // Take the suspension out → Busy (the turn is resuming).
                    // Suspended was confirmed under this same lock above, so this
                    // yields the payload without a re-match-and-panic.
                    let Some(s) = take_suspension(&mut st) else {
                        return Err(McpError::internal_error(
                            "session state changed under lock (expected Suspended)",
                            None,
                        ));
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
            .cancel_slot()
            .unwrap_or_else(empty_cancel_slot);
        Ok(self
            .drive_detached(
                "session_resume",
                suspension.session_rx,
                suspension.response_tx,
                suspension.gate,
                suspension.captured,
                DriveCtl { state, cancel },
                ct,
            )
            .await)
    }

    /// Resolve a turn's state on a DETACHED task, decoupled from this RPC
    /// caller's future. [`Self::drive`] is the single writer of terminal state
    /// (`Idle`/`Suspended`/`Wedged`); running it on its own `tokio::task` means a
    /// cancelled or dropped RPC future can no longer strand the state at `Busy`
    /// (the cancel-wedge). The turn always resolves its own state.
    ///
    /// The RPC side races the resolver against `ct` (rmcp cancels this token on a
    /// client cancel; it does NOT drop the handler future over stdio). On a
    /// cancel we fire the SAME cooperative-abort levers the timeout path uses
    /// (`request_abort` + the machine's `CancelHandle`), then give a bounded
    /// grace for a prompt stop — after which the detached resolver keeps owning
    /// final state (self-heal `Idle` or `Wedged`) while the caller returns. No
    /// arm/disarm latch is needed: a spurious `cancel()` is cleared by the next
    /// turn's `Session::run_turn` → `reset_cancel` (`session.rs`).
    #[allow(clippy::too_many_arguments)]
    async fn drive_detached(
        &self,
        op: &str,
        session_rx: tokio::sync::mpsc::UnboundedReceiver<WorkerMessage>,
        response_tx: std::sync::mpsc::Sender<ResumeMsg>,
        gate: Arc<PauseGate>,
        captured: CapturedOutput,
        ctl: DriveCtl,
        ct: CancellationToken,
    ) -> CallToolResult {
        // Abort levers cloned out before the rest moves into the resolver task.
        let gate_abort = Arc::clone(&gate);
        let cancel_abort = ctl.cancel.clone();
        let op_owned = op.to_string();
        let this = self.clone();
        let (result_tx, mut result_rx) = tokio::sync::oneshot::channel::<CallToolResult>();
        tokio::spawn(async move {
            let r = this
                .drive(&op_owned, session_rx, response_tx, gate, captured, ctl)
                .await;
            // Err only if the RPC side already returned (grace expired / future
            // dropped); state is resolved regardless, so the drop is harmless.
            let _ = result_tx.send(r);
        });

        tokio::select! {
            r = &mut result_rx => r.unwrap_or_else(|_| {
                CallToolResult::error(vec![Content::text(format!(
                    "{op}: turn resolver task ended without a result (internal error)"
                ))])
            }),
            _ = ct.cancelled() => {
                // Client asked to stop. Signal abort on both fronts — the same
                // levers the timeout branch of `drive` uses — then let the
                // resolver record the real terminal state.
                gate_abort.request_abort(format!("{op} cancelled by client"));
                if let Some(h) = cancel_abort.lock().as_ref().cloned() {
                    h.cancel();
                }
                match timeout(Duration::from_secs(ABORT_GRACE_SECS), &mut result_rx).await {
                    Ok(Ok(r)) => r,
                    Ok(Err(_)) => CallToolResult::error(vec![Content::text(format!(
                        "{op}: turn resolver task ended without a result (internal error)"
                    ))]),
                    // Uninterruptible turn: don't block the caller on the full
                    // turn budget. The detached resolver keeps owning final
                    // state (it self-heals to Idle or goes Wedged).
                    Err(_) => CallToolResult::error(vec![Content::text(format!(
                        "{op} cancelled; the turn is stopping and the session will be ready \
                         shortly (or wedged if uninterruptible — session_reset to force-recover)"
                    ))]),
                }
            }
        }
    }

    /// Await the next worker message for an in-flight turn, mapping it to an MCP
    /// result AND driving the session's [`SessionState`] transition. The state
    /// arrived `Busy` (set by the caller); this resolves it to `Idle` (turn
    /// finished), `Suspended` (parked an `ask` — the suspension payload, incl.
    /// `response_tx`, is stored IN the state), or `Wedged` (timeout / crash).
    /// Runs on a detached task (see [`Self::drive_detached`]) so its terminal
    /// writes survive a cancelled/dropped RPC future.
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
                // Snapshot whether an effect handler was active at the moment
                // of timeout BEFORE requesting abort (abort changes the gate
                // state, not in_effect, but reading early is clearest).
                let effect_in_flight = gate.is_in_effect();
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
                let wedged_msg = if effect_in_flight {
                    // The worker was blocked inside an effect handler (e.g.
                    // an Exec/Http/Lsp call) when the timeout fired. The JIT
                    // is not running so the JIT cancel has no effect; the
                    // effect handler will continue until its external call
                    // completes. Any spawned child process is NOT killed — it
                    // runs to completion on its own. The session is wedged
                    // for the reaper (session_reset will reclaim it).
                    format!(
                        "{op} timed out after {to_secs}s while an effect was in \
                         flight (an external call — e.g. a spawned process or \
                         network request — was still running). Raise timeout_secs \
                         or check the external command duration. Any spawned child \
                         process was NOT killed. The session is wedged; session_reset \
                         to recover."
                    )
                } else {
                    // The worker was in pure JIT computation with no effect
                    // dispatch in progress — likely an infinite loop or
                    // unbounded recursion. The JIT cancel was signalled but
                    // the thread did not abort within the grace period.
                    format!(
                        "{op} timed out after {to_secs}s on pure JIT computation \
                         (no effect boundary reached; likely an infinite loop or \
                         unbounded recursion). The session is wedged; session_reset \
                         to recover."
                    )
                };
                return CallToolResult::error(vec![Content::text(wedged_msg)]);
            }
        };
        match received {
            Some(WorkerMessage::Completed { result }) => {
                *state.lock() = SessionState::Idle;
                let out = captured.drain();
                CallToolResult::success(vec![Content::text(
                    tidepool_mcp::server_common::format_with_output(&out, &result),
                )])
            }
            Some(WorkerMessage::Error { error }) => {
                *state.lock() = SessionState::Idle;
                let out = captured.snapshot();
                CallToolResult::error(vec![Content::text(
                    tidepool_mcp::server_common::format_with_output(&out, &error),
                )])
            }
            Some(WorkerMessage::Suspended { prompt, meta }) => {
                let cont_id = self.next_continuation_id();
                let (json_obj, expected_schema) =
                    tidepool_mcp::server_common::build_suspension_envelope(
                        &cont_id.0, &prompt, meta,
                    );
                // The suspension payload lives IN the state — a parked `ask` can't
                // exist untracked, so teardown (reset/reaper) is forced to release
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

    /// The live `tidepool://session/bindings` body: the worker's last-published
    /// snapshot, or the empty-session shape if no session has opened yet.
    fn session_bindings_body(&self) -> String {
        match self.inner.manager.bindings_slot() {
            Some(slot) => slot.lock().to_string(),
            None => serde_json::json!({
                "bindings": [],
                "generation": 0,
                "valGeneration": 0,
            })
            .to_string(),
        }
    }
}

/// One reaper sweep: reclaim the suspension / wedge if older than `ttl`.
///
/// - An abandoned `Suspended` (never resumed) → `Idle`: dropping the
///   `Suspension` drops its `response_tx`, so the parked worker's
///   `response_rx.recv()` errors, the turn unwinds, and the worker returns to
///   its command loop — the session stays alive and usable (H2).
/// - A stale `Wedged` (timed-out turn) → removed, freeing the slot. The worker
///   is DETACHED, not joined: a pure-compute runaway can't be joined without
///   hanging.
fn reap_once(
    inner: &ReplServerInner,
    suspended_ttl: Option<Duration>,
    wedged_ttl: Option<Duration>,
) {
    let now = Instant::now();
    let Some(state) = inner.manager.state() else {
        return;
    };
    let remove_wedged = {
        let mut st = state.lock();
        match &*st {
            SessionState::Suspended(s)
                if suspended_ttl.is_some_and(|ttl| now.duration_since(s.since) >= ttl) =>
            {
                *st = SessionState::Idle;
                false
            }
            SessionState::Wedged { since }
                if wedged_ttl.is_some_and(|ttl| now.duration_since(*since) >= ttl) =>
            {
                *st = SessionState::Closing;
                true
            }
            _ => false,
        }
    };
    if remove_wedged {
        if let Some(handle) = inner.manager.remove() {
            handle.detach();
        }
    }
}

fn build_tool_description(decls: &[EffectDecl]) -> String {
    // DERIVED from the decls (tidepool_mcp::describe): the same effect index
    // (name + first-sentence + verb names) the eval tool description and
    // `:browse` render — one source, so the three surfaces can't drift.
    let effects = describe_effects_index(decls);
    format!(
        "tidepool-repl — a GHCi-style stateful Haskell session. ONE resident JIT machine whose \
         value heap and module scope persist across turns; declarations accumulate across \
         `session_run` calls.\n\n\
         WHY STATEFUL (vs one-shot eval) — the session IS your typed working memory across \
         turns, and that is the whole reason to reach for the repl. Lean on it:\n\
         • BUILD UP: bind an expensive substrate ONCE (a corpus, a parsed graph, an API \
         result) and interrogate it over many cheap turns — no re-fetch, no re-derive.\n\
         • ACCUMULATE: grow a result across turns by rebinding a name from its own prior \
         value — `acc <- pure (x : acc)` reads the old `acc` and shadows it (GHCi `>>=` \
         semantics, not a recursive `let`). Define helpers early; refine them turn over turn.\n\
         • KEEP IT OFF-CONTEXT: a big intermediate lives in the session heap, NOT your \
         context window. Fold it IN the session (count / group / sort / join) and return only \
         the conclusion — aggregate, don't dump. A value too large to render is auto-stubbed \
         but stays a live binding you can keep computing on.\n\
         • DERIVE what static tools can't: the sharpest wins aren't a faster grep — they are \
         signals no grep or call-graph can see, because you fold a whole substrate (a git \
         history, a corpus, an API dump) into a derived metric. Method: substrate once → each \
         turn one composable fold that adds a lens → NORMALIZE for surprise not volume (divide \
         a raw count by a baseline so you rank the anomalous, not the merely busy) → CLASSIFY \
         results into expected vs smell. Reach here when \"what moves/appears together\" matters \
         and static analysis comes up empty.\n\n\
         PRIMARY TOOL: session_run\n\
         Pass a list of items run in sequence: top-level declarations (`data Foo = …`, \
         `f x = …`), bind statements (`x <- e` / `let x = e`), bare expressions, or \
         :commands (`:bindings`, `:reset`, `:t <expr>`, `:i <name>`, `:vocab`, `:browse [Effect]`, \
         `:stub <n>`, `:program`). \
         Items are classified automatically. Execution stops on the first error. \
         Returns per-item results and the last expression's value. \
         The session auto-opens on the first `session_run` — no open step.\n\n\
         DISCOVER VERBS: `:browse` lists every effect + one-line description; \
         `:browse <Effect>` (case-insensitive) lists that effect's verbs (name :: signature) and \
         constructors — reach for it instead of guessing verb names. `:vocab` covers the \
         .tidepool/lib verbs, each module tagged bare (in scope) vs needs-import.\n\n\
         PREFERRED IDIOM — define then call in one block:\n\
         Put helper definitions and type aliases in the early items, then call them in the \
         final expression. One block with a clean definition + its caller beats cramming all \
         logic into a single expression.\n\n\
         ONE DECLARATION PER ITEM: each declaration item compiles as its own module, so a \
         type signature and its binding — and all equations of a multi-clause function — must \
         live in the SAME item (newline-separated), not split across items.\n\n\
         BINDS vs VALUE: end a block with a bare EXPRESSION to populate the top-level `value` \
         and `type` — a block ending in a bind (`x <- e` / `let x = e`) leaves `value` null. \
         A bare `x = 5` (no `let`) is a top-level DECLARATION; use `let x = 5` to bind.\n\n\
         RESPONSE SHAPE: slim by default — per-item inline objects (`kind`, `ok`, plus result \
         fields merged in), final-expression value at top-level `value`/`type` only. \
         Example bind: {{\"kind\":\"stmt\",\"ok\":true,\"bound\":\"vs\",\"type\":\"[Text]\"}}. \
         Example decl: {{\"kind\":\"decl\",\"ok\":true,\"decl\":\"slug\",\"type\":\"Text -> Text\"}} \
         — a value decl also carries the inferred `type` the server had at compile time (no \
         `:t` needed); omitted for type/class/data/import decls and on a type-probe miss. \
         Pass `verbose: true` to get the full diagnostic shape \
         (per-item `index`, `generation` counters, double-encoded `result` string).\n\n\
         JSON OUTPUT: opt-in — return an `Aeson.Value` to get structured JSON instead of \
         Show output.\n\n\
         EFFECTS (invoke via the helper verbs; `:browse <Effect>` for its constructors + full \
         signatures):\n{effects}\n\
         LIFECYCLE: `session_run` auto-opens the session; `session_reset` drops the resident \
         machine and starts fresh (and drops any pending `ask`). \
         An in-turn `ask` suspends with a continuation_id; answer it with session_resume \
         (resetting while suspended drops it).\n\n\
         LIVE STATE: the `tidepool://session/bindings` resource serves the current \
         environment as JSON (name/type/kind/generation per binding).\n\n\
         RECORDS — effect results are named records, not tuples. Use record-dot syntax: \
         `run cmd` → `Either <EffectError> Proc` — bind the `Right` (`Right p <- run cmd`) and \
         read `p.stdout`, `p.exitCode`, `p.stderr` (`ok p` = zero exit); \
         `grepGlob`/`searchFiles` → `[Hit]` (`h.path`, `h.line`, `h.text`); \
         `readGlob` → `[FileRead]` (`r.path`, `r.contents :: Either FsError Text`). \
         Bare selectors like `stdout p` are ambiguous — always use dot syntax.\n\n\
         SURFACE NOTES — the OverloadedRecordDot extension is ON: `x.f` is field access, so \
         write function composition WITH SPACES (`f . g`); `f.g` parses as projecting field \
         `g` and will not typecheck. Common partial functions (`head`, `tail`, `!!`, \
         `fromJust`) are deliberately unsupported — they compile-error naming the total form \
         (`L.head`, `atMay`, or a pattern-match generator `(x:_) <- …`); reach for those \
         directly rather than the partial.",
    )
}

// ---------------------------------------------------------------------------
// ServerHandler
// ---------------------------------------------------------------------------

impl ServerHandler for TidepoolReplServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            instructions: Some(self.inner.tool_description.clone()),
            capabilities: ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .build(),
            ..Default::default()
        }
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        // rmcp cancels `context.ct` on a client `CancelledNotification` (it does
        // NOT drop this future over stdio) — forward it so an interrupted turn
        // aborts at a safepoint instead of wedging the session.
        self.dispatch_tool_ct(
            request.name.as_ref(),
            request.arguments.unwrap_or_default(),
            context.ct,
        )
        .await
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        let tools = vec![
            tidepool_mcp::server_common::make_tool(
                "session_run",
                "Run a list of GHCi-capable items in sequence on the resident machine (the \
                 session auto-opens on first use). Each item is a declaration (`data Foo = …`, \
                 `f x = …`), a bind statement (`x <- e` / `let x = e`), a bare expression, or a \
                 `:command` (`:bindings`, `:reset`, `:t <expr>`, `:i <name>`, `:vocab`, \
                 `:browse [Effect]`, `:stub <n>`, `:program`). \
                 Items are classified automatically; \
                 execution stops on the first error. Returns slim per-item inline JSON plus the \
                 last expression's `value` and `type` at the top level. \
                 DISCOVER VERBS with `:browse` (bare = all effects + descriptions; \
                 `:browse <Effect>` = that effect's verbs + constructors) rather than guessing \
                 verb names; `:vocab` lists .tidepool/lib verbs tagged bare vs needs-import. \
                 PREFERRED IDIOM: define helpers and types in early items, then invoke them in \
                 the final expression — one block with a clean definition plus its caller beats \
                 one cramped expression. Each declaration item is its own module, so a type \
                 signature and its binding (and a multi-clause function's equations) must share \
                 ONE newline-separated item. \
                 JSON OUTPUT: return an `Aeson.Value` to get structured JSON instead of Show \
                 output. Pass `verbose: true` for the full diagnostic shape (generation counters, \
                 double-encoded result strings). \
                 An in-turn `ask` suspends with a continuation_id; resume with session_resume \
                 or drop it with session_reset.",
                tidepool_mcp::server_common::schema_to_map(schemars::schema_for!(
                    SessionBlockRequest
                ))
                .map_err(|e| McpError::internal_error(e, None))?,
            ),
            tidepool_mcp::server_common::make_tool(
                "session_resume",
                "Answer an in-turn `ask` suspension (continuation_id from a {\"suspended\":true} \
                 result) and run the turn to completion. A reply that doesn't match the \
                 suspension's schema is rejected WITHOUT consuming the continuation, so it can \
                 be retried.",
                tidepool_mcp::server_common::schema_to_map(schemars::schema_for!(
                    SessionResumeRequest
                ))
                .map_err(|e| McpError::internal_error(e, None))?,
            ),
            tidepool_mcp::server_common::make_tool(
                "session_reset",
                "Drop the resident machine (freeing its heap and all bindings) and open a fresh \
                 session. Also drops any pending `ask` continuation — the universal \
                 get-unstuck button (abort folds into reset). Takes no arguments.",
                tidepool_mcp::server_common::schema_to_map(schemars::schema_for!(EmptyRequest))
                    .map_err(|e| McpError::internal_error(e, None))?,
            ),
        ];
        Ok(ListToolsResult {
            tools,
            next_cursor: None,
            meta: None,
        })
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        let resources = vec![RawResource {
            uri: SESSION_BINDINGS_URI.to_string(),
            name: "Session bindings".to_string(),
            title: None,
            description: Some(
                "Live session environment as JSON: one entry per in-scope binding \
                 (name, type, kind = decl|bind, generation), plus the decl/value generation \
                 counters. Refreshed after every turn."
                    .to_string(),
            ),
            mime_type: Some("application/json".to_string()),
            size: None,
            icons: None,
            meta: None,
        }
        .no_annotation()];
        Ok(ListResourcesResult {
            resources,
            next_cursor: None,
            meta: None,
        })
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResult, McpError> {
        if request.uri == SESSION_BINDINGS_URI {
            Ok(ReadResourceResult {
                contents: vec![ResourceContents::TextResourceContents {
                    uri: request.uri,
                    mime_type: Some("application/json".to_string()),
                    text: self.session_bindings_body(),
                    meta: None,
                }],
            })
        } else {
            Err(McpError::resource_not_found(
                format!("Unknown resource: {}", request.uri),
                None,
            ))
        }
    }
}

/// An empty request schema — `session_reset` takes no arguments.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct EmptyRequest {}
