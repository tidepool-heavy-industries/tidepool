# G2 SOURCE-owner integration: Argon review checkpoint

## Candidate and scope

Integration branch: `integration/g2-domain-runtime-20261023` in
`/srv/swarm/checkouts/tidepool-domain-runtime-join-20261023`.
Code candidate: `bff9e561755246ace2fe8ff139441844c4067a04`.
Reviewed-plan baseline: `594f110a8b812c829814bc184d89d7c9a4e4f6c9`.
Evidence root: `/srv/swarm/evidence/inanna/g2-source-owner-20261024/`.

The bounded G2 Rust/Haskell changes, Q9, Q10 and Q6 are integrated. They are
not G2/G3 qualified. The actual actor run is deliberately pinned to the earlier
joined G2+Q9 candidate `36c0873bbcbe981f433ec84d57ced935bb814920`; it does not
establish execution of the later Q6/Q10 changes. No bundle or Persona was started.
Please review read-only and return source-grounded findings and bounded repair
options. Preserve exact selected issuers, historical edges, ordered slots,
SOURCE admission, and the distinction between availability and execution.

## Primary question: ownership after concurrent publication

The group-46 failure is no longer the first actor boundary. The following case
now reaches the final value check after both parked cells have completed:

`resident_local_actor::reload_progress::unavailable_reload_leaves_parked_cells_and_control_independent`

Execution at `36c0873bbc` failed with:

```
extractor contract failure: artifact inventory: acquired HOME binding has competing physical occurrences
```

Case receipt:
`joined/actor-36c0873/0bcd8106e3a311c327afcd2e909939f2af58040891d3682ae6890f2744d0cb22.json`.
Its `artifacts/compiler-transactions/` retains these authored/scaffold histories:

| Transaction | Actual operation |
|---|---|
| `.tmpJewFrf` | Seed `x <- pure (0 :: Int)` |
| `.tmp0zAElq` | Capture `oldA <- pure x`; park a command; eventually publish `x = 11` |
| `.tmp9NwCiQ` | Capture `oldB <- pure x`; park a command; eventually publish `x = 22` |
| `.tmp8Bc9YZ` | Independent `c <- pure (42 :: Int)` while the other cells are parked |
| `.tmpjSWOb2` | Final `x == 22 && c == 42 && oldA == 0 && oldB == 0` check; fails |

The reload invocation and status assertions pass before that final read. The
failing transaction's stderr retains `NativeDemandRejected NativeInputConflict`;
the host retains the more specific acquired-HOME error. Process and compiler
cleanup are confirmed. Hosted cleanup is `not_observed`, and the runner marks
`diagnostic_evidence_complete: false`; do not convert those into stronger claims.

### Directly verified source relationships

- `artifact_inventory/compiler_projection.rs::AcquiredNativeClosures::group_owner`
  searches held closures for the same exact `NativeGroupBinding`, then refuses
  when their `PendingCertifiedGroup::same_issuance` differs. It also separately
  checks linked code identity. The observed failure is the former check.
- `certified_products.rs::PendingCertifiedGroup::same_issuance` compares origin,
  owner, and pointer identity of both `group` and `imports`. Equality of bytes
  or scalar binding alone does not satisfy this contract.
- `OriginalValidationInventory::selected_scope` reuses an issued row when
  supplied; otherwise its `current` baseline preserves a prior product. With
  neither, it builds a new product using `imports.pending` from validation.
- `declaration_context.rs` native receipt certification gets its baseline from
  acquired closures plus rows already acquired in this demand sequence. The
  newly integrated SOURCE observer does not mint selected native rows.
- `AcquiredNativeClosures::append` preserves distinct actual closure owners,
  including equal recorded closure identities. Completion publication reaches
  it through `with_linked_native_target` / `with_native_completion_from`.
- `linked_native/restore.rs::restore_acquired_native_closures` separately calls
  `acquire_linked_home_grants` for each closure. Whether restoration participates
  in this particular live failure is not yet established.

### Hypotheses to distinguish, not accepted diagnoses

1. Two legitimate requests starting from the same seed independently certify a
   previously unselected group from the same Full offer. Their ordered-import
   allocations differ; completion joins preserve both, and the later lookup
   treats them as competing physical issuances. Identify the exact binding and
   both issuance paths before accepting this explanation.
