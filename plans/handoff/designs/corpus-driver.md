# Prepared-corpus Suite driver: classification, native oracle, floors

Draft against `a43ec1971` (`engine/stg-production-cutover`). Nothing here has
been built or run. The only checks were `git apply --check`, `bash -n`,
`rustfmt --check`, and running the jq filters on synthetic reports.

Patches (apply from the repo root with `git apply`):

- `patches/01-harness-classification.patch`: `tidepool-testing/src/prepared_corpus.rs`,
  `tidepool-testing/src/bin/prepared-corpus.rs`, `scripts/prepared-corpus.sh`
- `patches/02-suite-oracle-generator.patch` (new files): `scripts/prepared-corpus-oracle.sh`,
  `haskell/test-prepared-stg/suite-oracle/{SuiteOracle,SuiteOracleTH,SuiteOracleRender}.hs`,
  `SuiteOracleClassifications.json`, `SuiteOracleNonterminating.txt`

Patch 01 has `prepared-corpus.sh` call `prepared-corpus-oracle.sh check`. Land
both patches together, then run `scripts/prepared-corpus-oracle.sh update
<suite manifest>` once, before the first gated run.

## 0. What the 184 failures and 595 missing expectations are

A row's scope comes from `suite.HCetap`. `source` means the occurrence is a
top-level binding with a signature in `Suite.hs` (252 of them, all of which are
in the manifest). `compiler` means a value-namespace top that GHC introduced.
`local` means the `local` namespace.

| Execution outcome today | source | compiler | local | total |
|---|---:|---:|---:|---:|
| `HostArguments` (managed arguments) | 14 | 74 | 7 | 95 |
| scalar `Arguments` (`$w` workers) | 0 | 9 | 0 | 9 |
| `Address` observation (engine) | 0 | 55 | 19 | 74 |
| `Unobservable(Function)` (`$fFoldableBox`, `$fFunctorBox`, `$fTraversableBox`) | 0 | 3 | 0 | 3 |
| observation budget (`thunk_cycle_xs'`, `thunk_letrec_knot_xs`) | 0 | 2 | 0 | 2 |
| omitted `thunk_blackhole` | 1 | 0 | 0 | 1 |
| passed | 237 | 78 | 313 | 628 |

Comparison has 216 passes, all on source tops. The 595 missing expectations
break down as:

- 412 rows that executed: 21 source, 78 compiler, 313 local.
- 183 rows whose execution failed. They are reported as missing expectations
  only because `comparison_for_execution_error(_, None)` returns
  `MissingExpectation`.

**Consequence.** No native oracle can address about 560 of the 595 rows. A
simplifier float such as `t_swap1`, a `sat.N` local, or a `$tc` binding has no
source name that GHC can evaluate; its evidence comes through the source top
that uses it. Only 21 executed source tops lack an oracle. Of those, 18 are
representable today; the other 3 are `qq_j_anti`, `qq_j_build` and
`qq_j_scalar`, all of type `Tidepool.Aeson.Value.Value`. The harness therefore
has to tell "missing oracle" apart from "no oracle can exist". Otherwise the
missing count is permanently about 560 and gates nothing.

## 1. Classification instead of failure

Every row still carries all six stages in order (`StageRecords` is
unchanged). The patch adds two `Outcome` variants:

- `Classified { class, reason }`. JSON:
  `{"status":"classified","class":"not_closed"|"no_finite_observation"|"function_valued","reason":...}`.
  It counts as neither passed nor failed.
- `NoOracle`. JSON: `{"status":"no_oracle"}`. Used on the comparison stage
  only.

`StageTotal` gains the counters `not_closed`, `no_finite_observation`,
`function_valued` and `no_oracle`.

### 1a. Function-typed tops: `NotClosed`, not generated arguments

A top whose prepared entry signature takes arguments is a function, not a
closed program. We do not drive it with generated arguments, because nothing
could check the result. The oracle generator runs GHC only on closed
source-level values; it cannot evaluate `$fOrdDown_$ccompare` or
`$wmyTakeWhileT` at arbitrary inputs. Generated arguments would give execution
evidence (the code did not crash) but never a comparison. Those functions are
already exercised through the closed tops that apply them (`t_sortOnDown`,
`text_takeWhileT_*`, and so on), and those tops are compared.

- **Decided before native entry.** `entry_closure` finds the entry's top
  binding. If its right-hand side is `HeapRhs::Function`, the function's
  `Signature.arguments` go through the same predicate the engine applies in
  `prepared_program::invocation`:
  - any `LiftedRef` or `UnliftedRef` argument makes it not closed (the engine
    would return `Unsupported::HostArguments`);
  - any other non-`Void` argument makes it not closed (the engine would return
    `ExecutionError::Arguments`, "physical scalar slots");
  - `Void`-only arguments leave it closed.

  Thunks and constructors take no arguments, matching how `prepared_program.rs`
  assigns entry signatures.
