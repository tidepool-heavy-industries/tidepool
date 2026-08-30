//! The green-thread scheduler: thread table, waiter map,
//! and FIFO ready queue backing `Tidepool.Async`'s `fork`-composed
//! `async`/`wait` idiom, scoped to one `run_loop_fragment_inner` call
//! (structured concurrency — nothing survives past the `loop` fragment that
//! spawned it).

use std::collections::{HashMap, VecDeque};

use tidepool_bridge::ToCore;
use tidepool_eval::value::Value;
use tidepool_repr::DataConTable;
use tidepool_runtime::session::{ResidentHole, ResidentOutcome};

use super::fork::{drive_concurrent, ForkBudget};
use super::SelfHarnessDriver;
use super::*;
use crate::engine::{self, ClassifiedSuspension, SuspensionRouting};
use crate::harness::{HarnessError, Session};
use crate::selfharness::observer::FormSource;
use crate::tree::{FanBadge, HoleId, NodeId};

// --- Green threads --------------------------------------------------------
//
// The scheduler is entirely LOCAL to one `run_loop_fragment_inner` call —
// every thread a loop spawns is structured-concurrency-scoped to that one
// `loop` fragment run; nothing here survives as driver state across loops.

/// Which control-flow chain a suspension belongs to. Chain is invariant
/// across a resume (resuming a hole continues the SAME chain into whatever
/// it suspends on next); only starting a freshly spawned thread introduces a
/// new one. Needed because `AsyncDoneWith`'s own leading `Int` field is
/// always the dummy `0` `asyncSpawn` bakes in (the wrapping closure is built
/// before its real thread id is known, `tidepool-mcp/src/effect_defs.rs`) —
/// the driver identifies which thread settled by WHICH chain reached
/// `AsyncDoneWith`, never by decoding that field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GreenChain {
    /// The outer `loop`'s own top-level continuation, or transitively
    /// whatever spawned the thread that (recursively) spawned this chain.
    Primary,
    Thread(i64),
}

/// The driver's bookkeeping for one green thread: which realm its frames
/// park under (the unit `cancel` closes) and its terminal-state sum.
pub(crate) struct GreenThread {
    pub(crate) realm: tidepool_codegen::suspension::RealmId,
    pub(crate) state: GreenThreadState,
}

pub(crate) enum GreenThreadState {
    Running,
    Settled,
    Cancelled,
}

/// One already-produced suspension (or completion) waiting to be classified
/// and serviced — the scheduler's FIFO ready queue. `chain` is threaded
/// through unchanged so a later `AsyncDoneWith`/wake can attribute correctly;
/// order is the driver's business only — the representation-pinning
/// contract is that resuming ready work in EITHER order produces identical
/// results, so this queue just picks one (FIFO).
pub(crate) struct GreenReady {
    pub(crate) chain: GreenChain,
    pub(crate) outcome: ResidentOutcome,
}

/// How a serviced green suspension's own resume is DELIVERED — the one
/// genuinely plane-specific seam in green servicing (dup-e finding 2).
/// Thread frames and the authored outer loop resume RAW on the shared
/// machine session and re-enter the ready queue; the answerer NODE's own
/// chain must resume through the node-aware path
/// (`Harness::resume_with_value`), which
/// restores the node's runtime resource scope and keeps its pending record
/// truthful. Everything ABOVE this seam — constructor decode, thread-table
/// transitions, realm minting, waiter wakes — is ONE implementation
/// ([`SelfHarnessDriver::service_green_hole`]), not two.
pub(crate) enum GreenDelivery<'a> {
    Raw,
    Node { node: NodeId, hole: &'a HoleId },
}

/// What one popped ready item resolved to, and what the scheduler owes it in
/// response — the classified-hole dispatcher's return value instead of each
/// arm independently pushing to `ready`/breaking the loop. A new arm that
/// computes a next outcome and forgets to hand it back through one of these
/// is a compile error: it has nothing else to return.
///
/// [`SuspensionRouting::Green`] is the deliberate exception, checked and rejected
/// before this was written for the rest: a single Green suspension can
/// settle into zero, one, or two ready continuations (a spawn resumes the
/// spawner AND starts the new thread; a join with no terminal candidate
/// parks with none and touches no hole; a settle or cancel can wake an
/// arbitrary number of parked waiters), and it mutates the thread table and
/// waiter map alongside `ready`. No two-or-three-variant sum expresses
/// "zero to N pushes plus a table mutation" without degrading to a `Vec` or
/// a payload-free `Handled` marker that types nothing a `Result<(),
/// DriverError>` didn't already type — so
/// [`SelfHarnessDriver::service_green_hole`] keeps owning `ready`/the
/// thread table/the waiter map directly instead of returning one of these.
pub(crate) enum ServicedSuspension {
    /// The popped item was itself terminal — the PRIMARY chain's `loop` has
    /// finished.
    Completed { result: Value, table: DataConTable },
    /// The hole was resumed; its next outcome re-enters the ready queue
    /// under the same chain.
    Resumed(GreenReady),
    /// The hole was left parked, unresumed — reinserted into the ready
    /// queue so a later iteration revisits it (`RepoEventAwait`'s
    /// empty-poll case).
    LeaveParked(GreenReady),
}

/// Pull a plain `Int` field out of a Green request Con — every
/// thread-id-shaped field (`AsyncJoinAnyWith`'s elements, `AsyncStatusWith`/
/// `AsyncCancelWith`'s leading arg) shares this decode.
pub(crate) fn green_int_field(request: &Value, idx: usize, table: &DataConTable) -> i64 {
    let Value::Con(_, fields) = request else {
        return 0;
    };
    fields
        .get(idx)
        .map(|p| tidepool_runtime::value_to_json(p, table, 0))
        .and_then(|j| j.as_i64())
        .unwrap_or(0)
}

/// Pull an `[Int]` field out of a Green request Con (`AsyncJoinAnyWith`'s
/// sole field).
pub(crate) fn green_int_list_field(request: &Value, idx: usize, table: &DataConTable) -> Vec<i64> {
    let Value::Con(_, fields) = request else {
        return Vec::new();
    };
    fields
        .get(idx)
        .map(|p| tidepool_runtime::value_to_json(p, table, 0))
        .and_then(|j| j.as_array().cloned())
        .map(|arr| arr.iter().filter_map(|v| v.as_i64()).collect())
        .unwrap_or_default()
}

