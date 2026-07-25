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
use std::path::PathBuf;
use std::sync::Arc;

use parking_lot::Mutex;
use serde_json::Value as Json;
use tidepool_effect::dispatch::DispatchEffect;
use tokio::sync::oneshot;
use tidepool_eval::value::Value;
use tidepool_mcp::CapturedOutput;
use tidepool_repr::{CoreExpr, DataConTable};
use tidepool_runtime::session::{ResidentError, ResidentOutcome, ResidentSession};
use tidepool_runtime::DEFAULT_NURSERY_SIZE;

use crate::compile::{self, AsksSidecar};
use crate::engine::{
    self, ClassifiedHole, EngineConfig, EngineError, HoleRouting, RESUME_HELPER,
};
use crate::forcing::{ForkShape, NodeTree, TreeError};
use crate::log::{Actor, AnswerOutcome, LogWriter};
use crate::provider::{DynModelProvider, Message, Role};
use crate::tree::{HoleId, NodeId};

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
    #[error("node {0:?} has no pending elaborator proposal to confirm/reject")]
    NoPendingProposal(NodeId),
    #[error("node {node:?} aborted: {reason}")]
    Aborted { node: NodeId, reason: String },
    #[error("node {0:?} has no pending operator escalation to resolve")]
    NoPendingEscalation(NodeId),
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
    pending: Option<PendingHole>,
    /// Compile artifacts of the turn that suspended — the table is needed to
    /// bridge an answer Value against the same constructor set.
    suspend_table: Option<DataConTable>,
    suspend_asks: AsksSidecar,
    /// B2 elaboration: a GHC-valid `resume expr` the calling model produced
    /// for a non-mechanical dialog submission, staged for an operator
    /// confirm/reject decision — NOT yet run or consumed. `None` unless an
    /// elaboration just completed and is awaiting that decision.
    pending_proposal: Option<PendingProposal>,
}

#[derive(Clone)]
struct PendingHole {
    hole: HoleId,
    classified: ClassifiedHole,
}

/// A staged, unconsumed elaborator answer (B2). `expr`/`table` are the
/// COMPILED artifacts from the elaboration turn that produced a GHC-valid
/// `resume` expression — confirming re-runs them via `run_child` (the same
/// discipline [`Harness::answer_mechanical`] uses), so confirming never
/// re-compiles or re-asks the model. `source` is the Haskell the operator
/// sees in the inspector.
struct PendingProposal {
    hole: HoleId,
    source: String,
    expr: CoreExpr,
    table: DataConTable,
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

/// The orchestrator. Cloneable-cheap? No — it owns the tree + sessions, so it
/// is shared behind an `Arc`.
pub struct Harness {
    tree: NodeTree<()>,
    cfg: EngineConfig,
    provider: Arc<dyn DynModelProvider>,
    convos: Mutex<HashMap<NodeId, NodeConvo>>,
    /// The Haskell that seeds a fresh session's ConTags (the 10-effect stack).
    /// Compiled once, reused for every node's bootstrap.
    boot: Arc<compile::CompiledTurn>,
    /// A just-created root's opening prompt, staged between `create_root` and
    /// `force` (a thunk node has no live `NodeConvo` to hold it yet). Removed
    /// once consumed at force time.
    seeds: Mutex<HashMap<NodeId, String>>,
    /// A just-registered fork/fanout child's inherited transcript (parent
    /// prefix + hole card), staged between `register_fork_child` and `force`
    /// for the same reason as `seeds`. Removed once consumed at force time.
    forked_transcripts: Mutex<HashMap<NodeId, Vec<Message>>>,
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
        let boot_src = engine::template_turn(
            &cfg,
            "pure (toJSON (0 :: Int))",
            "",
            "",
        );
        let boot = compile::compile_turn(&cfg.extract_bin, &boot_src, "result", &cfg.include)
            .map_err(|e| HarnessError::Compile(e.to_string()))?;
        Ok(Harness {
            tree: NodeTree::new(writer),
            cfg,
            provider,
            convos: Mutex::new(HashMap::new()),
            boot: Arc::new(boot),
            seeds: Mutex::new(HashMap::new()),
            forked_transcripts: Mutex::new(HashMap::new()),
            escalations: Mutex::new(HashMap::new()),
            operator_decisions: Mutex::new(HashMap::new()),
        })
    }

