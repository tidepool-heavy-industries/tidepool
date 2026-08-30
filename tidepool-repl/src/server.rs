//! The `tidepool-repl` MCP server — a SEPARATE server/binary from the `tidepool`
//! eval server (whose request path is untouched). It exposes ONE implicit
//! session over three tools and one live-state resource.
//!
//! Tools:
//! - `session_run` — run a list of GHCi-capable items (decls, binds, exprs,
//!   :commands). Auto-opens the session on first use.
//! - `session_resume` — answer an in-turn `ask` suspension (the threadless
//!   stow-as-data mechanism shared with the eval server and the harness).
//! - `session_reset` — drop the resident machine and open a fresh one; also
//!   drops any pending `ask` continuation (abort folds into reset).
//!
//! Resource:
//! - `tidepool://session/bindings` — read-only JSON over live session state.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use rmcp::{model::*, service::RequestContext, ErrorData as McpError, RoleServer, ServerHandler};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tidepool_effect::dispatch::DispatchEffect;
use tidepool_mcp::{describe_effects_index, CapturedOutput, EffectDecl, EffectRoster};
use tidepool_repr::{MonotonicIdIssuer, SessionId};
use tidepool_runtime::session::{
    classify_workbench_item, GraceOutcome, ModuleEnv, TurnSupervisor, WorkbenchItem,
};
use tokio::time::{timeout, Duration};
use tokio_util::sync::CancellationToken;

use tidepool_effect::pause::PauseGate;

use tidepool_runtime::session::registry::{CheckoutReceipt, SlotKind};
use tidepool_runtime::session::SuspendableSession;

use crate::command::{BlockItem, DeclText, ExprText, MetaCommand, SessionCommand};
use crate::kernel_adapter::{SlotContext, SlotHole};
use crate::manager::{empty_cancel_slot, CancelSlot, ContinuationId, SessionManager};
use crate::session::{BoxedStack, Session, SessionConfig, TurnStep, DEFAULT_NURSERY_SIZE};

/// The `tidepool://session/bindings` resource URI: read-only JSON over the live
/// session environment (decl plane + value/pure binds).
const SESSION_BINDINGS_URI: &str = "tidepool://session/bindings";

/// Per-turn window before a turn is declared timed out. A runaway that outruns
/// its abort grace takes the session with it (the session is moved INTO the
/// turn's blocking task, so an unreturning task holds it); the window keeps a
/// single MCP call from hanging forever.
const TURN_TIMEOUT_SECS: u64 = 600;

/// After a turn times out and is cancelled, how long to wait for it to abort at
/// a JIT safepoint before declaring the session `Wedged`. Allocating /
/// tail-recursive runaways abort within milliseconds of `cancel()`; this margin
/// only covers scheduling. A turn that doesn't abort in this window is treated
/// as genuinely uninterruptible, and its session is gone with it.
const ABORT_GRACE_SECS: u64 = 3;

/// What a caller loses when a turn wedges, stated at the one moment it is true.
///
/// A wedged turn holds the only copy of its `Session` (it was MOVED into the
/// blocking closure), so the manager entry is dropped and the session's
/// declarations and value bindings go with it. The next `session_run` auto-opens
/// a fresh session exactly as it does from cold.
///
/// This rides the timeout/crash error the caller is already being handed — it is
/// NOT a breadcrumb stored for a later turn to report. Nothing new is recorded;
/// the notice is emitted at the instant the loss happens, and never again.
///
/// Why it is load-bearing rather than politeness: the repl's whole contract is
/// that the session IS the caller's working memory across turns. Losing it after
/// a runaway may be unavoidable; losing it SILENTLY is a trap — a later turn
/// would fail with "Variable not in scope: <name>" and read as a Haskell error
/// rather than as the reclaim that actually caused it.
///
/// `session_reset` would not help here either: the entry is already gone, so
/// there is nothing left to reset.
const RECLAIMED_NOTICE: &str = "The session was reclaimed: its declarations and \
     bindings are GONE. The next session_run opens a fresh, empty session — no \
     session_reset needed.";

/// Build the error text for a turn that wedged (timed out and did not abort
/// within the grace period). Split out as a pure function so every wedge path
/// carries [`RECLAIMED_NOTICE`] by construction rather than by three separate
/// authors remembering to append it — and so the message is unit-testable
/// WITHOUT driving a real runaway, which is the one wedge transition that has no
/// reliable test seam (entry INTO `Wedged` needs a JIT-cancel-resistant runaway
/// or a seam to simulate one; the reclaim paths OUT of it are pinned).
///
/// `effect_in_flight` distinguishes a turn blocked inside an effect handler (an
/// external call still running, the JIT idle so the JIT cancel cannot bite) from
/// a pure-compute runaway that outran the cancel.
fn wedged_message(op: &str, to_secs: u64, effect_in_flight: bool) -> String {
    if effect_in_flight {
        // Blocked inside an effect handler (e.g. Exec/Http) when the
        // timeout fired. The handler runs on until its external call returns,
        // and any spawned child process is NOT killed.
        format!(
            "{op} timed out after {to_secs}s while an effect was in flight (an \
             external call — e.g. a spawned process or network request — was \
             still running). Raise timeout_secs or check the external command \
             duration. Any spawned child process was NOT killed. \
             {RECLAIMED_NOTICE}"
        )
    } else {
        // Pure JIT computation with no effect dispatch in progress — likely an
        // infinite loop or unbounded recursion. The JIT cancel was signalled
        // but the turn did not abort within the grace period.
        format!(
            "{op} timed out after {to_secs}s on pure JIT computation (no effect \
             boundary reached; likely an infinite loop or unbounded recursion). \
             {RECLAIMED_NOTICE}"
        )
    }
}

/// The manager-side handles for the session whose turn [`TidepoolReplServer::drive`]
/// awaits: the [`CancelSlot`] read on timeout to abort a runaway at a JIT
/// safepoint, and this turn's checkout [`CheckoutReceipt`] — settled against
/// a stale epoch (a `session_reset` swapped the entry mid-turn) drops the
/// session instead of clobbering the fresh one; the receipt itself
/// guarantees the epoch it carries can't be separated from the session it
/// settles. Bundled so `drive` stays within the argument-count budget.
struct DriveCtl {
    cancel: CancelSlot,
    receipt: CheckoutReceipt,
}

