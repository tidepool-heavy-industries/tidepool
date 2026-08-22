# Session-ownership capstone: one registry, one suspension truth

Status: in flight (2026-08-22). Consolidates dup-c-survey items 1 and 5
(`/tmp/claude-1000/.../dup-c-result.md`, ranks 1 and 5 — the survey requires
they be designed together). Verified against HEAD as of this branch's start;
line numbers below are HEAD references, not survey references, where they
differ.

## Item 1 — one ownership registry

### What exists today

- `tidepool-harness::registry::SessionRegistry<M>` (`registry.rs`) +
  `tidepool-harness::tree::Slot<M>` (`tree.rs:98`): `HashMap<SessionId,
  Slot<M>>` behind one `parking_lot::Mutex`, `Idle(M) | Running{holes} |
  Suspended{machine,holes}` (MULTI-HOLE — a session can carry a SET of parked
  holes, e.g. several attached answerer realms suspended concurrently on one
  shared machine). `checkout_run`/`checkout_resume`/`checkout_child` move the
  machine out under a short lock; the RAII `Checkout` (`#[must_use]`,
  panic-safety `Drop`) restores it under a second short lock, keyed strictly
  by `SessionId`. No epoch/replacement guard — see hazard below.
- `tidepool-repl::manager::SessionManager` (`manager.rs`): the same
  ownership discipline at N=1 (`Mutex<Option<SessionEntry>>`, no key), PLUS
  an `epoch: u64` per entry and a `CheckoutCustody` linear token carrying it
  — `session_reset` can replace the entry while a turn is still checked out,
  and a stale settle (`restore_idle`/`restore_suspended`/`retire`) against
  the OLD epoch drops its session instead of clobbering the fresh one.
- `tidepool-repl::state::SessionState` (`state.rs`): a SECOND, independently
  transitioned lifecycle enum (`Idle|Busy|Suspended(Box<Suspension>)|
  Wedged{since}|Closing`), stored as `SharedState` beside the manager's slot
  in `SessionEntry`. `server.rs` co-transitions the two by hand at ~20 sites
  (grep `SessionState::` in `server.rs`).

### The hazard the two designs solve differently, and why one wins

Two independent problems, only one of which the current harness registry
actually closes:

1. **Stale-checkout-after-replace (ABA-shaped).** A turn holds a machine out
   on the blocking pool; something replaces or removes the entry under it
   before it restores. REPL's epoch closes this (`a_stale_turn_cannot_
   clobber_a_session_installed_after_a_reset`, `manager.rs` tests). The
   harness registry does NOT have this guard — `restore_suspended` does an
   unconditional `self.slots.lock().insert(id, slot)`. Because harness
   `SessionId`s are minted from a monotonic counter and never reused, the
   classic ABA (id reused for a genuinely different session) cannot happen —
   but **remove-while-checked-out still can**: `Harness::terminate_node`
   calls `registry().remove(sid)` for an owning node; if a `Checkout` for
   that same `sid` is still outstanding elsewhere (e.g. a concurrent
   cancel racing an in-flight turn), the removed placeholder is silently
   RECREATED by the stale checkout's eventual `restore_suspended`,
   resurrecting a session the caller believed was gone. Latent, not
   currently known to be hit by a real caller, but real. The promoted
   registry closes it for both consumers with the same mechanism REPL
   already proved: an epoch per entry, carried by the `Checkout`, checked at
   restore.
2. **Caller-facing terminal states with no slot representation
   (`Wedged`/`Closing`).** REPL's second SessionState enum exists mostly to
   answer "why is there nothing here" after a session was torn down — the
   registry slot approach as it stands can only say "absent", losing the
   TTL-reapable "wedged" reason and turning `session_reset`/reaper logic
   into an external, manually-synchronized second truth.

### The winning design

Promote the harness's `SessionRegistry`/`Slot`/`Checkout` machinery into
`tidepool_runtime::session::registry` (new module, `pub use`d from
`session::mod`, following `supervisor.rs`'s marker-module precedent), with
three changes on the way in:

1. **Generic over the hole-identity type too**: `Slot<M, H>`,
   `SessionRegistry<M, H>`, `Checkout<M, H>`, `CheckoutError<H>`, bound
   `H: Clone + PartialEq + std::fmt::Debug`. Harness instantiates `H =
   tidepool_harness::tree::HoleId`; REPL instantiates `H =
   tidepool_repl::state::ContinuationId` (both newtypes stay where they are
   — unifying them is a SEPARATE, not-yet-proposed duplicate and out of
   this lane's scope; the registry doesn't care what a hole id looks like,
   only that it's comparable).
2. **Epoch-guarded entries.** Each map entry carries a monotonic epoch
   (minted by the registry, one shared `AtomicU64` per `SessionRegistry`
   instance); `Checkout` carries the epoch it read. `restore_suspended`/the
   panic-safety `Drop` check the CURRENT entry's epoch before writing — a
   mismatch (entry removed, or replaced under a reused key) drops the
   machine instead of resurrecting/clobbering. This closes hazard 1 for
   harness for free and IS hazard-1's REPL fix, now shared.
3. **Two additional terminal `Slot` variants, `Wedged{since: Instant}` and
   *no* `Closing`.** `Wedged` is genuinely load-bearing (TTL-reaped,
   observably tested — `tidepool-repl/tests/lifecycle_state.rs`,
   `server.rs`'s `reaper_removes_a_wedged_entry_only_once_past_its_ttl` /
   `reset_reclaims_a_wedged_entry`), so it earns a real slot variant:
   `checkout_run`/`checkout_resume`/`checkout_child` refuse it with a new
   `CheckoutError::Terminal{session, label}`, and a dedicated
   `Checkout::mark_wedged(self, since: Instant)` settlement (a fourth
   sibling of `restore_suspended`, consuming the checkout without a machine
   to hand back — the JoinError/timeout-without-recovery path never has one
   to restore anyway) writes it in place of a bare `remove()`.
   `Closing` is NOT promoted: auditing its only two writers
   (`teardown_current`, `reap_once`'s wedge-sweep branch) shows it exists
   solely to give a concurrent reader a label during the synchronous
   sliver between "decide to remove" and "remove" — once both live under
   ONE lock (this promotion's whole point), that sliver disappears and
   `remove()` is atomic; nothing observes `Closing` in a test. Recorded here
   so nobody re-derives the option and re-adds it.
   Harness never constructs `Wedged` (its equivalent is `terminate_node` +
   `NodeState::Cancelled`, a different, node-scoped mechanism it keeps) —
   the variant is inert dead weight for that consumer, matched by an
   `unreachable!()`-free catch-all in its match arms (a `Wedged` slot
   reaching a keyed harness checkout call is impossible by construction:
   nothing in that crate ever writes one).

   `Slot::label()` gives both consumers one shared human string
   (`"idle"`/`"running"`/`"suspended (continuation …)"`/`"wedged (a turn
   timed out)"`) — REPL's `SessionState::busy_label` becomes a thin call
   into it, closing a small extra duplication found while doing this move.

4. **Facades.** The keyed API (`SessionRegistry<M,H>::checkout_run(id)` etc.,
   `Result<Checkout, CheckoutError<H>>`) is the primitive; harness's
   `tree.rs`/`registry.rs` becomes a thin re-export (`pub use
   tidepool_runtime::session::registry::*` plus the harness's own
   `Slot`-derived `NodeState` glue) rather than a parallel implementation —
   whether anything harness-specific survives in `tidepool-harness::registry`
   at all is decided during implementation; if nothing does, the module is
   deleted and callers import the runtime path directly. A
   `SingleSlot<M,H>` wrapper (new, in the same runtime module) adapts the
   keyed primitive to REPL's "at most one entry, no id parameter" shape:
   it owns one `SessionRegistry<M,H>` plus a `Mutex<Option<SessionId>>`
   tracking which key is current, and re-exposes `install`/`checkout_run()`/
   `checkout_resume(&H)`/`checkout_child()`/`remove()`/`state_label()`
   without an id argument. REPL's `manager.rs` shrinks to policy glue
   (cancel slot, bindings slot, tool-facing error text) over `SingleSlot`;
   its own `Checkout`/`CheckoutCustody` types are deleted in favor of the
   promoted ones.

