//! `Harness` — the orchestrator that owns the node tree, the resident
//! sessions, the transcript store, and the provider-driven turn loop. This is
//! the object the web protocol server drives.
//!
//! # Ownership map
//!
//! - [`NodeTree`] (`crate::forcing`) owns the tree structure, per-node
//!   [`NodeState`], and the durable event log. Every state transition and
//!   every event goes through it.
//! - The Harness owns the resident sessions directly (one
//!   [`ResidentSession`] per forced node), keyed by [`NodeId`], behind a
//!   mutex. A node's session is created at force time and dropped when the
//!   node terminates.
//! - The transcript store is reconstructed by folding `TurnDelta`/`TurnForked`
//!   events; the live in-memory copy per node lives here too.
//!
//! # Scheduler (thin, TARGET §4)
//!
//! One parent + one live child. A fork PARKS the parent (it is suspended);
//! the child answerer runs its own turn loop, and its final answer eval runs
//! via [`ResidentSession::run_child`] against the parent's suspended machine
//! (same heap, GC-rooted), then resumes the parent. Child evals run only while
//! the parent is parked — which it is, by construction (a fork suspends). No
//! preemption, no work-stealing: inference-bound fan sizes make starvation a
//! non-issue for R0.
//!
//! # Sync core, async driver
//!
//! The resident-session calls (`run`/`run_child`/`resume`) are synchronous and
//! block (they spawn their own eval thread internally). The provider calls are
//! async. The turn-driving methods here are `async`; they `spawn_blocking` the
//! resident-session steps so the tokio reactor is never blocked.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::Mutex;
use serde_json::Value as Json;
use tidepool_effect::dispatch::DispatchEffect;
use tidepool_eval::value::Value;
use tidepool_mcp::CapturedOutput;
use tidepool_repr::{DataConTable, Generation, SessionId};
use tidepool_runtime::session::{
    classify_turn, compile_session_turn, BoundBinder, ModuleEnv, ResidentError, ResidentOutcome,
    ResidentSession, SessionBind, SessionLib, TurnKind,
};
use tidepool_runtime::DEFAULT_NURSERY_SIZE;
use tokio::sync::{mpsc, oneshot};

use crate::compile::{self, AsksSidecar};
use crate::effect_trace::{EffectRecord, EffectTrace, TracingDispatcher};
use crate::engine::{
    self, ClassifiedHole, EngineConfig, EngineError, HoleRouting, TurnOutcome, RESUME_HELPER,
};
use crate::forcing::{ForkShape, NodeTree, TreeError};
use crate::log::{Actor, AnswerOutcome, LogWriter};
use crate::provider::{DynModelProvider, Message, Role, StreamDelta, Usage};
use crate::timing;
use crate::tree::{FanBadge, HoleId, NodeId};

/// The boxed handler stack — one concrete machine type so the Harness (and the
/// web server over it) is not generic. `build_base_stack` returns an opaque
/// `impl DispatchEffect<CapturedOutput>`; boxing it here erases that so the
/// resident-session type is nameable.
pub type BoxedStack = Box<dyn DispatchEffect<CapturedOutput> + Send>;

/// The concrete resident session type the Harness stores per forced node.
pub type Session = ResidentSession<BoxedStack, CapturedOutput>;

#[derive(Debug, thiserror::Error)]
pub enum HarnessError {
    #[error(transparent)]
    Tree(#[from] TreeError),
    #[error(transparent)]
    Engine(#[from] EngineError),
    #[error("compile failed:\n{0}")]
    Compile(String),
    #[error("resident session error: {0}")]
    Resident(String),
    #[error("node {0:?} has no live session (not forced, or already terminal)")]
    NoSession(NodeId),
    #[error("node {0:?} is not suspended on a hole")]
    NotSuspended(NodeId),
    #[error("node {node:?}: answer routed to {routing} but hole is {actual}")]
    RoutingMismatch {
        node: NodeId,
        routing: &'static str,
        actual: String,
    },
    #[error("node {0:?}: no mechanical derived form for the pending hole (uiOf yields None, or the submission doesn't map) — fall back to the model-driven answerer")]
    NoDerivedForm(NodeId),
    #[error("node {node:?} aborted: {reason}")]
    Aborted { node: NodeId, reason: String },
    #[error("node {0:?} has no pending operator escalation to resolve")]
    NoPendingEscalation(NodeId),
}

/// What a node must produce to resolve the hole it is answering, and what its
/// turns need in scope to produce it.
///
/// Both halves are required for the "GHC validates the answer against `T`"
/// guarantee to hold for `finalize`. `ty` pins `finalize` to the hole's answer
/// type ([`crate::engine::finalize_shim`]) so a wrong-typed answer is a compile
/// error instead of a value that crosses in-heap into a `T`-typed continuation.
/// `imports` puts `T` itself in scope: `T` is an author type (the wizard's
/// `Contribution`, defined in the harness source module), and a turn that cannot
/// NAME `T` cannot construct one — the model tries `@T`, gets "not in scope",
/// and settles for whatever does compile. Importing the module that defines `T`
/// also fixes WHICH `T` the turn means: the same defining module the parent
/// resolved, hence the same `DataConId` at the crossing.
#[derive(Debug, Clone)]
pub struct AnswerContract {
    /// The hole's rendered answer type, as it appears in `asks.json`.
    pub ty: String,
    /// Import lines (without the leading `import`) prepended to every turn on
    /// this node — the module(s) defining `ty`.
    pub imports: Vec<String>,
}

/// Per-node conversation state the Harness keeps live (also derivable from the
/// log by folding). `transcript` is the message list the turn engine assembles
/// prompts from; `turn_seq` is the monotonic per-node turn index logged with
/// each `TurnDelta`. `pending` is the classified hole when suspended.
struct NodeConvo {
    /// `None` only transiently while a turn runs on the blocking pool (the
    /// session is moved out to be `run` off-reactor, then moved back). Every
    /// public method that could observe the gap holds the convos lock across
    /// the take, so an external caller never sees `None` for a live node.
    session: Option<Session>,
    transcript: Vec<Message>,
    turn_seq: u64,
    /// Shared buffer the session's [`TracingDispatcher`] appends each effect to;
    /// drained per turn by [`Harness::flush_effects`] into `Event::Effect`.
    effect_trace: EffectTrace,
    /// Monotonic per-node effect sequence number for the logged `Event::Effect`s.
    effect_seq: u64,
    pending: Option<PendingHole>,
    /// The typed hole this node is currently answering, when it answers by
    /// `finalize` (the self-iterating harness's answerer). Set per hole by
    /// [`Harness::set_answer_contract`]; read by [`Harness::run_block`] to pin
    /// `finalize` to the hole's type and to put that type in scope. `None` for
    /// every node that isn't driving toward a `finalize`.
    answer_contract: Option<AnswerContract>,
    /// Compile artifacts of the turn that suspended — the table is needed to
    /// bridge an answer Value against the same constructor set.
    suspend_table: Option<DataConTable>,
    suspend_asks: AsksSidecar,
    /// When the SUSPENDED turn is a value-plane bind (`x <- fork …`), the binder
    /// metadata + generation to materialize once the bind completes on resume.
    /// `resume_parent` drives `resume_bind` (not `resume`) while this is `Some`,
    /// and clears it when the bind finally lands (a completion, not a re-suspend).
    pending_bind: Option<(BoundBinder, Generation)>,
    /// Running sum of every assistant turn's [`Usage`] on this node
    /// (self-iterating-harness WS-E: the driver's emergency-compaction
    /// trigger, [`Harness::node_usage`], sums this across every `runLLMTurn`
    /// answerer / compaction node it drives per loop).
    usage: Usage,
    /// The MOST RECENT turn's `input_tokens` (overwritten every turn, not
    /// summed) — the provider's per-round input token count already includes
    /// the whole re-sent transcript, so the latest value IS the node's real
    /// current context size (a high-water mark). The self-iterating-harness
    /// driver's compaction threshold reads THIS (review C-1) rather than the
    /// running [`Self::usage`] sum, which super-linearly over-counts across a
    /// multi-round hole (each round's input re-counts every prior round's
    /// transcript). `0` before the node's first turn.
    last_input_tokens: u64,
    /// This node's OWN system message, overriding the default
    /// [`engine::SYSTEM_FRAMING`] when set (self-iterating-harness W1/C1: the
    /// per-loop answerer session's framing is `render`'s output — the
    /// distilled conditional the whole thesis rests on, wired to the model
    /// here rather than left observational). `None` for an ordinary Agent
    /// node (the default full-surface framing).
    framing: Option<String>,
}

#[derive(Clone)]
struct PendingHole {
    hole: HoleId,
    classified: ClassifiedHole,
    /// The raw suspended request `Value`, kept alongside `classified` (which
    /// is JSON-shaped, lossy for a `Finalize` hole — its carried value may be
    /// non-serializable, e.g. a closure). `Harness::take_finalized_value`
    /// reads the finalize payload straight out of this, never through JSON.
    raw_request: Value,
}

/// A node's live heap/GC snapshot (observatory heap pane) — plain numbers off
/// its resident `JitEffectMachine`, straight from
/// [`tidepool_codegen::jit_machine::HeapStats`]: no new GC/rooting
/// instrumentation, this is a read-only view of counters that already exist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeapSummary {
    pub nursery_bytes: usize,
    pub live_bytes: usize,
    pub gc_count: u64,
}

/// A tree-pane row: a node's identity, parentage, lifecycle state, and (when
/// suspended) whether the pending hole is a fork and its prompt.
#[derive(Debug, Clone)]
pub struct NodeSummary {
    pub node: NodeId,
    pub parent: Option<NodeId>,
    pub state: crate::tree::NodeState,
    pub is_fork_hole: bool,
    pub hole_prompt: Option<String>,
    /// Set when this node is mid-`drive_answerer_to_value`, PARKED on the
    /// operator-decision channel after exhausting its rung-1 auto-retry
    /// (the escalation ladder's rung 2) — the node's [`crate::tree::NodeState`]
    /// itself is unchanged (still `Running`: no hole was published, no event
    /// was logged), so this is the harness's own in-memory signal the
    /// observatory badges/pops up on. `None` otherwise.
    pub awaiting_operator: bool,
}

/// The escalation-ladder's rung-2 state (operator-in-the-loop): a child
/// answerer exhausted its auto-retry and is parked awaiting an operator
/// decision. IN-PROCESS ONLY — this lives in [`Harness`]'s memory, not the
/// durable event log; a process restart mid-escalation loses it (the
/// operator re-triggers by re-forcing, same as any other in-flight turn —
/// durable mid-fan suspension is explicitly out of R0 scope).
#[derive(Debug, Clone)]
pub struct Escalation {
    /// Human-facing summary of why this node escalated (e.g. cap-exhausted
    /// after N attempts).
    pub reason: String,
    /// A short tail of the answerer's own transcript, for the popup's
    /// "what has it been trying" preview. Model-authored — render it through
    /// the same HTML-neutralizing path any other model text uses.
    pub transcript_preview: String,
}

/// The operator's rung-2 decision, delivered through the oneshot channel
/// [`Harness::resolve_escalation`] fires. `AllocateMore` grants a fresh turn
/// budget (replacing, not adding to, what remained) and optionally injects
/// `steer` as the answerer's next corrective user turn before it retries.
#[derive(Debug, Clone)]
pub enum OperatorDecision {
    AllocateMore { turns: u32, steer: Option<String> },
    Abort,
}

/// The outcome of [`Harness::handle_cap_exhaustion`]'s ladder step: either
/// the caller's loop keeps going with a (possibly larger) turn budget, or the
/// answerer is being torn down.
enum CapDecision {
    Retry { turn_budget: u32 },
    Abort { reason: String },
}

/// A fork/fanout child's inherited context, staged between
/// `register_fork_child` and `force`: the cloned parent transcript prefix
/// (through the fork checkpoint) plus the hole card, and the parent's framing
/// (its system message), so the child's request prefix is byte-identical to
/// the parent's through the checkpoint.
type ForkedContext = (Vec<Message>, Option<String>);

/// A fork child's compile row: the parent row minus the fork-spawning effects
/// (`Fork`/`RunLLMTurn`). A child keeps everything else it needs to compute its
/// answer (base effects, `AskUser`, `Finalize`) but literally cannot name
/// `fork`/`forkAll`/`runLLMTurn` — depth-one is structural, not a runtime
/// guard. For the answerer (`[AskUser, Fork, Finalize]`) this yields the leaf
/// `[AskUser, Finalize]`.
fn fork_child_decls(parent: &[tidepool_mcp::EffectDecl]) -> Vec<tidepool_mcp::EffectDecl> {
    parent
        .iter()
        .filter(|d| !matches!(d.type_name, "Fork" | "RunLLMTurn"))
        .copied()
        .collect()
}

/// Cap a (possibly huge) GHC/extract compile error before feeding it back to
/// the model as a corrective turn — the head carries the structured diagnostics
/// and first errors, which is what the model needs to fix its Haskell. UTF-8
/// safe (truncates on a char boundary).
fn truncate_ghc_error(msg: &str) -> String {
    const CAP: usize = 3000;
    if msg.chars().count() <= CAP {
        msg.to_string()
    } else {
        let head: String = msg.chars().take(CAP).collect();
        format!("{head}\n… (truncated)")
    }
}

/// A node's in-progress turn as it streams — the answer text and reasoning
/// ("thinking") accumulated so far, before the turn completes and is logged.
/// The observatory renders this so tokens appear live; it's cleared when the
/// turn lands in the durable log.
#[derive(Debug, Clone, Default)]
pub struct LiveTurn {
    pub text: String,
    pub reasoning: String,
}

/// The orchestrator. Cloneable-cheap? No — it owns the tree + sessions, so it
/// is shared behind an `Arc`.
pub struct Harness {
    tree: NodeTree<()>,
    cfg: EngineConfig,
    /// The row a FORK CHILD's answer block compiles against: the node's own
    /// row minus the fork-spawning effects (`Fork`/`RunLLMTurn`), so a child
    /// structurally cannot fork — a `forkAll` in a child block is a GHC
    /// "not in scope" error, not a runtime `ChildSuspended`. For the answerer
    /// (`[AskUser, Fork, Finalize]`) this is the leaf `[AskUser, Finalize]`.
    /// The child's answer still runs via `run_child` against the PARENT's
    /// session (a pure `resume expr` value crossing), so the leaf row only
    /// scopes what the child can NAME, not where its value lands.
    child_cfg: EngineConfig,
    provider: Arc<dyn DynModelProvider>,
    convos: Mutex<HashMap<NodeId, NodeConvo>>,
    /// Per-node streaming turn buffer (see [`LiveTurn`]). Present only while a
    /// node's turn is actively streaming; the entry is removed when the turn
    /// completes (its content is then in the log).
    live_turns: Mutex<HashMap<NodeId, LiveTurn>>,
    /// A "something changed" callback the web layer installs
    /// ([`Self::set_notifier`]) so streaming deltas nudge the SSE stream to
    /// re-render. `None` (unset) in tests / headless runs — the harness works
    /// the same, just without live push.
    notifier: std::sync::OnceLock<Box<dyn Fn() + Send + Sync>>,
    /// The Haskell that seeds a fresh session's ConTags (the 10-effect stack).
    /// Compiled once, reused for every node's bootstrap.
    boot: Arc<compile::CompiledTurn>,
    /// A just-created root's opening prompt PLUS its optional per-node framing
    /// (the system message override — [`NodeConvo::framing`]), staged between
    /// `create_root`/`create_root_framed` and `force` (a thunk node has no
    /// live `NodeConvo` to hold either yet). Removed once consumed at force
    /// time.
    seeds: Mutex<HashMap<NodeId, (String, Option<String>)>>,
    /// A just-registered fork/fanout child's inherited [`ForkedContext`],
    /// staged between `register_fork_child` and `force` (same lifetime as
    /// `seeds`). Removed once consumed at force time.
    forked_transcripts: Mutex<HashMap<NodeId, ForkedContext>>,
    /// Rung-2 escalation state (operator popup), keyed by the answerer node
    /// that is parked awaiting a decision. Set by
    /// [`Self::escalate_to_operator`] just before the await, read by the web
    /// layer to render the stuck-node popup, removed once resolved.
    escalations: Mutex<HashMap<NodeId, Escalation>>,
    /// The oneshot sender half for each PENDING rung-2 escalation, keyed the
    /// same way as `escalations`. [`Self::resolve_escalation`] (driven by the
    /// web resolve endpoint, or fired directly in a test) removes and fires
    /// the sender; the matching receiver lives on `escalate_to_operator`'s
    /// async stack, in-process only (see that method's doc for the
    /// durability caveat).
    operator_decisions: Mutex<HashMap<NodeId, oneshot::Sender<OperatorDecision>>>,
}

impl Harness {
    /// Build a harness over `writer` (a fresh log past its header), the engine
    /// config, and a signed-in provider. Compiles the bootstrap seed once.
    pub fn new(
        writer: LogWriter,
        cfg: EngineConfig,
        provider: Arc<dyn DynModelProvider>,
    ) -> Result<Self, HarnessError> {
        // A trivial effectful seed carrying the full effect-stack ConTags.
        let boot_src = engine::template_turn(&cfg, "pure (toJSON (0 :: Int))", "", "", None);
        // No real answerer node exists yet (this is the one-time boot compile) —
        // NO_NODE/NO_ROUND. `NodeId(0)` is a real, live node id, never a sentinel.
        let boot = compile::compile_turn(
            &cfg.extract_bin,
            &boot_src,
            "result",
            &cfg.include,
            timing::NO_NODE,
            timing::NO_ROUND,
        )
        .map_err(|e| HarnessError::Compile(e.to_string()))?;
        // The fork-child compile config: this node's row minus the
        // fork-spawning effects, so a child cannot fork (see `child_cfg`).
        let child_cfg = EngineConfig::from_decls(
            fork_child_decls(&cfg.decls),
            cfg.prelude_dir.clone(),
            cfg.project_lib.clone(),
        )
        .map_err(|e| HarnessError::Compile(format!("fork-child engine config: {e}")))?;
        Ok(Harness {
            tree: NodeTree::new(writer),
            cfg,
            child_cfg,
            provider,
            convos: Mutex::new(HashMap::new()),
            live_turns: Mutex::new(HashMap::new()),
            notifier: std::sync::OnceLock::new(),
            boot: Arc::new(boot),
            seeds: Mutex::new(HashMap::new()),
            forked_transcripts: Mutex::new(HashMap::new()),
            escalations: Mutex::new(HashMap::new()),
            operator_decisions: Mutex::new(HashMap::new()),
        })
    }

