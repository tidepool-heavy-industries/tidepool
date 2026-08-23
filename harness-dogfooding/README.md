# harness-dogfooding

Live authored harnesses for the self-iterating harness system — **Xmonad.hs
vibes**: you write `render` / `loop` / `State` in plain Haskell, the runtime
(`tidepool-selfharness`) drives it. This is *our* dogfood space (distinct from
`examples/harness/`, which holds the frozen reference contract), where the
"smart agent" (a clean-context Claude session) authors and iterates the harness
by hand — the manual precursor to autonomous distillation.

Each subdirectory is one authored harness. The current examples follow the
two-file layout:

- `Harness.hs` — the config (Xmonad-like): re-exports the vocabulary + `render`
  and defines `loop`, the one effectful piece. A loop may ask a resident model
  for cognition, orchestrate typed subagents directly, or combine both.
- `HarnessTypes.hs` — `State` + the answer ADTs + the pure `render`, with **no**
  reference to `loop` or runtime handles, so answerers and checkpoint codecs can
  import durable vocabulary without pulling in the outer effect row.

Point the driver at a subdir's `Harness.hs`; its directory becomes the include
root so the sibling `HarnessTypes` resolves.

**Minimal effect surfaces, on purpose:** the answerer row is
`Eff '[AskUser, Fork, ReadState, Green, Finalize]` (`tidepool-harness`'s
`selfharness::driver::answerer_decls`; the delegating companion prepends
`Subagent`/`Worktree`) — typed forms, RECURSIVE sub-answerers bounded by
spawn-time depth/descendant budgets rather than depth-one, and the typed
yield. The development tree instead gives headless workers their native
coding tools while Haskell owns typed orchestration. Frictions hit while
authoring/running these *are the roadmap* for the next harness helpers.

## Harnesses

- [`companion/`](companion/Harness.hs) — **an open-ended, persistent companion
  and harness co-designer**. It uses OODA as an internal typed control-loop
  shape rather than a user-facing workflow: durable state is deterministically
  oriented into a cognition window; the companion converses, wonders, explores,
  or consults the operator; then it returns the compact orientation its next
  iteration should inherit. It is explicitly invited to critique and redesign
  the harness shaping it. [`INSPIRATION.md`](companion/INSPIRATION.md) sketches
  the larger playground: the typed resident REPL, capability rows, executable
  harness self-design, and ephemeral non-serializable values such as closures
  and records of functions alongside durable state.
- [`dev-tree/`](dev-tree/Harness.hs) — **forward dogfood for the typed swarm**
  (PRD 20 S1-L3; see
  [`plans/self-iterating-harness/20-s1-l3-dev-tree-v2.md`](../plans/self-iterating-harness/20-s1-l3-dev-tree-v2.md)).
  The tree is a monadic hylomorphism over `Tidepool.Swarm`'s `PlanF`: cognition
  enters at exactly two typed seams — a coalgebra that splits (the parent-first
  scaffold worker, then one child worktree per child plan seeded from the
  scaffold HEAD) and an algebra that combines (leaf implementation, or the
  eager rebase cascade plus the merge). The plan never materializes as a worked
  tree; what persists is git plus the append-only run journal (`record`).
  Failure is data — `traverse` visits every sibling, and a failed child arrives
  at its parent as an ordinary value. Rebases cascade eagerly, mechanical git
  first and an ephemeral resolution agent second (typed `spawnAsync` handles,
  awaited in plan order), with escalation as a typed value the parent's failure
  policy reads. Its row IS the driver's widened outer session — `[RunLLMTurn,
  AskUser, Console, Worktree, RepoEvent, Exec, Subagent, Journal,
  DelegateBranches, Green]` — and
  `tidepool-harness/tests/dogfood_harness_typecheck.rs` compiles it against
  exactly that row. Node residency (resident select loops) is S1-L4; the seam
  where it lands is named at the `hyloM` call site and built nowhere.
- [`recursive-companion/`](recursive-companion/README.md) — **an
  investigation companion collapsed around `fork`**
  ([PRD 21](../plans/self-iterating-harness/21-recursive-companion-prd.md),
  fork-subsumes-split step 4, superseding the earlier C3 layer-walk slice).
  One turn is one top-level typed request: the session decomposes the
  operator's question by forking typed sub-answerers of its own, recursively
  (`async (fork @T "brief")`), each a full multi-round session, and the
  driver services that tree — spawn-time depth/descendant budgets, the
  operator-page node lifecycle, journal receipts. The fold is ordinary
  Haskell in the session's own block: the code after the `wait`s. See that
  harness's own README for the exact shape and launch line.
