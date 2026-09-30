# Engine and embedded harness completion evidence

Status as of 2026-09-30. This is a checkpoint ledger, not an acceptance claim.
The approved scope and final gates remain in
[`engine-harness-completion-wave.md`](engine-harness-completion-wave.md).

## Joined source and bounded passing checks

The source-authority checkpoint is `48c70dcb6dacfa10a01b382b18c3d5e9ad311966`. It includes the
source-authority work from `1b6bfbb81ac3034a0b4d66920c3919df2a4b4dca`
(immutable owner-issued capsule), `81d9e35026c1000be121ac6052bab55cddeafd40`
(run-lease validation), `aeeefccbfd1a87f93476ab9d17f80161fce15006`
(unowned-source refusal), `eaf790125b02859556951fdf38d66c5558899167`
(capture lifetime tests), `45c2459fb4e62b6986015075a02019f6af28a15d`
(cell-admission validation), `8a358eb2ce40a9246bd99fe860cdc6969b22cf75`
(selection refusal mapping), and `5f97c0e74412313ad64330a468944cff0026768e`
(model-free TempDir retention). Source evidence is recorded in
[`source-authority-8d7926a8a/evidence.json`](../target/completion-evidence/source-authority-8d7926a8a/evidence.json):
actor authority tests 3 passed; native facade tests 5 passed; default facade
tests 5 passed; privacy doctest 1 passed. The direct actor integration target
compiled with `--no-run`; it was not executed. These checks do not include the
later runtime producer/consumer join.

The matched harness revision is `31ae2e52ca091235c670dbe28aaf730ae148cd4c`;
its retained integration result is Rust 19 passed and UI 44 passed. The fetched
bundle and ref identity are verified in
[`next-wave/harness-31ae2e52/`](../target/completion-evidence/next-wave/harness-31ae2e52/).

The root process-fixture command executed 6 Cargo tests successfully, with 215
skipped. Exact command, exit and count are in
[`browser-process-command.txt`](../target/completion-evidence/next-wave/browser-process-command.txt)
and output in [`browser-process.log`](../target/completion-evidence/next-wave/browser-process.log).
These test process framing/cancellation behavior; they did not start Node,
Chromium, the resident compiler, or a real browser journey.

## Current integration checkpoint and open gates

The compiler/runtime/actor/capture dependency join is `cbc8bdad26`. Its pinned
no-default library check for `tidepool`, `exomonad-actor`, and `tidepool-runtime`
passed (35.15 seconds compiler time; 1.6 GiB unit peak), retained in
[`foundation-joined-check.log`](../target/completion-evidence/next-wave/foundation-joined-check.log).
This is compile evidence, not execution acceptance.

- The real Chromium journey was executed once and failed at the authenticated
  status selector: one failed, 224 skipped, 247.704 seconds. No Haskell-result
  or browser-control proof is claimed from that run. Its log is
  [`browser-journey.log`](../target/completion-evidence/next-wave/browser-journey.log).
  The selector and retained-history navigation were repaired in `f6b8f8874b`;
  the driver native build passed. The second journey failed at a cookie-path
  assertion (one failed, 225 skipped, 232.629 seconds); the cookie inspection
  was repaired to use the authenticated API path in `9febda58bb`. Neither run
  proves the raw Haskell result or control gates.
- Runtime publication repair `496178115f` passed eight paired tests and fresh
  independent source review. Checked physical-prefix settlement `637518d5cd`
  was joined, but fresh review found unsupported display/host-mount entry points
  accepting certificates without validating their sealed execution plan. Repair
  `3829bfbc6` passed a real matched-worker test (one passed, 303 filtered;
  64.27 seconds), including seven refusals and authenticated prefix advancement;
  it is joined as `4d64664760`. Private concurrent admission and recovery remain
  open; the v3 codec does not itself hydrate a recovered public session.
- Protected compiler evidence includes strict same-offer fold, fresh checked
  binding, and hidden nominal binding/expression tests. The hidden case passed
  one test with 302 skipped (206.183 seconds). Original declaration relocation,
  closed capture/display recipes, and mixed-cell publication remain in progress.
- Actor owned primary watch cancellation passed with the real resident compiler.
  Authority-through-cleanup repair `3be3249d2` passed that cancellation gate and
  independent source review. Remaining waits are being converted; this is not
  concurrent actor/worker-tree acceptance.
- Capture producer/consumer join verification `1f8121067f` passed three focused
  lifetime tests with 402 skipped. The real two-delayed-child Haskell test
  was executed alongside the actual workspace-barrier fixture: both failed
  upstream at prepared original-package root generation, before the child-value
  assertions. The identified missing root is `Control.Monad.Freer.Internal.qApp`.
  Package-closure diagnosis is underway; neither capture gate is accepted.
- SOURCE/boot reuse parcel `3796d50b93` passed the Haskell suite, one Rust witness
  test, and one production Cached-receipt test; independent source review cleared
  this bounded parcel. Native Buck SOURCE reuse executed and passed one test
  in `native-source-test-r4.log`. Dynamic plugin input policy passed six cases
  and is joined; its main frontend hook remains with the compiler owner. Native
  family-consistency and plugin-policy targets also passed (two tests total,
  `native-compiler-small-gates.log`). Wider cache acceptance remains open.
- The real-host pending Haskell output across compaction fixture is being
  compiled but failed at the same original-package root generation defect. The
  exact selected test failed after 419.25 seconds; it is not acceptance evidence.
- The measured family-validation structural repair `3ae3b7acf` is joined as
  `f1e3e53ef0`: 1,024 independent injective families took 83.860 → 1.337 ms
  and allocated 602.941 → 5.336 MiB. Twelve semantic cases and the retained
  declaration-join suite passed; see [measurement report](engine-family-validation-cost.md).
- Native facade execution, exact runtime recovery, complete fixture corpus,
  matched packaging, warm-cache mutation gates, and independent reusable worker
  trees remain open.

No M1 or M2 acceptance, full engine completion, live trial, or backend/default
cutover is claimed here. Compilation, review, and partial focused tests are
reported separately from executed end-to-end acceptance.
