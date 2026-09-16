# STG continuation: ownership and completion gates

This is the current execution map after recovering the interrupted Claude
session. `plans/stg-completion.md` remains the governing completion contract;
`next-wave-2026-09-15b.md` supplies the F/S work breakdown. This map records
current evidence and narrows the next assignments. It does not declare step 2
or the cutover complete. Schema 10 landed as `cf54ed3a0`; the branch head also
contains `3f330a41d` for the independent child-budget rendering cleanup.

## Recovered state

- `b8d25637f`: F1, engine selection inside the resident session.
- `7b136ecbb`: F2, pattern binds and cell display on the prepared route.
- `29602fe8c`: fake extractor emits the prepared artifact required by the cache
  property harness. The current continuation has not rerun that property suite.
- `aad9f184b`: actor-exit publication owner, already committed before recovery.
- `b01b29908`: recovered S1 work. Join visibility uses its declared contract;
  request decoding avoids an eager allocation from an untrusted field count;
  harness and self-harness turn requests carry the session's retained bindings.
- `30b4d0b75`: constructor host IDs are unique across the machine interner,
  including atomic batch absorption. Prepared freer lookups use defining
  module and occurrence and refuse ambiguous package identities.

The interrupted public-export pruning was reverted. The interrupted retirement
fixture edit would have replaced `ActorRunTarget::retire_placement` with a
lower-level runtime call; it was reverted to preserve the existing proof.
`PreparedRuntime: ActorRunTarget` remains a temporary test compatibility path.
Its deletion depends on a replacement through `ResidentSession` after F4.

Preserve the pre-existing untracked `examples/guess/` and
`haskell/dist-newstyle-wave4-haskell/`. Nothing has been pushed.

## Work ownership

### Current wave boundary

The user requested an economical Terra implementation/orchestration wave before
switching subscriptions. Finish already-decided schema-10 changes, focused
verification, fixture regeneration, and the handoff. Do not resolve new
structural questions by extending the design during this wave. Record the
source evidence, blocked acceptance criterion, and reason review is needed;
continue independent work.

Deferred to the next structural review/implementation wave:

- **F4 frame and site ownership:** evidence owner and runner can be different
  programs. The proposed contract is in `designs/prepared-resume-integration.md`;
  choosing the final API affects roots, installation rollback, and retirement.
- **F4 generated settlement and ordinary replies:** auxiliary executable roots,
  request forcing, and synthetic reply evidence must agree with the production
  effect generator. A Bool-only shortcut would leave ordinary handled replies
  without authoritative types.
- **F5 answer construction:** allocation rollback and frame consumption order
  need engine-invariant review before implementation; invalid answers must leave
  the parked frame and resource counts intact.
- **F6 residency:** program identities, per-program roots, quiescence, live-header
  census, collection, and retirement are coupled ownership changes. Fixture
  updates cannot establish bounded residency.

These deferrals do not close step 2 or authorize prepared default routing.

The lead owns structural decisions, producer/consumer agreement, integration,
and acceptance against the production owners. Execution agents get bounded
file ownership, a fixed shared contract, exact tests, and a stop/report rule
for invariant surprises. The current tool session offers Sol Medium for those
execution assignments; Sonnet is unavailable here. Astra consultations review
consequential design questions.

Current independent parcels:

1. **Haskell F3 producer:** `EffectSchema`, `TypePolicy`, `PreparedSites`,
   `PreparedStg`, `GhcPipeline`, execution schema/encoder/projection, and focused
   Haskell tests. Contract: `designs/schema-10.md`.
2. **Rust F3 consumer:** schema types, decoder bounds, validator, authored wire
   fixtures, and mechanical propagation of the two new program fields.
3. **Lead analysis:** F4/F5 ownership and integration, with a read-only review
   of cross-program site provenance and settlement entry retention.

CPU-heavy checks are serialized. Neither the schema version bump nor one
side's unit tests constitutes an integrated F3 pass. The Haskell producer,
Rust reader, regenerated binary fixtures, and canonical fixture check must
agree before that wave lands.

## Structural decisions fixed before F3 implementation

The recovered schema draft had five errors. The reviewed
`designs/schema-10.md` now specifies:

