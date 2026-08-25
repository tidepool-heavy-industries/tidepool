//! Fork/fanout servicing: spawn-time budgets (`ForkBudget`,
//! `check_fork_budgets`), the branch/fork agent-session guard, driving a
//! recursive fork child or a concurrent `RunLLMTurn` fanout child to
//! `finalize` (`drive_fork_child_agent_session`/`drive_fanout_child`), and
//! the shared multi-round pump (`drive_agent_session_to_finalize`) both ride.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use futures_util::stream::{self, StreamExt};
use tidepool_eval::value::Value;
use tidepool_repr::DataConTable;

use super::corrective::fork_child_failure_corrective;
use super::green::{GreenRoundExit, ModelRoundGreenThreadScheduler};
use super::SelfHarnessDriver;
use super::*;
use crate::engine::{self, EngineError, InvocationExit, SuspensionRouting, TurnOutcome};
use crate::harness::{Harness, HarnessError};
use crate::log::Actor;
use crate::selfharness::lifecycle::SelfHarnessState;
use crate::selfharness::observer::{Event, FormSource};
use crate::tree::{FanBadge, NodeId};

/// One answerer WINDOW's fork budget — total children across all rounds,
/// drawn on by direct `fork`/`forkAll` servicing ([`SelfHarnessDriver::drain_answerer_fork`])
/// and green-thread forks ([`SelfHarnessDriver::service_thread_ready`])
/// alike. Spending happens BEFORE the spawn, so the refusal costs nothing.
pub(crate) struct ForkBudget {
    cap: u32,
    spent: u32,
}

impl ForkBudget {
    /// How many children `routing` would spawn: a single fork is 1, a fanout
    /// its fan (`Bounded`/`Dynamic` badges fall back to the decoded prompt
    /// count — the number of children that would actually be driven).
    pub(crate) fn cost(routing: &SuspensionRouting) -> u32 {
        match routing {
            SuspensionRouting::Fork { fan: None, .. } => 1,
            SuspensionRouting::Fork {
                fan: Some(FanBadge::Exact { n }),
                ..
            } => *n,
            SuspensionRouting::Fork { prompts, .. } => prompts.len() as u32,
            _ => 0,
        }
    }

    /// Spend `cost` children if the pool covers them; `false` (nothing
    /// spent) when it doesn't.
    pub(crate) fn try_spend(&mut self, cost: u32) -> bool {
        if self.spent.saturating_add(cost) > self.cap {
            return false;
        }
        self.spent += cost;
        true
    }
}

/// What [`SelfHarnessDriver::drive_agent_session_to_finalize`] does when its node
/// suspends on something other than `finalize` — the one axis the sol
/// cross-family review's finding 4 confirmed genuinely differs between the
/// driver's model-session pumps (everything else — round caps, provider
/// handling, compile correctives, finalize detection, event emission — is
/// now the ONE shared loop).
#[derive(Debug, Clone, Copy)]
pub(crate) enum AgentSessionExitPolicy {
    /// The reused single-hole answerer, a sequential branch/branch-fanout
    /// child, and a recursive fork child: service every suspension this
    /// driver knows how to (`askUser`, `note`, `fork`, green threads) via the
    /// ordinary dispatcher.
    Interactive,
    /// A concurrent `runLLMTurnFork`/`runLLMTurnFanout` child
    /// ([`SelfHarnessDriver::drive_fanout_child`], v1 scope): `finalize`
    /// only. No operator-gate serialization or fork bookkeeping across
    /// siblings racing the same machine, so any other suspension folds
    /// straight to [`InvocationExit::NotFinalized`] DATA at this child's own
    /// position instead of being serviced. `idx` names the child in the
    /// resulting message.
    FinalizeOnly { idx: usize },
}

/// Put ONE child answer into the shape the parked fork continuation
/// expects, against the caller-supplied round table (unlike
/// `Harness::wrap_fork_answer`, which derives its table from node-pending
/// state) — a thin `DriverError` wrapper over the ONE shared implementation,
/// [`engine::wrap_fork_answer`] (sol cross-family review finding 9d).
pub(crate) fn wrap_fork_value(
    source: engine::ForkSource,
    value: Value,
    table: &DataConTable,
) -> Result<Value, DriverError> {
    engine::wrap_fork_answer(source, value, table).map_err(|e| DriverError::Session(e.to_string()))
}

/// Drive `count` children CONCURRENTLY up to `cap` at once
/// (`buffer_unordered`), then re-sort the results back to DECLARATION order
/// — completion order is nondeterministic and must never be observable in
/// the resumed answer. `child` is called once per index; a free function
/// (no `self`) so the caller supplies whatever `&self`-reachable state each
/// child needs via its own capture, same reasoning
/// [`SelfHarnessDriver::drive_fanout_child`]'s doc gives for why this is
/// never `Arc<Self>`/`tokio::spawn`. THE ordering/concurrency shell
/// [`SelfHarnessDriver::service_outer_fanout`] uses.
pub(crate) async fn drive_concurrent<T, F, Fut>(
    cap: usize,
    count: usize,
    child: F,
) -> Vec<(usize, T)>
where
    F: Fn(usize) -> Fut,
    Fut: std::future::Future<Output = T>,
{
    let child = &child;
    let mut results: Vec<(usize, T)> = stream::iter(0..count)
        .map(|idx| async move { (idx, child(idx).await) })
        .buffer_unordered(cap)
        .collect()
        .await;
    results.sort_by_key(|(idx, _)| *idx);
    results
}

/// The character budget a fork child's derived label slug truncates to —
/// short enough that a long brief still reads as one path segment, long
/// enough to stay recognizable alongside a sibling's. See
/// [`SelfHarnessDriver::fork_child_label`].
pub(crate) const FORK_LABEL_SLUG_BUDGET: usize = 24;

/// A fork child's own GUI path segment: `f<idx>-<slug>`, where `slug` is an
/// ASCII, lowercase, hyphen-joined prefix of the fork's authored BRIEF (not
/// the composed hole card) — non-alphanumeric runs collapse to one hyphen,
/// leading/trailing hyphens are trimmed, and a brief with no alphanumeric
/// content at all (or an empty one) falls back to the bare index so the
/// segment is never empty. See [`SelfHarnessDriver::fork_child_label`] for
/// how this combines with the parent's own path.
pub(crate) fn fork_child_path_segment(idx: u32, brief: &str) -> String {
    let mut slug = String::new();
    let mut pending_hyphen = false;
    for c in brief.chars() {
        if slug.len() >= FORK_LABEL_SLUG_BUDGET {
            break;
        }
        if c.is_ascii_alphanumeric() {
            if pending_hyphen && !slug.is_empty() {
                slug.push('-');
            }
            pending_hyphen = false;
            slug.push(c.to_ascii_lowercase());
        } else {
            pending_hyphen = true;
        }
    }
    if slug.is_empty() {
        format!("f{idx}")
    } else {
        format!("f{idx}-{slug}")
    }
}

impl SelfHarnessDriver {
    /// The step-2 spawn admission: per-session fan pool AND whole-subtree
    /// descendant budget, checked (and the subtree spent) atomically at the
    /// one moment children are about to exist. `Some(corrective)` = refused,
    /// nothing spent; `None` = both budgets debited, spawn may proceed.
    ///
    /// The subtree reservation is a compare-exchange loop, not a
    /// check-then-act (F10): two concurrent sharers of one `fork_subtree`
    /// counter (a window driving its own fork children concurrently) can no
    /// longer both observe headroom and both add past the cap — each
    /// attempt re-reads the counter on a lost race and re-checks against the
    /// cap before retrying. `budget` (the per-window pool) is `&mut`, so it
    /// has no such race — but its check-and-spend still runs AFTER the
    /// subtree reservation, so a window-budget refusal rolls the subtree
    /// reservation back rather than leaving it charged for a child that
    /// will never spawn.
    pub(crate) fn check_fork_budgets(
        &self,
        budget: &mut ForkBudget,
        cost: u32,
        fork_subtree: &std::sync::atomic::AtomicU32,
        ty_label: &str,
    ) -> Option<String> {
        use std::sync::atomic::Ordering;
        let mut spent = fork_subtree.load(Ordering::Relaxed);
        loop {
            let new_spent = spent.saturating_add(cost);
            if new_spent > self.fork_subtree_cap {
                return Some(fork_subtree_refusal(
                    spent,
                    self.fork_subtree_cap,
                    cost,
                    ty_label,
                ));
            }
            match fork_subtree.compare_exchange_weak(
                spent,
                new_spent,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(actual) => spent = actual,
            }
        }
        if !budget.try_spend(cost) {
            // Roll back: the subtree reservation was provisional on the
            // window pool also covering `cost`, and it doesn't.
            fork_subtree.fetch_sub(cost, Ordering::Relaxed);
            return Some(fork_budget_refusal(
                budget.spent,
                budget.cap,
                cost,
                ty_label,
            ));
        }
        None
    }
}

/// The refusal corrective when the WHOLE fork tree's descendant budget is
/// spent — distinct from the per-session pool below, so the model knows
/// the boundary is tree-wide, not something a deeper fork escapes.
pub(crate) fn fork_subtree_refusal(spent: u32, cap: u32, needed: u32, ty_label: &str) -> String {
    let ty_disp = display_ty(ty_label);
    format!(
        "Fork budget exhausted for this WHOLE tree of sessions: {spent} of {cap} \
         descendant sessions are already spawned across all depths, and that block \
         needed {needed} more. The block was ABORTED (top-level declarations from \
         earlier rounds persist; the aborted block's bindings are lost). Do not \
         fork again anywhere in this tree — finalize with what you have: evaluate \
         `finalize @{ty_disp} value`."
    )
}

