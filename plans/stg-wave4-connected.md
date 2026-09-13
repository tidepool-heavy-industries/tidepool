# Wave 4: connected prepared execution

Baseline: `08669ea2896d061fa9e7a00a7f32c5e2ca77182f`.
This wave is not production cutover. Compile closed programs once, instantiate
immutable tops per invocation, execute connected strict forms, observe bounded
results without forcing, and release the invocation. Wave 7's final Core-code
removal is intended as a dogfooding assignment for the new swarm.

## Contracts and order

1. Prelude: schema 5 carries global dead-end evidence (no successful result at
   saturation, not a language-error classification). Dead-end entry signatures
   retain arguments and use an empty result vector; the explicit evidence is
   what distinguishes them from ordinary zero-result entries. Character atoms
   become target-width words; scalar tag 3 is retired, not reused. Operations
   intern by name and signature. Internal identities use namespace local and
   deterministic collision-free spelling, reserved before target filtering;
   external identities and retained-generation matching stay exact.
2. Compile-once owner: all entries declared before definitions, pinned code and
   descriptors, constructor observation metadata, static bytes and stack maps.
   Admit closed programs only, no thunk RHS or unsupported application/operation,
   including nested bodies. Fail with typed errors and offending IDs.
3. Static image: immutable top constructors/functions, internal managed
   relocations, exact-start bitmap and per-invocation top table. Validate every
   managed edge stays inside the image. Instantiate fallibly and publish only
   after complete relocation. Collector admits static starts with valid tags;
   nursery-to-static edges remain unchanged. No scan of immutable static fields.
4. ABI and lowering: internal multi-results with Cranelift implicit sret; host
   caller area only at platform boundary. Status controls payload publication.
   Register every live managed SSA component at safepoints. Fold analyses over
   the flat arena; use a worklist for emission. Implement Return, exact direct
   Call, evaluated Enter, classified Case, Constructor/Function Let, joins and
   Jump. Reserve recursive groups once, then initialize all siblings without
   host calls before publication. Updated Enter is an integrity error this wave.
5. Nonforcing observation consumes a rooted result vector (empty is valid),
   constructor identity plus logical reps from the compiled owner, and a shared
   node budget. Unobservable kinds and budget exhaustion are typed failures;
   partial output cleanup is stack-safe. Delete superseded sketches as real
   owners land. No managed host arguments or escaping invocation pointers.

## Seeds and delegation

Lead commits types, signatures, hardest path and acceptance tests before Luna
implementation. Search `wave4:PARCEL` task markers. Unfinished paths must not be
admitted into execution; no successful placeholder values. Investigation and
exact-command tooling need no artificial scaffolding. No wave-4 TODO remains
at the review boundary. Main agent writes this plan and semantic decisions.

Luna High for bounded implementation; Luna Medium for obvious mechanical work.
Keep at least three useful workers active while substantial work remains; more
are allowed when ownership is clean (the present harness has four slots including
the lead). Exclusive file ownership and one build slot still apply. Short briefs
point to seed SHA and marker with exact acceptance.
Two unsuccessful attempts escalate to Terra with findings. Prelude projection
edits serialize; independent readers/tests may run together. Builds use the
repository dev shell. Do not touch untracked generated example artifacts.

## Acceptance

### Corpus execution and schedule addendum

The swarm is currently down. Do not run programs through the outgoing Core
engine or introduce a fallback to it. Fatal producer projection errors remain
early typed/reportable failures; no non-fatal projection mode is required.

Bring semantic corpus execution into this wave alongside freshness checks.
Verify the reported 348 programs and 347 asks against the actual corpus before
using those numbers. Reuse source targets and existing asks, but project new
prepared-STG artifacts: legacy Core CBOR is not prepared input. Give each
program a durable stage result: projects, validates, admits, runs, matches ask.
Record every unsuccessful stage by program name and reason, including expected
closed-strict admission limits and missing asks. Fresh bytes alone are not a
semantic pass. No silent exclusions or denominator changes.

