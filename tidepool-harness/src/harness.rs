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
use tidepool_eval::value::Value;
use tidepool_mcp::CapturedOutput;
use tidepool_repr::DataConTable;
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
}

#[derive(Clone)]
struct PendingHole {
    hole: HoleId,
    classified: ClassifiedHole,
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
        })
    }

    /// Read-only handle to the node tree (state/children/parent queries for the
    /// protocol server's tree pane).
    pub fn tree(&self) -> &NodeTree<()> {
        &self.tree
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
        self.seeds().lock().insert(node, prompt.to_string());
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

        // Seed the transcript: a fork answerer inherits its parent's transcript
        // (set by `register_fork`); a plain root gets its opening prompt.
        let mut convos = self.convos.lock();
        let transcript = self
            .forked_transcripts()
            .lock()
            .remove(&node)
            .unwrap_or_else(|| {
                let seed = self
                    .seeds()
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

    // -- lazy side maps (seed prompts, forked transcripts) --------------------
    // These carry data between create/register and force without widening the
    // NodeConvo lifecycle. OnceLock-per-field via helper accessors keeps the
    // struct lean; they're small and short-lived.
    fn seeds(&self) -> &Mutex<HashMap<NodeId, String>> {
        self.seeds_cell().get_or_init(|| Mutex::new(HashMap::new()))
    }
    fn forked_transcripts(&self) -> &Mutex<HashMap<NodeId, Vec<Message>>> {
        self.forks_cell()
            .get_or_init(|| Mutex::new(HashMap::new()))
    }
    fn seeds_cell(&self) -> &std::sync::OnceLock<Mutex<HashMap<NodeId, String>>> {
        &SEEDS
    }
    fn forks_cell(&self) -> &std::sync::OnceLock<Mutex<HashMap<NodeId, Vec<Message>>>> {
        &FORKS
    }
}

// The seed/fork side maps are per-process-simple: a single Harness per process
// in R0 (one run, one log). Thread-locals-free module statics keyed by NodeId,
// gated by the Harness's own construction. (If R0 ever runs >1 Harness in one
// process, these move into the struct — flagged in the freeze notes.)
static SEEDS: std::sync::OnceLock<Mutex<HashMap<NodeId, String>>> = std::sync::OnceLock::new();
static FORKS: std::sync::OnceLock<Mutex<HashMap<NodeId, Vec<Message>>>> =
    std::sync::OnceLock::new();

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
        self.run_block(node, &block, "", "").await
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

    /// Force + drive a FORK answerer for `node`'s pending fork hole. Registers a
    /// child node (transcript forked at the parent's current turn), forces it,
    /// drives its turn loop until it produces an answering block, runs that block
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
            HoleRouting::Fork { ty, .. } => (ty.clone(), pending.classified.prompt.clone()),
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
        let child = self.register_fork(node, &prompt, site_ty.as_deref())?;
        self.force(child, actor)?;

        // Drive the child's turn loop until it emits an answering block, then run
        // that block via run_child against the SUSPENDED PARENT (not the child's
        // own session) to produce a Value in the parent's heap.
        let answer_value = self
            .drive_answerer_to_value(child, node, site_ty.as_deref())
            .await?;

        // Resume the parent with the child's typed answer.
        self.resume_parent(node, &pending.hole, answer_value).await?;
        // The child answerer node is done once it has produced the answer.
        let _ = self.tree.node_done(child, "answer delivered".to_string());
        self.drop_session(child);
        Ok(child)
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
        let value = self.drive_answerer_to_value(node, node, ty.as_deref()).await?;
        self.resume_parent(node, &pending.hole, value).await?;
        Ok(())
    }

    /// Answer an operator `dialogAsk` (or plain `ask`) hole with a form
    /// submission `{values, prose}`. MECHANICAL-FIRST (D6): an empty-prose known
    /// option key consumes directly (the submission JSON becomes the resume
    /// Value); non-empty prose or an unknown shape would route to the model as
    /// elaborator (R0 spike: prose is passed through as the resume Value too — the
    /// elaboration path is the documented exception handler, drafted here).
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
        self.log_answer_attempt(node, &pending.hole, "operator", AnswerOutcome::Consumed)?;
        self.resume_parent(node, &pending.hole, value).await?;
        Ok(())
    }

    /// Drive `answerer`'s turn loop until it emits an answering block, then run
    /// that block via `run_child` against `target`'s suspended session to
    /// produce a Value. On a compile failure (the GHC-verbatim retry), feed the
    /// error back as the answerer's next user turn and loop (bounded by the turn
    /// cap). `ty` is threaded into the answerer's `resume :: ty -> M ty` helper.
    async fn drive_answerer_to_value(
        &self,
        answerer: NodeId,
        target: NodeId,
        ty: Option<&str>,
    ) -> Result<Value, HarnessError> {
        let mut attempts = 0;
        loop {
            if attempts >= self.cfg.max_turns {
                return Err(EngineError::NoBlock { turns: attempts }.into());
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
            let src = engine::template_turn(&self.cfg, &block, "import Tidepool.Ui", &helpers);
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
                Ok(result) => return Ok(result.into_value()),
                Err(ResidentError::NotSuspended) => {
                    return Err(HarnessError::NotSuspended(target))
                }
                Err(e) => {
                    // A run-time fault in the answerer — retry with the message.
                    self.push_user_turn(
                        answerer,
                        &format!("The answer failed at runtime: {e}. Try again."),
                    )?;
                    continue;
                }
            }
        }
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
        self.log_answer_attempt(node, hole, "harness", AnswerOutcome::Consumed)?;
        self.tree.hole_consumed(node, hole.clone())?;

        let mut session = self.take_session(node)?;
        let hole_str = hole.0.clone();
        let (session, outcome) = tokio::task::spawn_blocking(move || {
            let out = session.resume(&hole_str, answer);
            (session, out)
        })
        .await
        .map_err(|e| HarnessError::Resident(format!("resume join: {e}")))?;
        self.put_session(node, session, None, AsksSidecar::default());
        {
            let mut convos = self.convos.lock();
            if let Some(convo) = convos.get_mut(&node) {
                convo.pending = None;
            }
        }

        match outcome {
            Ok(ResidentOutcome::Completed { result, .. }) => {
                let rendered = result.to_string_pretty();
                self.tree.node_done(node, rendered)?;
                self.drop_session(node);
                Ok(())
            }
            Ok(ResidentOutcome::Suspended { hole, request, .. }) => {
                // The resumed turn hit ANOTHER hole. Re-classify + re-publish.
                let (table, asks) = {
                    let convos = self.convos.lock();
                    let c = convos.get(&node);
                    (
                        c.and_then(|c| c.suspend_table.clone()).unwrap_or_default(),
                        c.map(|c| c.suspend_asks.clone()).unwrap_or_default(),
                    )
                };
                let classified = engine::classify_hole(&request, &table, &asks);
                let fork = matches!(classified.routing, HoleRouting::Fork { .. });
                self.tree.hole_published(
                    node,
                    HoleId(hole.clone()),
                    None,
                    None,
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
            Err(e) => Err(HarnessError::Resident(e.to_string())),
        }
    }

    /// Register a fork child under `parent`, inheriting the parent's transcript
    /// up to its current turn plus the hole card. Emits `TurnForked` referencing
    /// the parent position. The child is a THUNK — the caller forces it.
    fn register_fork(
        &self,
        parent: NodeId,
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
            "fork answerer",
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
        self.forked_transcripts().lock().insert(child, transcript);
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
