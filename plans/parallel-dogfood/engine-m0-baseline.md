# Prepared-STG engine M0 baseline

Status: measured pre-M1/M2 baseline. This report is based on
`303c297cbb5b2cbf86c454437256fe38b5faee9e` and does not define a prepared
program, schema, or ABI.

Vocabulary: implementation identifiers use `prepared_*`; “STG” names the GHC
pass consumed by the M1 handoff.

## Boundary and safety

The measurements use the existing `scripts/bench-turn.sh`, public runtime,
REPL, and harness examples, the `JitEffectMachine` diagnostic counters, and
the existing engine-review fixture. No extractor or compiler endpoint used by
the running swarm was changed. The direct run explicitly had
`TIDEPOOL_EXTRACT_DAEMON_SOCKET`, `TIDEPOOL_EXTRACT`, and
`TIDEPOOL_EXTRACT_WORKER` unset. The resident run started one owned daemon on a
new `mktemp` Unix socket through `scripts/lib-extract.sh::start_battery_daemon`
and its trap stopped that exact PID after the run.

No shared cache was cleared. `bench-turn.sh` uses fresh temporary compile-memo
directories for its cold row and one new temporary directory, warmed once, for
its warm row; it removes those directories afterward. Cargo's existing build
state was uncontrolled and is not evidence of a warm compiler or shared
compile cache. Nix activation and release-example builds are outside each
reported row. The direct command's complete setup plus measurement wall time
was 228 s; its three release builds reported 0.31, 0.33, and 0.25 s. The
resident command measured owned-daemon startup separately at 523 ms; its three
builds reported 0.22, 0.23, and 0.21 s.

An initial attempt exposed a harness-path ambiguity: `.cargo/config.toml`
places artifacts under `.shoal/build/cargo`, while `bench-turn.sh` executes
hard-coded `target/release/examples/...` paths. It stopped before the first
sample with exit 127. The checked runs used a temporary untracked
`target -> .shoal/build/cargo` symlink and made no source or endpoint change.
The script should eventually resolve Cargo artifact paths rather than assume
`target/`; that repair is outside this report-only parcel.

## Environment and executable identity

- Host: Linux 6.12.63 x86_64, Intel Core i5-12600K, 32,632,468 KiB memory.
- Nix GHC: package `ghc-9.12.2`, executable
  `/nix/store/nj7qd6d1pjy1v28bh5mniljsxfr9a57v-ghc-native-bignum-9.12.2-with-packages/bin/ghc`,
  SHA-256 `5733c877678e78851bcd76d2108853c162bcb1e62956bd2c87fd6e586ca48014`.
- Extract frontend: `.shoal/build/cargo/debug/tidepool-extract`, 2,912,688
  bytes, SHA-256
  `cc220a59ff0accc05582949603b6e240456611c56e7be63c6595c870dfe0d5c8`.
- Haskell compiler worker: the Cabal `tidepool-extract-bin` under
  `haskell/dist-newstyle/build/x86_64-linux/ghc-9.12.2/`, 83,536,776 bytes,
  SHA-256
  `956f30fa49d57dcd89fe101440c42724c115f94370b384c3457e6aa53aea1b7a`.
- Rust: 1.93.0 (`254b59607`, host `x86_64-unknown-linux-gnu`);
  `tidepool-codegen`, `tidepool-eval`, and `tidepool-runtime` 0.1.0 with
  Cranelift codegen/JIT 0.129.1 from `Cargo.lock`.
- Engine outcome test binary:
  `.shoal/build/cargo/debug/deps/codegen-1baceb4a92e469d0`, SHA-256
  `cadd8fa23851e8595070b005588ca4b493cf0d83a93efff587432c5353a3a7a7`.
