# Wave 5 delivery

Latest behavioral result: 216/217 fixture keys match, zero mismatches; all 812
tops admit and compile. See “Finite-corpus acceptance reached” below for exact
denominators, omissions, executable provenance, and the separate broad-gate
status. This delivery record replaces the retired chronological
`plans/stg-wave5.md` trial log, which remains recoverable in Git.

Final source check at `b217095e5`: 837/837 focused nextest library tests pass;
workspace test targets compile. **Workspace green is not established:** the
broad changed gate stops in Clippy before running its tests.

Finish recovered-body and lazy execution delivery before Wave 6 effect-interpreter
integration. Delivery takes precedence over delegation experiments and unused
primitive-family completeness. The prior verified checkpoint is `800ab0d06`;
its 173 matching fixture keys are evidence, not completion.

## Current evidence

`59945d6e6` closes the pending mutable-array-copy batch: 163 prepared-program
tests and 42 machine-state tests passed through `scripts/dev-shell.sh cargo test
-p tidepool-codegen --lib <filter> -- --nocapture` with filters
`prepared_program::` and `machine_state::`. Logs remain in
`target/w5-delivery-{prepared,machine}.log`.

Schema migration `6c86ca470` has 242 repr library tests and 12 codec tests
passing; Haskell execution-schema-encode passes. Two old producer fixtures
remain schema 7 until the final coordinated regeneration. This is not a full
fixture gate. Workspace formatting is isolated in `93e1b25f5`.

`4dd4f226a` connects MutVar new/read/write, noDuplicate, wired-in failures and
the exact deferred-capability runtime catalog. `nix develop --command cargo
test -p tidepool-codegen --lib` passed 384 tests, including reached and untaken
capability paths and same-CAF retry. Combined Haskell projection and corpus
projection self-tests pass with the wired-in producer regressions in
`0c9143feb`. These are focused results, not a workspace or corpus acceptance.

The three historical STG JSON ledgers are removed from the active tree by
request; `800ab0d06` preserves them in Git. Corpus evidence is reproducible via
`env -u TIDEPOOL_EXTRACT -u TIDEPOOL_EXTRACT_WORKER just fixtures-check`.
Its generated manifests/results remain build artifacts, not new committed logs.

### Delivery rerun

`37d8a8c51` records regenerated producer fixtures and the canonical fingerprint.
Both producer writers passed; repr's cross-language contract passed 7/7.
`just fixtures-update` and `just fixtures-check` exited zero. The latter's
corpus report is **not** all-green acceptance:

| Stage | Passed | Failed | Not reached / missing expectation |
| --- | ---: | ---: | ---: |
| Projection | 709 | 103 | 0 |
| Validation, admission, compilation (each) | 709 | 0 | 103 |
| Execution | 526 | 183 | 103 |
| Comparison | 174 | 0 | 104 not reached; 534 missing |

The denominator remains 812 STG tops and 217 expectation keys. The 103
projection failures now all name `__hsbase_MD5Init`; the old `patError`
admission failure is gone. The 183 execution outcomes remain 94 managed-host
argument limitations, 74 Address observations, nine missing arguments, three
function observations, two budgets, and one non-finite blackhole omission.
They are not 183 newly discovered engine defects.

Both production structural probes pass all six stages: `Project.Work.candidate`
observed through `Preparation/Complete -> Left/Right` is `Left 7`; the
`awaitSettled` dependency projection is `[[(17,True)]]`. A compiled pinned-GHC
oracle independently produced both values and the exact wired-in pattern-error
message `Suite.hs:3: Non-exhaustive patterns in f\n`. These monomorphic probes
use GHC2024 and -O2; they are not general workbench-dialect oracle coverage.

Artifacts: `target/prepared-corpus/run.NVIiwK/provenance.json` identifies the
frozen executables; `target/prepared-corpus/suite.w1YOfv/results.json` holds
per-row outcomes. The run began at dirty `1a9246e9f`; frozen producer/probe
changes were subsequently committed as `1af9be1b9` and `8d55d0d08`.

`nix develop --command cargo test --workspace --no-run --quiet` exited zero
on the regenerated checkpoint (warnings remain). Formatting and suite
registration checks passed. `just changed 800ab0d06` stopped at three denied
validator `expect` calls before nextest; failure evidence is under
`target/tidepool-test-runs/20260913T173901Z-990022-changed`.
Those three calls were removed in `eae444084`: repr library tests passed 242/242
and production repr clippy passed. All-targets repr clippy still rejects
`items_after_test_module` in `execution_schema/validation.rs`; no lint allowance
or large test-module relocation was folded into that cleanup. Workspace
compilation also reports existing unused code/import warnings. Nextest has not
run through the broad gate, so focused passes are not workspace-test evidence.
The finite-key acceptance target is still 216, not 174. Wave 5 remains open.

