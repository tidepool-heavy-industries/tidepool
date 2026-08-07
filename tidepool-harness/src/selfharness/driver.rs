//! WS-A seam: the runtime driver spine — the outer `render`/`loop`
//! alternation (01/02-runtime.md) over the OUTER Harness-monad resident
//! session, servicing each `runLLMTurn` hole by driving a NESTED
//! [`crate::harness::Harness`] (an ordinary Agent node, reusing
//! `run_to_hole_or_done` — recon: `harness.rs:634-676,975-1016`) to a
//! `finalize` (WS-B) and `run_child`-ing the result back in-heap
//! (`resident.rs:407-465`) to resume `loop`.
//!
//! ANTI-PATTERNS this seam is shaped to avoid (07-impl-orchestration.md
//! WS-A): don't reuse `tidepool-repl`'s parked-thread mechanism (dead end
//! once `fork` lands — the outer session is `Threadless`, same mechanism
//! `Harness`'s own nodes use); don't reimplement the turn loop —
//! [`SelfHarnessDriver::service_runllm_hole`] reshapes
//! `Harness::run_to_hole_or_done`, it does not duplicate it; don't copy
//! heaps — the nested Agent's `finalize` value crosses via
//! [`Harness::take_finalized_value`] (deep-forced out of its own heap, never
//! JSON) and is fed straight into [`ResidentSession::resume`] to resume the
//! OUTER session's parked `runLLMTurn` continuation.
//!
//! # Sync surface, async underneath
//!
//! [`SelfHarnessDriver::run_loop`]/[`SelfHarnessDriver::run_one_cycle`] are
//! synchronous (the frozen S3 contract), but servicing a `runLLMTurn` hole
//! drives the nested [`Harness`]'s `async` turn loop
//! ([`service_runllm_hole`](SelfHarnessDriver::service_runllm_hole)) — so
//! every entry point here must be called from a thread with an ACTIVE tokio
//! runtime (`#[tokio::main]`/`#[tokio::test(flavor = "multi_thread")]`); the
//! bridge is `tokio::task::block_in_place` + `Handle::current().block_on`,
//! which requires the multi-thread runtime flavor.

use std::path::PathBuf;
use std::sync::Arc;

use serde_json::Value as Json;
use tidepool_eval::value::Value;
use tidepool_repr::DataConTable;
use tidepool_runtime::session::ResidentOutcome;

use crate::compile::{self, CompiledTurn};
use crate::engine::{self, EngineConfig, HoleRouting, TurnOutcome};
use crate::harness::{Harness, HarnessError};
use crate::log::Actor;
use crate::provider::Usage;
use crate::selfharness::harness_source::HarnessSource;
use crate::selfharness::lifecycle::SelfHarnessState;
use crate::selfharness::observer::{Event, Observer};
use crate::selfharness::state_cross;

#[derive(Debug, thiserror::Error)]
pub enum DriverError {
    #[error("self-harness driver: {0}")]
    Session(String),
    #[error(transparent)]
    Agent(#[from] HarnessError),
}

/// One full `render` → `loop` → (service each `runLLMTurn` hole) → `render`
/// cycle's outcome — [`SelfHarnessDriver::run_one_cycle`]'s return value,
/// what a spine test asserts against. `state_json` is what the caller
/// persists and threads into the NEXT cycle's `prior_state`.
#[derive(Debug, Clone)]
pub struct CycleOutcome {
    /// `render(state, lastCompaction)`'s text BEFORE this cycle's `loop` ran
    /// — the prompt the loop's `runLLMTurn` answerer(s) implicitly worked
    /// under.
    pub prompt_before: String,
    /// `loop`'s returned `State`, serialized ([`state_cross::state_out`]).
    pub state_json: Json,
    /// `render(state, lastCompaction)`'s text AFTER this cycle's `loop`
    /// completed — proves the new `State` reaches the next render.
    pub prompt_after: String,
}

/// The outer session's own bootstrapped [`crate::harness::Session`] plus the
/// [`EngineConfig`] it was compiled against (kept alongside it — every later
/// fragment compile needs the same `extract_bin`/`include`/decls) and the
/// harness module's name (every later fragment's `qualified ... as Loaded`
/// import, see [`SelfHarnessDriver::compile_outer`]).
struct OuterSession {
    session: crate::harness::Session,
    cfg: EngineConfig,
    module_name: String,
}

/// The outer Harness-monad's OWN decl list — `Eff '[RunLLMTurn]`
/// (02-runtime.md LOCKED: no base effects for v1), distinct from the nested
/// Agent's full stack (`crate::engine`'s private `agent_decls`).
fn outer_decls() -> Vec<tidepool_mcp::EffectDecl> {
    vec![tidepool_mcp::runllmturn_decl()]
}

fn not_bootstrapped() -> DriverError {
    DriverError::Session("outer session not bootstrapped (call run_loop/run_one_cycle)".into())
}

fn turn_outcome_tag(o: &TurnOutcome) -> &'static str {
    match o {
        TurnOutcome::Completed { .. } => "Completed",
        TurnOutcome::Suspended { .. } => "Suspended",
        TurnOutcome::NoBlock { .. } => "NoBlock",
    }
}