5. **`tidepool-repl/src/state.rs` is deleted.** `SessionState` and
   `Suspension` go away as an independently-transitioned type.
   Caller-facing labels come from `Slot::label()`; the suspension payload
   REPL's ask/resume path needs (`cont_id`, `captured` output, expected
   schema, TTL clock) moves to living INSIDE `Slot::Suspended` as REPL-side
   metadata carried alongside the holes vec (mirrors the harness's own
   per-hole metadata decision in item 5, applied to REPL's single-hole
   case) — concretely `Slot::Suspended<M, H> { machine: M, holes: Vec<H>,
   meta: HashMap<H, SuspMeta> }` where `SuspMeta` is a small
   consumer-supplied associated payload (`()` for harness, which tracks its
   own richer metadata in the item-5 map instead; REPL's
   `{captured, expected_schema, since}` for its one hole). This is the one
   point where the "keep it generic" and "REPL needs payload" pulls meet —
   resolved by making the metadata slot itself generic (`type Meta`) rather
   than either hardcoding REPL's shape into the shared primitive or
   reintroducing REPL's second map outside it.
   `server.rs`'s ~20 `SessionState::` writes collapse to registry
   calls + `Slot::label()` reads; `dispatch_tool`'s busy-guard becomes one
   registry-level "is this session idle" check instead of a separate
   `SessionState` peek before the checkout attempt — closing the exact
   two-lock hazard the survey names at `checkout_resume` (`server.rs:816`
   today): state consumed under one lock, checkout validated under a
   second, separately.

## Item 5 — one suspension-metadata truth in the harness

### What exists today

Three representations of "is this suspended, and on what":

- `SessionRegistry`'s `Slot::Suspended{holes}` — machine-reported, multi-hole,
  authoritative for CHECKOUT purposes (which holes exist, in what order).
- `tree::NodeState::Suspended{hole: HoleId}` — single-hole, per NODE,
  written by `NodeTree::hole_published`/`hole_consumed`
  (`forcing.rs:322,381`).
- `harness::NodeConvo`'s four fields (`harness.rs:181`): `pending:
  Option<PendingHole>` (classified hole + raw request), `resident_hole:
  Option<ResidentHole>` (the typed continuation token `resume_parent`
  drives), `suspend_table: Option<DataConTable>` + `suspend_asks:
  AsksSidecar` (the compile artifacts a bridged answer/re-classify needs).
  ~30 read/write sites across `harness.rs` (`finish_run`, `resume_parent`,
  `pending_hole*`, `take_finalized_*`, `finalize_is_closure`, more).

### Why NodeState stays single-hole, and why the map doesn't

A node's OWN turn can only ever be suspended on the resident session's
CURRENT continuation for that node's own realm — `resident_hole` is always
REPLACED, never accumulated, across a suspend → resume → re-suspend cycle
(`resume_parent`, `harness.rs:3963-4025`). The registry's multi-hole SET is
multi because it spans MULTIPLE NODES sharing one session (the one-session
collapse's concurrently-driven attached answerer realms), not because one
node juggles several holes at once. Forcing `NodeState` to carry a `Vec` to
"match" the registry would manufacture a distinction with no node-level
referent and touch the tree's durable-log-adjacent, HTTP-served wire shape
for no behavioral gain — so `NodeState::Suspended{hole: HoleId}` is UNCHANGED
(this is the "resolve toward multi-hole" call: the STORE becomes
multi-hole-capable, matching the registry's own idiom of a multi-hole
backing store with a single-newest convenience view — see
`registry.rs`'s `pending_hole`/`pending_holes` test helpers, which already
established exactly this pattern); the per-node PROJECTION stays single
because that reflects the real invariant, not an arbitrary simplification.

### The winning design

One map on `Harness`, replacing `NodeConvo`'s four fields:

```rust
struct PendingHole {           // extends the existing type at harness.rs:287
    node: NodeId,               // NEW — the owner, for node-scoped derivation
    hole: HoleId,
    classified: ClassifiedHole,
    raw_request: Value,
    resident_hole: ResidentHole, // NEW — was NodeConvo::resident_hole: Option<_>
    suspend_table: DataConTable, // NEW — was NodeConvo::suspend_table: Option<_>
    suspend_asks: AsksSidecar,   // NEW — was NodeConvo::suspend_asks
}

