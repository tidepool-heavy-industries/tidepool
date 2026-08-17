//! Repository events (PRD 19, lane L4) — the runtime half of `withHandler`.
//!
//! `withHandler` itself is HASKELL: a scoped interposition over its body's
//! freer-simple structure that runs a drain before every effect the body
//! performs (`pumpEff`, `drainSubscription` — see `event_effect_def!` in
//! `tidepool-mcp/src/effect_defs.rs` and the mechanism write-up in
//! `plans/post-restart/worktree-lanes/L4-mechanism.md`). The author's handler
//! closure is applied by ordinary Haskell application inside the resident's
//! own continuation; it never crosses to Rust and Rust never roots or applies
//! one.
//!
//! What is left for this module is exactly three effect requests — subscribe,
//! drain, unsubscribe — over a registry of per-subscription queues. Several
//! authored semantics are therefore NOT enforced here, deliberately, because
//! they are structural in the Haskell and two copies of a rule are two rules
//! that can disagree:
//!
//! | Semantic | Where it actually lives |
//! |---|---|
//! | one handler at a time per subscription | `pumpEff` recurses on the BODY only |
//! | handler invocation order within a drain | Haskell's sequential `mapM_` over this FIFO |
//! | handler failure fails the enclosing scope | ordinary `Eff` propagation |
//! | lexical drain, then unregister | written literally in `withHandler`'s body |
//!
//! This module owns the rest: identity, membership, broadcast, the FIFO, the
//! bound, and — the load-bearing one — NO REPLAY.
//!
//! ## No replay — a rule about JOURNAL ROWS, not about repository movement
//!
//! [`EventJournal`](tidepool_worktree::EventJournal)'s module docs state the
//! rule this registry exists to keep: a subscription registered now begins at
//! the journal's current end and never sees a row written before it. A
//! replaying handler would, in the dev-tree dogfood, poke every child to rebase
//! onto commits they were already built from — an unbounded amount of
//! correct-looking, useless work.
//!
//! Here that rule is structural rather than defended: a queue only ever holds
//! what a reconciliation PASS produced while the subscription was live, and a
//! pass produces only facts newer than the observer's last journalled
//! observation. Rows already in the journal are, by construction, never
//! produced again, so they cannot be queued.
//!
//! **It is emphatically NOT a licence to lose movement that happened while no
//! subscription existed.** Between two resident cycles there is a window with
//! agents still running and nobody subscribed. Those commits are not journalled
//! yet, so the new cycle's first pass reports them as genuinely NEW
//! observations and the freshly registered subscription receives them — which
//! is why [`RepoEventHandler::repo_event_subscribe`] does NOT poll: it is a
//! cheap registry insert, and it deliberately leaves the first pass of the
//! cycle to the first drain, AFTER the subscription is live. Registering and
//! then quietly consuming the backlog on the registrant's behalf would drop
//! commits in the direction nobody notices.
//!
//! Both rules hold at once, and for one reason: the durable baseline is the
//! JOURNAL (the observer's business — see [`MonitorObservations`]), and the
//! queue is per-subscription (this registry's business).
//!
//! ## Blocking await, and deadlines (PRD 20, S1-L3)
//!
//! [`RepoEventHandler::repo_event_await`] is [`RepoEventHandler::repo_event_drain`]
//! plus a timeout: it loops \[reconcile pass, check the queue, sleep bounded
//! by [`EventConfig::poll_interval`]\] until the subscription has queued at
//! least one observation or the timeout elapses. An elapsed timeout returns
//! an EMPTY batch — typed data distinguishable from a real (non-empty) one,
//! never an [`EventError`] — and every rule above still holds: no replay, no
//! recovery from poison, the same bound. `withHandler`'s Haskell
//! (`haskell/lib/Tidepool/Event.hs`'s `nextEvent`) is this verb's ONE caller
//! that matters; nothing here understands `Event`'s projection, only raw
//! batches.
//!
//! Deadlines ride the SAME [`SubscriptionRegistry`] as repository watches so
//! one await covers both. [`EvWatch::WatchDeadline`] carries a RELATIVE
//! millisecond duration — Haskell's `after` performs no effect of its own
//! (deliberately: `RepoEvent` already ships in rows with no `Time` handler,
//! and a new mandatory dependency on one would break them), so THIS registry
//! is what reads "now" and fixes the absolute deadline, at `subscribe()`
//! time. [`SubscriptionRegistry::fire_due_deadlines`] then checks every live
//! subscription's pending deadlines — on every reconcile pass AND every
//! await-loop iteration, never rate-limited by `poll_interval` the way git
//! reads are, since it costs no I/O — and queues exactly one
//! [`EvRepositoryEvent::ObservedTick`] the first time a deadline is found
//! due, then forgets it: a deadline fires ONCE, never again, even under
//! repeated reconcile passes. Firing is per-subscription and never broadcast
//! through [`SubscriptionRegistry::publish`] — a `WatchDeadline` an agent
//! armed for itself cannot wake anyone else. The same bound and
//! poison-on-overflow rule applies to a fired tick as to any other queued
//! observation.
//!
//! ## Cycle-scoped, and re-registered every cycle
//!
//! A subscription never crosses a resident cycle boundary. The registry is
//! owned by [`RepoEventHandler`], which is owned by the cycle's handler stack;
//! dropping that stack ends every registration. There is no durable
//! subscription store here and there must not be one — an attached Haskell
//! handle, a parked Haskell continuation, and an event subscription are the
//! three things PRD 19 says never survive a cycle.
//!
//! Re-registering from explicit state and stable worktree ids each cycle is
//! therefore an ordinary, repeated path, not a recovery story. It is safe
//! precisely because of the paragraph above: the new cycle's subscriptions are
//! empty, and the gap's movement arrives as new observations rather than as
//! replayed rows.
//!
//! ## The bound
//!
//! Per-subscription queue depth is configurable ([`EventConfig::queue_bound`])
//! but not optional. Overflow POISONS the subscription: every subsequent drain
//! returns `EventQueueOverflow` carrying the dropped count, forever. There is
//! no recovery path on purpose — a subscription that dropped commits and then
//! looked healthy is worse than one that fails.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use tidepool_bridge_effects::{
    EvCommitReceipt, EvEventId, EvHeadChangeKind, EvHeadChangeReceipt, EvRepositoryEvent,
    EvSubscriptionId, EvTickReceipt, EvWatch, WtBranchName, WtGitOid, WtWorktreeId,
};
use tidepool_worktree::storage::now_ms;

