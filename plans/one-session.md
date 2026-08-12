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
2. `ResumeInput` gains an in-heap variant (resume a parked frame with an
   already-tenured heap value by RootSlot, bypassing response
   materialization) — this is how a finalized CLOSURE is delivered into the
   loop's parked continuation on the same heap. Data answers keep the existing
   materialization path.
3. Extend the realm adversarial suite: Project/Render parks under poison/verify;
   heap-value resume of a closure into a sibling frame.

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
  `terminate_node` becomes realm-scoped for such nodes (cancel realm, drop its
  parked frames, NEVER remove the shared session).
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
- Fork children: UNCHANGED (own sessions, parallel fanout, source-crossed
  answers). The collapse is self-harness-scoped; multi-trust-domain harnesses
  keep per-node isolation.
- Acceptance: the promoted fn-finalize spike (two cycles,
  `finalize @(State -> State)` composing across loops); living-structure test
  (helper/value bound in loop N referenced in loop N+2); guard tests (M-typed
  decl and M-nested bind rejected loudly, attributed to the model's turn).

### Phase 5 — companion + docs

Companion loop moves to `runLLMTurn @(State -> State)`; render gains the
legible-loss line for restart (living structure is not durable). CLAUDE.md
rewrites (harness, codegen, runtime session, repl if touched); retire
`ChildSuspended` prose everywhere; memory instrumentation (fragment count,
machine VSZ/RSS at loop boundaries) feeding the rebirth policy decision.

## Locked decisions (interviewed 2026-08-13; do not re-derive)

1. **Resume order: ANY-ORDER at every layer.** Session and registry accept a
   resume for any parked hole by id, matching the machine contract. The driver
   behaves LIFO today as a matter of flow, not enforcement. This keeps N
   concurrently-open operator forms / interleaved holes representable later.
2. **Machine lifetime: instrument now, decide later.** Phase 5 logs fragment
   count + machine RSS/VSZ at loop boundaries; operator bounce remains the only
   rebirth until dogfood evidence picks a policy. No speculative K-loop or
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