## Order and owners

1. Repair pending mutable-copy test fixtures; run prepared-program and
   machine-state tests and commit the isolated batch.
2. Walk recovered STG for operation identities/signatures, literal labels and
   recovery residuals without changing production projection. Missing bodies
   remain explicit boundaries of this inventory. Catalog from this evidence,
   not from a guessed spelling list.
3. One schema migration adds deferred capabilities and wired-in errors.
   Capability operations retain Returns; wired-in errors use NoSuccess.
   Haskell encoder, Rust decoder, validation and the existing cross-language
   contract migrate together. Unknown identities remain rejected.
4. Implement live-path MutVar new/read/write through the external-storage and
   barrier owner with a distinct descriptor. noDuplicate is a no-op only under
   invocation-private, serialized, nonconcurrent execution. Cancellation/retry
   restarts evaluation; it is not GHC stack resumption. Atomic variants defer.
5. Lower exact catalogued missing functions as ordinary synthesized callable
   tops so bare references and PAPs work. Recognize wired-ins by GHC keys.
   Catalogued labels, if still present after the defining capability boundary,
   fail through an Address-returning operation, never a fabricated pointer.
6. Run the corpus and close newly exposed live-path gaps. Format the workspace
   in a separate commit, run the broad gate, remove the three STG JSON ledgers,
   and publish a compact handoff with commands and named limitations.

## Failure contracts

- Deferred capability execution records UnsupportedCapability as the first
  cause, publishes no result, and leaves a sound machine reusable. An unknown
  import is not automatically a deferred capability.
- patError and nonExhaustiveGuardsError are PatternMatchFailure; recConError,
  noMethodBindingError, recSelError and typeError retain distinct language kinds.
  Preserve GHC UTF-8 message formatting, including recSelError's prefix.
- absentError, absentConstraintError, absentSumFieldError, impossibleError and
  impossibleConstraintError are typed terminal integrity failures. Treating
  impossible workers as terminal is an intentional stricter policy than GHC's
  ErrorCall rendering: reaching them violates a compiler invariant.
- Read error strings only inside authenticated pinned storage with a bounded
  NUL scan. Invalid storage is integrity failure, never a partial message.
- Reusable failures restore thunk state/captures; terminal failures do not
  dereference a potentially damaged heap. Preserve the existing first cause.
- noDuplicate's no-op requires one evaluator per heap, eager blackholing and
  no scheduler that starts another evaluator while a thunk is active. Restarting
  after cancellation can repeat unsafePerformIO allocations; the supported
  backtrace CAF uses only allocation, not externally committed effects. Wave 6
  must distinguish a suspended evaluator's blackhole from an actual loop before
  permitting another evaluator on that heap.

## Inventory boundary

The recovered-closure inventory includes operations with signatures, address
labels, and recovery residuals. It is diagnostic data, not permissive projection.
The first complete run is `target/prepared-corpus/suite-inventory.7dlMDZ`:
812 targets, zero inventory failures, and 146 distinct operation/signature
pairs across the 103 previously stack-blocked closures. Missing-body interiors
are not observable to this walk. A post-catalog corpus run remains necessary.

The exact stack boundary includes the pinned `collectStackTrace1` worker as
well as `collectStackTrace`; its body owns the libdw session and address imports.
Fingerprint MD5 operations also occur in these closures. Pinned-source review
finds a live ordinary-error path: exception annotation for Backtraces uses its
Typeable dictionary, `mkTrCon` computes `fingerprintFingerprints`, and that calls
`GHC.Internal.Fingerprint.$wfingerprintData`, which owns MD5Init/Update/Final.
This is not exclusive to the default-off mechanisms. MD5 therefore requires
real implementation, not a deferred-capability replacement. The corpus rerun
must establish the concrete remaining stage failures before further work.

### Authenticated fingerprinting implementation

The pinned MD5 operations are `MD5Init [Address, Void] -> []`,
`MD5Update [Address, Address, Int(32), Void] -> []`, and
`MD5Final [Address, Address, Void] -> []`, all successful `Returns` contracts.
GHC's configured MD5 context is 88 bytes and its digest buffer is 16 bytes;
those are pinned ABI facts, not portable Rust-layout assumptions.

The caller also uses pinned byte arrays, contents addresses, keepAlive,
address offsets, byte reads/writes and width conversions. A correct patch must
preserve managed ownership while addresses are live and authenticate complete
read/write spans. The existing owner is MachineState's external-storage ledger
and its checked byte-range/store operations. The old Core JIT's non-poison raw
address check is not sufficient authority and must not be copied as the guard.