- Release workload binaries `bench_oneshot`, `bench_session`, and
  `turn_latency_bench` had SHA-256 identities
  `2f59ea734acad1c0cba715fe26533aeee00933033166b939ec1f5fd5f4b27fb2`,
  `bcf8db3e7e617bc560b4a3949a30cb67402826b05a6ef2b8a1ea9dd8de3f5337`,
  and `bd670381a3aaec80678da6c8cd1e51ffb6e62e48be76f69851d4172e8fe1098e`,
  respectively.

## Fixed compile/turn workloads

`scripts/bench-turn.sh` ran every row three times and independently reported
the median for each integer-millisecond key:

- `oneshot_cold`: `sum [1..5000 :: Int]` through
  `EvalHarness::run_pure`, a new compile-memo directory for every process.
- `oneshot_warm`: the same program, one explicit untimed warm-up followed by
  three new processes sharing that one temporary memo directory.
- `session`: a fresh public REPL `Session`, declaration
  `helper x = x + 1`, then three one-item binding turns.
- `block5`: one fresh session and one block containing five independent
  bindings.
- `harness`: one real `Harness` turn with `ReplayProvider`; no live model call.

### Direct compiler endpoint

Command (after the temporary artifact-path symlink described above):

```sh
unset TIDEPOOL_EXTRACT_DAEMON_SOCKET TIDEPOOL_EXTRACT TIDEPOOL_EXTRACT_WORKER
nix develop --command scripts/bench-turn.sh
```

The command exited 0. Representative medians were:

| Row | wall ms | extract spawn/total ms | JIT codegen ms | run ms |
|---|---:|---:|---:|---:|
| one-shot cold | 208 | 138 total | not separately emitted | not separately emitted |
| one-shot warm | 71 | no extractor spawn (memo hit) | not separately emitted | not separately emitted |
| resident-session declaration | 6,837 | 3,337 / 3,321 | unavailable on this REPL path | unavailable on this REPL path |
| resident-session turn 1 | 6,311 | 3,026 / 3,010 | unavailable | unavailable |
| resident-session turn 2 | 6,321 | 3,087 / 3,073 | unavailable | unavailable |
| resident-session turn 3 | 6,391 | 3,095 / 3,080 | unavailable | unavailable |
| five-item block | 34,949 | 17,037 / 16,948 | unavailable | unavailable |
| harness replay turn | 12,852 | 8,727 / 8,715 | 368 | 0 |

The direct cold one-shot extractor median broke down as 105 ms GHC load, 31
ms Core, 1 ms GHC setup, and all other emitted phases below the integer-ms
resolution. The harness extractor median included 2,888 ms GHC load, 4,841 ms
Core, 609 ms typecheck, 131 ms translation, 33 ms setup, 11 ms classify, and 2
ms CBOR encode. A reported `0` means below the integer-ms recorder resolution,
not no execution.

### Owned resident compiler endpoint

Command:

```sh
nix develop --command bash -lc '
  unset TIDEPOOL_EXTRACT_DAEMON_SOCKET TIDEPOOL_EXTRACT TIDEPOOL_EXTRACT_WORKER
  source scripts/lib-extract.sh
  resolve_tidepool_extract
  trap teardown_battery_daemon EXIT INT TERM
  start_battery_daemon
  scripts/bench-turn.sh
'
```

The command exited 0, started an owned daemon in 523 ms, and tore down its PID
afterward. Representative medians were:

| Row | wall ms | extractor-front-end boundary ms | JIT codegen ms | run ms |
|---|---:|---:|---:|---:|
| one-shot cold outer memo | 10 | not emitted | not separately emitted | not separately emitted |
| one-shot warm outer memo | 7 | no outer extraction | not separately emitted | not separately emitted |
| resident-session declaration | 967 | 416 | unavailable on this REPL path | unavailable |
| resident-session turn 1 | 960 | 432 | unavailable | unavailable |
| resident-session turn 2 | 1,000 | 430 | unavailable | unavailable |
| resident-session turn 3 | 974 | 440 | unavailable | unavailable |
| five-item block | 4,737 | 2,280 | unavailable | unavailable |
| harness replay turn | 4,691 | 3,669 | 357 | 0 |

