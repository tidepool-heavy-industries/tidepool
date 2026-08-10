# Lane 1 → lanes 2–5 routing handoff (agent-core)

Written at agent-core's fold. Lane 1 (one-cycle coupled-spawn vertical) is
built; this routes what it LEARNED into the deferred lanes without building
any of them. Lane charter references: inheritance-for-agent-core.md §6
("Scope: agent-core is LANE 1 ONLY") — (2) coupled-spawn failure/saga matrix,
(3) durable poke/interrupt ordering, (4) cross-cycle detach/reattach +
tool-handler wakeup, (5) mailbox/staleness seam.

## What lane 1 froze (provisional) that later lanes inherit or must revisit

- **`OneCycleBackend` is deliberately too narrow to survive lane 3/4.** It
  has exactly `start_thread` + `run_cycle` (sync, blocking). Pokes need
  steer/interrupt verbs; reattach needs resume-by-thread-id; the mailbox
  needs an event stream rather than a run-to-completion call. Route: widen
  by REPLACING this trait at the seam, not by accreting optional methods —
  the lane-1 trait is a vertical proof, not an API to preserve. The codex
  adapter's internals (Session, drive_turn's continuous read loop) are
  reusable; the trait shape is not.
- **The saga stage vocabulary held up.** Allocating → WorktreeReady → Bound
  → ThreadAccepted → Running, with the stage carried on every SpawnError,
  was enough to write every failure test unambiguously and to make the
  rollback decision table mechanical (see next section). Lane 2's failure
  matrix should keep stages as the row axis.
- **`Subagent` GADT name, `spawnAgent*` helper names.** `Tidepool.Agent` the
  module is still the harness answerer's (inheritance §8); the rename to
  PRD 18's public `Tidepool.Agent` surface remains root's call with the
  harness owner. Nothing in lane 1 blocks it — the def is one block in
  effect_defs.rs and the wire types are `Ag*`-prefixed, so a rename is
  mechanical.
- **Synchronous one-call surface.** `spawnAgent` blocks to completion. The
  async handle surface (`waitAgent`, events) is where lanes 3–5 live;
  when it lands, the lane-1 call becomes the library combinator
  (spawn+wait), per the PRD's "synchronous delegation is a combinator"
  rule — do not keep two primitives.

## Rollback semantics — the decision lane 2's matrix builds on

Retain-first (locked, PRD 19) forces the interpretation: **rollback settles
the binding; it never deletes the worktree.** The rolled-back end state is
worktree retained + registered + UNBOUND; "orphaned" means "left
Active-bound to an agent that will never run", not "exists". Proven from
reopened disk state in `tidepool-agent/tests/spawn_saga.rs` and end-to-end in
`tidepool-handlers/tests/subagent_one_cycle.rs`.

Decisions inside that, for lane 2 to keep or consciously revise:

- **Success settles `Terminal`; failure-rollback settles `Released`.** The
  finished-vs-stopped-waiting distinction is binding.rs's own; lane 1 maps
  cycle-completion to Terminal because a one-cycle agent is terminal by
  construction. Lane 4 (agents outliving cycles) breaks that equation: a
  completed CYCLE is no longer a completed AGENT, so settle-on-success must
  move to wherever agent-terminal is actually observed.
- **A failed settle on the SUCCESS path is `RollbackFailed`.** It is the
  enum's only signal that an Active binding may remain on disk. Lane 2's
  matrix needs a row for "work succeeded, bookkeeping failed" — it is not
  the same failure class as "work failed".
- **A created worktree is retained even when the spawn fails at
  ThreadAccepted.** Deliberate consequence of retain-first. If lane 2 wants
  fail-fast worktree reuse ("retry the spawn into the tree we just made"),
  `SpawnWorkspace::Existing` already expresses it — the retry spawns into
  the retained tree; nothing new needed.
- **The failure/saga MATRIX itself (lane 2) is bigger than lane 1's tests:**
  binding-persist failure mid-bind (covered in tidepool-worktree's own
  gates), backend death BETWEEN start_thread and run_cycle, double-spawn
  races on one Existing worktree (BindingTable's flock + bind refusal
  handles the enforcement; the matrix should pin the ERROR SHAPE races
  produce), and rollback-failure cascades. Lane 1's
  `rollback_failure_reports_both_causes` gate (EISDIR obstruction, root-safe)
  is the injection pattern to reuse.

## The encoding-polarity split is now two committed codecs — keep it two

- `Tidepool.Agent.CodecSpike` — positional `{tag, fields}` — Tidepool↔Tidepool
  ONLY (gate 1(b) proof).
- `Tidepool.Agent.ModelCodec` — named-field objects + tag-discriminated sums,
  schema/decoder/encoder from ONE traversal — everything a MODEL reads or
  writes. Machine-checked pins live in its haddock (schemas render
  key-sorted; tag spelled `{"enum": [..]}` not `const`; decode tolerates
  unknown keys while the schema says `additionalProperties: false` — the one
  deliberate asymmetry, reasoned in its header).