    /// Install the "changed" callback the web layer uses to nudge its SSE
    /// stream when a streaming delta lands. Set once, at startup.
    pub fn set_notifier(&self, f: impl Fn() + Send + Sync + 'static) {
        let _ = self.notifier.set(Box::new(f));
    }

    /// Fire the installed notifier, if any (no-op otherwise).
    fn notify(&self) {
        if let Some(f) = self.notifier.get() {
            f();
        }
    }

    /// The node's in-progress streaming turn, if one is active — the
    /// observatory renders this to show tokens/thinking as they arrive.
    pub fn live_turn(&self, node: NodeId) -> Option<LiveTurn> {
        self.live_turns.lock().get(&node).cloned()
    }

    /// Fold one streaming delta into the node's live-turn buffer.
    fn apply_delta(&self, node: NodeId, delta: StreamDelta) {
        let mut live = self.live_turns.lock();
        let entry = live.entry(node).or_default();
        match delta {
            StreamDelta::Text(t) => entry.text.push_str(&t),
            StreamDelta::Reasoning(r) => entry.reasoning.push_str(&r),
        }
    }

    /// Drive one model turn on `node` with live streaming: deltas land in the
    /// node's live-turn buffer (rendered token-by-token in the observatory)
    /// while the complete turn is assembled, with a throttled `notify()`
    /// nudging the SSE stream. On failure the partial buffer is dropped; on
    /// success it's left intact for the caller to swap for the logged turn via
    /// [`Self::finish_live_turn`], so the transcript never flickers empty.
    /// Shared by the root turn loop and the fork/fanout answerer loops.
    async fn stream_turn(
        &self,
        node: NodeId,
        transcript: &[Message],
        framing: Option<&str>,
    ) -> Result<engine::DrivenTurn, HarnessError> {
        let (tx, mut rx) = mpsc::unbounded_channel::<StreamDelta>();
        let provider = self.provider.as_ref();
        let drive_fut =
            engine::drive_model_turn(provider, transcript, self.cfg.max_tokens, framing, Some(tx));
        tokio::pin!(drive_fut);
        let mut last_notify: Option<std::time::Instant> = None;
        let result = loop {
            tokio::select! {
                res = &mut drive_fut => {
                    while let Ok(d) = rx.try_recv() {
                        self.apply_delta(node, d);
                    }
                    break res;
                }
                Some(delta) = rx.recv() => {
                    self.apply_delta(node, delta);
                    if last_notify.is_none_or(|t| t.elapsed() >= std::time::Duration::from_millis(120)) {
                        self.notify();
                        last_notify = Some(std::time::Instant::now());
                    }
                }
            }
        };
        match result {
            Ok(d) => Ok(d),
            Err(e) => {
                self.live_turns.lock().remove(&node);
                self.notify();
                Err(e.into())
            }
        }
    }

    /// Clear the node's live-turn buffer and re-render — called right after the
    /// turn is logged, so the transcript swaps the streaming buffer for the
    /// durable turn in a single frame.
    fn finish_live_turn(&self, node: NodeId) {
        self.live_turns.lock().remove(&node);
        self.notify();
    }

    /// Drain `node`'s effect-trace buffer and write one `Event::Effect` per
    /// captured effect (mapping the stack tag to its effect name). Called after
    /// a turn's block runs, while the node is still `Running`. Requires the node
    /// to be `Running` (the tree's `effect` guard); a drained record that fails
    /// to log is dropped rather than aborting the turn.
    fn flush_effects(&self, node: NodeId) {
        let (records, mut seq) = {
            let mut convos = self.convos.lock();
            let Some(convo) = convos.get_mut(&node) else {
                return;
            };
            let records: Vec<EffectRecord> = convo
                .effect_trace
                .lock()
                .map(|mut t| std::mem::take(&mut *t))
                .unwrap_or_default();
            (records, convo.effect_seq)
        };
        if records.is_empty() {
            return;
        }
        for rec in records {
            let tag = self
                .cfg
                .effect_names
                .get(rec.tag as usize)
                .cloned()
                .unwrap_or_else(|| format!("tag{}", rec.tag));
            let _ = self.tree.effect(node, seq, tag, rec.req, rec.resp);
            seq += 1;
        }
        if let Some(convo) = self.convos.lock().get_mut(&node) {
            convo.effect_seq = seq;
        }
    }

    /// Read-only handle to the node tree (state/children/parent queries for the
    /// protocol server's tree pane).
    pub fn tree(&self) -> &NodeTree<()> {
        &self.tree
    }

    /// This harness's engine config — the self-iterating harness driver
    /// (WS-A) reads `prelude_dir`/`project_lib` off it to build the OUTER
    /// session's own (narrower) `EngineConfig`, so the outer `Eff
    /// '[RunLLMTurn]` compile and this nested Agent's compile resolve
    /// author-defined types (e.g. a harness's own `Decision`) from the SAME
    /// module — required for a value to cross between them via `resume`.
    pub fn cfg(&self) -> &EngineConfig {
        &self.cfg
    }

    /// A flat snapshot of the tree for the observatory tree pane, in DFS
    /// (parent-before-child, creation order) order. Each entry carries enough
    /// to render a node row: id, parent, state, and — when suspended —
    /// whether the pending hole is a fork (so the pane can badge it).
    ///
    /// Unpaginated — reads the whole tree via [`NodeTree::node_ids_after`]
    /// with no limit. Fine at R0 scale; [`Self::tree_snapshot_page`] is the
    /// cursor-paged alternative for the protocol endpoint (D1: "usable at
    /// 10³–10⁴ nodes").
    pub fn tree_snapshot(&self) -> Vec<NodeSummary> {
        let (all_ids, _) = self.tree.node_ids_after(None, usize::MAX);
        let mut stack: Vec<NodeId> = all_ids
            .into_iter()
            .filter(|n| self.tree.parent(*n) == Some(None))
            .collect();
        // DFS from roots, preserving child order.
        stack.reverse();
        let mut visit = stack;
        let mut order = Vec::new();
        while let Some(n) = visit.pop() {
            order.push(n);
            if let Some(children) = self.tree.children(n) {
                for c in children.into_iter().rev() {
                    visit.push(c);
                }
            }
        }
        order
            .into_iter()
            .filter_map(|n| self.node_summary(n))
            .collect()
    }

    /// Cursor-paged tree snapshot (widen C4: "snapshot endpoints paginate, no
    /// small-tree assumption") — flat id order (not the DFS parent/child order
    /// `tree_snapshot` uses; a page is a slice of the id space, not a subtree).
    /// Returns up to `limit` rows after `cursor`, plus the next cursor to page
    /// with (`None` once exhausted). Built on [`NodeTree::node_ids_after`], the
    /// one additive pagination primitive this leaf adds to `NodeTree`.
    pub fn tree_snapshot_page(
        &self,
        cursor: Option<NodeId>,
        limit: usize,
    ) -> (Vec<NodeSummary>, Option<NodeId>) {
        let (ids, next) = self.tree.node_ids_after(cursor, limit);
        let nodes = ids
            .into_iter()
            .filter_map(|n| self.node_summary(n))
            .collect();
        (nodes, next)
    }

    /// Build one node's summary row, or `None` if `n` doesn't exist (a benign
    /// race with a concurrent tree mutation — callers filter these out).
    fn node_summary(&self, n: NodeId) -> Option<NodeSummary> {
        let state = self.tree.state(n)?;
        let pending = self.pending_hole(n);
        let is_fork = matches!(
            pending.as_ref().map(|c| &c.routing),
            Some(HoleRouting::Fork { .. })
        );
        let prompt = pending.as_ref().map(|c| c.prompt.clone());
        Some(NodeSummary {
            node: n,
            parent: self.tree.parent(n).flatten(),
            state,
            is_fork_hole: is_fork,
            hole_prompt: prompt,
            awaiting_operator: self.escalations.lock().contains_key(&n),
        })
    }

    /// `node`'s live heap/GC snapshot, straight off its resident
    /// `JitEffectMachine` — what the observatory heap pane renders. `None`
    /// when `node` has no live session (never forced, terminal) or during the
    /// transient mid-turn gap while its session runs on the blocking pool
    /// (the same benign race [`Self::node_summary`] tolerates).
    pub fn heap_stats(&self, node: NodeId) -> Option<HeapSummary> {
        let convos = self.convos.lock();
        let stats = convos.get(&node)?.session.as_ref()?.heap_stats()?;
        Some(HeapSummary {
            nursery_bytes: stats.nursery_bytes,
            live_bytes: stats.live_bytes,
            gc_count: stats.gc_count,
        })
    }

    /// Create a ROOT node as a thunk with the DEFAULT system framing
    /// ([`engine::SYSTEM_FRAMING`]). `title` seeds the teaser + first user
    /// turn. Equivalent to [`Self::create_root_framed`] with `framing: None`.
    pub fn create_root(&self, title: &str, prompt: &str) -> Result<NodeId, HarnessError> {
        self.create_root_framed(title, prompt, None)
    }

    /// Create a ROOT node as a thunk with an EXPLICIT per-node system message
    /// `framing` (overriding the default [`engine::SYSTEM_FRAMING`] once the
    /// node is forced). `title` seeds the teaser + first user turn.
    /// The self-iterating harness's per-loop answerer session uses this to
    /// install `render`'s output as the answerer's system prompt (W1/C1).
    pub fn create_root_framed(
        &self,
        title: &str,
        prompt: &str,
        framing: Option<String>,
    ) -> Result<NodeId, HarnessError> {
        let node = self.tree.create_node(
            None,
            title,
            self.cfg.effect_names.clone(),
            ForkShape::Exact(0),
            false,
        )?;
        // Seed the (not-yet-live) transcript with the operator's opening prompt
        // and the node's framing. The convo entry is created lazily at force
        // time; stash both in the pending seed map until then.
        self.seeds
            .lock()
            .insert(node, (prompt.to_string(), framing));
        Ok(node)
    }