Daemon-side phase lines stay in the daemon log and are not forwarded to the
client, so the resident GHC/typecheck/Core/translation split is unknown. The
three cold-row samples also share one daemon and the report retains medians,
not individual samples: the row proves fresh outer memo directories against a
resident worker, but it is not a controlled distribution of compiler-process
cold versus compiler-process warm startup. The one-shot warm row proves an
outer compiled-artifact memo hit. Neither row establishes the state of any
shared cache.

## Independent native, reference, and JIT outcomes

The fixed fixture is `tidepool-codegen/tests/fixtures/EngineReview.hs`. Direct
Nix GHC evaluation produced `lazyArgument = 42`, `unusedLoop = 42`,
`customAppend = 1`, and `nulString = 195`:

```sh
nix develop --command ghc -ignore-dot-ghci \
  tidepool-codegen/tests/fixtures/EngineReview.hs \
  -e 'print EngineReview.<target>'
```

The production extractor plus reference and JIT checks were executed together:

```sh
env -u IN_NIX_SHELL just test-target tidepool-codegen codegen \
  'test(=engine_review::reference_evaluator_preserves_lazy_arguments) or \
   test(=engine_review::jit_does_not_enter_unused_recursive_argument)'
```

Observed result: 2 selected, 2 executed, both failed as the current M0
regression baseline. GHC returned 42 for both targets. The reference evaluator
entered the unused `bad 0` argument and returned a division-by-zero
`TypeMismatch`; the JIT entered the unused recursive argument and the owned
one-second watchdog returned `Yield(Runtime(Cancelled))`. These are distinct
engine outcomes, not agreement. Extracted storage for these cases was 536-byte
Core CBOR plus 769-byte metadata for `lazyArgument`, and 381-byte Core CBOR plus
769-byte metadata for `unusedLoop`; both had zero ask sites. The failed test
artifacts retain the exact output under
`target/tidepool-test-runs/20260908T020241Z-4121250-battery` in the measurement
workspace, but that transient path is not part of the commit.

The later independent observer at exact semantic source
`e3793ad96df2c822696124a4704d0456ac3632a7` ran each engine in an owned bounded
process and recorded all four original defects without treating any pair of
failures as agreement:

| Target | Native GHC | Reference evaluator | JIT |
|---|---|---|---|
| `customAppend` | `Success(1)` | `Success(2)` | `Success(2)` |
| `nulString` | `Success(195)` | `CompileError` | `CompileError` |
| `lazyArgument` | `Success(42)` | `RuntimeError` | `Success(42)` |
| `unusedLoop` | `Success(42)` | `NativeFault(SIGABRT)` | `RuntimeError(SignalError(11))` |

The exact four-test selection executed four and failed four because these are
required intended-semantics acceptance tests, not passing baseline assertions.
After recording the evidence, the M0 integration marks those four exact tests
individually ignored with a link to this report, so the ordinary registered
target is green without converting a current defect into expected success.
They remain executable acceptance gates for the later prepared-STG repair; the
ignore is removed when each defect is fixed. No broad module or suite ignore is
used. The private subprocess observer and its successful cases remain ordinary
green tests.

## Allocation, roots, code, and lifetime

The existing 2 KiB-nursery resident-machine measurement ran 64 successive
`add_function` plus `run_pure_and_bind` operations under GC poison and heap
verification:

```sh
env -u IN_NIX_SHELL NEXTEST_SUCCESS_OUTPUT=immediate \
  just test-target tidepool-codegen resident \
  'test(=realm_root_growth::realm_root_growth_persistent_roots_and_heap)'
```

It passed (1 selected/executed; 60 skipped):

The fixture is registered by the `resident` integration target, not
`codegen`: an earlier `just test-target tidepool-codegen codegen` selection
ran zero tests and exited 4, so it is not counted as evidence. The owning
`resident` command above is the executed result.