- **The engine stays the backstop.** If the predicate admits an entry but the
  engine refuses it, execution is `Failed`. That catches drift between the two
  copies of the predicate.
- **An oracle on a non-closed entry is `Failed`.** It means GHC evaluated a
  program this artifact does not describe.
- **Scalar workers (9) are also `NotClosed`.** Suite.hs has no closed wrapper
  that is the worker. The closed callers (for example `t_bangPatterns` and
  `letrec_pow`) already execute and carry oracles. Driving the worker directly
  would need scalar arguments and a matching oracle, which is follow-up work
  that needs a dedicated wrapper in a contract cohort.

### 1b. `thunk_blackhole`: keep it omitted; do not expect `BlackHole`

The brief suggested executing it and expecting `RuntimeError::BlackHole`.
In-tree evidence says that is unsound. The test
`no_finite_observation_preserves_compile_evidence_without_native_execution`
records that the pinned GHC lowers `let x = x in x` to a self-recursive
let-no-escape join: the native binary loops and never raises a blackhole. Its
prepared STG is the same join, so the machine loops until the watchdog fires.
An expected blackhole would contradict the oracle and turn a watchdog kill into
a failure.

So the patch keeps omitting native execution. What changes is how the row is
recorded: execution becomes `Classified(NoFiniteObservation)` instead of
`Failed("harness limitation")`. The generator does not hand-write this
classification. It is required to observe the timeout, and
`SuiteOracleNonterminating.txt` must list the top with a reason. A listed top
that terminates also fails generation.

### 1c. Cyclic tops: `CyclicObservation`

`thunk_cycle_xs'` and `thunk_letrec_knot_xs` reach weak head normal form, but
their graphs are cycles. They get a new `Expectation::CyclicObservation`, and
native execution does run:

- The only accepted result is
  `ExecutionError::Observation(ObservationFailure::BudgetExceeded)`, recorded
  as `Classified(NoFiniteObservation)` with comparison not reached.
- Running out of budget is weak evidence, because any runaway structure does
  the same, so it never counts as a pass.
- A finite value fails comparison.

These names are compiler floats, so the classification is the one hand-reviewed
input (`SuiteOracleClassifications.json`, each entry with a reason).

`validate_oracle_domain` in the runner fails the whole run in three cases:

- a classification key is not a manifest expectation key;
- a source top has disappeared from the manifest;
- a compiler-introduced key carries anything other than `CyclicObservation`.

A GHC rename is therefore loud.

A sound replacement is engine work and is not drafted here. The observer would
detect a back edge (DFS gray set over constructor addresses, run only after the
budget fails) and return `ObservationFailure::Cyclic`. The harness would then
classify on that typed cause, and the hand list could be deleted.

### 1d. Dictionaries with function fields: `FunctionValued` now; constructor layer later

What "constructor layer with function-field handles" would take:

- `tidepool_bridge::Value` has only `Lit`, `Con` and `ByteArray`. It would need
  an opaque leaf, for example `Value::Closure(ObjectKind)`, or an observation
  mode in `RunOptions` that emits one.
- The dictionary's constructor name would need to be in the metadata table.
- `Expectation` would need a `Constructor { name, fields }` kind with a
  `function` leaf.

That is a cross-crate change to a type used across the workspace, so it is left
as follow-up.

What the patch does instead: a row with no oracle whose native call returned but
whose observation failed with `Unobservable(Function | Pap)` is recorded as
`Classified(FunctionValued)`. The cause is typed, never matched on strings. A
value oracle always wins, so a row GHC evaluated to data stays `Failed`.

### 1e. Comparison scope

`Expectations` gains `source_tops: Option<BTreeSet<String>>` (serde default).
`OracleScope::of(expectation_key, expectations)` is:

- `Unscoped` when the file has no domain. Every hand-written contract cohort is
  in this case, so their behaviour does not change.
- `SourceTop` when the key is in `source_tops`.
- `CompilerIntroduced` otherwise, which includes `local` rows with no key.

A row that executed without an oracle is `MissingExpectation` for
`SourceTop`/`Unscoped` and `NoOracle` for `CompilerIntroduced`.

A row whose execution failed or was classified now has comparison `NotReached`,
unless it has an error oracle. It is no longer `MissingExpectation`, which
double-counted the failures. The only other reader of `results.json` is
`prepared-corpus.sh` (checked with `rg`).