/// The refusal corrective for a fork that would exceed the window's budget:
/// what happened, what survives, and the one useful next step. `cap == 0`
/// (the `fork_depth >= max_fork_depth` case, `drive_agent_session_to_finalize`)
/// is a DEPTH refusal, not a per-session pool refusal — depth-1..7 children
/// fork fine, so the reason must be the tree's depth, never "nested forking
/// is not supported" (false, and contradicts the Fork card).
pub(crate) fn fork_budget_refusal(spent: u32, cap: u32, needed: u32, ty_label: &str) -> String {
    let ty_disp = display_ty(ty_label);
    if cap == 0 {
        return format!(
            "Forking is not available in THIS session: the fork tree has reached its \
             maximum depth, so this session must answer its own brief directly. The \
             block was ABORTED (top-level declarations from earlier rounds persist; \
             the aborted block's bindings are lost). Answer with what you can \
             establish yourself: evaluate `finalize @{ty_disp} value`."
        );
    }
    format!(
        "Fork budget exhausted: this session has spawned {spent} of its {cap} fork \
         children, and that block needed {needed} more, so the block was ABORTED \
         (top-level declarations from earlier rounds persist; the aborted block's \
         bindings are lost). Do not fork again — finalize with what you have: \
         evaluate `finalize @{ty_disp} value`."
    )
}

/// The plain-language round-progress summary for a `NoBlock` reply — the
/// round dispatcher's third arm, alongside a compile success/failure.
pub(crate) const NO_HASKELL_BLOCK_ROUND_ERROR: &str = "reply had no haskell block";

/// The corrective line appended when a round ends with green threads still
/// running — the round-scoped structured-concurrency contract, stated at the
/// moment it bit rather than left to be rediscovered.
pub(crate) fn dropped_threads_warning(dropped: usize) -> String {
    format!(
        "Note: {dropped} async thread(s) from that block were still running and were \
         DROPPED — their handles are now dead. `async` and the `wait` that collects \
         it belong in the SAME ```haskell block; results you already bound with \
         `<-` persist and remain usable."
    )
}

/// A one-shot fork child window's transaction — the deeper, ownership-tracked
/// treatment of [`AgentSessionMode::OneShotBranch`] alone (P3.2's typestate
/// opportunity). Retiring used to be a hand-written four-site discipline per
/// caller (a mechanism error, a non-finalize exit, a closure rejection, and
/// success), linked only by sequencing and a bare `NodeId` a reader had to
/// trust every future error arm would remember to terminate. This guard
/// makes retiring exactly once, on every exit, structural instead:
/// [`Self::finalize_fork_data`] retires on success, [`Self::fold_exit`]
/// retires then produces the exit, and `Drop` retires an unfinished window —
/// the mechanism-failure `?` early return that used to need its OWN
/// hand-written `terminate_node` call now needs none.
///
/// Non-Clone: at most one guard exists per child window.
pub(crate) struct BranchAgentSessionGuard {
    agent: Arc<Harness>,
    node: NodeId,
    realm: tidepool_codegen::jit_machine::RealmId,
    scope: tidepool_codegen::scope::ScopeId,
    retired: bool,
}

// Every field here (`Arc`, `NodeId`, `RealmId`, `ScopeId`, `bool`) is
// independently Clone, so a `#[derive(Clone)]` would compile silently — and
// then a clone's `retired` flag would diverge from the original's, letting
// `finalize_fork_data`/`fold_exit` and the panic-safety `Drop` each believe
// THEY own retiring the window, double-retiring the node this guard exists
// to retire exactly once.
static_assertions::assert_not_impl_any!(BranchAgentSessionGuard: Clone, Copy);

impl BranchAgentSessionGuard {
    /// Mint a guard from an already-established [`AgentSessionMode::OneShotBranch`]
    /// — `require_one_shot` refuses to hand back node/realm/scope if `lease`
    /// were ever (by a future refactor) the loop's reusable answerer instead
    /// of a one-shot child's own, so this is where that check is
    /// load-bearing.
    pub(crate) fn from_lease(
        lease: AgentSessionMode,
        agent: Arc<Harness>,
    ) -> Result<Self, DriverError> {
        let (node, realm, scope) = lease.require_one_shot()?;
        Ok(Self {
            agent,
            node,
            realm,
            scope,
            retired: false,
        })
    }

    /// Success: a fork answer
    /// is bare data. A successful fork child's durable ending must be
    /// `NodeDone`, recorded BEFORE retirement — `terminate_node` alone would
    /// mark it `NodeCancelled`, an accidental mismatch this guards against
    /// by name. Retries the finalized-value take across a
    /// `TurnInFlight` race (the original one-shot fork path's own
    /// discipline). Consumes the window.
    pub(crate) async fn finalize_fork_data(mut self) -> Result<(Value, String), HarnessError> {
        let node = self.node;
        let (value, rendered) =
            retry_on_turn_in_flight(|| self.agent.take_finalized_value_keep_open(node)).await?;
        let _ = self
            .agent
            .tree()
            .node_done(node, "fork answer delivered".to_string());
        self.agent.terminate_node(node, "fork child retired")?;
        self.retired = true;
        Ok((value, rendered))
    }

    /// A failure ATTRIBUTABLE TO THIS CHILD's window (round exhaustion, a
    /// non-finalize suspension, a closure answer this driver cannot
    /// carry): retire with `reason`, producing nothing further. Consumes
    /// the window (see `drive_fork_child_agent_session`'s own doc for what
    /// happens AFTER this): a mechanism problem (closure, dispatcher-contract
    /// violation) still turns into a hard `Err`, but a fork child's own
    /// `InvocationExit` (round exhaustion, a non-answer ending, its provider
    /// call failing) becomes a plain-language corrective instead — fork
    /// children are not branch positions with a typed `Left` to fold into,
    /// so the corrective is delivered by aborting the block that was
    /// consuming this child, not by folding a `Left`. The retirement itself
    /// (this method) is identical either way.
    pub(crate) fn fold_exit(mut self, reason: &str) {
        let _ = self.agent.terminate_node(self.node, reason);
        self.retired = true;
    }
}

impl Drop for BranchAgentSessionGuard {
    /// Covers exactly the mechanism-failure path: a caller returns `Err(e)`
    /// via `?` before ever reaching [`Self::finalize_fork_data`]/
    /// [`Self::fold_exit`], and this guard simply goes out of scope.
    /// Idempotent with the two consuming methods (`retired` is set the
    /// instant either runs), so this never double-retires an
    /// already-finished window.
    fn drop(&mut self) {
        if !self.retired {
            tracing::warn!(
                node = ?self.node,
                realm = ?self.realm,
                scope = ?self.scope,
                "child window dropped without an explicit exit (mechanism failure)"
            );
            let _ = self.agent.terminate_node(
                self.node,
                "child window dropped without an explicit exit (mechanism failure)",
            );
        }
    }
}

