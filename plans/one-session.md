# One session: collapse the self-harness onto a single resident session, retire the single-slot suspension path

**Goal.** `runLLMTurn @(State -> State)` (and function-valued answers generally)
work end-to-end: the answerer finalizes a closure, the loop applies it. The
mechanism is architectural, not a transport: answerer turns run as parked
fragments ON the outer session, so a finalized closure is born in the loop's own
heap. Alongside, the legacy single-slot suspension path is fully removed — the
parked continuation registry becomes the ONLY suspension mechanism, so the
slot/registry mixing hazard class is unrepresentable.

**Why this is cheaper than it looks.** The machine layer is already done: the
parked-continuation registry (realm machinery) in `tidepool-codegen` is landed,
adversarially tested (`tests/realm_multi_continuation.rs` — parent+child parked
simultaneously, resumed in both orders, 8 parks with GC between each resumed
shuffled, all under `TIDEPOOL_GC_POISON`+`TIDEPOOL_HEAP_VERIFY`), and
contract-frozen (`plans/post-restart/realm-lanes/continuation-parking-contract.md`).
It has zero consumers above codegen. Everything above it still assumes one
continuation per session; the work is converting the session layer, harness
registry, and driver onto the registry, then deleting the slot path.

## Ground truth (mapped 2026-08-13; anchors are file:line at that date)

Layer status:

- **Machine** (`tidepool-codegen/src/jit_machine.rs`) — DONE, minus gaps below.
  Registry: `continuations: HashMap<ContinuationId, ContinuationFrame>` (:722),
  each frame a registered GC root for its whole parked lifetime
  (`park_continuation` :3300, receipt `stowed_roots_count() == parked_count()`
  at every mutation). Resume by identity, any order; frame replays its own
  `suspend_tag`/`kind`/table/cancel-flag (`resume_parked` :3471). The SLOT path
  (`suspended_continuation` :654) is protected only by a TEMPORAL argument (no
  GC while suspended, L7 asserts) — not rooted. The two paths never mix
  (asserted both directions), which forces wholesale conversion.
- **Runtime session** (`tidepool-runtime/src/session/`) — all slot-path.
  `persistent.rs` has 5 run/resume pairs (Value/Bind/Project/Render + session);
  `resident.rs` has `pending: Option<String>` (:186), the ChildSuspended wall
  (:623 — dead code today: children run `drive_to_done`, a suspending child
  surfaces `UnhandledEffect`, never `ChildSuspended`), `apply_finalized`
  (:652, same-session closure application). One-shot eval's ask path also
  slot-resumes (`tidepool-runtime/src/lib.rs:433`).
- **Harness registry** (`tidepool-harness/src/registry.rs`) —
  `Slot::Suspended { machine, hole }` carries ONE hole; `checkout_resume` does
  `mem::replace(slot, Running)` destroying the hole (:210); `RunningChild` has
  no transition for a child that suspends.
- **Driver** (`tidepool-harness/src/selfharness/driver.rs`) — outer session is
  a private field, NOT in the registry, with NO decl plane (`lib: None`,
  :596). Per-loop answerer node has its own session, boot ~1096-1112, retire
  :1122. Finalize crosses by value: `take_finalized_value_keep_open` →
  `outer.session.resume(&hole, answer)` (:1197-1200), tolerant bridge
  substitutes `CLOSURE_SENTINEL` for closures.

Compatibility facts that make the collapse viable:

- Both rows (`outer_decls` = `[RunLLMTurn, AskUser]`, `answerer_decls` =
  `[AskUser, Fork, Finalize T]`) compute `suspend_tag = 0` and an EMPTY handled
  prefix — every effect suspends, `handlers.dispatch` is unreachable on both,
  and empty prefixes are compatible with anything under
  `check_prefix_compatible`.
- The generated effect GADTs from different rows' `Tidepool.Effects` modules
  get IDENTICAL DataConIds and qualified names (module name is the constant
  string `Tidepool.Effects`; `stableVarId` never sees the dir), so
  `merge_table` unifies them silently and constructor-name hole routing
  (`classify_hole`) is row-agnostic. Consequence: the table cannot distinguish
  rows — "which fragment is running" is the caller's job.
