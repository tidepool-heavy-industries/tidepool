//! Repository events — the runtime half of `withHandler`.
//!
//! `withHandler` itself is HASKELL: a scoped interposition over its body's
//! freer-simple structure that runs a drain before every effect the body
//! performs (`pumpEff`, `drainSubscription` — DEFINITIONS in
//! `haskell/lib/Tidepool/Event.hs`). The author's handler
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
//! subscription existed.** Between two resident loop iterations there is a gap with
//! agents still running and nobody subscribed. Source registration recovers a
//! previously observed worktree's baseline from its journal, so the next drain
//! reports movement from that gap. A genuinely never-before-observed worktree
//! is different: subscription registration fixes its fresh HEAD as the cutoff,
//! without emitting or replaying history. Any movement after that cutoff is a
//! delta for the live subscription, even when another source is in cooldown.
//!
//! Both rules hold at once, and for one reason: the durable baseline is the
//! JOURNAL (the observer's business — see [`MonitorObservations`]), and the
//! queue is per-subscription (this registry's business).
//!
//! ## Blocking await, and deadlines
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
//! ## Loop-iteration-scoped, and re-registered every loop iteration
//!
//! A subscription never crosses a resident loop-iteration boundary. The registry is
//! owned by [`RepoEventHandler`], which is owned by the loop iteration's handler stack;
//! dropping that stack ends every registration. There is no durable
//! subscription store here and there must not be one — an attached Haskell
//! handle, a parked Haskell continuation, and an event subscription are the
//! three things that must never survive a loop iteration.
//!
//! Re-registering from explicit state and stable worktree ids each loop iteration is
//! therefore an ordinary, repeated path, not a recovery story. It is safe
//! precisely because of the paragraph above: the new loop iteration's subscriptions are
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

use std::collections::{HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant};

use tidepool_bridge_effects::{
    EvCommitReceipt, EvEventId, EvHeadChangeKind, EvHeadChangeReceipt, EvRepositoryEvent,
    EvSubscriptionId, EvTickReceipt, EvWatch, WtBranchName, WtGitOid, WtWorktreeId,
};
use tidepool_repr::MonotonicIdIssuer;
use tidepool_worktree::storage::now_ms;

// Wall-clock epoch milliseconds for `Tick`'s `firedAtMs` — an observability
// stamp only; internal deadline SCHEDULING uses the monotonic `Instant` clock
// instead, immune to a system-clock jump. `now_ms` now PANICS on a pre-epoch
// clock (`tidepool_worktree::storage::now_ms`'s documented behavior), unlike
// this call site's prior local copy, which silently stamped `0` — see that
// module's docs for why the panic won.