2. An intermediate capture, materialization or restoration drops the real issued
   row and reconstructs it even though an existing owner should have survived.
   Repair that transfer rather than adding a cache or weakening all identity
   checks.
3. There is a genuine conflict in selected dependencies or code. The refusal
   must remain; locate the earlier selection/join mistake instead.

The present error omits the conflicting binding and issuance origins. If another
run is necessary, add bounded normal structured tracing at the owning boundary;
do not use println debugging or log full inventories. Ask whether the contract
intends physical identity across independent authenticated acquisitions, and
where that shared identity is actually issued. Do not blindly replace Arc
checks with equality or drop one closure at merge.

Please recommend a small production-path reproducer with two overlapping
independent requests and a subsequent consuming request, plus a real competing-
issuer refusal. Existing tests of custody unions alone do not exercise this join.

## Secondary question: discriminating SOURCE fixture construction

The core repair is integrated as `36c0873bbc`; acquired-scope test repair is
`807be7c470`. The five old passive-view group methods and the test containment
backdoor are deleted; lower view helpers are private. Contribution queries retain
exact membership; selected-view queries do not widen to custody. All
`ArtifactView::from_custody` production transitions remain unchanged.

Executed SOURCE/scope and certification controls pass, but the locked combined
Full-offer + absent-passive-group + nonempty-retained-terminal control remains
unproven. This is an acceptance gap, not a reason to manufacture certificates.

What the implementation worker learned:

- The pre-existing publication fixture links the queried group. Even an
  unreferenced exported body in the attempted ordinary execution construction
  can remain in the actual observer's passive custody. Inspect
  `membership.acquisition().artifacts()`, not a broader ancestor view.
- An ordinary SOURCE module importing a live `Val.G*` failed before observation
  because that source qualification did not have the selected value's HPT owner.
  Making the terminal a direct later cell demand did not repair that premise.
- A three-step construction successfully produced a real value, compiled a
  real declaration capturing it, and published that declaration through
  `certify_publication_join` and `CellProgram::declaration_item_successor`.
  A later ordinary SOURCE parent failed during preparation before observation.
  That attempt mistakenly disabled compiler capture, losing its underlying
  stderr. An exact repeat with the G2 Haskell cause-preservation repair and
  diagnostics capture is in flight; no root cause is claimed from `Corrupt`.
- An attempted fresh receiving projection using
  `CompilerInputProjection::admit_interfaces_with_native_offers` and existing
  offers refused `native namespace pair differs from its issuing bindings`.
  The worker did not rewrite roots, synthesize bindings, or weaken admission.
- Independently recovered issuers in the attempted competing control yield
  partial offers, not Full. Full must come from actual completed-stage issuance.

Preserved attempts and exact reports are under `rust/`; the failed three-step
source fragment is `rust/world-offer-authored-child.fragment.rs`. Ask which
existing owner construction supplies the required relationship. Consider a
smaller real notebook-history consumer if the initially proposed isolated fixture
cannot express the contract. Partial/ambiguity SOURCE-path and out-of-contribution
terminal controls must remain explicitly pending until executed.

## Other integrated parcels and bounded follow-ups

- **G2 Haskell** (`fa20d9623a` through `eae5c40fab`): seven distinct focused
  cases pass. A counterfactual removing only the new native-failure catch makes
  the live-load regression fail by losing the typed native cause; restoring the
  exact committed bytes passes. See `haskell/report.json` and mutation evidence.
- **Q9** (`35fa315631`): production/test compilation and seven actual GHC
  discovery cases pass. See `q9/result.json`. Absence, unavailable information,
  checked-local lookup and legitimate post-lookup filtering remain distinct.
- **Q10** (`a0f3c39355`, `2fea57769e`): nine attribution controls pass. The first
  real history attempt could not start without declared service resources;
  `53a864772b` registers the named owned-service target. Its next execution found
  the shared test helper still reading deleted `lifecycle.json`; `7a7619c830`
  uses the actual live schema-2 `owned-service.json`, validates its exact owned
  child identity, and passes its new focused control. The joined history is now
  running with both G2 halves. Exact later-cell attribution is not yet proven.
- **Q6** (`e1d108e3e0`): nine focused ownership/pre-write controls and three
  real checked-worker cases pass at worker candidate `f6948bcb4a`. The inherited
  source-free original-projection test still refuses `ExecutionSourceUnavailable
  main:ProjectionIndependent`; this reproduces without Q6 at `082f4925d1`.
  Its lexical-only fixture imports authored SOURCE with no current source
  admission. Preserve the refusal; proper production fixture migration is open.
