# Realm-ownership checklist — scored against current code

Scope: items 1-5 of the seven-item checklist (`one-compile-bootstrap.md:56-58`:
pending, binding/decl planes, finalized+bound root slots, cancellation, effect
roster + suspend threshold). Items 6-7 (persistent-root retirement,
compiled-function lifetime) belong to a sibling lane
(`plans/post-restart/spike-notes/realm-lifetime.md`) — referenced here only
where they intersect.

Method: every claim below carries a file:line anchor I read directly. No
source file was modified to produce this doc.

**Anchor verification note:** this doc was originally written against
`b6da8035` and rebased across root's `jit-chain-2` → `harness-interaction-surface`
fold. Every anchor below was re-read and corrected (where drifted) against
the post-rebase tree at `root.realm-spike` commit `feb0e0ff`. One correction
was substantive, not just a line-number shift: the fold's "D9" change
narrowed `BindingTable::seed_external_env` from an unconditional
every-live-binding sweep to a referenced-VarId intersection, which changes
part of Item 2's finding — noted inline where it applies.

**Method lesson worth recording plainly, not just implying:** the
re-verification pass also caught one anchor (`resident.rs:159` in Item 5,
now corrected to `:155`/`:163`) that was WRONG BEFORE THE REBASE — it
pointed at a doc-comment line, not the struct/field it was actually citing
evidence for. The rebase didn't cause this; re-deriving every citation from
scratch caught a pre-existing imprecision that the original spec's
"spot-check ten anchors" instruction had missed. That is a direct argument
for full re-verification over sampling after a rebase of this size, not
just for this doc — a spot-check finds drift, but it doesn't find a claim
that was never right in the first place.

---

## Item 1 — `pending` (suspension bookkeeping)

**WHAT EXISTS TODAY**

Two layers track "this computation is suspended on a hole," one per
abstraction level:

- `ResidentSession::pending: Option<String>` (`tidepool-runtime/src/session/resident.rs:181`)
  — the string continuation-id the *session* is suspended on, `None` when
  idle. Read via `pending_continuation`/`is_idle` (:280-287), gates every
  entry point (`run` :322-324, `run_bind` :371-373, `run_child`'s
  `prepare_child_fragment` :482-484, `reenter` :651-659). Set to `None` at
  entry (:666, before the re-entry even runs) and re-armed with a **fresh**
  hole only if the re-entry suspends again (`classify`, :781 completion /
  :797 suspension).
