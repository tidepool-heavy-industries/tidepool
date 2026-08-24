# One session: the self-harness as a single VM

**STATUS (2026-08-12): Phases 0–5 LANDED** — commits 153c955f (P0 machine),
89cf6da3 (P1 session lane), 11167061 (P2 multi-hole registry), 90b50724 (P3
the collapse), ab6b1b63 (fn-finalize standing acceptance), c2268375 (P4
rotation), plus the companion promotion and doc rewrites. **Phase 6
(repl/one-shot conversion + slot deletion) is PARKED behind the
production-soak gate** (locked decision 6): the registry path must run the
live harness before the repl surface converts. **LIVING STRUCTURE LANDED
(6e755ac3, post-review):** the shared session's decl plane is installed —
model-defined pure helpers persist by name across loops and rotations
(structural pure-decls guard via the effects-dir-free validation include;
plane transfers through rotation via `take_lib`; end-to-end acceptance
`living_helper_survives_loop_boundary_and_rotation`). Remaining deferred:
restart persistence of the plane (decl-log disk reload), higher-order FORK
answers' own test, and the review's realm-ownership matrix acceptance.

## Executive summary

Today, the model that answers the harness's questions runs in a separate
little world from the loop that asked: its answers cross a boundary that can
only carry data, so a function — an *edit*, a *policy*, a *strategy* — cannot
be an answer, and anything the model builds up while thinking is thrown away
at the end of every loop. This plan removes that boundary. The loop and the
model's cognition run on ONE resident virtual machine with one heap: the model
can finalize `State -> State` — or a record of functions, capturing anything
it has in scope — and the loop applies it directly; and pure helpers the
model DEFINES persist by name across loops and machine rotations (the
living library, on the shared session's decl plane). What does NOT change is
the safety story: what the model is *allowed to do* was never enforced by the
separate world — it is enforced by the typed effect row each turn compiles
against, and that stays exactly as strong.

The reason this is tractable is that the hardest machinery already exists and
is battle-tested but unused: the JIT machine can already hold many suspended
computations at once, each protected across garbage collections, resumable in
any order (built for an earlier wave, never consumed). The work is to convert
the layers above it onto that machinery, guided by four proven patterns:
structured concurrency (suspended computations are tasks owned by scopes, so
cleanup is automatic and leak-free), an embedder handle API (values —
including functions — move between computations as opaque handles and are only
serialized to be *displayed*, never to be *delivered*), image/source duality
(the machine is a bounded cache over durable source; a deliberate, legible
"rotation" bounds memory instead of unbounded growth), and module-system
namespacing (what each surface can see is decided by imports, the way Haskell
already decides visibility).

Cost and risk are sequenced, not hand-waved: six phases, each landing green
under the existing adversarial GC test tiers before the next starts. The
production REPL and one-shot eval surfaces are untouched until the new path
has soaked under the harness (the legacy suspension path is deleted last, not
first). Memory is bounded by an enforced ceiling with a tested reconstruction
path — losses at rotation are enumerated and shown, never silent. The endgame
demo is the companion dogfood finalizing function-valued answers and
accumulating a living library of its own helpers across a long session.

---

**Goal.** `runLLMTurn @(State -> State)` — and function-valued answers
generally, capturing arbitrary in-scope values — work end-to-end: the answerer
finalizes a closure, the loop applies it, and session bindings (including
closures) persist across loops. No source-crossing restriction, no
self-containment requirement: this is the full feature. The mechanism is
architectural: answerer turns run as parked fragments ON the outer session, so
every value the answerer produces is born in the loop's own heap.

**Non-goals (v1).** Effectful function types (`State -> M State`) as answer
types — the generated `M`/row is nominally per-fragment, so a Kleisli arrow
cannot unify across surfaces without row polymorphism at the seam; pure
arrows capture arbitrary values and cover the design space. Function-valued
fields inside durable `State` — the checkpoint is JSON by design; closures
live in the session plane, not the checkpoint. Collapsing the general tree —
fork/Agent nodes outside the self-harness keep their own sessions (trust
isolation, parallel fanout).

## Ground truth (mapped 2026-08-12; anchors are file:line at that date)

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
  not rooted. The two paths never mix ON ONE MACHINE (asserted both
  directions); different machines may use different paths, which is what makes
  the phased conversion safe.
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
  substitutes `CLOSURE_SENTINEL` for closures. The eager bridge is the
  closure-killer on every DELIVERY path it appears in; observation bridging
  (classification, logging) is fine and stays.

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

The two real cross-row hazards (structural guards in Phase 3):

- **Decl plane is SOURCE, recompiled per fragment** against that fragment's
  `Tidepool.Effects`. An accumulated decl mentioning `M`/`Tidepool.Effects`/
  `Tidepool.Form`/`Tidepool.Fork`/`Tidepool.Harness` poisons every later turn
  on the other row — total failure, misattributed to `Lib.G<g>.hs`. Guard:
  pure-decls-only in the shared plane (keep `ModuleEnv::standalone_default`;
  REJECT hoisted effects-family imports and `M`-typed decls at define time,
  with an error attributed to the model's turn, feeding the corrective retry).
- **Tier-1 value binds with `M` inside the type** (`Int -> M Int`) serialize
  the row into `Val.G<g>.hi`; cross-row injection fails iface-load or types at
  the wrong `Eff`. Guard: reject at `mkBoundBinders`
  (`haskell/app/Main.hs:1478-1484`) any bind whose type mentions a TyCon
  defined in the generated effects module. Pure closures (`State -> State`)
  are row-free and bind fine — including across loops, which is the feature.

## The four pillars

### A. Structured concurrency — frames are tasks, realms are scopes

The parked-continuation registry is a single-threaded executor's task set;
what it lacked was the ownership discipline. Adopt the nursery/scope model
(Trio, Kotlin): every parked frame is OWNED by a scope (realm); a scope's
frames cannot outlive it, by construction. The machine grows the one op every
structured-concurrency runtime has — **scope exit** (`close_realm`): all
frames owned by the realm removed, their stowed roots deregistered, untaken
finalized roots handled, sibling realms untouched, rooting receipt
(`stowed_roots_count() == parked_count()`) true before and after.
Answerer-node retirement IS scope exit; the driver never loops over frame ids.
A result is delivered to its awaiter before its scope closes — machine
enforced, not caller convention.

### B. The embedder handle API — values move as handles; serialize only to observe

Every embeddable VM (JNI, V8, Lua) converged on the same shape: the host holds
opaque, Send-able **handle ids** in a registry; the VM's values never eagerly
serialize into host types on the way to another VM computation; serialization
happens only at observation boundaries. Our session layer is the embedder API
of the JIT VM; its defect is eager bridging on DELIVERY paths, which is
exactly where closures become `CLOSURE_SENTINEL`.

The rule: **delivery by handle, observation by serialization.**

- `ValueHandle` — an opaque `u64` id (Send), minted by the machine, mapping to
  a machine-side registered root. The `!Send` `RootSlot` never crosses a
  thread boundary or an API layer.
- Handles are SCOPE-OWNED BORROWS (JNI local refs in a local frame): using a
  handle (observing, delivering) does not consume it; scope exit releases
  every handle the scope minted. Observe-then-deliver in either order; leaks
  structurally impossible. A value outlives its scope only by being delivered
  into a continuation (heap-reachable thereafter) or bound into the session
  value plane (a persistent root) — both visible acts.
- A completed/finalized computation's result is a handle. `resume(hole,
  handle)` delivers machine-internally — no materialization, no bridge,
  identical for an `Int` and a closure.
- Observation — classification, transcript logging, JSON render, operator
  display — is `observe(handle) -> Json` through the tolerant bridge, the ONE
  place the closure stub remains, where it is honest (an opaque view of an
  opaque value), never lossy delivery.

Passing functions around IS this API: the driver, the harness, and authored
Haskell exchange opaque handles whose payloads may capture anything; only
display flattens, and display says so.

### C. Image and sources — the machine is a bounded cache; rotation, not immortality

Cranelift never frees executable memory, so a machine that lives forever grows
forever — the parking contract's §2(c) rightly calls an unbounded immortal
machine a NO-GO. The proven answer when the allocator cannot free is **arena
rotation with reconstruction from durable truth** (Smalltalk image/sources,
Erlang code purge). The durable truth exists: the decl plane is source on disk
(machine-independent already), the value plane has names/types + serializable
payloads for Tier-0 data, durable `State` has the generation-tagged
checkpoint.

Rotation = build a fresh machine; the decl plane simply persists (source);
Tier-0 data values re-inject via JSON; what cannot reconstruct — closures and
other runtime-only values — is ENUMERATED and surfaced in `render`: legible
loss as a mechanism. The rotation trigger is an enforced ceiling (fragment
count / RSS) present from the first collapsed build; instrumentation tunes the
bound and cadence. Because cross-loop closures are the feature, rotation is
RARE and DELIBERATE — but its reconstruction path is exercised continuously in
the test tier from day one (a recovery path that only runs in emergencies is a
broken recovery path). Contract §2(c) is amended in the phase that lands
rotation: answerer realms are cycle-scoped as written (closed every loop); the
machine's lifetime is ceiling-bounded with tested reconstruction.

### D. Visibility is imports — the module system is the namespace mechanism

No per-binding origin tags. Haskell already has the visibility mechanism —
modules and imports — and the planes are already generated modules. Each
compile SURFACE composes its own preamble:

- Answerer fragments auto-import the session's model-plane modules (that is
  the living structure: a helper bound in loop N is in scope in loop N+40).
- Outer render/loop compiles import ONLY the authored harness module and the
  state splice — never the model plane by default. The authored file reaches
  a model binding only by writing an import: a visible act in a reviewed
  file. (Mechanically already true — `compile_outer` adds no session imports —
  promoted from accident to stated rule.)
- Collisions are what modules make them: the model plane keeps generational
  shadowing within itself (newest gen wins, existing machinery); it cannot
  shadow the authored module (imported qualified as `Loaded.*`).

## Locked decisions (2026-08-12; do not re-derive)

1. **Full feature, no transport restrictions.** Closures capture arbitrary
   in-scope values; no source-crossing/self-containment fallback anywhere on
   the self-harness path. (Source-crossing remains what it always was on the
   general tree's fork holes — unrelated to this plan.)
2. **Resume order: ANY-ORDER at every layer; deterministic servicing as a
   REPLAY INVARIANT.** Session and registry accept a resume for any parked
   hole by id. The driver services holes in a deterministic (LIFO) order and
   that determinism is a stated invariant — `ReplayProvider`'s turn
   substitution depends on stable servicing order across versions.
3. **Machine lifetime: enforced ceiling + tested reconstruction; cadence from
   evidence.** No speculative K-loop or compaction-coupled rotation; the
   ceiling exists from the first collapsed build and refuses the next loop
   with a legible error naming rotation as the remedy.
4. **Outer session ownership: registry slot.** The outer session moves into
   `SessionRegistry`; the driver holds its SessionId; uniform checkout/restore
   + panic-safety `Drop`; answerer nodes map `session_of` to it.
5. **Scope: self-harness only.** Within the collapsed self-harness, fork
   children's compute serializes on the one machine (accepted). The general
   tree keeps per-node sessions.
6. **Slot path deleted LAST, after soak.** The registry path must run the
   harness in production before the repl/one-shot surfaces convert. The
   per-machine mixing asserts make coexistence safe in the interim.

## Phases

Each phase lands green (full quick tier + named GHC-heavy legs + the realm/GC
adversarial suites) before the next starts.

### Phase 0 — machine completion (tidepool-codegen only)

1. `ParkKind::Project { n_fields }` / `ParkKind::Render { field0_forced }` +
   matching `ParkedOutcome` completion products, returned INLINE like
   `bound_root`.
2. **The handle registry (pillar B, machine half):** `ValueHandle` minting
   over machine-side roots; scope-owned-borrow semantics;
   completed/finalized results exposed as handles (absorbing
   `take_parked_finalized_root`/`finalized_root`); `resume_parked` accepts a
   handle as the answer (machine-internal delivery, no materialization);
   `observe(handle)` as the one bridging seam.
3. **Scope exit (pillar A):** `close_realm(realm)` with the stated guarantees,
   receipt-asserted; releases the realm's unconsumed handles.
4. Adversarial suite extensions under poison/verify: Project/Render parks;
   handle-delivery of a closure into a sibling frame with forced GC between
   mint and delivery, observe before AND after delivery; `close_realm` with
   parked finalize frames + outstanding handles; receipts throughout.

### Phase 1 — session layer conversion, harness lane (tidepool-runtime)

Strangler-fig, not big-bang: the parked family lands as SIBLINGS in
`persistent.rs`/`resident.rs`; harness sessions route through it; repl and
one-shot stay on the untouched slot path (different machines — legal, safe).

- Multi-pending: `pending: Option<String>` → insertion-ordered map
  String→ContinuationId; outcomes carry ids + handles; `run`/`run_bind` while
  suspended allowed on the parked lane; the :796 boolean reconciliation
  becomes a parked-ids comparison (the wedge guard survives, depth-aware).
- `run_child`/`run_child_pure` dissolve on the harness lane (a "child" is an
  ordinary fragment while frames are parked; it MAY suspend and park);
  `apply_finalized` reshapes over handles; `ChildSuspended` retired from this
  lane.
- **Value-plane generation audit:** bind generations are minted at compile
  time; any-order resume makes out-of-order materialization representable.
  Either prove the gen machinery order-independent or enforce in-order bind
  materialization at the session layer — decided by the audit, pinned by a
  test.
- Oracle: full harness suite + the realm suites; repl suite proves the slot
  lane untouched (byte-identical behavior).

### Phase 2 — harness registry multi-hole (tidepool-harness)

- `Slot::Suspended { machine, holes }`; a fragment running while N holes are
  parked is the NORMAL state (`RunningChild` generalizes).
- `checkout_resume(session, hole)`: member check; siblings retained through
  checkout and restore; `checkout_run` on a suspended session permitted (new
  fragment over parked frames — what re-homed answerer turns use);
  panic-safety `Drop` restores the surviving hole set.
- `NodeConvo.pending` plural; `pending_hole` → top; restore logic depth-aware
  over ids; error variants carry hole sets; registry unit suite rewritten.
- Outer session moves INTO the registry (locked decision 4).

### Phase 3 — the collapse (driver + extract)

- Answerer nodes map `session_of` → the outer SessionId; each per-loop
  answerer gets a RealmId; `terminate_node` for such nodes is `close_realm`,
  never removing the shared session. The outer session gains a `SessionLib`
  decl plane (pure-decls env + define-time guards, Ground truth).
- Answerer turns compile exactly as today (`turn_target(Some((ty, imports)))`,
  per-type effects dir, anchored template) but RUN as parked fragments on the
  outer machine under the node's realm. askUser/note/fork suspensions park
  beside the loop's frame; the driver services them by id (deterministic
  order — locked decision 2).
- Finalize delivery: the payload handle is resumed into the loop's frame —
  one path for data and closures alike. Transcript logs via `observe`
  (closures render as the opaque stub, plus the turn's source is already
  logged — the lambda's text is in `TurnStart{source}`).
- Preamble composition per surface (pillar D).
- Extract: `checkRunLLMTurnType` admits pure arrows (still rejects `M`/`Eff`
  occurrences and polymorphism); `mkBoundBinders` row-mention guard;
  decl-plane import guard; the R0 comments rewritten.
- **Acceptance battery:** the promoted fn-finalize spike (two cycles,
  `finalize @(State -> State)` composing across loops); closures capturing
  session bindings (an answerer binds a value in round 1, captures it in the
  round-3 finalize); CROSS-LOOP living structure (helper + Tier-0 value +
  CLOSURE bound in loop N, used by an answerer fragment in loop N+2, and
  INVISIBLE to the outer compile); a higher-order FORK answer (child
  finalizes a function to the answerer — same handle mechanism); guard tests
  (M-typed decl and M-nested bind rejected loudly, attributed to the model's
  turn); latency instrumentation for per-round compile growth vs plane size.

### Phase 4 — rotation + instrumentation (pillar C)

The enforced ceiling; the rotate operation (fresh machine; decl plane
persists as source; Tier-0 data re-injects via JSON; losses — closures,
effect-derived values — enumerated into `render`'s legible-loss line, which
also covers process restart); fragment-count + RSS/VSZ instrumentation per
loop boundary; the reconstruction path in the CI test tier (exercised
continuously, not only at emergencies); contract §2(c) amended in the same
commit.

### Phase 5 — companion + docs

Companion loop moves to `runLLMTurn @(State -> State)`; prompt work
encouraging decl-plane policies ("standing behavior is a named helper; a
closure is a within-reach value") without forbidding closure bindings.
CLAUDE.md rewrites (harness, codegen, runtime session); retire
`ChildSuspended` prose on converted lanes.

### Phase 6 — repl/one-shot conversion + slot deletion (post-soak)

After the registry path has soaked under the live harness: convert the repl
lane (Project/Render parks from Phase 0.1) and the one-shot ask path; then
delete `suspended_continuation`, `stowed_root_cell`, `last_bound_root`,
`suspended_finalized_root`, the nested-child machinery, the four slot
run/resume families, `ParkTarget::Slot`, the L7 asserts and both mixing
guards; port remaining `nested_child_gc_rooting.rs` scenarios; update the
parking contract §3. The repl suite is the conversion oracle; this phase has
its own soak gate before the deletion commit.

## Risk register (owned, not hand-waved)

- **Continuation-rooting under load** — parked frames surviving many GCs with
  model-authored allocation between: covered by the realm suites + Phase 0.4
  extensions; any new GC-adjacent code runs under poison/verify.
- **Val-gen ordering under interleaved binds** — Phase 1 audit, pinned.
- **Replay determinism** — locked decision 2; a test pins servicing order.
- **Per-round latency growth** (session-scoped compiles are uncacheable;
  include closure grows with the plane): instrumented in Phase 3's
  acceptance; informs decl-plane hygiene, not blocked on it.
- **Blast radius** (a wedged shared machine costs the run, not the loop):
  mitigated by rotation-from-durable-truth being a tested path and by the
  checkpoint restart story; accepted as the price of cross-loop closures.
- **Repl regression risk**: structurally deferred to Phase 6 behind a soak
  gate; until then the repl binary path is byte-untouched.

## Standing constraints

- Every intermediate commit green; the realm adversarial suites + repl suite
  are the oracles. GC-adjacent changes run under
  `TIDEPOOL_GC_POISON`/`TIDEPOOL_HEAP_VERIFY`.
- `turn_target` is the ONLY way an effects dir enters an include path (two
  `Tidepool.Effects` roots on one path resolve silently by search order).
- The parking contract's consumer API (§1/§2/§4) is frozen; §3 internals may
  churn. Slot removal (Phase 6) updates §3 and invariant (b); rotation
  (Phase 4) amends §2(c) — both deliberate, in the commits that land them.
