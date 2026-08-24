# Harness architecture wave — orchestration runbook

Written 2026-08-24 so a downshifted root (Sonnet/Opus) can execute the
remaining wave without re-deriving any decision. Every spec here is
operator-approved; the orchestrator's job is spawn → idle → fold → verify,
in the order below. If a situation arises that this document does not
cover, STOP and ask the operator — do not improvise architecture.

Operator approvals this document encodes (2026-08-23/24):

- **#18** fork children run concurrently.
- **#19** delete (not gate) the test-only branch/snapshot + one-shot surfaces.
- **#17** fork-child failure fails the calling block with a corrective —
  **landed** at `f99d6b89`.
- **#20** suspension effects onto the generated schema plane. Refinement:
  the schema is a pure wire IDL — payload shapes, constructor names, and
  decls are tied to the wire types; **handlers keep full freedom**; no
  dispatch metadata in the schema. Exhaustiveness comes from per-plane
  roster composition.
- **#21** version the harness persistence wire like repr's CBOR format
  (design first).
- **#22** unify repl ask/suspend with harness suspension routing, best
  parts of both, own-crate candidate (design first).
- **#23** composition root moves out of tidepool-web (web = GUI/server only).
- **#24** one-home rule for the model-facing Haskell surface (after #20
  steps 2–3).