## 2. Native oracle generator (skeleton in patch 02)

This is the "pending native GHC oracle" named in `scripts/fixtures.sh`. It is
separate from `haskell/test/corpus/GenOracle.hs`. That generator serves the
retired Core corpus, has a different wire format, and keeps a hand-maintained
binding list. The new one produces the existing prepared `Expectation` JSON and
has no hand list.

### Domain

- Input is the Suite projection manifest's `expectation_key` set, which is what
  the runner looks up.
- A Template Haskell splice (`SuiteOracleTH.oracleTable`, file named by
  `SUITE_ORACLE_NAMES`) resolves each key as `Suite.<occurrence>` with
  `lookupValueName`. GHC's renamer is the authority. Floats (`t_swap1`) and
  non-identifier occurrences (`$w…`, `$f…`, `Rec:field`) do not resolve and
  become `compiler_introduced`.
- `domain` output lists `scope` and, for source tops, `class`: `value`,
  `not_closed` (arrow, forall or type variable at top level), or
  `unrepresentable`.
- Data constructors that resolve (`Box`, `Down`, …) are source tops. They are
  functions, so `not_closed`.

### Type to expectation (must match `compare_values` and the observation `Value` shape)

| Haskell type | Expectation | Observation matched |
|---|---|---|
| `Int` | `int` | `I#` or literal, via `i64::from_value` |
| `Bool` | `bool` | `True`/`False` |
| `Char` | `char` | `C#` or canonical word (`unbox_char`); surrogates refused |
| `Data.Text.Text` | `text` | `Text ByteArray# off len` via `String::from_value` |
| `[Char]` / `String` | `list` of `char` | a `:`/`[]` chain, element-wise (not `text`) |
| `[a]` | `list` | `:`/`[]` chain |
| `()`, `(a,b,…)` | `tuple` | constructor `()`, `(,)`, … by name |
| `Maybe a` | `maybe` (`null` for Nothing) | `Nothing`/`Just` by name |
| `Either a b` | `either_left` / `either_right` | `Left`/`Right` by name |
| `Double` | `float64_approx` with `absolute_tolerance: 0.0` | literal double bits |