Prioritize the Project.* workspace and Tidepool.* library paths actually reached
by actors, using caller evidence, before generics corner cases. The per-stage
corpus counts guide subsequent waves; this wave establishes the baseline and
failure reasons, not a high pass-rate target. The compiled-GHC pure oracle
remains a separate source of semantic authority, not replaced by historical asks.

Lead settles source/projection/observation/comparison interfaces in code; workers
own bounded corpus plumbing, reports, and exact-command verification. Hook the
execution report into the owning freshness workflow without executing Core tests.
Broad legacy batteries must be re-scoped accordingly; workspace compile-only
remains required.

Wave 7 remains the new swarm's first workload: small mechanical deletion parcels,
an explicit approval list, exact checks, and no semantic decisions. Failure to
run that workload is evidence that the new engine/integration is not ready.

Initial inventory correction: the regenerated working directory contains 348
CBOR files **including `meta.cbor`**, hence 347 program artifacts, and 347 asks
sidecars. Git tracks 293 CBOR files including metadata and no asks sidecars;
fresh generation, not tracked-file count, must define the replay manifest.
The asks sidecars are yield/effect-site metadata (`renderAsksJson`), not expected
return values. The comparison stage needs actual expectations; an empty asks
array must never count as a matching result. Record missing expectations
explicitly while locating/reusing the semantic assertion source.

### Focused engine contracts

### Intermediate review checkpoint evidence

This is a connected-execution checkpoint, not completion of Wave 4 or the STG
cutover. Snapshot includes the implementation, retired prepared sketches,
projection inventory, this plan, and friction notes.

- `bash scripts/dev-shell.sh cargo test -p tidepool-codegen --lib prepared_program:: -- --nocapture`:
  **20 passed, 0 failed**, including static-image integration, deep observation,
  connected calls/cases/lets/joins, and the structural group-reservation test.
- `just fixtures-check`: passed after regenerating the separate prepared M3
  fixture and semantic-corpus fingerprint. Semantic Core CBOR bytes did not
  change; this command establishes freshness, not STG corpus execution.
- `bash scripts/dev-shell.sh cargo test -p tidepool-runtime --lib session::prepared -- --nocapture`:
  2 passed, 1 failed. `terminal_failure_is_replayed_before_cancellation` fails
  while decoding its fixture: required generation must be a tagged array.
- `bash scripts/dev-shell.sh cargo test -p tidepool-runtime --test prepared_execution -- --nocapture`:
  0 passed, 3 failed. `one_shot_rejects_missing_import_malformed_and_precancel`
  and `retained_session_caches_closed_program_and_rejects_unclosed_artifact`
  have the same fixture-decoding error. `one_shot_runs_closed_compiled_program_and_returns_values`
  reports one collection while asserting zero. These four observed runtime failures
  remain unresolved in this checkpoint; no workspace-wide test claim is made.
- `bash scripts/dev-shell.sh cargo build --workspace --tests`: passed, exit 0,
  warnings only, after removing three stale MCP tests referencing retired eval
  helpers. This compiled all workspace test targets; it did not execute them.
- Corpus execution is investigated, not implemented. Expected result data must
  come from real assertions, not asks sidecars. Historical assertions were found
  in `tidepool-eval/tests/haskell_suite.rs` at `e1a4b9145^`; any migration copies
  expectations, not the retired interpreter.

The larger counts and chronological failures below remain trial evidence;
this section identifies the latest checkpoint results.

#### Review corrections after `820f76ff9`

The three runtime decoding failures were a real cross-language field-order bug,
not stale fixtures: Haskell emitted dead-end/evaluated/generation while Rust
decoded evaluated/generation/dead-end. `fdbf24d3f` fixes the producer and adds
an encoder-order assertion. A repr-level decode of the actual Haskell M3
artifact is being added; test-only byte normalization was rejected and must
not be retained. The fourth failure asserted zero **collections**, not zero
returned values; `run_prepared_once` explicitly requests a collection. The
checkpoint handoff misdescribed that assertion.