// on Harness:
pending_holes: Mutex<HashMap<(SessionId, HoleId), PendingHole>>,
```

Keyed by `(SessionId, HoleId)`, not bare `HoleId`: the JIT's `scont_N`
continuation ids are minted per-machine, so two independent sessions can
legitimately produce the same string — a flat global map would silently
collide. `SessionId` is already resolved at every existing call site
(`tree.session_of(node)`), so this costs nothing extra.

Node-scoped reads (`pending_hole`, `pending_hole_full`,
`pending_turn_outcome`, `finalize_is_closure`, …) become one linear scan —
`pending_holes.lock().values().find(|p| p.node == node)` — never a second
index to keep in sync: node/session counts in flight are small (bounded by
concurrent fanout width, not corpus size), so O(n) beats a second choreographed
map. `PendingHole` derives `Clone` so a read can drop the lock immediately.

**Log transitions from map mutations, not the other way around.** Today
`finish_run`/`resume_parent`'s re-suspend arm each hand-sequence: write
`suspend_table`/`suspend_asks` → `tree.hole_published` → `set_pending` →
write `resident_hole` (4 separate mutations, duplicated verbatim in two
methods). This collapses to one helper:

```rust
fn publish_hole(&self, node: NodeId, sid: SessionId, pending: PendingHole) -> Result<(), HarnessError> {
    self.tree.hole_published(node, pending.hole.clone(), /* site, ty, prompt, fork from pending.classified */)?;
    self.pending_holes.lock().insert((sid, pending.hole.clone()), pending);
    Ok(())
}
```
called from both `finish_run`'s suspend arm and `resume_parent`'s re-suspend
arm (today's ~15 duplicated lines in each collapse to the same call — a
bonus dedup the consolidation surfaces, not a goal in itself). Symmetrically,
consuming a hole (`resume_parent`'s success path, `take_finalized_value_
core`, `take_finalized_handle_keep_open`) becomes `tree.hole_consumed(node,
hole)?` + `pending_holes.lock().remove(&(sid, hole))` — two calls, one
helper (`consume_hole`), replacing the scattered `convo.pending = None` /
`convo.resident_hole = None` pairs at `harness.rs:2661,2806,3959-3973`.

`terminate_node` gains one more line: purge any `pending_holes` entries for
the retiring `(sid, node)` — an owning node's whole session going away
already drops its holes' relevance; without this the map would accumulate
orphaned entries for a node that never resumed before cancellation.

### What does NOT change

- `NodeState`'s wire shape (`{state: "suspended", hole: "scont_3"}`) — no
  HTTP/observatory consumer sees a shape change.
- `Event::HolePublished`/`Event::HoleConsumed`'s durable log schema — these
  were never the duplicated thing; only the in-memory choreography around
  emitting them was.
- Checkout semantics observable to callers (stowed-XOR-running, contention
  behavior, restart-safety) — pinned by existing suites per this lane's
  boundary; the registry promotion is additive (new epoch guard, new
  `Wedged` variant) except where hazard 1 above was already a latent bug.

## Consumer map (who ends up thin, who deletes)

| File | Before | After |
|---|---|---|
| `tidepool-runtime/src/session/registry.rs` | doesn't exist | NEW: `SessionRegistry<M,H>`, `Slot<M,H>`, `Checkout<M,H>`, `CheckoutError<H>`, `SingleSlot<M,H>` — the one mechanism |
| `tidepool-harness/src/registry.rs` | full impl (612 lines) | thin re-export + harness-specific glue, or deleted if nothing harness-specific survives |
| `tidepool-harness/src/tree.rs` | `Slot<M>` defined here | `Slot` import from runtime; `NodeState`/`HoleId`/`NodeId`/`SiteId` stay (harness-owned domain vocabulary) |
| `tidepool-harness/src/harness.rs` | `NodeConvo` 4 suspension fields, ~30 scattered read/write sites | `pending_holes` map + `publish_hole`/`consume_hole` helpers, same call count but one shape |
| `tidepool-repl/src/manager.rs` | full slot machine + `Checkout`/`CheckoutCustody` (335 lines) | thin policy wrapper over `SingleSlot` (cancel slot, bindings slot) |
| `tidepool-repl/src/state.rs` | `SessionState`/`Suspension`/`ContinuationId` (109 lines) | DELETED (`ContinuationId` moves into `manager.rs` or a small `ids.rs`; `Suspension`'s fields fold into `SingleSlot`'s generic `Meta`) |
| `tidepool-repl/src/server.rs` | ~20 co-transition sites | registry calls + `Slot::label()` reads |

## Sequencing (per STEPS)

1. This document — commit.
2. Promote the registry (`tidepool-runtime::session::registry`); harness
   becomes a client; harness suites green — commit.
3. REPL onto `SingleSlot`; `state.rs` deleted; `server.rs` co-transitions
   removed; repl suites green — commit.
4. Item 5's `pending_holes` map; `NodeConvo` shrinks; harness suites green
   — commit.
5. CLAUDE.md updates (harness + repl, present tense); full VERIFY;
   `submit_branch`.