// Wall-clock epoch milliseconds for `Tick`'s `firedAtMs` — an observability
// stamp only; internal deadline SCHEDULING uses the monotonic `Instant` clock
// instead, immune to a system-clock jump. `now_ms` now PANICS on a pre-epoch
// clock (`tidepool_worktree::storage::now_ms`'s documented behavior), unlike
// this call site's prior local copy, which silently stamped `0` — see that
// module's docs for why the panic won.

// `EventError` + `RepoEventReq` + `DescribeEffect` + the `EffectHandler`
// dispatch match are generated from the single-source definition in
// `tidepool-mcp/src/effect_defs.rs`; only the handler struct, the registry, and
// the three per-verb methods below are hand-written.
tidepool_mcp::event_effect_def!(crate::effect_glue::effect_rust_projection);

// ============================================================================
// Configuration
// ============================================================================

/// Tunables for the event runtime. Both values move; neither mechanism is
/// optional.
#[derive(Clone, Debug)]
pub struct EventConfig {
    /// Maximum queued observations per subscription before it poisons. The
    /// VALUE is tunable; the EXISTENCE of a bound is not — an unbounded queue
    /// turns a slow handler into unbounded memory growth, and the failure
    /// arrives as an OOM instead of a named error.
    pub queue_bound: usize,
    /// Minimum wall-clock gap between drain-triggered reconciliation passes.
    /// V1 is polling; hooks, when they arrive, are only a wake-up that causes a
    /// read. Raising it delays a report; it never loses one, because a pass is
    /// a delta against the observer's durable baseline rather than a window.
    pub poll_interval: Duration,
}

impl Default for EventConfig {
    fn default() -> Self {
        Self {
            // Deep enough that an ordinary handler suspension (an `ask` round
            // trip, a spawned reviewer) never trips it, shallow enough that a
            // genuinely wedged handler fails in bounded memory.
            queue_bound: 1024,
            // DERIVED from the substrate's one reasoned default (measured
            // cost + latency budget on that constant's doc), not restated: a
            // second hand-written figure here once disagreed with it by 20x
            // and silently multiplied the git traffic the reasoning budgeted.
            poll_interval: Duration::from_millis(tidepool_worktree::DEFAULT_POLL_INTERVAL_MS),
        }
    }
}

// ============================================================================
// The subscription registry
// ============================================================================

/// One live registration: what it watches, what it has queued, and whether it
/// has already lost an observation.
#[derive(Debug)]
struct Subscription {
    watches: Vec<EvWatch>,
    queue: VecDeque<EvRepositoryEvent>,
    /// Observations this subscription lost to the bound. Nonzero means
    /// POISONED: it never queues or drains again, it only reports.
    dropped: i64,
    /// Absolute deadlines from this subscription's `WatchDeadline` entries,
    /// still armed — computed from `now + ms` at `subscribe()` time, since
    /// the wire value is a RELATIVE duration. A fired deadline is removed
    /// here (fire-once) — separate from `watches`, which stays the immutable
    /// record of what was registered.
    pending_deadlines: Vec<Instant>,
}

impl Subscription {
    fn poisoned(&self) -> bool {
        self.dropped > 0
    }
}

/// Identity, membership, broadcast, FIFO, and the bound — the whole of what the
/// runtime owes `withHandler`, with no JIT and no git in it, so it is testable
/// on its own.
#[derive(Debug)]
pub struct SubscriptionRegistry {
    /// Monotonic. An id is NEVER reused, so a spent id is a lookup miss rather
    /// than an alias of some later subscription.
    next_id: i64,
    /// Separate id space from `next_id` (subscription ids) and from the ids
    /// [`MonitorObservations`] mints for git-observed facts — this registry
    /// is the only minter of a `Tick`'s [`EvEventId`], since no monitor pass
    /// produces one.
    next_event_id: i64,
    subs: Vec<(i64, Subscription)>,
    bound: usize,
}

impl SubscriptionRegistry {
    pub fn new(bound: usize) -> Self {
        assert!(
            bound > 0,
            "the per-subscription queue bound must be positive"
        );
        Self {
            next_id: 1,
            next_event_id: 1,
            subs: Vec::new(),
            bound,
        }
    }

    pub fn bound(&self) -> usize {
        self.bound
    }

    /// Register `watches` and return a fresh id. The queue starts EMPTY: this
    /// call is the subscription's start point, and nothing observed before it
    /// is ever visible through it. A `WatchDeadline` entry carries a RELATIVE
    /// millisecond duration — Haskell's `after` performs no effect of its
    /// own, so THIS call is where "now" is read and the absolute deadline is
    /// fixed.
    pub fn subscribe(&mut self, watches: Vec<EvWatch>) -> EvSubscriptionId {
        let now = Instant::now();
        let pending_deadlines = watches
            .iter()
            .filter_map(|w| match w {
                EvWatch::WatchDeadline(ms) => {
                    Some(now + Duration::from_millis((*ms).max(0) as u64))
                }
                _ => None,
            })
            .collect();
        let raw = self.next_id;
        self.next_id += 1;
        self.subs.push((
            raw,
            Subscription {
                watches,
                queue: VecDeque::new(),
                dropped: 0,
                pending_deadlines,
            },
        ));
        EvSubscriptionId { raw }
    }