- Delivery-specific evidence from explicit typed verb strategies. A wrapper's
  return type is insufficient: `forkCata` internally resumes with a list;
  `serve` resumes with state; exit-cell delivery and terminal capture have
  different evidence meanings.
- Ordered type arguments, including phantoms, in Data nodes. Constructor field
  shapes alone cannot distinguish `P Int` from `P Bool`.
- A one-to-one source/runtime field mapping for the first construction slice.
  Unsupported unpacked layouts are explicitly unconstructible.
- Bounded normalization and type expansion, including recursive newtypes and
  nonregular recursive datatypes.
- Constructor closure for Text/Integer/Natural leaves, plus reachable-owner
  filtering and separate prepared metadata that leaves the Core sidecar intact.

Unreduced type families and opaque type equality remain explicitly limited.
Those limits must not disappear from the parity checklist when Bool works.

## Critical path to completion

| Gate | Concrete proof | Work that follows |
|---|---|---|
| F3: authoritative site evidence | Real prepared Bool site, complete constructor closure, schema-9 refusal, codec/validator negatives, regenerated fixtures | F4 parking and shared settlement |
| F4/F5 first slice | A production `ResidentSession` turn parks on `runLLMTurn @Bool`; a valid answer completes; invalid answers preserve the same parked frame | General answer and delivery coverage |
| Step 2 integration | Declare, retain/PAP, effect, sibling turn, resume, lookup; cancellation, committed-prefix failure, stale incarnation and sibling retirement through real owners | Wave A broad gate |
| Step 3 residency | Quiescent retirement and full collection; escaped values remain callable; repeated turns beyond the old slot budget have bounded live counters | Default-routing eligibility |
| Step 4 parity/default | Notebook dialect, production effect deliveries and answer forms pass; fresh production sessions use prepared execution | Delete Core and migration paths |
| Step 5 deletion | One production engine and notebook route; obsolete adapters/tests/config removed and the final gate passes | STG complete |

F3 is complete. Pure prepared notebook execution is implemented;
production effect suspension is still refused. Prepared execution must not be
made the default while effect routing or bounded residency is incomplete.

The Bool first slice does not satisfy all of step 2. Outstanding scope includes
structured and byte-backed host answers, managed/framed answers, live reentry,
exit-cell fill, terminal capture, and production authority/recovery behavior.
The first residency slice similarly does not establish external-storage or
parked-frame lifetime coverage by itself.

## Lead decisions for F4/F5

The source-reviewed refinement is [prepared resume integration](designs/prepared-resume-integration.md).
Before implementation, establish these facts in the owning APIs:

- A site reached by calling a retained closure may belong to an earlier
  program. The currently invoked program is not evidence of site ownership.
  Resolve installed site evidence with an explicit ambiguity/conflict policy,
  and retain its owner on the parked frame through resume and retirement.
- Use the existing `ResourceLedger` for continuation identity and consumption;
  extend frame evidence rather than creating a second registry. Every failure
  before take leaves roots, handles and the frame unchanged.
- Keep one settlement/completion routine for initial and resumed execution.
  The existing `Settled` scaffold and `resumeLifted` definition are present;
  their presence in source alone does not prove executable entry retention or
  row-specific request forcing after resume.
- Notebook bindings, generations, scope and completion obligations stay with
  `PersistentSession`/`ResidentSession`. Machine code/heap ownership stays
  with `PreparedMachine`; remove the temporary duplicate runtime only after
  its production-consumer replacement is proven.
- Put engine provenance into an existing owned receipt or provenance record,
  with an explicit internal schema decision. Do not encode control state in
  transcript strings. Shoal's Core-only bootstrap remains a default-routing
  dependency until the effectful prepared path is supported.

The F4 source survey also establishes a separate ordinary-effect obligation:
Print/file-read/KV handler replies have no dynamic typedSite. The resume
integration design reuses schema-10 evidence via explicit prepared-only
synthetic sites generated from each verb's full result type. Generator forcing
must also preserve the distinction between CoreValue and policy-authorized
live payloads. Both are included in full step-2 acceptance, beyond Bool-first.

