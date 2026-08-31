//! Turn suspension routing: `askUser`/`note` servicing on both the nested
//! answerer plane and the outer loop plane, plus the operator-gate/observer
//! plumbing those paths share (`FormSource` routing, note announcements,
//! and the top-level `service_typed_request_suspension` dispatch).

use std::sync::atomic::Ordering;
use std::sync::Arc;

use serde_json::Value as Json;
use tidepool_repr::DataConTable;
use tidepool_runtime::session::{ResidentHole, ResidentOutcome};

use super::fork::AgentSessionExitPolicy;
use super::SelfHarnessDriver;
use super::*;
use crate::engine::{self, ClassifiedSuspension, CompiledTurn, SuspensionRouting, TurnOutcome};
use crate::selfharness::lifecycle::SelfHarnessState;
use crate::selfharness::observer::{AskId, Event, FormSource};
use crate::selfharness::operator::{FormShape, OperatorGate};
use crate::tree::NodeId;

impl SelfHarnessDriver {
    /// Service one `runLLMTurn @A` suspension (`site`/`ty` from
    /// [`crate::engine::SuspensionRouting::RunLLMTurn`], `prompt` the hole's
    /// human-facing text) against the CURRENT loop's SINGLE render-seeded
    /// answerer session (`self.answerer`): push the hole card as a User
    /// turn onto that persistent node — so hole #2 sees hole #1's exchange
    /// (the accumulating context window) — then drive it as a bounded
    /// multi-turn interaction to `finalize` (the effect that terminates the
    /// Agent turn loop rather than resuming it). The finalized value feeds
    /// straight into the OUTER session's `resume` to answer `loop`'s parked
    /// continuation.
    ///
    /// Bounded: each non-finalize model round counts against a per-hole
    /// budget — at [`TYPED_REQUEST_AGENT_NUDGE_ROUNDS`] the answerer is nudged to
    /// finalize, at [`TYPED_REQUEST_AGENT_MAX_ROUNDS`] the hole hard-fails — and
    /// against the per-loop [`LOOP_INFERENCE_CALL_CAP`] total.
    pub async fn service_typed_request_suspension(
        &mut self,
        site: u64,
        ty: Option<&str>,
        modules: &[String],
        prompt: &str,
        table: &DataConTable,
    ) -> Result<FinalAnswer, DriverError> {
        self.lifecycle = SelfHarnessState::SuspendedOnHole;
        self.emit(Event::RunLLMTurnHole {
            site,
            ty: ty.map(String::from),
            prompt: prompt.to_string(),
        });

        let lease = self.answerer.ok_or_else(|| {
            DriverError::Session(
                "service_typed_request_suspension called with no per-loop answerer (run_loop_fragment \
                 must create it first)"
                    .into(),
            )
        })?;
        // This hole finishes by taking the finalized answer and keeping the
        // node open for the NEXT hole — only a `ReusableLoop` lease may do
        // that (see `AgentSessionMode::require_reusable`'s doc).
        let node = lease.require_reusable()?;

        // Declare THIS hole's answer contract on the (reused) answerer node
        // before it takes a turn: the type pins `finalize`, and the harness's
        // types module puts that type in scope. Set per hole, because
        // consecutive holes in one loop can want different types.
        self.agent
            .set_answer_contract(node, self.answer_contract(ty, modules));

        // Push the hole card onto the EXISTING answerer node, accumulating
        // context rather than spawning a fresh one. The SCOPED answerer card
        // (`[AskUser, Finalize]`) names `finalize @T`, NOT the generic
        // `resume expr` (which does not compile against this stack).
        let child_prompt = engine::finalize_typed_request_prompt(
            "The loop",
            prompt,
            ty,
            modules,
            Some(table),
            &self.agent.finalize_typed_request_prompt_effect_row(),
        );
        self.agent.push_user_turn(node, &child_prompt)?;
        self.emit(Event::TurnStart { node });

        // An in-context window has NO branch position and no siblings — its
        // failure IS this turn's failure, which is why `runLLMTurn @T` keeps
        // a bare answer (the asymmetry is stated at the verb
        // declaration). So a typed exit from the shared round loop collapses
        // back into a hard failure HERE, unchanged from before the exit
        // plumbing existed.
        let fork_subtree = std::sync::atomic::AtomicU32::new(0);
        let outcome = match self
            .drive_agent_session_to_finalize(
                node,
                ty,
                site,
                0,
                &fork_subtree,
                AgentSessionExitPolicy::Interactive,
            )
            .await?
        {
            Ok(o) => o,
            Err(exit) => {
                return Err(DriverError::Session(format!(
                    "runLLMTurn answerer node {node:?}: {exit}"
                )))
            }
        };
        self.emit(Event::TurnEnd { node });

        let is_finalize = matches!(
            &outcome,
            TurnOutcome::Suspended { classified, .. }
                if matches!(classified.routing, SuspensionRouting::Finalize { .. })
        );
        if !is_finalize {
            return Err(DriverError::Session(format!(
                "runLLMTurn answerer node {node:?} did not suspend on finalize (got {})",
                turn_outcome_tag(&outcome)
            )));
        }

        // Take the finalized answer AND keep the node live (consume the
        // finalize hole, Suspended→Running) so the NEXT hole can push onto the
        // same accumulating session — not `take_finalized_value`, which cancels.
        // A closure payload is taken as a handle (it never bridges,
        // it is delivered verbatim into the loop's parked continuation on the
        // shared heap); data keeps the bridged-value path.
        let answer = if self.agent.finalize_is_closure(node) {
            let handle = self.agent.take_live_payload_handle_keep_open(node).await?;
            self.emit(Event::Finalize {
                node,
                value: "\"<closure>\"".to_string(),
            });
            FinalAnswer::Handle(handle)
        } else {
            let (value, rendered) = self.agent.take_finalized_value_keep_open(node).await?;
            self.emit(Event::Finalize {
                node,
                value: rendered,
            });
            FinalAnswer::Value(value)
        };
        // The answerer node is REUSED across the loop's holes, so
        // `node_usage` returns the node's CUMULATIVE context size. The
        // MID-LOOP compaction check (`maybe_compact_answerer`) reads it BETWEEN
        // holes, once per hole, right after this returns — never summed per-hole
        // (that would double-count the reused node's running total).
        self.lifecycle = SelfHarnessState::RunningLoop;
        Ok(answer)
    }