// EventError, RepoEventReq, DescribeEffect and the EffectHandler dispatch are
// GENERATED from the `tidepool-protocol` schema — re-exported
// here so the public paths (`tidepool_handlers::EventError`,
// `tidepool_handlers::RepoEventReq`) are unchanged. Only the handler struct,
// the registry, and the per-verb methods below are hand-written.
pub use crate::generated::repo_event::{EventError, RepoEventReq};

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
    /// Minimum wall-clock gap between successful reconciliation passes for one
    /// source. Failed sources remain retryable on the next dependent operation.
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
    /// The driver lifecycle epoch that owns this registration. `None` is for
    /// direct handler users (including the unit-only registry); a resident
    /// driver always installs an explicit owner and closes it on every cycle
    /// exit, including an error path that bypasses Haskell's unsubscribe.
    owner: Option<u64>,
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
    /// Where a still-queued, not-yet-drained mailbox message sits, keyed by
    /// (mailbox, caller-supplied coalesce key) — how
    /// [`SubscriptionRegistry::publish_mailbox_message`] finds the entry a
    /// same-key send must REPLACE in place rather than append. Cleared
    /// whenever `queue` drains to empty: a fresh epoch has nothing left to
    /// coalesce against. Indices stay valid between drains because entries
    /// are only ever appended or replaced in place, never removed singly.
    mailbox_slots: HashMap<(i64, String), usize>,
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
    sub_ids: MonotonicIdIssuer,
    /// Separate id space from `sub_ids` (subscription ids) and from the ids
    /// [`MonitorObservations`] mints for git-observed facts — this registry
    /// is the only minter of a `Tick`'s [`EvEventId`], since no monitor pass
    /// produces one. Starts at 2 (not 1) to preserve the original
    /// pre-increment counter's first-minted value.
    event_ids: MonotonicIdIssuer,
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
            sub_ids: MonotonicIdIssuer::new("sub"),
            event_ids: MonotonicIdIssuer::starting_at("event", 2),
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
        self.subscribe_owned(watches, None)
    }

    fn subscribe_owned(&mut self, watches: Vec<EvWatch>, owner: Option<u64>) -> EvSubscriptionId {
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
        let raw = self.sub_ids.next_raw() as i64;
        self.subs.push((
            raw,
            Subscription {
                owner,
                watches,
                queue: VecDeque::new(),
                dropped: 0,
                pending_deadlines,
                mailbox_slots: HashMap::new(),
            },
        ));
        EvSubscriptionId { raw }
    }

    /// Queue a retained mailbox message for one newly-created subscription.
    /// Retained entries have already been coalesced by `MailboxTable`, so this
    /// only needs the ordinary bounded-delivery rule.
    fn queue_retained_mailbox(
        &mut self,
        id: EvSubscriptionId,
        mailbox: i64,
        key: String,
        payload: serde_json::Value,
    ) {
        let bound = self.bound;
        let event_id = EvEventId {
            raw: self.event_ids.next_raw() as i64,
        };
        let Some(sub) = self.lookup_mut(id.raw) else {
            return;
        };
        if sub.poisoned() || sub.queue.len() >= bound {
            sub.dropped += 1;
            return;
        }
        let idx = sub.queue.len();
        sub.queue.push_back(EvRepositoryEvent::ObservedMessage(
            event_id, mailbox, payload,
        ));
        sub.mailbox_slots.insert((mailbox, key), idx);
    }

    /// A terminal async state is level-triggered at registration: queue it
    /// for this subscription only, rather than pretending a past transition
    /// can be broadcast again.
    pub fn observe_terminal_async(&mut self, id: EvSubscriptionId, tid: i64) {
        let bound = self.bound;
        let event_id = EvEventId {
            raw: self.event_ids.next_raw() as i64,
        };
        let Some(sub) = self.lookup_mut(id.raw) else {
            return;
        };
        if !sub
            .watches
            .iter()
            .any(|w| matches!(w, EvWatch::WatchAsync(i) if *i == tid))
        {
            return;
        }
        if sub.poisoned() || sub.queue.len() >= bound {
            sub.dropped += 1;
            return;
        }
        sub.queue
            .push_back(EvRepositoryEvent::ObservedAsyncDone(event_id, tid));
    }

    pub fn close_owner(&mut self, owner: u64) {
        self.subs.retain(|(_, sub)| sub.owner != Some(owner));
    }

    /// A mailbox may have overflowed before any subscription existed. Its
    /// first claimant inherits that named loss, just as if its own bounded
    /// queue had overflowed while live; retained messages are not presented
    /// as a deceptively complete prefix.
    fn poison_from_retained_mailbox(&mut self, id: EvSubscriptionId, dropped: i64) {
        if let Some(sub) = self.lookup_mut(id.raw) {
            sub.dropped += dropped;
        }
    }

    fn has_healthy_mailbox_receiver(&self, mailbox: i64) -> bool {
        self.subs.iter().any(|(_, sub)| {
            !sub.poisoned()
                && sub
                    .watches
                    .iter()
                    .any(|w| matches!(w, EvWatch::WatchMailbox(m) if *m == mailbox))
        })
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
                let id = self.event_ids.next_raw() as i64;
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

    /// Mint a fresh [`EvEventId`] and broadcast `ObservedAsyncDone` for a
    /// green thread that just reached a terminal state (settle OR cancel —
    /// the completion watch backing `WatchAsync`/
    /// `Tidepool.Event.waitEvent`). Called by the DRIVER's own scheduler
    /// bookkeeping, never by an authored `RepoEvent` verb — a thread
    /// settling is not a request/response the Haskell side ever sends, so
    /// there is no `RepoEventReq` variant for this; the driver reaches
    /// straight into the registry the moment it decides a thread is
    /// terminal. Shares `publish`'s broadcast/bound/poison rule.
    pub fn publish_async_done(&mut self, tid: i64) {
        let id = EvEventId {
            raw: self.event_ids.next_raw() as i64,
        };
        self.publish(&EvRepositoryEvent::ObservedAsyncDone(id, tid));
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

    /// Publish one mailbox message, coalescing by (mailbox, key) — a caller
    /// send whose key already sits, undrained, in a subscription's queue
    /// REPLACES that entry in place (latest payload, earliest position); a
    /// fresh key appends. Shares `publish`'s queue, bound, and
    /// poison-on-overflow rule: a poisoned subscription only ever counts the
    /// loss, and a coalesced replacement never itself grows the queue, so
    /// coalescing can never be the thing that overflows it.
    pub fn publish_mailbox_message(&mut self, mailbox: i64, key: &str, payload: serde_json::Value) {
        for (_, sub) in self.subs.iter_mut() {
            if !sub
                .watches
                .iter()
                .any(|w| matches!(w, EvWatch::WatchMailbox(m) if *m == mailbox))
            {
                continue;
            }
            if sub.poisoned() {
                sub.dropped += 1;
                continue;
            }
            let slot_key = (mailbox, key.to_string());
            if let Some(&idx) = sub.mailbox_slots.get(&slot_key) {
                sub.queue[idx] = EvRepositoryEvent::ObservedMessage(
                    EvEventId {
                        raw: self.event_ids.next_raw() as i64,
                    },
                    mailbox,
                    payload.clone(),
                );
                continue;
            }
            if sub.queue.len() >= self.bound {
                sub.dropped += 1;
                continue;
            }
            let idx = sub.queue.len();
            sub.queue.push_back(EvRepositoryEvent::ObservedMessage(
                EvEventId {
                    raw: self.event_ids.next_raw() as i64,
                },
                mailbox,
                payload.clone(),
            ));
            sub.mailbox_slots.insert(slot_key, idx);
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
        // Every queued index a mailbox message coalesced against is about to
        // be drained away; the next send with that key has nothing left to
        // replace, so it must append fresh.
        sub.mailbox_slots.clear();
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
    /// reconciliation pass has to look at. `WatchDeadline`, `WatchAsync`, and
    /// `WatchMailbox` name no worktree, so they contribute nothing here —
    /// deadlines are checked by [`Self::fire_due_deadlines`], async-done and
    /// mailbox facts arrive through their own publish paths, never through a
    /// git-observing pass.
    pub fn watched_worktrees(&self) -> Vec<WtWorktreeId> {
        let mut out: Vec<WtWorktreeId> = Vec::new();
        for (_, sub) in &self.subs {
            for w in &sub.watches {
                let id = match w {
                    EvWatch::WatchCommit(id) | EvWatch::WatchHead(id) => id,
                    EvWatch::WatchDeadline(_)
                    | EvWatch::WatchAsync(_)
                    | EvWatch::WatchMailbox(_) => continue,
                };
                if !out.contains(id) {
                    out.push(id.clone());
                }
            }
        }
        out
    }

    /// The worktree sources one live subscription depends on, deduplicated in
    /// authored watch order. Used to route a source-local failure without
    /// failing unrelated subscribers.
    fn subscription_worktrees(
        &self,
        id: EvSubscriptionId,
    ) -> Result<Vec<WtWorktreeId>, EventError> {
        let sub = self
            .subs
            .iter()
            .find(|(raw, _)| *raw == id.raw)
            .map(|(_, sub)| sub)
            .ok_or(EventError::EventUnknownSubscription(id.raw))?;
        let mut out = Vec::new();
        for watch in &sub.watches {
            let source = match watch {
                EvWatch::WatchCommit(id) | EvWatch::WatchHead(id) => id,
                EvWatch::WatchDeadline(_) | EvWatch::WatchAsync(_) | EvWatch::WatchMailbox(_) => {
                    continue
                }
            };
            if !out.contains(source) {
                out.push(source.clone());
            }
        }
        Ok(out)
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

/// Registration and reconciliation for one named worktree source.
///
/// Injectable because the git reasoning belongs to
/// [`WorktreeMonitor`](tidepool_worktree::WorktreeMonitor), not here:
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
/// new loop iteration, and every commit made in the gap between loop iterations vanishes — a
/// silently dropped commit, which the PRD forbids as firmly as a dropped queue
/// entry. A "start from now" source is therefore legitimate ONLY in a
/// single-loop-iteration test; it must never be the production implementation.
pub trait ObservationSource: Send {
    /// Establish this source's "start from now" cutoff before a subscription
    /// becomes live. Production resolves the durable worktree path and primes
    /// the monitor here; focused policy seams may keep the default no-op when
    /// they already carry an explicit baseline.
    fn register(&mut self, _worktree: &WtWorktreeId) -> Result<(), EventError> {
        Ok(())
    }

    /// Reconcile exactly one source. Keeping the result source-scoped is what
    /// lets a caller retain A's journalled facts when an independent B fails.
    fn observe(&mut self, worktree: &WtWorktreeId) -> Result<Vec<EvRepositoryEvent>, EventError>;
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
/// between-loop-iteration movement the journal exists to preserve.
///
/// It carries NO other state beyond an optional read handle onto the durable
/// worktree registry, used only to resolve an id `register` was never called
/// for (see [`Self::with_registry`]). The `EventId` on each observation is
/// the one the monitor minted and journalled under, so `Observed.eventId`
/// correlates with its journal row.
pub struct MonitorObservations {
    monitor: tidepool_worktree::WorktreeMonitor,
    /// The durable id→path authority a runtime-created worktree was recorded
    /// in. `None` for callers (tests, the acceptance harness) that only ever
    /// watch worktrees they registered by hand.
    registry: Option<tidepool_worktree::WorktreeRegistry>,
}

impl MonitorObservations {
    pub fn new(monitor: tidepool_worktree::WorktreeMonitor) -> Self {
        Self {
            monitor,
            registry: None,
        }
    }

    /// Same as [`Self::new`], but subscription-time source registration may
    /// resolve an id's path through the durable `registry` before fixing its
    /// cutoff.
    pub fn with_registry(
        monitor: tidepool_worktree::WorktreeMonitor,
        registry: tidepool_worktree::WorktreeRegistry,
    ) -> Self {
        Self {
            monitor,
            registry: Some(registry),
        }
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
    fn register(&mut self, wire_id: &WtWorktreeId) -> Result<(), EventError> {
        let domain_id = tidepool_worktree::WorktreeId::from_raw(wire_id.raw.clone());
        if self.monitor.is_registered(&domain_id) {
            return Ok(());
        }
        if let Some(registry) = &self.registry {
            if let Some(receipt) = registry
                .get(&domain_id)
                .map_err(worktree_error_to_event_error)?
            {
                return self
                    .monitor
                    .register(domain_id, receipt.cwd)
                    .map_err(worktree_error_to_event_error);
            }
        }
        Err(worktree_error_to_event_error(
            tidepool_worktree::WorktreeError::WorktreeNotRegistered(domain_id),
        ))
    }

    fn observe(&mut self, wire_id: &WtWorktreeId) -> Result<Vec<EvRepositoryEvent>, EventError> {
        let mut out = Vec::new();
        let domain_id = tidepool_worktree::WorktreeId::from_raw(wire_id.raw.clone());
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
// Capability mailboxes
// ============================================================================

/// Mailbox identity and its undelivered keyed messages. A mailbox is a
/// capability source, not an alias for a currently-live subscription: sends
/// made before a receiver registers remain here until one matching receiver
/// consumes them. Possession of the minted `Int` is the whole capability;
/// this is never an address space the authored surface reasons about.
#[derive(Debug)]
struct MailboxTable {
    ids: MonotonicIdIssuer,
    live: HashSet<i64>,
    bound: usize,
    /// Global first-arrival order across mailboxes. A same-key replacement
    /// updates the payload in place, retaining that first position.
    pending: VecDeque<PendingMailboxMessage>,
    pending_slots: HashMap<(i64, String), usize>,
    /// Unique-key sends rejected after a mailbox's retained backlog reached
    /// `bound`. Kept until a receiver claims (and is poisoned by) that
    /// mailbox, or the capability is dropped.
    overflowed: HashMap<i64, i64>,
}

#[derive(Debug)]
struct PendingMailboxMessage {
    mailbox: i64,
    key: String,
    payload: serde_json::Value,
}

struct MailboxClaim {
    messages: Vec<PendingMailboxMessage>,
    dropped: i64,
}

impl MailboxTable {
    fn new(bound: usize) -> Self {
        Self {
            ids: MonotonicIdIssuer::new("mailbox"),
            live: HashSet::new(),
            bound,
            pending: VecDeque::new(),
            pending_slots: HashMap::new(),
            overflowed: HashMap::new(),
        }
    }

    fn mint(&mut self) -> i64 {
        let id = self.ids.next_raw() as i64;
        self.live.insert(id);
        id
    }

    fn is_live(&self, id: i64) -> bool {
        self.live.contains(&id)
    }

    /// `true` when `id` was live and is now dropped; `false` when it was
    /// never minted or already dropped — the caller turns that into a typed
    /// `EventUnknownMailbox`.
    fn drop_mailbox(&mut self, id: i64) -> bool {
        if !self.live.remove(&id) {
            return false;
        }
        self.pending.retain(|message| message.mailbox != id);
        self.overflowed.remove(&id);
        self.reindex_pending();
        true
    }

    fn retain(&mut self, mailbox: i64, key: String, payload: serde_json::Value) {
        let slot = (mailbox, key.clone());
        if let Some(&idx) = self.pending_slots.get(&slot) {
            self.pending[idx].payload = payload;
            return;
        }
        let retained_for_mailbox = self
            .pending
            .iter()
            .filter(|message| message.mailbox == mailbox)
            .count();
        if retained_for_mailbox >= self.bound {
            *self.overflowed.entry(mailbox).or_default() += 1;
            return;
        }
        let idx = self.pending.len();
        self.pending.push_back(PendingMailboxMessage {
            mailbox,
            key,
            payload,
        });
        self.pending_slots.insert(slot, idx);
    }

    /// Consume retained messages selected by this subscription. A mailbox is
    /// single-consumer while idle; once a receiver is live, later sends use
    /// Event's normal broadcast delivery to every live receiver.
    fn take_matching(&mut self, watches: &[EvWatch]) -> MailboxClaim {
        let claimed_mailboxes: HashSet<i64> = watches
            .iter()
            .filter_map(|watch| match watch {
                EvWatch::WatchMailbox(mailbox) => Some(*mailbox),
                _ => None,
            })
            .collect();
        let mut taken = Vec::new();
        let mut retained = VecDeque::new();
        while let Some(message) = self.pending.pop_front() {
            if claimed_mailboxes.contains(&message.mailbox) {
                taken.push(message);
            } else {
                retained.push_back(message);
            }
        }
        self.pending = retained;
        self.reindex_pending();
        let dropped = claimed_mailboxes
            .into_iter()
            .filter_map(|mailbox| self.overflowed.remove(&mailbox))
            .sum();
        MailboxClaim {
            messages: taken,
            dropped,
        }
    }

    fn reindex_pending(&mut self) {
        self.pending_slots.clear();
        for (idx, message) in self.pending.iter().enumerate() {
            self.pending_slots
                .insert((message.mailbox, message.key.clone()), idx);
        }
    }
}

// ============================================================================
// The handler
// ============================================================================

/// Serves `RepoEventSubscribe` / `RepoEventDrain` / `RepoEventUnsubscribe` /
/// `MailboxNew` / `MailboxSend` / `MailboxDrop`.
///
/// Not in `build_base_stack`'s row: this surface is opt-in, so a caller that
/// wants repository events builds a row containing this handler explicitly.
pub struct RepoEventHandler {
    registry: SubscriptionRegistry,
    source: Box<dyn ObservationSource>,
    poll_interval: Duration,
    sources: Vec<SourceState>,
    mailboxes: MailboxTable,
    active_owner: Option<u64>,
}

#[derive(Clone, Debug)]
enum SourceFailure {
    Lost(String),
    Failed(String),
}

impl SourceFailure {
    fn from_event_error(error: EventError) -> Self {
        match error {
            EventError::EventSourceLost(source) => Self::Lost(source),
            EventError::EventSourceFailed(detail) => Self::Failed(detail),
            other => Self::Failed(format!("observation source returned {other:?}")),
        }
    }

    fn to_event_error(&self) -> EventError {
        match self {
            Self::Lost(source) => EventError::EventSourceLost(source.clone()),
            Self::Failed(detail) => EventError::EventSourceFailed(detail.clone()),
        }
    }
}

#[derive(Debug)]
struct SourceState {
    worktree: WtWorktreeId,
    registered: bool,
    /// Cooldown starts only after a successful observation. A failure remains
    /// reportable to dependent subscriptions and, once reported, is retried on
    /// their next operation regardless of `poll_interval`.
    last_successful_pass: Option<Instant>,
    failure: Option<SourceFailure>,
    failure_reported: bool,
}

impl RepoEventHandler {
    /// The production wiring: reconcile through a real
    /// [`WorktreeMonitor`](tidepool_worktree::WorktreeMonitor).
    pub fn new(monitor: tidepool_worktree::WorktreeMonitor, config: EventConfig) -> Self {
        Self::with_source(Box::new(MonitorObservations::new(monitor)), config)
    }

    /// Same production wiring as [`Self::new`], plus the durable worktree
    /// registry a runtime-created worktree was recorded in — see
    /// [`MonitorObservations::with_registry`].
    pub fn with_registry(
        monitor: tidepool_worktree::WorktreeMonitor,
        registry: tidepool_worktree::WorktreeRegistry,
        config: EventConfig,
    ) -> Self {
        Self::with_source(
            Box::new(MonitorObservations::with_registry(monitor, registry)),
            config,
        )
    }

    /// Any other observation source. [`MonitorObservations`] is the real
    /// production adapter; the acceptance harness supplies a second source
    /// that reads a real temporary repository directly, scoped to that
    /// harness's single-loop-iteration/process-memory baseline — not because the
    /// production adapter doesn't exist.
    pub fn with_source(source: Box<dyn ObservationSource>, config: EventConfig) -> Self {
        Self {
            registry: SubscriptionRegistry::new(config.queue_bound),
            source,
            poll_interval: config.poll_interval,
            sources: Vec::new(),
            mailboxes: MailboxTable::new(config.queue_bound),
            active_owner: None,
        }
    }

    pub fn registry(&self) -> &SubscriptionRegistry {
        &self.registry
    }

    pub fn registry_mut(&mut self) -> &mut SubscriptionRegistry {
        &mut self.registry
    }

    /// Start one driver-owned lifecycle epoch. The driver closes exactly this
    /// owner on every cycle exit; authored happy-path unsubscribe remains an
    /// early release, not the only cleanup mechanism.
    pub fn begin_owner(&mut self, owner: u64) {
        assert!(self.active_owner.is_none(), "event owner already active");
        self.active_owner = Some(owner);
    }

    pub fn end_owner(&mut self, owner: u64) {
        if self.active_owner == Some(owner) {
            self.registry.close_owner(owner);
            self.active_owner = None;
        }
    }

    /// Run one independent reconciliation pass for every due source watched by
    /// the draining subscription and broadcast each success immediately.
    /// Cadence and typed failure state are per source, so one subscription's
    /// operation neither polls, cools down, nor fails an unrelated source.
    ///
    /// The rate limit exists to stop a body's effect storm from becoming a
    /// git-read storm — `withHandler` sends one drain before EVERY effect its
    /// body performs. It never loses anything: a pass reports movement relative
    /// to the observer's durable baseline, so a skipped pass is a delayed
    /// report, not a dropped one.
    fn reconcile(&mut self, subscription: EvSubscriptionId) -> Result<(), EventError> {
        // Deadlines cost no I/O, so they are checked on EVERY pass —
        // unconditionally, ahead of the git-read rate limit below, and even
        // when nothing is watched for commits/heads at all.
        self.registry.fire_due_deadlines(Instant::now());
        // Git I/O is scoped to the subscription whose drain/await caused this
        // pass. In particular, an A-only operation must not register, observe,
        // or retry an unrelated B merely because B is watched elsewhere.
        let watched = self.registry.subscription_worktrees(subscription)?;
        if watched.is_empty() {
            return Ok(());
        }
        for worktree in watched {
            let Some(index) = self
                .sources
                .iter()
                .position(|state| state.worktree == worktree)
            else {
                // Subscription registration establishes every source before
                // inserting the subscription, so this is an internal invariant.
                continue;
            };
            if self.sources[index].failure.is_some() {
                if !self.sources[index].failure_reported {
                    continue;
                }
            } else if self.sources[index]
                .last_successful_pass
                .is_some_and(|last| last.elapsed() < self.poll_interval)
            {
                continue;
            }
            if !self.sources[index].registered {
                match self.source.register(&worktree) {
                    Ok(()) => {
                        self.sources[index].registered = true;
                        self.sources[index].failure = None;
                        self.sources[index].failure_reported = false;
                    }
                    Err(error) => {
                        self.sources[index].failure = Some(SourceFailure::from_event_error(error));
                        self.sources[index].failure_reported = false;
                        continue;
                    }
                }
            }
            match self.source.observe(&worktree) {
                Ok(events) => {
                    self.sources[index].failure = None;
                    self.sources[index].failure_reported = false;
                    self.sources[index].last_successful_pass = Some(Instant::now());
                    for event in events {
                        self.registry.publish(&event);
                    }
                }
                Err(error) => {
                    self.sources[index].failure = Some(SourceFailure::from_event_error(error));
                    self.sources[index].failure_reported = false;
                }
            }
        }
        Ok(())
    }

    fn failure_for(
        &mut self,
        subscription: EvSubscriptionId,
    ) -> Result<Option<EventError>, EventError> {
        let worktrees = self.registry.subscription_worktrees(subscription)?;
        for worktree in worktrees {
            if let Some(state) = self
                .sources
                .iter_mut()
                .find(|state| state.worktree == worktree)
            {
                if let Some(failure) = state.failure.as_ref() {
                    let error = failure.to_event_error();
                    state.failure_reported = true;
                    return Ok(Some(error));
                }
            }
        }
        Ok(None)
    }

    // Errors-tagged verbs: total in `EventError`, no `cx` — the generated
    // dispatch arm wraps the `Result` via `cx.respond` (Ok→Right, Err→Left).
    // See #335 and `tidepool-handlers/CLAUDE.md`.

    pub(crate) fn repo_event_subscribe(
        &mut self,
        watches: Vec<EvWatch>,
    ) -> Result<EvSubscriptionId, EventError> {
        // Establish every never-before-seen source cutoff BEFORE making the
        // subscription live. This reads only the baseline; it neither
        // reconciles nor publishes journal rows, so historical rows never
        // replay while movement after this point remains observable.
        let mut worktrees = Vec::new();
        for watch in &watches {
            let worktree = match watch {
                EvWatch::WatchCommit(id) | EvWatch::WatchHead(id) => id,
                EvWatch::WatchDeadline(_) | EvWatch::WatchAsync(_) | EvWatch::WatchMailbox(_) => {
                    continue
                }
            };
            if !worktrees.contains(worktree) {
                worktrees.push(worktree.clone());
            }
        }
        for worktree in worktrees {
            if self.sources.iter().any(|state| state.worktree == worktree) {
                continue;
            }
            let registration = self.source.register(&worktree);
            self.sources.push(SourceState {
                worktree,
                registered: registration.is_ok(),
                last_successful_pass: None,
                failure: registration.err().map(SourceFailure::from_event_error),
                failure_reported: false,
            });
        }
        let sub = self
            .registry
            .subscribe_owned(watches.clone(), self.active_owner);
        let claim = self.mailboxes.take_matching(&watches);
        if claim.dropped > 0 {
            self.registry
                .poison_from_retained_mailbox(sub, claim.dropped);
        } else {
            for message in claim.messages {
                self.registry.queue_retained_mailbox(
                    sub,
                    message.mailbox,
                    message.key,
                    message.payload,
                );
            }
        }
        Ok(sub)
    }

    /// Subscribe after querying the driver's existing green-thread state.
    /// This is the level-triggered counterpart to transition broadcasting:
    /// terminal ids are supplied by the scheduler's authoritative thread
    /// table, never copied into this handler.
    pub fn repo_event_subscribe_with_terminal_async(
        &mut self,
        watches: Vec<EvWatch>,
        terminal_async: impl IntoIterator<Item = i64>,
    ) -> Result<EvSubscriptionId, EventError> {
        let sub = self.repo_event_subscribe(watches)?;
        for tid in terminal_async {
            self.registry.observe_terminal_async(sub, tid);
        }
        Ok(sub)
    }

    // `pub` (not `fn`, unlike this module's other tagged-verb methods):
    // the driver-side non-blocking parked-await servicing
    // calls this DIRECTLY, from `tidepool-harness`, as its own poll step —
    // never `repo_event_await`, whose internal sleep loop would stall the
    // whole green-thread scheduler. Identical to the `RepoEventDrain` verb
    // dispatch (same reconcile-then-drain, same bound/poison rule); this is
    // a visibility widening only, not a second implementation.
    pub fn repo_event_drain(
        &mut self,
        subscription: EvSubscriptionId,
    ) -> Result<Vec<EvRepositoryEvent>, EventError> {
        // A drain is where polling happens: `withHandler`'s interposition sends
        // one before every effect its body performs, so this is the natural —
        // and rate-limited — heartbeat.
        self.reconcile(subscription)?;
        if let Some(error) = self.failure_for(subscription)? {
            return Err(error);
        }
        self.registry.drain(subscription)
    }

    pub(crate) fn repo_event_unsubscribe(
        &mut self,
        subscription: EvSubscriptionId,
    ) -> Result<(), EventError> {
        self.registry.unsubscribe(subscription)
    }

    pub(crate) fn mailbox_new(&mut self) -> Result<i64, EventError> {
        Ok(self.mailboxes.mint())
    }

    pub(crate) fn mailbox_send(
        &mut self,
        mailbox: i64,
        key: String,
        payload: crate::effect_glue::JsonArg,
    ) -> Result<(), EventError> {
        if !self.mailboxes.is_live(mailbox) {
            return Err(EventError::EventUnknownMailbox(mailbox));
        }
        if self.registry.has_healthy_mailbox_receiver(mailbox) {
            self.registry
                .publish_mailbox_message(mailbox, &key, payload.0);
        } else {
            self.mailboxes.retain(mailbox, key, payload.0);
        }
        Ok(())
    }

    pub(crate) fn mailbox_drop(&mut self, mailbox: i64) -> Result<(), EventError> {
        if self.mailboxes.drop_mailbox(mailbox) {
            Ok(())
        } else {
            Err(EventError::EventUnknownMailbox(mailbox))
        }
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
    pub(crate) fn repo_event_await(
        &mut self,
        subscription: EvSubscriptionId,
        timeout_ms: i64,
    ) -> Result<Vec<EvRepositoryEvent>, EventError> {
        let deadline = if timeout_ms < 0 {
            // The no-deadline SENTINEL, not a bug to guard against: this is
            // `nextEvent`'s own calling convention (`awaitFirst` passes `-1`),
            // and it is the documented contract. Rejecting it would break the
            // one blocking coordination primitive.
            None
        } else {
            // Plain `+`: `Instant` is a `timespec` whose `tv_sec` is an
            // `i64`, and the largest `timeout_ms` an `i64` can carry is
            // ~9.2e15 ms ≈ 9.2e12 seconds — twelve orders of magnitude short
            // of overflowing it, so no input to this verb can overflow here.
            Some(Instant::now() + Duration::from_millis(timeout_ms as u64))
        };
        loop {
            self.reconcile(subscription)?;
            if let Some(error) = self.failure_for(subscription)? {
                return Err(error);
            }
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
                EvRepositoryEvent::ObservedAsyncDone(_, tid) => format!("async@{tid}"),
                EvRepositoryEvent::ObservedMessage(_, mid, v) => format!("msg@{mid}:{v}"),
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
            _worktree: &WtWorktreeId,
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

    fn handler_with_mailbox_bound(bound: usize) -> RepoEventHandler {
        RepoEventHandler::with_source(
            Box::new(ScriptedSource {
                passes: vec![],
                calls: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            }),
            EventConfig {
                queue_bound: bound,
                poll_interval: Duration::ZERO,
            },
        )
    }

    #[test]
    fn registration_does_not_reconcile_or_consume_backlog() {
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
            "registration establishes cutoffs but must not reconcile"
        );
        assert_eq!(
            oids(&h.repo_event_drain(sub).unwrap()),
            vec!["c1"],
            "movement from the gap with no subscriber must not be swallowed \
             by the act of registering"
        );
    }

    #[test]
    fn a_later_registration_does_not_replay_an_earlier_ones_facts() {
        // Re-registration is an ordinary repeated path (a new resident loop iteration
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

    struct IndependentSource {
        emitted_a: bool,
        b_calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }

    impl ObservationSource for IndependentSource {
        fn observe(
            &mut self,
            worktree: &WtWorktreeId,
        ) -> Result<Vec<EvRepositoryEvent>, EventError> {
            match worktree.raw.as_str() {
                "a" if !self.emitted_a => {
                    self.emitted_a = true;
                    Ok(vec![commit_event(1, "a", "a1")])
                }
                "a" => Ok(Vec::new()),
                "b" => {
                    self.b_calls
                        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    Err(EventError::EventSourceLost("b".into()))
                }
                other => panic!("unexpected source {other}"),
            }
        }
    }

    #[test]
    fn a_success_is_delivered_when_b_fails_and_only_b_subscribers_fail() {
        let b_calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut h = RepoEventHandler::with_source(
            Box::new(IndependentSource {
                emitted_a: false,
                b_calls: b_calls.clone(),
            }),
            EventConfig {
                queue_bound: 8,
                poll_interval: Duration::ZERO,
            },
        );
        let a = h
            .repo_event_subscribe(vec![EvWatch::WatchCommit(wt("a"))])
            .unwrap();
        let b = h
            .repo_event_subscribe(vec![EvWatch::WatchCommit(wt("b"))])
            .unwrap();

        assert_eq!(oids(&h.repo_event_drain(a).unwrap()), vec!["a1"]);
        assert_eq!(
            h.repo_event_drain(b),
            Err(EventError::EventSourceLost("b".into()))
        );
        assert_eq!(b_calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(
            h.repo_event_drain(a).unwrap(),
            Vec::<EvRepositoryEvent>::new(),
            "B's persistent failure does not poison A"
        );
        assert_eq!(
            b_calls.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "an A-only drain must not retry B after B's failure was reported"
        );
    }

    struct NewSourceDuringCooldown {
        b_moved: std::sync::Arc<std::sync::atomic::AtomicBool>,
        registered: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
        observed: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
        emitted_b: bool,
    }

    impl ObservationSource for NewSourceDuringCooldown {
        fn register(&mut self, worktree: &WtWorktreeId) -> Result<(), EventError> {
            self.registered.lock().unwrap().push(worktree.raw.clone());
            Ok(())
        }

        fn observe(
            &mut self,
            worktree: &WtWorktreeId,
        ) -> Result<Vec<EvRepositoryEvent>, EventError> {
            self.observed.lock().unwrap().push(worktree.raw.clone());
            if worktree.raw == "b"
                && self.b_moved.load(std::sync::atomic::Ordering::SeqCst)
                && !self.emitted_b
            {
                self.emitted_b = true;
                return Ok(vec![commit_event(2, "b", "b1")]);
            }
            Ok(Vec::new())
        }
    }

    #[test]
    fn a_new_b_source_does_not_inherit_a_source_cooldown() {
        let b_moved = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let registered = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let observed = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut h = RepoEventHandler::with_source(
            Box::new(NewSourceDuringCooldown {
                b_moved: b_moved.clone(),
                registered: registered.clone(),
                observed: observed.clone(),
                emitted_b: false,
            }),
            EventConfig {
                queue_bound: 8,
                poll_interval: Duration::from_secs(3600),
            },
        );
        let a = h
            .repo_event_subscribe(vec![EvWatch::WatchCommit(wt("a"))])
            .unwrap();
        h.repo_event_drain(a).unwrap();

        let b = h
            .repo_event_subscribe(vec![EvWatch::WatchCommit(wt("b"))])
            .unwrap();
        assert_eq!(
            registered.lock().unwrap().as_slice(),
            &["a".to_string(), "b".to_string()],
            "B's cutoff is established during subscription"
        );
        b_moved.store(true, std::sync::atomic::Ordering::SeqCst);
        assert_eq!(oids(&h.repo_event_drain(b).unwrap()), vec!["b1"]);
        assert_eq!(
            observed.lock().unwrap().as_slice(),
            &["a".to_string(), "b".to_string()],
            "A stays cooled down while never-polled B runs immediately"
        );
    }

    enum RecoveryStage {
        Registering { calls: usize },
        Observing { calls: usize },
    }

    struct RecoveringSource {
        stage: RecoveryStage,
    }

    impl ObservationSource for RecoveringSource {
        fn register(&mut self, worktree: &WtWorktreeId) -> Result<(), EventError> {
            if let RecoveryStage::Registering { calls } = &mut self.stage {
                *calls += 1;
                if *calls == 1 {
                    return Err(EventError::EventSourceLost(worktree.raw.clone()));
                }
            }
            Ok(())
        }

        fn observe(
            &mut self,
            worktree: &WtWorktreeId,
        ) -> Result<Vec<EvRepositoryEvent>, EventError> {
            if let RecoveryStage::Observing { calls } = &mut self.stage {
                *calls += 1;
                if *calls == 1 {
                    return Err(EventError::EventSourceFailed(
                        "temporarily unavailable".into(),
                    ));
                }
            }
            Ok(vec![commit_event(3, &worktree.raw, "recovered")])
        }
    }

    fn recovering_handler(stage: RecoveryStage) -> RepoEventHandler {
        RepoEventHandler::with_source(
            Box::new(RecoveringSource { stage }),
            EventConfig {
                queue_bound: 8,
                poll_interval: Duration::from_secs(3600),
            },
        )
    }

    #[test]
    fn failed_sources_report_once_then_retry_on_the_next_dependent_operation() {
        let mut registration = recovering_handler(RecoveryStage::Registering { calls: 0 });
        let reg_sub = registration
            .repo_event_subscribe(vec![EvWatch::WatchCommit(wt("registration"))])
            .unwrap();
        assert_eq!(
            registration.repo_event_drain(reg_sub),
            Err(EventError::EventSourceLost("registration".into()))
        );
        assert_eq!(
            oids(&registration.repo_event_drain(reg_sub).unwrap()),
            vec!["recovered"],
            "a registration failure retries immediately after being reported"
        );

        let mut observation = recovering_handler(RecoveryStage::Observing { calls: 0 });
        let obs_sub = observation
            .repo_event_subscribe(vec![EvWatch::WatchCommit(wt("observation"))])
            .unwrap();
        assert_eq!(
            observation.repo_event_drain(obs_sub),
            Err(EventError::EventSourceFailed(
                "temporarily unavailable".into()
            ))
        );
        assert_eq!(
            oids(&observation.repo_event_drain(obs_sub).unwrap()),
            vec!["recovered"],
            "an observation failure is not held behind the successful-poll cooldown"
        );
    }

    // ── await / deadlines ──

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

    // ── capability mailboxes ──

    fn payload(v: serde_json::Value) -> crate::effect_glue::JsonArg {
        crate::effect_glue::JsonArg(v)
    }

    fn mailbox_payloads(evs: &[EvRepositoryEvent]) -> Vec<serde_json::Value> {
        evs.iter()
            .map(|e| match e {
                EvRepositoryEvent::ObservedMessage(_, _, v) => v.clone(),
                other => panic!("expected ObservedMessage, got {other:?}"),
            })
            .collect()
    }

    #[test]
    fn a_send_is_observed_by_a_watchmailbox_subscriber() {
        let (mut h, _calls) = handler_with(vec![], Duration::ZERO);
        let mid = h.mailbox_new().unwrap();
        let sub = h
            .repo_event_subscribe(vec![EvWatch::WatchMailbox(mid)])
            .unwrap();
        h.mailbox_send(mid, "k".into(), payload(serde_json::json!("hello")))
            .unwrap();
        let batch = h.repo_event_drain(sub).unwrap();
        assert_eq!(mailbox_payloads(&batch), vec![serde_json::json!("hello")]);
    }

    #[test]
    fn a_subscriber_to_a_different_mailbox_is_not_observed() {
        let (mut h, _calls) = handler_with(vec![], Duration::ZERO);
        let a = h.mailbox_new().unwrap();
        let b = h.mailbox_new().unwrap();
        let sub_b = h
            .repo_event_subscribe(vec![EvWatch::WatchMailbox(b)])
            .unwrap();
        h.mailbox_send(a, "k".into(), payload(serde_json::json!(1)))
            .unwrap();
        assert_eq!(h.repo_event_drain(sub_b).unwrap(), vec![]);
    }

    #[test]
    fn a_burst_of_same_key_sends_is_observed_once_carrying_the_last_payload() {
        let (mut h, _calls) = handler_with(vec![], Duration::ZERO);
        let mid = h.mailbox_new().unwrap();
        let sub = h
            .repo_event_subscribe(vec![EvWatch::WatchMailbox(mid)])
            .unwrap();
        h.mailbox_send(mid, "k".into(), payload(serde_json::json!(1)))
            .unwrap();
        h.mailbox_send(mid, "k".into(), payload(serde_json::json!(2)))
            .unwrap();
        h.mailbox_send(mid, "k".into(), payload(serde_json::json!(3)))
            .unwrap();
        let batch = h.repo_event_drain(sub).unwrap();
        assert_eq!(mailbox_payloads(&batch), vec![serde_json::json!(3)]);
    }

    #[test]
    fn retained_mailbox_burst_is_consumed_by_a_later_receiver() {
        let (mut h, _calls) = handler_with(vec![], Duration::ZERO);
        let mid = h.mailbox_new().unwrap();
        h.mailbox_send(mid, "k".into(), payload(serde_json::json!(1)))
            .unwrap();
        h.mailbox_send(mid, "k".into(), payload(serde_json::json!(2)))
            .unwrap();
        h.mailbox_send(mid, "k".into(), payload(serde_json::json!(3)))
            .unwrap();

        let first = h
            .repo_event_subscribe(vec![EvWatch::WatchMailbox(mid)])
            .unwrap();
        assert_eq!(
            mailbox_payloads(&h.repo_event_drain(first).unwrap()),
            vec![serde_json::json!(3)]
        );

        // A mailbox backlog is a one-receiver handoff, not a durable
        // broadcast log. Once it is handed off, a later subscriber cannot
        // replay it; later sends still broadcast to all live receivers.
        let second = h
            .repo_event_subscribe(vec![EvWatch::WatchMailbox(mid)])
            .unwrap();
        assert_eq!(h.repo_event_drain(second).unwrap(), vec![]);
        h.mailbox_send(mid, "next".into(), payload(serde_json::json!(4)))
            .unwrap();
        assert_eq!(
            mailbox_payloads(&h.repo_event_drain(first).unwrap()),
            vec![serde_json::json!(4)]
        );
        assert_eq!(
            mailbox_payloads(&h.repo_event_drain(second).unwrap()),
            vec![serde_json::json!(4)]
        );
    }

    #[test]
    fn retained_mailbox_keys_keep_first_arrival_order() {
        let (mut h, _calls) = handler_with(vec![], Duration::ZERO);
        let mid = h.mailbox_new().unwrap();
        h.mailbox_send(mid, "a".into(), payload(serde_json::json!("a1")))
            .unwrap();
        h.mailbox_send(mid, "b".into(), payload(serde_json::json!("b1")))
            .unwrap();
        h.mailbox_send(mid, "a".into(), payload(serde_json::json!("a2")))
            .unwrap();
        let sub = h
            .repo_event_subscribe(vec![EvWatch::WatchMailbox(mid)])
            .unwrap();
        assert_eq!(
            mailbox_payloads(&h.repo_event_drain(sub).unwrap()),
            vec![serde_json::json!("a2"), serde_json::json!("b1")]
        );
    }

    #[test]
    fn unique_retained_messages_over_the_bound_poison_the_first_claimant() {
        let mut h = handler_with_mailbox_bound(2);
        let mid = h.mailbox_new().unwrap();
        for key in ["a", "b", "c"] {
            h.mailbox_send(mid, key.into(), payload(serde_json::json!(key)))
                .unwrap();
        }
        let sub = h
            .repo_event_subscribe(vec![EvWatch::WatchMailbox(mid)])
            .unwrap();
        assert_eq!(
            h.repo_event_drain(sub),
            Err(EventError::EventQueueOverflow(sub.raw, 1)),
            "the third unique retained key is named loss, never silently omitted"
        );
    }

    #[test]
    fn retained_same_key_coalesces_within_the_mailbox_bound() {
        let mut h = handler_with_mailbox_bound(2);
        let mid = h.mailbox_new().unwrap();
        h.mailbox_send(mid, "a".into(), payload(serde_json::json!(1)))
            .unwrap();
        h.mailbox_send(mid, "b".into(), payload(serde_json::json!(2)))
            .unwrap();
        h.mailbox_send(mid, "a".into(), payload(serde_json::json!(3)))
            .unwrap();
        let sub = h
            .repo_event_subscribe(vec![EvWatch::WatchMailbox(mid)])
            .unwrap();
        assert_eq!(
            mailbox_payloads(&h.repo_event_drain(sub).unwrap()),
            vec![serde_json::json!(3), serde_json::json!(2)]
        );
    }

    #[test]
    fn dropping_a_mailbox_releases_its_retained_backlog_and_overflow_accounting() {
        let mut h = handler_with_mailbox_bound(1);
        let mid = h.mailbox_new().unwrap();
        h.mailbox_send(mid, "a".into(), payload(serde_json::json!(1)))
            .unwrap();
        h.mailbox_send(mid, "b".into(), payload(serde_json::json!(2)))
            .unwrap();
        h.mailbox_drop(mid).unwrap();
        assert!(h.mailboxes.pending.is_empty());
        assert!(!h.mailboxes.overflowed.contains_key(&mid));
        assert_eq!(
            h.mailbox_send(mid, "c".into(), payload(serde_json::json!(3))),
            Err(EventError::EventUnknownMailbox(mid))
        );
    }

    #[test]
    fn terminal_async_is_observed_when_subscription_starts_after_settlement() {
        let (mut h, _calls) = handler_with(vec![], Duration::ZERO);
        let sub = h
            .repo_event_subscribe_with_terminal_async(vec![EvWatch::WatchAsync(7)], [7])
            .unwrap();
        assert!(matches!(
            h.repo_event_drain(sub).unwrap().as_slice(),
            [EvRepositoryEvent::ObservedAsyncDone(_, 7)]
        ));
    }

    #[test]
    fn closing_an_owner_removes_its_abandoned_subscriptions_only() {
        let (mut h, _calls) = handler_with(vec![], Duration::ZERO);
        h.begin_owner(10);
        let abandoned = h
            .repo_event_subscribe(vec![EvWatch::WatchMailbox(1)])
            .unwrap();
        h.end_owner(10);
        assert_eq!(
            h.repo_event_drain(abandoned),
            Err(EventError::EventUnknownSubscription(abandoned.raw))
        );
        let unowned = h
            .repo_event_subscribe(vec![EvWatch::WatchMailbox(1)])
            .unwrap();
        assert!(h.repo_event_drain(unowned).is_ok());
    }

    #[test]
    fn different_keys_do_not_coalesce_with_each_other() {
        let (mut h, _calls) = handler_with(vec![], Duration::ZERO);
        let mid = h.mailbox_new().unwrap();
        let sub = h
            .repo_event_subscribe(vec![EvWatch::WatchMailbox(mid)])
            .unwrap();
        h.mailbox_send(mid, "a".into(), payload(serde_json::json!("a1")))
            .unwrap();
        h.mailbox_send(mid, "b".into(), payload(serde_json::json!("b1")))
            .unwrap();
        let batch = h.repo_event_drain(sub).unwrap();
        assert_eq!(
            mailbox_payloads(&batch),
            vec![serde_json::json!("a1"), serde_json::json!("b1")]
        );
    }

    #[test]
    fn a_coalesced_replacement_keeps_the_earlier_arrival_position() {
        let (mut h, _calls) = handler_with(vec![], Duration::ZERO);
        let mid = h.mailbox_new().unwrap();
        let sub = h
            .repo_event_subscribe(vec![EvWatch::WatchMailbox(mid)])
            .unwrap();
        h.mailbox_send(mid, "a".into(), payload(serde_json::json!("a1")))
            .unwrap();
        h.mailbox_send(mid, "b".into(), payload(serde_json::json!("b1")))
            .unwrap();
        // Replaces "a"'s entry in place — "a" must stay FIRST, not move to
        // the back behind "b".
        h.mailbox_send(mid, "a".into(), payload(serde_json::json!("a2")))
            .unwrap();
        let batch = h.repo_event_drain(sub).unwrap();
        assert_eq!(
            mailbox_payloads(&batch),
            vec![serde_json::json!("a2"), serde_json::json!("b1")]
        );
    }

    /// The pin that matters here: a NEGATIVE
    /// timeout is the documented no-deadline sentinel and `nextEvent`'s own
    /// calling convention (`awaitFirst` passes `-1`). It must never join the
    /// rejection above — doing so would break the one blocking coordination
    /// primitive. Driven with a match already queued so it returns at once
    /// instead of blocking this test forever, which is exactly the behaviour
    /// under test: wait with no deadline, return on the first match.
    #[test]
    fn a_negative_timeout_is_the_no_deadline_sentinel_not_an_error() {
        let (mut h, _calls) = handler_with(vec![], Duration::ZERO);
        let mid = h.mailbox_new().unwrap();
        let sub = h
            .repo_event_subscribe(vec![EvWatch::WatchMailbox(mid)])
            .unwrap();
        h.mailbox_send(mid, "k".into(), payload(serde_json::json!("queued")))
            .unwrap();
        let batch = h
            .repo_event_await(sub, -1)
            .expect("a negative timeout is accepted, never a validation error");
        assert_eq!(mailbox_payloads(&batch), vec![serde_json::json!("queued")]);
    }

    #[test]
    fn send_to_a_dropped_mailbox_is_a_typed_error() {
        let (mut h, _calls) = handler_with(vec![], Duration::ZERO);
        let mid = h.mailbox_new().unwrap();
        h.mailbox_drop(mid).unwrap();
        assert_eq!(
            h.mailbox_send(mid, "k".into(), payload(serde_json::json!(1))),
            Err(EventError::EventUnknownMailbox(mid))
        );
    }

    #[test]
    fn send_to_a_never_minted_mailbox_is_a_typed_error() {
        let (mut h, _calls) = handler_with(vec![], Duration::ZERO);
        assert_eq!(
            h.mailbox_send(999, "k".into(), payload(serde_json::json!(1))),
            Err(EventError::EventUnknownMailbox(999))
        );
    }

    #[test]
    fn a_mailbox_watch_is_skipped_by_worktree_reconciliation() {
        let mut reg = SubscriptionRegistry::new(8);
        reg.subscribe(vec![
            EvWatch::WatchMailbox(1),
            EvWatch::WatchCommit(wt("a")),
        ]);
        assert_eq!(
            reg.watched_worktrees(),
            vec![wt("a")],
            "WatchMailbox names no worktree, same as WatchDeadline"
        );
    }

    // ── lazy registration from the durable worktree registry ──

    fn registry_and_monitor_over_temp_dirs() -> (
        tidepool_worktree::WorktreeRegistry,
        tidepool_worktree::WorktreeMonitor,
        tempfile::TempDir,
        tempfile::TempDir,
    ) {
        let registry_dir = tempfile::tempdir().unwrap();
        let registry = tidepool_worktree::WorktreeRegistry::open(registry_dir.path()).unwrap();
        let journal_dir = tempfile::tempdir().unwrap();
        let journal =
            tidepool_worktree::EventJournal::open(journal_dir.path().join("events.jsonl")).unwrap();
        let monitor =
            tidepool_worktree::WorktreeMonitor::new(tidepool_worktree::GitCli::new(), journal);
        (registry, monitor, registry_dir, journal_dir)
    }

    #[test]
    fn subscription_registers_a_registry_recorded_source_before_later_movement() {
        // Reproduces the live dev-tree failure: a worktree the Worktree effect
        // created and durably registered, watched by RepoEvent, with NO
        // `MonitorObservations::register` call anywhere in this test — the
        // monitor must resolve the baseline lazily from the registry instead
        // of failing the whole turn with `WorktreeNotRegistered`.
        let repo = tidepool_worktree::testing::TestRepo::init().unwrap();
        repo.writer().commit_file("a.txt", "hello", "init").unwrap();
        let cwd = repo.path().to_path_buf();

        let (registry, monitor, _registry_dir, _journal_dir) =
            registry_and_monitor_over_temp_dirs();
        let worktree_id = tidepool_worktree::WorktreeId::from_raw("wt-lazy");
        registry
            .put(&tidepool_worktree::WorktreeReceipt {
                worktree_id: worktree_id.clone(),
                cwd: cwd.clone(),
                branch: tidepool_worktree::BranchName::from_raw("main"),
                source_head: tidepool_worktree::GitOid::from_raw("deadbeef"),
                snapshot_ref: None,
                origin: tidepool_worktree::WorktreeOrigin::CurrentRepository,
                source_repository: cwd,
                created_at_ms: 0,
                status: tidepool_worktree::WorktreeRecordStatus::Finalized,
            })
            .unwrap();

        let mut h = RepoEventHandler::with_registry(
            monitor,
            registry,
            EventConfig {
                queue_bound: 8,
                poll_interval: Duration::ZERO,
            },
        );
        let wire_id = WtWorktreeId {
            raw: worktree_id.as_str().to_string(),
        };
        let sub = h
            .repo_event_subscribe(vec![EvWatch::WatchCommit(wire_id)])
            .unwrap();
        let moved = repo
            .writer()
            .commit_file("a.txt", "later", "after subscription")
            .unwrap();
        assert_eq!(
            oids(&h.repo_event_drain(sub).unwrap()),
            vec![moved.as_str()],
            "subscription-time registration fixes the cutoff before later movement"
        );
    }

    #[test]
    fn an_id_unknown_to_both_the_monitor_and_the_registry_still_fails_typed() {
        let (registry, monitor, _registry_dir, _journal_dir) =
            registry_and_monitor_over_temp_dirs();
        let mut h = RepoEventHandler::with_registry(monitor, registry, EventConfig::default());

        let sub = h
            .repo_event_subscribe(vec![EvWatch::WatchCommit(wt("wt-does-not-exist"))])
            .unwrap();
        assert!(
            matches!(
                h.repo_event_drain(sub),
                Err(EventError::EventSourceFailed(_))
            ),
            "an id the registry also does not know must still fail the \
             existing typed path, unchanged"
        );
    }
}