/// The outer driver: owns the Harness-monad's own resident session
/// (`Eff '[RunLLMTurn]`, distinct from any Agent node's session) plus a
/// nested [`Harness`] used ONLY to answer `runLLMTurn` holes by driving an
/// Agent turn loop to `finalize`. One driver per running self-harness
/// process (`tidepool-selfharness`).
pub struct SelfHarnessDriver {
    /// The Harness-monad resident session. `None` before bootstrap.
    outer: Option<OuterSession>,
    /// The nested multi-node orchestrator that answers a `runLLMTurn` hole
    /// by driving an Agent turn loop (`run_to_hole_or_done`) to a
    /// `finalize`. Shared, not owned exclusively, so a future GUI/inspector
    /// can observe the same node tree.
    agent: Arc<Harness>,
    lifecycle: SelfHarnessState,
    observer: Arc<dyn Observer>,
}

impl SelfHarnessDriver {
    /// Construct a driver over an already-booted [`Harness`] (the nested
    /// orchestrator for `runLLMTurn`-answering Agent sessions) and an event
    /// [`Observer`]. The outer Harness-monad session itself is not
    /// bootstrapped until the first [`Self::run_loop`]/[`Self::run_one_cycle`]
    /// call (it needs the loaded [`HarnessSource`] first).
    pub fn new(agent: Arc<Harness>, observer: Arc<dyn Observer>) -> Self {
        SelfHarnessDriver {
            outer: None,
            agent,
            lifecycle: SelfHarnessState::Idle,
            observer,
        }
    }

    /// The driver's current lifecycle state.
    pub fn lifecycle(&self) -> &SelfHarnessState {
        &self.lifecycle
    }

    /// Bootstrap the outer `PersistentSession<Threadless>` (via
    /// [`crate::harness::Session`]) and splice `source`'s whole module body
    /// ([`HarnessSource`]) as a plain `--include`d module (NOT the session
    /// decl plane — see [`HarnessSource`]'s module doc for why: a static
    /// on-disk harness needs one stable defining module BOTH this compile
    /// and a nested Agent's answerer turn resolve identically, so
    /// author-defined types crossing between them get the same DataConId),
    /// compiled against [`outer_decls`]/[`tidepool_mcp::runllmturn_decl`] —
    /// so `Harness = M` resolves to the literal `Eff '[RunLLMTurn]` row
    /// (02-runtime.md LOCKED). No-op if already bootstrapped.
    fn bootstrap(&mut self, source: &HarnessSource) -> Result<(), DriverError> {
        if self.outer.is_some() {
            return Ok(());
        }
        let agent_cfg = self.agent.cfg();
        let mut outer_cfg = EngineConfig::from_decls(
            outer_decls(),
            agent_cfg.prelude_dir.clone(),
            agent_cfg.project_lib.clone(),
        )
        .map_err(|e| DriverError::Session(format!("outer engine config: {e}")))?;
        outer_cfg.include.push(source.source_dir.clone());

        // A trivial effectful seed carrying the RunLLMTurn-only stack's
        // ConTags (mirrors `Harness::new`'s own boot seed).
        let boot_src = engine::template_turn_for(
            &outer_decls(),
            &outer_cfg,
            "pure (toJSON (0 :: Int))",
            "",
            "",
        );
        let boot = compile::compile_turn(
            &outer_cfg.extract_bin,
            &boot_src,
            "result",
            &outer_cfg.include,
        )
        .map_err(|e| DriverError::Session(format!("outer bootstrap compile: {e}")))?;

        let handler_cfg = tidepool_handlers::HandlerConfig {
            cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            kv_path: tidepool_runtime::paths::cache_dir().join("selfharness-kv.json"),
            llm_model: std::env::var("TIDEPOOL_LLM_MODEL")
                .unwrap_or_else(|_| "gpt-4o-mini".to_string()),
        };
        // Never actually dispatched to: `outer_cfg.ask_tag == 0` means the
        // ONE declared effect (RunLLMTurn) always suspends before reaching a
        // handler. Reused verbatim from `Harness::build_stack` for the same
        // well-tested concrete stack type.
        let stack: crate::harness::BoxedStack =
            Box::new(tidepool_handlers::build_base_stack(&handler_cfg));

        let session = crate::harness::Session::bootstrap(
            &boot.expr,
            boot.table,
            stack,
            outer_cfg.ask_tag,
            outer_cfg.effect_names.clone(),
            tidepool_mcp::CapturedOutput::new(),
            outer_cfg.include.clone(),
            tidepool_runtime::DEFAULT_NURSERY_SIZE,
            None,
        )
        .map_err(|e| DriverError::Session(format!("outer bootstrap: {e}")))?;

        self.outer = Some(OuterSession {
            session,
            cfg: outer_cfg,
            module_name: source.module_name.clone(),
        });
        Ok(())
    }

