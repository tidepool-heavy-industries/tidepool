//! Rung 5 of the acceptance ladder ("retiring an incarnation releases its
//! parked frame; a different incarnation cannot resume it"), proven through
//! `tidepool-actor`'s own `ActorRunTarget::retire_placement` seam against a
//! real `ResidentSession` on both engines.
//!
//! KNOWN GAP (Prepared engine only): `ResidentSession::close_realm`
//! (`tidepool-runtime/src/session/resident.rs`, `pub fn close_realm` around
//! line 1319) reads the machine only through `PersistentSession::machine_mut`
//! (`tidepool-runtime/src/session/persistent.rs` around line 426), which
//! resolves via `ResidentEngine::core_mut` and is `None` whenever the
//! session's engine is `ResidentEngine::Prepared` (the Prepared engine's own
//! accessor is the DIFFERENT method `PersistentSession::prepared_mut`,
//! `persistent.rs` around line 430, which `close_realm` never calls). So on
//! the Prepared route, `close_realm` hits its early `None` branch and returns
//! `(0, 0)` unconditionally -- `tidepool_codegen::prepared_program::
//! PreparedMachine::close_realm` (which DOES correctly release a realm's
//! parked frames and handles, see `prepared.rs`'s `PreparedEngine::close_realm`
//! at line 1248, itself forwarding to the codegen machine) is never invoked.
//! `PlacementRetirement::frames`/`handles` are therefore always `0` on the
//! Prepared route today, whatever a placement's resource realm actually held,
//! and a placement's parked hole is NOT actually released by retirement on
//! that route. This file demonstrates and documents that gap on the Prepared
//! engine rather than hiding it behind a weakened assertion with no
//! explanation, and shows the Core route (unaffected -- `close_realm`'s
//! `machine_mut()` call DOES resolve for `ResidentEngine::Core`) behaving to
//! the originally intended contract.
//!
//! ADDITIONAL FINDING, same shape, different symptom:
//! `ResidentSession::parked_realm` reads `PersistentSession::machine()`
//! (`persistent.rs` around line 422), the read-only sibling of the
//! `machine_mut()` behind the gap above -- also Core-only, also
//! unconditionally `None` on the Prepared route. So this file never uses
//! `parked_realm` to observe a Prepared-route hole's liveness; it uses
//! `ResidentSession::parked_holes()` (session-level bookkeeping the engine
//! gap does not touch) as its engine-agnostic "is this hole still parked"
//! observable instead. `suspend_ask_as` below checks realm attribution via
//! `parked_realm` on Core only, documenting why.
//!
//! This file does NOT touch `tidepool-runtime/src/session/resident.rs` (out
//! of this task's scope) -- it only documents the gap through an honest test
//! on the Prepared route, and exercises the parts of the acceptance ladder
//! that remain true on both routes regardless of the gap:
//!
//! - two placements (their own resource realm + lexical scope) can be set up
//!   on one shared `ResidentSession`, each parking its own suspension via a
//!   real ask turn (mirrors `tidepool-runtime/tests/prepared_turn.rs`'s
//!   `Notebook::suspend_ask` pattern -- this needs the Haskell extractor,
//!   `tidepool_testing::eval_harness::require_extract()`, exactly as that
//!   file does);
//! - `session.parked_realm(&hole)` attributes each hole to its OWN realm
//!   before either retirement;
//! - `retirement.leases` is `0` on both engines (hardcoded in
//!   `impl ActorRunTarget for ResidentSession` -- never wired to a real
//!   leasing mechanism on either route, so this is not part of the gap);
//! - `retirement.scope_roots` (from `retire_scope`, a value-plane mechanism
//!   entirely independent of `close_realm`'s realm-plane mechanism) is
//!   correct on BOTH engines: retiring incarnation 1's placement releases
//!   exactly the one persistent root incarnation 1's own bind installed,
//!   and incarnation 2's own bound value is untouched;
//! - the session remains usable (a further turn still runs) after
//!   `retire_placement`, even on the Prepared route where it under-released;
//! - incarnation 2's still-parked hole resumes to completion regardless of
//!   incarnation 1's retirement (this never depended on the gap).
//!
//! The request-layer half of rung 5 ("a stale incarnation is refused") is
//! NOT exercised here: `tidepool-actor::request::RequestRegistry` and its
//! `reserve`/`mark_queued`/`present`/`begin_reply` methods are all
//! `pub(crate)`, unreachable from a `tests/` integration crate, and nothing
//! else in `tidepool-actor`'s public surface constructs a request against a
//! target actor. Exercising that half would need either a `pub(crate)`
//! visibility widening (out of this task's scope) or driving the whole
//! `ResidentActorWorkbench`/kernel actor-mailbox path, far more than a
//! standalone integration test's "small helpers" are meant to carry. This
//! file asserts the machine/session-layer half only.

