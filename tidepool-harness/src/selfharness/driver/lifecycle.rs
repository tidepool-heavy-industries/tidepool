//! The outer loop lifecycle: bootstrap, the outer session's own decl
//! row's compile targets (`OuterSession`), checkpoint restore, the
//! `render`/`loop` fragment drive (`run_loop_fragment_inner`), machine
//! rotation (`machine_maintenance`), and emergency compaction
//! (`maybe_compact_answerer`).

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::Ordering;

use serde_json::Value as Json;
use tidepool_eval::value::Value;
use tidepool_repr::DataConTable;
use tidepool_runtime::session::{ResidentHole, ResidentOutcome};

use super::contract::{outer_decls, outer_template};
use super::corrective::typed_request_agent_framing_suffix;
use super::fork::drive_concurrent;
use super::green::{
    GreenChain, GreenDelivery, GreenHoleServiced, GreenReady, GreenThread, GreenThreadState,
    ServicedSuspension,
};
use super::SelfHarnessDriver;
use super::*;
use crate::engine::{self, CompiledTurn, EngineConfig, SuspensionRouting};
use crate::log::Actor;
use crate::selfharness::harness_source::HarnessSource;
use crate::selfharness::lifecycle::SelfHarnessState;
use crate::selfharness::observer::{Event, FormSource};
use crate::selfharness::operator::{FieldShape, FormShape};
use crate::selfharness::persistence::{self};
use crate::selfharness::state_cross;
use crate::timing;

pub(crate) struct OuterSession {
    /// The SHARED session's registry id: the outer
    /// render/loop fragments AND every answerer node's turns run on this one
    /// machine — the driver holds the id, the registry holds the machine,
    /// every access goes through the checkout discipline
    /// ([`crate::harness::Harness::with_session`]).
    pub(crate) sid: tidepool_repr::SessionId,
    pub(crate) cfg: EngineConfig,
    pub(crate) module_name: String,
}

/// The [`FormShape`] [`SelfHarnessDriver::between_loops_gate`] presents
/// through the operator gate: a single-field record — ONE optional `steer`
/// text field — with the whole question ("Turn N complete — start turn
/// N+1?") carried as the ROOT shape's `doc`, same as
/// `Harness::escalate_to_operator`'s `AllocateMore`/`Abort` form carries its
/// stuck-node reason there. `iteration` is [`SelfHarnessDriver::iteration`]
/// (the count of turns already completed, restored across a restart), so the
/// title is accurate on both a live loop and a freshly restarted one.
pub(crate) fn between_loops_gate_shape(iteration: u64) -> FormShape {
    FormShape::Product {
        type_key: "BetweenTurns".to_string(),
        constructor: "BetweenTurns".to_string(),
        fields: vec![FieldShape {
            key: "steer".to_string(),
            shape: FormShape::Optional(Box::new(FormShape::String)),
            doc: Some(
                "Optional message for the next turn — leave blank to just continue.".to_string(),
            ),
        }],
        doc: Some(format!(
            "Turn {iteration} complete — start turn {next}?",
            next = iteration + 1
        )),
    }
}

/// A cycle's loop-entry decision, minted once per cycle by
/// [`SelfHarnessDriver::take_loop_entry`] and consumed by whichever
/// compilation path runs this cycle — the fused
/// [`SelfHarnessDriver::compile_loop_entry`] or the unfused
/// [`SelfHarnessDriver::run_loop_fragment_inner`]. Its whole reason to
/// exist is [`SelfHarnessDriver::resume`]'s destructive `self.resume.take()`:
/// before this type, both compile sites called `take_loop_entry` directly,
/// each independently reading `self.resume`, and the fact that only one of
/// them runs per cycle was a RUNTIME CONVENTION a reader had to trust
/// rather than something the types enforced — exactly the shape a future
/// third call site (a preparatory/fallback compile) could violate. Non-Clone:
/// at most one plan is ever live, so a resume fold cannot be injected twice
/// or consumed by the wrong compile.
pub(crate) struct LoopEntryPlan {
    code: String,
    helpers: String,
}

impl LoopEntryPlan {
    /// Consumes the plan. `code` names the loop entry (`Loaded.loop
    /// __selfHarnessState` or `Loaded.resumeLoop …`); `helpers` is the
    /// resume-fold decode splice (empty on an ordinary cycle) a caller
    /// appends alongside `state_cross::state_in`/`operator_msg_in`, which
    /// stay the CALLER's business — they are cycle-wide, not part of the
    /// resume decision this plan makes.
    pub(crate) fn into_code_and_helpers(self) -> (String, String) {
        (self.code, self.helpers)
    }
}

impl SelfHarnessDriver {
    /// Register the outer `PersistentSession` (via
    /// [`crate::harness::Session`]) and splice `source`'s whole module body
    /// ([`HarnessSource`]) as a plain `--include`d module (NOT the session
    /// decl plane — see [`HarnessSource`]'s module doc for why: a static
    /// on-disk harness needs one stable defining module BOTH this compile
    /// and a nested Agent's answerer turn resolve identically, so
    /// author-defined types crossing between them get the same DataConId),
    /// compiled against [`outer_decls`]/[`tidepool_mcp::runllmturn_decl`] —
    /// so `Harness = M` resolves to the literal `Eff '[RunLLMTurn]` row
    /// (02-runtime.md LOCKED). No-op if already bootstrapped. The outer
    /// session's machine comes up lazily, on its first real compile (the
    /// pre-loop `render`) — see [`crate::harness::ResidentSession::unbootstrapped`]
    /// — so this pays no GHC extract compile of its own.
    pub(crate) fn bootstrap(&mut self, source: &HarnessSource) -> Result<(), DriverError> {
        // BEFORE anything else, including the early return: a run whose journal
        // has entries against a harness with no `resumeLoop` is refused here,
        // so the refusal lands before a single cycle runs rather than after a
        // run has already redone finished work.
        if let Some(pending) = &self.resume {
            if !pending.fold.is_empty() && !source.declares_resume_entry {
                return Err(DriverError::ResumeEntryMissing {
                    harness: source.path.display().to_string(),
                    journal: format!(
                        "{} segment(s) under {}",
                        pending.segment_count,
                        pending.log_dir.display()
                    ),
                    run_id: pending.fold.run_id().to_string(),
                    entries: pending.fold.len(),
                });
            }
        }
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

        let session = Self::build_outer_session(&outer_cfg, Self::open_outer_plane(&outer_cfg));

        // The outer session lives in the tree's
        // registry (uniform checkout discipline, panic-safety Drop), the
        // driver holds only its id. Answerer nodes attach to it as realms.
        let sid = self.agent.adopt_session(session);
        self.outer = Some(OuterSession {
            sid,
            cfg: outer_cfg,
            module_name: source.module_name.clone(),
        });
        Ok(())
    }

    /// Construct a fresh outer-session machine handle from `cfg` — shared by
    /// [`Self::bootstrap`] and machine ROTATION ([`Self::machine_maintenance`]):
    /// one construction, so a rotated machine cannot differ from a booted one.
    /// The shared session's decl-plane root — STABLE across rotations
    /// within a process (the plane transfers), wiped at bootstrap (restart
    /// persistence of the plane is future work: the decl log has no disk
    /// reload yet, so a fresh process starts a fresh library — the legible
    /// restart-loss line covers it).
    pub(crate) fn outer_plane_root() -> PathBuf {
        tidepool_runtime::paths::cache_dir().join("selfharness/outer-plane")
    }

    /// Open the shared session's decl plane — a LIVING STRUCTURE:
    /// model-authored declarations accumulate here as SOURCE, in scope for
    /// every later answerer turn — across loops, and across machine
    /// rotations (the plane transfers; it is source-side state). Validated
    /// against [`EngineConfig::validation_include`] — the include set MINUS
    /// the per-window SHIM dir, PLUS the stable `Tidepool.Effects.Core` dir
    /// (stable-effects-core). An effectful declaration written
    /// `Member <Eff> effs => ... -> Eff effs T` validates at define time AND
    /// persists across turns/windows — Core's tycons are the same ones every
    /// later turn's compile sees, so a bound call site unifies cleanly. A
    /// declaration that instead spells the per-window `M` alias persists
    /// identically: `M` still never resolves on this plane (the shim isn't on
    /// its include path), but the plane strips the M-mentioning signature
    /// before compiling and lets GHC infer the same `Member`-polymorphic
    /// shape (`tidepool_runtime::session::render`'s `generalize_m_signatures`
    /// — M carries forward cleanly, so this is no longer a taxonomy the model
    /// needs to reason about). Only a declaration pinning a genuinely
    /// CONCRETE row still surfaces the row boundary, and only as an ordinary
    /// unsolved-`Member` error at whatever later use can't satisfy it — never
    /// a define-time refusal. The OUTER render/loop compiles never see this
    /// plane (their include never carries it): the authored harness cannot
    /// silently depend on model-authored names — unaffected by
    /// this change.
    pub(crate) fn open_outer_plane(
        cfg: &EngineConfig,
    ) -> Option<tidepool_runtime::session::SessionLib> {
        let root = Self::outer_plane_root();
        let _ = std::fs::remove_dir_all(&root);
        // The PURE-OR-STABLE-EFFECTFUL decl env, not `standalone_default`: the
        // plane validates under the same ambient pure names a turn has
        // (`Text`, `object`, the Prelude) PLUS the stable Core effect surface,
        // minus the per-window shim modules its include excludes. A minimal
        // environment is insufficient because authored declarations may use
        // ambient types such as `Text`.
        tidepool_runtime::session::SessionLib::open(
            tidepool_repr::SessionId(0),
            &root,
            tidepool_mcp::pure_decl_module_env(),
        )
        .map(|lib| lib.with_validation_include(cfg.validation_include()))
        .ok()
    }