    /// Compile `code` (with `helpers`) against the outer session, importing
    /// the harness module QUALIFIED as [`state_cross::LOADED_QUALIFIER`]
    /// rather than unqualified — every later fragment turn's shared compile
    /// step (`Loaded.loop ...`, `Loaded.render ...`, `Loaded.initialState`).
    /// Qualifying dodges a turn's default preamble always bringing
    /// `Tidepool.Prelude` into scope unqualified too, which exports names
    /// an authored harness routinely also defines (e.g. `render`) — see
    /// `state_cross`'s module doc for the "ambiguous occurrence" this
    /// avoids.
    fn compile_outer(&mut self, code: &str, helpers: &str) -> Result<CompiledTurn, DriverError> {
        let outer = self.outer.as_mut().ok_or_else(not_bootstrapped)?;
        let imports = format!(
            "qualified {} as {}",
            outer.module_name,
            state_cross::LOADED_QUALIFIER
        );
        let src = engine::template_turn_for(&outer_decls(), &outer.cfg, code, &imports, helpers);
        compile::compile_turn(&outer.cfg.extract_bin, &src, "result", &outer.cfg.include)
            .map_err(|e| DriverError::Session(format!("outer compile failed: {e}")))
    }

    /// `pure Loaded.initialState`'s JSON — used only for the very first
    /// cycle's `render_framing` call (no persisted `State` yet to pass it
    /// directly).
    fn initial_state_json(&mut self) -> Result<Json, DriverError> {
        let code = format!("pure {}.initialState", state_cross::LOADED_QUALIFIER);
        let compiled = self.compile_outer(&code, "")?;
        let outer = self.outer.as_mut().ok_or_else(not_bootstrapped)?;
        let outcome = outer
            .session
            .run("initial-state", &compiled.expr, &compiled.table)
            .map_err(|e| DriverError::Session(format!("initialState run failed: {e}")))?;
        match outcome {
            ResidentOutcome::Completed { result, .. } => Ok(state_cross::state_out(
                &result.into_value(),
                &compiled.table,
            )),
            ResidentOutcome::Suspended { .. } => Err(DriverError::Session(
                "initialState suspended unexpectedly — it must be a pure value".into(),
            )),
        }
    }

