# Component and cluster property tests

Keep reusable property tests beside the owning components and merge them with
the code. Test small component clusters through their real boundaries as well
as individual algorithms. End-to-end acceptance exercises the assembled product
path; cheap generated histories explore interactions that a few full runs cannot.

## Recognize targets and choose the claim

Look for complex construction with a simpler independent check, many histories
leading to the same apparent state, or an invariant spanning several owners.
Prioritize mechanisms with broad consumers, plausible failure histories, and
cheap repeatable observations. These structures suggest different techniques:

- **Derived state:** indexes, caches and retained membership can be compared
  with full recomputation from primary facts. Vary insertion, replacement,
  invalidation and removal orders; compare missing and extra results.
- **Identity and graph structure:** aliases, shared roots, cycles and retirement
  expose differences between value equality and identity. Compare logical
  relationships across implementations; unrelated physical handle values need
  not match. Generate both valid graphs and targeted invalid constructions.
- **State transitions:** request settlement, publication and cancellation need
  model-based histories with explicit preconditions and observable effects.
  Include rejected transitions and check what remains unchanged or partially
  completed. Equal final return values do not imply equal resource lifetimes.
- **Transformations:** parsing, serialization, lowering and normalization invite
  differential and metamorphic tests. Derive laws from the contract: idempotence
  fits a canonicalizer; reordering fits only operations promised to commute.
  A round trip can pass when both directions share the same mistake.

Extrapolate by mechanism: a dependency index and a compiler cache can share an
invalidation problem despite different vocabulary. Do not transfer a model
whose essential relationship is absent. A sequential lifecycle model does not
prove concurrent linearizability; a concurrency claim also needs histories,
ordering observations and schedules that can expose its violation. A single
fixed defect may be adequately covered by a deterministic regression.

## Choose an independent oracle

Start from the observable contract. Model an incremental index with a list and
full scans, retained graph membership with a straightforward fixed point, and
request settlement with accepted facts and the first terminal outcome. Do not
copy the production algorithm, its caches, or its ordering accidents into the
oracle. Check both missing and extra results.

State the observation boundary and the law before choosing a generator. Keep
oracle independence and common-mode failures explicit: sharing production's
identity map, normalization or traversal can hide the very defect being tested.
An intentionally slower list, exhaustive check over small domains, or separate
language implementation can make failures easier to distinguish. GHC is the
language oracle for evaluation; a synthetic Rust model proves only the smaller
contract it actually represents.

Separate component and cluster claims. Calling an index directly can establish
its maintenance algorithm, but cannot prove every caller updates it. A cluster
test must exercise those callers and the resulting observable state. Keep real
compiler/runtime integration tests for contracts that synthetic values cannot
establish; do not add test-only production authority constructors.

Exercise proof issuance when that is the claim. Manually supplying a valid
certificate can test its consumer, but cannot show that the real producer earns
or publishes it. Keep logical model identities independent of returned production
handles; assert required uniqueness before recording their correspondence so an
ID collision cannot overwrite the oracle's own state.

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

Assert the scenario's semantic premises before triggering the event under test.
A test of cancellation during pending work must establish that the intended
operation was admitted and is pending; a sleep or ignored setup result does not
prove that. A refusal test must reach the intended boundary rather than fail
earlier because its supposedly valid fixture is malformed. Observe the setup without consuming the event whose
ordering the test needs to explore.

Measure generated behavior: shared owners, successful mutations, terminal
transitions, refusals, depth, sizes and relevant operation combinations. Keep
deterministic support checks for important partitions. More cases cannot cover
an operation absent from the generator. Vary operation mixes when repeated
clears or removals keep every generated state small.

Treat generator support, reachability and observation sensitivity as separate
questions. A generator may mention release while rarely creating shared owners;
a checker may inspect membership while missing retained memory. Track the
preconditions and outcomes that make the claimed failure observable. Rejections
can be test results, but a campaign dominated by rejected operations supplies
little evidence about successful transitions. Fix the distribution or model
before buying more cases.

For repeated stateful exploration, extend a reusable replay driver at the owning
boundary. Add generation, observation and shrinking capabilities as new
counterexamples demand them. Shrinking should minimize the causal history,
not merely its serialized size: preserve the alias, authority distinction or
ordering that makes the failure real. Check minimized traces against the model's
preconditions before classifying a failure as a product defect.

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

For custom runners, keep generated production replay inside the configured
runner so failures receive shrinking and persistence. Separate deterministic
regressions from generator-support checks; a failing prelude must not prevent
the campaign from running unnoticed. Report observed coverage on failure too.
Distinguish selected test functions, configured fresh cases and actual callbacks:
replay and shrinking can add callbacks, while zero-case replay may execute none.
A passing empty replay establishes no search coverage.

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
If a sensitivity check fails, classify whether the generator never reached the
case, the oracle shared the defect, the observation hid it, or the mutation did
not violate the stated contract. Repair the investigative machinery before
claiming that a quiet campaign supports the product. Deliver the property, its
independent model, generator coverage, a replayable minimized failure when found,
and the exact execution evidence; a case count alone is not the handoff.
