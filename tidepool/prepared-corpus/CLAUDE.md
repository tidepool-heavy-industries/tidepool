# tidepool-prepared-corpus

Owns the compiler-derived prepared-STG corpus consumer, typed projection and
diagnostic inventories, oracle outcomes and six-stage acceptance policy.
`prepared-corpus validate-fixture` admits a complete declared source/target
output set through production metadata, prepared-program and source-evidence
readers. It grants no execution authority.
`validate-original` shares those output readers but validates completed source
observations independently of replay eligibility. It is for fresh native test
output, and grants neither source replay nor a hermetic build-input proof.

`prepared-corpus verify-cohort` executes every manifest row in an isolated
process and requires a nonzero complete count. Contract cohorts pass all six
stages. Suite retains every compiler-introduced row through compilation and
checks source outcomes against native GHC values or explicit typed refusals.
The existing per-item watchdog records the exact suspected nontermination;
crashes and missing child reports cannot omit rows.

`build/haskell/corpus_fixture.bzl` owns declared per-module corpus actions.
Every cohort carries same-transaction metadata, dependencies and diagnostic
inventory. Pure, replay-eligible cohorts use immutable runtime resources.
Suite executes its actual quasiquotes in a fresh compiler integration test:
the declared runtime recipe invokes the same generator once, validates its
completed output, runs the same GHC oracle, then verifies every corpus row.
Its output belongs to the test run, is removed after success and retained after
failure. False dependency replay flags remain false; arbitrary TH effects are
not claimed to be hermetic build inputs. Native oracle actions reuse
`SuiteOracleTH` and `SuiteOracleRender`, compiling and evaluating the declared
source with pinned GHC. Time-intrinsic expectations remain independent chrono
goldens because the Haskell source is an intrinsic placeholder.

Focused validation uses the relevant
`//bridge/haskell:corpus_<cohort>_test` target through the admitted pinned Buck
entrypoint. `just fixtures-check` selects these targets; it does not maintain a
second compiler cache, oracle updater, corpus registry or acceptance policy.