    /// Force a thunk node: emit `Forced`, bootstrap its resident session, seed
    /// its transcript with the opening prompt. Returns the node's session id.
    pub fn force(&self, node: NodeId, actor: Actor) -> Result<(), HarnessError> {
        // Bootstrap a fresh resident session for this node, keeping a handle to
        // its effect-trace buffer so per-turn effects can be logged.
        let (stack, effect_trace) = self.build_stack();
        // Give the node its OWN decl plane so declarations accumulate across its
        // turns (a value bound in turn N is a live binding in turn N+1). Each
        // node's plane is rooted in its own directory, so a fork parent's
        // declarations survive independently of any child's — the child forces a
        // separate node with a separate plane. Degrades to no accumulation
        // (`None`) if the session root cannot be created.
        let lib = self.node_decl_plane(node);
        let session = ResidentSession::bootstrap(
            &self.boot.expr,
            self.boot.table.clone(),
            stack,
            self.cfg.suspend_tag,
            self.cfg.effect_names.clone(),
            CapturedOutput::new(),
            self.cfg.include.clone(),
            DEFAULT_NURSERY_SIZE,
            lib,
        )
        .map_err(|e| HarnessError::Resident(e.to_string()))?;

        // Register with the tree (emits Forced BEFORE the session is visible;
        // the tree's `M = ()` machine handle is unused — the Harness owns the
        // real session).
        self.tree.force(node, actor, ())?;

        // Seed the transcript: a fork/fanout answerer inherits its parent's
        // transcript (set by `register_fork_child`); a plain root gets its
        // opening prompt.
        let mut convos = self.convos.lock();
        let inherited = self.forked_transcripts.lock().remove(&node);
        let (transcript, framing) = match inherited {
            // A fork/fanout answerer inherits its parent's transcript (turns
            // already in the log — nothing to re-log) AND the parent's framing,
            // so the child's request prefix is byte-identical to the parent's
            // through the fork checkpoint (exact-context fork).
            Some((t, framing)) => (t, framing),
            // A plain root: log its opening prompt as a User turn so the
            // transcript shows what was asked, not just the model's reply
            // (symmetric with the assistant `turn_delta` in `drive_turn`).
            None => {
                let (seed, framing) = self
                    .seeds
                    .lock()
                    .remove(&node)
                    .unwrap_or_else(|| ("Begin.".to_string(), None));
                self.tree
                    .turn_delta(node, 0, Role::User, seed.clone(), None)?;
                (
                    vec![Message {
                        role: Role::User,
                        content: seed,
                    }],
                    framing,
                )
            }
        };
        convos.insert(
            node,
            NodeConvo {
                session: Some(session),
                transcript,
                turn_seq: 0,
                effect_trace,
                effect_seq: 0,
                pending: None,
                answer_contract: None,
                suspend_table: None,
                suspend_asks: AsksSidecar::default(),
                pending_bind: None,
                usage: Usage::default(),
                last_input_tokens: 0,
                framing,
            },
        );
        Ok(())
    }

    /// Build the concrete handler stack (rooted at the process CWD sandbox),
    /// wrapped in a [`TracingDispatcher`] that records every effect into the
    /// returned [`EffectTrace`] — the harness drains it per turn to write
    /// `Event::Effect` (the observatory's trace pane).
    fn build_stack(&self) -> (BoxedStack, EffectTrace) {
        let cfg = tidepool_handlers::HandlerConfig {
            cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            kv_path: tidepool_runtime::paths::cache_dir().join("harness-kv.json"),
            llm_model: std::env::var("TIDEPOOL_LLM_MODEL")
                .unwrap_or_else(|_| "gpt-4o-mini".to_string()),
        };
        let trace: EffectTrace = Arc::new(std::sync::Mutex::new(Vec::new()));
        let stack =
            TracingDispatcher::new(tidepool_handlers::build_base_stack(&cfg), trace.clone());
        (Box::new(stack), trace)
    }

    /// Create a fresh, per-node decl plane rooted in its OWN directory, so a
    /// declaration turn accumulates for that node alone (independent of every
    /// other node's plane). Returns `None` — degrading to no cross-turn
    /// accumulation — if the root can't be prepared. The validation include is
    /// the node's compile include, so a decl can resolve the same imports a turn
    /// does (stdlib, the generated effects module).
    fn node_decl_plane(&self, node: NodeId) -> Option<SessionLib> {
        let root = tidepool_runtime::paths::cache_dir()
            .join("harness-sessions")
            .join(format!("node-{}", node.0));
        // Fresh: clear any stale gen modules left by a prior run at this node id.
        let _ = std::fs::remove_dir_all(&root);
        SessionLib::open(SessionId(node.0), &root, ModuleEnv::standalone_default())
            .map(|lib| lib.with_validation_include(self.cfg.include.clone()))
            .ok()
    }
}

impl Harness {
    /// Drive ONE model turn on `node`: assemble its transcript, call the
    /// provider, extract + compile + run the block against the node's session.
    /// Logs the assistant turn and the resident outcome. Returns the turn
    /// outcome (Completed / Suspended / NoBlock).
    ///
    /// This is the inner step of the turn loop; [`Self::run_to_hole_or_done`]
    /// loops it until the node suspends, completes, or hits the turn cap.
    pub async fn drive_turn(&self, node: NodeId) -> Result<engine::TurnOutcome, HarnessError> {
        // Snapshot the transcript AND the node's framing under the lock, then
        // release before the await.
        let (transcript, turn_seq, framing) = {
            let convos = self.convos.lock();
            let convo = convos.get(&node).ok_or(HarnessError::NoSession(node))?;
            (
                convo.transcript.clone(),
                convo.turn_seq,
                convo.framing.clone(),
            )
        };

        // Stream the provider call into `node`'s live-turn buffer (rendered
        // token-by-token), then log the completed turn with its thinking and
        // swap the buffer for the durable turn in one frame.
        let provider_started = std::time::Instant::now();
        let driven = self
            .stream_turn(node, &transcript, framing.as_deref())
            .await?;
        timing::record_stage(
            node.0,
            timing::NO_ROUND,
            timing::STAGE_PROVIDER_CALL,
            provider_started.elapsed(),
            0,
        );
        self.tree.turn_delta_reasoned(
            node,
            turn_seq,
            Role::Assistant,
            driven.reply.clone(),
            Some(driven.usage),
            driven.reasoning.clone(),
        )?;
        self.finish_live_turn(node);
        {
            let mut convos = self.convos.lock();
            let convo = convos.get_mut(&node).ok_or(HarnessError::NoSession(node))?;
            convo.transcript.push(Message {
                role: Role::Assistant,
                content: driven.reply.clone(),
            });
            convo.turn_seq += 1;
            convo.usage.input_tokens += driven.usage.input_tokens;
            convo.usage.output_tokens += driven.usage.output_tokens;
            // C-1: the latest turn's input_tokens IS the node's real context
            // size (the provider re-sends the whole transcript each round, so
            // its input count already includes every prior turn). Overwrite,
            // don't accumulate — this is the high-water the compaction
            // threshold reads.
            convo.last_input_tokens = driven.usage.input_tokens;
        }

        let Some(block) = driven.block else {
            // WS4 (self-iterating-harness): a prose-only turn ran no Haskell, but
            // still record a `TurnStart` whose `source` is the reply text — so a
            // node's durable log always shows one `TurnStart` per model turn
            // (`tail`ing it never has a silent gap), and consent integrity's
            // "no Turn/Effect before Forced" holds (this is well after Forced).
            self.tree.turn_start(node, driven.reply.clone(), None)?;
            return Ok(engine::TurnOutcome::NoBlock {
                reply: driven.reply,
            });
        };

        // WS4 (self-iterating-harness): record the EXTRACTED executed Haskell as
        // this turn's `TurnStart.source` — so `tail -f <log>` shows the exact
        // block the turn ran, not a coarse "model" provenance tag (external-review
        // finding 2: "tail the logs to see executed Haskell" must actually work).
        // Emitted before the block runs, so it precedes this turn's Effect /
        // HolePublished events in the durable log.
        self.tree.turn_start(node, block.clone(), None)?;
        tracing::debug!(node = node.0, %block, "executed Haskell");

        // Compile + run the block synchronously (spawn_blocking off the reactor).
        let (imports, body) = engine::split_imports(&block);
        self.run_block(node, &body, &imports, "").await
    }

    /// Drive ONE plain model turn on `node`: push `prompt` as a User message,
    /// call the provider, log the assistant reply, and return its RAW TEXT plus
    /// that single turn's [`Usage`] — WITHOUT compiling or running any Haskell
    /// block. Unlike [`Self::drive_turn`], the model's answer is captured as
    /// prose, not executed; the node's session is untouched (still idle), so it
    /// can keep driving afterward.
    ///
    /// This is the self-iterating harness's simplified compaction primitive
    /// (review C-2/H-1/J-1): compaction is ONE ordinary turn on the answerer
    /// session that ALREADY holds the full context — "summarize everything
    /// above" — no second node, no `finalize`, no serializing the transcript
    /// into a prompt (the model has it in context). The caller then resets the
    /// session's context to the returned summary via
    /// [`Self::replace_transcript_with_summary`].
    pub async fn summarize_turn(
        &self,
        node: NodeId,
        prompt: &str,
    ) -> Result<(String, Usage), HarnessError> {
        // Push the summarize request, then snapshot the transcript + framing.
        self.push_user_turn(node, prompt)?;
        let (transcript, turn_seq, framing) = {
            let convos = self.convos.lock();
            let convo = convos.get(&node).ok_or(HarnessError::NoSession(node))?;
            (
                convo.transcript.clone(),
                convo.turn_seq,
                convo.framing.clone(),
            )
        };

        self.tree.turn_start(node, "model".to_string(), None)?;
        let driven = self
            .stream_turn(node, &transcript, framing.as_deref())
            .await?;
        self.tree.turn_delta_reasoned(
            node,
            turn_seq,
            Role::Assistant,
            driven.reply.clone(),
            Some(driven.usage),
            driven.reasoning.clone(),
        )?;
        self.finish_live_turn(node);
        {
            let mut convos = self.convos.lock();
            let convo = convos.get_mut(&node).ok_or(HarnessError::NoSession(node))?;
            convo.transcript.push(Message {
                role: Role::Assistant,
                content: driven.reply.clone(),
            });
            convo.turn_seq += 1;
            convo.usage.input_tokens += driven.usage.input_tokens;
            convo.usage.output_tokens += driven.usage.output_tokens;
            convo.last_input_tokens = driven.usage.input_tokens;
        }
        Ok((driven.reply, driven.usage))
    }

    /// Compile a `block` (with optional imports/helpers) and run it against
    /// `node`'s resident session as a TOP-LEVEL turn. Classifies a suspension.
    async fn run_block(
        &self,
        node: NodeId,
        block: &str,
        imports: &str,
        helpers: &str,
    ) -> Result<engine::TurnOutcome, HarnessError> {
        let cfg_bin = self.cfg.extract_bin.clone();

        // Classify the block OFF-REACTOR (it shells the extractor). A top-level
        // DECLARATION accumulates on the node's decl plane (so a value declared
        // this turn is a live binding next turn) instead of running as an
        // expression; a classify failure falls through to the expression path,
        // which re-reports any real error.
        let block_owned = block.to_string();
        let classify_started = std::time::Instant::now();
        let classification = {
            let b = block_owned.clone();
            tokio::task::spawn_blocking(move || classify_turn(&b))
                .await
                .map_err(|e| HarnessError::Resident(format!("classify task join: {e}")))?
                .ok()
        };
        timing::record_stage(
            node.0,
            timing::NO_ROUND,
            timing::STAGE_CLASSIFY_EXTRACT,
            classify_started.elapsed(),
            0,
        );
        let kind = classification.as_ref().map(|c| c.kind);

        if kind == Some(TurnKind::Decl) {
            let mut session = self.take_session(node)?;
            let (session, res) = tokio::task::spawn_blocking(move || {
                let r = session.define_scoped(&[&block_owned]);
                (session, r)
            })
            .await
            .map_err(|e| HarnessError::Resident(format!("declare task join: {e}")))?;
            self.put_session(node, session, None, AsksSidecar::default());
            self.flush_effects(node);
            return match res {
                Ok(gen) => {
                    let rendered = format!("declared (gen {})", gen.0);
                    self.tree.node_done(node, rendered.clone())?;
                    Ok(engine::TurnOutcome::Completed { rendered })
                }
                Err(e) => {
                    let msg = format!("The declaration failed: {e}");
                    self.push_user_turn(node, &msg)?;
                    Err(HarnessError::Resident(e.to_string()))
                }
            };
        }

        // A value-plane BIND turn (`x <- e`) materializes its result into the
        // node's value plane so a later turn can reference it. Single-binder only
        // for now (multi-bind is a follow-up); a bind with no parsed binder name
        // falls through to the expression path below.
        if kind == Some(TurnKind::Bind) {
            if let Some(name) = classification.and_then(|c| c.binders.into_iter().next()) {
                return self
                    .run_bind_turn(node, block, imports, helpers, &name)
                    .await;
            }
        }

        // Expression turn: make the compile session-aware — import the node's
        // current `Lib.G<g>` decl module (if any) and add its directory to the
        // search path, so a reference to a prior turn's declaration resolves. The
        // decl context is peeked under the lock WITHOUT checking the session out,
        // so a compile failure below never leaks it (the session is taken only
        // once a compiled fragment is in hand — as the original path did).
        let (session_module, session_include) = self.session_decl_context(node);
        // The node's answer contract (when it is driving toward a `finalize`)
        // contributes both halves: its `imports` put the answer type in scope,
        // and its `ty` pins `finalize` to that type inside `template_turn`.
        let contract = self.answer_contract(node);
        let mut import_lines: Vec<String> = contract
            .iter()
            .flat_map(|c| c.imports.iter().cloned())
            .collect();
        if !imports.is_empty() {
            import_lines.push(imports.to_string());
        }
        import_lines.extend(session_module);
        let merged_imports = import_lines.join("\n");
        let template_started = std::time::Instant::now();
        let src = engine::template_turn(
            &self.cfg,
            block,
            &merged_imports,
            helpers,
            contract.as_ref().map(|c| c.ty.as_str()),
        );
        timing::record_stage(
            node.0,
            timing::NO_ROUND,
            timing::STAGE_TEMPLATE,
            template_started.elapsed(),
            src.len() as u64,
        );
        let mut include = self.cfg.include.clone();
        if let Some(dir) = session_include {
            include.push(dir);
        }

        // Compile off-reactor (the session is still resident — no leak on a
        // compile failure).
        let node_id = node.0;
        let compiled = tokio::task::spawn_blocking(move || {
            compile::compile_turn(
                &cfg_bin,
                &src,
                "result",
                &include,
                node_id,
                timing::NO_ROUND,
            )
        })
        .await
        .map_err(|e| HarnessError::Resident(format!("compile task join: {e}")))?
        .map_err(|e| HarnessError::Compile(e.to_string()))?;

        // Run the compiled fragment against the session (move it onto the
        // blocking pool and back — the resident session is `Send`).
        let mut session = self.take_session(node)?;
        let expr = compiled.expr;
        let table = compiled.table.clone();
        let asks = compiled.asks;

        let (session, outcome) = tokio::task::spawn_blocking(move || {
            let out = session.run("turn", &expr, &table);
            (session, out)
        })
        .await
        .map_err(|e| HarnessError::Resident(format!("run task join: {e}")))?;

        // Restore the session, flush effects, and classify the outcome — shared
        // with the value-plane bind path (`None` = this turn is not a bind).
        self.finish_run(node, session, outcome, compiled.table, asks, None)
    }