impl SelfHarnessDriver {
    /// Service a `runLLMTurnFork @T`/`runLLMTurnFanout @T` suspension raised
    /// DIRECTLY by the AUTHORED outer loop (concurrent cognition windows) —
    /// `fan: Some(_)` for a fanout (`prompts` one per
    /// child, answered as `[T]`), `fan: None` for a single fork (answered as
    /// bare `T`, `single_prompt` the one task text). Unlike this driver's
    /// other fork-servicing path ([`Self::drain_answerer_fork`], which drives
    /// each child sequentially through the recursive pump), every child here
    /// gets its own freshly-minted answerer realm on the SHARED outer
    /// machine and is driven CONCURRENTLY, up to [`Self::concurrency_cap`]
    /// at once ([`Self::drive_fanout_child`]/[`buffer_unordered`]): only
    /// machine occupancy serializes a child's actual compile+run, everything
    /// else (assembling its prompt, awaiting the provider) overlaps freely.
    /// Completion order is never observable — results are re-sorted back to
    /// DECLARATION order before assembly.
    ///
    /// # The child-attributable / mechanism line
    ///
    /// This is the ONE place the two are separated, and the separation is the
    /// whole point of the verbs' `Either` shape:
    ///
    /// - A failure ATTRIBUTABLE TO ONE CHILD'S WINDOW — its rounds ran out, it
    ///   ended on something that is not an answer, its own provider call
    ///   failed — comes back from [`Self::drive_fanout_child`] as
    ///   `Ok(Err(exit))` and is folded as `Left exit` AT THAT CHILD'S BRANCH
    ///   POSITION. Its siblings' answers are unaffected: the whole reason
    ///   decision 6 exists is that an exception here erases results that were
    ///   already produced.
    /// - A failure of the MECHANISM — the fan cardinality check below, the
    ///   `Either`/list assembly against the table, session bookkeeping, the
    ///   per-loop inference-call runaway cap — still hard-fails the turn via
    ///   `?`. Laundering a broken mechanism into "the model failed" would put
    ///   a false receipt in front of the operator, which is precisely what
    ///   this codebase refuses.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn service_outer_fanout(
        &mut self,
        site: u32,
        ty: Option<&str>,
        modules: &[String],
        fan: Option<FanBadge>,
        single_prompt: &str,
        prompts: &[String],
        table: &DataConTable,
    ) -> Result<Value, DriverError> {
        self.lifecycle = SelfHarnessState::SuspendedOnHole;

        let is_fanout = fan.is_some();
        // A fanout site's recorded type is the LIST type (`[T]`); a plain
        // fork's is already the element type.
        let element_ty = if is_fanout {
            ty.and_then(engine::strip_list_type)
        } else {
            ty
        };
        // Single-vs-fanout normalization + cardinality integrity, via the
        // ONE home (`engine::fork_briefs`).
        let prompts: Vec<&str> = engine::fork_briefs(&fan, prompts, single_prompt)
            .map_err(|e| DriverError::Session(e.to_string()))?;

        for prompt in &prompts {
            self.emit(Event::RunLLMTurnHole {
                site,
                ty: element_ty.map(String::from),
                prompt: (*prompt).to_string(),
            });
        }

        let sid = self.outer_sid()?;
        let cap = self.concurrency_cap;
        // A shared borrow of `self` — every concurrent child needs only
        // `&self`-reachable state (the `Arc`-shared `agent`/`gate`, the
        // atomic counters, the plain round-cap config); none of them
        // outlives this `.await`, so no `Arc<Self>`/`tokio::spawn` is
        // needed (see `drive_fanout_child`'s doc for why `tokio::spawn`
        // itself doesn't fit here). `drive_concurrent` owns the
        // ordering/concurrency shell (buffer_unordered + re-sort to
        // DECLARATION order, since completion order is nondeterministic and
        // must never be observable in the resumed answer).
        let this = &*self;
        #[allow(clippy::type_complexity)]
        let results: Vec<(usize, Result<Result<Value, InvocationExit>, DriverError>)> =
            drive_concurrent(cap, prompts.len(), |idx| {
                let prompt = prompts[idx];
                async move {
                    this.drive_fanout_child(sid, site, idx, prompt, element_ty, modules, table)
                        .await
                }
            })
            .await;

        self.lifecycle = SelfHarnessState::RunningLoop;

        // Per-child assembly. The `?` on the OUTER `Result` is the mechanism
        // line: only a mechanism failure reaches it. The INNER `Result` is the
        // child's own outcome and becomes `Right`/`Left` at its position —
        // `engine::build_child_answer_value` follows `build_list_value`'s
        // loud-failure discipline (a `Left`/`Right`/`Exit*` constructor absent
        // from the turn's table is itself a mechanism failure, never a
        // defaulted value).
        let mut answers = Vec::with_capacity(results.len());
        for (idx, r) in results {
            let outcome = r?;
            if let Err(exit) = &outcome {
                tracing::warn!(
                    child = idx,
                    exit = %exit,
                    "fanout child exited without an answer — folding it as data at its \
                     branch position; siblings are unaffected"
                );
            }
            answers.push(
                engine::build_child_answer_value(outcome, table)
                    .map_err(|e| DriverError::Session(e.to_string()))?,
            );
        }

        if is_fanout {
            engine::build_list_value(answers, table)
                .map_err(|e| DriverError::Session(e.to_string()))
        } else {
            answers
                .into_iter()
                .next()
                .ok_or_else(|| DriverError::Session("outer fork produced no answer".into()))
        }
    }

    /// Drive ONE fanout/fork child to `finalize`, from scratch: mint a fresh
    /// answerer node ATTACHED to the shared outer session as its OWN realm
    /// (the "freshly-minted answerer realm" per window S1-L4 asks for —
    /// distinct from [`Self::answerer`], the single node the REUSED
    /// single-hole path drives), drive it through the ONE round loop
    /// ([`Self::drive_agent_session_to_finalize`], under
    /// [`AgentSessionExitPolicy::FinalizeOnly`] — a concurrent child supports
    /// `finalize` only; explore/define rounds and compile-error correction
    /// work exactly like the interactive path, but a nested
    /// `askUser`/`note`/`fork` suspension folds straight to
    /// `InvocationExit::NotFinalized` instead of being serviced — v1 scope,
    /// no operator-gate serialization or fork bookkeeping across siblings
    /// racing the same machine), and retire the node either way (realm
    /// scope-exit, never session removal — same discipline
    /// [`Self::retire_typed_request_agent`] uses for the reused answerer).
    ///
    /// `&self`, not `&mut self`: [`Self::service_outer_fanout`] runs up to
    /// [`Self::concurrency_cap`] of these concurrently via
    /// `futures_util::stream::buffer_unordered`, all borrowing the SAME
    /// `&SelfHarnessDriver` for the duration of one `.await` — genuine
    /// `tokio::spawn` tasks would need `'static` ownership of driver state
    /// this borrow-based shape avoids entirely. Every `Harness` call this
    /// makes is `&self` too (`agent: Arc<Harness>`); the two pieces of
    /// driver state a round loop mutates (`loop_inference_calls`,
    /// `iteration_realm`) are atomics for exactly this reason.
    ///
    /// The nesting of the return type is the contract: the OUTER `Result` is
    /// the MECHANISM (a hard failure of this driver, which fails the turn),
    /// the INNER one is THIS CHILD'S WINDOW (`Err(exit)` folds as `Left` at
    /// its branch position). See [`Self::service_outer_fanout`]'s doc for the
    /// line between them. The node is retired either way — a child that exits
    /// without an answer still releases its realm and scope.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn drive_fanout_child(
        &self,
        sid: tidepool_repr::SessionId,
        site: u32,
        idx: usize,
        prompt: &str,
        element_ty: Option<&str>,
        modules: &[String],
        table: &DataConTable,
    ) -> Result<Result<Value, InvocationExit>, DriverError> {
        let node = self.agent.create_root_framed(
            &format!("fanout answerer {idx}"),
            "",
            self.answerer_framing.clone(),
        )?;
        self.agent.force_attached(node, Actor::Operator, sid)?;
        self.agent.set_node_realm(node, self.mint_realm());
        // Sibling fanout children share this ONE session's machine — a
        // checkout race against another child's turn is expected, benign
        // contention (not a real conflict), so this node's checkouts WAIT
        // instead of failing fast. See `Harness::checkout_run_retrying`'s
        // doc for why `drive_turn` itself is never retried as a whole.
        self.agent.set_retry_checkout_on_contention(node, true);

        let result = self
            .finalize_fanout_child(node, site, idx, prompt, element_ty, modules, table)
            .await;
        let _ = self.agent.terminate_node(node, "fanout child retired");
        result
    }

    /// Seed `node` with this fanout/fork child's hole card, drive it through
    /// the shared pump ([`Self::drive_agent_session_to_finalize`], `FinalizeOnly`
    /// policy), and extract the finalized value.
    /// Split out of [`Self::drive_fanout_child`] only so that function's
    /// `terminate_node` always runs, on every return path here.
    ///
    /// # What is a typed exit here and what is not
    ///
    /// `Ok(Err(exit))` — THIS WINDOW ended without an answer, and nothing
    /// about the driver is broken: round exhaustion
    /// ([`InvocationExit::RoundsExhausted`]), a suspension on a
    /// non-`finalize` hole ([`InvocationExit::NotFinalized`], raised inside
    /// the shared pump under `FinalizeOnly`), or the window's own provider
    /// call failing ([`InvocationExit::RuntimeFailure`]).
    ///
    /// `Err(..)` — the MECHANISM is broken, and calling that "the model
    /// failed" would be a false receipt: the per-loop inference-call cap (a
    /// runaway HARNESS, not a runaway window — and it is shared, so the next
    /// child would trip it too); a finalized CLOSURE (the window DID answer,
    /// and this driver cannot carry the answer it gave — v1 scope, the gap is
    /// ours); or session/registry faults, and any `Harness` error that is not
    /// the window's own compile (handled in-loop) or provider call.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn finalize_fanout_child(
        &self,
        node: NodeId,
        site: u32,
        idx: usize,
        prompt: &str,
        element_ty: Option<&str>,
        modules: &[String],
        table: &DataConTable,
    ) -> Result<Result<Value, InvocationExit>, DriverError> {
        self.agent
            .set_answer_contract(node, self.answer_contract(element_ty, modules));
        let child_prompt = engine::finalize_typed_request_prompt(
            "The loop",
            prompt,
            element_ty,
            modules,
            Some(table),
            &self.agent.finalize_typed_request_prompt_effect_row(),
        );
        self.agent.push_user_turn(node, &child_prompt)?;
        self.emit(Event::TurnStart { node });

        let outcome = self
            .drive_agent_session_to_finalize(
                node,
                element_ty,
                site,
                0,
                &std::sync::atomic::AtomicU32::new(0),
                AgentSessionExitPolicy::FinalizeOnly { idx },
            )
            .await?;
        self.emit(Event::TurnEnd { node });

        // `FinalizeOnly` only ever returns `Ok(_)` with a `Finalize`
        // suspension (any other suspension is folded to `Err(NotFinalized)`
        // inside the shared pump), so this is the finalize extraction only.
        if let Some(exit) = outcome.err() {
            return Ok(Err(exit));
        }

        if self.agent.finalize_is_closure(node) {
            // NOT a typed exit: the window DID answer, and it is this driver
            // that cannot carry a closure across the fanout join (v1 scope).
            // Reporting our own gap as the child's failure would be a false
            // receipt — see `service_outer_fanout`'s doc.
            return Err(DriverError::Session(format!(
                "fanout child {idx} finalized a closure — a concurrent fanout/fork \
                 answer must be plain data in this driver (v1 scope)"
            )));
        }
        let (value, rendered) =
            retry_on_turn_in_flight(|| self.agent.take_finalized_value_keep_open(node)).await?;
        self.emit(Event::Finalize {
            node,
            value: rendered,
        });
        Ok(Ok(value))
    }

    /// Drive `node` (an answerer, already seeded with this hole's card)
    /// turn-by-turn until it suspends on `finalize`, applying the runaway
    /// caps: count each non-finalize model round; at [`TYPED_REQUEST_AGENT_NUDGE_ROUNDS`]
    /// push a one-time "finalize now" nudge; at [`TYPED_REQUEST_AGENT_MAX_ROUNDS`]
    /// hard-fail the hole; and abort the whole loop if the per-loop
    /// [`LOOP_INFERENCE_CALL_CAP`] is hit. A `Completed` (non-finalize) or
    /// `NoBlock` turn is treated as a wasted round — re-prompted toward
    /// `finalize` — rather than accepted, since the answerer's contract is to
    /// resolve the hole via `finalize`, not return a plain value.
    ///
    /// THE ONE PUMP (sol cross-family review finding 4): this is the ONLY
    /// model-session round loop in this driver — the reused single-hole
    /// answerer, a sequential branch/branch-fanout child, a recursive fork
    /// child, and a concurrent `runLLMTurnFork`/`runLLMTurnFanout` child
    /// ([`Self::drive_fanout_child`]) all drive through here. What genuinely
    /// differs between them is not the round loop — it is which suspensions
    /// get SERVICED once the node parks, captured by [`AgentSessionExitPolicy`]:
    /// [`AgentSessionExitPolicy::Interactive`] runs the full dispatcher
    /// (`askUser`/`note`/`fork`/green threads); [`AgentSessionExitPolicy::FinalizeOnly`]
    /// is what a concurrent fanout/fork child gets (v1 scope: no operator
    /// gate serialization or fork bookkeeping across siblings racing the same
    /// machine) — any non-finalize suspension folds straight to
    /// `InvocationExit::NotFinalized` DATA at that child's own position
    /// instead of being serviced.
    ///
    /// Each round `.await`s [`Harness::drive_turn`] directly — the resident
    /// JIT run it performs is CPU-blocking and sits inside this `async fn`
    /// unchanged; it already blocked a tokio worker before this method was
    /// `async` (called straight from async test bodies and `#[tokio::main]`
    /// with no bridge), so nothing about that changes here. It is not
    /// `spawn_blocking`'d: the resident session is not `Send`-shaped for
    /// that, and doing so is a separate piece of work.
    /// The return NESTING is the child-attributable/mechanism line: `Ok(Err(exit))`
    /// means THIS WINDOW ended without an answer (round exhaustion, its own
    /// provider call failing, or — under `FinalizeOnly` — a non-finalize
    /// suspension), `Err(..)` means the mechanism is broken (the per-loop
    /// inference-call cap, session faults). Whether an exit is DATA or fatal
    /// is the CALLER's to decide, because it depends on whether the window
    /// sits at a branch position: [`Self::drive_fanout_child`] folds it as
    /// `Left` at that branch, while [`Self::service_typed_request_suspension`] —
    /// answering IN CONTEXT on the outer turn's own continuation, with no
    /// siblings and no position — still hard-fails, exactly as before;
    /// [`Self::drive_fork_child_agent_session`] — a recursive fork/async-fork
    /// child, also with no branch position — turns it into a plain-language
    /// corrective instead of either of those (operator decision, 2026-08-24):
    /// the child's own node still retires and reports `node_failed`, but the
    /// caller aborts only the block that was consuming this child, not the
    /// parent's whole turn.
    pub(crate) async fn drive_agent_session_to_finalize(
        &self,
        node: NodeId,
        ty: Option<&str>,
        site: u32,
        fork_depth: u32,
        fork_subtree: &std::sync::atomic::AtomicU32,
        policy: AgentSessionExitPolicy,
    ) -> Result<Result<TurnOutcome, InvocationExit>, DriverError> {
        let ty_label = ty.unwrap_or("A");
        let max_rounds = self.answerer_max_rounds;
        let nudge_rounds = self.answerer_nudge_rounds;
        // The glide: at `max_rounds`, ONE explicit ultimatum ("your next
        // reply must be the minimal honest finalize") and two grace rounds
        // before the hard fail — a window that wedges on an expressible
        // answer (the live 2026-08-14 incident burned 32 rounds on a
        // spelling it was never told) gets a direct instruction first, and
        // only a window that cannot even comply takes the loop down.
        let hard_rounds = max_rounds.saturating_add(2);
        let mut rounds: u32 = 0;
        let mut nudged = false;
        let mut ultimatum = false;
        // Session-scoped (all rounds): total fork children, direct + green.
        // DEPTH CONTAINMENT: a child session's fork budget is the ordinary
        // per-window cap, forced to ZERO once `fork_depth` reaches
        // `max_fork_depth` — that's the depth-cap enforcement point. A zero
        // cap rides the existing loud-refusal machinery;
        // `fork_budget_refusal` teaches the boundary.
        let mut fork_budget = ForkBudget {
            cap: if fork_depth >= self.max_fork_depth {
                0
            } else {
                self.fork_budget_per_window
            },
            spent: 0,
        };
        // The subject named in this round's diagnostics — the reused
        // single-hole answerer under `Interactive`, or `fanout child {idx}`
        // under `FinalizeOnly` (finding 4: these messages used to live in
        // two separately-worded copies of this same loop).
        let subject = match policy {
            AgentSessionExitPolicy::Interactive => "runLLMTurn answerer".to_string(),
            AgentSessionExitPolicy::FinalizeOnly { idx } => format!("fanout child {idx}"),
        };
        'round: loop {
            let cap = self.loop_inference_call_cap;
            if self.loop_inference_calls.load(Ordering::SeqCst) >= cap {
                let ctx = match policy {
                    AgentSessionExitPolicy::Interactive => String::new(),
                    AgentSessionExitPolicy::FinalizeOnly { idx } => {
                        format!(" while servicing concurrent fanout child {idx}")
                    }
                };
                return Err(DriverError::Session(format!(
                    "per-loop inference-call cap ({cap}) reached{ctx} — \
                     hard-stopping the loop (a runaway harness)"
                )));
            }
            if rounds >= hard_rounds {
                // ROUND EXHAUSTION — this window's own budget, spent. Data
                // for a caller that has a branch position to fold it at;
                // `service_typed_request_suspension` still turns it into a hard failure.
                return Ok(Err(InvocationExit::RoundsExhausted(format!(
                    "{subject} exceeded {hard_rounds} rounds (cap {max_rounds} \
                     + ultimatum grace) without finalizing"
                ))));
            }
            if rounds >= max_rounds && !ultimatum {
                self.agent.push_user_turn(
                    node,
                    &format!(
                        "ROUND CAP REACHED. Your NEXT reply must be a single ```haskell \
                         block that ONLY finalizes — the minimal honest answer of type \
                         `{}` (a no-change/empty answer is acceptable and preferred over \
                         anything elaborate). Nothing else will be accepted.",
                        display_ty(ty_label)
                    ),
                )?;
                ultimatum = true;
            }
            if rounds == nudge_rounds && !nudged {
                self.agent.push_user_turn(
                    node,
                    &format!(
                        "Reminder: you have used {rounds} of {max_rounds} model rounds on \
                         this request. Budget the remainder — finalize as soon as another \
                         round would not improve the answer, and no later than round \
                         {max_rounds}: evaluate `finalize @{} value`.",
                        display_ty(ty_label)
                    ),
                )?;
                nudged = true;
            }

            self.loop_inference_calls.fetch_add(1, Ordering::SeqCst);
            rounds += 1;
            let outcome = self.agent.drive_turn(node).await;
            // A retry loop that burns rounds must be visible while it is
            // happening, not reconstructable afterwards — one `AnswererRound` per round that reached a
            // compile attempt, `error: None` on success regardless of what the
            // block went on to do, PLUS a `NoBlock` reply (no compile
            // attempt at all, but still a round the operator watched pass).
            // `round_progress` mirrors the same three-way split onto the
            // operator gate so it is visible live, not only in the durable
            // log.
            match &outcome {
                Ok(TurnOutcome::Suspended { .. } | TurnOutcome::Completed { .. }) => {
                    self.emit(Event::AnswererRound {
                        node,
                        site,
                        round: rounds,
                        error: None,
                    });
                    let gate = self.resolve_gate(&FormSource::Answerer { node });
                    gate.round_progress(rounds, None);
                    // Show the operator what the answerer actually ran —
                    // once per COMPILED round (a failed compile has no
                    // executed source to show; `post_turn_source` is a
                    // default-no-op on headless gates). Routed per-node like
                    // asks/notes: a labeled branch child's turns belong on
                    // its own section, not the default one.
                    if let Some(src) = self.agent.last_turn_source(node) {
                        gate.post_turn_source(&src);
                    }
                }
                Err(HarnessError::Compile(msg)) => {
                    self.emit(Event::AnswererRound {
                        node,
                        site,
                        round: rounds,
                        error: Some(msg.clone()),
                    });
                    self.resolve_gate(&FormSource::Answerer { node })
                        .round_progress(rounds, Some(msg.as_str()));
                }
                Ok(TurnOutcome::NoBlock { .. }) => {
                    // Previously silent: no `Event` and no gate call at all —
                    // the operator saw nothing pass while the answerer burned
                    // an empty-reply round. `NoBlock` never reaches a
                    // compile, so it is not a round for the corrective-retry
                    // fold's purposes, but it IS a round the operator should
                    // see go by.
                    self.emit(Event::AnswererRound {
                        node,
                        site,
                        round: rounds,
                        error: Some(NO_HASKELL_BLOCK_ROUND_ERROR.to_string()),
                    });
                    self.resolve_gate(&FormSource::Answerer { node })
                        .round_progress(rounds, Some(NO_HASKELL_BLOCK_ROUND_ERROR));
                }
                Err(_) => {}
            }
            match outcome {
                Ok(out @ TurnOutcome::Suspended { .. }) => {
                    // The round's SERVICING DISPATCHER. A Finalize suspension
                    // is the answer. Everything else is serviced and the
                    // fresh suspension re-dispatched, so the four families
                    // COMPOSE in any order within one block: operator forms
                    // (`service_askuser_hole`), fork delegation
                    // (`drain_answerer_fork` — REUSED, not reimplemented),
                    // green threads (`service_green_round` — the
                    // answerer-plane scheduler behind `async (fork @T …)`),
                    // and the mechanical resumes (`note`/`getStateJson`/
                    // `delegate`, via `drain_note_holes`). Any OTHER
                    // suspension is a hard error: the scoped answerer stack
                    // (`[AskUser, Fork, ReadState, Green, Finalize]`) can
                    // reach nothing else, and this driver has no operator
                    // for it.
                    //
                    // `green` is ROUND-scoped (one compile = one table for
                    // every chain); at the round's end it is SWEPT — a
                    // thread still running when the block finalizes or
                    // completes is dropped, and the corrective prompt names
                    // the count when anything was.
                    let TurnOutcome::Suspended { hole, classified } = out else {
                        unreachable!("matched TurnOutcome::Suspended above");
                    };
                    // `FinalizeOnly` (a concurrent fanout/fork child, v1
                    // scope) never reaches the dispatcher below: `finalize`
                    // is the answer, and everything else — including a
                    // mechanical `note`/`getStateJson` — folds straight to
                    // `NotFinalized` DATA at this child's own branch
                    // position, since there is no per-child operator-gate
                    // serialization or fork bookkeeping to service it with.
                    if let AgentSessionExitPolicy::FinalizeOnly { idx } = policy {
                        return match &classified.routing {
                            SuspensionRouting::Finalize { .. } => {
                                Ok(Ok(TurnOutcome::Suspended { hole, classified }))
                            }
                            other => Ok(Err(InvocationExit::NotFinalized(format!(
                                "fanout child {idx} suspended on a non-finalize hole \
                                 ({other:?}) — a concurrent fanout/fork child cannot \
                                 present an operator form, note, or nested fork in this \
                                 driver (v1 scope)"
                            )))),
                        };
                    }
                    let mut green: Option<ModelRoundGreenThreadScheduler> = None;
                    let mut current = Some((hole, classified));
                    let finalized: Option<TurnOutcome> = loop {
                        // Mechanical holes first (note/getStateJson/delegate)
                        // — the block may read `note "..." >> choose [...]`,
                        // so the current hole is routinely `Note`, not the
                        // thing that follows it.
                        let Some((hole, classified)) = current.take() else {
                            break None;
                        };
                        let Some((hole, classified)) = self
                            .sweep_green_on_err(
                                node,
                                &mut green,
                                self.drain_note_holes(node, hole, classified).await,
                            )
                            .await?
                        else {
                            break None;
                        };
                        match &classified.routing {
                            SuspensionRouting::Finalize { .. } => {
                                break Some(TurnOutcome::Suspended { hole, classified });
                            }
                            SuspensionRouting::AskUser { shape } => {
                                match self
                                    .sweep_green_on_err(
                                        node,
                                        &mut green,
                                        self.service_askuser_hole(node, shape).await,
                                    )
                                    .await?
                                {
                                    Some(TurnOutcome::Suspended {
                                        hole: h,
                                        classified: c,
                                    }) => current = Some((h, c)),
                                    Some(_) | None => break None,
                                }
                            }
                            SuspensionRouting::Fork { .. } => {
                                match self
                                    .sweep_green_on_err(
                                        node,
                                        &mut green,
                                        self.drain_answerer_fork(
                                            node,
                                            ty_label,
                                            &mut fork_budget,
                                            fork_depth,
                                            fork_subtree,
                                        )
                                        .await,
                                    )
                                    .await?
                                {
                                    Some(TurnOutcome::Suspended {
                                        hole: h,
                                        classified: c,
                                    }) => current = Some((h, c)),
                                    // Completed without finalizing (or the
                                    // budget refused a fork) —
                                    // `drain_answerer_fork` already returned
                                    // the node to Running and pushed its
                                    // corrective.
                                    Some(_) | None => {
                                        if let Some(g) = green.as_mut() {
                                            let dropped = self.sweep_green_round(node, g).await;
                                            if dropped > 0 {
                                                self.agent.push_user_turn(
                                                    node,
                                                    &dropped_threads_warning(dropped),
                                                )?;
                                            }
                                        }
                                        continue 'round;
                                    }
                                }
                            }
                            SuspensionRouting::Green => {
                                // The `g` borrow must end before
                                // `sweep_green_on_err` can reborrow `green`
                                // mutably to sweep it on an `Err`.
                                let green_result = {
                                    let g = green
                                        .get_or_insert_with(ModelRoundGreenThreadScheduler::new);
                                    self.service_green_round(
                                        node,
                                        g,
                                        &mut fork_budget,
                                        fork_depth,
                                        fork_subtree,
                                        ty_label,
                                    )
                                    .await
                                };
                                match self
                                    .sweep_green_on_err(node, &mut green, green_result)
                                    .await?
                                {
                                    GreenRoundExit::NodeParked => {
                                        current = self
                                            .agent
                                            .pending_suspension_full(node)
                                            .map(|(h, c, _)| (h.0, c));
                                    }
                                    GreenRoundExit::NodeDone => break None,
                                    // A thread's fork was refused: abort the
                                    // block (the node is parked on its own
                                    // green join — refuse that hole), sweep
                                    // the round's threads, and push the
                                    // budget corrective. The WINDOW survives.
                                    GreenRoundExit::ForkBudgetRefused { msg } => {
                                        let dropped = match green.as_mut() {
                                            Some(g) => self.sweep_green_round(node, g).await,
                                            None => 0,
                                        };
                                        retry_on_turn_in_flight(|| {
                                            self.agent.refuse_pending_suspension(node, msg.clone())
                                        })
                                        .await?;
                                        let warn = if dropped > 0 {
                                            format!("\n\n{}", dropped_threads_warning(dropped))
                                        } else {
                                            String::new()
                                        };
                                        self.agent.push_user_turn(node, &format!("{msg}{warn}"))?;
                                        continue 'round;
                                    }
                                    // Async misuse: same loud-refusal shape
                                    // as the budget — the block dies, the
                                    // SESSION survives with a corrective.
                                    // One model slip must not end the run.
                                    GreenRoundExit::AsyncMisuse { msg } => {
                                        let dropped = match green.as_mut() {
                                            Some(g) => self.sweep_green_round(node, g).await,
                                            None => 0,
                                        };
                                        let corrective = format!(
                                            "Async misuse — the block was aborted; your \
                                             session continues and earlier rounds' \
                                             definitions/bindings persist. Problem: {msg}."
                                        );
                                        retry_on_turn_in_flight(|| {
                                            self.agent
                                                .refuse_pending_suspension(node, corrective.clone())
                                        })
                                        .await?;
                                        let warn = if dropped > 0 {
                                            format!("\n\n{}", dropped_threads_warning(dropped))
                                        } else {
                                            String::new()
                                        };
                                        self.agent
                                            .push_user_turn(node, &format!("{corrective}{warn}"))?;
                                        continue 'round;
                                    }
                                    // A thread's fork child ended in
                                    // `InvocationExit` rather than
                                    // finalizing: same loud-refusal shape —
                                    // the block dies (this operator
                                    // decision — see `GreenRoundExit::ForkChildFailed`'s
                                    // doc), the SESSION survives with a
                                    // corrective naming the child by its
                                    // path. The child's own node already
                                    // retired and reported `node_failed`
                                    // inside `drive_fork_child_agent_session`.
                                    GreenRoundExit::ForkChildFailed { msg } => {
                                        let dropped = match green.as_mut() {
                                            Some(g) => self.sweep_green_round(node, g).await,
                                            None => 0,
                                        };
                                        retry_on_turn_in_flight(|| {
                                            self.agent.refuse_pending_suspension(node, msg.clone())
                                        })
                                        .await?;
                                        let warn = if dropped > 0 {
                                            format!("\n\n{}", dropped_threads_warning(dropped))
                                        } else {
                                            String::new()
                                        };
                                        self.agent.push_user_turn(node, &format!("{msg}{warn}"))?;
                                        continue 'round;
                                    }
                                }
                            }
                            other => {
                                // A suspension this driver cannot service.
                                // Hard error rather than silently hanging.
                                if let Some(g) = green.as_mut() {
                                    self.sweep_green_round(node, g).await;
                                }
                                return Err(DriverError::Session(format!(
                                    "runLLMTurn answerer suspended on a hole this driver \
                                     has no operator for ({other:?})"
                                )));
                            }
                        }
                    };
                    let dropped = match green.as_mut() {
                        Some(g) => self.sweep_green_round(node, g).await,
                        None => 0,
                    };
                    if let Some(answer) = finalized {
                        // Finalize won; a still-running thread losing the
                        // race to it is the documented spawn-and-wait-in-one-
                        // block contract, swept silently above.
                        return Ok(Ok(answer));
                    }
                    // The chain resolved (the answerer's block completed)
                    // WITHOUT finalize — same corrective retry as a plain
                    // Completed turn below, and it must say the same thing:
                    // this is the EXPECTED batch-per-round idiom the suffix
                    // teaches (fork a wave, wait, end the round), not a
                    // failure to scold (companion dogfood, 2026-08-13's
                    // Completed-arm fix, mirrored here — see that arm's
                    // comment). Servicing resumes are NOT model rounds:
                    // `rounds` stays untouched, only this outer loop repeats.
                    self.agent.reopen_node(node)?;
                    let ty_disp = display_ty(ty_label);
                    let warn = if dropped > 0 {
                        format!("\n\n{}", dropped_threads_warning(dropped))
                    } else {
                        String::new()
                    };
                    self.agent.push_user_turn(
                        node,
                        &format!(
                            "Round complete — your session continues, and that round's \
                             definitions and bindings (including everything you waited \
                             on) persist. The request still awaits its answer: explore \
                             further, fork another batch, or evaluate \
                             `finalize @{ty_disp} value` when ready (that ends the \
                             session).{warn}"
                        ),
                    )?;
                    continue;
                }
                // A plain value: the block ran to completion WITHOUT
                // `finalize`, so the node is now `Done`. Reopen it
                // (`Done`→`Running`) before the corrective re-prompt, so the
                // same accumulating node keeps driving toward `finalize` (a
                // wasted round, already counted).
                Ok(TurnOutcome::Completed { rendered }) => {
                    // A completed non-finalize round is a VALID explore/define
                    // round, not a failure — the window is multi-round by
                    // design, and scolding here taught the model that only
                    // `finalize` is admitted (companion dogfood, 2026-08-13:
                    // it reported exactly that, accurately). Acknowledge, SHOW
                    // the block's value (GHCi parity — the wave-per-round
                    // idiom needs the model to SEE what it bound, see
                    // `rendered_result_snippet`), and keep the request
                    // standing.
                    self.agent.reopen_node(node)?;
                    let ty_disp = display_ty(ty_label);
                    let shown = rendered_result_snippet(&rendered);
                    self.agent.push_user_turn(
                        node,
                        &format!(
                            "Round complete — your session continues, and that round's \
                             definitions/bindings persist. The block evaluated to:\n\
                             {shown}\n\
                             The request still awaits its \
                             answer: when ready, evaluate `finalize @{ty_disp} value` \
                             (that ends the session)."
                        ),
                    )?;
                }
                // An empty turn (no haskell block): the node is still `Running`
                // (no block ran), so no reopen — just re-prompt.
                Ok(TurnOutcome::NoBlock { .. }) => {
                    let ty_disp = display_ty(ty_label);
                    self.agent.push_user_turn(
                        node,
                        &format!(
                            "Your reply had no ```haskell block, so nothing ran. Reply \
                             with ```haskell blocks — an explore/define round is fine, \
                             or `finalize @{ty_disp} value` when ready."
                        ),
                    )?;
                }
                // A compile error: feed it back so the answerer can correct,
                // same as the corrective-retry loop in `run_to_hole_or_done`.
                // A wrong-typed `finalize` now lands HERE rather than crossing
                // in-heap and case-trapping — that is what pinning `finalize`
                // to the hole's type buys.
                Err(HarnessError::Compile(msg)) => {
                    let hint = self
                        .types_in_scope_hint(node, ty_label, &msg)
                        .unwrap_or_default();
                    // Parenthesize a compound answer type in the prompt — an
                    // unparenthesized `finalize @State -> State` is itself
                    // ill-typed advice. And do NOT teach single-shot: the
                    // window stays multi-round; a fixed block may be another
                    // define/explore round, with `finalize` whenever ready
                    // (the companion learned "declarations are forbidden"
                    // from the old wording — dogfood, 2026-08-13). For a
                    // multi-block reply, `msg` already leads with the sequence
                    // context (which blocks ran/persist, where to resume —
                    // `engine::sequence_failure_context`).
                    let ty_disp = display_ty(ty_label);
                    self.agent.push_user_turn(
                        node,
                        &format!(
                            "A block did not compile — your session continues; everything \
                             that already ran persists. Reply with corrected ```haskell \
                             blocks. Another define/explore round is fine (top-level \
                             declarations are welcome and persist); when you are ready to \
                             answer, evaluate \
                             `finalize @{ty_disp} value`.\n\nGHC error:\n{msg}{hint}"
                        ),
                    )?;
                }
                // A provider fault is THIS SESSION's own runtime failure —
                // decision 6's "runtime failure" class, shared by every
                // policy: a branch-position caller (fork child, labeled
                // branch, `FinalizeOnly` fanout child) folds it as `Left` at
                // that position instead of one transient 5xx erasing every
                // sibling's finished answer. The in-context caller
                // (`service_typed_request_suspension`) still collapses it to a hard
                // failure, unchanged. Every OTHER `HarnessError` is
                // driver/session machinery and hard-fails the turn.
                Err(HarnessError::Engine(EngineError::Provider(pe))) => {
                    return Ok(Err(InvocationExit::RuntimeFailure(format!(
                        "{subject} provider call failed: {pe}"
                    ))));
                }
                // The PINNED `Finalize <T>` row itself failed to resolve —
                // `EngineConfig::turn_target`'s row-validation probe runs
                // before the model's own block is even looked at, so this is
                // not an ordinary compile mistake in what the model wrote:
                // `T` is the hole's OWN answer type (set by the caller that
                // spawned this window, not by anything this window can
                // change), so every future round would fail identically —
                // same "this window's own request cannot be satisfied" shape
                // as the provider-fault arm above, not a driver/mechanism
                // fault. Detected by the exact "Not in scope" + the pinned
                // type's own name pattern `types_in_scope_hint` already keys
                // on for the ANALOGOUS `HarnessError::Compile` case above, so
                // a genuine OTHER setup failure (extract binary resolution,
                // cache IO, materializing the shim module) — which never
                // mentions this hole's type — still falls through to the
                // catch-all below and hard-fails as driver/session machinery.
                Err(HarnessError::Engine(EngineError::Setup(msg)))
                    if msg.contains("Not in scope") && msg.contains(ty_label) =>
                {
                    return Ok(Err(InvocationExit::RuntimeFailure(format!(
                        "{subject} could not resolve its own answer type `{}`: {msg}",
                        display_ty(ty_label)
                    ))));
                }
                Err(e) => return Err(e.into()),
            }
        }
    }

    /// F11: the shared shape of driving one classified `Fork` hole's
    /// children to completion and assembling their answer —
    /// [`Self::drain_answerer_fork`] (direct `fork`/`forkAll`) and
    /// [`Self::service_thread_ready`]'s Fork arm (a thread's own
    /// `async (fork …)`) used to carry this ~30-line sequence as
    /// near-verbatim twins (the same drift shape that produced F3's
    /// round-loop divergence): brief normalization, the fanout element type,
    /// per-child title, sequential [`Self::drive_fork_child_agent_session`] drive,
    /// per-child [`wrap_fork_value`], then single-vs-fanout assembly. The two
    /// callers differ only in the per-child title wording (plain vs
    /// `"async "`-prefixed) and in how the ASSEMBLED answer crosses back
    /// (a node-aware resume vs a raw thread resume) — both stay with the
    /// caller. `title_single`/`title_fanout_prefix` carry the wording
    /// difference (the WORD itself changes — "fork" vs "fanout" — not just a
    /// shared prefix, so a bare prefix+index wouldn't reproduce the original
    /// titles).
    ///
    /// `Ok(Err(msg))` means one child ended in `InvocationExit` — a plain-
    /// language corrective (already retired at its own node; see
    /// [`Self::drive_fork_child_agent_session`]) — and the caller must abort
    /// the consuming block with it rather than propagate a `DriverError`.
    /// Every brief in this call is driven CONCURRENTLY, up to
    /// [`Self::concurrency_cap`] at once, via [`drive_concurrent`] — the ONE
    /// ordering/concurrency shell [`Self::service_outer_fanout`] already
    /// rides for the sibling fanout path — so a sibling that already
    /// finished has already finalized and retired independently of this
    /// return value regardless of which brief (if any) ends in `Err`; the
    /// FIRST brief (by DECLARATION order, not completion order) whose result
    /// is a mechanism error or an `InvocationExit` is what this returns.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn drive_fork_children(
        &self,
        node: NodeId,
        title_single: &str,
        title_fanout_prefix: &str,
        site: crate::tree::SiteId,
        ty: Option<&str>,
        fan: &Option<FanBadge>,
        prompts: &[String],
        prompt: &str,
        source: engine::ForkSource,
        table: &DataConTable,
        fork_depth: u32,
        fork_subtree: &std::sync::atomic::AtomicU32,
        ty_label: &str,
    ) -> Result<Result<Value, String>, DriverError> {
        let briefs = engine::fork_briefs(fan, prompts, prompt)
            .map_err(|e| DriverError::Session(e.to_string()))?;
        let element_ty = match fan {
            None => ty,
            Some(_) => ty.and_then(engine::strip_list_type),
        };
        let titles: Vec<String> = if fan.is_none() {
            vec![title_single.to_string()]
        } else {
            (0..briefs.len())
                .map(|idx| format!("{title_fanout_prefix} {idx}"))
                .collect()
        };
        let cap = self.concurrency_cap;
        let briefs_ref = &briefs;
        let titles_ref = &titles;
        #[allow(clippy::type_complexity)]
        let results: Vec<(usize, Result<Result<Value, String>, DriverError>)> =
            drive_concurrent(cap, briefs.len(), |idx| {
                let brief = briefs_ref[idx];
                let title = titles_ref[idx].as_str();
                async move {
                    self.drive_fork_child_agent_session(
                        node,
                        title,
                        brief,
                        element_ty,
                        site.get(),
                        table,
                        fork_depth + 1,
                        fork_subtree,
                        ty_label,
                    )
                    .await
                }
            })
            .await;

        let mut answers = Vec::with_capacity(results.len());
        for (_, r) in results {
            let value = match r? {
                Ok(v) => v,
                Err(msg) => return Ok(Err(msg)),
            };
            answers.push(wrap_fork_value(source, value, table)?);
        }
        if fan.is_none() {
            let value = answers.pop().ok_or_else(|| {
                DriverError::Session("fork_briefs yielded no answer for a single fork".to_string())
            })?;
            Ok(Ok(value))
        } else {
            engine::build_list_value(answers, table)
                .map(Ok)
                .map_err(|e| DriverError::Session(e.to_string()))
        }
    }

    /// Drain a `SuspensionRouting::Fork` suspension on the per-loop answerer
    /// (`forkAll`/`fork` via `Tidepool.Fork`): resume it by driving each
    /// child to completion on the full pump row via
    /// [`Self::drive_fork_child_agent_session`] (fork-subsumes-split step 1 — a
    /// child can `askUser`, `fork` again, and go multi-round; it is not the
    /// one-shot general-Agent path), looping in case the parent immediately
    /// hits ANOTHER fork right after resuming (e.g. `forkAll` then `fork` in
    /// sequence). `Ok(Some(out))` means the parent landed on `Finalize` — the
    /// caller should `return Ok(out)` straight through, same as any other
    /// finalize suspension. `Ok(None)` means the parent's block ran to
    /// completion WITHOUT ever finalizing; this already reopened the node and
    /// pushed the same corrective nudge [`Self::drive_agent_session_to_finalize`]'s
    /// `Completed` arm uses, so the caller should just let its round loop
    /// keep driving. Any OTHER resumed hole (an operator form after the fork
    /// results, a `wait` on a thread spawned earlier in the block) is handed
    /// back to the dispatcher — composing fork with askUser/async in one
    /// block is an ordinary continuation.
    pub(crate) async fn drain_answerer_fork(
        &self,
        node: NodeId,
        ty_label: &str,
        budget: &mut ForkBudget,
        fork_depth: u32,
        fork_subtree: &std::sync::atomic::AtomicU32,
    ) -> Result<Option<TurnOutcome>, DriverError> {
        loop {
            let Some((hole, classified, table)) = self.agent.pending_suspension_full(node) else {
                return Err(DriverError::Session(
                    "fork resume: node has no pending hole to service".into(),
                ));
            };
            // The budget check covers EVERY fork this drain loop services,
            // not only the one the dispatcher saw — `forkAll` then `fork` in
            // sequence spends per iteration. Spend BEFORE spawn; a refusal
            // costs nothing.
            let cost = ForkBudget::cost(&classified.routing);
            if matches!(classified.routing, SuspensionRouting::Fork { .. }) {
                if let Some(msg) = self.check_fork_budgets(budget, cost, fork_subtree, ty_label) {
                    retry_on_turn_in_flight(|| {
                        self.agent.refuse_pending_suspension(node, msg.clone())
                    })
                    .await?;
                    self.agent.push_user_turn(node, &msg)?;
                    return Ok(None);
                }
            }
            // Children run as full sessions on the pump
            // (`drive_fork_child_agent_session` — fork-subsumes-split step 1).
            match &classified.routing {
                SuspensionRouting::Fork {
                    site,
                    ty,
                    fan,
                    prompts,
                    source,
                } => {
                    match self
                        .drive_fork_children(
                            node,
                            "fork answerer",
                            "fanout answerer",
                            *site,
                            ty.as_deref(),
                            fan,
                            prompts,
                            &classified.prompt,
                            *source,
                            &table,
                            fork_depth,
                            fork_subtree,
                            ty_label,
                        )
                        .await?
                    {
                        Ok(answer) => {
                            retry_on_turn_in_flight_async(|| {
                                self.agent.resume_with_value(node, &hole, answer.clone())
                            })
                            .await?;
                        }
                        // A child ended in `InvocationExit`: abort this
                        // block with the corrective, the same shape the
                        // budget-refusal branch above uses — the WINDOW
                        // survives, only the parked continuation dies.
                        Err(msg) => {
                            retry_on_turn_in_flight(|| {
                                self.agent.refuse_pending_suspension(node, msg.clone())
                            })
                            .await?;
                            self.agent.push_user_turn(node, &msg)?;
                            return Ok(None);
                        }
                    }
                }
                other => {
                    return Err(DriverError::Session(format!(
                        "drain_answerer_fork: expected a pending Fork hole, got {other:?}"
                    )));
                }
            }

            match self.agent.pending_suspension(node).map(|c| c.routing) {
                Some(SuspensionRouting::Fork { .. }) => continue,
                // Finalize, or any OTHER routing (an operator form after the
                // fork results, a `wait` on a thread spawned earlier in the
                // block): hand the outcome back — the dispatcher in
                // `drive_agent_session_to_finalize` routes it. Before the
                // answerer-plane green scheduler this arm hard-errored on
                // everything but Finalize; composing fork with askUser/async
                // in one block is now an ordinary continuation.
                Some(_) => {
                    return self
                        .agent
                        .pending_turn_outcome(node)
                        .map(Some)
                        .ok_or_else(|| {
                            DriverError::Session("fork resume: pending hole vanished".into())
                        });
                }
                None => break,
            }
        }

        self.agent.reopen_node(node)?;
        let ty_disp = display_ty(ty_label);
        self.agent.push_user_turn(
            node,
            &format!(
                "Round complete — the forked sub-answerers returned and their results \
                 are bound in your session (evaluate a binding to see it). The request \
                 still awaits its answer: fork another batch, keep working, or evaluate \
                 `finalize @{ty_disp} value` when ready (that ends the session)."
            ),
        )?;
        Ok(None)
    }

    /// Cleanup for a mechanism failure between a fork/branch child's GUI +
    /// tree-path registration and its [`BranchAgentSessionGuard`] guard coming into
    /// existence (F7): those registrations predate the guard, so nothing
    /// else retires them on an early `?` between them and
    /// `BranchAgentSessionGuard::from_lease` — `force_attached`/`mint_scope` are both
    /// fallible there (a checkout race is the F4 contention class; a dead
    /// parent scope is `ok_or_else`'d). Removes the `node_labels` entry and
    /// retires the GUI panel (when a label was registered), then retires the
    /// tree/session node itself through the ONE retirement path — safe
    /// whether or not `force_attached` ever ran: [`Harness::terminate_node`]
    /// is idempotent over a never-forced (still `Thunk`) node, and if
    /// `force_attached` DID succeed before the failure (a live `mint_scope`
    /// refusal), it also closes the realm the caller already assigned via
    /// [`Harness::set_node_realm`] — the "permanently Running tree node"
    /// half of the leak.
    pub(crate) fn abort_unguarded_child(&self, node: NodeId, label: Option<&str>, reason: &str) {
        if let Some(label) = label {
            self.node_labels.lock().remove(&node);
            self.gate.node_failed(label, reason);
            self.gate.retire_node(label);
        }
        self.fork_child_seq.lock().remove(&node);
        let _ = self.agent.terminate_node(node, reason);
    }

    /// Drive ONE
    /// fork child as a full ATTACHED WINDOW on the shared session. A child
    /// on the window pump can explore across rounds, present operator
    /// forms, and answer with a REAL `finalize @T` —
    /// `ResidentError::ChildSuspended` is unreachable from here.
    ///
    /// The attach ladder: transcript forked from the LIVE parent's
    /// checkpoint (`register_fork_child_with_card` — the multi-round
    /// answerer card), child scope minted from the LIVE parent node's
    /// scope — which IS the declaration-inheritance wiring on the shared
    /// session (the ancestry-scoping rule; no separate-session
    /// include dance), and the finalize contract pinned from the fork
    /// site's own resolved modules.
    ///
    /// This child gets its own operator-GUI/tree
    /// lifecycle — a derived label/path (`Self::fork_child_label`),
    /// `node_gate`/`node_seeded` at birth (the AUTHORED brief, not the
    /// composed hole card), `node_finalized`/`node_failed` at the fold, and
    /// `retire_node` on every exit, all through [`BranchAgentSessionGuard`]
    /// so a mechanism-error `?` before the pump starts can never leak the
    /// label/path registrations or skip retirement.
    ///
    /// Exit semantics (operator decision, 2026-08-24): a child that exits
    /// without finalizing is reported via `node_failed` and retired exactly
    /// like a success, but only a MECHANISM problem still hard-fails
    /// through as `Err` — a finalized CLOSURE (v1 scope cannot carry it), a
    /// non-finalize dispatcher-contract violation, or a `DriverError` from
    /// the pump itself. A child ending in `InvocationExit` (round
    /// exhaustion, a non-answer ending, its own provider call failing) is
    /// NOT one of those: fork children are not branch positions with a
    /// typed `Left` to fold into (that applies at the
    /// concurrent `runLLMTurnFork`/`Fanout` branch position, not here), so
    /// this driver instead returns `Ok(Err(corrective))` — a plain-language
    /// message the caller ([`Self::drive_fork_children`]) hands up to
    /// [`Self::drain_answerer_fork`]/[`Self::service_thread_ready`], which
    /// abort the CONSUMING block through the same `refuse_pending_suspension`
    /// corrective plumbing [`GreenRoundExit::ForkBudgetRefused`] already
    /// uses — the parent session and the run survive; only the block that
    /// was `wait`-ing/consuming this child dies. A successful child is
    /// marked `NodeDone` BEFORE resource
    /// retirement (`BranchAgentSessionGuard::finalize_fork_data`), never via
    /// `NodeCancelled`-through-`terminate_node`.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn drive_fork_child_agent_session(
        &self,
        parent: NodeId,
        title: &str,
        brief: &str,
        ty: Option<&str>,
        site: u32,
        table: &DataConTable,
        fork_depth: u32,
        fork_subtree: &std::sync::atomic::AtomicU32,
        ty_label: &str,
    ) -> Result<Result<Value, String>, DriverError> {
        let sid = self.outer_sid()?;
        let modules = self.agent.asks_modules(parent, site);
        let card = engine::finalize_typed_request_prompt(
            "Your parent session",
            brief,
            ty,
            &modules,
            Some(table),
            &self.agent.finalize_typed_request_prompt_effect_row(),
        );
        let node = self
            .agent
            .register_fork_child_with_card(parent, title, card)?;

        // Step 3 GUI lane: a fork child's label/path is DERIVED — see
        // `Self::fork_child_label`'s doc. Registered NOW, not on its first
        // ask/note, so the operator watches the tree grow.
        let label = self.fork_child_label(parent, brief);
        self.node_labels.lock().insert(node, label.clone());
        let _ = self.gate.node_gate(&label);
        self.gate.node_seeded(&label, brief);

        // Attach to the SHARED session: no per-node machine, no separate
        // decl plane — the child's turns run as a realm on the one machine.
        if let Err(e) = self.agent.force_attached(node, Actor::Operator, sid) {
            let reason = format!("fork child of {parent:?}: attach failed: {e}");
            self.abort_unguarded_child(node, Some(&label), &reason);
            return Err(e.into());
        }
        let realm = self.mint_realm();
        self.agent.set_node_realm(node, realm);
        // F4: a fork child's window can run concurrently against a sibling
        // fanout child (or another fork subtree entirely) on the SAME
        // shared outer session — opt in so its very first checkout (the
        // scope mint below, then every turn `drive_agent_session_to_finalize`
        // drives) waits instead of failing fast on what is "expected, benign
        // contention" everywhere else on this plane.
        self.agent.set_retry_checkout_on_contention(node, true);
        // Scope minted from the LIVE parent's scope: this is what makes the
        // parent's declarations (and its ancestors') readable and sibling
        // declarations invisible — the same scope-tree ancestry the branch
        // path gets from its frozen snapshot's scope.
        let parent_scope = self.agent.node_scope(parent);
        let child_scope = match self
            .agent
            .with_session_retrying(node, sid, |s| s.mint_scope(parent_scope))
            .await
        {
            Ok(Some(scope)) => scope,
            Ok(None) => {
                let reason = format!(
                    "fork child of {parent:?}: parent scope {parent_scope:?} is not live \
                     (its window already retired?)"
                );
                self.abort_unguarded_child(node, Some(&label), &reason);
                return Err(DriverError::Session(reason));
            }
            Err(e) => {
                let reason = format!("fork child of {parent:?}: mint_scope failed: {e}");
                self.abort_unguarded_child(node, Some(&label), &reason);
                return Err(DriverError::Session(reason));
            }
        };
        self.agent.set_node_scope(node, child_scope);
        self.agent
            .set_answer_contract(node, self.answer_contract(ty, &modules));
        self.emit(Event::TurnStart { node });

        // This child's mode, typed (`AgentSessionMode::require_one_shot`'s doc):
        // it answers exactly once, then is retired below.
        let lease = AgentSessionMode::OneShotBranch {
            node,
            realm,
            scope: child_scope,
        };
        // Every exit below this point retires exactly through `window`
        // (`fold_exit`, `finalize_fork_data`, or — for a mechanism-error `?`
        // ABOVE this point, before the guard exists — a hand-rolled cleanup
        // would be needed; there is none between here and the guard's
        // construction). See `BranchAgentSessionGuard`'s doc.
        let window = BranchAgentSessionGuard::from_lease(lease, self.agent.clone())?;

        // Box::pin: the pump drives child pumps (a fork child can itself
        // present forms, and — step 2 — fork), so this call is genuinely
        // recursive; the indirection is the async-recursion requirement,
        // nothing more.
        let outcome = Box::pin(self.drive_agent_session_to_finalize(
            node,
            ty,
            site,
            fork_depth,
            fork_subtree,
            AgentSessionExitPolicy::Interactive,
        ))
        .await;
        self.emit(Event::TurnEnd { node });

        // Every path below this point is done with this node's own GUI
        // registration — see the insert above.
        let retired_label = self.node_labels.lock().remove(&node);
        if let Some(label) = &retired_label {
            self.gate.retire_node(label);
        }
        // This node retires here regardless of outcome below — purge its
        // own fork-child-label counter (it may have spawned children of its
        // own) at the same point its other per-node bookkeeping goes, so
        // `fork_child_seq` does not grow without bound across a long-running
        // companion tree (sol cross-family review finding 11).
        self.fork_child_seq.lock().remove(&node);

        match outcome {
            Ok(Ok(TurnOutcome::Suspended { classified, .. }))
                if matches!(classified.routing, SuspensionRouting::Finalize { .. }) =>
            {
                if self.agent.finalize_is_closure(node) {
                    let reason = format!(
                        "fork child {node:?} finalized a closure — a fork answer must \
                         be plain data in this driver (v1 scope)"
                    );
                    if let Some(label) = &retired_label {
                        self.gate.node_failed(label, &reason);
                    }
                    window.fold_exit(&reason);
                    return Err(DriverError::Session(reason));
                }
                let (value, rendered) = window
                    .finalize_fork_data()
                    .await
                    .map_err(|e| DriverError::Session(format!("fork child finalize take: {e}")))?;
                if let Some(label) = &retired_label {
                    self.gate.node_finalized(label, &rendered);
                }
                self.emit(Event::Finalize {
                    node,
                    value: rendered,
                });
                Ok(Ok(value))
            }
            Ok(Ok(other)) => {
                let reason = format!(
                    "fork child {node:?} returned a non-finalize outcome from the pump \
                     ({}) — dispatcher contract violation",
                    turn_outcome_tag(&other)
                );
                if let Some(label) = &retired_label {
                    self.gate.node_failed(label, &reason);
                }
                window.fold_exit(&reason);
                Err(DriverError::Session(reason))
            }
            // Operator decision (2026-08-24): a child ending in
            // `InvocationExit` no longer kills the parent's turn — the
            // child's own node still retires and reports `node_failed`
            // (unchanged), but instead of hard-failing through as `Err`
            // this returns `Ok(Err(corrective))`, a plain-language message
            // (docs/GLOSSARY.md prompt rules: no `InvocationExit`, no
            // constructor name) naming the child by its derived path — the
            // caller aborts only the block that was consuming this child.
            Ok(Err(exit)) => {
                let reason = format!("fork child {node:?} ended without an answer: {exit}");
                if let Some(label) = &retired_label {
                    self.gate.node_failed(label, &reason);
                }
                window.fold_exit(&reason);
                let path = retired_label.clone().unwrap_or_else(|| format!("{node:?}"));
                Ok(Err(fork_child_failure_corrective(&path, &exit, ty_label)))
            }
            Err(e) => {
                let reason = "fork child retired (mechanism failure)";
                if let Some(label) = &retired_label {
                    self.gate.node_failed(label, &format!("{reason}: {e}"));
                }
                window.fold_exit(reason);
                Err(e)
            }
        }
    }

    /// Derive a stable GUI
    /// label/tree-path for a fork child. Unlike `runLLMTurnBranchLabeled`,
    /// a fork's brief carries no wire-carried label, so both the label and
    /// the path segment are derived here rather than read off the wire.
    ///
    /// Base path: the parent's own registered GUI label (`node_labels` —
    /// the parent is itself a labeled branch/fork child), else the fixed
    /// root id `"root"` (mirrors `tidepool_web::DEFAULT_NODE_ID` as a
    /// literal — this crate cannot depend on `tidepool-web`).
    ///
    /// Child segment: `f<idx>-<ascii-slug-of-brief-prefix>`, mirroring a
    /// structurally-labeled branch's own `root/1-child` convention so a
    /// fork child's tree position reads the same way. `idx` is a per-PARENT
    /// monotonic counter (`Self::fork_child_seq`), not threaded in from the
    /// caller — see that field's doc for why.
    pub(crate) fn fork_child_label(&self, parent: NodeId, brief: &str) -> String {
        let idx = {
            let mut seq = self.fork_child_seq.lock();
            let counter = seq.entry(parent).or_insert(0);
            let idx = *counter;
            *counter += 1;
            idx
        };
        let base = self
            .node_labels
            .lock()
            .get(&parent)
            .cloned()
            .unwrap_or_else(|| "root".to_string());
        format!("{base}/{}", fork_child_path_segment(idx, brief))
    }
}