    /// Queue exactly one `Tick` for every pending deadline that has passed
    /// `now`, per subscription, then forget it — a deadline fires ONCE.
    /// Same bound and poison-on-overflow rule as [`Self::publish`]; a fired
    /// tick that overflows still counts toward `dropped`, so an await on a
    /// poisoned subscription still fails loudly rather than quietly losing
    /// the tick. Never broadcast: a subscription's deadlines only ever queue
    /// into that same subscription.
    pub fn fire_due_deadlines(&mut self, now: Instant) {
        // One wall-clock stamp for the whole pass — purely an observability
        // field on the `Tick`, never compared against `now` (the internal
        // scheduling clock is monotonic `Instant`, immune to a system-clock
        // jump).
        let wall_now_ms = now_ms();
        for (_, sub) in self.subs.iter_mut() {
            if sub.pending_deadlines.is_empty() {
                continue;
            }
            let due = sub.pending_deadlines.iter().filter(|&&d| d <= now).count();
            if due == 0 {
                continue;
            }
            sub.pending_deadlines.retain(|&d| d > now);
            for _ in 0..due {
                self.next_event_id += 1;
                let id = self.next_event_id;
                if sub.poisoned() || sub.queue.len() >= self.bound {
                    sub.dropped += 1;
                } else {
                    sub.queue.push_back(EvRepositoryEvent::ObservedTick(
                        EvEventId { raw: id },
                        EvTickReceipt {
                            fired_at_ms: wall_now_ms,
                        },
                    ));
                }
            }
        }
    }

    /// Append `event` to EVERY subscription whose watch set selects it.
    ///
    /// BROADCAST, not consumption: an event is not taken by the first observer,
    /// because two `withHandler` scopes over the same worktree are two
    /// independent reactions to one fact, not two claimants on it.
    pub fn publish(&mut self, event: &EvRepositoryEvent) {
        for (_, sub) in self.subs.iter_mut() {
            if !sub.watches.iter().any(|w| event.matches(w)) {
                continue;
            }
            if sub.poisoned() || sub.queue.len() >= self.bound {
                // Count the loss rather than evicting: a dropped commit that
                // nobody counted is exactly the silent failure the bound
                // exists to convert into a loud one.
                sub.dropped += 1;
                continue;
            }
            sub.queue.push_back(event.clone());
        }
    }

    /// Take everything queued, in observation order, leaving the queue empty.
    pub fn drain(&mut self, id: EvSubscriptionId) -> Result<Vec<EvRepositoryEvent>, EventError> {
        let sub = self
            .lookup_mut(id.raw)
            .ok_or(EventError::EventUnknownSubscription(id.raw))?;
        if sub.poisoned() {
            // No recovery, ever. Reporting the count on every drain (rather
            // than once) means the failure cannot be swallowed by whichever
            // caller happened to observe it first.
            return Err(EventError::EventQueueOverflow(id.raw, sub.dropped));
        }
        Ok(sub.queue.drain(..).collect())
    }

    /// Spend the id. A later drain on it is an error, not a silent empty.
    pub fn unsubscribe(&mut self, id: EvSubscriptionId) -> Result<(), EventError> {
        match self.subs.iter().position(|(raw, _)| *raw == id.raw) {
            Some(idx) => {
                self.subs.remove(idx);
                Ok(())
            }
            None => Err(EventError::EventUnknownSubscription(id.raw)),
        }
    }

    /// Every live subscription id, in registration order.
    pub fn live_ids(&self) -> Vec<EvSubscriptionId> {
        self.subs
            .iter()
            .map(|(raw, _)| EvSubscriptionId { raw: *raw })
            .collect()
    }

    /// The union of every live subscription's watched worktrees — what a
    /// reconciliation pass has to look at. `WatchDeadline` names no
    /// worktree, so it contributes nothing here — deadlines are checked by
    /// [`Self::fire_due_deadlines`], never through a git-observing pass.
    pub fn watched_worktrees(&self) -> Vec<WtWorktreeId> {
        let mut out: Vec<WtWorktreeId> = Vec::new();
        for (_, sub) in &self.subs {
            for w in &sub.watches {
                let id = match w {
                    EvWatch::WatchCommit(id) | EvWatch::WatchHead(id) => id,
                    EvWatch::WatchDeadline(_) => continue,
                };
                if !out.contains(id) {
                    out.push(id.clone());
                }
            }
        }
        out
    }

    fn lookup_mut(&mut self, raw: i64) -> Option<&mut Subscription> {
        self.subs
            .iter_mut()
            .find(|(id, _)| *id == raw)
            .map(|(_, s)| s)
    }
}

// ============================================================================
// Where observations come from
// ============================================================================

/// One reconciliation pass over the named worktrees.
///
/// Injectable because the git reasoning belongs to
/// [`WorktreeMonitor`](tidepool_worktree::WorktreeMonitor) (lane L3), not here:
/// this crate must not grow a second, drifting notion of what "HEAD moved"
/// means. [`MonitorObservations`] is the production implementation and is
/// deliberately thin.
///
/// Contract: `observe` reads each named worktree FRESH (never a cache, never a
/// hook payload) and returns what changed since its own last observation, in
/// observation order — empty when nothing moved. Co-emitted views of one
/// underlying change share one [`EvEventId`], which is how a consumer tells two
/// views of one change from two changes.
///
/// **Baseline obligation.** "Since its last observation" must mean the last
/// JOURNALLED observation, never one held only in process memory. A source
/// whose baseline is memory concludes that nothing moved on the first pass of a
/// new cycle, and every commit made in the gap between cycles vanishes — a
/// silently dropped commit, which the PRD forbids as firmly as a dropped queue
/// entry. A "start from now" source is therefore legitimate ONLY in a
/// single-cycle test; it must never be the production implementation.
pub trait ObservationSource: Send {
    fn observe(&mut self, worktrees: &[WtWorktreeId])
        -> Result<Vec<EvRepositoryEvent>, EventError>;
}