use std::path::{Path, PathBuf};

use tidepool_actor::{ActorId, ActorPlacement, ActorRef, ActorRunTarget, Incarnation};
use tidepool_effect::{EffectRunPolicy, LivePayloadPolicy};
use tidepool_repr::{Generation, PrincipalId};
use tidepool_runtime::session::{
    resident_workbench_templates, run_turn, BoundBinder, EngineKind, ResidentHole, ResidentOutcome,
    ResidentSession, SessionRunContext, TurnRequest, TurnResult, TurnTemplate,
};
use tidepool_testing::eval_harness;

/// `tidepool-runtime/tests/prepared_turn.rs`'s own `Notebook`, trimmed to
/// what this file needs and extended to run turns under an EXPLICIT actor
/// placement (rather than the session's default `SessionRunContext::ROOT`)
/// selected right before each turn actually executes -- exactly the shape
/// `resident_workbench.rs`'s own per-turn actor dispatch uses (compile is
/// session-context-free; only running/binding a compiled turn consults
/// `ResidentSession::run_context`).
struct Harness {
    session: ResidentSession<frunk::HNil, tidepool_mcp::CapturedOutput>,
    preamble: String,
    effect_stack: String,
    include: Vec<PathBuf>,
    root: tempfile::TempDir,
    generation: u64,
    last_table: Option<tidepool_repr::DataConTable>,
    engine: EngineKind,
}

impl Harness {
    fn new(engine: EngineKind) -> Self {
        eval_harness::require_extract();
        let decls = tidepool_mcp::standard_decls();
        let preamble = tidepool_mcp::build_preamble(&decls, false);
        let effect_stack = tidepool_mcp::build_effect_stack_type(&decls);
        let mut include = eval_harness::effects_include().to_vec();
        include.push(eval_harness::prelude_path());
        let session = ResidentSession::unbootstrapped_on(
            engine,
            frunk::HNil,
            tidepool_mcp::CapturedOutput::new(),
            include.clone(),
            tidepool_runtime::DEFAULT_NURSERY_SIZE,
            None,
        );
        Self {
            session,
            preamble,
            effect_stack,
            include,
            root: tempfile::tempdir().expect("session root"),
            generation: 0,
            last_table: None,
            engine,
        }
    }

    fn constructor(&self, name: &str) -> tidepool_repr::DataConId {
        self.last_table
            .as_ref()
            .expect("an expression turn ran")
            .get_by_name(name)
            .unwrap_or_else(|| panic!("constructor {name} is not in the session table"))
    }

    fn templates(&self) -> Vec<TurnTemplate> {
        resident_workbench_templates(&self.preamble, &self.effect_stack, "")
    }

    fn compile(&mut self, text: &str) -> TurnResult {
        self.generation += 1;
        let retained = self.session.prepared_retained();
        let templates = self.templates();
        let include: Vec<&Path> = self.include.iter().map(PathBuf::as_path).collect();
        run_turn(TurnRequest {
            turn_text: text,
            templates: &templates,
            include: &include,
            session_root: self.root.path(),
            inject_modules: &[],
            gen: self.generation,
            verdict: None,
            target: None,
            prepared: self.session.prepared_turn_request(&retained),
        })
        .unwrap_or_else(|failure| {
            panic!(
                "{text:?} failed to compile: {}\n{}",
                tidepool_runtime::classify_compile(&failure.error).message,
                failure
                    .attempted_source
                    .as_deref()
                    .unwrap_or("<no attempted source>")
            )
        })
    }