- **#25** delete tidepool-lsp.
- Test-architecture standard (folded into #18): provider mocks for
  concurrent paths are keyed by request CONTENT (a distinctive needle in
  the prompt), never FIFO arrival order.

## Hard rules for the orchestrating root

1. Decompose, spawn, fold. Never implement inline; never enter a child's
   worktree; never checkout another branch.
2. Spawn with `mcp__exomonad__spawn_dev` (refuses a dirty worktree — commit
   first). Fold with the `merge` tool, never raw `git merge`, always with
   gate `cargo check --workspace --all-targets`.
3. After folding any lane that touched `tidepool-harness`: run
   `cargo nextest run` (fast tier) at tip before spawning the next lane.
4. Wire bytes NEVER change until #21 lands: the log `Event` enum,
   `Checkpoint` struct, serde tags, journal kinds. A lane note claiming a
   wire change is a fold-blocker — escalate to the operator.
5. The running companion (PID 3711985, http://127.0.0.1:4600) is the
   operator's live test instance. NEVER bounce, redeploy, reseed, or touch
   its memory store (~/.local/share/tidepool/companion-memory) without an
   explicit operator instruction. No `scripts/redeploy.sh` during this
   wave without operator sign-off.
6. No pushes; commits stay on `harness-interaction-surface`.
7. Kill processes only by exact PID after `pgrep -a` inspection (a pgrep
   pattern matches your own shell — known trap).
8. Lane notes go stale against a moving tip: relocate every claim by
   SYMBOL, never trust `file:line` from a report written before a fold.
9. A lane hitting a wall ("the mechanism I need is out of my boundary")
   reports the wall; the answer is widen/extend/re-sequence — never a
   local copy (Mechanism Index, root CLAUDE.md).
10. On every `[READY]`: read the inbox note; check receipts actually list
    the verify commands from the spec; merge with the gate; run the
    post-fold verification for that lane (table at the bottom); mark the
    task completed.

## State when this document was written

- Tip: `f99d6b89` (fork-fail folded; #17 complete).
- In flight, specs already delivered, nothing to respawn — just fold on
  `[READY]`:
  - **surface-cut** (#19): deleting branch/snapshot verbs + the one-shot
    engine (`answer_fork`/`answer_fanout`/`drive_answerer_to_value`),
    purging `fork_child_seq` at retirement, migrating live-coverage
    assertions, updating `scripts/battery-shard.sh`.
  - **lsp-nuke** (#25): deleting tidepool-lsp workspace-wide; STOP-and-
    report if anything live depends on it; protocol golden regen only if
    an lsp-backed effect surface exists.
- Merge-conflict note for surface-cut: it was told to avoid fork-fail's
  regions, but both touched driver.rs. fork-fail's landed changes are:
  `drive_fork_child_agent_session` now returns
  `Result<Result<Value, String>, DriverError>`; new
  `ThreadServiced::ChildFailed` + `GreenRoundExit::ForkChildFailed`
  mirroring the budget-refusal arms; `fork_child_failure_corrective`;
  updated doc comments on `fold_exit` / the pump. On conflict: keep both
  sides — surface-cut only deletes, fork-fail only rewires failure
  delivery. If a conflict is not obviously resolvable that way, leave the
  merge uncommitted, abort it, and ask the operator.

## Execution order

Run strictly in this sequence. Driver lanes run SOLO — never two lanes
touching `tidepool-harness/src/selfharness/` at once.

1. Fold **surface-cut** and **lsp-nuke** as they signal (either order;
   they are disjoint).
2. After surface-cut folds → spawn **fork-concurrency** (#18). Solo.
3. After fork-concurrency folds → spawn **suspension-schema-step-1**
   (#20 step 1). Solo in the harness.
4. After lsp-nuke folds and no other lane holds workspace `Cargo.toml` →
   spawn **composition-root** (#23). May run alongside a harness lane
   (disjoint crates) but not alongside lsp-nuke.
5. After suspension-schema-step-1 folds → spawn the two DESIGN lanes
   **persistence-versioning-design** (#21) and **kernel-design** (#22)
   in parallel (both produce docs, no code conflicts).
6. HELD until an explicit operator ping, even if everything above is
   green: #20 steps 2–3, #24, and any IMPLEMENTATION of #21/#22.
7. End-of-wave gate, after steps 2–4 have folded: walk all
   `tidepool-harness` shard groups sequentially per
   `scripts/battery-shard.sh` (the wave touched the harness throughout),
   plus a final fast tier. Then write the wave report (template at the
   bottom) and stop. Do not redeploy.

---

## Spec: fork-concurrency (#18)

Spawn as `spawn_dev` name `fork-concurrency`, fields as follows.

**task:** Make fork children genuinely concurrent in tidepool-harness
(operator-approved). Today every child model session is awaited inline —
the green scheduler services one complete child session before touching
the next, and direct fork batches serialize the same way — so the latency
promise of `async (fork @T brief)` is unrealized. Represent in-flight
child drives as scheduler-owned futures polled under the existing
concurrency cap. Correctness is currently fine; do not change semantics,
only overlap.

**context:**

ANTI-PATTERNS:
- DO NOT write a second concurrency shell. `drive_concurrent`
  (driver.rs, currently ~line 576: buffer_unordered + index resort under
  a cap) is the ONE home; fork children ride it or an extension of it,
  widened in place if its granularity is wrong.
- DO NOT re-derive budget admission — it is already a compare-exchange
  loop (`check_fork_budgets`), safe under overlap. Do not weaken it.
- DO NOT break the #17 failure contract (landed at `f99d6b89`): a child's
  own semantic failure (`InvocationExit`) surfaces as a block-abort
  corrective at the consumption point via
  `GreenRoundExit::ForkChildFailed` / `ThreadServiced::ChildFailed`;
  sibling results survive; `DriverError` (mechanism failure) stays
  turn-fatal. All of that must hold when children overlap — extend the
  existing tests to the concurrent case rather than replacing them.
- DO NOT change wire bytes (log `Event` enum, `Checkpoint`). Overlapping
  children WILL interleave their journal events — that interleaving must
  replay cleanly with the existing event vocabulary; if you believe a new
  event kind is required, STOP and report.
- Provider mocks for every new/migrated test are keyed by request CONTENT
  (match a distinctive needle in the prompt text), never FIFO order —
  operator-approved standard. Extend the existing test-provider home
  (the ReplayProvider family) with content keying; do not build a twin
  mock type. Migrate any FIFO fixture your change makes order-dependent.
- A new test joins an existing family bundle (`answerer_async_fork.rs`)
  — no new standalone GHC-compiling test binary (suite wall-time rule,
  root CLAUDE.md).

DESIGN NOTES:
- The pump (`drive_agent_session_to_finalize`) and the scheduler both
  take `&mut self` on the driver; overlapping children need their drives
  to own disjoint state. Expect to restructure so each child's future
  owns its per-child session state, with shared driver state reached
  through the existing shared handles (the budget atomics, the gate,
  the registry). If this forces a restructure beyond the scheduler and
  the fork-servicing paths — e.g. you find yourself threading new locks
  through unrelated driver subsystems — STOP and report the design wall
  instead of forcing it.
- Find where the concurrency cap is sourced for `drive_concurrent`'s
  existing callers and use the same source; do not invent a second cap.
- The green scheduler's inline awaits are in the thread-servicing region
  (the `for` loop that drives each ready fork child to completion) and
  the direct-batch arm. Both become "spawn the drive as a pending
  future; deliver on completion", with `wait` consuming from the
  completed set and rotation/sweeps unchanged.

**read_first:** tidepool-harness/CLAUDE.md;
tidepool-harness/src/selfharness/driver.rs (drive_concurrent, the green
scheduler thread-servicing region, drive_fork_children, the #17 corrective
arms); tidepool-harness/tests/answerer_async_fork.rs;
docs/GLOSSARY.md (prompt rules, if any model-facing text changes).

**steps:**
1. Trace both fork paths' current inline awaits and write the
   before-state into your submit note.
2. Restructure child drives into scheduler-owned futures polled under
   the existing cap via `drive_concurrent` (widened in place if needed).
3. Extend the #17 failure tests to the concurrent case (failed child +
   surviving sibling, overlapping).
4. Add the overlap acceptance test: a content-keyed provider mock that
   records max concurrent in-flight requests; assert max-active > 1 for
   a two-child fork batch; assert results still deliver to the right
   `wait`s.
5. Add/extend a journal-replay assertion: run a turn with overlapping
   children, then resume from checkpoint and replay the log cleanly.
6. Migrate any FIFO fixtures the change breaks to content keying.
7. fmt, clippy, verify.

**boundary:** tidepool-harness/src/selfharness/ (scheduler + fork
servicing), tidepool-harness/tests/. DO NOT touch: engine.rs
classification, harness.rs, log Event enum, Checkpoint, tidepool-web,
tidepool-protocol. Out-of-bounds mechanism need = STOP-and-report.

**verify:** cargo check --workspace --all-targets; cargo fmt --all --
--check; cargo clippy --workspace --all-targets (warning set must not
grow); cargo nextest run; scripts/battery.sh -p tidepool-harness -E
'binary(answerer_async_fork) or binary(companion_collapsed_slice)'.

**done_criteria:** overlap test pins max-active > 1; #17 failure contract
holds under overlap (tests); journal interleave replays clean (test);
no new event kinds, no Checkpoint change (grep receipt); no second
concurrency shell or mock twin (submit note names the extended homes);
before-state trace in the submit note.

---

## Spec: suspension-schema-step-1 (#20 step 1)

Spawn as `spawn_dev` name `suspension-schema`, after fork-concurrency
folds. Fields:

**task:** Move harness suspension decoding onto the generated schema
plane (operator-approved, step 1 of 3). Replace `classify_hole`'s
hand-enumerated constructor roster in tidepool-harness/src/engine.rs
with generated typed decode from tidepool-protocol — the ONE approved
schema crate (plans/self-iterating-harness/22-effect-protocol-prd.md;
Exec/Journal/Worktree/RepoEvent already generate from it). The driver
keeps ALL orchestration semantics in its own match arms; only the
decode/typing layer moves.

**context:**

DESIGN PRINCIPLE (operator, 2026-08-24, binding): the schema is a pure
wire IDL. It owns constructor names and payload shapes and generates the
Rust typed decode. It carries NO dispatch metadata — nothing about who
services an effect or how. Exhaustiveness comes from roster composition:
the harness plane declares which suspension effects its stack carries;
the generated decode for that roster is a typed sum; the driver's match
over that sum is exhaustive by construction, so adding an effect to the
roster without servicing it becomes a compile error.

ANTI-PATTERNS:
- DO NOT build a second generator or a parallel schema. tidepool-protocol
  is the one home (Mechanism Index: effect/error type definitions —
  every projection generated, never hand-carried). If its current shape
  can't express something (see the polymorphism note), extend it in
  place or STOP and report.
- DO NOT touch the Haskell decls in step 1. `typed_request_agent_decls`
  stays hand-carried for now (steps 2–3, held). No model-facing prompt
  text changes at all — this step is invisible to the model.
- DO NOT change runtime behavior. Constructors outside the roster must
  still fail loud (today's `ClassifyError::UnsupportedConstructor`
  contract). Byte-compatible migration, one effect at a time, is the
  established PRD 22 pattern — follow it, goldens included.
- Enumerate the CURRENT roster from tip at spawn time (surface-cut will
  have deleted the branch/snapshot constructors) — do not trust any
  older enumeration.

**read_first:** tidepool-protocol/README.md;
plans/self-iterating-harness/22-effect-protocol-prd.md;
tidepool-protocol/src/effects/ (the four landed generated effects — the
pattern to mirror); tidepool-harness/src/engine.rs (classify_hole,
ClassifiedSuspension, SuspensionRouting); tidepool-harness/CLAUDE.md.

**steps:**
1. Inventory the live suspension roster from tip (every constructor
   classify_hole recognizes) with payload shapes; put it in the submit
   note.
2. Declare those payloads in tidepool-protocol following the landed
   per-effect pattern; generate the typed request types + decode; add
   goldens per crate convention.
3. Define the harness plane's roster as a composition of the generated
   effects; generate/derive the roster sum; replace classify_hole's
   body with decode-into-sum; keep the loud unknown-constructor error.
4. Convert the driver's SuspensionRouting consumption to match on the
   typed sum (mechanical; semantics unchanged).
5. fmt, clippy, verify; confirm zero model-facing text changed (diff the
   emitted decl surface before/after — must be byte-identical).

**boundary:** tidepool-protocol/, tidepool-harness/src/engine.rs,
tidepool-harness/src/selfharness/ (mechanical consumption conversion
only), tidepool-harness/tests/. DO NOT touch: Haskell decl text,
tidepool-mcp/src/effect_defs.rs legacy effects, wire bytes,
tidepool-web.

**verify:** cargo check --workspace --all-targets; fmt; clippy (set must
not grow); cargo nextest run; scripts/battery.sh -p tidepool-harness -E
'binary(answerer_async_fork) or binary(companion_collapsed_slice) or
binary(dogfood_harness_typecheck)'; protocol golden tests green.

**done_criteria:** classify_hole's hand roster gone; unknown constructor
still loud (test); decl surface byte-identical (receipt); roster
declared in exactly one place; submit note carries the roster inventory
and names every generated artifact.

---

## Spec: composition-root (#23)

Spawn as `spawn_dev` name `composition-root` per the execution order.

**task:** Make tidepool-web GUI/server-only (operator-approved): move the
`tidepool-selfharness` binary — the composition root assembling driver +
gate + provider + memory store + web server — out of tidepool-web and
into the facade crate (`tidepool/`), or a thin new bin crate ONLY if the
facade's dependency graph makes that genuinely ugly (report which and
why in the submit note). tidepool-web keeps the `OperatorGate` impl, the
rendering/server modules, and its own demo binary.

**context:** ANTI-PATTERNS: do not fork/duplicate any composition logic —
this is a MOVE (`ensure_memory_store`, settings/dial wiring, env
handling move intact). Do not redeploy or restart anything — the running
companion (PID 3711985) must not be touched; the operator redeploys
later. Update every live reference to the binary's crate path:
`scripts/redeploy.sh`, root CLAUDE.md structure listing,
tidepool-web/CLAUDE.md + README, `cargo install` docs (haskell/CLAUDE.md
deploy sections if they name the path). Wire bytes and behavior
unchanged; `TIDEPOOL_WEB_BIND_HOST` handling moves verbatim (never
0.0.0.0).

**read_first:** tidepool-web/src/bin/tidepool-selfharness.rs;
tidepool/ (facade crate layout); scripts/redeploy.sh;
tidepool-web/CLAUDE.md.

**steps:** move the binary + its wiring; fix deps (facade gains
tidepool-web/harness deps as needed — check for cycles); update scripts
+ docs; build the binary from its new home; fmt/clippy/verify.

**boundary:** tidepool/, tidepool-web/, scripts/redeploy.sh, docs
listings. DO NOT touch: tidepool-harness source, the running processes,
anything the harness lanes own.

**verify:** cargo check --workspace --all-targets; cargo build -p
tidepool --bin tidepool-selfharness (or the chosen home) --release;
cargo nextest run -p tidepool-web; fmt; clippy.

**done_criteria:** binary builds from its new home; tidepool-web has no
main-composition binary; scripts/docs reference the new path; no
duplicated wiring (grep receipt); no process was restarted.

---

## Design brief: persistence-versioning-design (#21) — DOC ONLY

Spawn as `spawn_dev` name `persistence-versioning-design`. The lane
produces a design document at `plans/persistence-versioning-design.md`
and NO production code. Operator reviews before any implementation.

**task:** Design version-stamped persistence for the harness wire
(checkpoint + journals), mirroring tidepool-repr's solved CBOR
wire-format versioning. Today the freeze on the log `Event` enum /
`Checkpoint` struct is social discipline, and schema evolution requires
destroying state (the RunSummary→lastAnswer rename is deferred exactly
because of this).

**the document must cover:** how repr's CBOR versioning works (survey);
where the version stamp lives for (a) the checkpoint and (b) the shared
durable-JSONL primitive in tidepool-repr — versioning likely belongs at
that shared home so all four consumers (worktree journal, handlers
journal, harness log, selfharness observer) inherit it, but the doc must
argue this rather than assume it; the migration-ladder shape (N→N+1
functions, composed); the old-corpus replay test plan (a checked-in
corpus of prior-version checkpoints/journals that must load at tip);
which currently-frozen changes it unblocks (enumerate: RunSummary→
lastAnswer, future Event/Checkpoint edits) and the rollout order (stamp
first, first migration second); what happens to a checkpoint OLDER than
the ladder's floor (explicit refusal with a plain message, presumably).

**boundary:** read anything; write ONLY the plans/ doc. Verify: none
beyond the doc existing and being internally consistent.

---

## Design brief: kernel-design (#22) — DOC ONLY

Spawn as `spawn_dev` name `kernel-design`, after suspension-schema-step-1
folds (the typed suspension plane is the vocabulary both sides converge
on). Produces `plans/resident-session-kernel-design.md`, no production
code. Operator reviews before any implementation.

**task:** Design the unified resident-session suspension kernel
(operator: "figure out how to unify this, best parts of both, perhaps in
own crate"). tidepool-repl's ask/suspend (block-runner + single-owned
SessionState lifecycle) and tidepool-harness's suspension routing
(engine + driver, post-#20 typed decode) implement the same concept:
resident machine parks on a typed hole, something external resolves it,
machine resumes.

**the document must cover:** side-by-side survey of both mechanisms
(state machines, ownership, resume typing, checkpoint interaction, error
paths); a "best parts of each" table with justification; the proposed
kernel seam (what the kernel owns vs what stays a policy of repl/
harness); the crate decision — tidepool-runtime vs a new crate — argued
against runtime's utility-attractor problem; precedents to build on
(SessionRegistry consolidation pattern: runtime owns, repl/harness thin
clients); a migration order that keeps both surfaces green at every
step; what the suspension-schema plane (#20) contributes and whether
step 2/3 should land before or after kernel implementation.

**boundary:** read anything; write ONLY the plans/ doc.

---

## Held briefs (do not spawn without an operator ping)

**#20 step 2 — decl generation:** move the hand-carried Haskell decls
(`typed_request_agent_decls` and friends) into tidepool-protocol's
generator. Decl text is model-facing prompt surface: needs golden
coverage of the emitted decl text, a before/after decl diff in the
submit note, and operator awareness that prompt bytes change (cache
implications for live sessions). Held behind step 1 + operator ping.

**#20 step 3 — polymorphic verbs:** schema support for verbs whose
response type is bound at the invocation site (`fork @T`,
`runLLMTurn @T`). This is the genuinely novel design work and the reason
the decls were hand-written originally. Wants a short design note before
implementation; held.

**#24 — one-home rule:** after #20 steps 2–3, write the standing rule for
what the generator owns vs the stdlib (haskell/lib/Tidepool) vs verb
libraries, into the CLAUDE.md hierarchy + GLOSSARY, and migrate
violations. Held.

---

## Fold checklist

| Lane | Merge gate | Post-fold verification | Task |
|---|---|---|---|
| surface-cut | cargo check --workspace --all-targets | fast tier + the lane's targeted battery leg re-run at tip if the merge had ANY conflicts | #19 |
| lsp-nuke | same | fast tier; if handlers/effect_defs changed: scripts/battery.sh -p tidepool-handlers | #25 |
| fork-concurrency | same | fast tier at tip | #18 |
| suspension-schema | same | fast tier at tip | #20 (step 1 done; task stays open for 2–3) |
| composition-root | same | fast tier + release build of the moved binary | #23 |
| persistence-versioning-design | same (doc-only) | doc reads consistently; link it from plans/README.md Active work | #21 (design done; impl held) |
| kernel-design | same (doc-only) | same | #22 (design done; impl held) |

## End-of-wave report (write for the operator, then stop)

- Folds landed this wave, one line each, with commit shas.
- Full harness shard walk results (all groups, sequential) + final fast
  tier counts.
- The two design docs, three-sentence summaries each, awaiting review.
- Held items (this doc's held section) restated.
- Explicitly: companion untouched and still parked; redeploy awaiting
  operator; which deferred renames #21's design would unfreeze.