/// The production source: [`WorktreeMonitor`](tidepool_worktree::WorktreeMonitor)
/// does the git reading and the honest classification; this adapter only maps
/// its domain types onto the wire types and mints the per-pass event id.
///
/// Anything more than that here would be a duplicate of the monitor's
/// reasoning — the classification (`Advanced` vs `Rewound` vs `UnknownChange`)
/// is the part that must exist exactly once.
///
/// **This adapter holds NO baseline of its own.** It keeps no last-observed
/// head, no cursor, and no seen-set: every comparison is
/// [`WorktreeMonitor::reconcile`](tidepool_worktree::WorktreeMonitor::reconcile)'s,
/// and that monitor's own contract is that its restart baseline is its last
/// JOURNALLED observation rather than an in-memory one. Keeping a second
/// baseline here would silently override the durable one and lose exactly the
/// between-cycle movement the journal exists to preserve.
///
/// It carries NO other state: the monitor is the only thing it holds. The
/// `EventId` on each observation is the one the monitor minted and journalled
/// under, so `Observed.eventId` correlates with its journal row.
pub struct MonitorObservations {
    monitor: tidepool_worktree::WorktreeMonitor,
}

impl MonitorObservations {
    pub fn new(monitor: tidepool_worktree::WorktreeMonitor) -> Self {
        Self { monitor }
    }

    /// Start watching `worktree` at `path`, and record that `observe` may
    /// reconcile it. Forwards to
    /// [`WorktreeMonitor::register`](tidepool_worktree::WorktreeMonitor::register),
    /// which establishes the starting baseline from the journal when it has one
    /// (so movement during a process gap is still reported) and from a fresh
    /// git read otherwise.
    pub fn register(
        &mut self,
        worktree: WtWorktreeId,
        path: std::path::PathBuf,
    ) -> Result<(), EventError> {
        let domain_id = tidepool_worktree::WorktreeId::from_raw(worktree.raw.clone());
        self.monitor
            .register(domain_id, path)
            .map_err(worktree_error_to_event_error)
    }
}

impl ObservationSource for MonitorObservations {
    fn observe(
        &mut self,
        worktrees: &[WtWorktreeId],
    ) -> Result<Vec<EvRepositoryEvent>, EventError> {
        let mut out = Vec::new();
        for wire_id in worktrees {
            let domain_id = tidepool_worktree::WorktreeId::from_raw(wire_id.raw.clone());
            // An unregistered id is now a TYPED failure from the monitor
            // itself, mapped like every other monitor error — the ids reaching
            // here come from author-supplied `Watch` values, so this is a case
            // authors can hit and must be able to case on.
            let facts = self
                .monitor
                .reconcile(&domain_id)
                .map_err(worktree_error_to_event_error)?;
            // The id on each observation is the one the monitor minted for this
            // pass and JOURNALLED under, so co-emitted views of one change still
            // share an id AND that id correlates with the journal row.
            out.extend(facts.iter().map(|o| {
                domain_event_to_wire(
                    EvEventId {
                        raw: o.event_id.0 as i64,
                    },
                    &o.value,
                )
            }));
        }
        Ok(out)
    }
}

/// A watched worktree that is gone is a DIFFERENT failure from git misbehaving,
/// and the authored surface distinguishes them, so the mapping does too.
fn worktree_error_to_event_error(e: tidepool_worktree::WorktreeError) -> EventError {
    match e {
        tidepool_worktree::WorktreeError::WorktreeLost(id) => {
            EventError::EventSourceLost(id.to_string())
        }
        other => EventError::EventSourceFailed(format!("{other}")),
    }
}

/// Domain (`tidepool-worktree`) → wire (`tidepool-bridge-effects`). The two are
/// deliberately separate types; this is the one explicit conversion between
/// them, so a change on either side surfaces here rather than silently.
fn domain_event_to_wire(
    event_id: EvEventId,
    ev: &tidepool_worktree::RepositoryEvent,
) -> EvRepositoryEvent {
    match ev {
        tidepool_worktree::RepositoryEvent::Commit(r) => EvRepositoryEvent::ObservedCommit(
            event_id,
            EvCommitReceipt {
                commit_worktree: WtWorktreeId {
                    raw: r.worktree.as_str().to_string(),
                },
                oid: WtGitOid {
                    raw: r.oid.as_str().to_string(),
                },
                parents: r
                    .parents
                    .iter()
                    .map(|p| WtGitOid {
                        raw: p.as_str().to_string(),
                    })
                    .collect(),
                subject: r.subject.clone(),
                author: r.author.clone(),
                committed_at_ms: r.committed_at_ms,
                files: r.files.clone(),
            },
        ),
        tidepool_worktree::RepositoryEvent::HeadChanged(r) => {
            EvRepositoryEvent::ObservedHeadChange(
                event_id,
                EvHeadChangeReceipt {
                    head_worktree: WtWorktreeId {
                        raw: r.worktree.as_str().to_string(),
                    },
                    old_head: r.old_head.as_ref().map(|o| WtGitOid {
                        raw: o.as_str().to_string(),
                    }),
                    new_head: WtGitOid {
                        raw: r.new_head.as_str().to_string(),
                    },
                    kind: domain_kind_to_wire(&r.kind),
                    head_branch: r.branch.as_ref().map(|b| WtBranchName {
                        raw: b.as_str().to_string(),
                    }),
                    observed_at_ms: r.observed_at_ms,
                },
            )
        }
    }
}

fn domain_kind_to_wire(k: &tidepool_worktree::HeadChangeKind) -> EvHeadChangeKind {
    let oid = |o: &tidepool_worktree::GitOid| WtGitOid {
        raw: o.as_str().to_string(),
    };
    match k {
        tidepool_worktree::HeadChangeKind::Advanced(os) => {
            EvHeadChangeKind::Advanced(os.iter().map(oid).collect())
        }
        tidepool_worktree::HeadChangeKind::Amended(a, b) => {
            EvHeadChangeKind::Amended(oid(a), oid(b))
        }
        tidepool_worktree::HeadChangeKind::Rewritten(ps) => {
            EvHeadChangeKind::Rewritten(ps.iter().map(|(a, b)| (oid(a), oid(b))).collect())
        }
        tidepool_worktree::HeadChangeKind::Rewound => EvHeadChangeKind::Rewound,
        tidepool_worktree::HeadChangeKind::Switched => EvHeadChangeKind::Switched,
        tidepool_worktree::HeadChangeKind::UnknownChange => EvHeadChangeKind::UnknownChange,
    }
}