    /// Run an expression turn under `context` and render its result as JSON
    /// -- used once as a warm-up (under `SessionRunContext::ROOT`) to seed
    /// the `True` constructor into the session's shared `DataConTable`
    /// before either placement asks anything.
    fn expression(&mut self, context: SessionRunContext, text: &str) -> serde_json::Value {
        self.session
            .set_run_context(context)
            .expect("context's lexical scope is live");
        let TurnResult::Expr { compiled, .. } = self.compile(text) else {
            panic!("{text:?} did not classify as an expression");
        };
        let outcome = self
            .session
            .run_with_sites("harness_expression", compiled.code())
            .unwrap_or_else(|error| panic!("{text:?} failed to run: {error}"));
        let ResidentOutcome::Completed { result, .. } = outcome else {
            panic!("{text:?} did not complete: {outcome:?}");
        };
        self.last_table = Some(result.table().clone());
        tidepool_runtime::value_to_json(result.value(), result.table(), 0)
    }

    /// Run a single-binder bind turn to completion under `placement` --
    /// installed through `ActorRunTarget::install_actor_execution`, the same
    /// seam `retire_placement` is on the other end of.
    fn bind_as(&mut self, actor: ActorRef, placement: ActorPlacement, text: &str) -> BoundBinder {
        ActorRunTarget::install_actor_execution(
            &mut self.session,
            SessionRunContext::new(
                placement.resource_scope,
                placement.lexical_scope,
                PrincipalId::from(actor),
            ),
            EffectRunPolicy::HandleOrSuspend,
            LivePayloadPolicy::default(),
        )
        .expect("placement's lexical scope is live");
        let TurnResult::Bind {
            bound, compiled, ..
        } = self.compile(text)
        else {
            panic!("{text:?} did not classify as a bind");
        };
        let [binder] = bound.as_slice() else {
            panic!("{text:?} bound {} names", bound.len());
        };
        let outcome = self
            .session
            .run_bind_with_sites(
                "harness_bind",
                compiled.code(),
                binder,
                Generation(self.generation),
            )
            .unwrap_or_else(|error| panic!("{text:?} failed to run: {error}"));
        assert!(
            matches!(outcome, ResidentOutcome::Completed { .. }),
            "{text:?} did not complete: {outcome:?}"
        );
        binder.clone()
    }

    /// Run the ask turn `b <- runLLMTurn @Bool "q"` under `placement` to its
    /// suspension -- `tidepool-runtime/tests/prepared_turn.rs`'s
    /// `Notebook::suspend_ask`, parameterized on the actor placement it
    /// suspends under instead of the session's default root context.
    fn suspend_ask_as(
        &mut self,
        actor: ActorRef,
        placement: ActorPlacement,
        binder_name: &str,
    ) -> (BoundBinder, ResidentHole) {
        ActorRunTarget::install_actor_execution(
            &mut self.session,
            SessionRunContext::new(
                placement.resource_scope,
                placement.lexical_scope,
                PrincipalId::from(actor),
            ),
            EffectRunPolicy::HandleOrSuspend,
            LivePayloadPolicy::default(),
        )
        .expect("placement's lexical scope is live");
        let TurnResult::Bind {
            bound, compiled, ..
        } = self.compile(&format!(
            "{binder_name} <- (runLLMTurn @Bool \"q\" :: M Bool)"
        ))
        else {
            panic!("the {binder_name} ask did not classify as a bind");
        };
        let [binder] = bound.as_slice() else {
            panic!("the {binder_name} ask bound {} names", bound.len());
        };
        assert_eq!(binder.name, binder_name);
        let outcome = self
            .session
            .run_bind_with_sites(
                "harness_ask",
                compiled.code(),
                binder,
                Generation(self.generation),
            )
            .unwrap_or_else(|error| panic!("the {binder_name} ask failed to run: {error}"));
        let ResidentOutcome::Suspended { hole, .. } = outcome else {
            panic!("the {binder_name} ask did not suspend: {outcome:?}");
        };
        assert!(
            self.session.parked_holes().contains(&hole.cont_id()),
            "the {binder_name} ask must leave its hole in the session's parked set"
        );
        // ADDITIONAL FINDING (not this file's headline gap, but the same
        // shape): `ResidentSession::parked_realm` reads
        // `PersistentSession::machine()`, which -- exactly like the
        // `machine_mut()` this file's module doc traces through
        // `close_realm` -- resolves only for `ResidentEngine::Core` and is
        // unconditionally `None` on the Prepared route, whatever the hole's
        // actual realm. So realm-attribution is checked here only on Core;
        // `parked_holes()` membership (checked above, and engine-agnostic
        // since it reads the session's own bookkeeping rather than the
        // per-engine machine) is this file's cross-engine observable for
        // "is this hole still parked".
        if self.engine == EngineKind::Core {
            assert_eq!(
                self.session.parked_realm(&hole),
                Some(placement.resource_scope),
                "the {binder_name} ask's hole must be attributed to its own placement's realm"
            );
        }
        (binder.clone(), hole)
    }
}