Routing: lane 4's reattach-supplies-tools needs tool INPUT decoding on the
same polarity — live tool arguments are named-field objects (wire caveat,
confirmed again by the fixtures). `ModelCodec`'s field machinery is the
substrate; `compileTools`' input side should consume it rather than grow a
third traversal. When Chain A lands lossless vendored sums, re-evaluate
whether ModelCodec's generic hierarchy can shrink onto it — but the SCHEMA
side stays regardless (the vendored path has no schema emitter).

## Receipts and observability learnings

- **Exact-model discipline works and is cheap.** `ModelPolicy` resolves at
  the backend; the receipt carries the exact slug (`mock-model-0` included),
  and the codex adapter's ALLOWLIST resolution makes the banned tier
  unreachable by construction rather than by skip-branch. Keep the allowlist
  pattern when the model vocabulary grows (open decision 3).
- **Usage counters are NOT in lane-1 receipts.** They ride
  `thread/tokenUsage/updated` notifications that `drive_turn` currently
  discards. Surfacing them needs the event-projection layer — which is the
  same layer lane 5's staleness observations and lane 3's lifecycle events
  need. Route: build ONE projection from the app-server notification stream
  to seam events (`RuntimeAgentEvent` already has the variants), and let
  receipts, mailbox wakeups, and staleness all read it. Documented in
  lane1-live-leg.md "Known gaps".
- **`SpawnResultMalformed` is produced ONLY Haskell-side** (the decoder), and
  the Rust wire map has no arm for it. Keep that ownership split when the
  poke/result surfaces grow — "malformed" is a fact about the caller's type,
  which only the Haskell side knows.

## Concrete watch-items for the deferred lanes

- **Lane 3 (pokes):** spike 2's second question is still THE open risk —
  does a parked `item/tool/call` block `turn/interrupt`? Lane 1 never parks
  (no tools), so it produced no evidence either way. Do not let lane 3
  design past it; it is a one-day measured answer with the existing Session
  + fixtures machinery.
- **Lane 4 (reattach):** `AgentId` is an in-process u64 mint in
  `CoupledSpawner`. Durable `AgentReference` = stable id + protocol
  fingerprint (locked decision); the mint must move to durable storage
  BEFORE anything checkpoints an id. The binding's `AgentRef` string
  (`agent-<id>-<label>`) is currently derived from the in-process id — a
  durable mint changes those strings' uniqueness story; the binding table
  itself doesn't care (opaque), but post-mortem joins will.
- **Lane 4 (row/vocab):** Subagent's generated types reference Worktree's
  (SpawnError embeds WorktreeError), so Subagent-without-Worktree rows don't
  compile. Harmless now; when the public surface freezes, either keep the
  coupling explicit (coupled spawn IS a worktree fact) or split the error
  type. Related: the L4 `(<|>)`/helper-emission story means any new
  Subagent operators must go through `emits_helpers_for`, never a
  restatement.
- **Lane 5 (mailbox):** nothing in lane 1 touched arrival-schedules-a-cycle.
  The one relevant fact: SubagentHandler is cycle-scoped like
  RepoEventHandler (not Clone, owns flocked state) — a durable mailbox
  cannot live in a handler; it needs the driver-owned home the PRD already
  specifies.

## First contact between the Haskell surface and the generated module

`Tidepool.Agent.Spawn` was authored and typechecked against a SCRATCH stub of
the frozen def signatures (no generated `Tidepool.Effects` exists in-tree);
the acceptance harness was its first contact with the REAL generated module
on the real extract/JIT. Result: **zero fixes required, on either side** —
the def block, the wire types, ModelCodec's pins, and Spawn.hs all agreed on
first contact, and the same held for the handler dev's saga-integration gates
(written blind against the frozen signature, green with zero edits at the
fold). Two waves of independent authors converging on a frozen contract
without a single reconciliation edit is the strongest evidence this lane
produces that freeze-first-then-fork is the right shape for lanes 2–5.

The acceptance gates (tidepool-handlers/tests/subagent_one_cycle.rs, all
passing by name on the real JIT at `c113278b`):
`typed_result_round_trips_through_the_real_jit` (2.19s),
`backend_failure_surfaces_as_typed_spawn_error_and_rolls_back` (7.87s —
includes the drop-session/reopen-from-disk rollback proof),
`malformed_payload_is_a_typed_decode_failure_never_a_success` (2.22s),
`unstructured_payload_is_typed_malformed` (2.22s),
`schema_reaches_the_backend_named_field_shape` (2.22s — pins that the schema
reaching the backend is ModelCodec's NAMED-FIELD shape, the wire-caveat gate).
Durations are plausible against the work claimed (real GHC → extract → JIT →
temp git repository per gate).

## Live leg

`plans/post-restart/agent-lanes/lane1-live-leg.md` documents the
human-triggered run (invocation, preconditions, model policy, printed
receipts, stop-and-hold, one-attempt rule). Zero live calls exist in
anything that runs on invocation; the three pre-existing `#[ignore]`d live
tests in `tidepool-agent` keep their attributes.