## Verification retained during recovery

These checks ran before schema-10 mutation began:

- New interner collision tests: 3 executed and failed as expected before the
  fix (`/tmp/tidepool-interner-before.log`). Final interner tests: 6 passed,
  together with 4 emitter policy tests in the focused codegen selection.
- Extractor request tests: 2 passed (impossible collection counts and one-byte
  payload-free tags). Avoiding eager allocation is code-reviewed; this is not
  an allocation-count regression proof or a complete request-memory bound.
- `cargo clippy -p tidepool-codegen --lib -- -D warnings` and the corresponding
  extract-cmd check passed in the repository environment. Existing C signedness
  warning remains outside Rust clippy diagnostics.
- `nix develop --command cargo check -p tidepool-harness --all-targets` passed;
  harness changes compiled, without executing effectful prepared routing.
- `bash scripts/dev-shell.sh cargo test -p tidepool-repr --lib freer_names::tests`:
  3 passed.
- `bash scripts/dev-shell.sh cargo test -p tidepool-runtime --test prepared_execution freer_resume_loop_drives_qapp_to_completion_via_managed_resume_arguments -- --exact`:
  1 passed.
- `bash scripts/dev-shell.sh cargo test -p tidepool-runtime --test prepared_resident_composite session_registry_drives_prepared_runtime_through_bind_import_park_resume_cancel_and_retire -- --exact`:
  1 passed. These two are fixture/runtime evidence, not the production step-2
  acceptance scenario. Log: `/tmp/tidepool-freer-names.log`.
- Changed Rust formatting and `git diff --check` passed.

No new broad gate has run. The next broad `just verify` follows the integrated
Wave A exit; solo-rerun timeouts before attributing them to load.

## Schema-10 fixture wave

- `nix develop --command bash -lc 'cd haskell && cabal test
  execution-schema-encode --builddir=dist-newstyle-schema10
  --test-options="--write-schema6-fixture
  test-execution-schema-encode/fixtures/schema6-intrinsic.cbor"
  --test-show-details=direct'` passed (one test) and regenerated the encoded
  schema fixture. The focused prepared-STG pipeline then passed; the M3,
  Freer retention/resume, and three import artifacts were regenerated from
  their documented generators, with the Freer manifest identities checked
  before copying.
- `nix develop --command bash -lc 'env RUSTC_WRAPPER= cargo test -p
  tidepool-repr --test execution_schema_contract'` passed (7 tests), and the
  prepared-turn complete-site-family metadata test passed (1 test).
- `just fixtures-update`, native-oracle reseal through
  `nix develop --command scripts/prepared-corpus-oracle.sh update <manifest>`,
  and `just fixtures-check` passed. The final prepared-corpus Suite run is at
  `target/prepared-corpus/run.sCjaiw`: 812 projected/validated/admitted/
  compiled, 702 executed with zero failures, and 234 comparisons with zero
  mismatches.

### Producer integration and metadata invariant

- Prepared projection returns the final constructor table with the wire
  program. `Main` prepares selected artifacts before the shared Core metadata
  write; `Artifacts` merges those constructors and their siblings into
  `meta.cbor`.
- The pre-write metadata contract now also checks every constructor admitted by
  prepared projection. `TIDEPOOL_TEST_DROP_DC` therefore fails before writing
  if it removes either a Core-emitted or prepared-admitted constructor.
- Type-evidence graph lowering indexes each module graph once with `IntMap`.
  Reachability and lowering use indexed lookups, and invalid graph references
  return the existing `UnsupportedPreparedShape` error through projection
  rather than raising a pure exception.

### Focused producer checks

- `nix develop --command bash -lc 'cd haskell && cabal --builddir=dist-newstyle-schema10 build tidepool-extract-bin && cabal --builddir=dist-newstyle-schema10 test execution-schema-encode'`: executable build succeeded; encoder test passed (1/1).
- `nix develop --command bash -lc 'cd haskell && cabal --builddir=dist-newstyle-schema10 test prepared-stg-pipeline-test'`: passed (1/1). GHC emitted existing simplifiable-constraint warnings in generated test sources.
- `git diff --check` passed after the producer changes.