- `__selfHarnessState` and the author-module import are per-FRAGMENT (helpers
  splice + include path); an answerer fragment cannot name them, and
  `Harness.hs` does not even compile under the answerer row (its
  `Tidepool.Harness` import pins `M`). No leakage, no shadowing.
- Union tags are baked per-fragment by GHC's `Member`; the machine reads the
  tag out of the heap per request. Nothing correlates tags across fragments.

The two real cross-row hazards (both get structural guards in Phase 4):

- **Decl plane is SOURCE, recompiled per fragment** against that fragment's
  `Tidepool.Effects`. An accumulated decl mentioning `M`/`Tidepool.Effects`/
  `Tidepool.Form`/`Tidepool.Fork`/`Tidepool.Harness` poisons every later turn
  on the other row — total failure, misattributed to `Lib.G<g>.hs`. Guard:
  pure-decls-only in the shared plane (keep `ModuleEnv::standalone_default`;
  REJECT hoisted effects-family imports and `M`-typed decls at define time).
- **Tier-1 value binds with `M` inside the type** (`Int -> M Int`) serialize
  the row into `Val.G<g>.hi`; cross-row injection fails iface-load or types at
  the wrong `Eff`. Guard: reject at `mkBoundBinders`
  (`haskell/app/Main.hs:1478-1484`) any bind whose type mentions a TyCon
  defined in the generated effects module. Pure closures (`State -> State`)
  are row-free and bind fine.

## Phases

Each phase lands green (full quick tier + named GHC-heavy legs + the realm/GC
adversarial suites) before the next starts. Deletion comes AFTER conversion so
every intermediate commit builds.

### Phase 0 — machine completion (tidepool-codegen only)

1. `ParkKind::Project { n_fields }` / `ParkKind::Render { field0_forced }` +
   matching `ParkedOutcome` completion products, returned INLINE like
   `bound_root` (the slot path already returns Project/Render roots inline —
   same `!Send` discipline; see `plans/unpark/suspendable-materialization.md`
   §6.2).
