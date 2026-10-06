# Component and cluster property tests

Keep reusable property tests beside the owning components and merge them with
the code. Test small component clusters through their real boundaries as well
as individual algorithms. End-to-end acceptance proves the assembled system;
cheap generated histories explore interactions that a few full runs cannot.

## Choose an independent oracle

Start from the observable contract. Model an incremental index with a list and
full scans, retained graph membership with a straightforward fixed point, and
request settlement with accepted facts and the first terminal outcome. Do not
copy the production algorithm, its caches, or its ordering accidents into the
oracle. Check both missing and extra results.

Separate component and cluster claims. Calling an index directly can establish
its maintenance algorithm, but cannot prove every caller updates it. A cluster
test must exercise those callers and the resulting observable state. Keep real
compiler/runtime integration tests for contracts that synthetic values cannot
establish; do not add test-only production authority constructors.

Useful maintained examples are:

- `tidepool/runtime/src/session/binding_table.rs`: incremental membership,
  aliases, shared roots and interface retirement against full recomputation.
- `exomonad/actor/src/request/sequence_tests.rs`: request lifecycle, shared
  targets, watches, acknowledgement and retirement histories.
- `tidepool/repr/src/type_graph/properties.rs`: identity, sharing, graph
  transformations and targeted validation defects.

## Generate histories that reach the contract

Use an explicit operation enum and deterministic replay. Draw interacting
operands from small domains: repeated identities, shared dependencies and
replacement values expose stale derived state. Include rejection and recovery,
not just successful construction. Reads belong in the history when they warm a
cache or acknowledge an observation; an invariant check must not silently
perform those state changes after every operation.

Combine arbitrary histories with targeted sequences embedded in arbitrary
prefixes and suffixes: retain → branch → mutate → release, settle → retry →
observe, and remove → reinsert → query. Shrinking must preserve meaningful
operations. Resolve logical handles deterministically and report explicit
rejections instead of silently skipping operations whose prerequisites vanished.

Measure generated behavior: shared owners, successful mutations, terminal
transitions, refusals, depth, sizes and relevant operation combinations. Keep
deterministic support checks for important partitions. More cases cannot cover
an operation absent from the generator. Vary operation mixes when repeated
clears or removals keep every generated state small.

## Run and retain failures

Use the counted native runner and the package's declared resources. Standard
`PROPTEST_*` campaign controls pass through delegated execution. For example,
from the checkout root after configuring the pinned tools:

```sh
swarm-build env PROPTEST_CASES=4096 just test-lib exomonad-actor \
  --exact request::sequence_tests::generated_request_lifecycle_matches_observable_model \
  --expected-count 1
```

Construct configuration from `proptest::test_runner::Config::default()` and
honor explicit campaign controls when applying suite defaults. Buck's native
test macros supply the compile-time `TIDEPOOL_PROPTEST_REGRESSIONS` path under
the owning package. Select `FileFailurePersistence::Direct` with that value
when present; keep normal Cargo persistence otherwise. Do not derive native
seed paths from `CARGO_MANIFEST_DIR` or `file!()`: those can identify disposable
Buck source projections. Run from the checkout root so the declared relative
path resolves into the source tree.

Retain the minimized operation trace as well as the seed, tested revision,
command, counts and logs. Strategies evolve, so a seed alone is not a stable
bug description. Convert a confirmed defect into a readable deterministic
regression and verify that it fails before the repair and passes afterward.

Calibrate important suites with a plausible temporary defect, such as omitted
invalidation or premature shared-owner release. Coordinate a separate worktree
or an exclusive, recorded source interval with the build owner. Verify generated
cases detect the mutation, that a seed is actually saved outside build outputs,
and that replay detects it. Restore the exact owned patch and rerun against the
repair. Never include deliberate mutants in release artifacts or publish them
as production changes.

Passing randomized tests are bounded evidence, not exhaustive correctness.
Record unmodeled operations and component boundaries alongside the results.