// ============================================================================
// The handler
// ============================================================================

/// Serves `RepoEventSubscribe` / `RepoEventDrain` / `RepoEventUnsubscribe`.
///
/// Not in `build_base_stack`'s row: PRD 19's surface is opt-in until the
/// dev-tree dogfood lands, so a caller that wants repository events builds a
/// row containing this handler explicitly.
pub struct RepoEventHandler {
    registry: SubscriptionRegistry,
    source: Box<dyn ObservationSource>,
    poll_interval: Duration,
    last_pass: Option<Instant>,
}

impl RepoEventHandler {
    /// The production wiring: reconcile through a real
    /// [`WorktreeMonitor`](tidepool_worktree::WorktreeMonitor).
    pub fn new(monitor: tidepool_worktree::WorktreeMonitor, config: EventConfig) -> Self {
        Self::with_source(Box::new(MonitorObservations::new(monitor)), config)
    }

    /// Any other observation source. [`MonitorObservations`] is the real
    /// production adapter; the acceptance harness supplies a second source
    /// that reads a real temporary repository directly, scoped to that
    /// harness's single-cycle/process-memory baseline — not because the
    /// production adapter doesn't exist.
    pub fn with_source(source: Box<dyn ObservationSource>, config: EventConfig) -> Self {
        Self {
            registry: SubscriptionRegistry::new(config.queue_bound),
            source,
            poll_interval: config.poll_interval,
            last_pass: None,
        }
    }

    pub fn registry(&self) -> &SubscriptionRegistry {
        &self.registry
    }

    pub fn registry_mut(&mut self) -> &mut SubscriptionRegistry {
        &mut self.registry
    }

    /// Run one reconciliation pass over every watched worktree and broadcast
    /// what it found, at most once per [`EventConfig::poll_interval`].
    ///
    /// The rate limit exists to stop a body's effect storm from becoming a
    /// git-read storm — `withHandler` sends one drain before EVERY effect its
    /// body performs. It never loses anything: a pass reports movement relative
    /// to the observer's durable baseline, so a skipped pass is a delayed
    /// report, not a dropped one.
    fn reconcile(&mut self) -> Result<(), EventError> {
        // Deadlines cost no I/O, so they are checked on EVERY pass —
        // unconditionally, ahead of the git-read rate limit below, and even
        // when nothing is watched for commits/heads at all.
        self.registry.fire_due_deadlines(Instant::now());
        if let Some(last) = self.last_pass {
            if last.elapsed() < self.poll_interval {
                return Ok(());
            }
        }
        let watched = self.registry.watched_worktrees();
        if watched.is_empty() {
            return Ok(());
        }
        self.last_pass = Some(Instant::now());
        for event in self.source.observe(&watched)? {
            self.registry.publish(&event);
        }
        Ok(())
    }

    // Errors-tagged verbs: total in `EventError`, no `cx` — the generated
    // dispatch arm wraps the `Result` via `cx.respond` (Ok→Right, Err→Left).
    // See #335 and `tidepool-handlers/CLAUDE.md`.

    fn repo_event_subscribe(
        &mut self,
        watches: Vec<EvWatch>,
    ) -> Result<EvSubscriptionId, EventError> {
        // A cheap registry insert, and deliberately nothing else. It reads no
        // git and it consumes no backlog: the first pass of a cycle belongs to
        // the first DRAIN, after this subscription is live, so movement that
        // happened while nobody was subscribed reaches it instead of being
        // quietly absorbed by the act of registering. See the module docs.
        Ok(self.registry.subscribe(watches))
    }

    fn repo_event_drain(
        &mut self,
        subscription: EvSubscriptionId,
    ) -> Result<Vec<EvRepositoryEvent>, EventError> {
        // A drain is where polling happens: `withHandler`'s interposition sends
        // one before every effect its body performs, so this is the natural —
        // and rate-limited — heartbeat.
        self.reconcile()?;
        self.registry.drain(subscription)
    }

    fn repo_event_unsubscribe(&mut self, subscription: EvSubscriptionId) -> Result<(), EventError> {
        self.registry.unsubscribe(subscription)
    }