    /// Shared turn epilogue: restore the session, flush effects, and turn a
    /// [`ResidentOutcome`] into a [`engine::TurnOutcome`] — `node_done` on
    /// completion, publish + `set_pending` on suspension. `pending_bind` is
    /// `Some` for a value-plane BIND turn that suspended (`x <- fork …`): it is
    /// stashed so `resume_parent` drives `resume_bind` and the binding
    /// materializes when the fork answers. A completion needs no `pending_bind`
    /// handling — `run_bind` already materialized it.
    fn finish_run(
        &self,
        node: NodeId,
        session: Session,
        outcome: Result<ResidentOutcome, ResidentError>,
        table: DataConTable,
        asks: AsksSidecar,
        pending_bind: Option<(BoundBinder, Generation)>,
    ) -> Result<engine::TurnOutcome, HarnessError> {
        self.put_session(node, session, Some(table.clone()), asks.clone());
        self.flush_effects(node);

        match outcome {
            Ok(ResidentOutcome::Completed { result, .. }) => {
                let rendered = result.to_string_pretty();
                self.tree.node_done(node, rendered.clone())?;
                Ok(engine::TurnOutcome::Completed { rendered })
            }
            Ok(ResidentOutcome::Suspended { hole, request, .. }) => {
                let classified = engine::classify_hole(&request, &table, &asks);
                let fork = matches!(classified.routing, HoleRouting::Fork { .. });
                let ty = match &classified.routing {
                    HoleRouting::Fork { ty, .. }
                    | HoleRouting::RunLLMTurn { ty, .. }
                    | HoleRouting::Finalize { ty, .. } => ty.clone(),
                    _ => None,
                };
                let site = match &classified.routing {
                    HoleRouting::Fork { site, .. }
                    | HoleRouting::RunLLMTurn { site, .. }
                    | HoleRouting::Finalize { site, .. } => Some(crate::tree::SiteId(*site)),
                    _ => None,
                };
                self.tree.hole_published(
                    node,
                    HoleId(hole.clone()),
                    site,
                    ty,
                    classified.prompt.clone(),
                    fork,
                )?;
                self.set_pending(
                    node,
                    PendingHole {
                        hole: HoleId(hole.clone()),
                        classified: classified.clone(),
                        raw_request: request,
                    },
                );
                // A suspended bind: remember the binder+gen so the resume path
                // materializes it (via `resume_bind`) when the fork answers.
                if let Some(pb) = pending_bind {
                    let mut convos = self.convos.lock();
                    if let Some(convo) = convos.get_mut(&node) {
                        convo.pending_bind = Some(pb);
                    }
                }
                Ok(engine::TurnOutcome::Suspended {
                    hole,
                    classified,
                    table,
                })
            }
            Err(e) => {
                let msg = format!("The eval failed at runtime: {e}");
                self.push_user_turn(node, &msg)?;
                Err(HarnessError::Resident(e.to_string()))
            }
        }
    }

    /// Peek a node's value-plane compile context WITHOUT checking the session
    /// out (so a compile failure never leaks it): `(decl module import, live
    /// `Val.G` inject modules, session root, the next value generation to mint)`.
    /// `None` when the node has no session or no decl plane.
    fn session_bind_context(
        &self,
        node: NodeId,
    ) -> Option<(String, Vec<String>, PathBuf, Generation)> {
        let convos = self.convos.lock();
        let s = convos.get(&node).and_then(|c| c.session.as_ref())?;
        let root = s.lib_include_dir()?;
        // Imports: the decl `Lib.G<g>` module + the CURRENT `Val.G<g>` module of
        // each live name (newest gen only — shadowed gens are injected, not
        // imported, to avoid an ambiguous occurrence). Injection (`--inject-val`)
        // uses ALL live gens.
        let mut import_lines: Vec<String> = Vec::new();
        if let Some(m) = s.session_import_module() {
            import_lines.push(m);
        }
        import_lines.extend(s.current_val_modules());
        Some((
            import_lines.join("\n"),
            s.inject_val_modules(),
            root,
            s.val_gen().next(),
        ))
    }

    /// Run a value-plane BIND turn (`x <- e`): compile it session-aware (the
    /// extract's `--session-root`/`--inject-val`/`--session-bind` path via
    /// `compile_session_turn`, so it resolves earlier value bindings and emits
    /// binder metadata), then drive `run_bind`. A fork bind suspends here and its
    /// value is materialized on resume (`finish_run` stashes the binder).
    async fn run_bind_turn(
        &self,
        node: NodeId,
        stmt: &str,
        imports: &str,
        helpers: &str,
        binder_name: &str,
    ) -> Result<engine::TurnOutcome, HarnessError> {
        let Some((session_imports, inject, session_root, gen)) = self.session_bind_context(node)
        else {
            return Err(HarnessError::Resident(
                "value-plane bind requires a node decl plane".into(),
            ));
        };
        // Decls + current value modules are IMPORTED (name visibility); values
        // are ALSO injected (`--inject-val`) so they resolve at runtime via the
        // session's ExternalEnv.
        let merged_imports = match (imports.is_empty(), session_imports.is_empty()) {
            (_, true) => imports.to_string(),
            (true, false) => session_imports,
            (false, false) => format!("{imports}\n{session_imports}"),
        };
        let src =
            engine::template_session_bind(&self.cfg, stmt, binder_name, &merged_imports, helpers);
        let mut include = self.cfg.include.clone();
        include.push(session_root.clone());
        let names = vec![binder_name.to_string()];
        let g0 = gen.0;

        // Compile off-reactor through the session-aware path.
        let compiled = tokio::task::spawn_blocking(move || {
            let include_refs: Vec<&Path> = include.iter().map(PathBuf::as_path).collect();
            compile_session_turn(
                &src,
                &include_refs,
                &session_root,
                &inject,
                Some(SessionBind {
                    names: &names,
                    gen: g0,
                }),
            )
        })
        .await
        .map_err(|e| HarnessError::Resident(format!("bind compile join: {e}")))?
        .map_err(|e| HarnessError::Compile(e.to_string()))?;

        let binder = match compiled.binders.into_iter().next() {
            Some(b) => b,
            None => {
                return Err(HarnessError::Resident(
                    "session-bind emitted no binder metadata".into(),
                ))
            }
        };
        let asks = AsksSidecar::from_pairs(compiled.asks);
        let table = compiled.table;
        let expr = compiled.expr;

        let mut session = self.take_session(node)?;
        let binder_for_run = binder.clone();
        let run_table = table.clone();
        let (session, outcome) = tokio::task::spawn_blocking(move || {
            let out = session.run_bind("bind", &expr, &run_table, &binder_for_run, gen);
            (session, out)
        })
        .await
        .map_err(|e| HarnessError::Resident(format!("bind run join: {e}")))?;

        self.finish_run(node, session, outcome, table, asks, Some((binder, gen)))
    }

