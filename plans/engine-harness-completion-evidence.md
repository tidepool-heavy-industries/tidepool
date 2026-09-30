# Engine and embedded harness completion evidence

Status as of 2026-09-30. This is a checkpoint ledger, not an acceptance claim.
The approved scope and final gates are in
[`engine-harness-final-delivery.md`](engine-harness-final-delivery.md), which
supersedes the earlier completion-wave sequencing.

## Final delivery wave: joined checkpoint

Main `d6955f602` joins the original/native compiler candidate through `595b2f72f`,
protected display consumption, runtime nonempty child initialization and
confirmation-only publication, actor command ownership/resource retention,
Ephemeral publication, and embedded provider attachment. These joins do not
enable concurrent actor admission or establish M1/M2 acceptance.

The native facade graph now shares one linked test binary across separately
declared process, host, late-output and browser actions. On `ec986da3e2` plus the
graph subsequently committed as `d6955f602`, `//bridge/facade:facade_process_tests`
executed **6 tests, 6 passed, 0 failed**, exit 0. The Buck action took 4.5 seconds;
the admitted service took 38.737 seconds. Generator tests passed 18/18. Logs:
[`native-process-six.log`](../target/completion-evidence/final-delivery/native-process-six.log)
and [`focused-generator-tests.log`](../target/completion-evidence/final-delivery/focused-generator-tests.log).
This exercises process framing/cleanup, not a real browser or resident Haskell
journey. Independent review subsequently found cancellation/discovery/descendant
cleanup gaps in the **Buck runner itself**; repair `a382aa1448` passed 15/15
runner regressions and source review. The six native process tests passed again
on that repaired runner; see
[`native-haskell-authority-r2.log`](../target/completion-evidence/final-delivery/native-haskell-authority-r2.log).
Nine actions executed locally with zero action-cache hits. Incremental reuse is
not evidence of an action-cache hit. The service's 57.5 MiB peak excludes the
pre-existing Buck daemon.

Native compiler suites `//bridge/haskell:planned_declaration` and
`//bridge/haskell:source_boot_product_reuse` both executed and passed on joined
`a382aa1448` plus the fixture graph committed as `bb72aaa55`. The former includes
the exact-byte thin-interface authority proof from `bfc688640`/`d3ae6ac32`.
The latter includes cold/resident/fresh-worker SOURCE reuse and invalidation.
The combined invocation with the six process tests took 45.106 seconds and
executed 29 local actions (zero action-cache hits). An initial graph parse error
ran no tests; the corrected invocation is the `r2` log above.

Runtime authority `851a1221b` and its scoped identity prerequisite `58f046fc7`
are joined as `c8a243cc02` and `7a745b65dd`. Native
`//tidepool/runtime:runtime_admission_tests` passed **3/3**, exit 0, on `c8a243cc02`
plus the graph committed as `7a2264df2`; generator tests passed 18/18. The
admitted service took 22.768 seconds, while the counted test action took 0.1
seconds. See [`native-runtime-admission.log`](../target/completion-evidence/final-delivery/native-runtime-admission.log).
This checks interface inventory, exact native scope membership and original
native-owner retention; it does not replace real checked-cell execution.

Native package smoke and packaged worker compilation passed **2/2** at
`d6955f602` (34.005 seconds service time, 13 local actions). The packaged worker
produced a nonempty prepared artifact from its declared `WorkerSmoke.hs` input.
See [`native-package.log`](../target/completion-evidence/final-delivery/native-package.log).
This is bounded startup/compiler evidence, not production embedded restart or
final matched-package acceptance after later source changes.

The original declaration → binding → expression → display native gate passed
on compiler candidate `595b2f72f`: one passed, 307 skipped, 171.833 seconds,
`/tmp/tidepool-next-compiler-original-bi.log`. Protected hidden display passed
on `05901db219`: one passed, 307 skipped, 289.363 seconds,
`/tmp/tidepool-next-compiler-display-bf.log`. Final Ephemeral declaration
publication and private actor activation are separate pending gates.

The actual two-delayed-child capture gate passed on combined candidate
`82e7f2d3e69ea3762f7a1565c88c86b881a1e44b`: one passed, 421 skipped,
404.185 seconds. Its source, command and compiler hashes are retained in
`/srv/build/buck-out/cargo-completion-next/qapp-provenance/actor-qapp-evidence.md`.
This supersedes that fixture's earlier qApp failure below; the workspace-barrier
fixture and the joined concurrent M2 worker tree remain unaccepted.

Runtime exact-publication recovery passed through a distinct Rust process and
fresh compiler processes on the candidate finalized as `4909c5818`: one passed,
313 skipped, 91.334 seconds, with four additional extractor-free tests and one
journal test passing. See the
[`recovery packet`](../target/completion-evidence/next-wave/recovery-final-packet.txt)
for exact tested WIP/source attribution and commands. Compiler-pair provenance
was corroborated retrospectively, and the test used an explicit cross-worktree
timestamp override; it is not a doctor-clean packaged gate. Production embedded
startup, typed journal/Store successor transfer and execution of a recovered
nonempty child remain separate work.

All eight retained worker threads are assigned implementation or independent
review. New worker creation hit the thread limit, so reviews in this wave reuse
an independent review thread; they must not be described as fresh-context
reviews. Compiler authority/value replacement, private/keyed actor execution,
durable recovery, structural scaling, cache invalidation and final packaging
are still in progress. No live provider or backend cutover has been run.

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

## Earlier checkpoint and retained gate history

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