    /// Service a contiguous run of `askUser` suspensions on `node`, starting
    /// from the just-classified `shape`: block on the operator gate for a
    /// submission ([`OperatorGate::present_form`]),
    /// resume the answerer with it via [`Harness::answer_dialog`] (the same
    /// audited resume path a mechanical dialog answer uses — `answer_dialog`
    /// accepts `AskUser` alongside `Dialog`/`Ask`), and repeat while the
    /// resume keeps landing on ANOTHER `AskUser` suspension — `askUser`
    /// re-prompts by RECURSION on a decode failure (no `Either`; the retry is
    /// entirely Haskell-side), so a bad submission genuinely re-suspends on a
    /// fresh `AskUserWith`, not an error this driver sees.
    ///
    /// Bounded by [`ASKUSER_MAX_REPROMPTS`] CONSECUTIVE re-presentations,
    /// independent of the model-round caps (see that constant's doc): a form
    /// resume never calls the model, so it must not touch `rounds`/
    /// `loop_inference_calls` — but left totally uncapped, a non-interactive
    /// gate at EOF (the default [`StdinGate`], closed stdin) composes with
    /// `askUser`'s unbounded re-prompt recursion into a hot loop no existing
    /// cap catches.
    ///
    /// Returns `Ok(Some(outcome))` when the chain resolves to any
    /// non-form suspension (`Finalize`, a fork, a green `wait`, …) — built
    /// from [`Harness::pending_suspension_full`] read right after the resume,
    /// since `answer_dialog` itself returns no outcome — which the caller's
    /// dispatcher routes. Returns `Ok(None)` when a
    /// resume completes the node with NO pending hole (the answerer's block
    /// finished without ever calling `finalize`); the caller falls through to
    /// its existing completed-without-finalize corrective retry. `Err` on a
    /// resume failure or the reprompt cap being hit.
    pub(crate) async fn service_askuser_hole(
        &self,
        node: NodeId,
        shape: &FormShape,
    ) -> Result<Option<TurnOutcome>, DriverError> {
        let mut shape = shape.clone();
        let mut reprompts: u32 = 0;
        loop {
            let submission = self
                .present_askuser_form(&mut reprompts, FormSource::Answerer { node }, &shape)
                .await?;
            self.agent.answer_dialog(node, submission).await?;

            let Some((hole, classified, _table)) = self.agent.pending_suspension_full(node) else {
                // The resume completed the node with no further suspension.
                return Ok(None);
            };
            // A submission (or a `note` resume below) may land on a `note`
            // hole next — e.g. `askUser @T >>= \t -> note (explain t) >>
            // finalize @T t` — drain it purely via resumes before checking
            // Finalize/AskUser.
            let Some((hole, classified)) = self.drain_note_holes(node, hole.0, classified).await?
            else {
                return Ok(None);
            };
            if let SuspensionRouting::AskUser { shape: next_shape } = classified.routing {
                shape = next_shape;
                continue;
            }
            // Finalize, or any OTHER routing (a fork after the form, a `wait`
            // on an earlier-spawned thread): hand the outcome back — the
            // dispatcher in `drive_agent_session_to_finalize` routes it. Before
            // the answerer-plane green scheduler this arm hard-errored on
            // everything but Finalize/AskUser.
            return Ok(Some(TurnOutcome::Suspended { hole, classified }));
        }
    }

