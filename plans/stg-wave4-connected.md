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
  receives one value while asserting zero. These four observed runtime failures
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

Closing summary pending: shapes handled cleanly, corrected, unsuitable for
delegation, with one-line reasons. No reconstructed token or cost estimates.