/// One turn's blocking execution: the `Session` moves in and comes back out —
/// the shape of `tidepool_harness::Harness::run_checked_out`.
struct TurnRun {
    session: Box<Session>,
    step: TurnStep,
}

/// Run one turn with the session MOVED into the blocking pool and returned out.
///
/// Inside the blocking task the work happens on a freshly-spawned big-stack
/// thread: deep JIT recursion needs [`tidepool_runtime::EVAL_STACK_SIZE`], which
/// tokio's blocking pool does not give. Mirrors
/// `ResidentSession::on_eval_thread`, one level up (the whole session crosses,
/// not just the machine).
fn spawn_turn<F>(mut session: Box<Session>, body: F) -> tokio::task::JoinHandle<TurnRun>
where
    F: FnOnce(&mut Session) -> TurnStep + Send + 'static,
{
    tokio::task::spawn_blocking(move || {
        let joined = std::thread::scope(|scope| {
            #[allow(
                clippy::expect_used,
                reason = "spawn_scoped a named thread with a fixed stack size does not fail in practice"
            )]
            std::thread::Builder::new()
                .name("tidepool-repl-turn".into())
                .stack_size(tidepool_runtime::EVAL_STACK_SIZE)
                .spawn_scoped(scope, || {
                    // Install SIGILL/SIGSEGV handlers so a JIT fault yields a
                    // clean error instead of killing the process.
                    tidepool_codegen::signal_safety::install();
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| body(&mut session)))
                })
                .expect("spawn tidepool-repl turn thread")
                .join()
        });
        let step = match joined {
            Ok(Ok(step)) => step,
            // A Rust-level panic that unwound past the JIT's own signal
            // protection. The session itself is intact and comes back — only
            // this turn failed.
            Ok(Err(payload)) | Err(payload) => {
                TurnStep::Completed(crate::command::TurnOutcome::Error(format!(
                    "session turn panicked: {}",
                    panic_message(payload)
                )))
            }
        };
        TurnRun { session, step }
    })
}

/// Render a caught panic payload as a human string.
fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic payload".to_string()
    }
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
/// Classification strategy (try-cascade):
/// - `:` prefix → [`BlockItem::Meta`] via the shared command lexer and
///   this frontend's [`MetaCommand`] extensions.
/// - Keyword-initiated declarations (`data`, `newtype`, `type`, `class`,
///   `instance`, …) → [`BlockItem::Decl`] (unambiguous; skip cascade).
/// - Everything else → [`BlockItem::Auto`]: `run_block` will attempt the item
///   as a declaration via `run_def` first; on a GHC parse error it falls back
///   to [`BlockItem::Stmt`]/`run_eval`.
///
/// Misclassification fails LOUD — the wrong handler's GHC error surfaces
/// immediately rather than silently producing a wrong result.
pub fn classify_item(text: &str) -> Result<BlockItem, String> {
    match classify_workbench_item(text)? {
        WorkbenchItem::Declaration(source) => Ok(BlockItem::Decl(DeclText(source))),
        WorkbenchItem::Haskell(source) => Ok(BlockItem::Auto(ExprText(source))),
        WorkbenchItem::Command(line) => MetaCommand::from_line(line).map(BlockItem::Meta),
    }
}

// ---------------------------------------------------------------------------
// Server config + inner
// ---------------------------------------------------------------------------

/// Static config for the server (everything but the per-session root, which is
/// minted per session open).
pub struct ReplServerConfig {
    /// The single-sourced effect roster built from the installed handlers.
    pub roster: EffectRoster,
    /// Base GHC include dirs (generated `Tidepool.Effects` dir + prelude/stdlib).
    pub base_include: Vec<PathBuf>,
    /// Import/pragma surface for generated `Lib.G<g>` decl modules.
    pub module_env: ModuleEnv,
    /// Parent dir under which per-session include trees are created.
    pub session_root_base: PathBuf,
    /// Session nursery size in bytes. `None` ⇒ [`DEFAULT_NURSERY_SIZE`] (64 MiB).
    /// Tests shrink it to force an organic GC between turns.
    pub nursery_size: Option<usize>,
    /// How long a suspended `ask` may linger before the reaper aborts it and
    /// returns the session to `Idle`. `None` ⇒ suspensions never expire — the
    /// production default (`main.rs`): a stowed ask holds no thread, only its
    /// JIT machine's heap, an acceptable cost for long-open knots. Tests set it
    /// small to exercise the reap path.
    pub continuation_ttl: Option<Duration>,
    /// How long a `Wedged` session (a timed-out turn) may linger before the
    /// reaper closes and removes it. `None` ⇒ no wedged sweep. `main.rs` keeps
    /// ~30 min — a wedged session is dead weight, unlike a parked ask.
    pub wedged_ttl: Option<Duration>,
    /// Wall-clock budget for a single turn before it is cancelled at a JIT
    /// safepoint (see [`drive`]). `None` ⇒ [`TURN_TIMEOUT_SECS`] (600 s). Tests
    /// shrink it to exercise the timeout/self-heal path fast.
    pub turn_timeout: Option<Duration>,
    /// Project/global verb-library dirs (project first), as resolved by
    /// [`tidepool_mcp::server_common::resolve_lib_dirs`]. Backs the shared
    /// `tidepool://vocab` resource — see [`ResourceCtx`](tidepool_mcp::resources::ResourceCtx).
    pub lib_dirs: Vec<PathBuf>,
    /// The vendored Tidepool stdlib dir (`Tidepool/*.hs`), if resolved. Backs
    /// `tidepool://capabilities` and `tidepool://stdlib/{module}`.
    pub stdlib_dir: Option<PathBuf>,
    /// `PATTERNS.md` beside the active `Library.hs`, if present. Backs
    /// `tidepool://patterns`.
    pub patterns_path: Option<PathBuf>,
}

/// Opens a [`Session`] for a [`SessionConfig`] — the erased handler-stack
/// builder (H is hidden behind this boxed closure, and behind the per-turn
/// [`BoxedStack`] factory the session it builds carries).
type SessionSpawn = Box<dyn Fn(SessionConfig) -> std::io::Result<Session> + Send + Sync>;