    pub(crate) async fn service_outer_askuser_hole(
        &mut self,
        hole: ResidentHole,
        routing: SuspensionRouting,
        compiled: &CompiledTurn,
    ) -> Result<ResidentOutcome, DriverError> {
        let mut hole = hole;
        let mut routing = routing;
        let mut reprompts: u32 = 0;
        loop {
            let outcome = match routing {
                SuspensionRouting::AskUser { shape } => {
                    let submission = self
                        .present_askuser_form(&mut reprompts, FormSource::OuterLoop, &shape)
                        .await?;
                    let answer = engine::json_answer_to_value(&submission, &compiled.table)
                        .map_err(|e| {
                            DriverError::Session(format!("outer askUser submission decode: {e}"))
                        })?;
                    let sid = self.outer_sid()?;
                    self.agent
                        .with_session(sid, |s| s.resume(hole, answer))
                        .map_err(|e| DriverError::Session(e.to_string()))?
                        .map_err(|e| {
                            DriverError::Session(format!("outer askUser resume failed: {e}"))
                        })?
                }
                SuspensionRouting::Note { text } => {
                    self.announce_note(FormSource::OuterLoop, &text);
                    use tidepool_bridge::ToCore;
                    let answer = ().to_value(&compiled.table).map_err(|e| {
                        DriverError::Session(format!("bridge unit note-answer to Value: {e}"))
                    })?;
                    let sid = self.outer_sid()?;
                    self.agent
                        .with_session(sid, |s| s.resume(hole, answer))
                        .map_err(|e| DriverError::Session(e.to_string()))?
                        .map_err(|e| {
                            DriverError::Session(format!("outer note resume failed: {e}"))
                        })?
                }
                other => {
                    return Err(DriverError::Session(format!(
                        "service_outer_askuser_hole: expected an AskUser or Note routing, \
                         got {other:?}"
                    )))
                }
            };

            match &outcome {
                ResidentOutcome::Suspended {
                    hole: next_hole,
                    request,
                    ..
                } => {
                    let classified =
                        engine::classify_hole(request, &compiled.table, &compiled.asks)?;
                    if matches!(
                        classified.routing,
                        SuspensionRouting::AskUser { .. } | SuspensionRouting::Note { .. }
                    ) {
                        // askUser's Haskell-side decode-retry re-suspended on a
                        // fresh form, or the chain's next `note`/`askUser` step
                        // — re-drive it (does NOT count as progress).
                        hole = next_hole.clone();
                        routing = classified.routing;
                        continue;
                    }
                    // A runLLMTurn suspension (or anything else) — hand it back
                    // to the main loop, which classifies and services it.
                    return Ok(outcome);
                }
                ResidentOutcome::Completed { .. } => return Ok(outcome),
            }
        }
    }