Remaining review parcels: internal namespace/generation protection and allocator
rename; IEEE float alternative equality and precise rejection locations; one
shared Enter slow inspection path, preserving the inline nonzero-tag fast path.
These changes require focused regressions before being counted complete.

#### Post-review integration evidence (in progress)

Checkpoint requested before final reruns. The complete measured Suite rows are
in `plans/stg-wave4-corpus-checkpoint.json`: 347 programs, 245 project, 206
validate, 64 admit and compile, 57 execute, 53 match, and one executed result
has no expectation. This report predates the final Char comparator correction.
The 217 recovered expectations match the historical test names and values by
source inspection, including composite shapes and the five float tolerances.

Outstanding findings for review:

- All 39 validation failures name duplicate internal `sat` globals. Source
  inspection identifies a genuine projection defect: target reachability maps
  `ExactName` (unit/module/occurrence) to the disambiguated symbol, collapsing
  distinct same-occurrence internal binders. A filtered-out home top falls
  through to `internGlobal`. Keep binder identity through reachability; the
  namespace fix alone does not repair this path. No fix is included yet.
- Three Char comparisons failed because the legacy comparator did not recognize
  canonical `C#` with a Word payload. The comparator now checks that exact
  constructor shape with checked character conversion; the final addition has
  not been rerun. No expected character values changed.
- Seven helpers (`down`, `fromLeft`, `fromRight`, `punField`, `rwField1`,
  `rwField2`, `swap`) require arguments; the corpus runner supplies none.
  Their execution rejection is not evidence of an incorrect returned value.
- The nested-owner regression omitted its recursive self-capture. Only the
  fixture was corrected; the intended rejection-location assertion remains.
  Its rerun is pending. Abnormal-child reporting also received an additional
  post-test regression so a cleanup crash cannot retain an all-passed row.
- Project.Work initially failed import discovery because fixture storage did
  not match module paths. Corrected staging successfully projected the target;
  the ad-hoc follow-up used malformed expectations JSON, so its validation
  failure is harness evidence only. Full recipe rerun is pending.
- The actor cohort lacks its generated production `Tidepool.Effects.Core`
  environment. Its observed `WorktreeReceipt` export error is setup evidence,
  not a prepared runtime result. Do not substitute the tiny prepared-probe
  `Effects.Core` stub; its include directory was removed from the recipe.

Haskell `execution-schema-projection` passed 1/1. The corpus command completed
and persisted every Suite row; command success means reporting succeeded, not
that the corpus passed. Current workspace compile, fixture freshness, and final
focused reruns remain unverified. This is a WIP review checkpoint, not Wave 4
completion or resident cutover.

- Repr's `execution_schema_contract` integration target: 2 passed, including
  decoding the unmodified regenerated Haskell M3 global record.
- Runtime `session::prepared`: 3 passed; runtime `prepared_execution`: 3 passed.
  These supersede the four runtime failures above without normalizing wire bytes.
- `cargo test -p tidepool-testing --lib prepared_corpus -- --nocapture` through
  `scripts/dev-shell.sh`: 7 passed. The comparator rejects Word64-to-Char
  truncation and surrogate values rather than manufacturing a match.
- `cargo test -p tidepool-testing --bin prepared-corpus -- --nocapture` through
  the same shell: 6 passed. Each corpus row runs in a separate child; stage
  reports are persisted before native execution. Abnormal exits retain the row.
- `cargo test -p tidepool-codegen --lib prepared_program:: -- --nocapture`:
  26 passed, 1 failed. The nested rejection-location test fails schema validation
  with `InvalidScope("value ValueId(1) is out of scope")`; fixture review is
  assigned. An earlier duplicate test-module compile blocker was repaired by
  renaming the inline slow-entry test module, preserving both suites.
- Corpus recipe now orders Project.Work.candidate, actor-used
  Tidepool.Agent.Watch.awaitSettled, then all generated Suite targets. The first
  two have no historical value oracle. Historical Suite expectations are data
  recovered from `e1a4b9145^`, not the interpreter or asks sidecars. Actual corpus
  execution and final freshness/workspace compilation remain pending.

