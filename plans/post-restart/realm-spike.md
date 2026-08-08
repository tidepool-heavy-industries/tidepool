# Spec: realm-machine spike TL (GO/NO-GO)

Owns Track 2 of `plans/post-restart/one-compile-bootstrap.md`: the unified
(realm) machine — `continuations: HashMap<ContinuationId,
ContinuationFrame>` replacing the single `suspended_continuation` slot,
with the CYCLE-SCOPED machine as the plausible shape. Deliverable is a
VERDICT plus a prototype branch, not a landing.

## The lane is also an experiment (Inanna, 2026-08-08, verbatim intent)

This spike runs as its OWN lane deliberately overlapping the extract
wave's runtime territory, "as a test to see if we're being too timid
about merge conflicts." Therefore:

- Do NOT defensively partition files or avoid natural edits because
  another lane might touch them. Work where the design says to work.
- Keep a CONFLICT LEDGER (`plans/post-restart/realm-conflict-ledger.md`):
  every rebase/fold conflict — file, hunk shape, resolution minutes,
  whether any side was dropped. The ledger is a first-class deliverable;
  it is the experiment's data.
- Stop-and-ask applies only when a resolution would DROP a side's
  behavior; mechanical resolutions just get logged.

## ANTI-PATTERNS

- DO NOT couple to Track 1 (one-compile bootstrap). Explicitly decoupled
  by the anchor doc; the extract wave owns Track 1.
- DO NOT merge a common source-level capability super-row. Separate
  capability rows stay regardless of machine unification (compile-time
  boundary: answerer code must not import runLLMTurn).
- DO NOT build an immortal unified machine. JITModule functions and
  machine-wide root clearing make it grow without bound; the shape under
  test is cycle-scoped (drop at loop boundary after State serializes).
- DO NOT skip GC hazard testing on any heap-touching prototype:
  `TIDEPOOL_GC_POISON` + `HEAP_VERIFY` on every prototype run that parks
  or resumes more than one continuation.
- Operational, copy VERBATIM into every dev spec:
  - Every GHC-heavy run goes through
    `/home/inanna/dev/tidepool/scripts/ghc-slots.sh run -- <cmd>`
    (absolute path). NEVER `exclusive` mode.
  - `export XDG_CACHE_HOME="$PWD/.cache"` before any tidepool-harness
    test shard (persistent per-worktree, not mktemp).
  - Spawns pass an explicit `model: sonnet` (or `opus` for sub-TLs);
    never fable.
  - Never path-unscoped `pkill -f`; scope kills to PID or full worktree
    path.
  - Commit with `--no-verify`. Never `git add -A`. Repo-root `tmp/` is
    protected. Grep/Read over LSP; no per-worktree rust-analyzer.

## READ FIRST

- `plans/post-restart/one-compile-bootstrap.md` — Track 2 IS the spike
  checklist; the file:line anchors were spot-checked 2026-08-08.
- `tidepool-codegen/src/jit_machine.rs` — `suspended_continuation`
  (~164), `run_child` + `ChildSuspended` (~2112), `fork_snapshot`.
- `tidepool-codegen/src/machine_state.rs` (~100) — stowed root-slot
  tracing (the GC foundation claimed ready — VERIFY, don't trust).
- `tidepool-codegen/CLAUDE.md`, `tidepool-eval/CLAUDE.md` (differential
  oracle), `tidepool-runtime/src/session/{persistent,resident}.rs`.
- Memory/plan context: harness-one-model-full-fork (2026-08-01) — parent
  and child continuations in one heap is the runtime foundation the
  full-fork decision wants; say in the verdict what the spike implies for
  fork_snapshot's 400-LOC clone.

## STEPS

1. Score the realm-ownership checklist item by item against the code:
   pending, binding/decl planes, finalized + bound root slots,
   cancellation, effect roster + suspend threshold, persistent-root
   retirement, compiled-function lifetime. The last two are the suspected
   real cost — get concrete (what exactly grows, what exactly can't be
   reclaimed today).
2. Prototype the minimal falsifier: two continuations parked in ONE
   machine (parent + one child that SUSPENDS — the thing `ChildSuspended`
   forbids today), resumed out of order, GC fired between parks
   (GC_POISON + HEAP_VERIFY + small MAX_HEAP). This kills or confirms the
   design faster than any analysis.
3. Check dispatch metadata per fragment (positional tags + suspend
   threshold) against a GENERAL node's base-effect row — the anchor flags
   this dormant-today hazard; the spike must confirm it stays tractable.
4. Prototype cycle-scoping: machine dropped at loop boundary after State
   serializes; confirm reclamation-by-drop actually reclaims (JITModule,
   heap arenas, root slots).
5. Write the verdict: GO (with the recommended landing shape + what
   fork_snapshot shrinks to) / NO-GO (with the specific wall). Either
   verdict is a success; a fast honest NO-GO beats a slow maybe.

## VERIFY

- Prototype runs under GC_POISON + HEAP_VERIFY; differential quick tier
  (`cargo nextest run -p tidepool-codegen`) green on the prototype branch
  at every commit that touches machine code.
- No correctness gate is owed for the verdict doc itself; receipts are
  owed for every empirical claim in it (the named test run, counts).

## DONE CRITERIA

- Verdict doc committed with the scored checklist, falsifier results, and
  fork_snapshot implication.
- Conflict ledger committed with every conflict encountered, however
  boring.
- Prototype branch preserved (not merged) and named in the verdict.