- `JitEffectMachine::suspended_continuation: Option<*mut u8>` (`tidepool-codegen/src/jit_machine.rs:180`)
  — the actual freer-simple continuation heap pointer. `ResidentSession::pending`
  is a string alias over this; the machine field is ground truth (comment at
  resident.rs:178: *"The machine's `suspended_continuation` is the ground
  truth"*).
- `stowed_root_cell: Option<Box<*mut u8>>` (`jit_machine.rs:188`) — holds the
  *parent's* continuation pointer for the duration of a nested child run
  (moved out of `suspended_continuation` by `enter_nested_child`,
  `jit_machine.rs:2194-2217`), registered as a GC root via
  `MachineState::register_stowed_root` (`machine_state.rs:513-514`).
- `nested_child_depth: usize` (`jit_machine.rs:195`) — counts children
  currently running against the one suspended parent; a parent resume is
  rejected while `> 0`. Incremented/decremented only by
  `enter_nested_child`/`NestedChildGuard::drop` (`jit_machine.rs:2209`,
  `:2306`).
- `last_bound_root`/`suspended_finalized_root` are scored under Item 3 (they
  record something different — a *value*, not "am I suspended" — but live on
  the same struct and are read out at the same call sites).

**MACHINE-GLOBAL OR PER-COMPUTATION**

Machine-global, structurally. Every one of these is a scalar/`Option` field
on `JitEffectMachine` (or the 1:1-wrapping `ResidentSession`), not a
per-realm entry in a collection. Today this is *safe* only because the
architecture keeps exactly one computation per machine — the field being a
singleton and the computation being a singleton coincide, so nothing has
ever had to prove they're different things. `run_child_fragment`
(`jit_machine.rs:2233`) is the one place today that runs a *second*
computation against the same machine, and it does so by construction
**not** letting that second computation itself suspend
(`run_child_fragment` → `run_with_entry`, which asserts
`suspended_continuation.is_none()` at :637 and passes `suspend_tag = None`
via `drive_to_done`, `jit_machine.rs:2478-2482` — confirms the TL's
background fact). `ResidentError::ChildSuspended` (`resident.rs:118`, raised
at `resident.rs:512`) is the wall this produces: if a child fragment yields
`Suspended` instead of completing, `finish_child_outcome` treats it as an
error rather than a second parked continuation, because there is nowhere
(no second slot) to park it.

One asymmetry worth naming precisely: the **GC-root-list side of this is
already generalized**. `MachineState::stowed_roots` (`machine_state.rs:100`)
is a `RefCell<Vec<*mut *mut u8>>` — it already accepts an arbitrary number
of registered stowed roots, and `perform_gc`'s root assembly already folds
it in alongside `persistent_roots` (comment at `machine_state.rs:538-539`,
`tidepool-codegen/CLAUDE.md`'s "FIVE sources" list). Nothing about *tracing*
N parked continuations is new work. What is *not* generalized is the
**bookkeeping that decides which pointer is "the" suspended one and whether
resuming it is legal** — that's the single-`Option` fields above, plus the
`nested_child_depth == 0` gate that only ever expresses "zero or one
children, and they can't suspend."

**WHAT REALM OWNERSHIP REQUIRES**

The type change the anchor doc already names:
`suspended_continuation: Option<*mut u8>` → `continuations:
HashMap<ContinuationId, ContinuationFrame>` (one entry per realm, each
carrying its own pointer). `ResidentSession::pending: Option<String>`
generalizes the same way one level up (a set/map of live continuation ids
instead of at-most-one). `stowed_root_cell`/`nested_child_depth` fold into
the same map: every *non-active* frame's pointer is already GC-rooted by
`MachineState::stowed_roots` for free (register one slot per frame instead
of one), and "how many children are running" generalizes from a depth
counter to "which realm currently owns the execution thread" (at most one,
since there is exactly one CPU driving one heap regardless of how many
realms are parked) — a marker, not a counter.

**COST RATING: REAL**

The GC-rooting half of this is FREE (already list-based, already traced).
The part that is REAL is the capability change, not the data-structure
change: today's design actively *forbids* a second computation from itself
suspending (`ChildSuspended`) precisely because there is only one slot to
put it in. The realm design's whole point is to remove that wall — to let
N continuations each independently reach a suspend point and later be
resumed in **any order** (the plan's own falsifier, step 2 of
`realm-spike.md:73-77`, is "resumed out of order," which is exactly the
scenario today's single-slot-plus-depth-counter invariant cannot express at
all, correctly *or* incorrectly — it's simply not representable). That's a
new scheduling invariant ("N independently resumable parked continuations,
resumed in caller-chosen order, GC-safe throughout"), not a mechanical
relocation of an existing one. Whether it holds is exactly what the
spike's prototype (step 2) needs to test empirically, not something this
read-only pass can settle by inspection.

---

## Item 2 — binding / decl planes

**WHAT EXISTS TODAY**

`BindingTable` (`tidepool-codegen/src/binding_table.rs:89-94`) is a single
flat two-layer map:

```
current: HashMap<BindingName, SessionVarId>   // name -> newest gen's id (shadowing)
live:    HashMap<SessionVarId, BindingEntry>  // every still-rooted binding, incl. shadowed
```

It lives as ONE field on `PersistentSession` (`tidepool-runtime/src/session/persistent.rs:251`
`bindings: BindingTable`) — one instance per session, i.e. one per machine.

**Post-rebase correction, not just a line-number fix — dated finding:** this
paragraph and the two after it were re-derived against the tree AFTER this
lane's rebase across `harness-interaction-surface` (verified at
`root.realm-spike` commit `feb0e0ff`; see the conflict ledger's Z6 entry for
the full rebase chain). They were NOT carried over from the pre-rebase
version of this doc; a reader should treat them as current against the
post-jit-chain-2 tree, not as a stale claim that merely survived a
line-number fixup.

The fold (landing what its own tests and commit message call "D9",
`git show 5d070690`) changed `seed_external_env`'s actual behavior, not just
its position. Pre-fold, `seed_external_env` walked `live.values()`
unconditionally — every live binding in the whole session, no matter what
the compiling fragment referenced. Post-fold it takes a `referenced: &[VarId]`
slice and only seeds the intersection with `live`
(`binding_table.rs:193-201`, doc at :175-191; wrapped by
`PersistentSession::seed_external_env` at `persistent.rs:680-688`). Callers
now compute that slice as the fragment's actual free variables before
calling it — `resident.rs`'s `run`/`run_bind` both do
`tidepool_repr::free_vars::free_vars(expr)` then
`self.core.seed_external_env(&referenced)` (`resident.rs:338-339` and
`:377-378`); the repl's four bind/reference paths in
`tidepool-repl/src/session.rs` do the same
(`session.rs:1041-1042, 1150-1151, 1305-1306, 1451-1452`). The table's own
test suite states the change explicitly: `binding_table.rs:259-262`'s doc
comment on `seed_external_env_seeds_every_referenced_binding_when_all_are_referenced`
— *"the old ... claim ('every live binding, unconditionally') no longer
holds by default — it only holds as the special case where every live
binding is also referenced"* — and `binding_table.rs:286-300`'s
`seed_external_env_narrows_to_only_referenced_bindings` proves the
narrowing directly.

**Deliberate, but deliberate for a different reason than realm isolation —
checked, not assumed.** The narrowing landed as commit `5d070690` ("fix
(codegen,runtime,repl): narrow seed_external_env to referenced bindings"),
whose own message states the motivation plainly: pre-fold,
`seed_external_env` "inserted EVERY live session binding into a fresh
`ExternalEnv` on every fragment compile, proportional to total live
bindings regardless of what the fragment touches" — a compile-time cost
problem that gets worse as a session accumulates bindings, nothing to do
with realm isolation or cross-realm safety. So this is NOT an incidental
side effect that could vanish if someone touches unrelated code near it —
it's a deliberate, tested change — but it was deliberately solving a
DIFFERENT problem than the one this checklist cares about. The commit's own
test (`session_seed_external_env_root_retention.rs`, referenced in its
message) pins "narrowing the seed doesn't narrow GC-root retention," not
"narrowing the seed keeps realm B's bindings out of realm A's compiled
fragment." Nothing in the test suite exercises two independent binding
scopes sharing one table, because that scenario doesn't exist yet — it's
exactly what the realm design would introduce. **Practical consequence for
the verdict:** the property this item leans on (VarId-keyed cross-realm
isolation) currently holds only as a corollary of fresh-id minting plus this
narrowing, and nothing pins that corollary directly. If the realm design
relies on it, land a test that states the property in its own terms (two
scopes, colliding local names, assert neither's `ExternalEnv` ever contains
the other's `SessionVarId`) rather than inferring it from D9's own
proportional-cost-focused tests, which could be satisfied by a future change
(e.g. a per-fragment env cache keyed differently) that reintroduces the leak
while still passing every test D9 added.

**Which half is which, stated once for quoting:** the VarId-keyed
`seed_external_env` layer is now CLOSE TO FREE for realm purposes — narrowed
by a deliberate, tested (if differently-motivated) change, needing only a
dedicated pinning test before the verdict can lean on it. The display-name
`current`/`resolve` layer (next paragraph) is UNCHANGED and stays REAL — no
narrowing, no realm concept, still a flat last-bind-wins map.

`add_fragment_session`/`add_child_fragment_session`
(`persistent.rs:436-453`, `:457-474`) both still funnel into
`JitEffectMachine::add_function` (`jit_machine.rs:1291`), which resolves
whatever `ExternalEnv` it's handed exactly as before — this pathway is
unaffected by the narrowing, it just now receives a smaller env.

Resolution by display name (`BindingTable::resolve`, `binding_table.rs:145-148`,
unaffected by D9) is used only by the single-user-scoped REPL
(`tidepool-repl/src/session.rs:269,772,1534,1749,1832`
— `iter_current`/`remove_current`, all interactive `:bindings`/rebind-shadow
operations) — i.e. everywhere it's used today, "the session" and "one
interactive scope" are the same thing by construction. This layer is
**not** touched by D9's narrowing: `current: HashMap<BindingName, SessionVarId>`
is still a single flat last-bind-wins map with no realm concept, and remains
this item's live hazard (below).

**MACHINE-GLOBAL OR PER-COMPUTATION**

Machine-global (session-global) flat namespace, but narrower than it was
before this lane's rebase. There is no realm/owner tag anywhere in
`BindingEntry` or `BindingTable` — `SessionVarId` disambiguates individual
bindings (collision-free by construction: the extract mints a fresh id per
(re)bind, `binding_table.rs:12-22`) — but `current`'s name→id map still
operates over the whole table with no partition. `seed_external_env` no
longer does (D9, above): it's scoped to whatever the caller says a fragment
actually references, which in practice means it's already close to
per-computation for the VarId-keyed layer.

**Is this a bug or deliberate?** Both, depending on which relationship it's
serving, and on which of the two layers (`current` vs. `seed_external_env`)
is in view:

- For the PARENT↔CHILD relationship `run_child_fragment` exists for, full
  sharing is explicitly the point: `tidepool-codegen/CLAUDE.md`'s
  "segment 40" section says outright that a child fragment reads "the
  parent's bindings zero-copy," and `resident.rs:44` ("reading the parent's
  bindings zero-copy") repeats it — and unlike `resident.rs::run`/`run_bind`,
  `run_child`'s `external_env` is passed in by the caller directly
  (`resident.rs:418-438`'s `run_child`, gated by `prepare_child_fragment`'s
  `self.pending.is_none()` check at `:482-484`), not derived via
  `BindingTable::seed_external_env` — so D9's narrowing doesn't touch this
  path at all. A nested child *is* conceptually
  part of the same computation (it exists only because the parent is
  suspended and its result flows back to that same parent) — sharing
  everything there is correct, not incidental.
- For the REALM relationship the spike is scoring — "the outer harness
  loop, or one answerer subtree" as *independent* logical computations
  sharing one machine — nothing distinguishes that from the parent/child
  case in `BindingTable`'s eyes, but the practical exposure is now smaller
  than it would have been pre-fold. The `seed_external_env`
  (VarId-keyed) leak is effectively closed by D9's narrowing: an unrelated
  realm's fresh `SessionVarId`s essentially never appear in another realm's
  fragment's free-variable set, so they're never seeded into that
  fragment's `ExternalEnv` regardless of both realms sharing one
  `BindingTable`. What's NOT closed is the display-name layer: two
  independent realms that each locally bind the same name (say each has its
  own turn that does `x <- e`) still land in the SAME `current` map, and
  last-bind-wins there means whichever realm's fragment bound most recently
  silently shadows the other's `x` for any name-based lookup
  (`resolve`/`iter_current`) — used today only by the single-user-scoped
  REPL, so nothing exercises this collision in practice yet, but nothing in
  the type prevents it either. `SessionVarId`-keyed access (`get`, the JIT's
  actual Var-miss resolution) stays collision-free because ids are minted
  fresh, so this remaining piece is a **silent display-name shadow**, not a
  root-safety hazard — a realm cannot corrupt another's binding, but its
  `:bindings`-style view could report the wrong owner for a shared name.

**WHAT REALM OWNERSHIP REQUIRES**

For the surviving hazard: a realm-scoped `current` map (or a
`realm: RealmId` field threaded through `BindingEntry` plus a
realm-filtered `resolve`/`iter_current`) so two unrelated realms sharing a
display name don't shadow each other, while the parent/child sharing
`run_child_fragment` deliberately wants stays untouched (it doesn't route
through `current` at all). The `seed_external_env` half no longer needs
this — D9 already gives it the right scoping for free, as a side effect of
work done for an unrelated reason (compile-time cost, not realm isolation).

**COST RATING: REAL**

Smaller in scope than it would have been pre-fold, but still REAL for the
same underlying reason: the codebase does not currently express "same
realm" vs. "different realm" as a *concept* in the `current`/`resolve`
layer — today the only relationship it knows there is "in one machine"
(share) vs. "in different machines" (share nothing, since each session owns
its own `BindingTable`). The `seed_external_env` half of this item would
have been REAL pre-fold and is now closer to FREE, but Item 2 as a whole
still needs a new invariant for the display-name layer, not a rename.

---

## Item 3 — finalized + bound root slots

**WHAT EXISTS TODAY**

Two single-`Option` fields on `JitEffectMachine`:

- `last_bound_root: Option<RootSlot>` (`jit_machine.rs:208`) — set once per
  value-plane BIND completion, at `finish_suspendable`'s `Done` arm
  (`jit_machine.rs:1063`, inside the `bind_forced` branch). Taken exactly
  once, by `JitEffectMachine::take_last_bound_root` (`jit_machine.rs:1173-1175`).
- `suspended_finalized_root: Option<RootSlot>` (`jit_machine.rs:217`) — set
  once per suspend-on-closure-valued-`finalize`, at `finish_suspendable`'s
  `Suspended` arm (`jit_machine.rs:1106-1107`). Taken exactly once, by
  `take_finalized_root` (`jit_machine.rs:1183-1185`).

**Take-site trace** (grepped across the whole workspace — `take_last_bound_root`/
`take_finalized_root` each have exactly ONE call site outside their own
crate):

- `take_last_bound_root` → `ResidentSession::materialize_binder`
  (`resident.rs:697`), called synchronously right after the `on_eval_thread`
  call that produced the completion, from exactly two places: `run_bind`
  (`resident.rs:399-401`, immediately after its own `on_eval_thread` call at
  :391-395) and `reenter` (`resident.rs:678-680`, immediately after its
  `on_eval_thread` call at :671-676).
- `take_finalized_root` → `ResidentSession::apply_finalized`
  (`resident.rs:557`), called at the top of that method, itself gated on
  `self.pending.is_none()` returning `NotSuspended` (:547-549) — i.e. it can
  only run while the session is suspended, and it's the *only* consumer of
  the slot the suspend that set `self.pending` just produced.

**Is the window closed by sequencing or by luck?**

By sequencing — and specifically by two overlapping guarantees, both of
which are properties of *today's one-computation-per-machine* architecture,
not of the `Option` type itself:

1. Rust aliasing: every call above takes `&mut self` on `ResidentSession`
   (or `&mut JitEffectMachine` through it). Two `on_eval_thread` calls (the
   only place a run/resume executes) cannot be in flight concurrently
   against the same session — the borrow checker forbids it. So within one
   session, "run something that might set the slot" and "read the slot"
   can never race at the Rust level.
2. Machine invariant: only one computation can be suspended at a time
   today (`suspended_continuation` is itself a single `Option`, and
   `run_child_fragment`'s children are structurally forbidden from
   suspending — Item 1). So there is only ever one write to
   `last_bound_root`/`suspended_finalized_root` in flight between any two
   consecutive reads, and the call sites always read immediately after the
   write that produced their own outcome, never after some *other*
   computation's write.

**This is a genuine, concrete correctness hazard under the realm
generalization, stated plainly:** if `continuations: HashMap<ContinuationId,
ContinuationFrame>` lands but `last_bound_root`/`suspended_finalized_root`
are left as machine-level `Option` fields (the naive incremental path), the
sequencing guarantee in point 2 above breaks. Two realms park
concurrently; realm A's bind-fragment suspends/completes and sets
`last_bound_root = Some(slot_A)`; before the caller gets around to calling
`take_last_bound_root()` for A (e.g. if a future orchestrator processes a
*batch* of newly-suspended/newly-completed realms before draining each
one's root — plausible once "drive whichever realm is ready" replaces
today's single-session `&mut self` serialization), realm B's own
bind-fragment also completes and overwrites the slot with `Some(slot_B)`.
The caller then calls `take_last_bound_root()` expecting A's value and
silently receives B's — `materialize_binder` would bind the WRONG value
under realm A's name, with no error raised anywhere (both are valid
`RootSlot`s; nothing type-checks or panics on the mismatch). The hazard is
not hypothetical-in-general — it is exactly the class of bug the "resumed
out of order" falsifier (`realm-spike.md:73-77`) is designed to catch, just
manifesting on this specific pair of fields rather than on
`suspended_continuation` itself.

**WHAT REALM OWNERSHIP REQUIRES**

Move both fields onto `ContinuationFrame` (one `last_bound_root`/
`suspended_finalized_root` slot per realm) instead of the machine. Each
realm's own take-site then reads its own frame's slot; a second realm's
completion cannot touch it.

**COST RATING: MECHANICAL**

This is a straight field relocation, not a new invariant: the *rule* stays
identical ("exactly one write between the suspend/complete that produces a
value and the one read that consumes it") — it just needs to be scoped per
realm instead of assumed globally. No new synchronization, no new GC
concern (each slot is still a plain `RootSlot`, already GC-safe via
`old_space`'s persistent-root registration). The risk is entirely in
*forgetting* to do this relocation when the HashMap lands — which is why
it's called out plainly above rather than left implicit.

---

## Item 4 — cancellation

**WHAT EXISTS TODAY**

`cancel_flag: Arc<AtomicBool>` (`jit_machine.rs:155`) is a single field on
`JitEffectMachine`, created once per machine (`Arc::new(AtomicBool::new(false))`
at both constructors, `jit_machine.rs:477`, `:509`) and never replaced for
the machine's life. `CancelHandle` (`jit_machine.rs:242`) is just a cloneable
wrapper around that one `Arc` (`cancel_handle()`, `jit_machine.rs:527-529`
— `CancelHandle(self.cancel_flag.clone())`), so every handle anyone holds
for a given machine controls the SAME flag.

The flag is (re-)installed into `MachineState` at the top of **every** run
entry, unconditionally, via `install_registries` (`jit_machine.rs:558`:
`self.machine_state.set_cancel_flag(self.cancel_flag.clone())`) — called
from `run_with_entry`, `run_suspendable_with_entry`,
`resume_suspended_inner`, etc. (all six call sites at `jit_machine.rs:675,
814, 980, 1641, 1778, 1933` thread `&self.cancel_flag` through to
`drive_effect_loop`/`drive_to_done`). It is cleared again at
`RegistryGuard::drop` (`jit_machine.rs:378`, `(*self.machine_state).clear_cancel_flag()`).
`MachineState` itself stores it as `RefCell<Option<Arc<AtomicBool>>>`
(`machine_state.rs:61`) — already "one flag, installed fresh per run,
cleared after," which is the right shape; it's just always fed `self`'s
one Arc.

It is observed at exactly two safepoints. **Checked-and-cleared false
alarm, recorded rather than dropped silently:** the rebase go-signal flagged
`host_fns/gc.rs` changing by 11 lines as worth an actual re-read for this
item, since the cancel flag is observed at the GC safepoint. Re-read
directly against the post-rebase tree rather than trusting the diffstat —
the 11-line change is entirely in `host_alloc_gc`'s alloc-retry
consolidation around line 1165, an unrelated allocator-path refactor into a
shared `heap_bridge::gc_retry` helper. The cancellation safepoint itself is
byte-identical to the pre-rebase read, same lines, same logic — the
question was worth asking, and the answer is no impact:

- The effect-dispatch boundary inside `drive_effect_loop`
  (`jit_machine.rs:2652`: `if cancel_flag.load(Relaxed) { … Cancelled … }`),
  checked after every handler call, using the SAME `&Arc<AtomicBool>`
  parameter threaded in from `self.cancel_flag`.
- `host_fns::check_cancel_and_set_error` (`tidepool-codegen/src/host_fns/cancel.rs:11-27`,
  unchanged), called from JIT-emitted safepoints (GC trigger,
  `host_fns/gc.rs:254-273`, unchanged; the recursive join-point back-edge,
  `host_fns/cancel.rs:29-51`, unchanged) — this one reads through
  `MachineState`'s installed flag (`machine_state.rs:192-201`, unchanged),
  i.e. through the SAME per-run-installed value as above.

**MACHINE-GLOBAL OR PER-COMPUTATION**

Machine-global: one `Arc<AtomicBool>` for the machine's entire life,
installed into the (also machine-global, but freshly-set-per-run)
`MachineState` slot at every run entry. `CancelHandle::cancel()` sets that
one bool; there is no way today to target "cancel just this parked
continuation" — a `CancelHandle` obtained from a machine cancels
*whichever computation that machine happens to be running* when the next
safepoint fires, with no realm-awareness at all.

**WHAT REALM OWNERSHIP REQUIRES**

A flag per frame, checked at the same safepoints. The install pattern
already does the hard part: it already re-installs a fresh `Arc` clone into
`MachineState` at the top of every single run/resume call
(`jit_machine.rs:558`), from a value chosen at that call site. Making that
per-realm is changing what's on the *right-hand side* of that one line —
`active_frame.cancel_flag.clone()` instead of `self.cancel_flag.clone()` —
not adding a new call site or a new safepoint. `CancelHandle` becomes
"obtained from a specific `ContinuationFrame`" instead of "from the
machine," and cancelling realm A's handle only ever gets installed into
`MachineState` while realm A's frame is the one actively running — realm
B's turn, driven later with B's own flag installed, is untouched.

**COST RATING: MECHANICAL**

The per-run re-install discipline this needs already exists and is already
exercised on every single call path (there is no "sometimes forget to
install" case to guard against — `install_registries` is unconditional).
This is a relocation of which `Arc` gets cloned into that existing call,
not new machinery or a new safepoint discipline.

---

## Item 5 — effect roster + suspend threshold

**WHAT EXISTS TODAY**

At the raw `JitEffectMachine` API, `suspend_tag: u64` is a genuine per-call
argument, not machine state — every `run_suspendable*`/`resume_suspended*`
signature takes it explicitly (`jit_machine.rs:716-768` for the run family,
`:848-874` for resume), and it flows straight through to
`drive_effect_loop`'s `suspend_tag: Option<u64>` parameter
(`jit_machine.rs:2539`). The threshold test itself:

```rust
if suspend_tag.is_some_and(|t| tag >= t) {   // jit_machine.rs:2624
    return Ok(DriveOutcome::Suspended { … });
}
```

The comment directly above it (`jit_machine.rs:2604-2623`) states the
assumption this relies on explicitly: `suspend_tag` is "the FIRST
interposed (unhandled) tag — the position right after the last effect with
a real handler," and every tag at or beyond it is unhandled *by
construction* because `Ask`/`RunLLMTurn`/`Finalize` are "always appended
consecutively after the handled stack." I.e. the test is only correct
relative to ONE specific effect row whose handled effects occupy a
contiguous low prefix and whose suspend-worthy effects occupy a contiguous
high suffix.

BUT one layer up, at the session API the spike actually exercises,
`suspend_tag` stops being per-call and becomes a cached session-lifetime
value: `PersistentSession` stores `ask_tag: u64` as a plain struct field
(`tidepool-runtime/src/session/persistent.rs:259`, set once at `new()`
:269-281), exposed via `ask_tag()` (`:321-323`), and `ResidentSession`
reads that ONE cached value at every single run/resume call site
(`resident.rs:347, 386, 660` — `let ask_tag = self.core.ask_tag();`) rather
than deriving it fresh per fragment. This is exactly the "dormant" state
the anchor doc names: today it's dormant because a `ResidentSession` (like
every session today) is monomorphized over exactly one effect row for its
whole life, so "the cached threshold" and "the correct threshold for this
fragment" are always the same number.

There's a second, structurally stronger reason the same dormancy applies to
handler dispatch itself, not just the threshold: `DispatchEffect::dispatch`
(`tidepool-effect/src/dispatch.rs:254-262`) is positional over an `HList`
— `HCons<H, T>::dispatch` peels tag 0 to the head handler and recurses with
`tag - 1` on the tail (`dispatch.rs:275-301`, unchanged — `tidepool-effect`
was untouched by this fold). `ResidentSession<H, O>` (`resident.rs:155`
struct declaration, `:163` `handlers: H` field) is monomorphized over ONE
concrete `H` (one fixed handler stack, one fixed effect-row shape) for the
session's entire life — this is a Rust type-level fact, not just a runtime
caching choice.

**THE TWO-ROW EXAMPLE (spike step 3)**

Row A — today's ordinary stack (handled effects, then the suspend-worthy
tail appended per the comment's own description):

```
tag 0: FileIO   (handled)
tag 1: Proc     (handled)
tag 2: Ask      (unhandled/suspend)
```
`suspend_tag_A = 2`. `tag >= 2` correctly separates {FileIO, Proc} (dispatch)
from {Ask} (suspend).

Row B — a GENERAL node's base-effect row with one extra handled domain
effect (`Memory`) prepended before the same suspend-worthy tail:

```
tag 0: FileIO   (handled)
tag 1: Proc     (handled)
tag 2: Memory   (handled)
tag 3: Ask      (unhandled/suspend)
```
`suspend_tag_B = 3`.

If a shared machine hosts one realm compiled against Row A and another
against Row B, and the session layer's ONE cached `ask_tag` (today always
correct because there's only one row) is applied uniformly:

- Using `suspend_tag_A = 2` while driving Row B's continuation: a `Memory`
  request yields `tag = 2`. `2 >= 2` is true → the machine **wrongly
  suspends on a normal, handled `Memory` call**, surfacing it as an Ask to
  a caller that has no idea what to do with it, instead of dispatching it
  to the Memory handler.
- Using `suspend_tag_B = 3` while driving Row A's continuation: an `Ask`
  request yields `tag = 2`. `2 >= 3` is false → the machine tries to
  **dispatch tag 2 to a handler** instead of suspending. Per
  `dispatch.rs:275-301`, tag 2 in Row A's own 3-handler-position space
  either falls off the end of a shorter `HList` (`HNil::dispatch`,
  `dispatch.rs:264-273`, returns `EffectError::UnhandledEffect`) or — worse,
  if the handler stack is long enough for unrelated reasons — silently
  routes to whatever handler happens to sit at position 2, running the
  WRONG effect's handler against an `Ask` request.

Either direction is a real, silent misclassification, not a crash that
would get caught immediately.

**Per-frame metadata, or something structural?**

Both, and the per-frame part is the easy half. Promoting `suspend_tag` from
a session-cached `u64` to a field the `ContinuationFrame` carries (exactly
what the anchor's own frame sketch already lists: "pointer, realm id,
table, suspend tag, pending-kind" — `one-compile-bootstrap.md:53`) fixes the
threshold-test half cleanly and mechanically, and the `table: &DataConTable`
already in that same sketch is the natural carrier for whatever
tag→effect-name metadata is needed for diagnostics.

What that sketch does **not** yet name, and what the `dispatch.rs` evidence
above shows is also required: `handlers: H` is a single, type-monomorphized
field on `ResidentSession` (`resident.rs:163`), not something that can vary
per-frame without either (a) every realm sharing one machine committing to
the SAME handled-effect prefix (only the suspend-worthy tail may differ in
length — a real constraint on realm composition, not automatic), or (b)
introducing a type-erased/dynamic dispatch object selected per active
realm (a `Box<dyn DispatchEffect<U>>` per frame, say) so two realms with
genuinely different handled prefixes can each dispatch through their own
row correctly. That second option is structural — a new abstraction over
today's compile-time-monomorphized `H`, not a field addition.

**COST RATING: REAL**

The threshold number is per-frame-mechanical. But the frame sketch as
written is incomplete for the general case: `H: DispatchEffect<U>`'s
positional, monomorphized dispatch means tag *meaning* is only stable
across realms if their handled-effect prefixes are identical, which is not
guaranteed by anything today and is exactly what "a GENERAL node's base
effect row" threatens to violate. Closing that gap needs either a
composition constraint enforced elsewhere (documented, not yet code) or a
new dynamic-dispatch layer — either way, new machinery, not a field move.

---

## Summary table

| # | Item | Machine-global or per-computation (today) | Cost rating |
|---|------|---------------------------------------------|--------------|
| 1 | `pending` (suspension bookkeeping) | Machine-global (`Option`/`usize` singletons); GC-root list side is already N-ready | **REAL** |
| 2 | Binding/decl planes | Two halves: `current` (display-name) map is machine-global, unchanged, no realm key; `seed_external_env` (VarId-keyed) is deliberately narrowed by D9 (commit `5d070690`, motivated by compile cost, not realm isolation) | **REAL** for `current`; VarId half close to **FREE** pending a dedicated pinning test |
| 3 | Finalized + bound root slots | Machine-global (single `Option<RootSlot>` × 2); window closed today by sequencing, not luck | **MECHANICAL** |
| 4 | Cancellation | Machine-global (one `Arc<AtomicBool>` per machine); re-install pattern already per-run | **MECHANICAL** |
| 5 | Effect roster + suspend threshold | Threshold is per-call at the JIT API but session-cached to one value; handler dispatch is type-monomorphized to one row | **REAL** |

## Step-3 dispatch-metadata question, answered directly

No — `tag >= suspend_tag` does **not** still separate "suspend me" from
"dispatch me" correctly once a GENERAL node's base-effect row enters,
*if* realms with different rows share a machine and either (a) the
session-cached single `ask_tag` is reused across them, or (b) they're
driven through one monomorphized `handlers: H`. The concrete break: Row A
(`FileIO=0, Proc=1, Ask=2`, `suspend_tag=2`) vs. Row B
(`FileIO=0, Proc=1, Memory=2, Ask=3`, `suspend_tag=3`) — applying A's
threshold to B's continuation turns a normal `Memory` call (tag 2) into a
wrongly-suspended Ask; applying B's threshold to A's continuation turns a
real `Ask` (tag 2) into a dispatch attempt that either errors
(`EffectError::UnhandledEffect`) or, worse, silently runs whatever handler
happens to occupy position 2 in a longer stack. The fix has two parts: a
mechanical one (move `suspend_tag` — and the `DataConTable` needed to
interpret a request's shape — onto the per-realm `ContinuationFrame`, which
the anchor's own sketch already anticipates) and a structural one not yet
named anywhere (`handlers: H`'s compile-time monomorphization over one
fixed handler stack must either become a documented composition constraint
— every realm sharing a machine keeps an identical handled-effect prefix,
varying only in how much suspend-worthy tail is appended — or become a
dynamic per-frame dispatch object). Item 5's REAL rating is carried
entirely by that second, structural part.

## Cross-references (not investigated in this lane)

- Persistent-root retirement and compiled-function lifetime (checklist
  items 6-7, "the real cost" per `one-compile-bootstrap.md:58-61`) are owned
  by a sibling lane; see `plans/post-restart/spike-notes/realm-lifetime.md`.
- The falsifier prototype (two continuations parked in one machine, resumed
  out of order, under `GC_POISON`/`HEAP_VERIFY`) that would empirically test
  Item 1's REAL rating is `realm-spike.md:73-77`'s step 2, owned separately;
  this doc is analysis only, not a test result.
