# One session: the self-harness as a single VM

**Goal.** `runLLMTurn @(State -> State)` (and function-valued answers
generally) work end-to-end: the answerer finalizes a closure, the loop applies
it. The mechanism is architectural, not a transport: answerer turns run as
parked fragments ON the outer session, so a finalized closure is born in the
loop's own heap. Alongside, the legacy single-slot suspension path is fully
removed — the parked continuation registry becomes the ONLY suspension
mechanism, so the slot/registry mixing hazard class is unrepresentable.

**The design in one sentence.** The collapsed self-harness is a
single-threaded VM with **structured concurrency** (frames are tasks, realms
are scopes), an **embedder handle API** (values move as handles; serialize
only to observe), **image/source duality** (the machine is a bounded cache
over durable source; rotation replaces immortality), namespaced by **its own
module system** (visibility is imports, not tags).

Each pillar is a proven pattern chosen because it DELETES special cases a
naive design accretes (an atomic-transfer op, a realm-drain bolt-on, a
fork-exclusion for higher-order rows, per-binding origin tags, a
memory-ceiling-as-apology). A 2026-08-13 external review surfaced those as
four High findings against the first draft; the pillars are the principled
resolutions.

## Ground truth (mapped 2026-08-13; anchors are file:line at that date)

Layer status:

- **Machine** (`tidepool-codegen/src/jit_machine.rs`) — the substrate is DONE.
  Registry: `continuations: HashMap<ContinuationId, ContinuationFrame>` (:722),
  each frame a registered GC root for its whole parked lifetime
  (`park_continuation` :3300, receipt `stowed_roots_count() == parked_count()`
  at every mutation). Resume by identity, any order; frame replays its own
  `suspend_tag`/`kind`/table/cancel-flag (`resume_parked` :3471). Adversarially
  tested (`tests/realm_multi_continuation.rs`: parent+child parked
  simultaneously, both resume orders, 8 parks GC-interleaved resumed shuffled,
  all under `TIDEPOOL_GC_POISON`+`TIDEPOOL_HEAP_VERIFY`). Contract-frozen
  (`plans/post-restart/realm-lanes/continuation-parking-contract.md`). Zero
  consumers above codegen. The SLOT path (`suspended_continuation` :654) is
  protected only by a TEMPORAL argument (no GC while suspended, L7 asserts) —
  not rooted. The two paths never mix (asserted both directions), which forces
  wholesale conversion.
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
  `outer.session.resume(&hole, answer)` (:1197-1200); the eager bridge
  substitutes `CLOSURE_SENTINEL` for closures — the eager bridge is the
  closure-killer everywhere it appears, which is what pillar B removes.

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
  rows — "which fragment is running" is the caller's bookkeeping.
- `__selfHarnessState` and the author-module import are per-FRAGMENT (helpers
  splice + include path); an answerer fragment cannot name them, and
  `Harness.hs` does not even compile under the answerer row (its
  `Tidepool.Harness` import pins `M`). No leakage, no shadowing.
- Union tags are baked per-fragment by GHC's `Member`; the machine reads the
  tag out of the heap per request. Nothing correlates tags across fragments.

The two real cross-row hazards (structural guards in Phase 4):

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

## The four pillars

### A. Structured concurrency — frames are tasks, realms are scopes

The parked-continuation registry is a single-threaded executor's task set;
what it lacked was the ownership discipline. Adopt the nursery/scope model
(Trio, Kotlin): every parked frame is OWNED by a scope (realm); a scope's
frames cannot outlive it, by construction. The machine grows the one op every
structured-concurrency runtime has — **scope exit** (`close_realm`): all
frames owned by the realm removed, their stowed roots deregistered, untaken
finalized roots handled per the existing `take_finalized_root` doc, sibling
realms untouched, rooting receipt true before and after. Answerer-node
retirement IS scope exit; the driver never loops over frame ids.