| binds | persistent roots | nursery bytes | live bytes | collections | old-space bytes |
|---:|---:|---:|---:|---:|---:|
| 1 | 1 | 2,048 | 0 | 1 | 56 |
| 8 | 8 | 2,048 | 0 | 8 | 448 |
| 64 | 64 | 2,048 | 0 | 64 | 3,584 |

Thus this workload retains one root and 56 old-space bytes per binding while
scope remains live. `live_bytes` is only nursery state and must not be read as
total retained bytes.

The existing equal-work code-lifetime comparison compiled 512 fragments as 32
machines x 16 fragments and as one machine x 512 fragments:

```sh
env -u IN_NIX_SHELL NEXTEST_SUCCESS_OUTPUT=immediate \
  just test-target tidepool-codegen resident \
  'test(=realm_leak_comparison::realm_leak_comparison_one_vs_many_machines)'
```

It passed (1 selected/executed; 60 skipped). The 32-machine arm's last machine
reported 17 defined functions; the one-machine arm reported 513. RSS/VSZ
observations in bytes were:

| checkpoint | RSS | VSZ |
|---|---:|---:|
| process baseline | 5,349,376 | 160,079,872 |
| 32nd small machine populated | 10,207,232 | 160,079,872 |
| all 32 small machines dropped | 10,207,232 | 160,079,872 |
| one 512-fragment machine populated | 14,389,248 | 161,132,544 |
| large machine dropped | 13,918,208 | 160,079,872 |

RSS is an allocator/process-footprint proxy and one sample is not a
distribution or proof of leaked mappings. The owning mapping check provides
the stronger cleanup boundary:

```sh
env -u IN_NIX_SHELL just test-lib tidepool-codegen \
  'test(=pipeline::tests::module_drop_releases_code_after_success_and_definition_failure)'
```

It passed (1 selected/executed; 118 skipped), proving finalized executable
addresses cease to be executable after module drop on both successful and
definition-failure construction paths.

Release example file/ELF observations (not JIT-emitted-code byte counts) were:

| executable | file bytes | ELF text | data | bss |
|---|---:|---:|---:|---:|
| `bench_oneshot` | 11,070,104 | 8,213,100 | 374,256 | 3,488 |
| `bench_session` | 13,396,640 | 9,785,645 | 406,424 | 4,992 |
| `turn_latency_bench` | 25,273,888 | 19,278,311 | 679,944 | 16,424 |

The current owner exposes monotonic `functions_defined`/fragment counts but no
aggregate finalized JIT-code byte count. `CodegenPipeline::define_function`
sees each `compiled.code_buffer().len()` transiently for stack-map ranges, but
does not retain a session total. Exact emitted code bytes and code/data
residency are therefore unverified M7 costs; this M0 report does not add an
unused counter merely to manufacture a number.

## Released-frontier revalidation

The M0 owner revalidated the observation boundary on the released-frontier
source derived from `1b3be1b64a09d0ddacd07981f7fc6f05c499bf11`. The private
observer now preserves these distinct outcomes through its subprocess
transport: language rejection, compiler/extractor failure, language/runtime
failure, cancellation, integrity failure, operational failure, unsupported
input, timeout, native fault, and observer/harness failure. Compile
classification consumes `CompileError::Diagnostics` for genuine source
rejection and keeps worker/protocol/I/O failures separate. JIT classification consumes
`RuntimeError::machine_disposition`, `RuntimeError::Cancelled`, and
`EmitError::NotYetImplemented` rather than interpreting rendered diagnostics.
The ledger, rather than an unobserved runtime variant, records the accepted
native-aarch64 deferral; it does not turn compile-only or x86_64 evidence into
native aarch64 proof.

The ordinary independent semantic selection executed five tests and passed
five (263 skipped): lazy constructor fields, returned functions,
multi-parameter recursion, multibyte characters, and strict-field failure.
The four individually ignored prepared-STG gates were then deliberately run
with `--run-ignored all`: four executed, four failed, 264 skipped. Their
current independent outcomes were:

| Target | Native GHC | Reference evaluator | JIT |
|---|---|---|---|
| `customAppend` | `Success(1)` | `Success(2)` | `Success(2)` |
| `nulString` | `Success(195)` | `CompilerFailure(modified UTF-8)` | `CompilerFailure(modified UTF-8)` |
| `lazyArgument` | `Success(42)` | `RuntimeError(division by zero)` | `Success(42)` |
| `unusedLoop` | `Success(42)` | `NativeFault(SIGABRT)` | `RuntimeError(HeapOverflow)` |

The last row is a bounded failure observation, not a proof of divergence. It
also supersedes the older transient `SignalError(11)` result for the current
source without rewriting that historical observation.

An independent measurement worker reran the fixed resident allocation and
code-lifetime consumers. Root growth executed one and passed one (60 skipped),
reproducing the saved `(binds, roots, nursery bytes, live nursery bytes,
collections, old-space bytes)` rows exactly:
`(1,1,2048,0,1,56)`, `(8,8,2048,0,8,448)`, and
`(64,64,2048,0,64,3584)`. The equal-work lifetime case executed one and passed
one (60 skipped), again defining 17 functions in the last small machine versus
513 in the one large machine. Its new process-footprint sample was:

| checkpoint | RSS | VSZ |
|---|---:|---:|
| process baseline | 5,271,552 | 160,129,024 |
| 32nd small machine populated | 10,272,768 | 160,129,024 |
| all 32 small machines dropped | 10,272,768 | 160,129,024 |
| one 512-fragment machine populated | 14,442,496 | 161,181,696 |
| large machine dropped | 14,417,920 | 160,129,024 |

The variation from the older RSS sample is expected and remains explicitly a
process/allocator proxy. The executable-mapping cleanup test executed one and
passed one (127 skipped) on both success and definition failure. The earlier
118-skipped count describes its older source, not this revalidation.

`scripts/bench-turn.sh` still owns the same median-of-three cold, warm,
resident-session, five-binding block, and replay-harness workloads. Its phase
and latency values above remain the measured pre-M1 baseline; this revalidation
audited the unchanged workload contract but did not claim a new latency sample.
The hard-coded `target/release/examples` lookup also remains an acknowledged
script portability gap when Cargo artifacts live under `.shoal/build/cargo`.
No production timing owner was duplicated to hide that gap.

## Explicit remaining gates

- No native aarch64 execution was performed; x86_64 measurements do not prove
  aarch64 ABI, tail-call, or root behavior.
- Direct and resident phase distributions beyond median-of-three are absent;
  resident daemon-side GHC phases and individual cold/warm samples were not
  retained by the owning harness.
- REPL session/block rows do not emit separate JIT-codegen or execution stages.
- Per-operation allocation counts, calls, forces, updates, root spills,
  external-array bytes, full/old-generation collection, exact JIT code bytes,
  and durable compile-cache storage remain unverified. Existing observations
  cover nursery/old-space/root/GC counts, fragment count, mapping cleanup,
  transient extracted artifact sizes, and process footprint only.
- The engine-review failures are recorded regressions to be repaired by later
  engine work; neither a cancellation nor a matched failure is success.

## Report checks

The released-frontier private M0 observation consumer was rechecked at the exact
source with the following selection (the command entered the repository Nix
environment and tore down its owned compiler daemon):

```sh
env -u IN_NIX_SHELL just test-target tidepool-codegen codegen \
  'test(=engine_review::m0_outcome_vocabulary_does_not_collapse_failures) or \
   test(=engine_review::m0_support_inventory_keeps_every_provisional_row_unverified) or \
   test(=engine_review::failure_classification_uses_owning_types) or \
   test(=engine_review::process_bound_preserves_awkward_failure_classifications)'
```

Result: four selected/executed, four passed, 264 skipped. Final report checks
also include Rust formatting and `git diff --check`. All `just` commands used
the wrapper-owned temporary compiler daemon; each log confirmed teardown of its
owned PID.