The implementation reuses GHC 9.12.2's public-domain MD5 kernel, with pinned
source provenance beside the C files. Authenticated bytes are copied into an
aligned Rust-owned context before calling C; user addresses never reach that
kernel. MachineState's existing external-storage ledger authenticates complete
spans, including signed offsets confined to the original allocation. No second
allocation registry is introduced. Pinned byte arrays use the existing stable
external byte storage; contents addresses do not carry ownership themselves.

`keepAlive#` calls the generated callback through the shared ABI and retains an
opaque use of its managed owner after success. A real-adapter regression forces
collection while the callback holds only the raw byte address and proves that
the external payload remains alive. MD5 has known empty, short and multi-block
kernel vectors, checked-span failure tests, and a generated-adapter digest test.
The full codegen library passed 405 tests; after an inert test-binding cleanup,
the five focused address tests passed again. Both Haskell projection suites
passed. These checks do not establish the remaining corpus count or replace
the pending compiled-GHC fingerprint comparison.

Inspect remaining operation/signature pairs from the corpus rerun rather than
assuming MD5Init was the only missing operation. No stack-capability stub or
exception-context erasure may turn this live fingerprint path into an apparent
success.

The next canonical `just fixtures-check` passed with executable snapshot
`target/prepared-corpus/run.ewbawl` and Suite report
`target/prepared-corpus/suite.Fm1mJX/results.json`. All 812 tops projected and
validated. Native admission accepted 709 and rejected 103; compilation accepted
those 709. Execution passed 526 and reported 183 failures/limitations. Comparison
remained 174 matching fixture keys, zero mismatches, out of 217 keys. Moving the
103 rows from projection to admission is not an execution success. Joining each
rejected node to its immutable artifact identifies five first-blocker cohorts:
88 `intToInt32#`, six `decodeDouble_Int64#`, six `leChar#`, two `timesInt2#`,
and one `addWordC#`. These are native catalog gaps, not MD5 execution failures.
The priority, actor, recovered-body, formatting and dependency-shadow cohorts
all passed every stage. The snapshot predates the non-producer-reachable
keepAlive NoSuccess admission tightening; its three focused tests passed later.

The validator test module was moved byte-for-byte after its production items;
242 repr library tests and all-target repr Clippy with `-D warnings` passed.
This removes that crate's prior lint blocker, not a claim about workspace Clippy.

### Audited native breadth and IPE boundary

`bb8c4703c` adds an all-operation audit over projected artifacts using native
admission's exact identity/signature classifier. The prior Suite snapshot
contained 145 distinct pairs, 16 unsupported; these are all-pair findings, not
another first-blocker count. Bounded scalar/address operations were implemented,
and `decodeDouble_Int64#` was checked against compiled pinned GHC, including
subnormal, infinity and NaN behavior. Three fingerprint programs also match
compiled-GHC structural expectations. No catch/masking no-op was introduced.

`d90ca7551` defers the pinned IPE decoder worker by exact module, identity and
signature. Its interface test also proves a source-module `$wgo` remains
ordinary code. Canonical `just fixtures-check` completed with snapshot
`target/prepared-corpus/run.vnfgUy` and Suite report `suite.mmH9Ox/results.json`.
All 812 tops project and validate, but only 709 admit and compile: 526 execute,
183 report execution failures/limitations, and 103 still stop at admission.
Comparison remains 174 of 217 fixture keys matching, zero mismatches. The
priority, actor dependency, recovered-body, formatting, shadow and fingerprint
probe cohorts pass. This is not the 216-key acceptance gate.

The same-snapshot all-operation audit reports 128 identity/signature pairs,
126 supported and two unsupported, with no projection or decoding omissions.
The remaining pairs are `newAlignedPinnedByteArray#` and `raiseIO#`, present
in all 103 rejected tops. Operation pairs and fixture keys are different
denominators. These remaining operations require disposition against their
actual recovered paths; absence from the catalog is not evidence of dead code.

Verification at this boundary: 427 codegen library tests, 10 bignum tests and
15 corpus-runner tests passed. The Haskell execution-schema-projection suite
passed and `just fixtures-update` completed. Workspace test-target compilation
(`nix develop --command cargo test --workspace --no-run --quiet`) passed;
it ran no tests. `just changed 800ab0d06` passed formatting and stopped at
25 heap Clippy errors before nextest. Its log is under
`target/tidepool-test-runs/20260913T185514Z-1252207-changed/`.
Heap cleanup subsequently passed all-target Clippy and 71 library tests.
The broad rerun advanced past heap but stopped on one worktree and 55 codegen
Clippy diagnostics; nextest still did not run. Its retained log is under
`target/tidepool-test-runs/20260913T190141Z-1270764-changed/`.
Focused green checks are not workspace green.