    /// Run ONE `render` → `loop` → (service each `runLLMTurn` hole) →
    /// `render` cycle: bootstrap the outer session if needed, render the
    /// pre-loop prompt, run `loop state` as a suspendable fragment
    /// (servicing every `runLLMTurn` hole via
    /// [`Self::service_runllm_hole`]), serialize the returned `State`
    /// ([`state_cross::state_out`]), and render the post-loop prompt.
    /// `prior_state` is `None` only for the very first cycle (mirrors
    /// `render`'s `Maybe Text` compaction argument being `Nothing`
    /// pre-history).
    pub fn run_one_cycle(
        &mut self,
        source: &HarnessSource,
        prior_state: Option<&Json>,
    ) -> Result<CycleOutcome, DriverError> {
        self.bootstrap(source)?;
        self.emit(Event::LoopBoundary);

        let initial_json;
        let pre_state: &Json = match prior_state {
            Some(j) => j,
            None => {
                initial_json = self.initial_state_json()?;
                &initial_json
            }
        };
        let prompt_before = self.render_framing(pre_state, None)?;

        self.lifecycle = SelfHarnessState::RunningLoop;
        let (value, table) = self.run_loop_fragment(prior_state)?;
        let state_json = state_cross::state_out(&value, &table);

        let prompt_after = self.render_framing(&state_json, None)?;
        self.lifecycle = SelfHarnessState::Idle;

        Ok(CycleOutcome {
            prompt_before,
            state_json,
            prompt_after,
        })
    }

    /// Bootstrap the outer session over `source`, restore the last
    /// persisted `State` if any is available (none in-process, so the very
    /// first cycle always starts from `initialState`), then run
    /// [`Self::run_one_cycle`] FOREVER, threading each cycle's `state_json`
    /// into the next. Production entry point — see the module doc for why
    /// this (and everything it calls) must run on a thread with an active
    /// multi-thread tokio runtime.
    pub fn run_loop(&mut self, source: &HarnessSource) -> Result<(), DriverError> {
        let mut state_json: Option<Json> = None;
        loop {
            let outcome = self.run_one_cycle(source, state_json.as_ref())?;
            state_json = Some(outcome.state_json);
        }
    }

    /// Drive `Loaded.loop __selfHarnessState` (spliced via
    /// [`state_cross::state_in`]) as a suspendable fragment on the outer
    /// session, servicing every `runLLMTurn` hole it suspends on via
    /// [`Self::service_runllm_hole`] until it completes. Returns the
    /// completed `State` value alongside the DataConTable its OWN compile
    /// produced (the table every hole along this same continuation
    /// classifies against — `resume` never recompiles, mirroring
    /// `Harness::resume_parent`'s snapshot-the-table discipline).
    fn run_loop_fragment(
        &mut self,
        prior_state: Option<&Json>,
    ) -> Result<(Value, DataConTable), DriverError> {
        let helpers = state_cross::state_in(prior_state);
        let code = format!("{}.loop __selfHarnessState", state_cross::LOADED_QUALIFIER);
        let compiled = self.compile_outer(&code, &helpers)?;

        let mut outcome = {
            let outer = self.outer.as_mut().ok_or_else(not_bootstrapped)?;
            outer
                .session
                .run("loop", &compiled.expr, &compiled.table)
                .map_err(|e| DriverError::Session(format!("loop run failed: {e}")))?
        };
        loop {
            match outcome {
                ResidentOutcome::Completed { result, .. } => {
                    return Ok((result.into_value(), compiled.table));
                }
                ResidentOutcome::Suspended { hole, request, .. } => {
                    let classified =
                        engine::classify_hole(&request, &compiled.table, &compiled.asks);
                    let (site, ty) = match &classified.routing {
                        HoleRouting::RunLLMTurn { site, ty } => (*site, ty.clone()),
                        other => {
                            return Err(DriverError::Session(format!(
                                "outer loop suspended on a non-RunLLMTurn hole ({other:?}) — \
                                 the Harness monad exposes runLLMTurn only"
                            )))
                        }
                    };
                    let answer =
                        self.service_runllm_hole(site, ty.as_deref(), &classified.prompt)?;
                    let outer = self.outer.as_mut().ok_or_else(not_bootstrapped)?;
                    outcome = outer
                        .session
                        .resume(&hole, answer)
                        .map_err(|e| DriverError::Session(format!("loop resume failed: {e}")))?;
                }
            }
        }
    }