- Type synonyms are expanded (`String`, Suite's `type Text = T.Text`).
- Newtypes, user ADTs, `Integer`, `Word`, Aeson `Value`, and function-valued
  components are `unrepresentable`. They stay `missing_expectation` and are
  listed in the fixture's `refusals` map, which Rust ignores.
- Rendering is typed and done by a renderer that TH generates for each type.
  There is no `Show` text. GHC's `show` for `Double` is the shortest
  round-trip decimal, which is valid JSON; NaN and infinities are refused.
  JSON strings are escaped to ASCII.

### Evaluation protocol (`suite-oracle eval OCCURRENCE`, one process per top)

1. Force the top to weak head normal form:
   - `NonTermination` gives `{"kind":"error","value":"blackhole"}`.
   - Any other synchronous exception gives
     `{"kind":"error","value":"raised_exception"}`.

   Both kinds match `matches_expected_failure`.
2. Render and force the JSON:
   - A bottom inside the value exits with status 4 (refused). A non-forcing
     observation cannot reproduce a nested bottom, so it gets no error oracle.
   - An unrepresentable leaf exits with status 3.
3. The driver wraps each call in `timeout` (default 10s). In-process timeouts
   cannot interrupt a loop that never allocates. Exit 124 is accepted only for
   tops listed in `SuiteOracleNonterminating.txt`, which become
   `no_finite_observation`.

Build: `ghc --make` with the prepared pipeline's optimisation contract from
`GhcPipeline.canonicalizeDFlags`:

`-O2 -fno-full-laziness -fno-cpr-anal -fexpose-all-unfoldings -fexpose-overloaded-unfoldings`

The include path is `-ihaskell/lib -ihaskell/test`, and GHC must be exactly
9.12.2.

### Output and freshness

`tidepool-testing/fixtures/prepared-corpus-expectations.json` is regenerated
with `jq -S` and contains:

- `source_revision`, `oracle_fingerprint`, `source_tops`, `refusals`;
- `expectations`: generated values plus the reviewed cyclic classifications.

The fingerprint covers `haskell/lib`, `suite-oracle/`, `Suite.hs`, `flake.*`,
`cabal.project`, the script, the flags, and the manifest key set. It follows the
`scripts/fixtures.sh` pattern.

Modes:

| Mode | What it does | Used by |
|---|---|---|
| `check` | compares the fingerprint only, no GHC | `prepared-corpus.sh` |
| `verify` | regenerates and diffs | — |
| `update` | regenerates and writes the fixture | — |

The first `update` replaces the 216 hand-transcribed legacy entries. Review
that diff: a disagreement is either a transcription error or an engine finding,
and either way it shows up as `comparison.failed > 0`.

## 3. Floors and how `prepared-corpus.sh` asserts them

`assert_suite_report` (jq) now asserts:

- `stg_programs == 812`, and the four structural stages are fully passed.
- Execution:
  - `passed >= 628`
  - `failed <= 74`
  - `not_closed + no_finite_observation + function_valued <= 110`
  - `passed + failed + classified == 812` (no running, not-reached or missing
    rows)
- Comparison:
  - `failed == 0`
  - `passed >= 234`
  - `missing_expectation <= 3`
  - `passed + missing_expectation + no_oracle + not_reached == 812`
- Every row whose execution passed has comparison not equal to `not_reached`.
- The fixture declares `source_tops`, and `prepared-corpus-oracle.sh check`
  passes against this run's manifest before the runner starts.

`assert_contract_report` also requires the four new counters to be zero, so
contract cohorts keep their all-six-passed contract. `report_totals` prints the
new counters.

Classifications never count as passes. Hiding a regression behind one still
breaks the execution-passed or comparison-passed floor, and the classified
ceiling catches growth. If the first real run measures differently, set the
floors to the measured values; they are floors and ceilings, not targets.

## Expected counts

| Stage | Before | After |
|---|---|---|
| execution | passed 628, failed 184 | passed 628, failed 74, not_closed 104, no_finite_observation 3, function_valued 3 |
| comparison | passed 216, missing 595, not_reached 1 | passed 234, failed 0, missing 3, no_oracle 391, not_reached 184 |

The 18 new comparisons:

- `qq_fmt_empty`, `qq_fmt_multi`, `qq_fmt_multiline`, `qq_fmt_plain`
- `qq_j_pat_array`, `qq_j_pat_extract`, `qq_j_pat_literal`, `qq_j_pat_nested`, `qq_j_pat_open`
- `t_bimap`, `t_first`, `t_fromEither`, `t_partitionEithers`, `t_rightsLefts`
- `t_second`, `t_sortOn`, `t_sortOnDown`, `t_swap`

`source_tops` should hold roughly 252–260 names: the 252 signature-declared
tops, plus constructors and fields that the renamer resolves.

## Risks

1. **Unbuilt.**
   - Rust: new imports (`Group`, `HeapRhs`, `PreparedProgram` from
     `execution_schema`; `ObjectKind` from `tidepool-heap`, already a
     dependency) and the changed `run_prepared_artifact` arity. The binary is
     the only caller.
   - TH details for GHC 9.12: `tupleTypeName 0` vs `''Unit` for `()`,
     `MulArrowT` spine shape, synonym reification.
   - `ghc --make` package visibility for Suite's library dependencies
     (`template-haskell`, `text`, …) needs the Nix with-packages GHC.
2. **The closure predicate is duplicated** from `prepared_program::invocation`,
   in tension with "one implementation". Better: codegen exposes
   `CompiledProgram::entry_admission(entry)` or a public
   `EntryAbi::semantic_arguments` accessor on compiled entries, and the harness
   calls it. The engine error backstop limits the damage in the meantime.
3. **Optimisation flags.** The in-tree comment says `-fcpr-anal`, but
   `canonicalizeDFlags` unsets CPR analysis. If the native binary built with the
   pipeline flags raises `<<loop>>` instead of looping, the generator emits a
   `blackhole` error oracle and the nonterminating-list check fails generation.
   That is loud, but someone then has to decide how to classify the top.
4. **Legacy fixture replacement.** Double tolerance changes from 1e-10 to 0.0
   on 5 rows, and any `[Char]` top previously transcribed as `text` becomes a
   list of `char`. Both are compared structurally, but review the first
   `update` diff.
5. **`FunctionValued` hides a data-to-function regression** on rows without an
   oracle. Only compiler and local rows are affected; the classified ceiling and
   the execution floor bound it.
6. **Cyclic classification depends on GHC float names.** A rename fails the run
   through `validate_oracle_domain`, which is intended but means
   reclassification work. It is removed by the engine-side `Cyclic` observation
   failure described in 1c.
7. **Bottom inside a value is refused** (exit 4) and so raises the missing
   count. If a top does this, update the `<= 3` ceiling from the measurement.
8. **Generator runtime:** an `-O2` build of Suite plus `haskell/lib`, roughly
   240 process runs, and one 10s timeout. `prepared-corpus.sh` uses only the
   fingerprint `check`.
9. **Out of scope:** the 74 `Address` observations (engine work).