    /// Read-only handle to the node tree (state/children/parent queries for the
    /// protocol server's tree pane).
    pub fn tree(&self) -> &NodeTree<()> {
        &self.tree
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
        order.into_iter().filter_map(|n| self.node_summary(n)).collect()
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
        let nodes = ids.into_iter().filter_map(|n| self.node_summary(n)).collect();
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

    /// Create a ROOT node as a thunk. `title` seeds the teaser + first user
    /// turn; `effect_row` is the branch's static effect capability.
    pub fn create_root(&self, title: &str, prompt: &str) -> Result<NodeId, HarnessError> {
        let node = self.tree.create_node(
            None,
            title,
            self.cfg.effect_names.clone(),
            ForkShape::Exact(0),
            false,
        )?;
        // Seed the (not-yet-live) transcript with the operator's opening prompt.
        // The convo entry is created lazily at force time; stash the prompt in a
        // pending seed map via the transcript on force. Here we just remember it
        // by re-deriving from the prompt at force. Keep it simple: store the seed
        // prompt keyed by node until forced.
        self.seeds.lock().insert(node, prompt.to_string());
        Ok(node)
    }

    /// Force a thunk node: emit `Forced`, bootstrap its resident session, seed
    /// its transcript with the opening prompt. Returns the node's session id.
    pub fn force(&self, node: NodeId, actor: Actor) -> Result<(), HarnessError> {
        // Bootstrap a fresh resident session for this node.
        let stack: BoxedStack = self.build_stack();
        let session = ResidentSession::bootstrap(
            &self.boot.expr,
            self.boot.table.clone(),
            stack,
            self.cfg.ask_tag,
            self.cfg.effect_names.clone(),
            CapturedOutput::new(),
            self.cfg.include.clone(),
            DEFAULT_NURSERY_SIZE,
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
        let transcript = self
            .forked_transcripts
            .lock()
            .remove(&node)
            .unwrap_or_else(|| {
                let seed = self
                    .seeds
                    .lock()
                    .remove(&node)
                    .unwrap_or_else(|| "Begin.".to_string());
                vec![Message {
                    role: Role::User,
                    content: seed,
                }]
            });
        convos.insert(
            node,
            NodeConvo {
                session: Some(session),
                transcript,
                turn_seq: 0,
                pending: None,
                suspend_table: None,
                suspend_asks: AsksSidecar::default(),
                pending_proposal: None,
            },
        );
        Ok(())
    }

    fn build_stack(&self) -> BoxedStack {
        // The concrete handler stack rooted at the process CWD (Fs/Exec/… sandbox).
        let cfg = tidepool_handlers::HandlerConfig {
            cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            kv_path: tidepool_runtime::paths::cache_dir().join("harness-kv.json"),
            llm_model: std::env::var("TIDEPOOL_LLM_MODEL")
                .unwrap_or_else(|_| "gpt-4o-mini".to_string()),
        };
        Box::new(tidepool_handlers::build_base_stack(&cfg))
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
        // Snapshot the transcript under the lock, then release before the await.
        let (transcript, turn_seq) = {
            let convos = self.convos.lock();
            let convo = convos.get(&node).ok_or(HarnessError::NoSession(node))?;
            (convo.transcript.clone(), convo.turn_seq)
        };

        self.tree
            .turn_start(node, "model".to_string(), None)?;

        let driven = engine::drive_model_turn(
            self.provider.as_ref(),
            &transcript,
            self.cfg.max_tokens,
        )
        .await?;

        // Log the assistant turn delta.
        self.tree.turn_delta(
            node,
            turn_seq,
            Role::Assistant,
            driven.reply.clone(),
            Some(driven.usage),
        )?;
        {
            let mut convos = self.convos.lock();
            let convo = convos.get_mut(&node).ok_or(HarnessError::NoSession(node))?;
            convo.transcript.push(Message {
                role: Role::Assistant,
                content: driven.reply.clone(),
            });
            convo.turn_seq += 1;
        }

        let Some(block) = driven.block else {
            return Ok(engine::TurnOutcome::NoBlock {
                reply: driven.reply,
            });
        };

        // Compile + run the block synchronously (spawn_blocking off the reactor).
        let (imports, body) = engine::split_imports(&block);
        self.run_block(node, &body, &imports, "").await
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
        let src = engine::template_turn(&self.cfg, block, imports, helpers);
        let cfg_bin = self.cfg.extract_bin.clone();
        let include = self.cfg.include.clone();

        // Compile off-reactor.
        let compiled = tokio::task::spawn_blocking(move || {
            compile::compile_turn(&cfg_bin, &src, "result", &include)
        })
        .await
        .map_err(|e| HarnessError::Resident(format!("compile task join: {e}")))?
        .map_err(|e| HarnessError::Compile(e.to_string()))?;

        // Run the fragment against the session. We must move the session out to
        // run it on the blocking pool, then move it back — the resident session
        // is `Send`. Take it out under the lock, run, restore.
        let mut session = self.take_session(node)?;
        let expr = compiled.expr;
        let table = compiled.table.clone();
        let asks = compiled.asks;

        let (session, outcome) = tokio::task::spawn_blocking(move || {
            let out = session.run(
                "turn",
                &expr,
                &table,
                &tidepool_codegen::emit::ExternalEnv::new(),
            );
            (session, out)
        })
        .await
        .map_err(|e| HarnessError::Resident(format!("run task join: {e}")))?;

        // Restore the session and record the turn's compile table.
        self.put_session(node, session, Some(compiled.table.clone()), asks.clone());

        match outcome {
            Ok(ResidentOutcome::Completed { result, .. }) => {
                let rendered = result.to_string_pretty();
                self.tree.node_done(node, rendered.clone())?;
                self.drop_session(node);
                Ok(engine::TurnOutcome::Completed { rendered })
            }
            Ok(ResidentOutcome::Suspended { hole, request, .. }) => {
                let classified = engine::classify_hole(&request, &compiled.table, &asks);
                let fork = matches!(classified.routing, HoleRouting::Fork { .. });
                let ty = match &classified.routing {
                    HoleRouting::Fork { ty, .. } | HoleRouting::ReturnControl { ty, .. } => {
                        ty.clone()
                    }
                    _ => None,
                };
                let site = match &classified.routing {
                    HoleRouting::Fork { site, .. }
                    | HoleRouting::ReturnControl { site, .. } => Some(crate::tree::SiteId(*site)),
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
                    },
                );
                Ok(engine::TurnOutcome::Suspended {
                    hole,
                    classified,
                    table: compiled.table,
                })
            }
            Err(e) => {
                // A run-time fault (not a language error — compile already
                // succeeded). Feed it back and let the caller decide; log a nudge.
                let msg = format!("The eval failed at runtime: {e}");
                self.push_user_turn(node, &msg)?;
                Err(HarnessError::Resident(e.to_string()))
            }
        }
    }

    /// Loop [`Self::drive_turn`] until the node SUSPENDS at a hole, COMPLETES,
    /// or hits the per-node turn cap. A pure-prose turn (NoBlock) feeds a nudge
    /// and loops. Returns the terminal turn outcome.
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
            match self.drive_turn(node).await? {
                out @ (engine::TurnOutcome::Completed { .. }
                | engine::TurnOutcome::Suspended { .. }) => return Ok(out),
                engine::TurnOutcome::NoBlock { .. } => {
                    self.push_user_turn(
                        node,
                        "Reply with a single ```haskell block to run (or to answer the \
                         hole with `resume expr`).",
                    )?;
                }
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

    /// The pending `Ui` value for a `dialogAsk` hole on `node`, deserialized
    /// from the routing payload — what the observatory form pane renders. `None`
    /// unless the node is suspended on a Dialog hole with a well-formed `Ui`.
    pub fn pending_dialog_ui(&self, node: NodeId) -> Option<crate::ui::Ui> {
        match self.pending_hole(node)?.routing {
            HoleRouting::Dialog { ui } => serde_json::from_value(ui).ok(),
            _ => None,
        }
    }

    /// The SERVER-DERIVED `Ui` form (§6 D6, `uiof::ui_of`) for a
    /// `returnControl`/`returnControlFork` hole on `node`, when the answer
    /// type maps mechanically — what the hole card renders IN ADDITION TO
    /// the raw Code+eval card, when `Some`. `None` when the node isn't
    /// suspended on such a hole, its answer type is unknown, or `uiof::ui_of`
    /// can't map the type (the caller falls back to the raw card as today).
    pub fn pending_derived_ui(&self, node: NodeId) -> Option<crate::ui::Ui> {
        let convos = self.convos.lock();
        let convo = convos.get(&node)?;
        let pending = convo.pending.as_ref()?;
        let ty = match &pending.classified.routing {
            HoleRouting::ReturnControl { ty: Some(ty), .. }
            | HoleRouting::Fork { ty: Some(ty), .. } => ty.clone(),
            _ => return None,
        };
        let table = convo.suspend_table.clone()?;
        drop(convos);
        crate::uiof::ui_of(&table, &ty)
    }

    /// The source of `node`'s pending elaborator proposal (B2), if one is
    /// staged awaiting an operator confirm/reject — what the inspector's
    /// proposal card renders instead of the raw form. `None` when no
    /// elaboration has produced an unconsumed proposal.
    pub fn pending_proposal_source(&self, node: NodeId) -> Option<String> {
        self.convos
            .lock()
            .get(&node)
            .and_then(|c| c.pending_proposal.as_ref())
            .map(|p| p.source.clone())
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
    pub fn resolve_escalation(&self, node: NodeId, decision: OperatorDecision) -> Result<(), HarnessError> {
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

    /// Force + drive a FORK answerer for `node`'s pending fork hole (a plain
    /// `returnControlFork`, `fan: None` — a `returnControlFanout` hole
    /// routes to [`Self::answer_fanout`] instead). Registers a child node
    /// (transcript forked at the parent's current turn), forces it, drives
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
        let child =
            self.register_fork_child(node, "fork answerer", &prompt, site_ty.as_deref())?;
        self.force(child, actor)?;

        // Drive the child's turn loop until it emits an answering block, then run
        // that block via run_child against the SUSPENDED PARENT (not the child's
        // own session) to produce a Value in the parent's heap.
        let answer_value = self
            .drive_answerer_to_value(child, node, site_ty.as_deref(), self.cfg.max_turns)
            .await?;

        // Resume the parent with the child's typed answer.
        self.resume_parent(node, &pending.hole, answer_value).await?;
        // The child answerer node is done once it has produced the answer.
        let _ = self.tree.node_done(child, "answer delivered".to_string());
        self.drop_session(child);
        Ok(child)
    }

    /// Force + drive a FANOUT answerer set for `node`'s pending fanout hole
    /// (`returnControlFanout @T`, `HoleRouting::Fork` with `fan: Some(_)`).
    /// One park, N thunk children — each registered under `node` (transcript
    /// forked at the checkpoint, same discipline as [`Self::answer_fork`]),
    /// forced, and driven to an answering value IN DECLARATION ORDER: children
    /// serialize against the parked parent's single heap (F3's
    /// sequential-isolated rule — `run_child` only ever touches one machine at
    /// a time), so this needs no new `Slot` state beyond what a plain fork
    /// already uses. Each child gets its own turn-cap budget
    /// (`cfg.max_child_turns`) rather than the whole-node cap. The N raw
    /// per-child `T` values are assembled into a genuine `[T]` `Value` (F3's
    /// RAW-value rule, same discipline a single fork's `unsafeCoerce` relies
    /// on) and resume the parent exactly once.
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
    pub async fn answer_fanout(&self, node: NodeId, actor: Actor) -> Result<Vec<NodeId>, HarnessError> {
        let pending = self
            .convos
            .lock()
            .get(&node)
            .and_then(|c| c.pending.clone())
            .ok_or(HarnessError::NotSuspended(node))?;
        let (list_ty, prompts) = match &pending.classified.routing {
            HoleRouting::Fork {
                ty,
                fan: Some(_),
                prompts,
                ..
            } => (ty.clone(), prompts.clone()),
            other => {
                return Err(HarnessError::RoutingMismatch {
                    node,
                    routing: "fanout",
                    actual: format!("{other:?}"),
                })
            }
        };
        let element_ty = list_ty.as_deref().and_then(engine::strip_list_type);

        let mut children = Vec::with_capacity(prompts.len());
        let mut answers = Vec::with_capacity(prompts.len());
        for (idx, prompt) in prompts.iter().enumerate() {
            let child = self.register_fork_child(
                node,
                &format!("fanout answerer {idx}"),
                prompt,
                element_ty,
            )?;
            self.force(child, actor)?;
            let value = self
                .drive_answerer_to_value(child, node, element_ty, self.cfg.max_child_turns)
                .await?;
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

    /// Answer an in-context `returnControl` hole: the SAME node's model writes
    /// `resume expr`, which runs via `run_child` against the (suspended) node's
    /// own session to produce the Value, then resumes it. No child node.
    pub async fn answer_return_control(&self, node: NodeId) -> Result<(), HarnessError> {
        let pending = self
            .convos
            .lock()
            .get(&node)
            .and_then(|c| c.pending.clone())
            .ok_or(HarnessError::NotSuspended(node))?;
        let ty = match &pending.classified.routing {
            HoleRouting::ReturnControl { ty, .. } => ty.clone(),
            other => {
                return Err(HarnessError::RoutingMismatch {
                    node,
                    routing: "return_control",
                    actual: format!("{other:?}"),
                })
            }
        };
        // Push the hole card as a user turn, then drive the node's own loop to an
        // answering value against itself.
        self.push_user_turn(node, &engine::hole_card(&pending.classified.prompt, ty.as_deref()))?;
        let value = self
            .drive_answerer_to_value(node, node, ty.as_deref(), self.cfg.max_turns)
            .await?;
        self.resume_parent(node, &pending.hole, value).await?;
        Ok(())
    }

    /// Answer a `returnControl`/`returnControlFork` hole MECHANICALLY (§6 D6)
    /// from its server-derived form ([`Self::pending_derived_ui`]): ZERO model
    /// turns. `submission` is F1's `{values, prose}` answer encoding; a
    /// non-empty `prose` is NOT mechanical (that's the elaboration path's
    /// job, unbuilt) — rejected here rather than guessed at. On the
    /// mechanical path, `values` must map via
    /// [`crate::uiof::resume_expr_from_submission`] against the hole's
    /// derived `Ui`; the result is compiled as `resume <expr>` (specialized
    /// to the hole's answer type, same as the model-driven answerer) and run
    /// via `run_child` against `node`'s own suspended session, then resumes
    /// it — the same discipline [`Self::answer_return_control`] uses, minus
    /// the model loop. Any failure to derive a form or map the submission is
    /// [`HarnessError::NoDerivedForm`] — the caller falls back to
    /// [`Self::answer_return_control`]/[`Self::answer_fork`].
    pub async fn answer_mechanical(&self, node: NodeId, submission: Json) -> Result<(), HarnessError> {
        let pending = self
            .convos
            .lock()
            .get(&node)
            .and_then(|c| c.pending.clone())
            .ok_or(HarnessError::NotSuspended(node))?;
        let ty = match &pending.classified.routing {
            HoleRouting::ReturnControl { ty: Some(ty), .. }
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
        let compiled = tokio::task::spawn_blocking(move || {
            compile::compile_turn(&cfg_bin, &src, "result", &include)
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

        let value = out
            .map_err(|e| HarnessError::Resident(e.to_string()))?
            .into_value();
        self.resume_parent(node, &pending.hole, value).await
    }

    /// Answer an operator `dialogAsk` (or plain `ask`) hole with a form
    /// submission `{values, prose}`. MECHANICAL-FIRST (D6): an empty-prose known
    /// option key consumes directly (the submission JSON becomes the resume
    /// Value) — ZERO model turns, UNCHANGED from the R0 spike. Non-empty prose
    /// or an unknown shape (no `values` at all) routes to [`Self::elaborate_dialog`]
    /// — the calling model interprets the submission and proposes a `resume
    /// expr`, SHOWN to the operator (not auto-consumed); see
    /// [`Self::confirm_proposal`]/[`Self::reject_proposal`].
    pub async fn answer_dialog(
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
        match &pending.classified.routing {
            HoleRouting::Dialog { .. } | HoleRouting::Ask { .. } => {}
            other => {
                return Err(HarnessError::RoutingMismatch {
                    node,
                    routing: "dialog",
                    actual: format!("{other:?}"),
                })
            }
        }

        if !Self::submission_is_mechanical(&submission) {
            return self.elaborate_dialog(node, &pending, submission).await;
        }

        // The suspend table is the constructor set the hole suspended with; the
        // submission Value bridges against it. dialogAsk returns a Value, so the
        // submission JSON IS the resume answer (mechanical: no model turn).
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

    /// D6's mechanical/exception-path split for a dialog submission (F1):
    /// empty prose AND a non-empty `values` map is the ONLY mechanical shape —
    /// non-empty prose (prose wins over a conflicting widget, per FREEZES.md
    /// F1) or an empty/absent `values` map (unknown shape: no answer the
    /// harness can bridge without interpretation) both fall to elaboration.
    fn submission_is_mechanical(submission: &Json) -> bool {
        let prose_empty = submission
            .get("prose")
            .and_then(Json::as_str)
            .is_none_or(str::is_empty);
        let has_values = submission
            .get("values")
            .and_then(Json::as_object)
            .is_some_and(|m| !m.is_empty());
        prose_empty && has_values
    }

    /// F1's exception path (B2): push the elaborator prompt (hole card + the
    /// raw submission + a `resume :: Value -> M Value` instruction) as `node`'s
    /// next user turn, drive the SAME node's own conversation (no forked
    /// child — this is operator-hole interpretation in the calling model's
    /// own context, same discipline [`Self::answer_return_control`] uses) to a
    /// GHC-valid proposal, then STAGE it rather than running/consuming it.
    async fn elaborate_dialog(
        &self,
        node: NodeId,
        pending: &PendingHole,
        submission: Json,
    ) -> Result<(), HarnessError> {
        let prompt = engine::elaborator_prompt(&pending.classified.prompt, &submission);
        self.push_user_turn(node, &prompt)?;

        let (source, expr, table) = self
            .drive_elaborator_to_proposal(node, self.cfg.max_turns)
            .await?;

        self.log_answer_attempt(
            node,
            &pending.hole,
            "operator",
            AnswerOutcome::Proposed {
                source: source.clone(),
            },
        )?;
        self.set_pending_proposal(
            node,
            PendingProposal {
                hole: pending.hole.clone(),
                source,
                expr,
                table,
            },
        );
        Ok(())
    }

    /// Drive `node`'s own turn loop until it emits a GHC-valid `resume expr ::
    /// Value` block (retrying with the verbatim GHC error on a compile
    /// failure, same discipline as [`Self::drive_answerer_to_value`]) — but,
    /// unlike that helper, NEVER runs the compiled expr; elaboration only
    /// needs it to TYPE-CHECK before showing it to the operator. Returns the
    /// proposed source text plus the compiled artifacts
    /// [`Self::run_compiled_answer`] later runs at confirm time.
    async fn drive_elaborator_to_proposal(
        &self,
        node: NodeId,
        max_turns: u32,
    ) -> Result<(String, CoreExpr, DataConTable), HarnessError> {
        let mut attempts = 0;
        loop {
            if attempts >= max_turns {
                return Err(EngineError::NoBlock { turns: attempts }.into());
            }
            attempts += 1;

            let (transcript, turn_seq) = {
                let convos = self.convos.lock();
                let convo = convos.get(&node).ok_or(HarnessError::NoSession(node))?;
                (convo.transcript.clone(), convo.turn_seq)
            };
            let driven = engine::drive_model_turn(
                self.provider.as_ref(),
                &transcript,
                self.cfg.max_tokens,
            )
            .await?;
            self.tree.turn_delta(
                node,
                turn_seq,
                Role::Assistant,
                driven.reply.clone(),
                Some(driven.usage),
            )?;
            {
                let mut convos = self.convos.lock();
                let convo = convos.get_mut(&node).ok_or(HarnessError::NoSession(node))?;
                convo.transcript.push(Message {
                    role: Role::Assistant,
                    content: driven.reply.clone(),
                });
                convo.turn_seq += 1;
            }

            let Some(block) = driven.block else {
                self.push_user_turn(
                    node,
                    "Reply with a single ```haskell block: `resume expr` where `expr` is a \
                     `Value` built via `toJSON`.",
                )?;
                continue;
            };

            let (imports, body) = engine::split_imports(&block);
            let src = engine::template_answer_turn(
                &self.cfg,
                &body,
                &imports,
                engine::DIALOG_RESUME_HELPER,
            );
            let cfg_bin = self.cfg.extract_bin.clone();
            let include = self.cfg.include.clone();
            let compiled = tokio::task::spawn_blocking(move || {
                compile::compile_turn(&cfg_bin, &src, "result", &include)
            })
            .await
            .map_err(|e| HarnessError::Resident(format!("compile join: {e}")))?;

            match compiled {
                Ok(c) => return Ok((body, c.expr, c.table)),
                Err(e) => {
                    // GHC-verbatim retry: the continuation is untouched — no
                    // proposal is ever staged from a rejected attempt.
                    let err = e.to_string();
                    let hole = self
                        .convos
                        .lock()
                        .get(&node)
                        .and_then(|c| c.pending.as_ref().map(|p| p.hole.clone()))
                        .unwrap_or(HoleId(String::new()));
                    self.log_answer_attempt(
                        node,
                        &hole,
                        "operator",
                        AnswerOutcome::Rejected { error: err.clone() },
                    )?;
                    self.push_user_turn(
                        node,
                        &format!(
                            "That did not compile. Fix it and try again — the error is:\n\n\
                             ```\n{err}\n```"
                        ),
                    )?;
                    continue;
                }
            }
        }
    }

    /// Confirm `node`'s pending elaborator proposal (B2): run the ALREADY
    /// GHC-validated compiled expr via `run_child` against `node`'s own
    /// suspended session (same discipline as [`Self::answer_mechanical`]'s
    /// tail — no re-compile, no second model turn) and resume the parent with
    /// the result. Errors with [`HarnessError::NoPendingProposal`] if nothing
    /// is staged.
    pub async fn confirm_proposal(&self, node: NodeId) -> Result<(), HarnessError> {
        let proposal = self.take_pending_proposal(node)?;
        let value = self
            .run_compiled_answer(node, proposal.expr, proposal.table)
            .await?;
        self.resume_parent(node, &proposal.hole, value).await
    }

    /// Reject `node`'s pending elaborator proposal (B2): discard it without
    /// running it. The hole was never anything but `Suspended` while the
    /// proposal was staged, so this is a pure log event — the continuation is
    /// untouched and the hole is already "reopened" (it never closed).
    pub fn reject_proposal(&self, node: NodeId) -> Result<(), HarnessError> {
        let proposal = self.take_pending_proposal(node)?;
        self.log_answer_attempt(
            node,
            &proposal.hole,
            "operator",
            AnswerOutcome::ProposalDiscarded,
        )
    }

    fn take_pending_proposal(&self, node: NodeId) -> Result<PendingProposal, HarnessError> {
        let mut convos = self.convos.lock();
        let convo = convos.get_mut(&node).ok_or(HarnessError::NoSession(node))?;
        convo
            .pending_proposal
            .take()
            .ok_or(HarnessError::NoPendingProposal(node))
    }

    fn set_pending_proposal(&self, node: NodeId, proposal: PendingProposal) {
        let mut convos = self.convos.lock();
        if let Some(convo) = convos.get_mut(&node) {
            convo.pending_proposal = Some(proposal);
        }
    }

    /// Run an already-compiled answer expression via `run_child` against
    /// `target`'s suspended session — the shared tail [`Self::confirm_proposal`]
    /// uses, factored out since it needs no drive loop (the expr already
    /// type-checked at elaboration time).
    async fn run_compiled_answer(
        &self,
        target: NodeId,
        expr: CoreExpr,
        table: DataConTable,
    ) -> Result<Value, HarnessError> {
        let mut session = self.take_session(target)?;
        let (session, out) = tokio::task::spawn_blocking(move || {
            let out = session.run_child(
                "elaborated",
                &expr,
                &table,
                &tidepool_codegen::emit::ExternalEnv::new(),
            );
            (session, out)
        })
        .await
        .map_err(|e| HarnessError::Resident(format!("run_child join: {e}")))?;
        self.put_session(target, session, None, AsksSidecar::default());
        out.map(|r| r.into_value())
            .map_err(|e| HarnessError::Resident(e.to_string()))
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
        let compiled = tokio::task::spawn_blocking(move || {
            compile::compile_turn(&cfg_bin, &src, "result", &include)
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
    async fn drive_answerer_to_value(
        &self,
        answerer: NodeId,
        target: NodeId,
        ty: Option<&str>,
        max_turns: u32,
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
            let (transcript, turn_seq) = {
                let convos = self.convos.lock();
                let convo = convos.get(&answerer).ok_or(HarnessError::NoSession(answerer))?;
                (convo.transcript.clone(), convo.turn_seq)
            };
            let driven = engine::drive_model_turn(
                self.provider.as_ref(),
                &transcript,
                self.cfg.max_tokens,
            )
            .await?;
            self.tree.turn_delta(
                answerer,
                turn_seq,
                Role::Assistant,
                driven.reply.clone(),
                Some(driven.usage),
            )?;
            {
                let mut convos = self.convos.lock();
                let convo = convos.get_mut(&answerer).ok_or(HarnessError::NoSession(answerer))?;
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
            let src = engine::template_answer_turn(&self.cfg, &body, &imports, &helpers);
            let cfg_bin = self.cfg.extract_bin.clone();
            let include = self.cfg.include.clone();
            let compiled = tokio::task::spawn_blocking(move || {
                compile::compile_turn(&cfg_bin, &src, "result", &include)
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

            match child_out {
                Ok(result) => {
                    if std::env::var("HARNESS_DEBUG").is_ok() {
                        eprintln!("[harness] child answer value: {:?}", result.value());
                    }
                    return Ok(result.into_value());
                }
                Err(ResidentError::NotSuspended) => {
                    return Err(HarnessError::NotSuspended(target))
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
            let ty_clause = ty
                .map(|t| format!(" of type `{t}`"))
                .unwrap_or_default();
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
        let (table, asks) = {
            let convos = self.convos.lock();
            let c = convos.get(&node);
            (
                c.and_then(|c| c.suspend_table.clone()).unwrap_or_default(),
                c.map(|c| c.suspend_asks.clone()).unwrap_or_default(),
            )
        };

        let mut session = self.take_session(node)?;
        let hole_str = hole.0.clone();
        let (session, outcome) = tokio::task::spawn_blocking(move || {
            let out = session.resume(&hole_str, answer);
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
                self.drop_session(node);
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
                    HoleRouting::Fork { ty, .. } | HoleRouting::ReturnControl { ty, .. } => {
                        ty.clone()
                    }
                    _ => None,
                };
                let site = match &classified.routing {
                    HoleRouting::Fork { site, .. }
                    | HoleRouting::ReturnControl { site, .. } => Some(crate::tree::SiteId(*site)),
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
                    },
                );
                Ok(())
            }
        }
    }

    /// Register a fork/fanout child under `parent`, inheriting the parent's
    /// transcript up to its current turn plus the hole card. Emits
    /// `TurnForked` referencing the parent position. The child is a THUNK —
    /// the caller forces it. `title` distinguishes a plain fork's single
    /// child ("fork answerer") from one of a fanout's N children ("fanout
    /// answerer <i>").
    fn register_fork_child(
        &self,
        parent: NodeId,
        title: &str,
        prompt: &str,
        ty: Option<&str>,
    ) -> Result<NodeId, HarnessError> {
        let (parent_transcript, parent_turn) = {
            let convos = self.convos.lock();
            let convo = convos.get(&parent).ok_or(HarnessError::NoSession(parent))?;
            (convo.transcript.clone(), convo.turn_seq.saturating_sub(1))
        };
        let child = self.tree.create_node(
            Some(parent),
            title,
            self.cfg.effect_names.clone(),
            ForkShape::Exact(0),
            false,
        )?;
        self.tree.turn_forked(child, parent, parent_turn)?;

        // The child's transcript = parent prefix + the hole card as a fresh user
        // task. The fork IS the calling agent (inherits scope), so the parent
        // conversation is genuine context.
        let mut transcript = parent_transcript;
        transcript.push(Message {
            role: Role::User,
            content: engine::hole_card(prompt, ty),
        });
        self.forked_transcripts.lock().insert(child, transcript);
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

    fn drop_session(&self, node: NodeId) {
        let mut convos = self.convos.lock();
        convos.remove(&node);
    }

    fn set_pending(&self, node: NodeId, pending: PendingHole) {
        let mut convos = self.convos.lock();
        if let Some(convo) = convos.get_mut(&node) {
            convo.pending = Some(pending);
        }
    }

    fn push_user_turn(&self, node: NodeId, content: &str) -> Result<(), HarnessError> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // -- the mechanical/elaborate routing decision (B2) ----------------------

    #[test]
    fn empty_prose_known_values_is_mechanical() {
        assert!(Harness::submission_is_mechanical(
            &json!({ "values": { "yes": true }, "prose": "" })
        ));
    }

    #[test]
    fn empty_prose_missing_prose_field_is_mechanical() {
        // The prose channel is always present per F1, but a submission
        // missing it entirely (e.g. a raw values-only client) is still
        // treated as empty prose, not a malformed shape.
        assert!(Harness::submission_is_mechanical(&json!({ "values": { "yes": true } })));
    }

    #[test]
    fn non_empty_prose_is_never_mechanical_even_with_values() {
        // Prose wins over a conflicting widget (FREEZES.md F1).
        assert!(!Harness::submission_is_mechanical(
            &json!({ "values": { "yes": true }, "prose": "actually no" })
        ));
    }

    #[test]
    fn non_empty_prose_alone_routes_to_elaboration() {
        assert!(!Harness::submission_is_mechanical(
            &json!({ "values": {}, "prose": "do the thing" })
        ));
    }

    #[test]
    fn empty_values_and_empty_prose_is_unknown_shape_routes_to_elaboration() {
        assert!(!Harness::submission_is_mechanical(&json!({ "values": {}, "prose": "" })));
        assert!(!Harness::submission_is_mechanical(&json!({})));
    }
}
