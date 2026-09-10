# Typed-actor RSI launch selection

The platform implementation is on main (`6d55dfc18`). The launch record selects
an immutable runner copy, exact main revision, native executable and one canonical
`.shoal` package. It must not inherit an old session's `TIDEPOOL_*` overrides.
The native pin remains `fe15831c8a22c0d1b8d78d5ce55b7aa5fc3fa666`.

Use the prepared continuation refs when the launch record supplies them:

- `restart/typed-actors-20260909/applications`
- `restart/typed-actors-20260909/engine`

Their `-preserved` companion refs retain the original r6 checkpoints. Each
continuation condenses the original net change into one commit, records its
original head/shared ancestor, and is rebased onto launch main. Read the lane's
`rebase-applications.md` or `rebase-engine.md` before resuming. Do not redo those
rebases if launch main is already an ancestor.

These are partial product inputs, not accepted engine/native replacements. Their
candidate fixture fingerprints must be regenerated with their own candidate
worker, followed by the owning consumer checks. Main's running worker/package
stays frozen. The engine continuation's generated bridge was regenerated from its
combined schema and passed all nine consistency checks. Native candidates remain
separate refs listed in [resume.md](resume.md).

## Platform evidence

- The full model-free package run passed 114 assertions across ten independent
  recipes: typed joins, ordered evidence, review/repair/integration, request recovery,
  notification retention, inherited context/source and collaboration.
- The expanded skill check passed nine assertions, including keeping pending work
  alive during scoped release and retaining the later cleanup receipt.
- The forwarding failure/display check passed four assertions; stale endpoints
  remain stale and the failed exit stays inspectable.
- Resident record checks cover state, typed calls, self/sender/lifecycle identity,
  drain, invalid-state TypeErrors and nested failure notification. Identity-only
  handle display has an additional focused resident check.
- Five prompt checks, nine main generated-bridge checks and all 217 semantic
  Haskell fixtures passed. Fixture regeneration changed only the source fingerprint.

No paid inference or native workers were launched by these checks. The next run
provides live evidence about efficiency and usability; these checks do not establish
cache-hit or token-saving claims.

Launch the Astra planner with [commission.md](commission.md) and the exact launch
record. It refines the saved graph, reviews Sol's understanding once, then leaves
execution to the two Sol trees. [resume.md](resume.md) routes source discovery;
the selected skills and Haskell package own current invocation guidance.