### Finite-corpus acceptance reached

The aligned-allocation/raiseIO runner snapshot
`target/prepared-corpus/run.aligned.YVZugd` replayed the unchanged canonical
`suite.mmH9Ox` artifacts. Results are retained in
`target/prepared-corpus/replay.aligned-suite.3xLZfU/`.
All 812 tops project, validate, admit and compile. Execution succeeds for 628;
the other 184 rows are 95 unsupported managed-host-argument invocations,
74 Address materializations, nine missing scalar-argument invocations,
three function observations, two observation-budget failures, and the one
explicitly omitted non-finite blackhole. These are not 184 semantic mismatches.

Comparison: **216 matching fixture keys, zero mismatches**, 595 rows without
an expectation, and one not reached. The fixture-key denominator remains 217;
the top-level-program denominator remains 812. The all-operation audit decodes
812 artifacts with zero omissions and accepts all 128 observed exact pairs.
This is a native-runner replay, not a second Haskell projection run. Snapshot
SHA256 is `7ed102b6bad9de2c0504fe1afa17a1d33482555b0ba83f9be050b1a82ccf658b`;
its provenance records `c5362e09b` plus dirty source subsequently committed in
`05037fbf7` and the cleanup changes. All 435 codegen library tests passed at
that source boundary. Remaining broad-gate cleanup is reported independently.
The same runner also passes every stage of the priority constructor (1), actor
dependency probe (1), recovered-body (1), formatting (5), shadow-identity (1),
and compiled-GHC fingerprint (3) cohorts. Reports are retained under
`target/prepared-corpus/replay.aligned-cohorts.hWHVFe`.

### Final verification and handoff

At `b217095e5`, the following completed:

- `nix develop --command cargo nextest run -p tidepool-codegen -p tidepool-heap -p tidepool-repr -p tidepool-toolchain --lib`: 837/837 passed across four binaries.
- `nix develop --command cargo test --workspace --no-run --quiet`: exit 0;
  all workspace test targets compiled, no tests executed by this command.
- `git diff --check`: exit 0; changed Rust files formatted.

Two lint commands remain red, with no source repairs made beyond the bounded
engine/toolchain cleanup:

- `nix develop --command cargo clippy -p tidepool-codegen --all-targets -- -D warnings`
  gets through codegen/toolchain and fails in the runtime dev dependency on
  12 diagnostics: `inspection.rs` (large enum and three unwraps),
  `persistent.rs` (two large errors and two expects), and `prepared.rs`
  (three large errors and one expect). Session reconciliation remains Wave 6.
- `just changed 800ab0d06` stops earlier in the workspace ordering at
  `tidepool-agent/src/backend/codex/rollout_usage.rs:189` (redundant guard)
  and two cloned-reference-to-slice diagnostics in
  `rollout_usage_bounded_tests.rs:61,168`. Its exact reproduction and log are
  in `target/tidepool-test-runs/20260913T192232Z-1348080-changed/`.
  This command did not execute nextest; the separate focused invocation above did.

The three `tidepool-agent` diagnostics are also present on `origin/main`; that
crate is byte-identical to main in this range. They block the workspace command,
but are not Wave 5 regressions. The runtime diagnostics belong to the Wave 6
session owners named above.

The vendored MD5 C signedness warning and unrelated test warnings remain
nonfatal. No performance claim or full-workspace test pass is inferred.
The next planning boundary is Wave 6: retained session integration and effect
suspension/resumption, including the deferred resident-workbench reconciliation.
No Core fallback was added to satisfy this corpus acceptance.

## Acceptance

216 finite Suite fixture keys match with no comparison mismatch; the optimized
non-finite blackhole remains explicitly omitted. Do not shrink the denominator
or classify harness omissions as successes. Capability and wired-in retry,
first-cause, no-output and MutVar moving-GC tests pass. Add structural expectations
for Project.Work.candidate and an awaitSettled dependency-only probe; execution
of its continuation remains Wave 6, not a hidden requirement here.

Run focused contracts, workspace test-target compilation, canonical fixture
check and the relevant broad changed gate. Record actual results separately.
Real stack snapshots, IPE/libdw, atomic MutVars and unused primitive gaps remain
explicitly deferred; no Core fallback is introduced.

## Execution discipline

Lead owns semantic seeds and cross-language decisions. Delegate bounded
implementation, inventory and checks where they reduce total work. No worker
headcount target. Shared-tree builds have one acknowledged lease; production
schema edits wait until the pending test batch completes. Use fresh review for
failure semantics and pointer ownership, not another general audit wave.