    /// Resolve which operator gate a form/note tied to `source` should reach:
    /// a labeled branch child (`source` is [`FormSource::Answerer`] AND the
    /// node carries an entry in [`Self::node_labels`]) routes to
    /// [`crate::selfharness::operator::OperatorGate::node_gate`]; every other
    /// case — an unlabeled answerer node, or [`FormSource::OuterLoop`] (the
    /// outer session's own seed question / between-loops asks, which are
    /// never node-scoped) — falls back to the default gate, byte-identical to
    /// before per-node routing existed.
    pub(crate) fn resolve_gate(&self, source: &FormSource) -> Arc<dyn OperatorGate> {
        if let FormSource::Answerer { node } = source {
            if let Some(label) = self.node_labels.lock().get(node).cloned() {
                if let Some(gate) = self.gate.node_gate(&label) {
                    return gate;
                }
            }
        }
        Arc::clone(&self.gate)
    }

    /// Post `text` to the operator gate and emit [`Event::NotePosted`] — the
    /// shared, non-blocking half of servicing a `note` hole. `source`
    /// distinguishes a nested answerer's own note from one the AUTHORED
    /// OUTER loop raised directly, same as [`FormSource`] does for a form.
    /// Unlike [`Self::present_askuser_form`], there is nothing to wait for:
    /// the caller resumes immediately after this returns.
    pub(crate) fn announce_note(&self, source: FormSource, text: &str) {
        let gate = self.resolve_gate(&source);
        self.emit(Event::NotePosted {
            source,
            text: text.to_string(),
        });
        let posted = text.to_string();
        tokio::task::block_in_place(move || gate.post_note(&posted));
    }

    /// Post `text` to the operator gate and resume `node`'s `note` hole
    /// immediately with `()` via [`Harness::answer_note`] — no operator
    /// interaction, no model round. Unlike `askUser`'s reprompt cap, this has
    /// no bound of its own: a `note` resume always makes progress (the next
    /// pending hole, or none at all), so nothing here can spin.
    pub(crate) async fn service_note_hole(
        &self,
        node: NodeId,
        text: &str,
    ) -> Result<(), DriverError> {
        self.announce_note(FormSource::Answerer { node }, text);
        self.agent.answer_note(node).await?;
        Ok(())
    }

    /// A `getStateJson` hole's response: the current loop iteration's entry
    /// state, or `Null` when there is none (the general Agent path, which
    /// carries no cycle state at all). Both
    /// [`Self::drain_note_holes`]'s node-level resume and
    /// [`Self::service_thread_ready`]'s raw-thread resume read from it. Delivery differs (a node-level
    /// `answer_dialog` vs. a raw in-machine `resume`), the value doesn't.
    pub(crate) fn loop_state_snapshot(&self) -> Json {
        self.loop_state_json.clone().unwrap_or(Json::Null)
    }