2. **Atomic cross-frame delivery**: `resume_parked_with_finalized(target:
   ContinuationId, source: ContinuationId, ...)` — resume frame `target` with
   the finalized closure payload of frame `source`, entirely machine-internal
   (no `RootSlot` — which is `!Send` — ever crosses into upper layers). The op
   consumes `source` (frame removed, stowed root deregistered, finalized root
   ownership transferred for exactly the resume's duration), and the required
   GC ordering (root stays registered until the value is installed in the
   resumed continuation) is a machine invariant, not a caller convention.
   Data answers keep the existing `ResumeInput::Answer` materialization path.
3. **`drain_realm(realm: RealmId)`** — atomic realm retirement: every frame
   owned by the realm removed, its stowed roots deregistered, untaken
   finalized roots dropped (their persistent-root registrations handled per
   the existing `take_finalized_root` doc), sibling realms untouched, and the
   rooting receipt (`stowed_roots_count() == parked_count()`) true before and
   after. Per-frame `resume_parked(id, Abort)` exists but a loop over ids in
   upper layers is exactly the ownership protocol that gets violated.
4. Extend the realm adversarial suite: Project/Render parks under
   poison/verify; `resume_parked_with_finalized` delivering a closure into a
   sibling frame with forced GC between take and install; `drain_realm` with
   parked finalize frames + untaken roots; receipts throughout.

### Phase 1 — runtime session conversion (tidepool-runtime)

- `persistent.rs`: all five run/resume pairs route through
  `run_fragment_suspendable_parked`/`resume_parked`. `is_suspended()` →
  count/ids. Suspend threshold stays per-session for now (both rows agree at
  0) but is ASSERTED against the frame's recorded tag.
- `resident.rs`: `pending: Option<String>` → ordered map String→ContinuationId
  (insertion-ordered; top() = last). `run`/`run_bind` while suspended: ALLOWED
  (this is the point) — policy above decides who may run what.
  `run_child`/`run_child_pure` dissolve into plain fragment runs (a "child" is
  an ordinary fragment while frames are parked; it MAY suspend and park).
  `apply_finalized` keeps its shape over `take_parked_finalized_root(id)`.
  The :796 boolean reconciliation becomes a parked-ids comparison (the wedge
  guard survives, depth-aware). `ChildSuspended`, `WrongContinuation` plural,
  `ResidentOutcome::Suspended` carries the id.
- One-shot engine (`session/engine.rs`, `lib.rs:433`): single park per turn,
  trivial realm.
- Oracle: the full repl + runtime suites unchanged-green; repl is the richest
  consumer of Project/Render.

### Phase 2 — slot deletion (tidepool-codegen)

Remove `suspended_continuation`, `stowed_root_cell`, `last_bound_root`,
`suspended_finalized_root`, `nested_child_depth`, `enter_nested_child`,
`NestedChildGuard`, `run_child_fragment{,_pure}`, the four slot run/resume
families, `ParkTarget::Slot`, the L7 asserts and both mixing guards, and the
slot arms of `finish_suspendable`. Port `nested_child_gc_rooting.rs` scenarios
to registry equivalents (most already exist in `realm_multi_continuation.rs`).
`is_suspended` deleted in favor of `parked_count`/`parked_ids`. Update the
codegen CLAUDE.md + the parking contract's §3 internal list.

### Phase 3 — harness registry multi-hole (tidepool-harness)

- `Slot::Suspended { machine, holes: Vec<HoleId> }`; `RunningChild` → a
  `Running { parked: Vec<HoleId> }`-style state (a fragment running while N
  holes are parked is the NORMAL state now, not a special child window).
- `checkout_resume(session, hole)`: hole must be a MEMBER; siblings retained
  through the checkout and restore. `checkout_run` on a suspended session:
  permitted (new fragment over parked frames) — this is what re-homed answerer
  turns use. Panic-safety `Drop` restores with the surviving hole set.
- `NodeConvo.pending` plural; `set_pending` pushes; `pending_hole` → top;
  restore logic depth-aware (`harness.rs:2959-2989` keeps its
  read-the-machine-don't-guess argument, over ids instead of a boolean).
- Error variants carry hole sets. Registry unit suite rewritten.

### Phase 4 — the collapse (driver + extract guards)

- Outer session moves INTO the registry (driver holds its SessionId); it gains
  a `SessionLib` decl plane with the pure-decls env + define-time guards
  (above). Answerer nodes map `session_of` → the outer SessionId;
  `terminate_node` becomes realm-scoped for such nodes (Phase 0's
  `drain_realm`, NEVER removing the shared session).
- **Namespace visibility rules (origin-scoped source visibility over one
  heap).** Every session binding/decl carries its ORIGIN (outer-author vs
  answerer/model). Model-authored bindings are importable by later ANSWERER
  fragments (that is the living structure) but are NEVER auto-imported into
  outer render/loop compiles — the authored harness must not silently depend
  on model-authored names (an outer compile that wants one names it
  explicitly, which is a visible act in the authored file). Collision inside
  the model namespace keeps the existing generational shadowing (newest gen
  wins); a model binding colliding with an outer-author name is REJECTED at
  define time, not shadowed. Model bindings survive answerer-node retirement
  by design (they belong to the session, not the node).
- Answerer turns compile exactly as today (`turn_target(Some((ty, imports)))`,
  per-type effects dir, anchored template) but RUN as parked fragments on the
  outer machine under the node's RealmId. askUser/note/fork suspensions park
  beside the loop's frame; the driver services them by id.
- Finalize delivery: function-typed contract → heap-value resume of the loop's
  frame with the finalized closure's root (Phase 0.2); data contracts may keep
  the bridged-value path or go heap-direct (same heap now) — transcript logging
  stays `value_to_json` with an opaque stub for closures.
- Extract: `checkRunLLMTurnType` admits pure arrows (still rejects `M`/`Eff`
  occurrences and polymorphism); `mkBoundBinders` row-mention guard; decl-plane
  import guard. The R0 comment rewritten to state the new rule.
- Fork children: UNCHANGED (own conversations, parallel fanout, answers
  compiled as source against the suspended target session). **A
  function-valued contract row EXCLUDES `Fork` in v1**: the row builder drops
  the Fork decl when the contract type contains an arrow, so an answerer for a
  higher-order hole structurally cannot invoke a verb whose delivery path
  can't carry its result. (The crossing itself is not the blocker — a child's
  `resume expr` compiles against the shared session, so a child-authored
  lambda is born in the right heap; what breaks is `run_child`'s result
  plumbing round-tripping through a bridged Rust `Value`, where a closure
  becomes `CLOSURE_SENTINEL`. Generalizing heap-direct delivery to fork
  answers is Phase 6, plumbing not architecture.)
- Acceptance: the promoted fn-finalize spike (two cycles,
  `finalize @(State -> State)` composing across loops); living-structure test
  (helper/value bound in loop N referenced in loop N+2); guard tests (M-typed
  decl and M-nested bind rejected loudly, attributed to the model's turn).

### Phase 5 — companion + docs + the lifetime boundary

Companion loop moves to `runLLMTurn @(State -> State)`; render gains the
legible-loss line for restart (living structure is not durable). CLAUDE.md
rewrites (harness, codegen, runtime session, repl if touched); retire
`ChildSuspended` prose everywhere.

**Machine lifetime (resolves the contract conflict — see Locked decision 2 as
amended).** The parking contract's §2(c) ("an immortal unified machine is
out") is KNOWINGLY SUPERSEDED for the self-harness consumer, because living
structure across loops requires the machine to outlive loops — that is the
feature. The same commit that lands the consumer amends the contract:
"cycle" for this consumer = one loop for answerer realms (drained at loop
end, complying as written); the machine itself is bounded not by cycles but
by an ENFORCED CEILING — a hard fragment-count/RSS bound at which the driver
refuses the next loop with a legible "machine at capacity: bounce or rebirth
required" error instead of growing silently. Instrumentation (fragment count,
machine VSZ/RSS at loop boundaries) tunes the bound and informs a future
rebirth policy; it does not substitute for the bound existing from day one.

### Phase 6 — higher-order-compatible delegation

Generalize heap-direct answer delivery to fork children (`run_child` result
plumbing stops round-tripping closures through bridged `Value`), then restore
`Fork` to function-valued contract rows. Separate from the collapse landing
by design (codex review 2026-08-13): the no-fork spike proves the
architecture without coupling it to fanout plumbing.

## Locked decisions (interviewed 2026-08-13; do not re-derive)

1. **Resume order: ANY-ORDER at every layer.** Session and registry accept a
   resume for any parked hole by id, matching the machine contract. The driver
   behaves LIFO today as a matter of flow, not enforcement. This keeps N
   concurrently-open operator forms / interleaved holes representable later.
2. **Machine lifetime: enforced ceiling now, policy from evidence.** (Amended
   after codex review 2026-08-13 — instrumentation alone does not discharge
   the parking contract's §2(c).) A hard fragment-count/RSS ceiling with a
   legible refusal exists from the first collapsed build; instrumentation
   tunes it and informs a future rebirth policy. The contract is amended in
   the consumer-landing commit (see Phase 5). No speculative K-loop or
   compaction-coupled rebirth.
3. **Outer session ownership: registry slot.** The outer session moves into
   `SessionRegistry`; the driver holds its SessionId. Uniform checkout/restore
   discipline and panic-safety `Drop` apply to it like any node session;
   answerer nodes map `session_of` to it.
4. **Scope: self-harness only.** Loop + per-loop answerer share the outer
   session. Fork/fanout children and general Agent nodes keep their own
   sessions (parallel fanout needs multiple machines; trust domains stay
   isolated); their answers keep crossing as source.

## Standing constraints

- Every intermediate commit green; the realm adversarial suites + repl suite
  are the oracles. GC-adjacent changes run under
  `TIDEPOOL_GC_POISON`/`TIDEPOOL_HEAP_VERIFY`.
- `turn_target` is the ONLY way an effects dir enters an include path (two
  `Tidepool.Effects` roots on one path resolve silently by search order).
- The parking contract's consumer API (§1/§2/§4) is frozen; §3 internals may
  churn. Slot removal updates §3 and the invariant (b) prose.