/// Round-scoped scheduler state for green threads spawned by an ANSWERER
/// window's own block (`async (fork @T brief)` and friends) — the
/// answerer-plane sibling of the loop-scoped locals
/// [`SelfHarnessDriver::run_loop_fragment_inner`] owns for the AUTHORED
/// outer loop. The plane split mirrors the fork machinery's own
/// ([`SelfHarnessDriver::drive_fork_child_agent_session`] vs
/// [`SelfHarnessDriver::service_outer_fanout`]): thread chains are serviced
/// by the SAME [`SelfHarnessDriver::service_green_hole`] (raw session
/// resumes — correct for thread frames, which live under their own realms),
/// while every resume of the NODE's own turn goes through the node-aware
/// path (`Harness::resume_with_value`), which
/// restores the node's realm/scope and keeps its pending-hole record
/// truthful — a raw `with_session` resume would run the node's continuation
/// under `OUTER_REALM` and leave its bookkeeping stale.
///
/// ROUND-scoped, exactly as the outer scheduler is fragment-scoped: one
/// round = one compile = one `DataConTable`/asks sidecar shared by every
/// chain, which is what lets thread suspensions classify against the node's
/// pending artifacts. A thread still running when its round ends is SWEPT
/// (realm closed, frames dropped) — spawn and wait belong in the same
/// block, and the corrective prompt says so when anything was dropped.
pub(crate) struct ModelRoundGreenThreadScheduler {
    threads: HashMap<i64, GreenThread>,
    /// THREAD-chain joiners only. The node's own `wait` never registers
    /// here — [`SelfHarnessDriver::service_green_hole`]'s
    /// `wake_green_waiters` resumes waiters RAW, which must never touch the
    /// node chain; the node's blocked join is instead re-checked (a pure
    /// winner scan, no session touch) each scheduler iteration.
    waiters: HashMap<i64, Vec<(GreenChain, String)>>,
    ready: VecDeque<GreenReady>,
    next_tid: i64,
}

impl ModelRoundGreenThreadScheduler {
    pub(crate) fn new() -> Self {
        ModelRoundGreenThreadScheduler {
            threads: HashMap::new(),
            waiters: HashMap::new(),
            ready: VecDeque::new(),
            next_tid: 1,
        }
    }
}

/// How [`SelfHarnessDriver::service_green_round`] hands control back to
/// the answerer dispatcher: the node's own turn parked on a non-Green hole
/// (route it), ran to completion without finalizing (corrective retry), a
/// thread's fork was refused by the session's fork budget (abort the
/// block, corrective retry naming the budget), the block misused the
/// async surface (abort the block, corrective retry naming the mistake),
/// or a thread's fork CHILD ran and ended in `InvocationExit` — round
/// exhaustion, a non-answer ending, its own provider call failing — rather
/// than finalizing (abort the block, corrective retry naming the child by
/// its path; the child's own node already retired via `node_failed` before
/// this variant is ever produced).
pub(crate) enum GreenRoundExit {
    NodeParked,
    NodeDone,
    /// Carries the ACTUAL refusal text `check_fork_budgets` built —
    /// `fork_subtree_refusal` when the tree-wide cap fired,
    /// `fork_budget_refusal` when the per-window pool did — never rebuilt
    /// downstream. Rebuilding it here would lose which budget was exhausted.
    ForkBudgetRefused {
        msg: String,
    },
    AsyncMisuse {
        msg: String,
    },
    /// A thread's fork child ended in `InvocationExit` — carries the plain-
    /// language corrective [`fork_child_failure_corrective`] built. Sibling
    /// threads in the round are unaffected up to this point (they already
    /// ran and, if they finalized, their own node already reports so via
    /// `node_finalized`) — only the round's OUTSTANDING threads are swept
    /// when this abort fires, same as [`Self::ForkBudgetRefused`].
    ForkChildFailed {
        msg: String,
    },
}

/// One serviced THREAD-chain ready item's outcome — the thread-plane
/// outcome, widened with the fork-budget refusal that thread forks can hit
/// and the fork-child-failure corrective.
pub(crate) enum ThreadServiced {
    Continue,
    /// The ACTUAL refusal text `check_fork_budgets` built — see
    /// [`GreenRoundExit::ForkBudgetRefused`]'s documentation.
    BudgetRefused {
        msg: String,
    },
    Misuse(String),
    /// A thread's fork child ended in `InvocationExit` — see
    /// [`GreenRoundExit::ForkChildFailed`]'s doc.
    ChildFailed {
        msg: String,
    },
}

/// Where one serviced green suspension's resume lands — invariant across
/// every constructor arm inside one [`SelfHarnessDriver::service_green_hole`]
/// call; only the answer payload and the diagnostic label vary per call, so
/// those two stay their own parameters on [`SelfHarnessDriver::deliver_green_resume`].
pub(crate) struct GreenResumeSite<'a> {
    pub(crate) host: Option<NodeId>,
    pub(crate) sid: tidepool_repr::SessionId,
    pub(crate) delivery: &'a GreenDelivery<'a>,
    pub(crate) chain: GreenChain,
    pub(crate) hole: &'a str,
}

impl SelfHarnessDriver {
    /// [`Harness::with_session_waiting`] when `host` names a real answerer
    /// node whose work shares a machine with sibling windows, else plain
    /// [`Harness::with_session`] — the AUTHORED outer loop's own green
    /// servicing (`run_loop_fragment_inner`) runs against the node-LESS
    /// outer session and does not contend with attached windows here.
    pub(crate) async fn with_session_for_host<T>(
        &self,
        host: Option<NodeId>,
        sid: tidepool_repr::SessionId,
        f: impl FnOnce(&mut Session) -> T,
    ) -> Result<T, HarnessError> {
        match host {
            Some(_) => self.agent.with_session_waiting(sid, f).await,
            None => self.agent.with_session(sid, f),
        }
    }