    /// Drain a leading run of `note` holes on `node`, starting from
    /// `classified` (which may or may not already be `SuspensionRouting::Note` —
    /// a no-op passthrough when it isn't): post each via
    /// [`Self::service_note_hole`] and resume immediately with `()`,
    /// repeating while the resume keeps landing on ANOTHER note. Returns the
    /// first NON-note pending hole once the chain stops — the caller (already
    /// prepared to dispatch on `Finalize`/`AskUser`/`Fork`) proceeds from
    /// there — or `None` if the chain completed the node with NO further
    /// suspension (the caller's existing corrective-retry path, same as a
    /// plain `Completed` round outcome).
    pub(crate) async fn drain_note_holes(
        &self,
        node: NodeId,
        mut hole: String,
        mut classified: ClassifiedSuspension,
    ) -> Result<Option<(String, ClassifiedSuspension)>, DriverError> {
        loop {
            match classified.routing.clone() {
                SuspensionRouting::Note { text } => {
                    self.service_note_hole(node, &text).await?;
                }
                SuspensionRouting::ReadState => {
                    // Immediate resume with the cycle's entry state — no
                    // operator, no model round (note's service shape).
                    let state = self.loop_state_snapshot();
                    self.agent.answer_dialog(node, state).await?;
                }
                // A branch-node window's own `delegate` call
                // lowers to a real `Subagent` send (`Tidepool.Agent.Delegate.
                // runDelegate`) — same suspension, same driver-owned
                // handler, as the AUTHORED outer loop's `spawnAgent`
                // (`Self::service_outer_subagent`); this is the SAME
                // dispatch, just resumed against THIS node's own session
                // (`Harness::resume_with_value`) rather than the outer one.
                // No operator, no model round — the saga itself is the
                // "wait" (worktree + backend cycle), not a suspension this
                // driver presents to anyone.
                SuspensionRouting::Subagent => {
                    let (pending_suspension, _classified, table, request) = self
                        .agent
                        .pending_suspension_with_request(node)
                        .ok_or_else(|| {
                            DriverError::Session(format!(
                                "node {node:?} has no pending Subagent hole to service"
                            ))
                        })?;
                    let value = self
                        .service_outer_subagent(&request, &table, FormSource::Answerer { node })
                        .await?;
                    self.agent
                        .resume_with_value(node, &pending_suspension, value)
                        .await?;
                }
                // An interpreter around an answerer-local effect may use a
                // driver-owned outer capability without exposing that
                // capability in the model-authored row.  Delegate does this
                // after a worker settles: WorktreeHeadOf records the actual
                // candidate head beside the typed result.  Service it against
                // this node's parked continuation just as the authored outer
                // loop services the same request family.
                SuspensionRouting::OuterEffect(kind) => {
                    let (pending_suspension, _classified, table, request) = self
                        .agent
                        .pending_suspension_with_request(node)
                        .ok_or_else(|| {
                            DriverError::Session(format!(
                                "node {node:?} has no pending {kind:?} hole to service"
                            ))
                        })?;
                    let value = self.service_outer_effect(kind, &request, &table)?;
                    self.agent
                        .resume_with_value(node, &pending_suspension, value)
                        .await?;
                }
                _ => break,
            }
            match self.agent.pending_suspension_full(node) {
                Some((next_hole, next_classified, _table)) => {
                    hole = next_hole.0;
                    classified = next_classified;
                }
                None => return Ok(None),
            }
        }
        Ok(Some((hole, classified)))
    }

    /// Present `shape` via the operator gate and return the operator's raw
    /// submission — the servicing step shared by [`Self::service_askuser_hole`]
    /// (a nested answerer's own form) and [`Self::service_outer_askuser_hole`]
    /// (the authored OUTER loop's own form): check + increment the shared
    /// reprompt cap, emit [`Event::FormPresented`], block on the operator gate,
    /// then emit [`Event::FormSubmitted`]. `source` is the only observable
    /// difference between the two callers — a genuine tag distinguishing which
    /// side raised the form in the transcript, not a hidden behavior fork. It
    /// is also what [`Self::resolve_gate`] reads to route a labeled branch
    /// child's form to its own per-node gate.
    ///
    /// This is the ONE site every form presentation funnels through
    /// (regardless of `source`), so it also mints this presentation's
    /// [`AskId`] — one global monotonic counter rather than one per
    /// `source`, since `source` already disambiguates in the log and a
    /// re-prompt (another call here for the same logical ask) gets a FRESH
    /// id like any other presentation.
    pub(crate) async fn present_askuser_form(
        &self,
        reprompts: &mut u32,
        source: FormSource,
        shape: &FormShape,
    ) -> Result<Json, DriverError> {
        if *reprompts >= ASKUSER_MAX_REPROMPTS {
            return Err(DriverError::Session(format!(
                "operator form re-presented {reprompts} times without a decodable \
                 submission (a non-interactive gate at EOF, or a form whose \
                 submission never decodes)"
            )));
        }
        *reprompts += 1;

        let ask_id = AskId(self.ask_id_counter.fetch_add(1, Ordering::SeqCst) + 1);
        self.emit(Event::FormPresented {
            source: source.clone(),
            shape: shape.clone(),
            ask_id,
        });
        // `OperatorGate::present_form` is SYNC-BLOCKING by frozen contract
        // (`selfharness/operator.rs`) — a web gate parks a channel. Run it
        // under `block_in_place` so that blocking wait yields the tokio
        // worker rather than stalling it.
        let gate = self.resolve_gate(&source);
        let form = shape.clone();
        let submission = tokio::task::block_in_place(move || gate.present_form(&form));
        self.emit(Event::FormSubmitted {
            source,
            submission: submission.clone(),
            ask_id,
        });
        Ok(submission)
    }
}