    pub(crate) fn build_outer_session(
        cfg: &EngineConfig,
        lib: Option<tidepool_runtime::session::SessionLib>,
    ) -> crate::harness::Session {
        let handler_cfg = tidepool_handlers::HandlerConfig {
            cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            kv_path: tidepool_runtime::paths::cache_dir().join("selfharness-kv.json"),
            llm_model: std::env::var("TIDEPOOL_LLM_MODEL")
                .unwrap_or_else(|_| "gpt-4o-mini".to_string()),
        };
        // Never actually dispatched to: `cfg.suspend_tag == 0` means every
        // declared effect suspends before reaching a handler.
        let stack: crate::harness::BoxedStack =
            Box::new(tidepool_handlers::build_base_stack(&handler_cfg));
        crate::harness::Session::unbootstrapped(
            stack,
            cfg.suspend_tag,
            cfg.effect_names.clone(),
            tidepool_mcp::CapturedOutput::new(),
            cfg.include.clone(),
            tidepool_runtime::DEFAULT_NURSERY_SIZE,
            lib,
        )
    }

    /// LOOP-BOUNDARY MACHINE MAINTENANCE (bounded lifetime, not
    /// immortality): emit the machine's
    /// instrumentation ([`Event::MachineStats`] — the rotation-cadence
    /// evidence base), and at the fragment CEILING rotate: a fresh machine
    /// adopted under the SAME session id at a quiescent boundary. Durable
    /// state flows through the checkpoint exactly as every loop always has;
    /// decl-plane source (when present) is machine-independent; living
    /// session VALUES are lost — ENUMERATED into [`Event::MachineRotated`]
    /// and the next render's legible-loss note, never silently. A
    /// non-quiescent machine at the ceiling refuses the loop with a legible
    /// error instead of growing silently — the enforced bound.
    pub(crate) fn machine_maintenance(&mut self) -> Result<(), DriverError> {
        let sid = self.outer_sid()?;
        let (stats, hole_count, bindings) = self
            .agent
            .with_session(sid, |s| {
                (s.heap_stats(), s.parked_holes().len(), s.binding_names())
            })
            .map_err(|e| DriverError::Session(e.to_string()))?;
        let Some(stats) = stats else {
            // Machine not booted yet (first cycle) — nothing to measure.
            return Ok(());
        };
        self.emit(Event::MachineStats {
            fragments: stats.fragments,
            live_bytes: stats.live_bytes as u64,
            gc_count: stats.gc_count,
        });
        let ceiling = std::env::var("TIDEPOOL_MACHINE_FRAGMENT_CEILING")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(DEFAULT_FRAGMENT_CEILING);
        if stats.fragments < ceiling {
            return Ok(());
        }
        if hole_count > 0 {
            return Err(DriverError::Session(format!(
                "machine at fragment ceiling ({} >= {ceiling}) but not quiescent \
                 ({hole_count} parked hole(s)) — cannot rotate mid-suspension; raise \
                 TIDEPOOL_MACHINE_FRAGMENT_CEILING or bounce the harness",
                stats.fragments
            )));
        }
        // The decl plane is SOURCE-side state and SURVIVES rotation: take it
        // off the old machine and install it into the fresh one (living
        // structure defined by name persists; only heap VALUES die, and
        // those are the enumerated losses below).
        let lib = self
            .agent
            .with_session(sid, |s| s.take_lib())
            .map_err(|e| DriverError::Session(e.to_string()))?;
        let cfg = &self.outer.as_ref().ok_or_else(not_bootstrapped)?.cfg;
        let fresh = Self::build_outer_session(cfg, lib);
        self.agent
            .replace_session(sid, fresh)
            .map_err(|e| DriverError::Session(e.to_string()))?;
        self.emit(Event::MachineRotated {
            fragments: stats.fragments,
            bindings_lost: bindings.clone(),
        });
        self.last_rotation_losses = Some(bindings);
        Ok(())
    }