    /// Service one `runLLMTurn @A` suspension (`site`/`ty` from
    /// [`crate::engine::HoleRouting::RunLLMTurn`], `prompt` the hole's
    /// human-facing text): register + force a fresh node on `self.agent`,
    /// drive it (`run_to_hole_or_done`) until it resolves via `finalize`
    /// (WS-B's effect — terminates the Agent turn loop rather than resuming
    /// it, per 03-agent-surface.md), then feed the finalized value straight
    /// into the OUTER session's `resume` to answer `loop`'s parked
    /// continuation.
    pub fn service_runllm_hole(
        &mut self,
        site: u32,
        ty: Option<&str>,
        prompt: &str,
    ) -> Result<Value, DriverError> {
        self.lifecycle = SelfHarnessState::SuspendedOnHole;
        self.emit(Event::RunLLMTurnHole {
            site,
            ty: ty.map(String::from),
        });

        let child_prompt = engine::hole_card(prompt, ty);
        let node = self
            .agent
            .create_root("runLLMTurn answerer", &child_prompt)?;
        self.agent.force(node, Actor::Operator)?;
        self.emit(Event::TurnStart { node });

        let outcome = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(self.agent.run_to_hole_or_done(node))
        })?;
        self.emit(Event::TurnEnd { node });

        let is_finalize = matches!(
            &outcome,
            TurnOutcome::Suspended { classified, .. }
                if matches!(classified.routing, HoleRouting::Finalize { .. })
        );
        if !is_finalize {
            return Err(DriverError::Session(format!(
                "runLLMTurn answerer node {node:?} did not suspend on finalize (got {})",
                turn_outcome_tag(&outcome)
            )));
        }

        let value = self.agent.take_finalized_value(node)?;
        self.emit(Event::Finalize { node });
        self.lifecycle = SelfHarnessState::RunningLoop;
        Ok(value)
    }

    /// Evaluate `render(state, lastCompaction)` against the outer session
    /// and return its `Text` result — the next loop's system prompt.
    /// Runtime-invoked at loop boundaries ONLY (02-runtime.md LOCKED).
    pub fn render_framing(
        &mut self,
        state_json: &Json,
        last_compaction: Option<&str>,
    ) -> Result<String, DriverError> {
        let state_decl = state_cross::state_in(Some(state_json));
        let compaction_decl = match last_compaction {
            None => "__selfHarnessCompaction :: Maybe Text\n__selfHarnessCompaction = Nothing"
                .to_string(),
            Some(s) => format!(
                "__selfHarnessCompaction :: Maybe Text\n__selfHarnessCompaction = Just {}",
                state_cross::haskell_string_literal(s)
            ),
        };
        let helpers = format!("{state_decl}\n{compaction_decl}\n");
        let code = format!(
            "pure ({q}.render __selfHarnessState __selfHarnessCompaction)",
            q = state_cross::LOADED_QUALIFIER
        );
        let compiled = self.compile_outer(&code, &helpers)?;
        let outer = self.outer.as_mut().ok_or_else(not_bootstrapped)?;
        let outcome = outer
            .session
            .run("render", &compiled.expr, &compiled.table)
            .map_err(|e| DriverError::Session(format!("render run failed: {e}")))?;
        match outcome {
            ResidentOutcome::Completed { result, .. } => match result.to_json() {
                Json::String(s) => Ok(s),
                other => Err(DriverError::Session(format!(
                    "render did not yield Text, got {other:?}"
                ))),
            },
            ResidentOutcome::Suspended { .. } => Err(DriverError::Session(
                "render suspended unexpectedly — render must be a pure function".into(),
            )),
        }
    }

    /// Runtime-owned emergency compaction check (02-runtime.md LOCKED: the
    /// *runtime* owns this trigger, never the loop). At `usage` past the
    /// configured threshold (~80% of `max_tokens`), force a "compact to
    /// text, target X tokens" turn and return its `Text`; `None` under
    /// threshold. The returned `Text` feeds the NEXT [`Self::render_framing`]
    /// call as `lastCompaction`. WS-E (deferred — out of this wave's scope).
    pub fn compaction_trigger(
        &mut self,
        usage: &Usage,
        max_tokens: u32,
    ) -> Result<Option<String>, DriverError> {
        let _ = (usage, max_tokens);
        unimplemented!(
            "WS-E: ~80% token-usage check against max_tokens -> forced compact-to-text turn"
        )
    }

    /// Emit `event` to the configured [`Observer`] — the ONE place the
    /// driver touches the observer, so no call site hardwires logging or a
    /// future GUI push directly (WS-H anti-pattern guard).
    fn emit(&self, event: Event) {
        self.observer.on_event(&event);
    }
}