Deletes: the bespoke `drain_realm` bolt-on framing, the retirement-leak
protocol as caller diligence, and the finalized-root ordering question (a
task's result is delivered to its awaiter before its scope closes — the scope
cannot exit while a delivery is in flight, machine-enforced).

### B. The embedder handle API — values move as handles; serialize only to observe

Every embeddable VM (JNI, V8, Lua) converged on the same shape: the host holds
opaque, Send-able **handle ids** in a registry; the VM's values never eagerly
serialize into host types; serialization happens only at observation
boundaries. Our session layer is the embedder API of the JIT VM, and its
defect is eager bridging: every result round-trips through a Rust `Value`,
which is exactly where closures become `CLOSURE_SENTINEL`.

The rule: **delivery by handle, observation by serialization.**

- `ValueHandle` — an opaque `u64` id (Send), minted by the machine, mapping to
  a machine-side registered root. The `!Send` `RootSlot` never crosses a
  thread boundary or an API layer.
- A completed/finalized computation's result is a handle. `resume(hole,
  handle)` delivers machine-internally — no materialization, no bridge, works
  identically for an `Int` and a closure. Delivery consumes the handle (the
  value is now referenced by the resumed continuation).
- Observation — transcript logging, JSON render, operator display — is
  `session.observe(handle) -> Json`, the ONE place the tolerant bridge and its
  closure stub remain, where they are honest (an opaque view of an opaque
  value), never lossy delivery.
- Handles are SCOPE-OWNED (pillar A), like JNI local refs in a local frame:
  scope exit releases the scope's unconsumed handles. Handle leaks are
  structurally impossible. A value that must outlive its scope is delivered
  (consumed into a continuation) before exit — which is the only reason one
  ever should.

Deletes: the special-cased atomic `resume_parked_with_finalized` op (delivery
by handle is just resume); the Fork exclusion for higher-order contract rows
(a fork child's answer handle carries a closure as naturally as an `Int`, so
higher-order delegation needs no separate mechanism); `CLOSURE_SENTINEL` on
every delivery path; `take_last_bound_root`-style machine-level stashes
(a bind's result is a handle in the outcome).

### C. Image and sources — the machine is a cache; rotation, not immortality

Cranelift never frees executable memory, so a machine that lives forever grows
forever — the parking contract's §2(c) rightly calls an immortal unified
machine a NO-GO. The proven answer when the allocator cannot free is **arena
rotation with replay from durable truth** (Smalltalk image/sources, Erlang
code purge). And the durable truth already exists: the decl plane is source on
disk (`Lib.G<g>.hs` + DeclLog), the value plane has defining structure
(`Val.G<g>` + the bind log), durable `State` has the generation-tagged
checkpoint.

Declare it: **source is the durable representation of computation; the machine
is a bounded cache over it.** Rotation = build a fresh machine, replay the
planes and checkpoint against it, retire the old machine wholesale (heap and
roots reclaimed; leaked code arena is per-machine and now bounded by rotation
cadence). What cannot replay — runtime-only values with no defining source —
is ENUMERATED at rotation and surfaced to the companion in `render`: legible
loss as a mechanism, not a prompt nicety.

This dissolves the contract conflict rather than superseding it: there is no
immortal machine, there is a rotating one. The rotation trigger is an enforced
bound (fragment count / RSS) present from the first collapsed build;
instrumentation tunes the bound and cadence from dogfood evidence. The
contract's §2(c) prose is amended in the consumer-landing commit to name the
rotating-consumer pattern; "cycle" for answerer realms = one loop (they comply
as written, closed at loop end by pillar A).

### D. Visibility is imports — the module system is the namespace mechanism

No per-binding origin tags. Haskell already has the visibility mechanism —
modules and imports — and the planes are already generated modules. Each
compile SURFACE composes its own preamble:

- Answerer fragments auto-import the session's model-plane modules (that is
  the living structure: a helper bound in loop N is in scope in loop N+2).
- Outer render/loop compiles import ONLY the authored harness module and the
  state splice — never the model plane by default. The authored file can
  reach a model binding only by writing an import, a visible act in a
  reviewed file. (This is already true mechanically — `compile_outer` adds no
  session imports — the pillar makes it a stated rule instead of an
  accident.)
- Collisions are what modules make them: the model plane keeps generational
  shadowing within itself (newest gen wins, existing machinery); it cannot
  shadow the authored module because the authored module is imported
  qualified (`Loaded.*`) and the turn preamble controls what is unqualified.

Deletes: origin tags on `BindingEntry`, a define-time collision policy, and
the entire shared-namespace concern as a new mechanism — it is preamble
composition, which exists.

## Locked decisions (interviewed 2026-08-13; do not re-derive)

1. **Resume order: ANY-ORDER at every layer.** Session and registry accept a
   resume for any parked hole by id, matching the machine contract. The driver
   behaves LIFO today as a matter of flow, not enforcement. Keeps N
   concurrently-open operator forms / interleaved holes representable.
2. **Machine lifetime: rotation with an enforced bound (pillar C).** The bound
   exists from the first collapsed build; its value and the rotation cadence
   are tuned from instrumentation (fragment count + machine RSS/VSZ per loop
   boundary). No speculative K-loop or compaction-coupled policy.
3. **Outer session ownership: registry slot.** The outer session moves into
   `SessionRegistry`; the driver holds its SessionId. Uniform checkout/restore
   discipline and panic-safety `Drop` apply to it like any node session;
   answerer nodes map `session_of` to it.
4. **Scope: self-harness only.** Loop + per-loop answerer share the outer
   session. Fork/fanout children and general Agent nodes keep their own
   conversations and (in the general tree) their own sessions; within the
   collapsed self-harness their compute serializes on the one machine —
   accepted. Multi-trust-domain harnesses keep per-node isolation.

## Phases

Each phase lands green (full quick tier + named GHC-heavy legs + the realm/GC
adversarial suites) before the next starts. Deletion comes AFTER conversion so
every intermediate commit builds.

### Phase 0 — machine completion (tidepool-codegen only)

1. `ParkKind::Project { n_fields }` / `ParkKind::Render { field0_forced }` +
   matching `ParkedOutcome` completion products, returned INLINE like
   `bound_root` (the slot path already returns Project/Render roots inline —
   same `!Send` discipline; `plans/unpark/suspendable-materialization.md`
   §6.2).
2. **The handle registry (pillar B, machine half):** `ValueHandle` minting
   over machine-side roots; completed/finalized results exposed as handles;
   `resume_parked` accepts a handle as the answer (machine-internal delivery,
   no materialization); `observe(handle)` as the one bridging seam. The
   existing `take_parked_finalized_root`/`finalized_root` plumbing is absorbed
   into this (a finalize frame's payload is a handle like any other result).
3. **Scope exit (pillar A):** `close_realm(realm)` with the guarantees stated
   under pillar A, receipt-asserted. Handles are realm-owned; close releases
   unconsumed ones.
4. Extend the realm adversarial suite under poison/verify: Project/Render
   parks; handle-delivery of a closure into a sibling frame with forced GC
   between mint and consume; `close_realm` with parked finalize frames +
   unconsumed handles; receipts throughout.

### Phase 1 — runtime session conversion (tidepool-runtime)

- `persistent.rs`: all five run/resume pairs route through the parked family.
  Results become handles in outcomes; observation happens at the session's
  existing render/JSON seams. `is_suspended()` → count/ids. Suspend threshold
  stays per-session for now (both rows agree at 0) but is ASSERTED against
  the frame's recorded tag.
- `resident.rs`: `pending: Option<String>` → insertion-ordered map
  String→ContinuationId. `run`/`run_bind` while suspended: ALLOWED (this is
  the point). `run_child`/`run_child_pure` dissolve into plain fragment runs
  (a "child" is an ordinary fragment while frames are parked; it MAY suspend
  and park). `apply_finalized` reshapes over handles. The :796 boolean
  reconciliation becomes a parked-ids comparison (the wedge guard survives,
  depth-aware). `ChildSuspended` deleted; `WrongContinuation` plural;
  `ResidentOutcome` carries ids/handles.
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

- `Slot::Suspended { machine, holes }`; a fragment running while N holes are
  parked is the NORMAL state, not a special child window (`RunningChild`
  generalizes accordingly).
- `checkout_resume(session, hole)`: hole must be a MEMBER; siblings retained
  through the checkout and restore. `checkout_run` on a suspended session:
  permitted (new fragment over parked frames) — what re-homed answerer turns
  use. Panic-safety `Drop` restores with the surviving hole set.
- `NodeConvo.pending` plural; `pending_hole` → top; restore logic depth-aware
  (`harness.rs:2959-2989` keeps its read-the-machine-don't-guess argument,
  over ids instead of a boolean). Error variants carry hole sets. Registry
  unit suite rewritten.

### Phase 4 — the collapse (driver + extract guards)

- Outer session moves INTO the registry (driver holds its SessionId); it gains
  a `SessionLib` decl plane with the pure-decls env + define-time guards
  (Ground truth). Answerer nodes map `session_of` → the outer SessionId;
  `terminate_node` for such nodes is `close_realm`, NEVER removing the shared
  session.
- Answerer turns compile exactly as today (`turn_target(Some((ty, imports)))`,
  per-type effects dir, anchored template) but RUN as parked fragments on the
  outer machine under the node's RealmId. askUser/note/fork suspensions park
  beside the loop's frame; the driver services them by id.
- Finalize delivery: the answerer's finalize payload handle is resumed into
  the loop's frame — one path for data and closures alike (pillar B).
  Transcript logging via `observe` (closures render as the opaque stub).
- Preamble composition per surface (pillar D): answerer fragments auto-import
  the model plane; outer compiles never do.
- Extract: `checkRunLLMTurnType` admits pure arrows (still rejects `M`/`Eff`
  occurrences and polymorphism); `mkBoundBinders` row-mention guard;
  decl-plane import guard. The R0 comment rewritten to state the new rule.
- Acceptance: the promoted fn-finalize spike (two cycles,
  `finalize @(State -> State)` composing across loops); living-structure test
  (helper/value bound in loop N referenced in loop N+2 by an answerer
  fragment, and INVISIBLE to the outer compile); a higher-order FORK answer
  (child finalizes a function to the answerer — pillar B makes this the same
  mechanism); guard tests (M-typed decl and M-nested bind rejected loudly,
  attributed to the model's turn).

### Phase 5 — rotation, companion, docs

- **Rotation (pillar C):** the enforced bound + the rotate operation (fresh
  machine, replay planes + checkpoint, retire old machine, enumerate
  non-replayable values into `render`'s legible-loss line). Instrumentation
  (fragment count, RSS/VSZ per loop boundary) lands with it. Contract §2(c)
  amended in the same commit.
- Companion loop moves to `runLLMTurn @(State -> State)`.
- CLAUDE.md rewrites (harness, codegen, runtime session, repl if touched);
  retire `ChildSuspended` prose everywhere.

## Standing constraints

- Every intermediate commit green; the realm adversarial suites + repl suite
  are the oracles. GC-adjacent changes run under
  `TIDEPOOL_GC_POISON`/`TIDEPOOL_HEAP_VERIFY`.
- `turn_target` is the ONLY way an effects dir enters an include path (two
  `Tidepool.Effects` roots on one path resolve silently by search order).
- The parking contract's consumer API (§1/§2/§4) is frozen; §3 internals may
  churn. Slot removal updates §3 and invariant (b); rotation amends §2(c) —
  both deliberate, in the commits that land the corresponding phase.