/// Whether a project/global `Library` facade is on the include path. When true
/// the preamble emits `import Library` so `.tidepool/lib` verbs are in scope
/// (parity with the eval server). Derived from `base_include` so no extra config
/// field / test-harness churn: `main.rs` puts the lib dirs on `base_include`.
fn has_user_library(cfg: &ReplServerConfig) -> bool {
    cfg.base_include
        .iter()
        .any(|d| d.join("Library.hs").exists())
}

/// The repl's eval preamble: non-interactive pagination in
/// [`tidepool_mcp::PaginateMode::Passthrough`], so oversized results reach
/// Rust in full and are truncated by [`crate::truncate::truncate_result`]
/// instead — which can stash the elided subtrees for `:stub <n>` (Haskell-side
/// truncation discards them, leaving `stub_N` markers nothing could fetch).
fn repl_preamble(cfg: &ReplServerConfig) -> String {
    tidepool_mcp::build_preamble_non_interactive_mode(
        cfg.roster.decls(),
        has_user_library(cfg),
        tidepool_mcp::PaginateMode::Passthrough,
    )
}

/// The non-generic server core (H is erased into the `spawn` closure).
struct ReplServerInner {
    manager: SessionManager,
    next_cont_id: MonotonicIdIssuer,
    next_session_id: AtomicU64,
    /// Opens a session for a [`SessionConfig`] (captures the handler builder).
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
        let effect_stack = tidepool_mcp::build_effect_stack_type(cfg.roster.decls());
        // Erase H twice over: the spawn closure owns a clone of `base` per
        // SESSION, and hands the session a factory that clones it again per
        // TURN.
        let spawn: SessionSpawn = Box::new(move |sc| {
            let h = base.clone();
            Session::open(sc, Box::new(move || Box::new(h.clone()) as BoxedStack))
        });
        Self::from_spawn(spawn, cfg, preamble, effect_stack)
    }

    /// Build a server where the single session's handler stack comes from
    /// `builder`, invoked once per session open (`session_run` auto-open /
    /// `session_reset`). Use this to give the session its own KV namespace
    /// (e.g. a per-session backing file) while sharing all other construction.
    ///
    /// The `cfg` must already carry the correct `roster` (built via
    /// `EffectRoster::from_handlers` against a representative stack before
    /// calling this constructor — the roster's own constructor is what makes
    /// its decls/suspend-tag pairing structurally correct).
    pub fn new_with_session_builder<H, F>(builder: F, cfg: ReplServerConfig) -> TidepoolReplServer
    where
        H: DispatchEffect<CapturedOutput> + Clone + Send + Sync + 'static,
        F: Fn() -> H + Send + Sync + 'static,
    {
        let preamble = repl_preamble(&cfg);
        let effect_stack = tidepool_mcp::build_effect_stack_type(cfg.roster.decls());
        let spawn: SessionSpawn = Box::new(move |sc| {
            let h = builder();
            Session::open(sc, Box::new(move || Box::new(h.clone()) as BoxedStack))
        });
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
                next_cont_id: MonotonicIdIssuer::new("scont"),
                next_session_id: AtomicU64::new(1),
                spawn,
                preamble,
                effect_stack,
                tool_description: build_tool_description(cfg.roster.decls()),
                cfg,
            }),
        };
        server.spawn_reaper();
        server
    }

    /// Spawn the background reaper: periodically reclaim an abandoned suspension
    /// (an `ask` never resumed — only if `continuation_ttl` is set) and a
    /// `Wedged` session (a timed-out turn — only if `wedged_ttl` is set). No-op
    /// when both TTLs are `None` or there is no tokio runtime (e.g. a unit test
    /// that constructs the server off-runtime).
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
                reap_once(inner, suspended_ttl, wedged_ttl);
            }
        });
    }

    /// Start on stdio transport.
    pub async fn serve_stdio(self) -> Result<(), Box<dyn std::error::Error>> {
        tidepool_mcp::serve_stdio(self).await
    }

    /// Start on streamable HTTP transport.
    pub async fn serve_http(
        self,
        addr: std::net::SocketAddr,
    ) -> Result<(), Box<dyn std::error::Error>> {
        tidepool_mcp::serve_streamable_http(self, addr, "tidepool-repl", env!("CARGO_PKG_VERSION"))
            .await
    }

    /// Borrow the values [`tidepool_mcp::resources`] needs to render the
    /// SHARED resource catalog (guide/schema/edits/vocab/capabilities/
    /// patterns/effect/stdlib) — the same catalog the stateless `tidepool`
    /// server serves, so a `tidepool://…` URI mentioned in this server's
    /// model-visible text (e.g. `tidepool://capabilities`) actually resolves
    /// here too, instead of only on the other server.
    fn resource_ctx(&self) -> tidepool_mcp::resources::ResourceCtx<'_> {
        tidepool_mcp::resources::ResourceCtx {
            effects: self.inner.cfg.roster.decls(),
            lib_dirs: &self.inner.cfg.lib_dirs,
            patterns_path: self.inner.cfg.patterns_path.as_deref(),
            stdlib_dir: self.inner.cfg.stdlib_dir.as_deref(),
        }
    }

    fn next_continuation_id(&self) -> ContinuationId {
        ContinuationId(self.inner.next_cont_id.next_id())
    }

    // -- session lifecycle -------------------------------------------------

    /// Open a fresh resident session for the implicit slot (a new session id ⇒ a
    /// new include-tree root). Does NOT install it into the manager.
    fn open_session(&self) -> std::io::Result<Session> {
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
            roster: self.inner.cfg.roster.clone(),
            preamble: self.inner.preamble.clone(),
            effect_stack: self.inner.effect_stack.clone(),
            module_env: self.inner.cfg.module_env.clone(),
            nursery_size: self.inner.cfg.nursery_size.unwrap_or(DEFAULT_NURSERY_SIZE),
        };
        (self.inner.spawn)(cfg)
    }

    /// Auto-open the implicit session on first use (`session_run` needs no
    /// explicit open). If a concurrent caller wins the install race, our
    /// freshly-opened session is dropped and the winner's stays installed —
    /// either way a session is present once this returns `Ok`.
    fn ensure_session_open(&self) -> Result<(), String> {
        if self.inner.manager.is_present() {
            return Ok(());
        }
        let session = self
            .open_session()
            .map_err(|e| format!("session open failed: {e}"))?;
        // Lost the race ⇒ someone else installed first; ours drops here.
        let _ = self.inner.manager.install(session.id(), Box::new(session));
        Ok(())
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
            "session_reset" => Ok(self.session_reset()),
            other => Err(McpError {
                code: ErrorCode::METHOD_NOT_FOUND,
                message: format!("Tool not found: {other}").into(),
                data: None,
            }),
        }
    }

    /// Check the resident session out, run a `SessionCommand` on it, and await
    /// the result, auto-opening the session on first use. `eval_input` is
    /// forwarded so `input :: Aeson.Value` is in scope for every eval item in
    /// the block.
    async fn run_command(
        &self,
        op: &str,
        cmd: SessionCommand,
        eval_input: Option<serde_json::Value>,
        ct: CancellationToken,
    ) -> CallToolResult {
        if let Err(e) = self.ensure_session_open() {
            return CallToolResult::error(vec![Content::text(e)]);
        }
        // Busy-guard: only an Idle session accepts a new turn (REPL policy —
        // running, suspended on an `ask`, or wedged must be resolved first).
        // `admit_run` performs the checkout atomically with the guard: a
        // refusal hands the machine straight back before anyone else can
        // observe it as checked out.
        let checkout = match self.inner.manager.admit_run() {
            None => return CallToolResult::error(vec![Content::text("session is gone")]),
            Some(Err(label)) => {
                return CallToolResult::error(vec![Content::text(format!(
                    "session is {label}; resume it (or session_reset) before running again"
                ))])
            }
            Some(Ok(checkout)) => checkout,
        };
        let gate = PauseGate::new();
        let captured = CapturedOutput::new();
        let cancel = self
            .inner
            .manager
            .cancel_slot()
            .unwrap_or_else(empty_cancel_slot);
        let (session, receipt) = checkout.into_parts();
        let turn_gate = Arc::clone(&gate);
        let turn_captured = captured.clone();
        let join = spawn_turn(session, move |session| {
            session.set_eval_input(eval_input);
            session.run_turn(&cmd, turn_gate, &turn_captured)
        });
        self.drive_detached(op, join, gate, captured, DriveCtl { cancel, receipt }, ct)
            .await
    }

    /// `session_reset`: tear down the current session (dropping a suspended
    /// `ask` and aborting a runaway) and open a fresh resident machine. The
    /// universal get-unstuck button — abort folds into it, so resetting while
    /// suspended drops the pending continuation.
    fn session_reset(&self) -> CallToolResult {
        self.teardown_current();
        match self.ensure_session_open() {
            Ok(()) => CallToolResult::success(vec![Content::text(
                serde_json::json!({"reset": true}).to_string(),
            )]),
            Err(e) => CallToolResult::error(vec![Content::text(e)]),
        }
    }

    /// Tear down the current session if one is present: abort a runaway at a JIT
    /// safepoint, then REMOVE the whole manager entry. Removing drops the
    /// `Session`, and with it the resident machine and any stowed `ask`
    /// continuation — that is how abort folds into reset now: no channel to
    /// release, no worker to wake, nothing to acknowledge, so this returns
    /// immediately instead of waiting out an ack window.
    ///
    /// A turn still in flight keeps running until its next safepoint. Its
    /// settlement then finds a stale epoch and drops the session it is
    /// holding, so it can neither resurrect itself nor clobber the fresh
    /// entry — the registry's own epoch guard, not a `Closing` marker (the
    /// pre-promotion design's synchronous "about to remove" state, no longer
    /// needed now that `remove` is one atomic call under one lock).
    fn teardown_current(&self) {
        // Abort a runaway turn at a JIT safepoint so its thread stops promptly
        // rather than computing on against a session nobody can reach (no-op if
        // idle).
        if let Some(cancel) = self.inner.manager.cancel_slot() {
            if let Some(h) = cancel.lock().as_ref().cloned() {
                h.cancel();
            }
        }
        self.inner.manager.remove();
    }

    async fn session_resume(
        &self,
        req: SessionResumeRequest,
        ct: CancellationToken,
    ) -> Result<CallToolResult, McpError> {
        // Validate + canonicalize the reply against the suspension's schema
        // BEFORE consuming the continuation. This (a) makes `ask` return a
        // STRUCTURED, optic-extractable Value — a reply that arrived as a JSON
        // string is parsed into the canonical shape — and (b) leaves an
        // invalid reply's continuation un-consumed so the caller can retry.
        // Mirrors the eval server's resume (tidepool-mcp/src/server.rs).
        //
        // Three distinguishable rejection causes on mismatch: no suspension
        // at all (never asked, or already spent), suspended on a DIFFERENT
        // continuation, or a schema-invalid reply.
        let (schema, captured) = match self.inner.manager.suspension_for(&req.continuation_id) {
            Err(None) => {
                let msg = match self.inner.manager.busy_label() {
                    None => format!(
                        "no session is running; continuation_id {} cannot be resumed \
                         (run session_run to start one)",
                        req.continuation_id
                    ),
                    Some(label) => format!(
                        "session is not awaiting a resume (state: {label}); continuation_id {} \
                         is already spent or never existed",
                        req.continuation_id
                    ),
                };
                return Err(McpError::invalid_params(msg, None));
            }
            Err(Some(pending)) => {
                return Err(McpError::invalid_params(
                    format!(
                        "session is suspended on continuation {pending}, not {}; resume the \
                         pending one (or session_reset to drop it)",
                        req.continuation_id
                    ),
                    None,
                ));
            }
            Ok(v) => v,
        };
        let canonical =
            match tidepool_mcp::validate::validate_response(schema.as_ref(), &req.response) {
                tidepool_mcp::validate::Outcome::Invalid(violations) => {
                    // Anti-starvation: a retrying continuation must not become the
                    // reaper's oldest-first eviction victim while its caller fixes
                    // the reply. Stays Suspended (un-consumed).
                    self.inner.manager.refresh_suspension_since();
                    let msg = tidepool_mcp::server_common::validation_failed_body(
                        "session_resume",
                        "session_reset",
                        &violations,
                        schema.as_ref(),
                        &req.continuation_id.0,
                    );
                    return Ok(CallToolResult::error(vec![Content::text(msg)]));
                }
                tidepool_mcp::validate::Outcome::Valid(canonical) => canonical,
            };
        // Suspended{cont_id} → Running — the SAME registry checkout that
        // validated the continuation id in the first place (no second,
        // separately-locked check to drift from this one, unlike the
        // pre-promotion two-truths design).
        let checkout = match self.inner.manager.checkout_resume(&req.continuation_id) {
            Ok(c) => c,
            Err(e) => {
                return Err(McpError::internal_error(
                    format!("session is no longer holding the continuation it reported: {e}"),
                    None,
                ))
            }
        };
        let cancel = self
            .inner
            .manager
            .cancel_slot()
            .unwrap_or_else(empty_cancel_slot);
        // A fresh gate per re-entry: a suspended turn has no live computation to
        // latch, so the abort surface belongs to the RESUMING turn.
        let gate = PauseGate::new();
        // The captured buffer carries over from the suspending turn, so the
        // resumed turn's drain includes what the pre-ask items printed.
        let (session, receipt) = checkout.into_parts();
        let turn_gate = Arc::clone(&gate);
        let turn_captured = captured.clone();
        let hole = SlotHole(req.continuation_id.clone());
        let join = spawn_turn(session, move |session| {
            #[allow(
                clippy::expect_used,
                reason = "checkout_resume above already validated the session is suspended \
                          on this continuation; SlotKernelError::NotSuspended cannot fire here"
            )]
            SuspendableSession::resume(
                session,
                hole,
                canonical,
                SlotContext {
                    gate: turn_gate,
                    captured: turn_captured,
                },
            )
            .expect("session validated suspended by checkout_resume")
        });
        Ok(self
            .drive_detached(
                "session_resume",
                join,
                gate,
                captured,
                DriveCtl { cancel, receipt },
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
    async fn drive_detached(
        &self,
        op: &str,
        join: tokio::task::JoinHandle<TurnRun>,
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
            let r = this.drive(&op_owned, join, gate, captured, ctl).await;
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
                // levers the timeout branch of `drive` uses (via the shared
                // watchdog, `tidepool_runtime::session::TurnSupervisor`) —
                // then let the resolver record the real terminal state.
                gate_abort.request_abort(format!("{op} cancelled by client"));
                let handle = cancel_abort.lock().as_ref().cloned();
                let outcome = if let Some(h) = &handle {
                    TurnSupervisor::wait_for_abort_grace(
                        h,
                        Duration::from_secs(ABORT_GRACE_SECS),
                        &mut result_rx,
                    )
                    .await
                } else {
                    TurnSupervisor::wait_grace_without_cancel(
                        Duration::from_secs(ABORT_GRACE_SECS),
                        &mut result_rx,
                    )
                    .await
                };
                match outcome {
                    GraceOutcome::Recovered(Ok(r)) => r,
                    GraceOutcome::Recovered(Err(_)) => CallToolResult::error(vec![Content::text(format!(
                        "{op}: turn resolver task ended without a result (internal error)"
                    ))]),
                    // Uninterruptible turn: don't block the caller on the full
                    // turn budget. The detached resolver keeps owning final
                    // state (it self-heals to Idle or goes Wedged).
                    GraceOutcome::StillRunning => CallToolResult::error(vec![Content::text(format!(
                        "{op} cancelled; the turn is stopping and the session will be ready \
                         shortly (or wedged if uninterruptible — session_reset to force-recover)"
                    ))]),
                }
            }
        }
    }

    /// Resolve an in-flight turn: await its blocking task, map the outcome to an
    /// MCP result, and settle the registry checkout to `Idle` (turn finished),
    /// `Suspended` (the turn stowed an `ask`), or `Wedged` (timeout / crash) —
    /// the registry's own [`Slot`](tidepool_runtime::session::registry::Slot)
    /// is the one lifecycle truth this resolves. Runs on a detached task (see
    /// [`Self::drive_detached`]) so its terminal writes survive a
    /// cancelled/dropped RPC future.
    async fn drive(
        &self,
        op: &str,
        mut join: tokio::task::JoinHandle<TurnRun>,
        gate: Arc<PauseGate>,
        captured: CapturedOutput,
        ctl: DriveCtl,
    ) -> CallToolResult {
        let DriveCtl { cancel, receipt } = ctl;
        let turn_timeout = self
            .inner
            .cfg
            .turn_timeout
            .unwrap_or(Duration::from_secs(TURN_TIMEOUT_SECS));
        let to_secs = turn_timeout.as_secs();
        let joined = match timeout(turn_timeout, &mut join).await {
            Ok(r) => r,
            Err(_) => {
                // The turn is still computing past the budget. Abort it
                // cooperatively on two fronts:
                //   (1) `request_abort` unwinds the turn at its next effect
                //       dispatch (every effect is a gate checkpoint);
                //   (2) the resident machine's `CancelHandle` aborts an
                //       allocating / tail-recursive runaway at its next JIT
                //       safepoint (`YieldError::Cancelled`).
                // Snapshot whether an effect handler was active at the moment
                // of timeout BEFORE requesting abort (abort changes the gate
                // state, not in_effect, but reading early is clearest).
                let effect_in_flight = gate.is_in_effect();
                // Then a bounded grace re-wait: if the turn aborts promptly the
                // session SELF-HEALS back to `Idle` (handle reset, session back
                // in its slot, ready for the next turn); only a genuinely
                // uninterruptible turn (or a session whose first-ever turn ran
                // away before any machine was published) stays `Wedged`.
                gate.request_abort(format!("{op} timed out after {to_secs}s"));
                let handle = cancel.lock().as_ref().cloned();
                if let Some(h) = &handle {
                    if let GraceOutcome::Recovered(Ok(run)) = TurnSupervisor::wait_for_abort_grace(
                        h,
                        Duration::from_secs(ABORT_GRACE_SECS),
                        &mut join,
                    )
                    .await
                    {
                        // Aborted at a safepoint — clear the flag and put the
                        // session back Idle (self-healed).
                        h.reset();
                        self.inner.manager.restore_idle(receipt, run.session);
                        return CallToolResult::error(vec![Content::text(format!(
                            "{op} timed out after {to_secs}s and was aborted; the \
                                 session recovered and is ready for the next turn"
                        ))]);
                    }
                }
                // No handle (first-turn runaway) or no prompt abort → wedged.
                //
                // The `Session` was MOVED INTO the blocking closure, so a task
                // that never returns holds the only copy: there is genuinely
                // nothing to restore. Settle the checkout as `Wedged{since}` —
                // the registry's OWN persisted terminal state (not a full
                // removal): a LATER caller sees "wedged" too, not just this
                // one, until the reaper/`session_reset` reclaims it.
                self.inner.manager.mark_wedged(receipt, Instant::now());
                return CallToolResult::error(vec![Content::text(wedged_message(
                    op,
                    to_secs,
                    effect_in_flight,
                ))]);
            }
        };
        let run = match joined {
            Ok(run) => run,
            Err(_join_err) => {
                // The blocking task itself died (a JIT signal that took the
                // process thread down past `spawn_turn`'s catch_unwind). The
                // session went with it — same honest bookkeeping as the wedge
                // above.
                self.inner.manager.mark_wedged(receipt, Instant::now());
                return CallToolResult::error(vec![Content::text(format!(
                    "{op}: session turn thread crashed (likely a JIT signal — exhausted case \
                     branch or invalid memory access). {RECLAIMED_NOTICE}"
                ))]);
            }
        };

        match run.step {
            TurnStep::Completed(outcome) => {
                let is_error = outcome.is_error();
                let rendered = outcome.render();
                self.inner.manager.restore_idle(receipt, run.session);
                if is_error {
                    let out = captured.snapshot();
                    CallToolResult::error(vec![Content::text(
                        tidepool_mcp::server_common::format_with_output(&out, &rendered),
                    )])
                } else {
                    let out = captured.drain();
                    CallToolResult::success(vec![Content::text(
                        tidepool_mcp::server_common::format_with_output(&out, &rendered),
                    )])
                }
            }
            TurnStep::Suspended(ask) => {
                let cont_id = self.next_continuation_id();
                let (json_obj, expected_schema) =
                    tidepool_mcp::server_common::build_suspension_envelope(
                        &cont_id.0,
                        &ask.prompt,
                        ask.meta,
                    );
                // The session goes back into its slot holding the stowed
                // continuation; the caller-facing suspension payload lives in
                // the manager's own suspension record, set atomically with
                // this same settlement so it can never exist untracked.
                self.inner.manager.restore_suspended(
                    receipt,
                    run.session,
                    cont_id.clone(),
                    captured,
                    expected_schema,
                );
                CallToolResult::success(vec![Content::text(json_obj.to_string())])
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

/// What a reaper sweep decided to do — computed from one read of the
/// registry's own state, acted on right after (no lock held across the
/// `AbortSuspension` branch's eventual `.await`, same discipline as before,
/// just one fewer lock now that there is only the registry's).
enum ReapAction {
    Nothing,
    /// An abandoned suspension (never resumed): abort its stowed continuation.
    AbortSuspension(ContinuationId),
    /// A stale wedge: the entry is dead weight — remove it.
    RemoveWedged,
}

/// One reaper sweep: reclaim the suspension / wedge if older than `ttl`.
///
/// - An abandoned `Suspended` (never resumed) → the stowed continuation is
///   ABORTED and the session returns to `Idle` with everything it had already
///   accumulated intact: the `ask` fails, the turn unwinds, the session
///   survives.
/// - A stale `Wedged` (a timed-out turn whose session is gone) → the entry is
///   removed, so the next `session_run` auto-opens a fresh one.
fn reap_once(
    inner: Arc<ReplServerInner>,
    suspended_ttl: Option<Duration>,
    wedged_ttl: Option<Duration>,
) {
    let now = Instant::now();
    let action = match inner.manager.slot_kind() {
        None => ReapAction::Nothing,
        Some(SlotKind::Suspended) => match inner.manager.suspension_since() {
            Some(since) if suspended_ttl.is_some_and(|ttl| now.duration_since(since) >= ttl) => {
                match inner.manager.suspension_cont_id() {
                    Some(cont_id) => ReapAction::AbortSuspension(cont_id),
                    None => ReapAction::Nothing,
                }
            }
            _ => ReapAction::Nothing,
        },
        Some(SlotKind::Wedged) => match inner.manager.wedged_since() {
            Some(since) if wedged_ttl.is_some_and(|ttl| now.duration_since(since) >= ttl) => {
                ReapAction::RemoveWedged
            }
            _ => ReapAction::Nothing,
        },
        Some(_) => ReapAction::Nothing,
    };
    match action {
        ReapAction::Nothing => {}
        ReapAction::RemoveWedged => inner.manager.remove(),
        ReapAction::AbortSuspension(cont_id) => {
            tokio::spawn(async move { abort_abandoned(inner, cont_id).await });
        }
    }
}

/// Reclaim one abandoned suspension: check the session out on its pending
/// continuation, drive an ABORT through the machine (unwinding the `ask` and
/// consuming the continuation), and hand the session back `Idle`.
async fn abort_abandoned(inner: Arc<ReplServerInner>, cont_id: ContinuationId) {
    let checkout = match inner.manager.checkout_resume(&cont_id) {
        Ok(c) => c,
        // Raced with a real resume or a reset — nothing to reclaim.
        Err(_) => return,
    };
    let (session, receipt) = checkout.into_parts();
    let gate = PauseGate::new();
    let captured = CapturedOutput::new();
    let hole = SlotHole(cont_id);
    let join = spawn_turn(session, move |session| {
        #[allow(
            clippy::expect_used,
            reason = "checkout_resume above already validated the session is suspended \
                      on this continuation; SlotKernelError::NotSuspended cannot fire here"
        )]
        SuspendableSession::abort(
            session,
            hole,
            "continuation expired before it was resumed".to_string(),
            SlotContext { gate, captured },
        )
        .expect("session validated suspended by checkout_resume")
    });
    match join.await {
        Ok(run) => match run.step {
            TurnStep::Completed(_) => {
                inner.manager.restore_idle(receipt, run.session);
            }
            TurnStep::Suspended(_) => {
                // The aborted turn caught the failure and asked AGAIN. Nobody is
                // waiting on this reap, so restoring would leave a machine
                // holding a continuation no caller knows the id of. Retire the
                // session instead; the next `session_run` opens a fresh one.
                tracing::warn!("reaped continuation re-suspended on abort; retiring the session");
                inner.manager.retire(receipt);
            }
        },
        Err(_join_err) => {
            inner.manager.retire(receipt);
        }
    }
}

fn build_tool_description(decls: &[EffectDecl]) -> String {
    // DERIVED from the decls (tidepool_mcp::describe): the same effect index
    // (name + first-sentence + verb names) the eval tool description and
    // `:browse` render — one source, so the three surfaces can't drift.
    let effects = describe_effects_index(decls);
    // Mirrors the eval tool description's gate (`preamble::build_eval_tool_description`) —
    // one Schema vocabulary, inlined here too so a first `llm`/`ask` call doesn't cost a
    // second introspection round-trip (`:i Schema`) just to learn the constructors.
    let names: std::collections::HashSet<&str> = decls.iter().map(|e| e.type_name).collect();
    let schema_note = if names.contains("Llm") && names.contains("Ask") {
        "\nStructured LLM / Ask (one Schema vocabulary; also `:i Schema`):\n\
         Schema = SObj [(Text,Schema)] | SArr Schema | SStr | SNum | SBool | SEnum [Text] | SOpt Schema\n\
         ask schema prompt  -- SUSPEND to the caller; reply validated vs schema, no token burn\n\
         llm schema prompt  -- AUTONOMOUS server-side call (costs tokens); structured Value, no fences\n\
         bind the Right, then extract with optics:\n\
         do {{ Right v <- llm (SObj [(\"k\", SEnum [\"a\",\"b\"])]) p; pure (v ^? key \"k\" . _String) }}\n\n"
    } else {
        ""
    };
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
         (per element, not as a whole — a stubbed list mixes real values and plain marker \
         strings like \"[~9140 chars -> stub_0]\" in the SAME array; guard for that shape rather \
         than mapping blindly: `[ f v | v <- xs, not (isStub v) ] where isStub v = case v of \
         {{ String s -> \"-> stub_\" `T.isInfixOf` s; _ -> False }}`) but stays a live binding \
         you can keep computing on, and a stubbed subtree is still fetchable in full via \
         `:stub <n>`.\n\
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
         {schema_note}\
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
         SURFACE NOTES — `FilePath` is `Text` here: path/extension work is `T.` functions \
         (`T.dropWhile`, `T.stripPrefix`), never `String` idioms like `dropWhile (== '.')`. \
         The OverloadedRecordDot extension is ON: `x.f` is field access, so \
         write function composition WITH SPACES (`f . g`); `f.g` parses as projecting field \
         `g` and will not typecheck. Common partial functions are deliberately unsupported — \
         each compile-errors naming its total form: `head`→`headMay`, `tail`→`tailMay`, \
         `last`→`lastMay`, `init`→`initMay`, `(!!)`→`atMay xs i`, `fromJust`→`fromMaybe`/`maybe`/ \
         a `Just` pattern (or a pattern-match generator `(x:_) <- …`). `(!?)` looks like the \
         safe `(!!)` but is Map-only lookup — use `atMay` for a safe list index, not `(!?)`. \
         tidepool://capabilities has the full pairing.",
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
        let local = vec![tidepool_mcp::resources::descriptor_to_raw(
            tidepool_mcp::resources::ResourceDescriptor {
                uri: SESSION_BINDINGS_URI.to_string(),
                name: "Session bindings".to_string(),
                description: "Live session environment as JSON: one entry per in-scope binding \
                 (name, type, kind = decl|bind, generation), plus the decl/value generation \
                 counters. Refreshed after every turn."
                    .to_string(),
                mime: "application/json",
            },
        )];
        let ctx = self.resource_ctx();
        let resources = tidepool_mcp::resources::list_catalog_resources(&ctx, local);
        Ok(ListResourcesResult {
            resources,
            next_cursor: None,
            meta: None,
        })
    }

    async fn list_resource_templates(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, McpError> {
        Ok(ListResourceTemplatesResult {
            resource_templates: tidepool_mcp::resources::list_catalog_templates(),
            next_cursor: None,
            meta: None,
        })
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResult, McpError> {
        let ctx = self.resource_ctx();
        let uri = request.uri;
        let local = || {
            (uri == SESSION_BINDINGS_URI).then(|| tidepool_mcp::resources::ResourceBody {
                mime: "application/json",
                text: self.session_bindings_body(),
            })
        };
        match tidepool_mcp::resources::read_catalog_body(&ctx, &uri, local) {
            Some(b) => Ok(tidepool_mcp::resources::render_resource_body(uri, b)),
            None => Err(McpError::resource_not_found(
                format!("Unknown resource: {uri}"),
                None,
            )),
        }
    }
}

/// An empty request schema — `session_reset` takes no arguments.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct EmptyRequest {}

#[cfg(test)]
mod tests {
    use super::*;

    /// A server with a session root under `dir` and NO background reaper (both
    /// TTLs `None`, so `spawn_reaper` returns early) — the wedge tests below
    /// call [`reap_once`] directly with an explicit TTL instead, so a sweep
    /// happens exactly when the test says it does.
    ///
    /// Everything here is GHC-free: constructing a server builds strings, and
    /// `ensure_session` only creates the session include dir. No turn is run.
    fn wedge_test_server(dir: &std::path::Path) -> TidepoolReplServer {
        let cfg = ReplServerConfig {
            roster: EffectRoster::from_handlers(&frunk::HNil),
            base_include: Vec::new(),
            module_env: ModuleEnv::standalone_default(),
            session_root_base: dir.to_path_buf(),
            nursery_size: None,
            continuation_ttl: None,
            wedged_ttl: None,
            turn_timeout: None,
            lib_dirs: Vec::new(),
            stdlib_dir: None,
            patterns_path: None,
        };
        TidepoolReplServer::new(frunk::HNil, cfg)
    }

    /// Every wedge path must TELL the caller their session is gone.
    ///
    /// A wedged turn's session is unrecoverable (the turn holds the only copy),
    /// so the entry is dropped and the next `session_run` auto-opens an empty
    /// one. That is the right mechanism, but it makes the loss inferable rather
    /// than stated — and the repl's contract is that the session IS the caller's
    /// working memory across turns. A silent reclaim surfaces later as
    /// "Variable not in scope: <name>", which reads as a Haskell error rather
    /// than as the reclaim that actually caused it.
    ///
    /// The notice rides the timeout error the caller is already being handed, so
    /// this asserts on the message rather than on stored state — there IS no
    /// stored state, deliberately (no breadcrumb, no second pathway).
    ///
    /// Both branches are checked because they are separately authored strings;
    /// routing them through `wedged_message` is what makes the notice structural,
    /// and this is what keeps it that way. Testing the function directly also
    /// sidesteps the one wedge transition with no reliable seam — ENTRY via a
    /// real runaway.
    #[test]
    fn every_wedge_message_states_the_session_was_reclaimed() {
        for effect_in_flight in [true, false] {
            let msg = wedged_message("session_run", 30, effect_in_flight);
            assert!(
                msg.contains("bindings are GONE"),
                "a wedge must state the binding loss outright (effect_in_flight={effect_in_flight}): {msg}"
            );
            assert!(
                msg.contains("fresh, empty session"),
                "a wedge must say what the NEXT turn gets (effect_in_flight={effect_in_flight}): {msg}"
            );
            // Telling a caller to run a recovery step that no longer applies is
            // its own trap: the entry is already gone, so there is nothing left
            // to reset.
            assert!(
                !msg.contains("session_reset to recover"),
                "the stale 'session_reset to recover' instruction must not survive (effect_in_flight={effect_in_flight}): {msg}"
            );
        }
    }

    /// Manufacture a `Wedged` entry directly via the manager, mirroring what
    /// `drive`'s timeout/crash paths do (`mark_wedged` on a checked-out
    /// receipt) — driven at the transition level rather than by
    /// manufacturing a real runaway: a pure loop that outruns the JIT cancel
    /// through the full abort grace is neither reliable nor cheap to
    /// construct, and a flaky test here would be worse than none.
    fn wedge_current_session(server: &TidepoolReplServer) {
        server.ensure_session_open().expect("session auto-opens");
        let checkout = server
            .inner
            .manager
            .admit_run()
            .expect("session present")
            .expect("idle -> run");
        let (_session, receipt) = checkout.into_parts();
        server.inner.manager.mark_wedged(receipt, Instant::now());
    }

    /// A wedged turn holds the only copy of its session, so nothing can be
    /// restored — but (post-promotion) the registry's own `Wedged` slot
    /// stays visibly wedged for every caller, not just the one that wedged
    /// it, until the reaper's TTL sweep or `session_reset` clears it.
    #[tokio::test]
    async fn reaper_removes_a_wedged_entry_only_once_past_its_ttl() {
        let dir = tempfile::tempdir().expect("tempdir");
        let server = wedge_test_server(dir.path());
        wedge_current_session(&server);

        // A sweep with a TTL the wedge has NOT outlived leaves it alone — this
        // is what makes the assertion below mean "the TTL was consulted" rather
        // than "reaping always fires".
        reap_once(
            Arc::clone(&server.inner),
            None,
            Some(Duration::from_secs(3600)),
        );
        assert!(
            server.inner.manager.is_present(),
            "a wedge younger than the TTL must survive the sweep"
        );

        // Past the TTL, the entry goes — freeing the slot so the next
        // `session_run` auto-opens a fresh session.
        reap_once(Arc::clone(&server.inner), None, Some(Duration::ZERO));
        assert!(
            !server.inner.manager.is_present(),
            "a wedge past its TTL must be removed, not left occupying the slot"
        );
        server
            .ensure_session_open()
            .expect("a fresh session auto-opens");
        assert_eq!(
            server.inner.manager.slot_kind(),
            Some(SlotKind::Idle),
            "the reopened session must be Idle and usable"
        );
    }

    /// The other way out of a wedge: `session_reset`, the universal get-unstuck
    /// button. It must REPLACE the entry — a fresh `Idle` one, not the wedged
    /// one nursed back — which is what keeps reset working uniformly whether
    /// the session was idle, suspended, or wedged.
    #[tokio::test]
    async fn reset_reclaims_a_wedged_entry() {
        let dir = tempfile::tempdir().expect("tempdir");
        let server = wedge_test_server(dir.path());
        wedge_current_session(&server);
        let wedged_id = server.inner.manager.busy_label();
        assert!(wedged_id.is_some_and(|l| l.contains("wedged")));

        let result = server.session_reset();
        assert_ne!(
            result.is_error,
            Some(true),
            "reset must succeed from a wedged session"
        );

        assert_eq!(
            server.inner.manager.slot_kind(),
            Some(SlotKind::Idle),
            "reset must REPLACE the wedged entry with a fresh Idle one"
        );
    }

    /// Guards the dead-resource-link class of bug: every `tidepool://`
    /// URI the tool description POINTS a model at must actually resolve on
    /// THIS server. GHC-free: `resources::read` only touches
    /// strings + the filesystem, no compile.
    #[test]
    fn every_advertised_tidepool_uri_resolves() {
        let dir = tempfile::tempdir().expect("tempdir");
        let server = wedge_test_server(dir.path());
        let text = &server.inner.tool_description;
        let ctx = server.resource_ctx();
        let mut checked = 0;
        for token in text.split(|c: char| c == '`' || c.is_whitespace() || c == ')') {
            let Some(uri) = token.strip_prefix("tidepool://") else {
                continue;
            };
            // Strip trailing punctuation the prose may have glued on.
            let uri = format!("tidepool://{}", uri.trim_end_matches(['.', ',', ':', ';']));
            assert!(
                tidepool_mcp::resources::read(&ctx, &uri).is_some()
                    || uri.as_str() == SESSION_BINDINGS_URI,
                "advertised {uri:?} does not resolve on this server"
            );
            checked += 1;
        }
        assert!(
            checked > 0,
            "sanity: the tool description should mention at least one tidepool:// URI"
        );
    }
}