/// Rung 5, run on one engine: two placements of the same actor lineage each
/// park their own ask, incarnation 1's placement is retired through
/// `ActorRunTarget::retire_placement`, and incarnation 2 is confirmed
/// untouched and still resumable to completion.
fn two_placements_one_retired(engine: EngineKind) {
    let mut harness = Harness::new(engine);

    // Warm-up: seed `True`/`False` into the shared session constructor
    // table, under the session's default root context (nothing actor-scoped
    // yet).
    let rendered = harness
        .expression(SessionRunContext::ROOT, "not False")
        .to_string();
    assert!(
        rendered.contains("true"),
        "{engine:?}: warm-up rendered {rendered}"
    );
    let true_id = harness.constructor("True");

    let actor_id = ActorId(77);
    let incarnation_1 = ActorRef {
        id: actor_id,
        incarnation: Incarnation::FIRST,
    };
    let incarnation_2 = ActorRef {
        id: actor_id,
        incarnation: Incarnation(2),
    };
    assert_ne!(
        incarnation_1, incarnation_2,
        "a retirement must be able to distinguish these two incarnations"
    );

    let session_id = tidepool_repr::SessionId(0);

    let realm_1 = tidepool_codegen::suspension::RealmId::fresh();
    let scope_1 = harness.session.mint_isolated_scope();
    let placement_1 = ActorPlacement {
        session: session_id,
        resource_scope: realm_1,
        lexical_scope: scope_1,
    };

    let realm_2 = tidepool_codegen::suspension::RealmId::fresh();
    let scope_2 = harness.session.mint_isolated_scope();
    let placement_2 = ActorPlacement {
        session: session_id,
        resource_scope: realm_2,
        lexical_scope: scope_2,
    };
    assert_ne!(
        placement_1.resource_scope, placement_2.resource_scope,
        "each incarnation owns its own resource realm"
    );
    assert_ne!(
        placement_1.lexical_scope, placement_2.lexical_scope,
        "each incarnation owns its own lexical scope"
    );

    // Incarnation 1: one bound value (source of a nonzero `scope_roots` at
    // retirement) plus one parked ask (source of the frame/handle counts the
    // gap concerns).
    let roots_before_bind = harness.session.persistent_roots_count();
    harness.bind_as(incarnation_1, placement_1, "x <- pure (1 :: Int)");
    let roots_after_bind = harness.session.persistent_roots_count();
    assert!(
        roots_after_bind > roots_before_bind,
        "{engine:?}: binding x under placement 1 must add at least one persistent root"
    );
    let (_, hole_1) = harness.suspend_ask_as(incarnation_1, placement_1, "p");

    // Incarnation 2: its own unrelated parked ask, standing in for a sibling
    // incarnation that must survive incarnation 1's later retirement.
    let (_, hole_2) = harness.suspend_ask_as(incarnation_2, placement_2, "q");

    assert!(
        harness.session.parked_holes().contains(&hole_1.cont_id()),
        "{engine:?}: hole_1 must be parked before retirement"
    );
    assert!(
        harness.session.parked_holes().contains(&hole_2.cont_id()),
        "{engine:?}: hole_2 must be parked before retirement"
    );
    // ==== retire incarnation 1's placement through `ActorRunTarget` =======
    let retirement = ActorRunTarget::retire_placement(
        &mut harness.session,
        placement_1.resource_scope,
        placement_1.lexical_scope,
    );

    // `leases` is hardcoded 0 in `impl ActorRunTarget for ResidentSession`
    // for BOTH engines -- never wired to a real leasing mechanism on either
    // route, so this is not part of the `close_realm` gap.
    assert_eq!(retirement.leases, 0, "{engine:?}");

    // `scope_roots` is `retire_scope`'s own receipt -- a value-plane
    // mechanism entirely independent of `close_realm`'s realm-plane one, and
    // NOT affected by the gap on either engine: incarnation 1's own `x`
    // binding is released.
    assert_eq!(
        retirement.scope_roots, 1,
        "{engine:?}: retiring placement 1 must release exactly x's one root"
    );

    match engine {
        EngineKind::Core => {
            // Core is NOT affected by the gap: `close_realm`'s
            // `self.core.machine_mut()` call resolves to `Some` for
            // `ResidentEngine::Core`, so `PlacementRetirement` reports the
            // originally intended contract.
            assert_eq!(
                retirement.frames, 1,
                "{engine:?}: retiring placement 1 must release its one parked frame"
            );
            assert_eq!(retirement.handles, 0, "{engine:?}");
            assert!(
                !harness.session.parked_holes().contains(&hole_1.cont_id()),
                "{engine:?}: retiring placement 1 released its parked continuation"
            );
        }
        EngineKind::Prepared => {
            // KNOWN GAP: see the module doc and
            // `tidepool-runtime/src/session/resident.rs`'s `close_realm`
            // (around line 1319) / `tidepool-runtime/src/session/
            // persistent.rs`'s `machine_mut` (around line 426). On this
            // route `close_realm` never reaches
            // `PreparedEngine::close_realm`/`PreparedMachine::close_realm`
            // at all, so `frames`/`handles` read 0 here rather than
            // reflecting anything actually released, and hole_1 is NOT
            // actually released.
            assert_eq!(
                retirement.frames, 0,
                "{engine:?}: KNOWN GAP -- close_realm does not forward to the Prepared engine, \
                 so this reads 0 rather than reflecting anything actually released"
            );
            assert_eq!(
                retirement.handles, 0,
                "{engine:?}: KNOWN GAP -- same as above; the prepared engine's parked \
                 continuation was never released because close_realm never forwarded the call"
            );
            assert!(
                harness.session.parked_holes().contains(&hole_1.cont_id()),
                "{engine:?}: KNOWN GAP -- retire_placement did NOT release hole_1's parked \
                 continuation on the Prepared route; a reader must not mistake this for the \
                 intended contract"
            );
        }
    }

    // Incarnation 2's placement is untouched by incarnation 1's retirement,
    // on EITHER engine (this half of rung 5 does not depend on the gap).
    assert!(
        harness.session.parked_holes().contains(&hole_2.cont_id()),
        "{engine:?}: incarnation 2's placement is untouched by incarnation 1's retirement"
    );

    // The session remains usable after `retire_placement`, even on the
    // Prepared route where it under-released.
    let rendered = harness
        .expression(SessionRunContext::ROOT, "41 + 1")
        .to_string();
    assert!(
        rendered.contains("42"),
        "{engine:?}: the session must still run turns after retire_placement, got {rendered}"
    );

    // Incarnation 2's own still-parked hole resumes to completion regardless
    // of incarnation 1's retirement -- this never depended on the gap.
    ActorRunTarget::install_actor_execution(
        &mut harness.session,
        SessionRunContext::new(
            placement_2.resource_scope,
            placement_2.lexical_scope,
            PrincipalId::from(incarnation_2),
        ),
        EffectRunPolicy::HandleOrSuspend,
        LivePayloadPolicy::default(),
    )
    .expect("placement 2's lexical scope is still live");
    let outcome = harness
        .session
        .resume(hole_2, tidepool_bridge::Value::Con(true_id, Vec::new()))
        .unwrap_or_else(|error| {
            panic!("{engine:?}: resuming incarnation 2's hole failed: {error}")
        });
    assert!(
        matches!(outcome, ResidentOutcome::Completed { .. }),
        "{engine:?}: incarnation 2's resumed ask did not complete: {outcome:?}"
    );

    // Retire incarnation 1's placement AGAIN and incarnation 2's placement,
    // to leave the session clean. On Core, incarnation 1's frame/handle are
    // already gone (released above), so this second call is the documented
    // idempotent no-op. On Prepared, this is the gap again: hole_1 is STILL
    // parked, so a real teardown of this session would still leak it -- not
    // asserted further here, since that is exactly the gap already
    // documented above, not a new fact about incarnation 2.
    let _ = ActorRunTarget::retire_placement(
        &mut harness.session,
        placement_1.resource_scope,
        placement_1.lexical_scope,
    );
    let retirement_2 = ActorRunTarget::retire_placement(
        &mut harness.session,
        placement_2.resource_scope,
        placement_2.lexical_scope,
    );
    assert_eq!(
        retirement_2.handles, 0,
        "{engine:?}: incarnation 2's own parked continuation was already resumed to completion above"
    );
}

#[test]
fn placement_retirement_on_core() {
    two_placements_one_retired(EngineKind::Core);
}

#[test]
fn placement_retirement_on_prepared_stg() {
    two_placements_one_retired(EngineKind::Prepared);
}