    /// The shared session's registry id, or the not-bootstrapped error.
    pub(crate) fn outer_sid(&self) -> Result<tidepool_repr::SessionId, DriverError> {
        self.outer
            .as_ref()
            .map(|o| o.sid)
            .ok_or_else(not_bootstrapped)
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
    ///
    /// `label` (`"render"`/`"loop"`) identifies this compile in the emitted
    /// [`Event::OuterCompile`] — the OUTER session has no per-node durable
    /// log of its own (`crate::log::Event::TurnStart` only ever covers a tree
    /// node's turns), so this event is the whole record of what the outer
    /// session's fragments actually were, verbatim.
    pub(crate) fn compile_outer(
        &mut self,
        code: &str,
        helpers: &str,
        label: &str,
    ) -> Result<CompiledTurn, DriverError> {
        let outer = self.outer.as_ref().ok_or_else(not_bootstrapped)?;
        let imports = format!(
            "qualified {} as {}",
            outer.module_name,
            state_cross::LOADED_QUALIFIER
        );
        let stack = outer
            .cfg
            .turn_target(None)
            .map_err(|e| DriverError::Session(format!("outer engine target: {e}")))?
            .stack;
        let extract_bin = outer.cfg.extract_bin.clone();
        let include = outer.cfg.include.clone();
        let src = outer_template(&stack, code, &imports, helpers);
        self.emit(Event::OuterCompile {
            label: label.to_string(),
            source: src.clone(),
        });
        engine::compile_turn(
            &extract_bin,
            &src,
            "result",
            &include,
            timing::NO_NODE,
            timing::NO_ROUND,
        )
        .map_err(|e| DriverError::Session(format!("outer compile failed: {e}")))
    }

    /// The `--targets` name of the fused module's extra entry — the loop
    /// body, compiled alongside `result` (the render entry) by
    /// [`Self::compile_loop_entry`] in ONE `tidepool-extract` spawn. Named in
    /// the `__selfHarness*` family like every other runtime-generated splice
    /// in this driver.
    pub(crate) const LOOP_ENTRY_TARGET: &'static str = "__selfHarnessLoopEntry";

    /// Which loop entry THIS cycle compiles, and the extra helper text it
    /// needs: `(code, extra_helpers)`.
    ///
    /// - A fresh boot, or any cycle after the first, or an EMPTY fold →
    ///   `Loaded.loop __selfHarnessState` with no extra helpers: byte for byte
    ///   the entry every harness has always compiled, which is what keeps the
    ///   twelve `loop`-only harnesses and their tests untouched.
    /// - A non-empty boot fold → `Loaded.resumeLoop __selfHarnessResume
    ///   __selfHarnessState`, with [`state_cross::resume_in`]'s decode splice
    ///   in the helpers.
    ///
    /// `take()`s the fold: the injection is ONE-SHOT at boot (see
    /// [`Self::resume`]'s doc), minted into a [`LoopEntryPlan`] a caller
    /// then consumes exactly once. [`Self::run_one_loop_iteration`] mints ONE plan
    /// per cycle and passes it down to [`Self::compile_loop_entry`] (the
    /// fused, production path); the unfused [`Self::run_loop_fragment_inner`]
    /// — a direct fragment API `run_one_loop_iteration` never itself calls, used by a
    /// test driving the fragment in isolation — mints its OWN plan instead.
    /// Either way `self.resume` is readable ONLY through this method, so a
    /// second call in the same cycle cannot get the resume fold a second
    /// time: it already saw `self.resume.take()` return `None` from the
    /// first call and mints the fold-less plan instead — a physically
    /// enforced one-shot rather than "only one caller happens to run per
    /// cycle" left to convention.
    ///
    /// A non-empty fold against a harness with no `resumeLoop` never reaches
    /// here: `bootstrap` refused it.
    pub(crate) fn take_loop_entry(&mut self) -> LoopEntryPlan {
        let q = state_cross::LOADED_QUALIFIER;
        match self.resume.take() {
            Some(pending) if !pending.fold.is_empty() => LoopEntryPlan {
                code: format!("{q}.resumeLoop __selfHarnessResume __selfHarnessState"),
                helpers: state_cross::resume_in(&pending.fold),
            },
            _ => LoopEntryPlan {
                code: format!("{q}.loop __selfHarnessState"),
                helpers: String::new(),
            },
        }
    }

    /// Compile the PRE-loop `render` and this cycle's `loop` fragment as TWO
    /// entries of ONE module, in a SINGLE `tidepool-extract` spawn
    /// ([`compile_turns`]) — the pre-model boot-path fusion this
    /// driver exists to land. Both entries splice
    /// `state_cross::state_in(prior_state)` with the SAME `prior_state`, so
    /// their helper text is byte-identical by construction — one splice, not
    /// two — and [`tidepool_mcp::TurnTemplate::extra_entries`] renders the
    /// loop entry through the exact code path `result` (the render entry)
    /// uses, so the two are identical by construction rather than a
    /// hand-copied second shape.
    ///
    /// The merged table this spawn returns is a FEATURE, not an artifact: both
    /// targets share ONE `meta.cbor` (`compile_turns`'s whole point),
    /// so the render entry's [`CompiledTurn::table`] already carries the loop
    /// entry's constructors — including the `RunLLMTurn` ConTags the machine
    /// needs once `loop` starts suspending on holes.
    ///
    /// Returns `(render_turn, loop_turn)`. Does NOT run either — that stays
    /// [`Self::render_framing`]/[`Self::run_loop_fragment_inner`]'s job, so a
    /// caller can compile once and run each entry through its own existing
    /// path.
    ///
    /// `plan` is this cycle's [`LoopEntryPlan`] — minted ONCE by the caller
    /// ([`Self::run_one_loop_iteration`]) via [`Self::take_loop_entry`] and consumed
    /// HERE, never minted by this method itself: see `LoopEntryPlan`'s doc
    /// for why that split is the point.
    pub(crate) fn compile_loop_entry(
        &mut self,
        prior_state: Option<&Json>,
        plan: LoopEntryPlan,
    ) -> Result<(CompiledTurn, CompiledTurn), DriverError> {
        // Register this cycle's (stateJson, operatorMsgJson) under the
        // stable Val.G0 binding BEFORE compiling — the outer module's
        // `--inject-val` reference below resolves at RUN time against
        // whatever this call last registered on the OUTER session.
        self.refresh_harness_ctx(prior_state)?;

        let outer = self.outer.as_ref().ok_or_else(not_bootstrapped)?;
        let imports = format!(
            "qualified {} as {}\n{}",
            outer.module_name,
            state_cross::LOADED_QUALIFIER,
            state_cross::harness_ctx_module().module_name(),
        );
        let stack = outer
            .cfg
            .turn_target(None)
            .map_err(|e| DriverError::Session(format!("outer engine target: {e}")))?
            .stack;
        let extract_bin = outer.cfg.extract_bin.clone();
        let include = outer.cfg.include.clone();

        let (loop_code, resume_helpers) = plan.into_code_and_helpers();
        // FIXED text — turn-invariant by construction, see
        // `state_cross::state_in_via_ctx`'s doc. `resume_helpers` stays a
        // literal splice (unchanged, out of this pass's scope): it is
        // non-empty on at most the first cycle after a boot fold, never
        // re-paid every turn the way state/operator-msg were.
        let helpers = format!(
            "{}{}{}",
            state_cross::state_in_via_ctx(),
            state_cross::operator_msg_in_via_ctx(),
            resume_helpers,
        );
        let render_code = format!(
            "pure ({q}.render __selfHarnessState)",
            q = state_cross::LOADED_QUALIFIER
        );
        let src = engine::template_turn_for_fused(
            &outer_decls(),
            &stack,
            &render_code,
            &imports,
            &helpers,
            &[(Self::LOOP_ENTRY_TARGET, &loop_code)],
        );
        self.emit(Event::OuterCompile {
            label: "render+loop".to_string(),
            source: src.clone(),
        });
        let mut turns = engine::compile_turns_with_stable_inject(
            &extract_bin,
            &src,
            &["result", Self::LOOP_ENTRY_TARGET],
            &include,
            tidepool_runtime::StableValInject {
                module: state_cross::harness_ctx_module(),
                session_root: &Self::harness_ctx_session_root(),
            },
            timing::NO_NODE,
            timing::NO_ROUND,
        )
        .map_err(|e| DriverError::Session(format!("fused outer compile failed: {e:?}")))?;
        let render_turn = turns.remove("result").ok_or_else(|| {
            DriverError::Session("fused outer compile: missing render entry".into())
        })?;
        let loop_turn = turns.remove(Self::LOOP_ENTRY_TARGET).ok_or_else(|| {
            DriverError::Session("fused outer compile: missing loop entry".into())
        })?;
        Ok((render_turn, loop_turn))
    }

    /// The `--session-root` the harness-ctx bind writes/reads its `.hi`
    /// iface under — reuses [`Self::outer_plane_root`] rather than a
    /// separate directory: the OUTER session's `PersistentSession` is ONE
    /// `BindingTable`/value plane, so once
    /// `Val.G0` is registered there, ANY later compile on the same session
    /// that consults `live_val_modules`/`current_val_modules` — in
    /// particular an answerer turn's compile, which injects every live
    /// session value — reports it and expects to find its iface under
    /// THIS SAME root, whichever caller set it up. Two sessions couldn't
    /// each keep their own; there is exactly one root per session, already
    /// named. Safe to write into: `open_outer_plane` wipes this dir only at
    /// `bootstrap` (once per session — a machine ROTATION transfers the
    /// existing `SessionLib` via [`Self::build_outer_session`] rather than
    /// re-wiping), and the harness-ctx iface's CONTENT is a pure function of
    /// [`state_cross::harness_ctx_module`] (a fixed name/type), so
    /// overwriting it in place every cycle alongside the decl plane's own
    /// `Lib.G<g>.hs` files is safe and keeps the memo's content fingerprint
    /// stable turn to turn.
    pub(crate) fn harness_ctx_session_root() -> PathBuf {
        Self::outer_plane_root()
    }

    /// (Re-)bind [`state_cross::HARNESS_CTX_BINDING`] at
    /// [`state_cross::harness_ctx_module`] on the OUTER session to this
    /// cycle's `(stateJson, operatorMsgJson)` — the value half of the
    /// turn-invariant harness-context injection (the type/iface
    /// half is [`Self::compile_loop_entry`]'s `--inject-val`).
    ///
    /// Compiles a tiny standalone module
    /// ([`state_cross::harness_ctx_source`]) through
    /// [`tidepool_runtime::session::turn::compile_session_turn`]'s
    /// `--session-bind` path — its own small, deliberately non-cacheable
    /// spawn (fresh literal content every cycle; see that function's doc) —
    /// then runs it, tenures the result, and registers it against the OUTER
    /// session via [`tidepool_runtime::session::resident::ResidentSession::run_bind`]:
    /// the SAME `Tidepool.Session.Val.G<g>` value-plane mechanism the
    /// interactive session already uses for its rotating binds, just at the
    /// one reserved, non-rotating generation
    /// ([`state_cross::harness_ctx_module`]'s doc explains why gen 0 can
    /// never collide with a real one). `run_bind` both materializes AND
    /// registers the binding in one call, so nothing further is needed here
    /// for a later `--inject-val` reference (or this SAME session's own
    /// `render`/`loop` run, which resolves it automatically via
    /// `ResidentSession::run`'s existing `seed_external_env_for`) to see it.
    pub(crate) fn refresh_harness_ctx(
        &mut self,
        prior_state: Option<&Json>,
    ) -> Result<(), DriverError> {
        let state_json = prior_state.map_or_else(|| "null".to_string(), Json::to_string);
        let operator_json = serde_json::to_string(&self.pending_operator_input)
            .unwrap_or_else(|_| "null".to_string());
        let src = state_cross::harness_ctx_source(&state_json, &operator_json);

        let session_root = Self::harness_ctx_session_root();
        std::fs::create_dir_all(&session_root)
            .map_err(|e| DriverError::Session(format!("harness-ctx session root: {e}")))?;

        let binding_name = state_cross::HARNESS_CTX_BINDING.to_string();
        let turn = tidepool_runtime::session::turn::compile_session_turn(
            &src,
            &[],
            &session_root,
            &[],
            Some(tidepool_runtime::session::turn::SessionBind {
                names: std::slice::from_ref(&binding_name),
                gen: 0,
                probe_only: false,
            }),
        )
        .map_err(|e| DriverError::Session(format!("harness-ctx bind compile failed: {e:?}")))?;
        let binder = turn.binders.first().ok_or_else(|| {
            DriverError::Session("harness-ctx bind: extract returned no binders".into())
        })?;

        let sid = self.outer_sid()?;
        let outcome = self
            .agent
            .with_session(sid, |s| {
                s.run_bind(
                    "harness_ctx",
                    &turn.expr,
                    &turn.table,
                    binder,
                    tidepool_repr::Generation(0),
                )
            })
            .map_err(|e| DriverError::Session(e.to_string()))?
            .map_err(|e| DriverError::Session(format!("harness-ctx bind run failed: {e}")))?;
        match outcome {
            ResidentOutcome::Completed { .. } => Ok(()),
            ResidentOutcome::Suspended { .. } => Err(DriverError::Session(
                "harness-ctx bind suspended unexpectedly — must be a pure value".into(),
            )),
        }
    }

    /// Run ONE `render` → `loop` → (service each `runLLMTurn` hole) →
    /// `render` cycle: bootstrap the outer session if needed, render the
    /// pre-loop prompt ([`Self::render_framing`] — the author's `render`
    /// output composed with the prior compaction summary and the
    /// loop-iteration count), run `loop state` as a suspendable fragment
    /// (servicing every `runLLMTurn` hole via
    /// [`Self::service_typed_request_suspension`]), serialize the returned `State`
    /// ([`state_cross::state_out`]), advance `self.iteration`, and render
    /// the post-loop prompt. `prior_state` is `None` only for the very
    /// first cycle. The compaction summary fed to [`Self::render_framing`]
    /// is NOT a parameter — it is `self.last_compaction`, the latest
    /// emergency-compaction `Text` if one has fired, carried forward
    /// automatically across repeated calls (by [`Self::run_loop`], or by a
    /// caller driving cycles by hand — see `acceptance_selfharness.rs`),
    /// since the *runtime*, not the caller, owns the compaction lifecycle.
    /// This cycle's OWN compaction (if
    /// [`Self::maybe_compact_answerer`] fires one MID-LOOP) updates
    /// `self.last_compaction` before `prompt_after` is rendered, so
    /// `prompt_after` already reflects it — proving the summary reaches the
    /// very next render.
    pub async fn run_one_loop_iteration(
        &mut self,
        source: &HarnessSource,
        prior_state: Option<&Json>,
    ) -> Result<LoopIterationOutcome, DriverError> {
        self.refuse_if_poisoned()?;

        // A prior cycle's error guard (below) already discarded `self.outer`,
        // so this bootstrap call is where recovery from a `Failed` state
        // rebuilds it. If recovery itself cannot bootstrap, the driver has no
        // path back to a usable outer session — escalate past `Failed`
        // (recoverable) to `Poisoned` (not) rather than sit in a stale
        // `Failed` that will never clear. A bootstrap failure that is NOT a
        // recovery attempt (the very first cycle a driver ever runs) is an
        // ordinary cycle error: discard whatever partial state accumulated
        // and publish `Failed`, same as any other cycle error, so the next
        // call retries bootstrap rather than leaving `lifecycle()` reporting
        // the cosmetic `Idle` a driver starts in.
        let recovering_from_failure = matches!(self.lifecycle, SelfHarnessState::Failed { .. });
        if let Err(e) = self.bootstrap(source) {
            self.discard_resident_state();
            self.lifecycle = if recovering_from_failure {
                SelfHarnessState::Poisoned {
                    reason: e.to_string(),
                }
            } else {
                SelfHarnessState::Failed {
                    reason: e.to_string(),
                }
            };
            return Err(e);
        }
        // Loop-boundary machine maintenance: instrumentation + the enforced
        // fragment ceiling (rotation at quiescence).
        self.machine_maintenance()?;
        self.emit(Event::LoopBoundary);

        // Mint THIS cycle's loop-entry plan ONCE, here — the one call to
        // `take_loop_entry` a production cycle ever makes — and pass it
        // down to `compile_loop_entry` rather than letting that method
        // mint its own (`LoopEntryPlan`'s doc).
        let plan = self.take_loop_entry();

        // Compile the pre-loop `render` and this cycle's `loop` fragment
        // TOGETHER, in ONE spawn (`Self::compile_loop_entry`), then run the
        // render entry directly against `prior_state` — `None` (the very
        // first cycle) splices `Loaded.initialState` in the shared helpers
        // (`state_cross::state_in(None)`), so no redundant `pure initialState`
        // compile + round-trip through JSON is needed.
        //
        // THIS compile — not `bootstrap` — is where "can we build a usable
        // outer session at all" is actually answered, so its failure (compile
        // OR the render entry's run) takes the same Failed-vs-Poisoned
        // classification as a bootstrap failure. Being one fused spawn, a
        // loop-entry compile failure surfaces here too. A bare `?` here would
        // return early PAST the lifecycle update below, leaving a failed
        // driver reporting the cosmetic `Idle` (or a failed recovery
        // reporting `Failed` forever instead of escalating) — do not
        // simplify this to one.
        let prior_compaction = self.last_compaction.clone();
        let (prompt_before, loop_turn) = match self.compile_loop_entry(prior_state, plan) {
            Ok((render_turn, loop_turn)) => {
                match self.render_framing_with(&render_turn, prior_compaction.as_deref()) {
                    Ok(prompt) => (prompt, loop_turn),
                    Err(e) => {
                        self.discard_resident_state();
                        self.lifecycle = if recovering_from_failure {
                            SelfHarnessState::Poisoned {
                                reason: e.to_string(),
                            }
                        } else {
                            SelfHarnessState::Failed {
                                reason: e.to_string(),
                            }
                        };
                        return Err(e);
                    }
                }
            }
            Err(e) => {
                self.discard_resident_state();
                self.lifecycle = if recovering_from_failure {
                    SelfHarnessState::Poisoned {
                        reason: e.to_string(),
                    }
                } else {
                    SelfHarnessState::Failed {
                        reason: e.to_string(),
                    }
                };
                return Err(e);
            }
        };

        // The pre-loop render IS the answerer session's system message.
        // Compose it with the narrow answerer instruction and stash it for
        // `run_loop_fragment` to seed the per-loop answerer node.
        self.answerer_framing = Some(format!(
            "{prompt_before}\n\n{}",
            typed_request_agent_framing_suffix(
                &self.agent.cfg().decls,
                self.fork_budget_per_window,
                self.fork_subtree_cap
            )
        ));

        self.lifecycle = SelfHarnessState::RunningLoop;
        // The driver must not strand the lifecycle in `RunningLoop`/`Compacting`
        // on any exit from the loop body: a runaway-cap hard-fail, a failed
        // resume, or a compaction error all leave a mutable resident session
        // (the outer session, the per-loop answerer) that outlives this call.
        // Run the fallible body, then publish `Idle` on success or `Failed`
        // (after discarding that resident state) on error — never `Idle` on
        // a path that didn't actually finish.
        let result: Result<LoopIterationOutcome, DriverError> = async {
            let (value, table) = self.run_loop_fragment(prior_state, Some(loop_turn)).await?;
            let state_json = state_cross::state_out(&value, &table);

            // This cycle's `loop` completed — advance the runtime's OWN
            // iteration counter (never part of authored `State`) before the
            // post-loop render, so `prompt_after` (and the next cycle's
            // `prompt_before`) report the count of loops completed so far.
            self.iteration += 1;

            // Any MID-LOOP compaction that fired during this loop has already
            // set `self.cycle_compaction` (and `self.last_compaction`) IN PLACE —
            // the loop CONTINUED under the summary rather than aborting. `None` if
            // the context window never crossed threshold this loop.
            let compaction = self.cycle_compaction.take();
            // `self.last_compaction` carries the LATEST compaction summary forward
            // to the next render regardless of which cycle produced it: this
            // cycle's if one fired, else the prior cycle's (unchanged). Render
            // `prompt_after` against it so the summary reaches the very next render
            // (02-runtime.md; the compaction summary is composed by
            // `render_framing`, not threaded through the author's `render`).
            let next_compaction = self.last_compaction.clone();
            let prompt_after =
                self.render_framing(Some(&state_json), next_compaction.as_deref())?;

            // One writer, one boundary: a cycle that reaches this point
            // completed successfully, so its state and the compaction summary
            // in force right now commit together as the next generation.
            self.commit_checkpoint(source, &state_json)?;

            Ok(LoopIterationOutcome {
                prompt_before,
                state_json,
                prompt_after,
                compaction,
            })
        }
        .await;
        match &result {
            Ok(_) => self.lifecycle = SelfHarnessState::Idle,
            Err(err) => {
                self.discard_resident_state();
                self.lifecycle = SelfHarnessState::Failed {
                    reason: err.to_string(),
                };
            }
        }
        result
    }

    /// Bootstrap the outer session over `source`, restore the last
    /// committed checkpoint from [`Self::checkpoint_path`] if one is there
    /// yet (restart-reload — falls back to `initialState`, exactly the
    /// in-process very-first-cycle case, when nothing has been committed
    /// yet), then run [`Self::run_one_loop_iteration`] FOREVER, threading each
    /// cycle's returned `State` into the next one (each cycle commits its
    /// own checkpoint on success — see [`Self::commit_checkpoint`] — so
    /// this loop does no persistence of its own). Production entry point —
    /// see the module doc for why this (and everything it calls) must run
    /// on a thread with an active multi-thread tokio runtime.
    ///
    /// Between-loops human gate: before each new cycle, unless `auto` is set
    /// or this is the very first cycle of a fresh run (no checkpoint restored
    /// at all — straight into the loop, which asks the authored seed question
    /// via `askUser`), block on [`Self::between_loops_gate`] — an ordinary
    /// operator form ("Turn N complete — start turn N+1?" plus an optional
    /// steering field), presented through the same `present_form` machinery
    /// every `askUser` ask uses. `auto` (the binary's `--yes`/`--auto` flag)
    /// skips the gate for CI/replay. The acceptance path drives
    /// [`Self::run_one_loop_iteration`] directly and has NO gate.
    ///
    /// **Restart rule (uniform, no marker):** ANY boot that restores a
    /// checkpoint presents the between-turns gate before running the next
    /// turn — regardless of whether the prior process crashed mid-turn (the
    /// turn simply reruns per the existing at-least-once semantics, and the
    /// operator is asked again before it starts) or while genuinely parked on
    /// the gate itself (the operator is asked again, no different from any
    /// other restart). This replaces an earlier design that persisted a
    /// dedicated `awaiting_continue` checkpoint marker to distinguish the two
    /// cases — the marker is gone; every restart with prior history simply
    /// re-asks.
    ///
    /// Defense in depth against a state-decode failure taking the whole
    /// process down: whatever [`Self::restore`]'s fingerprint check misses (a
    /// hash collision, a hand-edited checkpoint, a same-source edit that
    /// changes the `State` type without changing the file's fingerprint), a
    /// cycle that fails with [`DriverError::StateDecode`] while `state_json`
    /// was `Some` (a restored or prior-cycle state, not the very first cycle)
    /// is retried EXACTLY ONCE from fresh `initialState` rather than
    /// propagated — logged loudly first. If the retry ALSO fails, that is an
    /// ordinary cycle error and takes the normal `Failed`/`Poisoned`
    /// path, same as any other error; this is a single retry, not a new
    /// ladder rung. A `StateDecode` when `state_json` is already `None` means
    /// the harness source's own `initialState`/`FromJSON State` disagree —
    /// a real bug in the harness, not a stale checkpoint — and propagates as
    /// it does for every other cycle error.
    pub async fn run_loop(
        &mut self,
        source: &HarnessSource,
        auto: bool,
    ) -> Result<(), DriverError> {
        self.refuse_if_poisoned()?;
        let mut state_json: Option<Json> = self.restore(source).await?;
        // Uniform restart rule: any restored checkpoint means this is not the
        // very first cycle ever, so the between-turns gate must present
        // before the next turn runs — whether the prior process crashed
        // mid-turn (the turn reruns, per existing at-least-once semantics,
        // and the operator is asked again first) or while genuinely parked on
        // the gate (asked again, no different from any other restart). Only
        // a first-ever run (no checkpoint at all, `last_checkpoint` is
        // `None`) skips straight into the loop, which asks the seed question
        // via the authored `askUser`.
        let mut first = self.last_checkpoint.is_none();
        loop {
            if !first && !auto {
                self.between_loops_gate().await?;
            }
            first = false;
            let outcome = match self
                .run_one_loop_iteration(source, state_json.as_ref())
                .await
            {
                Ok(outcome) => outcome,
                Err(DriverError::StateDecode(detail)) if state_json.is_some() => {
                    tracing::warn!(
                        detail = %detail,
                        "cycle failed to decode its restored State — retrying once from \
                         fresh initialState instead of taking the process down"
                    );
                    state_json = None;
                    // The carried state's loop history goes with it (same
                    // reasoning as restore's compaction drop).
                    self.iteration = 0;
                    self.discard_resident_state();
                    self.run_one_loop_iteration(source, state_json.as_ref())
                        .await?
                }
                Err(e) => return Err(e),
            };
            state_json = Some(outcome.state_json);
        }
    }

    /// Reload the checkpoint at [`Self::checkpoint_path`], if one is there
    /// yet, returning its `State` JSON (or `None` for a first-ever run — no
    /// checkpoint has been committed). Restores `self.last_compaction`,
    /// `self.checkpoint_generation`, and `self.iteration` from the same
    /// record, so the first render after a restart feeds the same
    /// compaction summary the prior process distilled, the next commit
    /// continues the generation sequence rather than restarting it at 1,
    /// and the loop-metadata count resumes at the right number instead of
    /// resetting to `0`.
    ///
    /// `source`'s fingerprint identifies the harness file THIS process just
    /// loaded. A restored checkpoint whose fingerprint disagrees is DISCARDED
    /// rather than restored: the checkpoint's `State` was produced by a
    /// DIFFERENT harness source and is not safe to decode against the
    /// current one (a mismatched `State` shape crashes the process on boot —
    /// the whole reason this check exists). [`Event::HarnessSourceChanged`]
    /// is still emitted, carrying both fingerprints, as the durable record of
    /// what happened; the generation counter still adopts
    /// `checkpoint.generation` so it stays monotonic across the restart, but
    /// `self.last_compaction` is left `None` (a compaction summary describes
    /// the discarded harness's loop, not this one) and the run starts fresh
    /// from `initialState`, exactly the first-ever-run path. A harness file
    /// that self-edits and restarts therefore loses its accumulated `State`
    /// even when the `State` TYPE didn't change — a real cost, taken
    /// deliberately: a lost `State` costs a run, a decoded-then-poisoned one
    /// costs the process.
    ///
    /// Called by [`Self::run_loop`] at start; exposed so a restart-durability
    /// test can drive the same reload path without entering the
    /// forever-loop.
    pub async fn restore(&mut self, source: &HarnessSource) -> Result<Option<Json>, DriverError> {
        self.refuse_if_poisoned()?;
        let Some(checkpoint) = persistence::load_checkpoint(&self.checkpoint_path)? else {
            return Ok(None);
        };
        // Kept whole so `Self::run_loop` can tell "a checkpoint exists" from
        // "first-ever run" — the uniform restart rule (see that method's
        // doc): any prior checkpoint means the between-turns gate presents
        // before the next turn, no marker needed.
        self.last_checkpoint = Some(checkpoint.clone());
        self.checkpoint_generation = Some(checkpoint.generation());
        self.iteration = checkpoint.iteration().get();
        // Neither is scoped to the harness source: the operator's pending
        // utterance and the ask-id high-water mark are driver-runtime facts,
        // not `State`, so both carry forward even through the
        // fingerprint-mismatch branch below (which only discards `State`).
        self.pending_operator_input = checkpoint.pending_operator_input().map(str::to_string);
        self.ask_id_counter
            .store(checkpoint.ask_id_high_water(), Ordering::SeqCst);
        if checkpoint.harness_source != source.fingerprint {
            // Carry state forward across source changes. `run_loop` retries a
            // genuinely incompatible state once from `initialState`, while a
            // prompt-only or shape-compatible change retains accumulated
            // state. `last_compaction` belongs to the old source and is always
            // discarded; iteration is reset only if state decoding falls back.
            tracing::info!(
                restored_fingerprint = %checkpoint.harness_source,
                current_fingerprint = %source.fingerprint,
                "harness source changed since the checkpoint — carrying the persisted \
                 state forward (a shape-incompatible state falls back to initialState \
                 via the StateDecode retry)"
            );
            self.emit(Event::HarnessSourceChanged {
                restored_fingerprint: checkpoint.harness_source,
                current_fingerprint: source.fingerprint.clone(),
            });
            self.last_compaction = None;
            return Ok(Some(checkpoint.state));
        }
        self.last_compaction = checkpoint.compaction;
        Ok(Some(checkpoint.state))
    }

    /// Commit the checkpoint for a cycle that just completed successfully:
    /// `state` (that cycle's own returned `State`), `self.last_compaction`
    /// (the compaction summary in force at this same moment — a mid-loop
    /// compaction already updated it in place, so a cycle that compacted and
    /// one that didn't commit through the same path), and `self.iteration`
    /// (already advanced by [`Self::run_one_loop_iteration`] before this call) go
    /// into one [`persistence::Checkpoint`], written atomically under the
    /// next generation. Called once, at the end of [`Self::run_one_loop_iteration`]'s
    /// success path — the ONLY place a checkpoint is written, so a state, a
    /// summary, and an iteration count read back together are always from
    /// the same generation.
    pub(crate) fn commit_checkpoint(
        &mut self,
        source: &HarnessSource,
        state: &Json,
    ) -> Result<(), DriverError> {
        let checkpoint = persistence::Checkpoint::committed(
            self.checkpoint_generation,
            state.clone(),
            self.last_compaction.clone(),
            source.fingerprint.clone(),
            persistence::LoopIteration::new(self.iteration),
        )
        .with_ask_id_high_water(self.ask_id_counter.load(Ordering::SeqCst));
        persistence::save_checkpoint(&self.checkpoint_path, &checkpoint)?;
        self.checkpoint_generation = Some(checkpoint.generation());
        self.last_checkpoint = Some(checkpoint);
        Ok(())
    }

    /// The between-loops human checkpoint: an ORDINARY operator form —
    /// [`between_loops_gate_shape`] — presented through
    /// [`Self::present_askuser_form`], the same funnel every `askUser` ask
    /// uses (precedent: `Harness::escalate_to_operator`'s `AllocateMore`/
    /// `Abort` form, driver-authored the same way). No dedicated gate
    /// mechanism, no checkpoint write of its own: the uniform restart rule
    /// ([`Self::run_loop`]'s doc) covers the crash-while-parked case without
    /// one — a kill here just means the next boot re-presents this same ask
    /// before running the next turn, same as a kill mid-turn means the turn
    /// reruns.
    ///
    /// An empty (or whitespace-only) `steer` field is a plain continue; any
    /// other text becomes [`Self::pending_operator_input`] — the operator's
    /// one channel for initiating — threaded into the next cognition
    /// window's framing exactly as before.
    pub(crate) async fn between_loops_gate(&mut self) -> Result<(), DriverError> {
        let shape = between_loops_gate_shape(self.iteration);
        let mut reprompts: u32 = 0;
        let submission = self
            .present_askuser_form(&mut reprompts, FormSource::OuterLoop, &shape)
            .await?;
        let operator_text = submission
            .get("steer")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        if let Some(text) = &operator_text {
            self.emit(Event::OperatorMessage { text: text.clone() });
        }
        self.pending_operator_input = operator_text;
        Ok(())
    }

    /// Drive `Loaded.loop __selfHarnessState` (spliced via
    /// [`state_cross::state_in`]) as a suspendable fragment on the outer
    /// session, servicing every `runLLMTurn` hole it suspends on via
    /// [`Self::service_typed_request_suspension`] until it completes. Returns the
    /// completed `State` value and the DataConTable its OWN compile produced
    /// (the table every hole along this same continuation classifies
    /// against — `resume` never recompiles).
    ///
    /// `precompiled`, when `Some`, is this cycle's loop entry from
    /// [`Self::compile_loop_entry`] — used AS-IS instead of compiling one
    /// here, which is how [`Self::run_one_loop_iteration`] pays only ONE fused spawn
    /// for both `render` and `loop`. `None` compiles it here via
    /// [`Self::compile_outer`], exactly as before fusion — a direct caller
    /// (a test driving this fragment in isolation) keeps working unfused.
    ///
    /// Emergency compaction does NOT happen here at loop end — it fires
    /// MID-LOOP via [`Self::maybe_compact_answerer`] (checked between the
    /// answerer's holes/rounds against its real accumulated context), setting
    /// `self.cycle_compaction`/`self.last_compaction` in place while the loop
    /// continues under the summary.
    pub(crate) async fn run_loop_fragment(
        &mut self,
        prior_state: Option<&Json>,
        precompiled: Option<CompiledTurn>,
    ) -> Result<(Value, DataConTable), DriverError> {
        self.loop_inference_calls.store(0, Ordering::SeqCst);
        self.cycle_compaction = None;
        self.loop_state_json = prior_state.cloned();

        // Create the ONE render-seeded answerer session for this whole
        // loop, up front — every `runLLMTurn` hole pushes onto it, so hole #2
        // sees hole #1's exchange (the accumulating context window). Retired
        // in `retire_typed_request_agent` once the loop completes (or errors out).
        let answerer =
            self.agent
                .create_root_framed("loop answerer", "", self.answerer_framing.clone())?;
        // ONE SESSION: the answerer node runs as a REALM on the shared outer
        // machine (its turns park beside the loop's own frame; its values —
        // closures included — are born in the loop's heap). Realm minted per
        // loop; retirement is that realm's scope exit via terminate_node.
        let sid = self.outer_sid()?;
        self.agent.force_attached(answerer, Actor::Operator, sid)?;
        let realm = self.mint_resource_scope();
        self.agent.set_node_realm(answerer, realm);
        self.answerer = Some(AgentSessionMode::ReusableLoop {
            node: answerer,
            realm,
        });

        let result = self.run_loop_fragment_inner(prior_state, precompiled).await;
        self.retire_typed_request_agent();
        result
    }

    /// Retire the current loop's answerer node (terminalize it and drop its
    /// session), so the next loop starts from a fresh render-seeded one.
    /// Idempotent — a no-op if no answerer is live.
    pub(crate) fn retire_typed_request_agent(&mut self) {
        if let Some(lease) = self.answerer.take() {
            let node = lease.node();
            let _ = self.agent.terminate_node(node, "loop answerer retired");
            self.fork_child_seq.lock().remove(&node);
        }
    }

    /// Mint a resource scope for one answerer window.
    pub(crate) fn mint_resource_scope(&self) -> tidepool_codegen::suspension::RealmId {
        tidepool_codegen::suspension::RealmId::fresh()
    }

    /// The body of [`Self::run_loop_fragment`] — run `loop`, service each
    /// `runLLMTurn` hole against the pre-created per-loop answerer
    /// (`self.answerer`), and return the completed `State` + table. Split out
    /// so [`Self::run_loop_fragment`] can retire the answerer whether this
    /// succeeds or errors.
    ///
    /// Mid-loop, in-place compaction: AFTER each hole is serviced — a
    /// natural boundary between the answerer's holes — [`Self::maybe_compact_answerer`]
    /// checks the answerer's real accumulated context against the threshold and,
    /// if past it, summarizes + replaces the answerer's context IN PLACE so the
    /// loop's REMAINING holes continue under a smaller window (no abort).
    ///
    /// `precompiled`, when `Some`, is used as-is instead of calling
    /// [`Self::compile_outer`] — see [`Self::run_loop_fragment`]'s doc.
    pub(crate) async fn run_loop_fragment_inner(
        &mut self,
        prior_state: Option<&Json>,
        precompiled: Option<CompiledTurn>,
    ) -> Result<(Value, DataConTable), DriverError> {
        let compiled = match precompiled {
            Some(compiled) => compiled,
            None => {
                // The unfused path mints its OWN plan — a direct fragment
                // API a test drives in isolation, never called from
                // `run_one_loop_iteration` (which mints one plan and passes it to
                // `compile_loop_entry` instead). See `LoopEntryPlan`'s doc.
                let (code, resume_helpers) = self.take_loop_entry().into_code_and_helpers();
                let helpers = format!(
                    "{}{}{}",
                    state_cross::state_in(prior_state),
                    state_cross::operator_msg_in(self.pending_operator_input.as_deref()),
                    resume_helpers,
                );
                self.compile_outer(&code, &helpers, "loop")?
            }
        };

        // The scheduler's FIFO ready queue — EVERY suspension
        // this loop drives (the primary `loop` chain's own, and any green
        // thread's) goes through it uniformly: pop one already-produced
        // outcome, classify it, service it (pushing back whatever it
        // produces next), repeat. A `Completed` can only ever come from the
        // PRIMARY chain's own top-level fragment — a green thread's body is
        // always wrapped (`asyncSpawn`) to end by SUSPENDING on
        // `AsyncDoneWith`, never by completing — so seeing one here IS this
        // cycle's `loop` finishing, regardless of any other thread still
        // parked (an unawaited thread is a legitimate orphan, same
        // starvation contract `Tidepool.Async`'s module doc already states).
        let mut ready: VecDeque<GreenReady> = VecDeque::new();
        {
            let sid = self.outer_sid()?;
            let first = self
                .agent
                .with_session(sid, |s| s.run("loop", &compiled.expr, &compiled.table))
                .map_err(|e| DriverError::Session(e.to_string()))?
                .map_err(|e| map_run_error("loop run failed", e.to_string()))?;
            ready.push_back(GreenReady {
                chain: GreenChain::Primary,
                outcome: first,
            });
        }

        let mut threads: HashMap<i64, GreenThread> = HashMap::new();
        let mut waiters: HashMap<i64, Vec<(GreenChain, String)>> = HashMap::new();
        let mut next_tid: i64 = 1;

        // EVERY branch below hands back a `ServicedSuspension` instead of directly
        // pushing to `ready`/breaking the loop. This makes forgetting a
        // computed outcome a type error. `SuspensionRouting::Green` is the exception,
        // documented at `ServicedSuspension` and at `Self::service_green_hole`.
        //
        // Wrapped in a bare (non-`move`) `async` block so every `?`
        // inside the loop body (`classify_hole`, each `service_*().await?`,
        // every resume's `map_err(...)?`) returns from THIS block instead of
        // from the whole function — `?` always targets the nearest enclosing
        // fn/closure/async-block, and an `async {}` block counts. Without
        // this, an early `?` skipped the structured-concurrency sweep below
        // entirely, leaking every still-open thread realm this fragment
        // spawned. No `move`: `threads`/`waiters`/`ready`/`next_tid` and
        // `self` stay borrowed for the block's
        // span and are still owned by this function afterward, which is what
        // the sweep below needs.
        let outcome_result: Result<(Value, DataConTable), DriverError> = async {
            loop {
            // Batch-and-drive every ready item CONCURRENTLY, but ONLY once
            // EVERY item currently in `ready` is SUBAGENT-routed — mirroring
            // `service_green_round`'s Fork drain (green.rs), generalized to
            // `Subagent` per `CONCURRENT_SIBLINGS_SPIKE_FINDINGS.md`'s
            // smallest driver change. `ready` here carries BOTH the primary
            // `loop` chain's own suspensions and every green thread's (this
            // loop's own `SuspensionRouting::Green` arm below feeds thread
            // resumes back into this SAME queue).
            //
            // Unlike Fork's spawn (a pure JIT step, never itself a driver
            // suspension), `spawnAsync`/`awaitAgent` — `spawnAgent`'s own
            // two-suspension shape (`haskell/lib/Tidepool/Agent/Spawn.hs`) —
            // ARE driver suspensions: a sibling thread's own `async
            // (spawnAgent ...)` needs its OWN `SuspensionRouting::Green`
            // pass (spawning the thread, running it to its first suspension)
            // before ITS `Subagent` request even exists. Firing a batch as
            // soon as ANY one `Subagent` item is ready — before a sibling's
            // still-pending `Green` spawn has had a chance to run — would
            // batch a batch of ONE (this thread's own `spawnAsync`, or worse
            // its slow `awaitAgent`) while the sibling's `spawnAsync` sits
            // unprocessed behind it, reproducing the exact one-at-a-time bug
            // this generalization exists to fix. Gating on "everything ready
            // is `Subagent`" defers batching until every sibling's own
            // fast/synchronous `Green` spawn step has already run via the
            // ordinary single-item path below (each such step is
            // JIT-fast and never blocks, so servicing it individually costs
            // nothing) — at that point every sibling that COULD be ready is,
            // including a slow `awaitAgent` sitting alongside a sibling's
            // freshly-arrived `spawnAsync`, which is exactly the pairing
            // that must run CONCURRENTLY for the two cycles' background
            // threads to actually overlap.
            let ready_for_subagent_batch = !ready.is_empty()
                && ready.iter().all(|item| {
                    matches!(
                        &item.outcome,
                        ResidentOutcome::Suspended { request, .. }
                            if matches!(
                                engine::classify_hole(request, &compiled.table, &compiled.asks),
                                Ok(c) if matches!(c.routing, SuspensionRouting::Subagent)
                            )
                    )
                });
            if ready_for_subagent_batch {
                let mut subagent_batch: Vec<(GreenChain, String, Value)> =
                    Vec::with_capacity(ready.len());
                for item in ready.drain(..) {
                    match item.outcome {
                        ResidentOutcome::Suspended { hole, request, .. } => {
                            subagent_batch.push((item.chain, hole.cont_id().to_string(), request));
                        }
                        ResidentOutcome::Completed { .. } => {
                            unreachable!("ready_for_subagent_batch checked Suspended above")
                        }
                    }
                }
                let this = &*self;
                let table = &compiled.table;
                let batch_ref = &subagent_batch;
                let cap = self.concurrency_cap;
                #[allow(clippy::type_complexity)]
                let results: Vec<(usize, Result<Value, DriverError>)> =
                    drive_concurrent(cap, subagent_batch.len(), |idx| {
                        let (_, _, request) = &batch_ref[idx];
                        async move {
                            this.service_outer_subagent(request, table, FormSource::OuterLoop)
                                .await
                        }
                    })
                    .await;
                let mut first_err: Option<DriverError> = None;
                for (chain, hole, value) in subagent_batch
                    .into_iter()
                    .zip(results.into_iter().map(|(_, r)| r))
                    .filter_map(|((chain, hole, _), r)| match r {
                        Ok(v) => Some((chain, hole, v)),
                        Err(e) => {
                            if first_err.is_none() {
                                first_err = Some(e);
                            }
                            None
                        }
                    })
                {
                    let sid = self.outer_sid()?;
                    let next = self
                        .agent
                        .with_session(sid, |s| s.resume(ResidentHole::plain(hole), value))
                        .map_err(|e| DriverError::Session(e.to_string()))?
                        .map_err(|e| {
                            DriverError::Session(format!("subagent resume failed: {e}"))
                        })?;
                    ready.push_back(GreenReady { chain, outcome: next });
                }
                if let Some(e) = first_err {
                    break Err(e);
                }
                continue;
            }

            let Some(GreenReady { chain, outcome }) = ready.pop_front() else {
                break Err(DriverError::Session(
                    "green scheduler starved: no ready work and the outer loop never completed \
                     (a parked thread with no waiter and no completion path)"
                        .into(),
                ));
            };
            let serviced: ServicedSuspension = match outcome {
                ResidentOutcome::Completed { result, .. } => ServicedSuspension::Completed {
                    result: result.into_value(),
                    table: compiled.table.clone(),
                },
                ResidentOutcome::Suspended { hole, request, .. } => {
                    let classified =
                        engine::classify_hole(&request, &compiled.table, &compiled.asks)?;
                    match &classified.routing {
                        SuspensionRouting::RunLLMTurn { site, ty } => {
                            let answer = self
                                .service_typed_request_suspension(
                                    site.get(),
                                    ty.as_deref(),
                                    compiled.asks.modules_of(site.get()),
                                    &classified.prompt,
                                    &compiled.table,
                                )
                                .await?;
                            // Between holes — if the answerer's accumulated
                            // context has crossed threshold, compact + replace its
                            // context IN PLACE now, so the NEXT hole drives under
                            // the smaller window.
                            //
                            // This runs only AFTER `service_typed_request_suspension` has
                            // already finalized THIS hole's answer
                            // (`take_finalized_value_keep_open` consumed the finalize
                            // continuation and returned the session to idle —
                            // harness.rs `take_finalized_value_keep_open`). So the
                            // summarize turn `maybe_compact_answerer` drives sees the
                            // last answer already IN the transcript and cannot drop
                            // it: a future refactor that moves this call BEFORE the
                            // answer is taken would compact a mid-finalize session —
                            // do not.
                            self.maybe_compact_answerer().await?;
                            let sid = self.outer_sid()?;
                            let next = self
                                .agent
                                .with_session(sid, |s| match answer {
                                    FinalAnswer::Value(v) => s.resume(hole, v),
                                    // The closure payload is
                                    // DELIVERED by handle — same heap, no
                                    // bridge, no sentinel.
                                    FinalAnswer::Handle(h) => s.resume_handle(hole.cont_id(), h),
                                })
                                .map_err(|e| DriverError::Session(e.to_string()))?
                                .map_err(|e| {
                                    DriverError::Session(format!("loop resume failed: {e}"))
                                })?;
                            ServicedSuspension::Resumed(GreenReady {
                                chain,
                                outcome: next,
                            })
                        }
                        // The AUTHORED loop itself evaluated `askUser`/`note`
                        // (`Tidepool.Form`, auto-imported because `AskUser` is in
                        // `outer_decls`) — a form or narration raised DIRECTLY by
                        // the loop, distinct from an answerer's own
                        // (`service_askuser_hole`). Service it via the same
                        // operator gate and resume the OUTER session; the helper
                        // loops over `askUser`'s Haskell-side decode-retry (a bad
                        // submission re-suspends on a fresh `AskUserWith`) and any
                        // interleaved `note`, returning the first outcome that
                        // ISN'T another operator form/note — a `runLLMTurn`
                        // suspension the main loop then services, or a completion.
                        SuspensionRouting::AskUser { .. } | SuspensionRouting::Note { .. } => {
                            let next = self
                                .service_outer_askuser_hole(
                                    hole.clone(),
                                    classified.routing.clone(),
                                    &compiled,
                                )
                                .await?;
                            ServicedSuspension::Resumed(GreenReady {
                                chain,
                                outcome: next,
                            })
                        }
                        // Reached whenever `ready` is a MIX of Subagent- and
                        // other-routed items (the batch gate at the top of
                        // this loop fires only once EVERY ready item is
                        // Subagent-routed — see that gate's own doc for why):
                        // this single suspension is serviced individually,
                        // same shape as before generalization. A `spawnAsync`
                        // popped here is fine either way — it never blocks
                        // meaningfully long — and once every sibling's own
                        // `Green` spawn step has run, any remaining
                        // (including slow `awaitAgent`) requests converge on
                        // the batch gate above instead of this arm.
                        SuspensionRouting::Subagent => {
                            let value = self
                                .service_outer_subagent(&request, &compiled.table, FormSource::OuterLoop)
                                .await?;
                            let sid = self.outer_sid()?;
                            let next = self
                                .agent
                                .with_session(sid, |s| s.resume(hole, value))
                                .map_err(|e| DriverError::Session(e.to_string()))?
                                .map_err(|e| {
                                    DriverError::Session(format!("subagent resume failed: {e}"))
                                })?;
                            ServicedSuspension::Resumed(GreenReady {
                                chain,
                                outcome: next,
                            })
                        }
                        // Console/Worktree/RepoEvent/Exec / Journal
                        // (run-journal lane) — same suspension-servicing
                        // shape as Subagent above, generalized over
                        // `OuterEffectKind`.
                        SuspensionRouting::OuterEffect(kind) => {
                            let kind = *kind;
                            // `RepoEventAwait` is the
                            // ONE outer-row suspension whose handler-side
                            // implementation can BLOCK indefinitely
                            // (`repo_event_await`'s own reconcile/check/sleep
                            // loop). Routing it through the ordinary
                            // dispatch-then-resume path below would stall
                            // THIS WHOLE ready-queue loop — every other
                            // chain's already-ready work, including a
                            // sibling green thread's — until it happens to
                            // match. Service it non-blockingly instead: poll
                            // with the SAME handler's plain (non-sleeping)
                            // drain; a match resumes the hole immediately,
                            // exactly as if `RepoEventAwait` itself had
                            // returned it; an EMPTY batch leaves the hole
                            // genuinely parked — `ServicedSuspension::LeaveParked`,
                            // reinserted at the BACK of `ready` so every other
                            // already-ready chain runs first, revisited by
                            // this same arm on a later iteration. The
                            // caller-observable contract (block until a
                            // match) is unchanged — only who does the waiting
                            // is. `repo_event_await`'s own implementation,
                            // and every other RepoEvent verb, are untouched:
                            // this is a DRIVER SERVICING CHOICE, made only
                            // for this one constructor.
                            if kind == engine::OuterEffectKind::RepoEvent
                                && engine::con_name(&request, &compiled.table)
                                    == Some("RepoEventAwait")
                            {
                                match self.poll_repo_event_await(&request, &compiled.table)? {
                                    None => {
                                        // Nothing else ready to interleave
                                        // with right now — a real sibling
                                        // wakes this up on the very next
                                        // iteration regardless (it's already
                                        // ahead in the queue), so this only
                                        // ever fires while genuinely waiting
                                        // on external progress (a deadline
                                        // elapsing, another chain not yet
                                        // scheduled). Bounds the poll to a
                                        // cooperative cadence instead of a
                                        // tight CPU spin; `tokio::time::sleep`
                                        // yields this task rather than
                                        // blocking the runtime.
                                        if ready.is_empty() {
                                            tokio::time::sleep(std::time::Duration::from_millis(
                                                5,
                                            ))
                                            .await;
                                        }
                                        ServicedSuspension::LeaveParked(GreenReady {
                                            chain,
                                            outcome: ResidentOutcome::Suspended {
                                                output: Vec::new(),
                                                hole,
                                                request,
                                            },
                                        })
                                    }
                                    Some(value) => {
                                        let sid = self.outer_sid()?;
                                        let next = self
                                            .agent
                                            .with_session(sid, |s| s.resume(hole, value))
                                            .map_err(|e| DriverError::Session(e.to_string()))?
                                            .map_err(|e| {
                                                DriverError::Session(format!(
                                                    "RepoEventAwait resume failed: {e}"
                                                ))
                                            })?;
                                        ServicedSuspension::Resumed(GreenReady {
                                            chain,
                                            outcome: next,
                                        })
                                    }
                                }
                            } else {
                                let value =
                                    self.service_outer_effect(kind, &request, &compiled.table)?;
                                let sid = self.outer_sid()?;
                                let next = self
                                    .agent
                                    .with_session(sid, |s| s.resume(hole, value))
                                    .map_err(|e| DriverError::Session(e.to_string()))?
                                    .map_err(|e| {
                                        DriverError::Session(format!(
                                            "outer effect resume failed: {e}"
                                        ))
                                    })?;
                                ServicedSuspension::Resumed(GreenReady {
                                    chain,
                                    outcome: next,
                                })
                            }
                        }
                        // `Tidepool.Async`'s substrate — raised
                        // either by the loop itself or by a green thread's own
                        // body. Unlike every other arm here, servicing may push
                        // ZERO, ONE, or TWO ready items (a park with no terminal
                        // candidate pushes none; a spawn pushes both the resumed
                        // spawner and the freshly started thread) and mutates the
                        // thread table / waiter map — it does not fit
                        // `ServicedSuspension` (see that type's doc), so it owns
                        // `ready` directly and this arm hands nothing back.
                        SuspensionRouting::Green => {
                            // Raw delivery never reports node-blocked. The
                            // AUTHORED plane keeps misuse a hard error —
                            // authored code fails loud, it is not coached.
                            if let GreenHoleServiced::Misuse(msg) = self
                                .service_green_hole(
                                    None,
                                    chain,
                                    hole.cont_id(),
                                    &request,
                                    &compiled.table,
                                    &mut threads,
                                    &mut waiters,
                                    &mut next_tid,
                                    &mut ready,
                                    GreenDelivery::Raw,
                                )
                                .await?
                            {
                                return Err(DriverError::Session(msg));
                            }
                            continue;
                        }
                        // `runLLMTurnFork @T`/`runLLMTurnFanout @T` raised
                        // DIRECTLY by the AUTHORED loop (`RunLLMTurn`'s own
                        // fork/fanout payload — reachable wherever
                        // `RunLLMTurn` is in the row, so the outer session
                        // needs no separate `Fork` decl): S1-L4 — service
                        // every prompt CONCURRENTLY, each in its own
                        // freshly-minted answerer realm, then resume this
                        // ONE hole once with the assembled answer.
                        // `source` is unread here: the outer row is
                        // `outer_decls()`, which has no `Fork` effect, so the
                        // only verb that can raise this routing on the outer
                        // session is `runLLMTurnFork`/`runLLMTurnFanout` —
                        // `ForkSource::RunLLMTurn` by construction.
                        SuspensionRouting::Fork {
                            site,
                            ty,
                            fan,
                            prompts,
                            source: _,
                        } => {
                            let value = self
                                .service_outer_fanout(
                                    site.get(),
                                    ty.as_deref(),
                                    compiled.asks.modules_of(site.get()),
                                    *fan,
                                    &classified.prompt,
                                    prompts,
                                    &compiled.table,
                                )
                                .await?;
                            let sid = self.outer_sid()?;
                            let next = self
                                .agent
                                .with_session(sid, |s| s.resume(hole, value))
                                .map_err(|e| DriverError::Session(e.to_string()))?
                                .map_err(|e| {
                                    DriverError::Session(format!("fanout resume failed: {e}"))
                                })?;
                            ServicedSuspension::Resumed(GreenReady {
                                chain,
                                outcome: next,
                            })
                        }
                        other => {
                            break Err(DriverError::Session(format!(
                                "outer loop suspended on an unserviceable hole ({other:?}) — \
                                 the Harness monad exposes runLLMTurn, askUser, note, \
                                 spawnAgent, say, \
                                 createWorktree/lookupWorktree/listWorktrees/worktreeBranch/\
                                 worktreeHead, withHandler (repository events), run/runIn/\
                                 runArgv, record, and Tidepool.Async's async/wait/waitEither/cancel \
                                 only"
                            )))
                        }
                    }
                }
            };
            match serviced {
                ServicedSuspension::Completed { result, table } => break Ok((result, table)),
                ServicedSuspension::Resumed(gr) | ServicedSuspension::LeaveParked(gr) => {
                    ready.push_back(gr);
                }
            }
        }
        }
        .await;

        // Structured-concurrency scope exit: every thread this loop spawned
        // is scoped to this ONE `loop` fragment run — close every realm that
        // is still open so its frames/handles don't outlive the cycle that
        // created them. That is Running threads (never joined/cancelled by
        // the authored code) AND Settled ones: a settle parks the thread's
        // `AsyncDoneWith` frame FOREVER by design (the arm never resumes
        // it), so a settled realm left unclosed is a permanently-parked hole
        // on the shared session — enough of them and the machine is never
        // quiescent again, which blocks rotation until the fragment ceiling
        // kills the run. Only a Cancelled thread's realm is already closed
        // (the cancel arm does it eagerly).
        if let Ok(sid) = self.outer_sid() {
            for entry in threads.values() {
                if !matches!(entry.state, GreenThreadState::Cancelled) {
                    let _ = self.agent.with_session(sid, |s| s.close_realm(entry.realm));
                }
            }
        }
        outcome_result
    }

    /// Evaluate `render(state)` against the outer session, then compose the
    /// full system message the answerer works under — author output first,
    /// then the prior compaction summary (if any), then the loop-iteration
    /// count. This composed text becomes `prompt_before`/`prompt_after`; it
    /// carries NO effects section of its own — the OUTER loop's own
    /// Available-effects section (folded over [`outer_decls`]) is never
    /// shown to the nested answerer, which sees only its own row's section
    /// (appended by the caller that builds `self.answerer_framing`, via
    /// [`typed_request_agent_framing_suffix`]). `render` itself takes only `State`
    /// — runtime context is the runtime's job, so the compaction summary and the
    /// iteration count are runtime facts the AUTHOR no longer states.
    /// Runtime-invoked at loop boundaries ONLY.
    /// `state_json` is `None` only for the very first cycle — then the render
    /// splice references `Loaded.initialState` directly (no JSON to decode),
    /// per [`state_cross::state_in`]. `last_compaction` is the
    /// runtime-carried summary to compose in (`self.last_compaction`, not
    /// itself decoded from any Haskell splice); `self.iteration` supplies the
    /// loop count.
    pub fn render_framing(
        &mut self,
        state_json: Option<&Json>,
        last_compaction: Option<&str>,
    ) -> Result<String, DriverError> {
        let state_decl = state_cross::state_in(state_json);
        let code = format!(
            "pure ({q}.render __selfHarnessState)",
            q = state_cross::LOADED_QUALIFIER
        );
        let compiled = self.compile_outer(&code, &state_decl, "render")?;
        self.render_framing_with(&compiled, last_compaction)
    }

    /// The shared run-and-compose tail of [`Self::render_framing`]: run an
    /// ALREADY-COMPILED `render` entry against the outer session, then
    /// compose the prior compaction summary and the loop-iteration count onto
    /// its `Text` result. Split out so [`Self::compile_loop_entry`]'s fused
    /// render entry runs through the exact same compose logic
    /// [`Self::render_framing`] uses standalone, rather than a second copy.
    pub(crate) fn render_framing_with(
        &mut self,
        compiled: &CompiledTurn,
        last_compaction: Option<&str>,
    ) -> Result<String, DriverError> {
        let sid = self.outer_sid()?;
        let outcome = self
            .agent
            .with_session(sid, |s| s.run("render", &compiled.expr, &compiled.table))
            .map_err(|e| DriverError::Session(e.to_string()))?
            .map_err(|e| map_run_error("render run failed", e.to_string()))?;
        let author_text = match outcome {
            ResidentOutcome::Completed { result, .. } => match result.to_json() {
                Json::String(s) => s,
                other => {
                    return Err(DriverError::Session(format!(
                        "render did not yield Text, got {other:?}"
                    )))
                }
            },
            ResidentOutcome::Suspended { .. } => {
                return Err(DriverError::Session(
                    "render suspended unexpectedly — render must be a pure function".into(),
                ))
            }
        };

        let mut framing = author_text;
        if let Some(summary) = last_compaction {
            framing.push_str("\n\nSummary of the prior context (compacted):\n");
            framing.push_str(summary);
        }
        framing.push_str(&format!("\n\nLoop count so far: {}.", self.iteration));
        if let Some(msg) = self.pending_operator_input.take() {
            framing.push_str("\n\nTHE OPERATOR SAID (between loops, addressed to you): ");
            framing.push_str(&msg);
        }
        if let Some(lost) = self.last_rotation_losses.take() {
            framing.push_str(
                "\n\nNOTE: the resident machine was rotated (bounded-lifetime \
                 maintenance). Durable state survived via the checkpoint; living \
                 session values did NOT: ",
            );
            framing.push_str(&if lost.is_empty() {
                "(none were held)".to_string()
            } else {
                lost.join(", ")
            });
        }
        Ok(framing)
    }

    /// Runtime-owned MID-LOOP emergency compaction with IN-PLACE relief: the
    /// *runtime* owns this trigger, never the loop, and it replaces the
    /// answerer's context with the summary so the loop CONTINUES — never a
    /// loop-abort. Called between the answerer's holes
    /// ([`Self::run_loop_fragment_inner`]).
    ///
    /// Watches the CURRENT loop's answerer session's REAL context size —
    /// [`Harness::node_last_input_tokens`], the LAST turn's `input_tokens`
    /// high-water mark (NOT [`Harness::node_usage`]'s summed
    /// `input_tokens`, which super-linearly over-counts across a multi-round
    /// hole because each round's provider `input_tokens` already re-includes
    /// the whole re-sent transcript) — against the threshold
    /// (`self.compaction_threshold_percent` of [`EngineConfig::context_window_tokens`],
    /// default ~80%). Under threshold, or with the budget disabled (`None`) or
    /// no live answerer, it is a no-op.
    ///
    /// Past threshold it (the SIMPLE mechanism — no separate node, no
    /// `finalize`, no transcript serialized into a prompt; the answerer already
    /// HAS the full context):
    /// 1. Pushes ONE plain "summarize everything above" turn onto the EXISTING
    ///    answerer session and captures the model's prose reply
    ///    ([`Harness::summarize_turn`]) — that model call counts against the
    ///    per-loop [`LOOP_INFERENCE_CALL_CAP`].
    /// 2. Replaces the answerer's context with `[system + summary]` IN PLACE
    ///    ([`Harness::replace_transcript_with_summary`]) — the loop's remaining
    ///    holes continue under the smaller window.
    /// 3. Records the summary as `self.cycle_compaction` (this cycle's, for
    ///    [`LoopIterationOutcome::compaction`]) and `self.last_compaction` (carried to
    ///    the NEXT [`Self::render_framing`] call to compose in, and
    ///    persisted for restart durability).
    /// 4. Emits [`Event::CompactionTrigger`] with its payload (summary, pre/post
    ///    context size, node).
    pub(crate) async fn maybe_compact_answerer(&mut self) -> Result<(), DriverError> {
        let Some(budget) = self.agent.cfg().context_window_tokens else {
            return Ok(());
        };
        let Some(lease) = self.answerer else {
            return Ok(());
        };
        let answerer = lease.node();
        let Some(context_tokens) = self.agent.node_last_input_tokens(answerer) else {
            return Ok(());
        };
        let threshold = (u64::from(budget) * self.compaction_threshold_percent) / 100;
        if context_tokens < threshold {
            return Ok(());
        }

        self.lifecycle = SelfHarnessState::Compacting;

        // The summarize turn is a real model call — count it against the
        // per-loop inference cap before driving it, exactly like an answerer
        // round, so compaction can never escape the 1024-call runaway guard.
        let cap = self.loop_inference_call_cap;
        if self.loop_inference_calls.load(Ordering::SeqCst) >= cap {
            return Err(DriverError::Session(format!(
                "per-loop inference-call cap ({cap}) reached during \
                 compaction — hard-stopping the loop (a runaway harness)"
            )));
        }
        self.loop_inference_calls.fetch_add(1, Ordering::SeqCst);

        // The target is a fraction of the budget, but clamp it against the
        // REAL current window (`context_tokens`) so a summary is never asked to
        // GROW context — under a low test threshold (or a tiny window) the flat
        // budget/DIVISOR could exceed what is actually there. Target strictly
        // below the current size keeps compaction a genuine reduction.
        let budget_target = u64::from(budget / COMPACTION_TARGET_DIVISOR);
        let target = budget_target.min(context_tokens.saturating_sub(1)).max(1);
        let prompt = format!(
            "This conversation has grown to roughly {context_tokens} tokens against a \
             {budget}-token context-window budget — it is time to compact before \
             continuing. Summarize EVERYTHING above (the whole conversation so far) \
             into a compact form you can continue from: a prose summary of what this \
             loop's work has accomplished and learned, targeting roughly {target} \
             tokens, preserving the load-bearing facts and decisions. Your reply will \
             REPLACE the detailed transcript above, so write it as the context you \
             will carry forward. Reply with the summary text directly (no code block)."
        );

        // The SIMPLE mechanism: one ordinary turn on the EXISTING answerer
        // session, which already holds the full context — no second node, no
        // `finalize`, no hand-serialized transcript. The model summarizes
        // itself, so it cannot confabulate.
        let (summary, post_usage) = self.agent.summarize_turn(answerer, &prompt).await?;
        let summary = summary.trim().to_string();

        // In-place relief: the answerer's context becomes `[system + summary]`,
        // so its remaining holes drive under the smaller window (loop CONTINUES).
        self.agent
            .replace_transcript_with_summary(answerer, &summary)?;

        // The trigger event carries what compaction produced — the
        // summary, the pre/post context size, and the node it fired on.
        self.emit(Event::CompactionTrigger {
            node: answerer,
            summary: summary.clone(),
            pre_input_tokens: context_tokens,
            post_input_tokens: post_usage.input_tokens,
        });

        self.cycle_compaction = Some(summary.clone());
        self.set_last_compaction(summary)?;
        self.lifecycle = SelfHarnessState::RunningLoop;
        Ok(())
    }

    /// Record `summary` as the latest compaction (`self.last_compaction`,
    /// composed into the next [`Self::render_framing`] call) — in-memory
    /// only. The loop
    /// CONTINUES under this summary immediately, but it does not reach disk
    /// on its own: [`Self::commit_checkpoint`] picks up whatever
    /// `self.last_compaction` holds at the cycle's own commit boundary, so a
    /// crash between a mid-loop compaction and that commit restores the
    /// PRIOR generation's summary, never a summary paired with a state it
    /// was never produced alongside.
    pub(crate) fn set_last_compaction(&mut self, summary: String) -> Result<(), DriverError> {
        self.last_compaction = Some(summary);
        Ok(())
    }
}