Follow-up coordination record: one runner acceptance review corrected OUTPUT
from a directory to a JSON file and required preservation of projection evidence
on pre-driver failures; one comparator review corrected unchecked character
narrowing; one mechanical test-module repair; one nested-fixture repair assigned.
These are observed correction events, not reconstructed full-wave round counts.

### Trial closing assessment so far

- Clean Luna shapes: recovering caller/ownership inventories and bounded docs
  updates after the contract was fixed. They produced usable artifacts without
  changing shared semantics.
- Needed correction: handwritten schema fixtures and generated-control-flow
  tests. Canonical field order, boolean encoding, postorder indices, and budget
  accounting required explicit review or failing integrated checks.
- Should not have been delegated as a routine fixture repair: an unexplained
  cross-language decoding failure. Normalizing generated bytes in tests hid a
  producer defect; the lead rejected that handback and fixed the owning encoder.
- Terra repaired the emitter and malformed static fixtures after Luna attempts;
  full connected and allocation evidence then passed. Source freezing matters
  independently of build serialization: several builds saw partial edits.
- Missing lead-round totals remain missing; token usage is not exposed here.
  This assessment records observed outcomes, not an invented cost comparison.

Prelude: duplicate and suffix-colliding internal names; stable full/target
identities; dead-end saturation/prefix checks; character constructor/case reps.
Bottoming imports project and validate but do not execute in this closed wave.
Connected: mixed results beyond registers, live caller across callee GC and
live results across later GC; call/case/let through one real adapter; recursive
allocation without intervening host calls; zero fast-path allocation host calls.
Static: cyclic image, nursery-to-static collection, escaping edges rejected,
bad static starts/tags rejected, no partial publication on instantiation failure.
Observation: distinct identities sharing a family-relative tag, deep small-stack
decode, cycles exhaust budget, functions reject, partial-result cleanup safe.

Compile changed targets and workspace tests; run focused checks then
`just changed 08669ea2896d061fa9e7a00a7f32c5e2ca77182f`, `just fixtures-check`,
format checks and `git diff --check` at fold. Record actual failures and exact
commands. Push review checkpoint only after connected execution is demonstrated;
never claim workspace green or production parity from narrow checks.

## Trial record

Count lead preparation/seed tool rounds, assignment, review and correction
messages contemporaneously per parcel. Worker rounds/outcome and disagreements
are separate. Token usage is unavailable unless explicitly exposed by harness.

| Parcel | Lead preparation rounds | Assignment | Review | Correction | Worker rounds | Outcome |
| --- | ---: | ---: | ---: | ---: | ---: | --- |
| P: schema/projection | Initial 3 recorded; later seed rounds not fully counted | 2 | ongoing | 1 Haskell identity correction | Rust 1; Haskell 2 | focused passes; integration pending |

### Current evidence and trial deviations

- Rust schema after host-constructor-ID migration: worker reports 55 lib tests
  and 9 codec/contract tests passed. Haskell projection+encode: 2 suites passed
  through the dev shell after identity-map correction. No fixture regeneration yet.
- Static-image instantiation: 5 heap tests passed. Static-aware collector checks
  and connected integration remain pending.
- Actor test keeps sibling closure/retirement assertions via the current policy
  dispatcher; its compile initially stopped on the in-progress schema.
- First schema and actor workers built without a visible explicit build-slot
  grant. Subsequent parcels were corrected to require a grant; no concurrency
  safety claim is inferred from the initial handbacks.
- The lead implemented more compile-owner/adapter plumbing than the seed needed.
  User called this out; remaining owner wiring, observation and integration are
  delegated. Those commits are work, not evidence that delegation was optimal.
- Lead round accounting was not fully maintained during initial seeding. Do not
  interpret missing counts as zero or invent retrospective token estimates.
  Subsequent review/correction events are recorded below as they occur.