    /// Deliver a serviced green suspension's own resume per
    /// [`GreenDelivery`] — see that type's doc for the plane split.
    pub(crate) async fn deliver_green_resume(
        &self,
        site: &GreenResumeSite<'_>,
        answer: Value,
        ready: &mut VecDeque<GreenReady>,
        what: &str,
    ) -> Result<(), DriverError> {
        match site.delivery {
            GreenDelivery::Raw => {
                let next = self
                    .with_session_for_host(site.host, site.sid, |s| {
                        s.resume(ResidentHole::plain(site.hole), answer)
                    })
                    .await
                    .map_err(|e| DriverError::Session(e.to_string()))?
                    .map_err(|e| DriverError::Session(format!("{what} resume failed: {e}")))?;
                ready.push_back(GreenReady {
                    chain: site.chain,
                    outcome: next,
                });
                Ok(())
            }
            GreenDelivery::Node { node, hole } => {
                Ok(self.agent.resume_with_value(*node, hole, answer).await?)
            }
        }
    }

    /// Service one `Tidepool.Async` suspension: decode which
    /// of the six `Async*With` verbs `request` is by CONSTRUCTOR NAME (never
    /// in [`engine::classify_hole`] — the payload may carry a live closure,
    /// see [`SuspensionRouting::Green`]'s doc) and act, mutating the scheduler's
    /// thread table / waiter map / ready queue in place.
    ///
    /// Deliberately returns `Result<(), DriverError>`, not a [`ServicedSuspension`]
    /// — checked and rejected before the rest of the dispatcher adopted that
    /// sum. Its six arms push zero (`AsyncJoinAnyWith` with no terminal
    /// candidate, `AsyncDoneWith` on a cancelled/already-settled thread),
    /// one, two (`AsyncSpawnWith`: the resumed spawner and the freshly
    /// started thread), or an arbitrary N (`AsyncDoneWith`/`AsyncCancelWith`
    /// waking every parked joiner) items onto `ready`, and several never
    /// resume the triggering hole at all (`AsyncDoneWith`'s own hole stays
    /// parked forever, its frame reclaimed only when the thread's realm
    /// eventually closes). `ServicedSuspension::{Resumed,LeaveParked}` both assume
    /// "exactly one hole, exactly one outcome, handed back once" — this
    /// method's job is precisely to not have that shape, so forcing it into
    /// the sum would mean returning `Vec<ServicedSuspension>` (or a payload-free
    /// `Handled` marker), neither of which catches anything a caller
    /// forgetting to `?` this `Result` doesn't already catch today. Mirrors
    /// [`Self::service_outer_subagent`]'s shape (driver-owned, suspension-
    /// serviced, no handler) but is not a single dispatch-then-resume: a
    /// spawn starts a NEW top-level run and a park-until-terminal join may
    /// register a waiter instead of answering immediately.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn service_green_hole(
        &self,
        // The HOST answerer node selects waiting admission for every session
        // access in this call, regardless of whether `delivery` targets this
        // same node's own hole (`GreenDelivery::Node`) or a raw thread chain
        // (`GreenDelivery::Raw`): a thread belongs to this host's own window,
        // so it contends on exactly the same checkout races its host does.
        // `None` selects the authored outer loop's node-less session access.
        host: Option<NodeId>,
        chain: GreenChain,
        hole: &str,
        request: &Value,
        table: &DataConTable,
        threads: &mut HashMap<i64, GreenThread>,
        waiters: &mut HashMap<i64, Vec<(GreenChain, String)>>,
        next_tid: &mut i64,
        ready: &mut VecDeque<GreenReady>,
        delivery: GreenDelivery<'_>,
    ) -> Result<bool, DriverError> {
        let sid = self.outer_sid()?;
        match engine::con_name(request, table) {
            // Field 1 is the thread body — ALWAYS a closure by construction
            // (`asyncSpawn` wraps every body in a lambda so the
            // closure-sentinel scan fires even for `async (pure 5)`; see
            // `tidepool-mcp/src/effect_defs.rs`'s `green_effect_def!` doc).
            Some("AsyncSpawnWith") => {
                let tid = *next_tid;
                *next_tid += 1;
                let realm = tidepool_codegen::suspension::RealmId::fresh();
                threads.insert(
                    tid,
                    GreenThread {
                        realm,
                        state: GreenThreadState::Running,
                    },
                );
                // Mint custody and start the thread under one checkout. This
                // makes ownership transfer atomic with respect to session
                // admission; a failed checkout never creates custody.
                let thread_start = self
                    .with_session_for_host(host, sid, |s| -> Result<ResidentOutcome, String> {
                        let body = s.live_payload_handle(hole).ok_or_else(|| {
                            "AsyncSpawnWith: spawner frame carries no untaken body closure"
                                .to_string()
                        })?;
                        s.run_rooted_entry("async_thread", body, 0, realm, Some(table))
                            .map_err(|e| format!("run_rooted_entry failed: {e}"))
                    })
                    .await
                    .map_err(|e| DriverError::Session(e.to_string()))?
                    .map_err(DriverError::Session)?;
                // Spawner-continues-first (the ready queue's own choice) —
                // resume the spawner immediately
                // with the fresh id, then start the thread; either push lands
                // on `ready` so both eventually run regardless.
                //
                // Boxed via `i64: ToCore` (an `I#` Con looked up in THIS
                // compile's own table) — NOT `engine::json_answer_to_value`
                // (which bridges to `Tidepool.Aeson.Value`, the wrong TYPE
                // for a plain `Int` `send` delivers natively — that generic
                // wire path is for an `askUser` submission's `FromJSON`
                // decode) and NOT a bare `Value::Lit` (unboxed; only
                // tolerated by the JIT's OWN synthesized `App` in
                // `apply_finalized`/`run_rooted_entry`, not by arbitrary compiled
                // Haskell that pattern-matches `case x of I# n#`).
                let tid_value = tid
                    .to_value(table)
                    .map_err(|e| DriverError::Session(format!("AsyncSpawnWith tid box: {e}")))?;
                // Spawner-continues-first: the spawner's resume lands (Raw:
                // pushed to `ready` ahead of the thread; Node: the node's own
                // pending record refreshes) before the fresh thread's first
                // outcome enters the queue.
                self.deliver_green_resume(
                    &GreenResumeSite {
                        host,
                        sid,
                        delivery: &delivery,
                        chain,
                        hole,
                    },
                    tid_value,
                    ready,
                    "AsyncSpawnWith spawner",
                )
                .await?;
                ready.push_back(GreenReady {
                    chain: GreenChain::Thread(tid),
                    outcome: thread_start,
                });
                Ok(false)
            }
            // A thread's last act. Its sole `Int` field is always the
            // dummy `0` `asyncSpawn` bakes in — the settling thread's real id
            // is `chain`, not that field (see `GreenChain`'s doc). The
            // AsyncDoneWith hole itself is deliberately never resumed. The
            // sealed Haskell wrapper has already published the typed result
            // into the managed cell carried by every `Async a`; this
            // suspension only linearizes the Rust terminal fact.
            Some("AsyncDoneWith") => {
                if matches!(delivery, GreenDelivery::Node { .. }) {
                    return Err(DriverError::Session(
                        "AsyncDoneWith delivered on the node chain (scheduler bug: settles \
                         are thread-only)"
                            .into(),
                    ));
                }
                let GreenChain::Thread(tid) = chain else {
                    return Err(DriverError::Session(
                        "AsyncDoneWith suspended on a non-thread chain (scheduler bug: every \
                         thread body is reached only via run_rooted_entry)"
                            .into(),
                    ));
                };
                let records_result = threads
                    .get(&tid)
                    .is_some_and(|t| matches!(t.state, GreenThreadState::Running));
                if !records_result {
                    self.wake_green_waiters(host, tid, table, sid, waiters, ready)
                        .await?;
                    return Ok(false);
                }
                let settled_realm = if let Some(entry) = threads.get_mut(&tid) {
                    entry.state = GreenThreadState::Settled;
                    // Wake any `WatchAsync tid` subscriber exactly once — the
                    // `Tidepool.Event.waitEvent`/`Tidepool.Event` completion
                    // watch. `records_result` above
                    // already established this is a genuine Running→Settled
                    // transition, so this always fires exactly once per
                    // settle. No-op if `RepoEvent` was never wired —
                    // `WatchAsync` is unusable without it anyway.
                    if let Some(h) = self.handlers.lock().event.as_mut() {
                        h.registry_mut().publish_async_done(tid);
                    }
                    Some(entry.realm)
                } else {
                    None
                };
                if let Some(realm) = settled_realm {
                    self.with_session_for_host(host, sid, |s| s.close_realm(realm))
                        .await
                        .map_err(|e| DriverError::Session(e.to_string()))?;
                }
                self.wake_green_waiters(host, tid, table, sid, waiters, ready)
                    .await?;
                Ok(false)
            }
            Some("AsyncJoinAnyWith") => {
                let ids = green_int_list_field(request, 0, table);
                let winner = ids.iter().copied().find(|&tid| {
                    threads
                        .get(&tid)
                        .is_some_and(|t| !matches!(t.state, GreenThreadState::Running))
                });
                match winner {
                    Some(winner) => {
                        let winner_value = winner.to_value(table).map_err(|e| {
                            DriverError::Session(format!("AsyncJoinAnyWith winner box: {e}"))
                        })?;
                        self.deliver_green_resume(
                            &GreenResumeSite {
                                host,
                                sid,
                                delivery: &delivery,
                                chain,
                                hole,
                            },
                            winner_value,
                            ready,
                            "AsyncJoinAnyWith",
                        )
                        .await?;
                    }
                    None => {
                        // None terminal yet. Raw chains park as waiters on
                        // EVERY listed thread — whichever settles/cancels
                        // first wakes them; nothing goes on `ready`. The
                        // NODE chain never registers (the raw waiter wake
                        // must never touch it) — it reports BLOCKED and its
                        // still-pending join is re-serviced (a pure winner
                        // scan) each scheduler iteration.
                        match delivery {
                            GreenDelivery::Raw => {
                                for tid in ids {
                                    waiters
                                        .entry(tid)
                                        .or_default()
                                        .push((chain, hole.to_string()));
                                }
                            }
                            GreenDelivery::Node { .. } => return Ok(true),
                        }
                    }
                }
                Ok(false)
            }
            Some("AsyncStatusWith") => {
                let tid = green_int_field(request, 0, table);
                let code: i64 = match threads.get(&tid).map(|t| &t.state) {
                    Some(GreenThreadState::Settled) => 1,
                    Some(GreenThreadState::Cancelled) => 2,
                    _ => 0,
                };
                let code_value = code
                    .to_value(table)
                    .map_err(|e| DriverError::Session(format!("AsyncStatusWith code box: {e}")))?;
                self.deliver_green_resume(
                    &GreenResumeSite {
                        host,
                        sid,
                        delivery: &delivery,
                        chain,
                        hole,
                    },
                    code_value,
                    ready,
                    "AsyncStatusWith",
                )
                .await?;
                Ok(false)
            }
            Some("AsyncCancelWith") => {
                let tid = green_int_field(request, 0, table);
                if let Some(entry) = threads.get_mut(&tid) {
                    if matches!(entry.state, GreenThreadState::Running) {
                        let realm = entry.realm;
                        entry.state = GreenThreadState::Cancelled;
                        self.with_session_for_host(host, sid, |s| {
                            s.close_realm(realm);
                        })
                        .await
                        .map_err(|e| DriverError::Session(e.to_string()))?;
                        // A cancel is a terminal-state transition exactly like
                        // a settle — `waitEvent` must fire for either, so it
                        // shares the same publish (see the `AsyncDoneWith` arm
                        // above).
                        if let Some(h) = self.handlers.lock().event.as_mut() {
                            h.registry_mut().publish_async_done(tid);
                        }
                        self.wake_green_waiters(host, tid, table, sid, waiters, ready)
                            .await?;
                    }
                    // Idempotent: a terminal thread's cancel is a no-op.
                }
                let unit = ()
                    .to_value(table)
                    .map_err(|e| DriverError::Session(format!("AsyncCancelWith () bridge: {e}")))?;
                self.deliver_green_resume(
                    &GreenResumeSite {
                        host,
                        sid,
                        delivery: &delivery,
                        chain,
                        hole,
                    },
                    unit,
                    ready,
                    "AsyncCancelWith",
                )
                .await?;
                Ok(false)
            }
            other => Err(DriverError::Session(format!(
                "outer loop suspended on an unrecognized Green constructor ({other:?})"
            ))),
        }
    }

    /// Wake every waiter parked (via `AsyncJoinAnyWith`) on `tid` — resume
    /// each with `tid`'s own id (the winner) and push the result onto
    /// `ready`. Shared by `AsyncDoneWith` (a settle) and `AsyncCancelWith` (a
    /// cancellation) servicing.
    pub(crate) async fn wake_green_waiters(
        &self,
        host: Option<NodeId>,
        tid: i64,
        table: &DataConTable,
        sid: tidepool_repr::SessionId,
        waiters: &mut HashMap<i64, Vec<(GreenChain, String)>>,
        ready: &mut VecDeque<GreenReady>,
    ) -> Result<(), DriverError> {
        let Some(parked) = waiters.remove(&tid) else {
            return Ok(());
        };
        let tid_value = tid
            .to_value(table)
            .map_err(|e| DriverError::Session(format!("green wake tid box: {e}")))?;
        for (wchain, whole) in parked {
            let next = self
                .with_session_for_host(host, sid, |s| {
                    s.resume(ResidentHole::plain(whole.clone()), tid_value.clone())
                })
                .await
                .map_err(|e| DriverError::Session(e.to_string()))?
                .map_err(|e| DriverError::Session(format!("green wake resume failed: {e}")))?;
            ready.push_back(GreenReady {
                chain: wchain,
                outcome: next,
            });
        }
        Ok(())
    }

    /// One scheduling pass of the answerer-plane green scheduler: service the
    /// NODE's own pending `Green` suspensions (node-aware resumes) and pump
    /// THREAD chains (raw resumes, shared [`Self::service_green_hole`]) until
    /// the node parks on something that isn't Green ([`GreenRoundExit::NodeParked`])
    /// or completes without finalizing ([`GreenRoundExit::NodeDone`]).
    /// `green` persists across passes within one ROUND (the dispatcher may
    /// interleave askUser/fork servicing between passes) and is swept at the
    /// round boundary by [`Self::sweep_green_round`].
    ///
    /// The node's blocked `wait` is deliberately NOT registered in
    /// `green.waiters`: each iteration re-services its pending
    /// `AsyncJoinAnyWith` (a pure winner scan when nothing settled), so a
    /// settle is observed on the very next loop — and the raw waiter-wake
    /// path structurally cannot touch the node chain.
    pub(crate) async fn service_green_round(
        &self,
        node: NodeId,
        green: &mut ModelRoundGreenThreadScheduler,
        budget: &mut ForkBudget,
        fork_depth: u32,
        fork_subtree: &std::sync::atomic::AtomicU32,
        ty_label: &str,
    ) -> Result<GreenRoundExit, DriverError> {
        loop {
            let Some((hole, classified, table, asks, request)) =
                self.agent.pending_suspend_artifacts(node)
            else {
                return Ok(GreenRoundExit::NodeDone);
            };
            if !matches!(classified.routing, SuspensionRouting::Green) {
                return Ok(GreenRoundExit::NodeParked);
            }
            let blocked = self
                .service_green_hole(
                    Some(node),
                    GreenChain::Primary,
                    &hole.0,
                    &request,
                    &table,
                    &mut green.threads,
                    &mut green.waiters,
                    &mut green.next_tid,
                    &mut green.ready,
                    GreenDelivery::Node { node, hole: &hole },
                )
                .await?;
            if !blocked {
                continue;
            }
            // The node is blocked on a join with no terminal candidate.
            // Drain every FORK-routed ready item out of `green.ready` and
            // drive them all CONCURRENTLY (`Self::drive_fork_ready_batch`):
            // by construction, every `async (fork @T brief)` a straight-line
            // block spawned before its first `wait` has already reached its
            // OWN `fork` suspension by the time the node blocks (spawning a
            // thread is a synchronous JIT step, no model call involved), so
            // every fork this wait could possibly be blocked on is already
            // sitting in `ready`. SUBAGENT-routed ready items get the SAME
            // treatment (`Self::drive_subagent_ready_batch`), generalizing
            // the pattern per `CONCURRENT_SIBLINGS_SPIKE_FINDINGS.md`: N
            // sibling threads that each call `spawnAgent` (a real
            // suspension, not a JIT-only step — `Subagent` is not a
            // JIT-handled effect here) reach their OWN `Subagent` suspension
            // at the same logical moment their fork'd threads are spawned,
            // so whatever is ready now is everything this round could
            // batch. Everything else keeps the single-item path below —
            // cheap, immediate resumes with nothing to gain from batching.
            let mut fork_batch: Vec<(GreenReady, ClassifiedSuspension)> = Vec::new();
            let mut subagent_batch: Vec<(GreenChain, String, Value)> = Vec::new();
            let mut rest: VecDeque<GreenReady> = VecDeque::with_capacity(green.ready.len());
            for item in green.ready.drain(..) {
                let classified = match &item.outcome {
                    ResidentOutcome::Suspended { request, .. } => {
                        engine::classify_hole(request, &table, &asks).ok()
                    }
                    ResidentOutcome::Completed { .. } => None,
                };
                match classified {
                    Some(c) if matches!(c.routing, SuspensionRouting::Fork { .. }) => {
                        fork_batch.push((item, c));
                    }
                    Some(c) if matches!(c.routing, SuspensionRouting::Subagent) => {
                        let chain = item.chain;
                        match item.outcome {
                            // Normalized to a raw `cont_id`, same as every
                            // other thread-chain resume on this plane
                            // (`drive_fork_ready_batch`'s own `Admitted.hole`
                            // does the same) — a thread frame's resume is
                            // always PLAIN, regardless of which `ResidentHole`
                            // variant the suspension originally carried.
                            ResidentOutcome::Suspended { hole, request, .. } => {
                                subagent_batch.push((chain, hole.cont_id().to_string(), request));
                            }
                            ResidentOutcome::Completed { .. } => {
                                unreachable!("classified as Suspended above")
                            }
                        }
                    }
                    // A classify failure here is not lost: the item goes to
                    // `rest` and `service_thread_ready` below re-classifies
                    // it (and surfaces the same error) on its own turn.
                    _ => rest.push_back(item),
                }
            }
            green.ready = rest;
            if !fork_batch.is_empty() {
                match self
                    .drive_fork_ready_batch(
                        node,
                        fork_batch,
                        &table,
                        green,
                        budget,
                        fork_depth,
                        fork_subtree,
                        ty_label,
                    )
                    .await?
                {
                    ThreadServiced::Continue => continue,
                    ThreadServiced::BudgetRefused { msg } => {
                        return Ok(GreenRoundExit::ForkBudgetRefused { msg });
                    }
                    ThreadServiced::Misuse(msg) => {
                        return Ok(GreenRoundExit::AsyncMisuse { msg });
                    }
                    ThreadServiced::ChildFailed { msg } => {
                        return Ok(GreenRoundExit::ForkChildFailed { msg });
                    }
                }
            }
            if !subagent_batch.is_empty() {
                self.drive_subagent_ready_batch(node, subagent_batch, &table, green)
                    .await?;
                continue;
            }
            // One thread step, then loop (the join re-check observes any
            // settle).
            let Some(GreenReady { chain, outcome }) = green.ready.pop_front() else {
                // Model-attributable, not a mechanism failure: the block
                // awaits a thread no ready work can ever settle — typically
                // a `wait` on a handle from an EARLIER round (swept at the
                // round boundary) or a thread deadlock. Abort the block with
                // a corrective instead of ending the whole run.
                return Ok(GreenRoundExit::AsyncMisuse {
                    msg: "your block is waiting on a thread that has no runnable work \
                          — usually a `wait` on a handle from an earlier round (thread \
                          handles do not survive a round boundary; spawn and wait in \
                          the SAME block), or threads waiting on each other"
                        .into(),
                });
            };
            match self
                .service_thread_ready(node, chain, outcome, green)
                .await?
            {
                ThreadServiced::Continue => {}
                ThreadServiced::BudgetRefused { msg } => {
                    return Ok(GreenRoundExit::ForkBudgetRefused { msg });
                }
                ThreadServiced::Misuse(msg) => {
                    return Ok(GreenRoundExit::AsyncMisuse { msg });
                }
                ThreadServiced::ChildFailed { msg } => {
                    return Ok(GreenRoundExit::ForkChildFailed { msg });
                }
            }
        }
    }

    /// Drive every FORK-routed thread-chain ready item in `batch`
    /// CONCURRENTLY, up to [`Self::concurrency_cap`] at once, via
    /// [`drive_concurrent`] and [`Self::drive_fork_children`] — called by
    /// [`Self::service_green_round`] once its own node blocks and it has
    /// drained every currently fork-routed [`GreenReady`] out of
    /// `green.ready`. This is the OUTER layer of the same concurrency shell
    /// [`Self::drive_fork_children`] already applies WITHIN one thread's own
    /// `forkAll` batch — here the batch spans DIFFERENT threads' own `fork`
    /// calls instead.
    ///
    /// Budget admission for the whole batch is checked EAGERLY, in the
    /// batch's original FIFO (== spawn) order, before any child is driven —
    /// `check_fork_budgets` is a compare-exchange spend-before-spawn (safe
    /// under overlap by construction — see the ANTI-PATTERNS note against
    /// re-deriving it), so checking the batch upfront in queue order
    /// reproduces the exact admission decisions the old fully-sequential
    /// scheduler made one ready item at a time. The FIRST refusal stops
    /// admission for the REST of the batch — mirroring the old
    /// pop-one-at-a-time loop, which never even looked at a later ready item
    /// once an earlier one aborted the round — so an item after a refusal is
    /// left unresumed; the round is about to abort regardless, and
    /// [`Self::sweep_green_round`] closes its still-`Running` thread realm.
    ///
    /// Every ADMITTED item is driven to completion regardless of a sibling's
    /// outcome (`drive_concurrent` never short-circuits), so a child that
    /// already finalized keeps its own retirement/GUI receipt even when a
    /// sibling in the SAME batch ends in `InvocationExit` — the abort this
    /// returns only discards the THREAD-level resume for the batch, never a
    /// child's own already-completed session bookkeeping.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn drive_fork_ready_batch(
        &self,
        node: NodeId,
        batch: Vec<(GreenReady, ClassifiedSuspension)>,
        table: &DataConTable,
        green: &mut ModelRoundGreenThreadScheduler,
        budget: &mut ForkBudget,
        fork_depth: u32,
        fork_subtree: &std::sync::atomic::AtomicU32,
        ty_label: &str,
    ) -> Result<ThreadServiced, DriverError> {
        let sid = self.outer_sid()?;

        struct Admitted {
            chain: GreenChain,
            hole: String,
            site: crate::tree::SiteId,
            ty: Option<String>,
            fan: Option<FanBadge>,
            prompts: Vec<String>,
            prompt: String,
            source: engine::ForkSource,
        }

        let mut admitted: Vec<Admitted> = Vec::with_capacity(batch.len());
        let mut admission_refusal: Option<String> = None;
        for (item, classified) in batch {
            let ResidentOutcome::Suspended { hole, .. } = &item.outcome else {
                return Err(DriverError::Session(
                    "answerer green scheduler: a fork-routed ready item completed \
                     without AsyncDoneWith (every asyncSpawn body parks on its own \
                     settle — scheduler bug)"
                        .into(),
                ));
            };
            if admission_refusal.is_some() {
                // A stricter item already refused this batch; the round is
                // aborting — see this fn's own doc.
                continue;
            }
            // Cost is read off the intact routing BEFORE it's destructured
            // below, so admission needs no clone of `ty`/`prompts`.
            let cost = ForkBudget::cost(&classified.routing);
            let SuspensionRouting::Fork {
                site,
                ty,
                fan,
                prompts,
                source,
            } = classified.routing
            else {
                return Err(DriverError::Session(
                    "answerer green scheduler: drive_fork_ready_batch received a \
                     non-Fork classified item (scheduler bug)"
                        .into(),
                ));
            };
            if let Some(msg) = self.check_fork_budgets(budget, cost, fork_subtree, ty_label) {
                admission_refusal = Some(msg);
                continue;
            }
            admitted.push(Admitted {
                chain: item.chain,
                hole: hole.cont_id().to_string(),
                site,
                ty,
                fan,
                prompts,
                prompt: classified.prompt,
                source,
            });
        }

        let admitted_ref = &admitted;
        let cap = self.concurrency_cap;
        #[allow(clippy::type_complexity)]
        let results: Vec<(usize, Result<Result<Value, String>, DriverError>)> =
            drive_concurrent(cap, admitted.len(), |idx| {
                let a = &admitted_ref[idx];
                async move {
                    self.drive_fork_children(
                        node,
                        "async fork answerer",
                        "async fanout answerer",
                        a.site,
                        a.ty.as_deref(),
                        &a.fan,
                        &a.prompts,
                        &a.prompt,
                        a.source,
                        table,
                        fork_depth,
                        fork_subtree,
                        ty_label,
                    )
                    .await
                }
            })
            .await;

        let mut first_mech_err: Option<DriverError> = None;
        let mut first_child_failed: Option<String> = None;
        for (idx, r) in results {
            let a = &admitted[idx];
            match r {
                Ok(Ok(answer)) => {
                    let next = self
                        .agent
                        .with_session_waiting(sid, |s| {
                            s.resume(ResidentHole::plain(a.hole.clone()), answer)
                        })
                        .await
                        .map_err(|e| DriverError::Session(e.to_string()))?
                        .map_err(|e| {
                            DriverError::Session(format!("async fork resume failed: {e}"))
                        })?;
                    green.ready.push_back(GreenReady {
                        chain: a.chain,
                        outcome: next,
                    });
                }
                Ok(Err(msg)) => {
                    if first_child_failed.is_none() {
                        first_child_failed = Some(msg);
                    }
                }
                Err(e) => {
                    if first_mech_err.is_none() {
                        first_mech_err = Some(e);
                    }
                }
            }
        }

        if let Some(e) = first_mech_err {
            return Err(e);
        }
        if let Some(msg) = first_child_failed {
            return Ok(ThreadServiced::ChildFailed { msg });
        }
        if let Some(msg) = admission_refusal {
            return Ok(ThreadServiced::BudgetRefused { msg });
        }
        Ok(ThreadServiced::Continue)
    }

    /// Drive every SUBAGENT-routed thread-chain ready item in `batch`
    /// CONCURRENTLY, up to [`Self::concurrency_cap`] at once, via
    /// [`drive_concurrent`] and [`Self::service_outer_subagent`] — the SAME
    /// generalization [`Self::drive_fork_ready_batch`] already gets, applied
    /// to `Subagent` per `CONCURRENT_SIBLINGS_SPIKE_FINDINGS.md`'s smallest
    /// driver change. Unlike Fork there is no spawn-time budget to admit
    /// against — every ready `Subagent` item is driven — and no per-child
    /// branch position to fold a failure into: a dispatch failure (an
    /// unwired handler, the handler erroring) is driver/mechanism-level,
    /// same as the single-item path this replaces, so the FIRST one found
    /// hard-fails the round via `Err` after every OTHER admitted item has
    /// still been driven to completion and resumed (`drive_concurrent` never
    /// short-circuits, so a sibling that already got its answer keeps it).
    ///
    /// Real wall-clock overlap for this batch comes from
    /// [`Self::service_outer_subagent`]'s own doc, not from anything here:
    /// this fn only removes the OLD one-item-at-a-time scheduling that kept
    /// a sibling's already-ready `Subagent` request from even being LOOKED
    /// AT while another sibling's `SubagentAwait` blocked.
    pub(crate) async fn drive_subagent_ready_batch(
        &self,
        node: NodeId,
        batch: Vec<(GreenChain, String, Value)>,
        table: &DataConTable,
        green: &mut ModelRoundGreenThreadScheduler,
    ) -> Result<(), DriverError> {
        let sid = self.outer_sid()?;
        let batch_ref = &batch;
        let cap = self.concurrency_cap;
        #[allow(clippy::type_complexity)]
        let results: Vec<(usize, Result<Value, DriverError>)> =
            drive_concurrent(cap, batch.len(), |idx| {
                let (_, _, request) = &batch_ref[idx];
                async move {
                    self.service_outer_subagent(request, table, FormSource::Answerer { node })
                        .await
                }
            })
            .await;

        let mut first_err: Option<DriverError> = None;
        for (chain, hole, value) in batch
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
            let next = self
                .agent
                .with_session_waiting(sid, |s| s.resume(ResidentHole::plain(hole), value))
                .await
                .map_err(|e| DriverError::Session(e.to_string()))?
                .map_err(|e| DriverError::Session(format!("async subagent resume failed: {e}")))?;
            green.ready.push_back(GreenReady {
                chain,
                outcome: next,
            });
        }
        match first_err {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    /// Service one popped THREAD-chain ready item that is NOT fork-routed
    /// nor subagent-routed (`service_green_round` drains and batches every
    /// fork-routed item via [`Self::drive_fork_ready_batch`] and every
    /// subagent-routed item via [`Self::drive_subagent_ready_batch`] before
    /// ever popping one for this dispatcher): classify it against the
    /// round's compile artifacts (read off the node's pending record —
    /// every chain of a round shares one compile) and dispatch. Green
    /// suspensions go through the SHARED [`Self::service_green_hole`] (raw
    /// resumes are correct for thread frames); `note`/`getStateJson`/
    /// `delegate` get their immediate service, raw-resumed. `askUser` and
    /// `finalize` inside a thread are refused loudly — operator forms and
    /// the window's answer belong on the main chain.
    pub(crate) async fn service_thread_ready(
        &self,
        node: NodeId,
        chain: GreenChain,
        outcome: ResidentOutcome,
        green: &mut ModelRoundGreenThreadScheduler,
    ) -> Result<ThreadServiced, DriverError> {
        let sid = self.outer_sid()?;
        let ResidentOutcome::Suspended { hole, request, .. } = outcome else {
            return Err(DriverError::Session(
                "answerer green scheduler: a thread chain completed without AsyncDoneWith \
                 (every asyncSpawn body parks on its own settle — scheduler bug)"
                    .into(),
            ));
        };
        let Some((_, _, table, asks, _)) = self.agent.pending_suspend_artifacts(node) else {
            return Err(DriverError::Session(
                "answerer green scheduler: node pending record vanished while a thread \
                 chain still had ready work"
                    .into(),
            ));
        };
        let classified = engine::classify_hole(&request, &table, &asks)
            .map_err(|e| DriverError::Session(format!("thread hole classify: {e}")))?;
        match classified.routing {
            SuspensionRouting::Green => {
                self.service_green_hole(
                    Some(node),
                    chain,
                    hole.cont_id(),
                    &request,
                    &table,
                    &mut green.threads,
                    &mut green.waiters,
                    &mut green.next_tid,
                    &mut green.ready,
                    GreenDelivery::Raw,
                )
                .await?;
                Ok(ThreadServiced::Continue)
            }
            // Unreachable by construction: `service_green_round` drains and
            // batches every FORK-routed ready item (`Self::drive_fork_ready_batch`)
            // BEFORE ever popping one for this per-item dispatcher — see that
            // fn's own doc for why the whole batch is known upfront (every
            // `async (fork …)` in a straight-line block has already reached
            // its own suspension by the time the node blocks).
            SuspensionRouting::Fork { .. } => Err(DriverError::Session(
                "answerer green scheduler: a Fork-routed ready item reached the \
                 per-item dispatcher — service_green_round must drain and batch \
                 these via drive_fork_ready_batch before popping (scheduler bug)"
                    .into(),
            )),
            SuspensionRouting::Note { text } => {
                self.announce_note(FormSource::Answerer { node }, &text);
                let unit = ()
                    .to_value(&table)
                    .map_err(|e| DriverError::Session(format!("note () bridge: {e}")))?;
                let next = self
                    .agent
                    .with_session_waiting(sid, |s| {
                        s.resume(ResidentHole::plain(hole.cont_id()), unit)
                    })
                    .await
                    .map_err(|e| DriverError::Session(e.to_string()))?
                    .map_err(|e| DriverError::Session(format!("thread note resume failed: {e}")))?;
                green.ready.push_back(GreenReady {
                    chain,
                    outcome: next,
                });
                Ok(ThreadServiced::Continue)
            }
            SuspensionRouting::ReadState => {
                let state = self.loop_state_snapshot();
                let value = engine::json_answer_to_value(&state, &table)
                    .map_err(|e| DriverError::Session(format!("getStateJson bridge: {e}")))?;
                let next = self
                    .agent
                    .with_session_waiting(sid, |s| {
                        s.resume(ResidentHole::plain(hole.cont_id()), value)
                    })
                    .await
                    .map_err(|e| DriverError::Session(e.to_string()))?
                    .map_err(|e| {
                        DriverError::Session(format!("thread getStateJson resume failed: {e}"))
                    })?;
                green.ready.push_back(GreenReady {
                    chain,
                    outcome: next,
                });
                Ok(ThreadServiced::Continue)
            }
            // Unreachable by construction, same reasoning as the `Fork` arm
            // above: `service_green_round` drains and batches every
            // SUBAGENT-routed ready item (`Self::drive_subagent_ready_batch`)
            // BEFORE ever popping one for this per-item dispatcher.
            SuspensionRouting::Subagent => Err(DriverError::Session(
                "answerer green scheduler: a Subagent-routed ready item reached the \
                 per-item dispatcher — service_green_round must drain and batch these \
                 via drive_subagent_ready_batch before popping (scheduler bug)"
                    .into(),
            )),
            SuspensionRouting::Finalize { .. } => Ok(ThreadServiced::Misuse(
                "a green thread called `finalize` — the session's answer belongs on the \
                 main chain: `wait` your threads, then finalize from the top level"
                    .into(),
            )),
            SuspensionRouting::AskUser { .. } | SuspensionRouting::Ask { .. } => {
                Ok(ThreadServiced::Misuse(
                    "a green thread called `askUser` — operator forms belong on the main \
                 chain: ask before spawning, or after your `wait`s"
                        .into(),
                ))
            }
            other => Ok(ThreadServiced::Misuse(format!(
                "a green thread suspended on an effect this driver cannot service inside \
                 async ({other:?}) — keep operator forms and the final answer on the \
                 main chain"
            ))),
        }
    }

    /// Round-boundary sweep: close every still-open thread realm and clear
    /// the scheduler — the answerer-plane sibling of
    /// [`Self::run_loop_fragment_inner`]'s end-of-scope sweep. Settled realms
    /// are closed here too as an idempotent backstop: settlement closes them
    /// eagerly because the result now lives in its managed Haskell cell, so
    /// the producer's terminal frame owns nothing a waiter needs.
    /// (Cancelled realms were closed eagerly by the cancel arm.) Returns how
    /// many threads were dropped
    /// MID-FLIGHT — Running only; a settled thread was not "dropped" — so
    /// the corrective prompt can say so.
    pub(crate) async fn sweep_green_round(
        &self,
        _node: NodeId,
        green: &mut ModelRoundGreenThreadScheduler,
    ) -> usize {
        let mut dropped = 0usize;
        if let Ok(sid) = self.outer_sid() {
            for entry in green.threads.values() {
                match entry.state {
                    GreenThreadState::Running => {
                        let _ = self
                            .agent
                            .with_session_waiting(sid, |s| s.close_realm(entry.realm))
                            .await;
                        dropped += 1;
                    }
                    GreenThreadState::Settled => {
                        let _ = self
                            .agent
                            .with_session_waiting(sid, |s| s.close_realm(entry.realm))
                            .await;
                    }
                    GreenThreadState::Cancelled => {}
                }
            }
        }
        green.threads.clear();
        green.waiters.clear();
        green.ready.clear();
        dropped
    }

    /// Sweep `green`'s still-open thread realms before an early exit out
    /// of [`Self::drive_agent_session_to_finalize`]'s inner round-servicing loop —
    /// that loop's NORMAL exits already sweep (the post-loop code, and the
    /// `ForkBudgetRefused`/`AsyncMisuse` arms' own inline sweeps before their
    /// `continue 'round`), but a `?`-propagated mechanism error from any of
    /// `drain_note_holes`/`service_askuser_hole`/`drain_answerer_fork`/
    /// `service_green_round` may propagate an error before normal cleanup.
    /// Wrapping the loop itself in a `?`-catching scope does not fit here:
    /// several arms `continue 'round` — a
    /// jump to the OUTER round loop — which cannot cross an intervening
    /// async-block boundary, so each unswept `?`/`return` site is wrapped
    /// individually instead. A no-op when `result` is `Ok` or `green` is
    /// still `None` (no threads were ever spawned this round).
    pub(crate) async fn sweep_green_on_err<T>(
        &self,
        node: NodeId,
        green: &mut Option<ModelRoundGreenThreadScheduler>,
        result: Result<T, DriverError>,
    ) -> Result<T, DriverError> {
        if result.is_err() {
            if let Some(g) = green.as_mut() {
                self.sweep_green_round(node, g).await;
            }
        }
        result
    }
}
