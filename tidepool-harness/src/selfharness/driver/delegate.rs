//! Outer-loop effect delegation: servicing `Subagent`/`Console`/`Worktree`/
//! `RepoEvent`/`Exec`/`Journal` suspensions the AUTHORED outer `loop` raises
//! (`service_outer_subagent`/`service_outer_effect` and their shared
//! decode-dispatch-convert plumbing), distinct from the answerer-plane
//! servicing in `suspension`/`fork`/`green`.

use std::sync::Arc;

use tidepool_bridge::ToCore;
use tidepool_eval::value::Value;
use tidepool_repr::DataConTable;

use super::rendered_result_snippet;
use super::SelfHarnessDriver;
use super::*;
use crate::engine::{self};
use crate::selfharness::observer::FormSource;
use crate::selfharness::operator::DelegationPhase;

impl SelfHarnessDriver {
    /// Service a run of `askUser` suspensions the AUTHORED OUTER loop itself
    /// raised (distinct from [`Self::service_askuser_hole`], which handles a
    /// nested ANSWERER's form). Present `shape` via the operator gate
    /// ([`OperatorGate::present_form`]),
    /// convert the flat submission into the `Value` `askUserRaw :: Value -> M
    /// Value` returns ([`engine::json_answer_to_value`] against the outer
    /// compile's `table`), and resume the OUTER session — repeating while the
    /// resume lands on ANOTHER `AskUser` suspension, since `askUser` re-prompts
    /// by RECURSION on a decode failure (no `Either`; the retry is entirely
    /// Haskell-side, so a bad submission genuinely re-suspends on a fresh
    /// `AskUserWith`, not an error this driver sees).
    ///
    /// Returns the FIRST [`ResidentOutcome`] that is NOT another operator form
    /// — a `runLLMTurn` suspension (which [`Self::run_loop_fragment_inner`]'s
    /// main loop then services) or a completion — so the outer loop can
    /// interleave author-driven forms and model-answered holes freely.
    ///
    /// Bounded by [`ASKUSER_MAX_REPROMPTS`] CONSECUTIVE re-presentations, for
    /// the same reason [`Self::service_askuser_hole`] is: the default headless
    /// [`StdinGate`] returns an EMPTY submission on EOF rather than erroring, so
    /// a non-interactive gate composes with `askUser`'s unbounded Haskell-side
    /// re-prompt into a hot loop no model-round cap catches (a form resume is
    /// not a model round). The between-loops human gate bounds loop ITERATIONS,
    /// not re-prompts WITHIN one loop's `askUser` — this counter does.
    /// `routing` is the FIRST hole's already-classified routing — either
    /// `SuspensionRouting::AskUser` (a typed form) or `SuspensionRouting::Note`
    /// (display-only narration, e.g. `note "..." >> askUser @T ...` at the
    /// outer level): each iteration dispatches on whichever of the two the
    /// CURRENT hole is, so a chain freely interleaving `note` and `askUser`
    /// (in either order) drives to completion without a model round.
    /// Service ONE `Subagent` suspension raised by the AUTHORED outer loop:
    /// decode the ORIGINAL suspended request through the generated
    /// `SubagentReq: FromCore` (against the loop compile's own table — the
    /// args are bridged ADTs, never JSON-probed), dispatch it into the
    /// driver-owned [`tidepool_handlers::SubagentHandler`], and return the
    /// `Response::Complete` value the caller resumes the hole with — the
    /// IDENTICAL generated conversion path a dispatched effect takes, minus
    /// the dispatch (the outer row's handled prefix must stay empty on the
    /// shared machine; see [`outer_decls`]).
    ///
    /// Runs the actual dispatch on the tokio BLOCKING thread pool via
    /// `tokio::task::spawn_blocking`, not `tokio::task::block_in_place`:
    /// `block_in_place` runs the closure SYNCHRONOUSLY, inline in this
    /// task's own poll — it frees the calling WORKER THREAD for other
    /// tokio TASKS, but it does not yield, so a caller batching several
    /// ready `Subagent` items through [`super::fork::drive_concurrent`]'s
    /// `buffer_unordered` would never even START a second item's dispatch
    /// until the first fully returns (the whole point of batching is lost —
    /// see `CONCURRENT_SIBLINGS_SPIKE_FINDINGS.md`). `spawn_blocking` hands
    /// the call to a real OS thread and returns a future that yields
    /// immediately, so sibling items in the same batch begin their own
    /// dispatch (and [`Self::subagent`] lock acquisition) concurrently.
    ///
    /// **This still serializes on [`Self::subagent`]'s ONE lock for the
    /// duration of whichever `SubagentReq` variant the caller issued** — a
    /// `SubagentSpawn`/`SubagentAwait` call holds `&mut SubagentHandler` (and
    /// therefore this lock) for as long as `tidepool_agent::spawn::CoupledSpawner`
    /// takes to reach a terminal, because `CoupledSpawner::spawn_one_cycle`'s
    /// signature requires `&mut self` end to end even though its BODY only
    /// touches `&self.substrate` (an `Arc<Mutex<SpawnSubstrate>>` already
    /// documented as safe for N concurrent cycles — `tidepool-agent/CLAUDE.md`'s
    /// "Concurrency: a shared substrate, N detachable sagas"). Narrowing that
    /// signature to `&self` would let two `SubagentSpawn` calls genuinely run
    /// concurrently through one handler instance, but `tidepool-agent`/
    /// `tidepool-handlers` are outside this crate's ALLOWED PATHS, so this
    /// dispatch cannot do that itself — see the findings addendum this spec
    /// asked for. **Real overlap is still achieved for `spawnAgent`'s actual
    /// shape** (`haskell/lib/Tidepool/Agent/Spawn.hs`: `spawnAgent spec =
    /// spawnAsync spec >>= either (pure . Left) awaitAgent`): `SubagentSpawnAsync`
    /// only briefly touches `&mut self` (admit the cycle, mint a backend,
    /// `std::thread::spawn` the actual agent conversation onto a DETACHED
    /// thread carrying its own substrate handle, `tidepool-handlers/src/handlers/agent.rs`'s
    /// `subagent_spawn_async`) before releasing this lock — so N siblings'
    /// `spawnAsync` calls, batched here, each kick off their own background
    /// thread in quick succession, and those threads run their (slow) real
    /// work fully in parallel. Each sibling's LATER `SubagentAwait` call
    /// still queues on this lock, but by the time it runs its own background
    /// thread has typically already finished (having run concurrently with
    /// its siblings' since `spawnAsync`), so the queued `recv()` returns
    /// near-instantly — the batch's total wall time approaches
    /// `max` of the siblings' cycles rather than their `sum`.
    ///
    /// `source` identifies who raised this delegation — the AUTHORED outer
    /// loop itself ([`FormSource::OuterLoop`]) or a labeled node's own
    /// `delegate` ([`FormSource::Answerer`]) — and is used ONLY to resolve
    /// which operator gate's timeline the delegation-lifecycle events
    /// ([`DelegationPhase`], via [`OperatorGate::delegation_progress`]) land
    /// on, via [`Self::resolve_gate`]; it never affects dispatch itself.
    /// Emits [`DelegationPhase::Started`] before the dispatch (so a spawn
    /// that never returns is still visible), then EXACTLY ONE of
    /// [`DelegationPhase::Settled`]/[`DelegationPhase::Failed`] — including
    /// the "no subagent handler configured" refusal, which used to be
    /// completely silent (no `Event`, no gate call, no `tracing` line).
    pub(crate) async fn service_outer_subagent(
        &self,
        request: &Value,
        table: &DataConTable,
        source: FormSource,
    ) -> Result<Value, DriverError> {
        let gate = self.resolve_gate(&source);
        gate.delegation_progress(&DelegationPhase::Started {
            brief: rendered_result_snippet(&request.to_string()),
        });
        let started = std::time::Instant::now();
        if self.subagent.lock().is_none() {
            let reason =
                "the authored loop called a Subagent verb (spawnAgent/spawnAgentRaw) but no \
                 subagent handler is configured — wire one with \
                 SelfHarnessDriver::set_subagent_handler (the tidepool-selfharness binary \
                 does this when TIDEPOOL_MEMORY_REPO is set)"
                    .to_string();
            gate.delegation_progress(&DelegationPhase::Failed {
                reason: reason.clone(),
                duration: started.elapsed(),
            });
            return Err(DriverError::Session(reason));
        }
        let subagent = Arc::clone(&self.subagent);
        let owned_request = request.clone();
        let owned_table = table.clone();
        let dispatched = tokio::task::spawn_blocking(move || {
            let mut guard = subagent.lock();
            #[allow(
                clippy::expect_used,
                reason = "checked wired immediately above; set_subagent_handler never unwires"
            )]
            let handler = guard
                .as_mut()
                .expect("checked wired immediately above — set_subagent_handler never unwires");
            Self::dispatch_outer_effect(handler, &owned_request, &owned_table)
        })
        .await
        .map_err(|e| DriverError::Session(format!("subagent dispatch task panicked: {e}")))?;
        let elapsed = started.elapsed();
        match dispatched {
            Ok(value) => {
                tracing::info!(
                    elapsed_ms = elapsed.as_millis() as u64,
                    "outer subagent suspension serviced"
                );
                gate.delegation_progress(&DelegationPhase::Settled {
                    outcome: rendered_result_snippet(&value.to_string()),
                    duration: elapsed,
                });
                Ok(value)
            }
            Err(e) => {
                let reason = format!("subagent dispatch: {e}");
                gate.delegation_progress(&DelegationPhase::Failed {
                    reason: reason.clone(),
                    duration: elapsed,
                });
                Err(DriverError::Session(reason))
            }
        }
    }

    /// Service a Console/Worktree/RepoEvent/Exec/Journal suspension raised by
    /// the AUTHORED outer loop (`kind` classified by [`engine::classify_hole`]):
    /// dispatch the ORIGINAL request into the matching driver-owned handler
    /// via [`Self::dispatch_outer_effect`] — the same decode-dispatch-convert
    /// shape [`Self::service_outer_subagent`] uses, generalized over which
    /// handler is reached. `Console`'s `say` additionally posts its text to
    /// the operator feed the way the `note` servicing arm does
    /// ([`Self::announce_note`]) before resuming with `()`.
    pub(crate) fn service_outer_effect(
        &mut self,
        kind: engine::OuterEffectKind,
        request: &Value,
        table: &DataConTable,
    ) -> Result<Value, DriverError> {
        if kind == engine::OuterEffectKind::Console {
            if let Ok(tidepool_handlers::ConsoleReq::Print(text)) =
                <tidepool_handlers::ConsoleReq as tidepool_bridge::FromCore>::from_value(
                    request, table,
                )
            {
                self.announce_note(FormSource::OuterLoop, &text);
            }
        }
        let mut handlers = self.handlers.lock();
        match kind {
            engine::OuterEffectKind::Console => {
                let handler = handlers.console.as_mut().ok_or_else(|| {
                    Self::unwired_outer_effect_error("Console", "say", "set_console_handler")
                })?;
                Self::dispatch_outer_effect(handler, request, table)
            }
            engine::OuterEffectKind::Worktree => {
                let handler = handlers.worktree.as_mut().ok_or_else(|| {
                    Self::unwired_outer_effect_error(
                        "Worktree",
                        "createWorktree/lookupWorktree/listWorktrees/worktreeBranch/worktreeHead",
                        "set_worktree_handler",
                    )
                })?;
                Self::dispatch_outer_effect(handler, request, table)
            }
            engine::OuterEffectKind::RepoEvent => {
                let handler = handlers.event.as_mut().ok_or_else(|| {
                    Self::unwired_outer_effect_error(
                        "RepoEvent",
                        "withHandler (repository events)",
                        "set_event_handler",
                    )
                })?;
                Self::dispatch_outer_effect(handler, request, table)
            }
            engine::OuterEffectKind::Exec => {
                let handler = handlers.exec.as_mut().ok_or_else(|| {
                    Self::unwired_outer_effect_error(
                        "Exec",
                        "run/runIn/runArgv",
                        "set_exec_handler",
                    )
                })?;
                Self::dispatch_outer_effect(handler, request, table)
            }
            engine::OuterEffectKind::Journal => {
                let handler = handlers.journal.as_mut().ok_or_else(|| {
                    Self::unwired_outer_effect_error("Journal", "record", "set_journal_handler")
                })?;
                Self::dispatch_outer_effect(handler, request, table)
            }
        }
        .map_err(|e| DriverError::Session(format!("{kind:?} dispatch: {e}")))
    }

    /// Non-blocking companion to the `RepoEventAwait` interception inside the
    /// `SuspensionRouting::OuterEffect` servicing arm: decode
    /// the original suspended request's `subscription`, poll the event
    /// handler's plain (non-sleeping) drain, and report `None` on an empty
    /// batch — still parked, nothing to resume with — or `Some(value)`
    /// already encoded exactly as `RepoEventAwait`'s own dispatch would
    /// encode it (`Either EventError [RepositoryEvent]`), ready to resume the
    /// hole with directly. Never calls `repo_event_await` — that verb's own
    /// blocking loop is exactly what this exists to avoid running inline in
    /// the scheduler.
    pub(crate) fn poll_repo_event_await(
        &mut self,
        request: &Value,
        table: &DataConTable,
    ) -> Result<Option<Value>, DriverError> {
        use tidepool_bridge::FromCore;
        let mut handlers = self.handlers.lock();
        let handler = handlers.event.as_mut().ok_or_else(|| {
            Self::unwired_outer_effect_error(
                "RepoEvent",
                "withHandler (repository events)",
                "set_event_handler",
            )
        })?;
        let req = tidepool_handlers::RepoEventReq::from_value(request, table)
            .map_err(|e| DriverError::Session(format!("RepoEventAwait decode: {e}")))?;
        let tidepool_handlers::RepoEventReq::RepoEventAwait(subscription, _timeout_ms) = req else {
            return Err(DriverError::Session(
                "poll_repo_event_await: decoded request was not RepoEventAwait (scheduler bug)"
                    .into(),
            ));
        };
        // `_timeout_ms` is deliberately unread: `nextEvent`'s own calling
        // convention (`awaitFirst`) always passes -1 (no deadline) — a
        // bounded wait is expressed by merging an `after ms` deadline into
        // the SAME subscription instead, which arrives as an ordinary `Tick`
        // through this same drain. No caller in the authored stdlib surface
        // passes a non-negative timeout to this verb.
        let result = handler.repo_event_drain(subscription);
        if let Ok(batch) = &result {
            if batch.is_empty() {
                return Ok(None);
            }
        }
        let value = result
            .to_value(table)
            .map_err(|e| DriverError::Session(format!("RepoEventAwait encode: {e}")))?;
        Ok(Some(value))
    }

    /// The legible "no handler wired" error every [`Self::service_outer_effect`]
    /// branch raises for its own effect — names the verb family and the
    /// setter that fixes it, never a hang.
    pub(crate) fn unwired_outer_effect_error(
        effect: &str,
        verbs: &str,
        setter: &str,
    ) -> DriverError {
        DriverError::Session(format!(
            "the authored loop called a {effect} verb ({verbs}) but no {effect} handler is \
             configured — wire one with SelfHarnessDriver::{setter}"
        ))
    }

    /// The shared decode-dispatch-convert shape every outer-row effect
    /// suspension goes through: decode the ORIGINAL suspended request `Value`
    /// via the handler's generated `<Eff>Req: FromCore` (against the loop
    /// compile's own table — never JSON-probed), dispatch it into `handler`
    /// under `tokio::task::block_in_place` (the same discipline every
    /// `OperatorGate` call and [`Self::service_outer_subagent`] use), and
    /// convert the [`tidepool_effect::Response`] back into a resumable
    /// `Value` — a `Complete` value as-is, a `List` folded into a cons chain
    /// from its carried `cons_id`/`nil_id` (mirrors the in-machine dispatch
    /// path's own fold, `tidepool_effect::machine`; a suspending outer row
    /// never reaches that path itself, so this is the suspend-side
    /// equivalent). No outer-row verb returns a list today, but a future one
    /// (`respond_list`) resumes correctly without another servicing site.
    pub(crate) fn dispatch_outer_effect<H>(
        handler: &mut H,
        request: &Value,
        table: &DataConTable,
    ) -> Result<Value, tidepool_effect::EffectError>
    where
        H: tidepool_effect::EffectHandler<tidepool_mcp::CapturedOutput>,
    {
        use tidepool_bridge::FromCore;
        use tidepool_effect::dispatch::EffectContext;
        let req = H::Request::from_value(request, table)?;
        let captured = tidepool_mcp::CapturedOutput::new();
        let resp = tokio::task::block_in_place(|| {
            let cx = EffectContext::with_user(table, &captured);
            handler.handle(req, &cx)
        })?;
        Ok(match resp {
            tidepool_effect::Response::Complete(v) => v,
            tidepool_effect::Response::List {
                items,
                cons_id,
                nil_id,
            } => {
                let mut acc = Value::Con(nil_id, vec![]);
                for item in items.into_iter().rev() {
                    acc = Value::Con(cons_id, vec![item, acc]);
                }
                acc
            }
        })
    }
}