    /// Loop [`Self::drive_turn`] until the node SUSPENDS at a hole, COMPLETES,
    /// or hits the per-node turn cap. A pure-prose turn (NoBlock) feeds a nudge
    /// and loops; a turn whose block DOESN'T COMPILE feeds the GHC error back so
    /// the model can self-correct (the same type-retry the fork answerer gets —
    /// a common, fixable model mistake like wrong verb arity shouldn't cancel
    /// the whole node). Returns the terminal turn outcome; only exhausting the
    /// turn cap surfaces an error to the caller.
    pub async fn run_to_hole_or_done(
        &self,
        node: NodeId,
    ) -> Result<engine::TurnOutcome, HarnessError> {
        let mut turns = 0;
        loop {
            if turns >= self.cfg.max_turns {
                return Err(EngineError::NoBlock { turns }.into());
            }
            turns += 1;
            match self.drive_turn(node).await {
                Ok(
                    out @ (engine::TurnOutcome::Completed { .. }
                    | engine::TurnOutcome::Suspended { .. }),
                ) => return Ok(out),
                Ok(engine::TurnOutcome::NoBlock { .. }) => {
                    self.push_user_turn(
                        node,
                        "Reply with a single ```haskell block to run (or to answer the \
                         hole with `resume expr`).",
                    )?;
                }
                // The model's Haskell didn't compile — feed the GHC error back
                // verbatim (capped) as a corrective user turn and retry, rather
                // than cancelling the node.
                Err(HarnessError::Compile(msg)) => {
                    let ghc = truncate_ghc_error(&msg);
                    self.push_user_turn(
                        node,
                        &format!(
                            "That Haskell did not compile. Fix it and reply with a \
                             corrected single ```haskell block. Common causes: a verb \
                             needs more arguments (e.g. `grepGlob pat path`), or you \
                             passed `Text` where a different type is expected.\n\n\
                             GHC error:\n{ghc}"
                        ),
                    )?;
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// Continue a COMPLETED node's conversation with a new operator message —
    /// the multi-turn REPL follow-up. Reopens the `Done` node
    /// ([`NodeTree::reopen`]), appends `message` as a user turn, and drives to
    /// the next hole/done. The node's resident session persisted past its
    /// previous completion, so bindings and heap carry over — this continues
    /// the session rather than restarting it. Errors if the node isn't `Done`
    /// or its session is gone (e.g. it was cancelled).
    pub async fn follow_up(
        &self,
        node: NodeId,
        message: &str,
    ) -> Result<engine::TurnOutcome, HarnessError> {
        if !self.convos.lock().contains_key(&node) {
            return Err(HarnessError::NoSession(node));
        }
        self.tree.reopen(node)?;
        self.push_user_turn(node, message)?;
        match self.run_to_hole_or_done(node).await {
            Ok(out) => Ok(out),
            Err(e) => {
                // The follow-up turn failed: return the node to `Done` so the
                // prior conversation is preserved and still continuable, rather
                // than stranded `Running`. The failure is recorded as the turn's
                // result so it shows in the transcript.
                let _ = self.tree.node_done(node, format!("follow-up failed: {e}"));
                self.finish_live_turn(node);
                Err(e)
            }
        }
    }
}

// -- answering holes: fork, in-context return, operator dialog ---------------

impl Harness {
    /// The classified pending hole on `node`, if it is suspended.
    pub fn pending_hole(&self, node: NodeId) -> Option<ClassifiedHole> {
        self.convos
            .lock()
            .get(&node)
            .and_then(|c| c.pending.as_ref())
            .map(|p| p.classified.clone())
    }

    /// Like [`Self::pending_hole`], but also returns the hole id and the
    /// compile table the pending suspension's constructor ids resolve
    /// against — what a caller needs to build a
    /// [`engine::TurnOutcome::Suspended`] out of a `pending_hole` read (the
    /// self-iterating-harness driver's `AskUser` servicing loop, which reads
    /// the pending hole again after a resume rather than threading the
    /// original `drive_turn` outcome through). `None` if `node` isn't
    /// suspended or has no compile table recorded.
    pub(crate) fn pending_hole_full(
        &self,
        node: NodeId,
    ) -> Option<(HoleId, ClassifiedHole, DataConTable)> {
        let convos = self.convos.lock();
        let convo = convos.get(&node)?;
        let pending = convo.pending.as_ref()?;
        let table = convo.suspend_table.clone()?;
        Some((pending.hole.clone(), pending.classified.clone(), table))
    }

    /// Reconstruct a [`TurnOutcome::Suspended`] from `node`'s CURRENT pending
    /// hole (self-iterating-harness fork widen: after [`Self::answer_fanout`]/
    /// [`Self::answer_fork`] resumes a parent answerer, the driver needs the
    /// parent's freshly re-published hole in the same shape
    /// [`Self::drive_turn`] returns, without re-running a model turn).
    /// `None` if `node` isn't currently suspended.
    pub fn pending_turn_outcome(&self, node: NodeId) -> Option<TurnOutcome> {
        let convos = self.convos.lock();
        let convo = convos.get(&node)?;
        let pending = convo.pending.as_ref()?;
        Some(TurnOutcome::Suspended {
            hole: pending.hole.0.clone(),
            classified: pending.classified.clone(),
            table: convo.suspend_table.clone().unwrap_or_default(),
        })
    }

    /// The pending `Ui` value for a `dialogAsk` hole on `node`, deserialized
    /// from the routing payload — what the observatory form pane renders. `None`
    /// unless the node is suspended on a Dialog hole with a well-formed `Ui`.
    pub fn pending_dialog_ui(&self, node: NodeId) -> Option<crate::ui::Ui> {
        match self.pending_hole(node)?.routing {
            // `dialogAsk :: Ui -> M Value` (typed at the Haskell surface): the
            // payload is a well-formed `Ui` by construction, so a parse failure
            // here means a genuine wire mismatch, not a model mistake. No
            // boundary coercion — the type system guards the shape upstream.
            HoleRouting::Dialog { ui } => serde_json::from_value(ui).ok(),
            _ => None,
        }
    }

    /// The SERVER-DERIVED `Ui` form (§6 D6, `uiof::ui_of`) for a
    /// `runLLMTurn`/`runLLMTurnFork` hole on `node`, when the answer
    /// type maps mechanically — what the hole card renders IN ADDITION TO
    /// the raw Code+eval card, when `Some`. `None` when the node isn't
    /// suspended on such a hole, its answer type is unknown, or `uiof::ui_of`
    /// can't map the type (the caller falls back to the raw card as today).
    pub fn pending_derived_ui(&self, node: NodeId) -> Option<crate::ui::Ui> {
        let convos = self.convos.lock();
        let convo = convos.get(&node)?;
        let pending = convo.pending.as_ref()?;
        let ty = match &pending.classified.routing {
            HoleRouting::RunLLMTurn { ty: Some(ty), .. }
            | HoleRouting::Fork { ty: Some(ty), .. } => ty.clone(),
            _ => return None,
        };
        let table = convo.suspend_table.clone()?;
        drop(convos);
        crate::uiof::ui_of(&table, &ty)
    }

    /// The harness-level primitive `service_runllm_hole` (self-iterating-
    /// harness WS-A, `selfharness/driver.rs`) calls once a nested Agent node
    /// suspends on `finalize @T x` (self-iterating-harness WS-B): read the
    /// finalized value straight out of the suspended request `Value` (NEVER
    /// through JSON — it may carry a closure or other non-serializable
    /// value, per `finalize`'s relaxed function-arrow rule) and terminate
    /// the node.
    ///
    /// `finalize` does NOT resume the Agent (unlike answering a
    /// `RunLLMTurn`/`Fork` hole via [`Self::drive_answerer_to_value`]) — it
    /// TERMINATES the node's turn loop and hands the value UP, so this uses
    /// [`NodeTree::node_cancelled`] (a `Suspended` node has no `node_done`
    /// transition — that one is reserved for a turn that ran to completion
    /// from `Running`; `Cancelled` is the tree's only terminal-from-Suspended
    /// move) with a `"finalized"` reason — a SUCCESSFUL termination, not a
    /// failure, even though the tree's own state name reads that way; the
    /// node's session is kept alive past this call, same as any other
    /// terminal node, so the observatory can still show it. The caller is
    /// expected to `run_child` the returned `Value` into the OUTER
    /// (Harness-monad) session to resolve the parent `runLLMTurn` hole,
    /// zero-copy.
    ///
    /// Errors if `node` has no live session, isn't suspended, or its pending
    /// hole isn't `Finalize`-routed.
    pub fn take_finalized_value(&self, node: NodeId) -> Result<Value, HarnessError> {
        self.take_finalized_value_with_table(node).map(|(v, _)| v)
    }

    /// Extract the finalized value + its compile table out of `node`'s pending
    /// `Finalize` hole, clearing the pending hole but NOT terminating the node.
    /// The caller decides the node's next state (cancel via
    /// [`Self::take_finalized_value_with_table`], or keep it reopenable via
    /// [`Self::take_finalized_value_keep_open`]).
    fn take_finalized_value_core(
        &self,
        node: NodeId,
    ) -> Result<(Value, DataConTable), HarnessError> {
        let mut convos = self.convos.lock();
        let convo = convos.get_mut(&node).ok_or(HarnessError::NoSession(node))?;
        let pending = convo
            .pending
            .as_ref()
            .ok_or(HarnessError::NotSuspended(node))?;
        if !matches!(pending.classified.routing, HoleRouting::Finalize { .. }) {
            return Err(HarnessError::RoutingMismatch {
                node,
                routing: "Finalize",
                actual: format!("{:?}", pending.classified.routing),
            });
        }
        let Value::Con(_, fields) = &pending.raw_request else {
            return Err(HarnessError::Resident(
                "finalize request was not a Con".into(),
            ));
        };
        let value = fields
            .get(1)
            .cloned()
            .ok_or_else(|| HarnessError::Resident("FinalizeWith missing its value field".into()))?;
        // C5: a finalize suspension without its compile table is an
        // inconsistency — fail LOUD rather than defaulting to an empty
        // table, which would silently misrender the finalized value's
        // constructor ids.
        let table = convo.suspend_table.clone().ok_or_else(|| {
            HarnessError::Resident(
                "finalize suspension has no compile table (cannot resolve the value's \
                 constructor ids)"
                    .into(),
            )
        })?;
        convo.pending = None;
        Ok((value, table))
    }

    /// Like [`Self::take_finalized_value`], but also returns the
    /// [`DataConTable`] the finalized value's constructor ids resolve
    /// against — needed by a caller that renders the raw value itself
    /// (self-iterating-harness WS-E's forced compaction turn finalizes a
    /// `Text`, then reads it out via [`tidepool_runtime::value_to_json`],
    /// which needs the SAME table the value was compiled with) rather than
    /// just feeding it opaquely into another suspended continuation. TERMINATES
    /// the node (`Cancelled`) — the finalized node is done.
    pub fn take_finalized_value_with_table(
        &self,
        node: NodeId,
    ) -> Result<(Value, DataConTable), HarnessError> {
        let (value, table) = self.take_finalized_value_core(node)?;
        self.tree.node_cancelled(node, "finalized".to_string())?;
        Ok((value, table))
    }

    /// Like [`Self::take_finalized_value`], but keeps the node + its resident
    /// session LIVE and reusable instead of cancelling — the self-iterating
    /// harness's per-loop answerer (W1/C2) reuses ONE node across the loop's
    /// holes so the model's transcript (the accumulating context window)
    /// persists, and hole #2 sees hole #1's exchange.
    ///
    /// Two things must happen for the reuse to work: (1) the TREE state goes
    /// `Suspended` → `Running` (`hole_consumed`) so a new turn is representable;
    /// (2) the RESIDENT SESSION's parked finalize continuation is ABORTED so it
    /// returns to idle — a suspended session REJECTS a new top-level turn
    /// (`ResidentError::Suspended`), and `finalize`'s continuation is terminal
    /// (nothing meaningful runs after it), so discarding it is exactly right.
    /// Without (2) the next hole's `drive_turn` cannot run a fresh block on the
    /// same session. Returns the finalized value (already read out of the
    /// suspended request by `take_finalized_value_core`, so aborting the
    /// continuation does not lose it).
    pub(crate) fn take_finalized_value_keep_open(
        &self,
        node: NodeId,
    ) -> Result<Value, HarnessError> {
        // Snapshot the pending finalize hole/continuation id BEFORE clearing it.
        let hole = {
            let convos = self.convos.lock();
            let convo = convos.get(&node).ok_or(HarnessError::NoSession(node))?;
            convo
                .pending
                .as_ref()
                .ok_or(HarnessError::NotSuspended(node))?
                .hole
                .clone()
        };
        let (value, _table) = self.take_finalized_value_core(node)?;

        // Abort the resident session's parked finalize continuation so the
        // session returns to idle and can run the NEXT hole's turn. `abort`
        // consumes the stowed continuation (clearing `pending` up front) and
        // then surfaces the abort as a terminal error outcome — that Err IS the
        // expected "continuation discarded" signal, not a failure, so it is
        // deliberately ignored. What matters is the session is now idle.
        let mut session = self.take_session(node)?;
        let _ = session.abort(&hole.0, "finalize consumed (answerer reused)".to_string());
        debug_assert!(
            session.is_idle(),
            "session must be idle after aborting the finalize continuation"
        );
        self.put_session(node, session, None, AsksSidecar::default());

        // Tree state: Suspended → Running, so the reused node accepts a new turn.
        self.tree.hole_consumed(node, hole)?;
        Ok(value)
    }

    /// Whether `node`'s pending finalize hole carries a CLOSURE value
    /// (self-iterating-harness W4): the tolerant suspend bridge substituted a
    /// `CLOSURE_SENTINEL` placeholder for field 1, so the finalized value is a
    /// live closure kept in-heap (applied by reference), not data. `false` for a
    /// plain-data finalize, or when `node` isn't suspended on a finalize hole.
    pub fn finalize_is_closure(&self, node: NodeId) -> bool {
        let convos = self.convos.lock();
        let Some(convo) = convos.get(&node) else {
            return false;
        };
        let Some(pending) = convo.pending.as_ref() else {
            return false;
        };
        if !matches!(pending.classified.routing, HoleRouting::Finalize { .. }) {
            return false;
        }
        matches!(
            &pending.raw_request,
            Value::Con(_, fields)
                if fields.get(1).is_some_and(|f| matches!(
                    f,
                    Value::Con(id, cf)
                        if id.0 == u64::MAX && cf.is_empty()
                ))
        )
    }

    /// Apply a `finalize`d CLOSURE by reference (self-iterating-harness W4):
    /// `node` must be suspended on a `finalize @(Int -> Int) f` hole whose value
    /// was kept LIVE in the shared heap (never deep-forced). This runs `f arg`
    /// in place against that same suspended heap — the "code as a value"
    /// round-trip — and returns the (data) result `Value`. The node stays
    /// suspended on its finalize hole afterward (the apply is a child run, like
    /// [`Self::eval_in_binding`]); the caller terminates it via
    /// [`Self::take_finalized_value`] when done.
    pub async fn apply_finalized_closure(
        &self,
        node: NodeId,
        arg: i64,
    ) -> Result<Value, HarnessError> {
        // Require the node to be suspended on a Finalize hole — the resident
        // session's parked continuation is what makes the child run (the apply)
        // legal, and the finalized closure's root was stashed on suspend.
        let is_finalize = matches!(
            self.pending_hole(node).map(|c| c.routing),
            Some(HoleRouting::Finalize { .. })
        );
        if !is_finalize {
            return Err(HarnessError::RoutingMismatch {
                node,
                routing: "Finalize",
                actual: format!("{:?}", self.pending_hole(node).map(|c| c.routing)),
            });
        }
        // The suspend turn's table, passed through so the apply fragment's
        // compile merges the closure's own defining constructors into the
        // accumulated session table (see `ResidentSession::apply_finalized`'s
        // doc for why no `I#`-id matching is needed here).
        let suspend_table = self
            .convos
            .lock()
            .get(&node)
            .and_then(|c| c.suspend_table.clone());
        let mut session = self.take_session(node)?;
        let (session, out) = tokio::task::spawn_blocking(move || {
            let out = session.apply_finalized(arg, suspend_table.as_ref());
            (session, out)
        })
        .await
        .map_err(|e| HarnessError::Resident(format!("apply_finalized join: {e}")))?;
        self.put_session(node, session, None, AsksSidecar::default());
        self.flush_effects(node);
        out.map(|r| r.into_value())
            .map_err(|e| HarnessError::Resident(e.to_string()))
    }

    /// Reopen a `Done` answerer node (`Done` → `Running`) for another turn —
    /// the self-iterating harness's bounded answerer drive (W1) reuses ONE
    /// per-loop node, and a `Completed` (non-finalize) turn leaves it `Done`,
    /// so a corrective re-prompt must reopen it first. Mirrors
    /// [`Self::follow_up`]'s reopen step. No-op-safe only from `Done`
    /// ([`crate::forcing::NodeTree::reopen`] refuses other states).
    pub(crate) fn reopen_node(&self, node: NodeId) -> Result<(), HarnessError> {
        self.tree.reopen(node)?;
        Ok(())
    }

    /// The running sum of every assistant turn's [`Usage`] logged on `node`
    /// so far (self-iterating-harness WS-E: what the driver's emergency
    /// compaction trigger accumulates across the `runLLMTurn` answerer nodes
    /// it drives per loop). `None` if `node` has no live session.
    pub fn node_usage(&self, node: NodeId) -> Option<Usage> {
        self.convos.lock().get(&node).map(|c| c.usage)
    }

    /// The MOST RECENT turn's `input_tokens` on `node` — the node's real
    /// current context size (the provider re-sends the whole transcript each
    /// round, so its per-round input count already includes every prior turn).
    /// This is a HIGH-WATER mark, not a running sum: the self-iterating
    /// harness's compaction threshold reads THIS (review C-1), never
    /// [`Self::node_usage`]'s summed `input_tokens`, which super-linearly
    /// over-counts across a multi-round hole. `Some(0)` before the node's
    /// first turn; `None` if `node` has no live session.
    pub fn node_last_input_tokens(&self, node: NodeId) -> Option<u64> {
        self.convos.lock().get(&node).map(|c| c.last_input_tokens)
    }

    /// `node`'s pending rung-2 escalation, if it is currently parked awaiting
    /// an operator decision — what the stuck-node popup renders.
    pub fn escalation_of(&self, node: NodeId) -> Option<Escalation> {
        self.escalations.lock().get(&node).cloned()
    }

    /// The first node currently parked on a rung-2 escalation, if any — what
    /// the inspector's stuck-node popup focuses by default (checked BEFORE
    /// the plain operator-hole focus, since an escalated answerer blocks fan
    /// progress and has no pending hole of its own to otherwise surface it).
    pub fn first_escalated_node(&self) -> Option<NodeId> {
        self.escalations.lock().keys().min().copied()
    }

    /// Resolve `node`'s pending rung-2 escalation with the operator's
    /// `decision` (driven by the web `/steer/:node` endpoint, or fired
    /// directly in a test to simulate the popup). Errors with
    /// [`HarnessError::NoPendingEscalation`] if `node` has no escalation
    /// parked (already resolved, or never escalated).
    pub fn resolve_escalation(
        &self,
        node: NodeId,
        decision: OperatorDecision,
    ) -> Result<(), HarnessError> {
        let tx = self
            .operator_decisions
            .lock()
            .remove(&node)
            .ok_or(HarnessError::NoPendingEscalation(node))?;
        tx.send(decision).map_err(|_| {
            HarnessError::Resident(format!(
                "node {node:?}: operator decision could not be delivered (its wait was already abandoned)"
            ))
        })
    }

    /// The first node currently suspended on a Dialog (operator) hole, if any —
    /// what the inspector focuses by default.
    pub fn first_operator_hole(&self) -> Option<NodeId> {
        let convos = self.convos.lock();
        convos.iter().find_map(|(n, c)| {
            matches!(
                c.pending.as_ref().map(|p| &p.classified.routing),
                Some(HoleRouting::Dialog { .. }) | Some(HoleRouting::Ask { .. })
            )
            .then_some(*n)
        })
    }

    /// Cancel + drop a fork/fanout CHILD that failed mid-drive, so no error
    /// path leaves it `Running` with a live resident session (the leak external
    /// review flagged: only the success path used to `node_done` + `drop_session`
    /// the child; a provider/join/log fault inside `drive_answerer_to_value`, or
    /// a `resume_parent` failure after, propagated via `?` and orphaned the
    /// child). Scoped to the fork/fanout callers deliberately — NOT baked into
    /// `drive_answerer_to_value` itself, which is also called in-context with
    /// `answerer == the main node`, where cancelling "the child" would kill the
    /// live agent. Idempotent w.r.t. the internal abort paths that already
    /// cancelled the child (cap-exhaustion / `ChildSuspended`): the tree
    /// transition is best-effort (`let _`), the session drop is the load-bearing
    /// half and is safe to repeat.
    fn cleanup_failed_child(&self, child: NodeId) {
        let _ = self
            .tree
            .node_cancelled(child, "fork child failed".to_string());
        self.drop_session(child);
    }

    /// Force + drive a FORK answerer for `node`'s pending single-fork hole
    /// (`fork @T` / `runLLMTurnFork @T`, `fan: None` — a fanout hole routes to
    /// [`Self::answer_fanout`] instead). Registers a child node (transcript
    /// forked at the checkpoint, framing inherited), forces it, drives
    /// its turn loop until it produces an answering block, runs that block
    /// via `run_child` against the parent, and resumes the parent with the
    /// resulting typed Value. The ONE deliberate ill-typed attempt in the golden
    /// path exercises the GHC-verbatim retry here (a compile failure feeds back
    /// as the child's next user turn; the parent's continuation is untouched).
    pub async fn answer_fork(&self, node: NodeId, actor: Actor) -> Result<NodeId, HarnessError> {
        let pending = self
            .convos
            .lock()
            .get(&node)
            .and_then(|c| c.pending.clone())
            .ok_or(HarnessError::NotSuspended(node))?;
        let (site_ty, prompt) = match &pending.classified.routing {
            HoleRouting::Fork { ty, fan: None, .. } => {
                (ty.clone(), pending.classified.prompt.clone())
            }
            other => {
                return Err(HarnessError::RoutingMismatch {
                    node,
                    routing: "fork",
                    actual: format!("{other:?}"),
                })
            }
        };

        // Register the fork child: inherit the parent transcript up to the
        // parent's current turn, append the hole card.
        let child = self.register_fork_child(node, "fork answerer", &prompt, site_ty.as_deref())?;
        if let Err(e) = self.force(child, actor) {
            self.cleanup_failed_child(child);
            return Err(e);
        }

        // Drive the child's turn loop until it emits an answering block, then run
        // that block via run_child against the SUSPENDED PARENT (not the child's
        // own session) to produce a Value in the parent's heap. On ANY failure
        // (provider/join/log fault, or cap-exhaustion abort) clean up the child
        // before propagating — no orphaned Running+resident node.
        let answer_value = match self
            .drive_answerer_to_value(
                child,
                node,
                site_ty.as_deref(),
                self.cfg.max_turns,
                &self.child_cfg,
            )
            .await
        {
            Ok(v) => v,
            Err(e) => {
                self.cleanup_failed_child(child);
                return Err(e);
            }
        };

        // Resume the parent with the child's typed answer.
        if let Err(e) = self.resume_parent(node, &pending.hole, answer_value).await {
            self.cleanup_failed_child(child);
            return Err(e);
        }
        // The child answerer node is done once it has produced the answer.
        let _ = self.tree.node_done(child, "answer delivered".to_string());
        self.drop_session(child);
        Ok(child)
    }

    /// Force + drive a FANOUT answerer set for `node`'s pending fanout hole
    /// (`forkAll @T` / `runLLMTurnFanout @T`, `HoleRouting::Fork` with
    /// `fan: Some(_)`). One park, N thunk children — each registered under
    /// `node` (transcript forked at the checkpoint, same discipline as
    /// [`Self::answer_fork`]), forced, and driven to an answering value IN
    /// DECLARATION ORDER: children serialize against the parked parent's single
    /// heap (`run_child` only ever touches one machine at a time), so this needs
    /// no new `Slot` state beyond what a plain fork already uses. Each child gets
    /// its own turn-cap budget (`cfg.max_child_turns`) rather than the whole-node
    /// cap. The N raw per-child `T` values are assembled into a genuine `[T]`
    /// `Value` (the same raw-representation discipline a single fork's
    /// `unsafeCoerce` relies on) and resume the parent exactly once.
    ///
    /// A child that exhausts its `max_child_turns` budget does NOT hard-fail
    /// straight out of this loop anymore: [`Self::drive_answerer_to_value`]
    /// runs the escalation ladder internally (auto corrective-retry, then an
    /// operator popup) before ever returning an error. If a child is still
    /// stuck after the operator aborts, that child is CANCELLED inside
    /// `drive_answerer_to_value` (never left `Running`) before the error
    /// propagates here via `?` — this function does nothing further: earlier
    /// children in the loop are already terminal (`Done`), later ones were
    /// never created, and `node` (the parent) is simply never resumed, so it
    /// stays `Suspended` on its original fanout hole, re-answerable.
    pub async fn answer_fanout(
        &self,
        node: NodeId,
        actor: Actor,
    ) -> Result<Vec<NodeId>, HarnessError> {
        let pending = self
            .convos
            .lock()
            .get(&node)
            .and_then(|c| c.pending.clone())
            .ok_or(HarnessError::NotSuspended(node))?;
        let (list_ty, fan, prompts) = match &pending.classified.routing {
            HoleRouting::Fork {
                ty,
                fan: Some(fan),
                prompts,
                ..
            } => (ty.clone(), *fan, prompts.clone()),
            other => {
                return Err(HarnessError::RoutingMismatch {
                    node,
                    routing: "fanout",
                    actual: format!("{other:?}"),
                })
            }
        };
        let element_ty = list_ty.as_deref().and_then(engine::strip_list_type);

        // Cardinality integrity: the Haskell side's `fan` is the ONE
        // authoritative child count (it is `length prompts` at the
        // `runLLMTurnFanoutSited` call, before serialization). Every prompt
        // element must have decoded to a `Text` brief — `classify_hole`'s
        // `filter_map(as_str)` SILENTLY DROPS a non-string element, so a
        // shorter `prompts` than `fan` means one was lost. Answering anyway
        // would resume the parent with a `[T]` shorter than its `forkAll`
        // promised (a length the type system already committed to). Fail loud
        // instead of under-answering. (An empty `fan == prompts == 0` is
        // legitimate — `forkAll [] :: M [T]` resumes with `[]` — so it passes.)
        if let FanBadge::Exact { n } = fan {
            if n as usize != prompts.len() {
                return Err(HarnessError::Resident(format!(
                    "fanout cardinality mismatch on {node:?}: fan={n} but {} prompt(s) \
                     decoded — a non-Text prompt element was dropped, or the fan/prompts \
                     wire fields disagree",
                    prompts.len()
                )));
            }
        }

        let mut children = Vec::with_capacity(prompts.len());
        let mut answers = Vec::with_capacity(prompts.len());
        for (idx, prompt) in prompts.iter().enumerate() {
            let child = self.register_fork_child(
                node,
                &format!("fanout answerer {idx}"),
                prompt,
                element_ty,
            )?;
            // Guard every child exit: a force/drive fault must not orphan this
            // child (earlier children are already Done+dropped; later ones are
            // never created — only the in-flight one can leak).
            if let Err(e) = self.force(child, actor) {
                self.cleanup_failed_child(child);
                return Err(e);
            }
            let value = match self
                .drive_answerer_to_value(
                    child,
                    node,
                    element_ty,
                    self.cfg.max_child_turns,
                    &self.child_cfg,
                )
                .await
            {
                Ok(v) => v,
                Err(e) => {
                    self.cleanup_failed_child(child);
                    return Err(e);
                }
            };
            let _ = self.tree.node_done(child, "answer delivered".to_string());
            self.drop_session(child);
            children.push(child);
            answers.push(value);
        }

        let table = self
            .convos
            .lock()
            .get(&node)
            .and_then(|c| c.suspend_table.clone())
            .unwrap_or_default();
        let list_value = engine::build_list_value(answers, &table)?;
        self.resume_parent(node, &pending.hole, list_value).await?;
        Ok(children)
    }

    /// Answer an in-context `runLLMTurn` hole: the SAME node's model writes
    /// `resume expr`, which runs via `run_child` against the (suspended) node's
    /// own session to produce the Value, then resumes it. No child node.
    pub async fn answer_run_llm_turn(&self, node: NodeId) -> Result<(), HarnessError> {
        let pending = self
            .convos
            .lock()
            .get(&node)
            .and_then(|c| c.pending.clone())
            .ok_or(HarnessError::NotSuspended(node))?;
        let ty = match &pending.classified.routing {
            HoleRouting::RunLLMTurn { ty, .. } => ty.clone(),
            other => {
                return Err(HarnessError::RoutingMismatch {
                    node,
                    routing: "run_llm_turn",
                    actual: format!("{other:?}"),
                })
            }
        };
        // Push the hole card as a user turn, then drive the node's own loop to an
        // answering value against itself.
        self.push_user_turn(
            node,
            &engine::hole_card(&pending.classified.prompt, ty.as_deref()),
        )?;
        let value = self
            .drive_answerer_to_value(node, node, ty.as_deref(), self.cfg.max_turns, &self.cfg)
            .await?;
        self.resume_parent(node, &pending.hole, value).await?;
        Ok(())
    }

    /// Answer a `runLLMTurn`/`runLLMTurnFork` hole MECHANICALLY (§6 D6)
    /// from its server-derived form ([`Self::pending_derived_ui`]): ZERO model
    /// turns. `submission` is F1's `{values, prose}` answer encoding; a
    /// non-empty `prose` is NOT mechanical (that's the elaboration path's
    /// job, unbuilt) — rejected here rather than guessed at. On the
    /// mechanical path, `values` must map via
    /// [`crate::uiof::resume_expr_from_submission`] against the hole's
    /// derived `Ui`; the result is compiled as `resume <expr>` (specialized
    /// to the hole's answer type, same as the model-driven answerer) and run
    /// via `run_child` against `node`'s own suspended session, then resumes
    /// it — the same discipline [`Self::answer_run_llm_turn`] uses, minus
    /// the model loop. Any failure to derive a form or map the submission is
    /// [`HarnessError::NoDerivedForm`] — the caller falls back to
    /// [`Self::answer_run_llm_turn`]/[`Self::answer_fork`].
    pub async fn answer_mechanical(
        &self,
        node: NodeId,
        submission: Json,
    ) -> Result<(), HarnessError> {
        let pending = self
            .convos
            .lock()
            .get(&node)
            .and_then(|c| c.pending.clone())
            .ok_or(HarnessError::NotSuspended(node))?;
        let ty = match &pending.classified.routing {
            HoleRouting::RunLLMTurn { ty: Some(ty), .. }
            | HoleRouting::Fork { ty: Some(ty), .. } => ty.clone(),
            other => {
                return Err(HarnessError::RoutingMismatch {
                    node,
                    routing: "mechanical",
                    actual: format!("{other:?}"),
                })
            }
        };
        let prose_is_empty = submission
            .get("prose")
            .and_then(Json::as_str)
            .is_none_or(str::is_empty);
        if !prose_is_empty {
            return Err(HarnessError::NoDerivedForm(node));
        }
        let values = submission
            .get("values")
            .and_then(Json::as_object)
            .ok_or(HarnessError::NoDerivedForm(node))?;

        let table = self
            .convos
            .lock()
            .get(&node)
            .and_then(|c| c.suspend_table.clone())
            .ok_or(HarnessError::NoDerivedForm(node))?;
        let ui = crate::uiof::ui_of(&table, &ty).ok_or(HarnessError::NoDerivedForm(node))?;
        let expr = crate::uiof::resume_expr_from_submission(&ui, values)
            .ok_or(HarnessError::NoDerivedForm(node))?;

        // Compile `resume <expr>` as an answerer turn, specialized to the
        // hole's answer type exactly like the model-driven answerer's helper
        // (`drive_answerer_to_value`) — a mismatched mechanical mapping would
        // fail here with a GHC error, same retry-worthy shape, though the
        // mechanical mapping is constructed to already match the type.
        let helpers = format!("resume :: {ty} -> M {ty}\nresume = pure");
        let imports = crate::uiof::defining_module(&table, &ty).unwrap_or_default();
        let src =
            engine::template_answer_turn(&self.cfg, &format!("resume {expr}"), &imports, &helpers);
        let cfg_bin = self.cfg.extract_bin.clone();
        let include = self.cfg.include.clone();
        let node_id = node.0;
        let compiled = tokio::task::spawn_blocking(move || {
            compile::compile_turn(
                &cfg_bin,
                &src,
                "result",
                &include,
                node_id,
                timing::NO_ROUND,
            )
        })
        .await
        .map_err(|e| HarnessError::Resident(format!("compile join: {e}")))?
        .map_err(|e| HarnessError::Compile(e.to_string()))?;

        let mut session = self.take_session(node)?;
        let cexpr = compiled.expr;
        let ctable = compiled.table.clone();
        let (session, out) = tokio::task::spawn_blocking(move || {
            let out = session.run_child(
                "mechanical",
                &cexpr,
                &ctable,
                &tidepool_codegen::emit::ExternalEnv::new(),
            );
            (session, out)
        })
        .await
        .map_err(|e| HarnessError::Resident(format!("run_child join: {e}")))?;
        self.put_session(node, session, None, AsksSidecar::default());
        self.flush_effects(node);

        let value = out
            .map_err(|e| HarnessError::Resident(e.to_string()))?
            .into_value();
        self.resume_parent(node, &pending.hole, value).await
    }

    /// Answer an operator `dialogAsk` (or plain `ask`) hole with a form
    /// submission `{values, prose}`. `dialogAsk :: Ui -> M Value` returns the
    /// submission DIRECTLY as its value — the program that called `dialogAsk`
    /// decides what it means — so the submission JSON always becomes the resume
    /// Value with zero model turns. (Typed structure is the caller's job, via
    /// `Tidepool.Form` / `dialogForm`, not a harness-side interpretation step.)
    pub async fn answer_dialog(&self, node: NodeId, submission: Json) -> Result<(), HarnessError> {
        let pending = self
            .convos
            .lock()
            .get(&node)
            .and_then(|c| c.pending.clone())
            .ok_or(HarnessError::NotSuspended(node))?;
        match &pending.classified.routing {
            HoleRouting::Dialog { .. } | HoleRouting::Ask { .. } | HoleRouting::AskUser { .. } => {}
            other => {
                return Err(HarnessError::RoutingMismatch {
                    node,
                    routing: "dialog",
                    actual: format!("{other:?}"),
                })
            }
        }

        // The suspend table is the constructor set the hole suspended with; the
        // submission Value bridges against it. `dialogAsk` returns a Value, so
        // the submission JSON IS the resume answer — always, no interpretation.
        let table = self
            .convos
            .lock()
            .get(&node)
            .and_then(|c| c.suspend_table.clone())
            .unwrap_or_default();
        let value = engine::json_answer_to_value(&submission, &table)?;
        // `resume_parent` logs the Consumed attempt itself, once, only after
        // the resume actually succeeds — logging it here too used to
        // double-log every mechanical dialog answer (fixed: single source of
        // truth for the Consumed record).
        self.resume_parent(node, &pending.hole, value).await?;
        Ok(())
    }

    /// Evaluate `expr` (a plain `M a` expression — no `resume`/hole semantics)
    /// against `node`'s currently SUSPENDED session heap: a non-consuming
    /// heap-browser peek (widen C4 / PRD D4). The pending hole, the tree
    /// state, and the event log are all untouched — `run_child` nests the
    /// eval against the suspended machine and restores it afterward, the same
    /// discipline [`Self::drive_answerer_to_value`] uses for a real answer,
    /// minus the model loop and the `resume`. `name` labels the compiled
    /// fragment (surfaces in JIT diagnostics); it is NOT persisted as a
    /// session binding — each call is independent, same as `run_child`
    /// elsewhere in this file. Requires `node` to be suspended (the
    /// precondition `run_child` itself enforces).
    pub async fn eval_in_binding(
        &self,
        node: NodeId,
        name: &str,
        expr: &str,
    ) -> Result<String, HarnessError> {
        let (imports, body) = engine::split_imports(expr);
        let src = engine::template_answer_turn(&self.cfg, &body, &imports, "");
        let cfg_bin = self.cfg.extract_bin.clone();
        let include = self.cfg.include.clone();
        let node_id = node.0;
        let compiled = tokio::task::spawn_blocking(move || {
            compile::compile_turn(
                &cfg_bin,
                &src,
                "result",
                &include,
                node_id,
                timing::NO_ROUND,
            )
        })
        .await
        .map_err(|e| HarnessError::Resident(format!("compile join: {e}")))?
        .map_err(|e| HarnessError::Compile(e.to_string()))?;

        let mut session = self.take_session(node)?;
        let cexpr = compiled.expr;
        let ctable = compiled.table.clone();
        let label = name.to_string();
        let (session, out) = tokio::task::spawn_blocking(move || {
            let out = session.run_child(
                &label,
                &cexpr,
                &ctable,
                &tidepool_codegen::emit::ExternalEnv::new(),
            );
            (session, out)
        })
        .await
        .map_err(|e| HarnessError::Resident(format!("run_child join: {e}")))?;
        self.put_session(node, session, None, AsksSidecar::default());
        self.flush_effects(node);

        out.map(|r| r.to_string_pretty())
            .map_err(|e| HarnessError::Resident(e.to_string()))
    }

    /// Drive `answerer`'s turn loop until it emits an answering block, then run
    /// that block via `run_child` against `target`'s suspended session to
    /// produce a Value. On a compile failure (the GHC-verbatim retry), feed the
    /// error back as the answerer's next user turn and loop (bounded by
    /// `max_turns` — a single answerer's cap for a plain fork/return-control
    /// answer, or one fanout child's per-child cap, `cfg.max_child_turns`, so
    /// no single child of a fan can consume the whole node's turn budget).
    /// `ty` is threaded into the answerer's `resume :: ty -> M ty` helper.
    ///
    /// CAP EXHAUSTION does NOT return straight out anymore (the old
    /// mid-fan hard-failure that leaked a `Running` answerer and wedged the
    /// parent) — it runs the escalation ladder via
    /// [`Self::handle_cap_exhaustion`]: an auto corrective-retry first
    /// (rung 1), then an operator popup (rung 2). Only an operator ABORT (or
    /// an unrelated resident/routing error) unwinds out of this loop; on
    /// abort, `answerer` is cancelled here (never left dangling) and
    /// [`HarnessError::Aborted`] is returned, naming `answerer` and why.
    /// `compile_cfg` is the row the answerer's block compiles against: the
    /// parent's own [`Self::cfg`] for an in-context answer (same node answers
    /// itself), or the fork-child [`Self::child_cfg`] for a forked child — so a
    /// child cannot name `fork`/`forkAll`. The answer VALUE always crosses via
    /// `run_child` against `target`'s session regardless (a pure `resume expr`).
    async fn drive_answerer_to_value(
        &self,
        answerer: NodeId,
        target: NodeId,
        ty: Option<&str>,
        max_turns: u32,
        compile_cfg: &EngineConfig,
    ) -> Result<Value, HarnessError> {
        let mut attempts = 0;
        let mut turn_budget = max_turns;
        let mut auto_retries_used = 0u32;
        loop {
            if attempts >= turn_budget {
                match self
                    .handle_cap_exhaustion(answerer, ty, attempts, &mut auto_retries_used)
                    .await?
                {
                    CapDecision::Retry {
                        turn_budget: new_budget,
                    } => {
                        turn_budget = new_budget;
                        continue;
                    }
                    CapDecision::Abort { reason } => {
                        self.tree.node_cancelled(answerer, reason.clone())?;
                        self.drop_session(answerer);
                        return Err(HarnessError::Aborted {
                            node: answerer,
                            reason,
                        });
                    }
                }
            }
            attempts += 1;

            // One provider turn on the answerer.
            let (transcript, turn_seq, framing) = {
                let convos = self.convos.lock();
                let convo = convos
                    .get(&answerer)
                    .ok_or(HarnessError::NoSession(answerer))?;
                (
                    convo.transcript.clone(),
                    convo.turn_seq,
                    convo.framing.clone(),
                )
            };
            let driven = self
                .stream_turn(answerer, &transcript, framing.as_deref())
                .await?;
            self.tree.turn_delta_reasoned(
                answerer,
                turn_seq,
                Role::Assistant,
                driven.reply.clone(),
                Some(driven.usage),
                driven.reasoning.clone(),
            )?;
            self.finish_live_turn(answerer);
            {
                let mut convos = self.convos.lock();
                let convo = convos
                    .get_mut(&answerer)
                    .ok_or(HarnessError::NoSession(answerer))?;
                convo.transcript.push(Message {
                    role: Role::Assistant,
                    content: driven.reply.clone(),
                });
                convo.turn_seq += 1;
            }

            let Some(block) = driven.block else {
                self.push_user_turn(
                    answerer,
                    "Reply with a single ```haskell block: `resume expr` where the value \
                     matches the hole type.",
                )?;
                continue;
            };

            // Compile the answering block. When the hole's answer type is
            // known, specialize `resume :: T -> M T` so a mismatched `resume
            // expr` fails at extract with a GHC error naming T (the golden
            // path's deliberate ill-typed attempt lands here) — otherwise fall
            // back to the polymorphic identity.
            let helpers = match ty {
                Some(t) => format!("resume :: {t} -> M {t}\nresume = pure"),
                None => RESUME_HELPER.to_string(),
            };
            let (imports, body) = engine::split_imports(&block);
            let src = engine::template_answer_turn(compile_cfg, &body, &imports, &helpers);
            let cfg_bin = compile_cfg.extract_bin.clone();
            let include = compile_cfg.include.clone();
            let answerer_id = answerer.0;
            let compiled = tokio::task::spawn_blocking(move || {
                compile::compile_turn(
                    &cfg_bin,
                    &src,
                    "result",
                    &include,
                    answerer_id,
                    timing::NO_ROUND,
                )
            })
            .await
            .map_err(|e| HarnessError::Resident(format!("compile join: {e}")))?;

            let compiled = match compiled {
                Ok(c) => c,
                Err(e) => {
                    // GHC-verbatim retry: feed the compile error back to the
                    // answerer. The continuation is NEVER consumed by a bad
                    // attempt.
                    let err = e.to_string();
                    self.log_answer_attempt(
                        target,
                        &self
                            .convos
                            .lock()
                            .get(&target)
                            .and_then(|c| c.pending.as_ref().map(|p| p.hole.clone()))
                            .unwrap_or(HoleId(String::new())),
                        "child",
                        AnswerOutcome::Rejected { error: err.clone() },
                    )?;
                    self.push_user_turn(
                        answerer,
                        &format!(
                            "That did not compile. Fix it and try again — the error is:\n\n\
                             ```\n{err}\n```"
                        ),
                    )?;
                    continue;
                }
            };

            // Run the answering block via run_child against the TARGET's
            // suspended session (same heap → the Value can feed resume).
            let mut session = self.take_session(target)?;
            let expr = compiled.expr;
            let ctable = compiled.table.clone();
            let (session, child_out) = tokio::task::spawn_blocking(move || {
                let out = session.run_child(
                    "answerer",
                    &expr,
                    &ctable,
                    &tidepool_codegen::emit::ExternalEnv::new(),
                );
                (session, out)
            })
            .await
            .map_err(|e| HarnessError::Resident(format!("run_child join: {e}")))?;
            self.put_session(target, session, None, AsksSidecar::default());
            self.flush_effects(target);

            match child_out {
                Ok(result) => {
                    if std::env::var("HARNESS_DEBUG").is_ok() {
                        eprintln!("[harness] child answer value: {:?}", result.value());
                    }
                    return Ok(result.into_value());
                }
                Err(ResidentError::NotSuspended) => return Err(HarnessError::NotSuspended(target)),
                // v1 limitation: a forked CHILD that itself suspends (nested
                // `fork`/`forkAll`, or `dialogAsk`/`dialogForm`) is unsupported.
                // The GUI/self-harness driver monitors ONE session (the
                // parent's), so there is no operator to answer a hole opened
                // two levels deep — and `run_child` itself only ever holds ONE
                // stowed continuation (R0 sequential-isolated), so the child
                // literally cannot park here. Cancel the child and hard-error
                // rather than falling into the generic retry arm below (which
                // would blind-retry forever: the child's own suspend is not a
                // transient compile/runtime fault it can self-correct from).
                Err(ResidentError::ChildSuspended) => {
                    self.tree.node_cancelled(
                        answerer,
                        "nested fork/askUser in a fork child unsupported (v1)".to_string(),
                    )?;
                    self.drop_session(answerer);
                    return Err(HarnessError::Aborted {
                        node: answerer,
                        reason: "a forked child answerer suspended on its own effect — nested \
                                 fork or askUser inside a fork child is unsupported in v1"
                            .to_string(),
                    });
                }
                Err(e) => {
                    // A run-time fault in the answerer (e.g. `error "boom"`
                    // forced during its own eval, before the parent's
                    // continuation is ever touched) — logged as a Rejected
                    // attempt, same as the compile-failure branch above, so
                    // the durable audit trail shows every attempt, not just
                    // the one that eventually consumes. The continuation is
                    // NEVER consumed by this attempt; retry with the message.
                    let err = e.to_string();
                    self.log_answer_attempt(
                        target,
                        &self
                            .convos
                            .lock()
                            .get(&target)
                            .and_then(|c| c.pending.as_ref().map(|p| p.hole.clone()))
                            .unwrap_or(HoleId(String::new())),
                        "child",
                        AnswerOutcome::Rejected { error: err.clone() },
                    )?;
                    self.push_user_turn(
                        answerer,
                        &format!("The answer failed at runtime: {err}. Try again."),
                    )?;
                    continue;
                }
            }
        }
    }

    /// The escalation ladder's ONE rung-1 auto-retry: a fixed extra turn
    /// budget granted exactly once per [`Self::drive_answerer_to_value`] call
    /// before further exhaustion escalates to rung 2. Kept small and
    /// singular deliberately — this is meant to unwedge the common case (the
    /// model just needed one more nudge), not to substitute for the
    /// operator.
    const AUTO_RETRY_MAX: u32 = 1;
    /// The turn-budget bump rung 1's single auto-retry grants.
    const AUTO_RETRY_BUMP: u32 = 3;

    /// [`Self::drive_answerer_to_value`]'s ladder step, called when
    /// `answerer` has exhausted its current turn budget (`attempts >=
    /// turn_budget`) without producing a consumed answer — whether from
    /// repeated `NoBlock` replies, repeated ill-typed `resume` attempts, or a
    /// mix (both retry paths consume from the same `attempts` counter, so
    /// either exhausts the same way). RUNG 1 fires at most once per answerer
    /// per call (`auto_retries_used` is the caller's counter, threaded
    /// through so a SECOND exhaustion after an operator-granted budget goes
    /// straight back to rung 2 rather than re-trying rung 1): it injects a
    /// corrective user turn — reusing the same feed-the-error-back-verbatim
    /// idiom the compile-failure retry above uses, just with a different
    /// message — and grants [`Self::AUTO_RETRY_BUMP`] more turns. Once rung 1
    /// is spent, this escalates to [`Self::escalate_to_operator`] (rung 2).
    async fn handle_cap_exhaustion(
        &self,
        answerer: NodeId,
        ty: Option<&str>,
        attempts: u32,
        auto_retries_used: &mut u32,
    ) -> Result<CapDecision, HarnessError> {
        if *auto_retries_used < Self::AUTO_RETRY_MAX {
            *auto_retries_used += 1;
            let ty_clause = ty.map(|t| format!(" of type `{t}`")).unwrap_or_default();
            self.push_user_turn(
                answerer,
                &format!(
                    "You have exhausted your turn budget ({attempts} turns) without \
                     producing a single valid `resume expr`{ty_clause}. You have \
                     {bump} more turns — produce a single valid `resume expr` now.",
                    bump = Self::AUTO_RETRY_BUMP
                ),
            )?;
            return Ok(CapDecision::Retry {
                turn_budget: attempts + Self::AUTO_RETRY_BUMP,
            });
        }
        self.escalate_to_operator(answerer, attempts).await
    }

    /// Rung 2 of the escalation ladder: park `answerer` awaiting an operator
    /// decision. Publishes an [`Escalation`] (what the stuck-node popup
    /// renders) and a oneshot sender (fired by [`Self::resolve_escalation`],
    /// driven by the web `/steer/:node` endpoint or a test simulating the
    /// popup), THEN drops every lock before awaiting the receiver — the
    /// partial fan state a caller further up the stack (e.g.
    /// [`Self::answer_fanout`]'s `answers` vector) is holding lives on the
    /// ASYNC STACK across this await, which is fine and intended: this is an
    /// IN-PROCESS control-plane wait, not durable-across-restart mid-fan
    /// suspension (explicitly out of R0 scope) — a process death here loses
    /// the in-flight fan and the operator re-triggers, same as any other
    /// in-flight turn.
    async fn escalate_to_operator(
        &self,
        answerer: NodeId,
        attempts: u32,
    ) -> Result<CapDecision, HarnessError> {
        let (tx, rx) = oneshot::channel();
        let escalation = Escalation {
            reason: format!("cap-exhausted after {attempts} attempts"),
            transcript_preview: self.transcript_tail(answerer, 6),
        };
        self.escalations.lock().insert(answerer, escalation);
        self.operator_decisions.lock().insert(answerer, tx);

        let decision = rx.await.map_err(|_| {
            self.escalations.lock().remove(&answerer);
            HarnessError::Resident(format!(
                "node {answerer:?}: operator escalation channel dropped without a decision"
            ))
        })?;
        self.escalations.lock().remove(&answerer);

        match decision {
            OperatorDecision::AllocateMore { turns, steer } => {
                if let Some(steer) = steer.filter(|s| !s.trim().is_empty()) {
                    self.push_user_turn(answerer, &steer)?;
                }
                Ok(CapDecision::Retry {
                    turn_budget: attempts + turns,
                })
            }
            OperatorDecision::Abort => Ok(CapDecision::Abort {
                reason: format!("cap-exhausted after {attempts} attempts; operator aborted"),
            }),
        }
    }

    /// The last `n` transcript messages on `node`, rendered as a plain-text
    /// preview for the escalation popup. Model-authored content — the caller
    /// renders it through the same HTML-neutralizing path any other
    /// model/operator-visible text uses; this returns bare text, no markup.
    fn transcript_tail(&self, node: NodeId, n: usize) -> String {
        let convos = self.convos.lock();
        let Some(convo) = convos.get(&node) else {
            return String::new();
        };
        let start = convo.transcript.len().saturating_sub(n);
        convo.transcript[start..]
            .iter()
            .map(|m| format!("{:?}: {}", m.role, m.content))
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    /// Resume `node`'s parked continuation with `answer` (a Value in the node's
    /// heap). Runs the resume off-reactor; on completion marks the node done, on
    /// a re-suspend re-publishes the new hole.
    async fn resume_parent(
        &self,
        node: NodeId,
        hole: &HoleId,
        answer: Value,
    ) -> Result<(), HarnessError> {
        // `Session::resume` continues the ALREADY-COMPILED fragment the node
        // suspended with (it does not recompile), so any hole reached further
        // down that same continuation — including a second sequential ask —
        // resolves its site-id -> type against this SAME table, exactly like
        // `run_block` resolves the first hole's. Snapshot it now (session is
        // about to be taken out) so the re-suspend arm below can classify the
        // next hole with real site/type instead of publishing `None`/`None`.
        let (table, asks, pending_bind) = {
            let convos = self.convos.lock();
            let c = convos.get(&node);
            (
                c.and_then(|c| c.suspend_table.clone()).unwrap_or_default(),
                c.map(|c| c.suspend_asks.clone()).unwrap_or_default(),
                c.and_then(|c| c.pending_bind.clone()),
            )
        };

        let mut session = self.take_session(node)?;
        let hole_str = hole.0.clone();
        // A suspended value-plane bind resumes via `resume_bind` (which
        // materializes the binding on completion); a plain hole via `resume`.
        let (session, outcome) = tokio::task::spawn_blocking(move || {
            let out = match &pending_bind {
                Some((binder, gen)) => session.resume_bind(&hole_str, answer, binder, *gen),
                None => session.resume(&hole_str, answer),
            };
            (session, out)
        })
        .await
        .map_err(|e| HarnessError::Resident(format!("resume join: {e}")))?;
        // Refresh suspend_table/suspend_asks explicitly on every restore, not
        // just the first suspend — downstream lookups (`pending_derived_ui`,
        // mechanical/dialog answers) must read the table the resident session
        // is actually compiled against, not silently-preserved first-suspend
        // state.
        self.put_session(node, session, Some(table.clone()), asks.clone());
        self.flush_effects(node);

        // Only log the hole as Consumed (and clear it from `pending`) once the
        // resume has ACTUALLY SUCCEEDED — a fault here (e.g. `error` forced
        // mid-resume) must not leave a durable Consumed record with no
        // matching NodeDone or re-HolePublished.
        let outcome = outcome.map_err(|e| HarnessError::Resident(e.to_string()))?;

        self.log_answer_attempt(node, hole, "harness", AnswerOutcome::Consumed)?;
        self.tree.hole_consumed(node, hole.clone())?;
        {
            let mut convos = self.convos.lock();
            if let Some(convo) = convos.get_mut(&node) {
                convo.pending = None;
            }
        }

        match outcome {
            ResidentOutcome::Completed { result, .. } => {
                let rendered = result.to_string_pretty();
                self.tree.node_done(node, rendered)?;
                // A suspended value-plane bind materialized on this completion
                // (via `resume_bind`) — clear the stashed binder.
                {
                    let mut convos = self.convos.lock();
                    if let Some(convo) = convos.get_mut(&node) {
                        convo.pending_bind = None;
                    }
                }
                // Keep the session ALIVE past completion (don't `drop_session`),
                // same as `run_block`: a node that suspended on a hole and then
                // resumed to Done is still a followable conversation — its
                // persisted session lets `follow_up` reopen it with heap intact.
                Ok(())
            }
            ResidentOutcome::Suspended { hole, request, .. } => {
                // The resumed turn hit ANOTHER hole. Re-classify against the
                // table snapshotted above (the compile the still-executing
                // fragment was built with) and re-publish with its REAL
                // site + type, same as a first-suspend `run_block` hole.
                let classified = engine::classify_hole(&request, &table, &asks);
                let fork = matches!(classified.routing, HoleRouting::Fork { .. });
                let ty = match &classified.routing {
                    HoleRouting::Fork { ty, .. }
                    | HoleRouting::RunLLMTurn { ty, .. }
                    | HoleRouting::Finalize { ty, .. } => ty.clone(),
                    _ => None,
                };
                let site = match &classified.routing {
                    HoleRouting::Fork { site, .. }
                    | HoleRouting::RunLLMTurn { site, .. }
                    | HoleRouting::Finalize { site, .. } => Some(crate::tree::SiteId(*site)),
                    _ => None,
                };
                self.tree.hole_published(
                    node,
                    HoleId(hole.clone()),
                    site,
                    ty,
                    classified.prompt.clone(),
                    fork,
                )?;
                self.set_pending(
                    node,
                    PendingHole {
                        hole: HoleId(hole),
                        classified,
                        raw_request: request,
                    },
                );
                Ok(())
            }
        }
    }

    /// Register a fork/fanout child under `parent`, inheriting the parent's
    /// transcript prefix through the fork checkpoint plus the hole card, and
    /// the parent's framing (its system message). Emits `TurnForked`
    /// referencing the checkpoint. The child is a THUNK — the caller forces it.
    /// `title` distinguishes a plain fork's single child ("fork answerer") from
    /// one of a fanout's N children ("fanout answerer <i>").
    ///
    /// The checkpoint is the parent transcript's PREFIX LENGTH at fork time (a
    /// durable transcript position), not the assistant-only `turn_seq` counter
    /// — the child clones exactly that prefix, so the two agree by
    /// construction.
    fn register_fork_child(
        &self,
        parent: NodeId,
        title: &str,
        prompt: &str,
        ty: Option<&str>,
    ) -> Result<NodeId, HarnessError> {
        let (parent_transcript, parent_framing) = {
            let convos = self.convos.lock();
            let convo = convos.get(&parent).ok_or(HarnessError::NoSession(parent))?;
            (convo.transcript.clone(), convo.framing.clone())
        };
        let checkpoint = parent_transcript.len() as u64;
        let child = self.tree.create_node(
            Some(parent),
            title,
            self.cfg.effect_names.clone(),
            ForkShape::Exact(0),
            false,
        )?;
        self.tree.turn_forked(child, parent, checkpoint)?;

        // The child's transcript = parent prefix + the hole card as a fresh user
        // task. The fork IS the calling agent (inherits scope + framing), so the
        // parent conversation is genuine context.
        let mut transcript = parent_transcript;
        transcript.push(Message {
            role: Role::User,
            content: engine::hole_card(prompt, ty),
        });
        self.forked_transcripts
            .lock()
            .insert(child, (transcript, parent_framing));
        Ok(child)
    }

    fn log_answer_attempt(
        &self,
        node: NodeId,
        hole: &HoleId,
        source: &str,
        outcome: AnswerOutcome,
    ) -> Result<(), HarnessError> {
        self.tree
            .hole_answer_attempt(node, hole.clone(), source.to_string(), outcome)?;
        Ok(())
    }

    /// Cancel a node (operator stop / teardown).
    pub fn cancel(&self, node: NodeId, reason: &str) -> Result<(), HarnessError> {
        self.tree.node_cancelled(node, reason.to_string())?;
        self.drop_session(node);
        Ok(())
    }

    /// F2's `turn_spliced` verb, now built: interject `content` into `node`'s
    /// OWN transcript, landing at `node`'s CURRENT turn position. Appends
    /// straight to the LIVE `NodeConvo::transcript` (the same list
    /// `drive_turn`/`push_user_turn` read/append), so the very next prompt
    /// assembly on `node` sees it — no separate replay-only path, the fold
    /// crash-replay uses (`replay::apply_event`) is the SAME shape a fresh
    /// process would reconstruct from the log. Logged as `Event::TurnSpliced`
    /// (not `TurnDelta`) so a genuine operator interjection is distinguishable
    /// from a harness-generated nudge when auditing history. Requires `node`
    /// to be `Running` or `Suspended` (the same non-terminal, forced-state
    /// guard `turn_delta` uses — a splice needs a live transcript to land in).
    pub fn splice(&self, node: NodeId, content: &str) -> Result<(), HarnessError> {
        let mut convos = self.convos.lock();
        let convo = convos.get_mut(&node).ok_or(HarnessError::NoSession(node))?;
        let turn = convo.turn_seq;
        convo.transcript.push(Message {
            role: Role::User,
            content: content.to_string(),
        });
        convo.turn_seq += 1;
        drop(convos);
        self.tree
            .turn_spliced(node, turn, Role::User, content.to_string())?;
        Ok(())
    }
}

// -- session take/put/drop + pending accessors -------------------------------

impl Harness {
    /// Read a node's decl-plane context — the current `Lib.G<g>` module to import
    /// and its include directory — WITHOUT checking the session out, so a caller
    /// can build a session-aware compile before taking the session for the run.
    /// `(None, None)` when the node has no session or no accumulated decl plane.
    fn session_decl_context(&self, node: NodeId) -> (Option<String>, Option<PathBuf>) {
        let convos = self.convos.lock();
        match convos.get(&node).and_then(|c| c.session.as_ref()) {
            Some(s) => (s.session_import_module(), s.lib_include_dir()),
            None => (None, None),
        }
    }

    /// Declare what `node` must produce to resolve the hole it is now
    /// answering — see [`AnswerContract`]. The self-iterating harness driver
    /// sets this per hole, before pushing the hole card, because its per-loop
    /// answerer node is REUSED across holes whose types differ. `None` clears
    /// it (back to the polymorphic `finalize`).
    ///
    /// Takes effect from the node's next turn: every turn compiled while it is
    /// set gets the answer type in scope and `finalize` pinned to it.
    pub fn set_answer_contract(&self, node: NodeId, contract: Option<AnswerContract>) {
        let mut convos = self.convos.lock();
        if let Some(convo) = convos.get_mut(&node) {
            convo.answer_contract = contract;
        }
    }

    /// `node`'s current [`AnswerContract`], if one is set.
    fn answer_contract(&self, node: NodeId) -> Option<AnswerContract> {
        let convos = self.convos.lock();
        convos.get(&node).and_then(|c| c.answer_contract.clone())
    }

    fn take_session(&self, node: NodeId) -> Result<Session, HarnessError> {
        let mut convos = self.convos.lock();
        let convo = convos.get_mut(&node).ok_or(HarnessError::NoSession(node))?;
        convo.session.take().ok_or(HarnessError::NoSession(node))
    }

    fn put_session(
        &self,
        node: NodeId,
        session: Session,
        table: Option<DataConTable>,
        asks: AsksSidecar,
    ) {
        let mut convos = self.convos.lock();
        if let Some(convo) = convos.get_mut(&node) {
            convo.session = Some(session);
            if table.is_some() {
                convo.suspend_table = table;
                convo.suspend_asks = asks;
            }
        }
    }

    /// Drop `node`'s live convo (transcript + session). The self-iterating
    /// harness driver calls this at loop end to retire the per-loop answerer
    /// session, so the next loop gets a fresh render-seeded one (W1/C2).
    pub(crate) fn drop_session(&self, node: NodeId) {
        let mut convos = self.convos.lock();
        convos.remove(&node);
    }

    fn set_pending(&self, node: NodeId, pending: PendingHole) {
        let mut convos = self.convos.lock();
        if let Some(convo) = convos.get_mut(&node) {
            convo.pending = Some(pending);
        }
    }

    /// Replace `node`'s transcript with a single summary message IN PLACE,
    /// keeping the resident session, per-node framing (`render`'s output), and
    /// turn-sequence continuity live — the self-iterating harness's MID-LOOP
    /// in-place compaction relief (W2 / 02-runtime.md LOCKED: "replace its
    /// context with the summary so the loop CONTINUES", NO loop-abort). The
    /// accumulated exchange is collapsed to one User-role message carrying
    /// `summary` as prior-window context; the node's running [`Usage`] is reset
    /// (`node_usage` now reflects only the small compacted window, so the
    /// driver's threshold check does not immediately re-fire). The next hole
    /// (or the current hole's next round) drives on under the smaller context.
    pub(crate) fn replace_transcript_with_summary(
        &self,
        node: NodeId,
        summary: &str,
    ) -> Result<(), HarnessError> {
        let mut convos = self.convos.lock();
        let convo = convos.get_mut(&node).ok_or(HarnessError::NoSession(node))?;
        let content = format!(
            "[Prior context compacted to relieve the context window.] Summary of \
             the work you have done in this loop so far:\n\n{summary}\n\nContinue \
             from here; the detailed transcript above has been replaced by this \
             summary."
        );
        let turn = convo.turn_seq;
        convo.transcript = vec![Message {
            role: Role::User,
            content: content.clone(),
        }];
        convo.turn_seq += 1;
        // Reset the running context-size meter: the live context is now just
        // this summary, so the driver's budget check must see the small
        // compacted window, not the pre-compaction cumulative total. Both the
        // summed `usage` and the high-water `last_input_tokens` (review C-1)
        // reset — the next turn's input_tokens re-establishes the real size.
        convo.usage = Usage::default();
        convo.last_input_tokens = 0;
        drop(convos);
        self.tree
            .turn_delta(node, turn, Role::User, content, None)?;
        Ok(())
    }

    /// Append a User-role message to `node`'s transcript (and log it), without
    /// driving a turn. The self-iterating harness driver pushes each
    /// `runLLMTurn` hole card onto the SAME per-loop answerer node this way, so
    /// hole #2 sees hole #1's exchange (W1/C2: the accumulating context
    /// window). Also the corrective-retry mechanism inside
    /// [`Self::run_to_hole_or_done`].
    pub(crate) fn push_user_turn(&self, node: NodeId, content: &str) -> Result<(), HarnessError> {
        let mut convos = self.convos.lock();
        let convo = convos.get_mut(&node).ok_or(HarnessError::NoSession(node))?;
        let turn = convo.turn_seq;
        convo.transcript.push(Message {
            role: Role::User,
            content: content.to_string(),
        });
        convo.turn_seq += 1;
        drop(convos);
        self.tree
            .turn_delta(node, turn, Role::User, content.to_string(), None)?;
        Ok(())
    }
}
