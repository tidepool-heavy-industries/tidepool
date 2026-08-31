# Resident typed-yield harness

This crate owns harness-node lifecycle, authored loop driving, agent sessions,
forks, operator interaction, durable harness events, and restart recovery. It
uses the runtime session registry, codegen roots, shared handlers, and web
presentation rather than replacing them.

- Access the resident machine only through checkout APIs. Machine continuation
  identity is `(SessionId, HoleId)`; never guess “the current hole.”
- Compile through `tidepool_runtime::artifacts::compile_targets`; do not add a
  second extractor or cache path.
- Effects are never replayed. `ReplayProvider` replays model replies and
  `fold_tree_state` rebuilds durable state; neither authorizes repeating
  external actions.
- Closures and live values cross continuations through `ValueHandle`, not JSON
  or a tolerant bridge. Runtime resource-scope retirement must preserve JIT
  root accounting.
- Every result-bearing agent session is statically pinned to its answer type.
  Imports used by generated turns and validation probes must match.
- Child failure is data at that child's typed position; scheduler, assembly,
  and bookkeeping failures remain harness failures. Restore fanout results to
  declaration order.
- Operator questions, notes, continue gates, and steering share one
  operator-gate mechanism.
- Use one focused GHC-backed test via `scripts/battery.sh -p tidepool-harness
  -E 'test(<name>)'`; broad harness shards are major-boundary checks.