- Emitter draft review 1: require declared Tail convention, untag captures,
  evaluated Enter rather than a call, VMContext top lookup and assigned Construct
  support. Worker is correcting before handback; no tests claimed.
- Static-aware collector integration: all 59 heap library tests passed through
  `bash scripts/dev-shell.sh cargo test -p tidepool-heap --lib`.
- Invocation and emitter first-slice checks passed
  `bash scripts/dev-shell.sh cargo check -p tidepool-codegen --lib`; this does
  not establish connected execution. Five warnings remained at that check.
- Focused connected-test attempt:
  `bash scripts/dev-shell.sh cargo test -p tidepool-codegen --lib prepared_program::tests -- --nocapture`
  executed zero tests: compilation encountered 14 unresolved helper references
  in the in-progress Case/join emitter. The emitter owner retains those repairs;
  assertions were not weakened.
- Invocation review found cursor validation using the initial nursery address
  after moving collection. The owner was directed to validate against the
  current active range before reclaiming the buffer. Verification is pending.
- Latest coordination: test worker received one verification assignment and one
  follow-up coverage assignment; invocation worker received one facade assignment
  and one cursor correction; emitter received CaseTrap approval, one build-slot
  handoff, and one host-target admission correction. These are recorded events,
  not reconstructed totals for the earlier wave.
- Case/join slice subsequently passed `cargo check -p tidepool-codegen` and
  `git diff --check` in the emitter worker's handback. The slice adds typed
  CaseTrap reporting, managed-only Enter results, and native-host admission.
  Execution tests are still pending. Let implementation follows this slice.
- Let lowering is now implemented. Review corrected continuation scheduling
  after allocation and primitive DEFAULT precedence; DEFAULT is a residual
  branch regardless of its position in the alternatives vector.
- Connected test rerun executed seven tests: one passed and six failed. Three
  failures were compile-time Unsupported errors; three returned unexpected
  scalars. The latter fixtures encoded canonical scalar bytes in native rather
  than big-endian order; fixture repair is assigned. No passing connected gate
  is claimed from this run.
- Runtime facade now caches the compiled owner and retains a terminal
  MachineFailure, replayed before subsequent cancellation checks. Its focused
  runtime tests have not yet run. A transient missing import blocked an earlier
  connected rerun and is now present in source.
- Structural allocation evidence gets a test-only, opt-in pre-compilation IR
  capture seeded in `f15734269`; allocation contract assertions are delegated.
- Subsequent connected rerun passed all seven tests after canonical fixture
  bytes and allocator-continuation parameter handling were corrected:
  `bash scripts/dev-shell.sh cargo test -p tidepool-codegen --lib prepared_program::tests -- --nocapture`.
  This supersedes the earlier 1/7 result, not the outstanding broader gates.
- The broader `prepared_program::` filter passed 17/18 tests, including
  `recursive_group_reserves_once_before_sibling_initialization`. The deep
  observation subprocess alone failed with BudgetExceeded at limit 20001;
  fixture node accounting is being checked against the stated observation
  budget contract. Workspace verification and fixture regeneration remain pending.
- Deep-observation fixture review found 20,000 parent constructors, one leaf
  constructor, and one scalar: 20,002 budget units, not 20,001. The fixture now
  derives `depth + 2`; production budget enforcement and cycle-failure coverage
  are unchanged. Its rerun executed no tests because sketch retirement was
  temporarily between deleting source files and removing module exports.
- Prepared fixture regeneration is separate from `just fixtures-update`:
  `execution-schema-projection` writes `test-prepared-stg/fixtures/m3-vertical.cbor`;
  the just recipe owns the semantic suite corpus and source fingerprint only.

### Schema-5 observation identity

Constructor records append the existing host DataConId minted by
`Tidepool.Identity.varId (dataConWorkId con)`. Rust validates uniqueness and
observation uses the descriptor-to-declaration mapping. This avoids a second
hash implementation or treating family-relative constructor tags as identities.

The trial closing assessment above records the observed delegation outcomes.
No reconstructed token or cost estimates are available.