    /// Block until `subscription` has queued at least one observation, or
    /// `timeout_ms` elapses (negative == no deadline — block until a match).
    /// Loop: reconcile pass (deadlines every time, git reads still bounded by
    /// `poll_interval`), check the queue, sleep bounded by both
    /// `poll_interval` and the remaining time to the deadline. An elapsed
    /// deadline returns an EMPTY batch — typed data, never an `EventError` —
    /// so it is distinguishable from a real (non-empty) observation without
    /// a second signal. Poison/overflow still fail loudly via `drain`'s own
    /// `Err`, exactly as `repo_event_drain` does.
    fn repo_event_await(
        &mut self,
        subscription: EvSubscriptionId,
        timeout_ms: i64,
    ) -> Result<Vec<EvRepositoryEvent>, EventError> {
        let deadline = if timeout_ms < 0 {
            None
        } else {
            // `checked_add` rather than a bare `+`: an absurdly large
            // timeout must not PANIC the handler — falling back to "no
            // deadline" is the same failure mode as "block until a match".
            Instant::now().checked_add(Duration::from_millis(timeout_ms as u64))
        };
        loop {
            self.reconcile()?;
            let batch = self.registry.drain(subscription)?;
            if !batch.is_empty() {
                return Ok(batch);
            }
            if let Some(dl) = deadline {
                if Instant::now() >= dl {
                    return Ok(Vec::new());
                }
            }
            // Bound the sleep by the poll interval (so a deadline that fires
            // between passes is still noticed promptly) and by the
            // remaining time to the deadline (so we never sleep past it); a
            // zero poll interval still yields instead of busy-spinning.
            let mut step = self.poll_interval.max(Duration::from_millis(1));
            if let Some(dl) = deadline {
                step = step.min(dl.saturating_duration_since(Instant::now()));
            }
            std::thread::sleep(step);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wt(name: &str) -> WtWorktreeId {
        WtWorktreeId {
            raw: name.to_string(),
        }
    }

    fn commit_event(id: i64, tree: &str, oid: &str) -> EvRepositoryEvent {
        EvRepositoryEvent::ObservedCommit(
            EvEventId { raw: id },
            EvCommitReceipt {
                commit_worktree: wt(tree),
                oid: WtGitOid {
                    raw: oid.to_string(),
                },
                parents: vec![],
                subject: format!("subject {oid}"),
                author: "t".into(),
                committed_at_ms: 0,
                files: vec![],
            },
        )
    }

    fn head_event(id: i64, tree: &str, oid: &str) -> EvRepositoryEvent {
        EvRepositoryEvent::ObservedHeadChange(
            EvEventId { raw: id },
            EvHeadChangeReceipt {
                head_worktree: wt(tree),
                old_head: None,
                new_head: WtGitOid {
                    raw: oid.to_string(),
                },
                kind: EvHeadChangeKind::UnknownChange,
                head_branch: None,
                observed_at_ms: 0,
            },
        )
    }

    fn oids(evs: &[EvRepositoryEvent]) -> Vec<String> {
        evs.iter()
            .map(|e| match e {
                EvRepositoryEvent::ObservedCommit(_, r) => r.oid.raw.clone(),
                EvRepositoryEvent::ObservedHeadChange(_, r) => r.new_head.raw.clone(),
                EvRepositoryEvent::ObservedTick(_, t) => format!("tick@{}", t.fired_at_ms),
            })
            .collect()
    }

    #[test]
    fn a_fresh_subscription_never_sees_what_was_published_before_it() {
        let mut reg = SubscriptionRegistry::new(8);
        reg.publish(&commit_event(1, "a", "old"));
        let sub = reg.subscribe(vec![EvWatch::WatchCommit(wt("a"))]);
        assert_eq!(reg.drain(sub).unwrap(), vec![]);
        reg.publish(&commit_event(2, "a", "new"));
        assert_eq!(oids(&reg.drain(sub).unwrap()), vec!["new"]);
    }

    #[test]
    fn publish_broadcasts_rather_than_being_consumed_by_the_first_observer() {
        let mut reg = SubscriptionRegistry::new(8);
        let a = reg.subscribe(vec![EvWatch::WatchCommit(wt("a"))]);
        let b = reg.subscribe(vec![EvWatch::WatchCommit(wt("a"))]);
        reg.publish(&commit_event(1, "a", "c1"));
        assert_eq!(oids(&reg.drain(a).unwrap()), vec!["c1"]);
        assert_eq!(oids(&reg.drain(b).unwrap()), vec!["c1"]);
    }

    #[test]
    fn a_watch_selects_on_kind_and_worktree_together() {
        let mut reg = SubscriptionRegistry::new(8);
        let commits_a = reg.subscribe(vec![EvWatch::WatchCommit(wt("a"))]);
        let heads_a = reg.subscribe(vec![EvWatch::WatchHead(wt("a"))]);
        let commits_b = reg.subscribe(vec![EvWatch::WatchCommit(wt("b"))]);
        reg.publish(&commit_event(1, "a", "c1"));
        reg.publish(&head_event(1, "a", "c1"));
        assert_eq!(oids(&reg.drain(commits_a).unwrap()), vec!["c1"]);
        assert_eq!(oids(&reg.drain(heads_a).unwrap()), vec!["c1"]);
        assert_eq!(reg.drain(commits_b).unwrap(), vec![]);
    }

    #[test]
    fn a_merged_event_is_one_subscription_over_several_watches() {
        // `<|>` concatenates watches, so `commit a <|> headChanged b` is ONE
        // registration that must see both kinds.
        let mut reg = SubscriptionRegistry::new(8);
        let sub = reg.subscribe(vec![
            EvWatch::WatchCommit(wt("a")),
            EvWatch::WatchHead(wt("b")),
        ]);
        reg.publish(&commit_event(1, "a", "c1"));
        reg.publish(&head_event(2, "b", "h1"));
        reg.publish(&commit_event(3, "b", "ignored"));
        assert_eq!(oids(&reg.drain(sub).unwrap()), vec!["c1", "h1"]);
    }

    #[test]
    fn drain_returns_observation_order_and_leaves_the_queue_empty() {
        let mut reg = SubscriptionRegistry::new(8);
        let sub = reg.subscribe(vec![EvWatch::WatchCommit(wt("a"))]);
        for (i, oid) in ["c1", "c2", "c3"].iter().enumerate() {
            reg.publish(&commit_event(i as i64, "a", oid));
        }
        assert_eq!(oids(&reg.drain(sub).unwrap()), vec!["c1", "c2", "c3"]);
        assert_eq!(reg.drain(sub).unwrap(), vec![]);
    }

    #[test]
    fn an_unsubscribed_id_is_an_error_not_a_silent_empty() {
        let mut reg = SubscriptionRegistry::new(8);
        let sub = reg.subscribe(vec![EvWatch::WatchCommit(wt("a"))]);
        assert_eq!(reg.unsubscribe(sub), Ok(()));
        assert_eq!(
            reg.drain(sub),
            Err(EventError::EventUnknownSubscription(sub.raw))
        );
        assert_eq!(
            reg.unsubscribe(sub),
            Err(EventError::EventUnknownSubscription(sub.raw))
        );
    }

    #[test]
    fn subscription_ids_are_never_reused() {
        let mut reg = SubscriptionRegistry::new(8);
        let a = reg.subscribe(vec![]);
        reg.unsubscribe(a).unwrap();
        let b = reg.subscribe(vec![]);
        assert_ne!(a.raw, b.raw, "a spent id must never be handed out again");
    }

    #[test]
    fn overflow_poisons_the_subscription_and_reports_the_dropped_count() {
        let mut reg = SubscriptionRegistry::new(2);
        let sub = reg.subscribe(vec![EvWatch::WatchCommit(wt("a"))]);
        for oid in ["c1", "c2", "c3", "c4"] {
            reg.publish(&commit_event(0, "a", oid));
        }
        assert_eq!(
            reg.drain(sub),
            Err(EventError::EventQueueOverflow(sub.raw, 2))
        );
    }

    #[test]
    fn a_poisoned_subscription_never_silently_recovers() {
        let mut reg = SubscriptionRegistry::new(1);
        let sub = reg.subscribe(vec![EvWatch::WatchCommit(wt("a"))]);
        reg.publish(&commit_event(0, "a", "c1"));
        reg.publish(&commit_event(0, "a", "c2"));
        assert!(reg.drain(sub).is_err());
        // A quiet period does not heal it: the commit it dropped is still
        // dropped, and a drain that started succeeding again would say
        // otherwise.
        assert_eq!(
            reg.drain(sub),
            Err(EventError::EventQueueOverflow(sub.raw, 1))
        );
        reg.publish(&commit_event(0, "a", "c3"));
        assert_eq!(
            reg.drain(sub),
            Err(EventError::EventQueueOverflow(sub.raw, 2))
        );
    }

    #[test]
    fn overflow_is_per_subscription_not_registry_wide() {
        let mut reg = SubscriptionRegistry::new(1);
        let slow = reg.subscribe(vec![EvWatch::WatchCommit(wt("a"))]);
        let fast = reg.subscribe(vec![EvWatch::WatchCommit(wt("a"))]);
        reg.publish(&commit_event(0, "a", "c1"));
        assert_eq!(oids(&reg.drain(fast).unwrap()), vec!["c1"]);
        reg.publish(&commit_event(0, "a", "c2"));
        assert!(reg.drain(slow).is_err(), "the slow one overflowed");
        assert_eq!(
            oids(&reg.drain(fast).unwrap()),
            vec!["c2"],
            "its neighbour keeps working"
        );
    }

    #[test]
    fn watched_worktrees_is_the_deduped_union_of_live_watches() {
        let mut reg = SubscriptionRegistry::new(8);
        let a = reg.subscribe(vec![
            EvWatch::WatchCommit(wt("a")),
            EvWatch::WatchHead(wt("a")),
        ]);
        reg.subscribe(vec![EvWatch::WatchCommit(wt("b"))]);
        assert_eq!(reg.watched_worktrees(), vec![wt("a"), wt("b")]);
        reg.unsubscribe(a).unwrap();
        assert_eq!(reg.watched_worktrees(), vec![wt("b")]);
    }

    // ── the handler's own wiring, over a scripted source (no git, no JIT) ──

    /// Hands back a canned pass per call, and counts the passes. Not a mock of
    /// git: the git-behaviour proof is the acceptance harness against a real
    /// repository. This exists to pin the RATE LIMIT and the subscribe/poll
    /// ordering, which are properties of this module, not of git.
    struct ScriptedSource {
        passes: Vec<Vec<EvRepositoryEvent>>,
        calls: PassCounter,
    }

    type PassCounter = std::sync::Arc<std::sync::atomic::AtomicUsize>;

    impl ObservationSource for ScriptedSource {
        fn observe(
            &mut self,
            _worktrees: &[WtWorktreeId],
        ) -> Result<Vec<EvRepositoryEvent>, EventError> {
            let n = self
                .calls
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Ok(self.passes.get(n).cloned().unwrap_or_default())
        }
    }

    fn passes_run(c: &PassCounter) -> usize {
        c.load(std::sync::atomic::Ordering::Relaxed)
    }

    fn handler_with(
        passes: Vec<Vec<EvRepositoryEvent>>,
        poll_interval: Duration,
    ) -> (RepoEventHandler, PassCounter) {
        let calls: PassCounter = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let h = RepoEventHandler::with_source(
            Box::new(ScriptedSource {
                passes,
                calls: calls.clone(),
            }),
            EventConfig {
                queue_bound: 8,
                poll_interval,
            },
        );
        (h, calls)
    }

    #[test]
    fn registration_reads_nothing_and_consumes_no_backlog() {
        // Pass 0 carries a commit that happened while nobody was subscribed.
        // If registering polled, that pass would run with no subscriber and the
        // commit would be gone; because it does not, the first DRAIN runs it
        // and the subscription — live by then — receives it.
        let (mut h, calls) = handler_with(vec![vec![commit_event(1, "a", "c1")]], Duration::ZERO);
        let sub = h
            .repo_event_subscribe(vec![EvWatch::WatchCommit(wt("a"))])
            .unwrap();
        assert_eq!(
            passes_run(&calls),
            0,
            "registration is a registry insert; it must not poll"
        );
        assert_eq!(
            oids(&h.repo_event_drain(sub).unwrap()),
            vec!["c1"],
            "movement from the window with no subscriber must not be swallowed \
             by the act of registering"
        );
    }

    #[test]
    fn a_later_registration_does_not_replay_an_earlier_ones_facts() {
        // Re-registration is an ordinary repeated path (a new resident cycle
        // rebuilds its reactions from state + stable worktree ids). The second
        // subscription must start empty and then track new facts.
        let (mut h, _calls) = handler_with(
            vec![
                vec![commit_event(1, "a", "c1")],
                vec![],
                vec![commit_event(2, "a", "c2")],
            ],
            Duration::ZERO,
        );
        let first = h
            .repo_event_subscribe(vec![EvWatch::WatchCommit(wt("a"))])
            .unwrap();
        assert_eq!(oids(&h.repo_event_drain(first).unwrap()), vec!["c1"]);
        h.repo_event_unsubscribe(first).unwrap();

        let second = h
            .repo_event_subscribe(vec![EvWatch::WatchCommit(wt("a"))])
            .unwrap();
        assert_eq!(
            oids(&h.repo_event_drain(second).unwrap()),
            Vec::<String>::new(),
            "a fact the previous registration already consumed must not replay"
        );
        assert_eq!(
            oids(&h.repo_event_drain(second).unwrap()),
            vec!["c2"],
            "and the new registration still tracks everything after it"
        );
    }

    #[test]
    fn drain_reconciles_at_most_once_per_poll_interval() {
        let (mut h, calls) = handler_with(vec![], Duration::from_secs(3600));
        let sub = h
            .repo_event_subscribe(vec![EvWatch::WatchCommit(wt("a"))])
            .unwrap();
        for _ in 0..5 {
            h.repo_event_drain(sub).unwrap();
        }
        assert_eq!(
            passes_run(&calls),
            1,
            "an effect storm inside the body must not become a git-read storm"
        );
    }

    #[test]
    fn a_zero_poll_interval_reconciles_on_every_drain() {
        let (mut h, calls) = handler_with(vec![], Duration::ZERO);
        let sub = h
            .repo_event_subscribe(vec![EvWatch::WatchCommit(wt("a"))])
            .unwrap();
        for _ in 0..3 {
            h.repo_event_drain(sub).unwrap();
        }
        assert_eq!(passes_run(&calls), 3);
    }

    #[test]
    fn nothing_is_observed_once_the_last_subscription_is_gone() {
        let (mut h, calls) = handler_with(vec![], Duration::ZERO);
        let sub = h
            .repo_event_subscribe(vec![EvWatch::WatchCommit(wt("a"))])
            .unwrap();
        h.repo_event_drain(sub).unwrap();
        h.repo_event_unsubscribe(sub).unwrap();
        let before = passes_run(&calls);
        // Nothing is watched, so a would-be pass has nothing to read — and the
        // spent id still fails loudly rather than answering empty.
        assert_eq!(
            h.repo_event_drain(sub),
            Err(EventError::EventUnknownSubscription(sub.raw))
        );
        assert_eq!(passes_run(&calls), before);
    }

    // ── await / deadlines (PRD 20, S1-L3) ──

    #[test]
    fn await_returns_as_soon_as_a_pass_produces_an_observation() {
        // A large poll interval would rate-limit every LATER pass, but the
        // very first one always runs (no `last_pass` yet) — so a match
        // already sitting in the first scripted pass must come back well
        // before the generous timeout elapses.
        let (mut h, _calls) = handler_with(
            vec![vec![commit_event(1, "a", "c1")]],
            Duration::from_secs(3600),
        );
        let sub = h
            .repo_event_subscribe(vec![EvWatch::WatchCommit(wt("a"))])
            .unwrap();
        let start = Instant::now();
        let batch = h.repo_event_await(sub, 5_000).unwrap();
        assert_eq!(oids(&batch), vec!["c1"]);
        assert!(
            start.elapsed() < Duration::from_millis(500),
            "must not wait out most of a generous timeout when the match is already there"
        );
    }

    #[test]
    fn await_times_out_with_an_empty_batch() {
        let (mut h, _calls) = handler_with(vec![], Duration::from_millis(5));
        let sub = h
            .repo_event_subscribe(vec![EvWatch::WatchCommit(wt("a"))])
            .unwrap();
        assert_eq!(
            h.repo_event_await(sub, 30).unwrap(),
            vec![],
            "an elapsed timeout is an empty batch, not an error"
        );
        // The subscription itself is unharmed — an ordinary drain still works.
        assert_eq!(h.repo_event_drain(sub).unwrap(), vec![]);
    }

    #[test]
    fn a_deadline_watch_fires_exactly_once() {
        let (mut h, _calls) = handler_with(vec![], Duration::from_millis(5));
        let sub = h
            .repo_event_subscribe(vec![EvWatch::WatchDeadline(0)])
            .unwrap();
        let first = h.repo_event_await(sub, 200).unwrap();
        assert_eq!(first.len(), 1);
        assert!(
            matches!(first[0], EvRepositoryEvent::ObservedTick(_, _)),
            "a due WatchDeadline must fire a Tick"
        );
        // No second tick — it fired exactly once, so a later await on the
        // SAME subscription times out empty rather than re-delivering it.
        assert_eq!(h.repo_event_await(sub, 30).unwrap(), vec![]);
    }

    #[test]
    fn an_overflowed_subscription_poisons_awaits() {
        let (mut h, _calls) = handler_with(vec![], Duration::ZERO);
        // `handler_with` fixes queue_bound at 8 — nine deadlines due at once
        // overflow the ninth.
        let watches: Vec<EvWatch> = (0..9).map(|_| EvWatch::WatchDeadline(0)).collect();
        let sub = h.repo_event_subscribe(watches).unwrap();
        assert_eq!(
            h.repo_event_await(sub, 200),
            Err(EventError::EventQueueOverflow(sub.raw, 1))
        );
        // No recovery: a later await on the same subscription still fails,
        // loudly, rather than quietly answering empty.
        assert!(h.repo_event_await(sub, 30).is_err());
    }

    #[test]
    fn the_poll_interval_rate_limit_still_bounds_git_reads_inside_the_await_loop() {
        let (mut h, calls) = handler_with(vec![], Duration::from_secs(3600));
        let sub = h
            .repo_event_subscribe(vec![EvWatch::WatchCommit(wt("a"))])
            .unwrap();
        assert_eq!(h.repo_event_await(sub, 50).unwrap(), vec![]);
        assert_eq!(
            passes_run(&calls),
            1,
            "the await loop's repeated iterations must not re-read git faster than poll_interval"
        );
    }
}