- A real activation-preview case first failed at group 46 without the Rust
  repair. At joined worker `42aa786f7f` it moved beyond that boundary and exposed
  a fixture owner mismatch before preview compilation. The helper opens
  `admission.source_lease()` (mounted target), while production
  `resident_workbench.rs` opens `admission.original_source()`. The checked
  renderer explicitly requires the latter and separately admits the target
  lease. The helper correction is integrated as `bff9e56175`; the full rendering
  assertion is unchanged. Worker execution now reaches `activation_preview`,
  exits the Haskell worker successfully, and emits `activation-preview.cbor`.
  The full case still fails later in Rust, as detailed below.

### Additional real consumer: activation-preview package ownership

The corrected helper's run (`q6/runtime-preview-original-owner.log`) executes
one case and passes zero. The Haskell request succeeds, then Rust refuses:

```
HOME package slot lacks current or exact held occurrence
```

Receipt directory:
`q6/runtime-preview-original-owner/123b101e5ec227983a9d24cca245ed34a28eebf53f62f42261d2bd62b86652a0/artifacts/`.
The preview artifact is retained under
`compiler-failures/.tmpzcpp7S/activation-preview.cbor` (554 bytes, SHA256
`5ad342c77520730904612b62f3e239214ee141ee669c3f3d9f5384be05fc385d`).
This is compiler-output evidence, not a successful rendered-value assertion.
See `q6/preview-source-owner-finding.json` for the full original-source/mount/
admission/transaction trace and exact worker source.

The refusal is in `compiler_projection.rs::link_native_target`. For a HOME
package slot it first looks for an applicable current HOME manifest choice or
exact Original package-slot choice. Without either, it requires an exact held
acquired import at that slot; this fallback is absent in the failing run. This
can be missing producer metadata, lost historical code custody, or a caller
choosing an unsupported projection; the error alone does not decide which.
Please trace that contract alongside the concurrent-publication failure without
assuming they share a root cause. Do not choose packages by broad symbol search.

## Execution and continuation

Actor command (warm validator frozen at `36c0873bbc`):

```sh
swarm-build just test-native //exomonad/actor:actor_notebook_contract_tests \
  --jobs 1 --timeout 600 --delegated-service \
  --service-slice tidepool-completion-build.slice \
  --output-dir /srv/swarm/evidence/inanna/g2-source-owner-20261024/joined/actor-36c0873 \
  --retain-artifacts
```

Build ID `ae0ce9e2-2db6-41ef-ab9c-de76438780ce`; actual per-case receipts and
logs are under `joined/actor-36c0873/` and `joined/actor-36c0873.log`.
Final first run: **five executed, three passed, two failed**, exit 1.
`joined/actor-36c0873-summary.json` links every case receipt. The concurrent-
publication failure is above. The other failure,
`resident_captured_source_retains_the_parked_job_for_a_later_cell`, first panics
at `known command marker`, then times out waiting for command admission. Its
shared Haskell fixture sends final argument `"1"`, while this one keyed backend
was initialized with `"captured-job"`. The independent generic caller of that
fixture is unkeyed and remains unchanged. Commit `d01a007676` aligns this test's
keyed backend with the actual authored argument; it changes no production code
or retained-job/completion assertions.

The validator is now frozen at full candidate `bff9e56175`, running the same
named five-case cohort with that fixture correction and all integrated parcels.
Log: `joined/actor-bff9e56175.log`; output: `joined/actor-bff9e56175/`.
The rerun is pending, not counted as passing evidence. The named cohort owns its
resources and does not accept an appended single-test selector.

The Rust worker is finishing the bounded evidence-recovery run; Q6 is closing
out at its new documented boundary; Q10 is running the real joined history at
worker `50028e22170b73ebe7fd05e2eb3176e992ad0b91`. Preserve
their WIP and evidence; do not switch active checkout branches or build sources.
Main/release delivery, the three capture-history reruns at the consolidated
candidate, remaining `g2-completion.md` gates and matched frozen-bundle/Persona
qualification remain pending. Integration-status has matching Harness pins but
reports `local_main_missing` in this campaign checkout.
