# G2 TYPE frontier: Argon checkpoint

Review candidate: `c947ed5d1ef8ba31bbc50ecef723a932f958f5c8`.
Base: `a57377524cce6fa9179fac70316524efadbee44a` (code `5e10df3bef`).
Branch: `integration/g2-domain-runtime-20261023`.
Checkout: `/srv/swarm/checkouts/tidepool-domain-runtime-join-20261023`.
Evidence: `/srv/swarm/evidence/inanna/g2-type-frontier-repair-20261024/`.
Package projection evidence remains under
`/srv/swarm/evidence/inanna/g2-value-type-20261024/frontier-b/`.

This is a mid-effort review checkpoint, not G2 acceptance. The early joined
pair ran at `41a21ea7b6c4cdf276150fcebb9a7896ba13a28e`, before the later
cohort-authority and package repairs. Both cases progressed beyond their old
TYPE failures but still failed later assertions. The integrated candidate has
not yet passed that pair or the five notebook histories.

## Review priorities

1. Review the new current-cohort producer handoff, including native witness
   identity. Independently assess the adjacent `authored_promotion` refusal;
   its exact-base comparison is still pending.
2. Diagnose the operand retirement timeout after successful execution. Determine
   which owner remains or which typed retirement check refuses before proposing
   a lifetime change. The timeout alone does not establish a leak.
3. Adversarially review package product isolation and admitted package-fact reuse,
   especially portable final projection, physical aliases, and current package
   validation. Preserve all exact scope, issuance, code and cleanup guards.

## Implemented repairs

**C — immutable facts versus producer authority.** `issue_selected_custody`
canonicalizes and validates the retained metadata merge before recording the
incoming producer facade. It adopts only the retained immutable facts owner,
preserving incoming Original admission and transport. The analogous general
`append_produced_interfaces` route now correlates its selected entries with the
finalized binding and acquired facts. Controls cover independent equal fact
allocations and the real producer authority routes.

**A — stage-only operand TYPE.** `WorldNativeDemandResolver::new` retains the
World primary unchanged. Existing checked-value authority still borrows the
operand's complete TYPE closure into each stage. The actual World-selection
control now asserts both primary absence and exact stage presence.

**B — reusable package products.** `projectPreparedPackageGroupsWithIdentities`
neutralizes entry and auxiliary roots only for standalone package definitions.
`demandPackages` uses package roots for discovery and the original consumer
context for native final emission. Portable final projection also reuses that
original context with the completed recovery and reachability; recovery is not
restarted. Auxiliary result evidence strips leading forall binders before
checking free result variables. Genuine concrete consumer answers remain covered.

**Package sidecar reuse.** `WorldAdmittedRow` retains fully forced
`PackageImportEvidence`. Selection carries the actual admitted rows after exact
binding/semantic agreement, then uses their package facts while still validating
captured tokens/seals, installed interfaces and current package selection. There
is no second cache or new independent retention lifetime.

**Decoder correction.** The reported successor reset is absent at the reviewed
source: `commitWorldInputRetention` already preserves the decoder and prunes
retained-original seals. A distinct possible omission of live World-only seals
was found. Safe repair needs the World cache owner's live custody contributions,
wrapper rebasing and accounting; simply retaining more seals could retain stale
uncharged bytes/paths. This is deferred, with no decoder change or speed claim.
See `haskell-inputs/decoder-retention-investigation.json`.

## Newly reached boundary: current-cohort producer authority

The early nominal actor history now succeeds through ordinary private reuse,
private promotion, fresh public reuse, semantic assertions, primary-isolation
assertions, and a later captured declaration publication. Its final fresh thin
consumer refuses at `WorldStageNativeProducts::capture`:
`World stage completed product has another physical issuer`.

The guard compares artifact IDs and `same_issuing_authority` between the completed
own offer and the captured producer certificate. The execution does not identify
the individual failing disjunct. Source inspection found this concrete loss:

- Initial `ArtifactAdmission.supplied` preserves the incoming producer while
  sharing the retained immutable facts/native witness.
- Current-cohort classification reads `physical.metadata_snapshot()` through
  `complete_native_entries`, which can recover the prior equal Original facade.
- Reissuing custody from those classified entries loses the incoming admission;
  `IssuedNativeOffer::issue_custody` then adopts that old supplied facade.

Candidate repair (`b91819fcca`, corrections `2c520a63df`/`fe4371867b`) makes
`issue_current_cohort_availability` consume the actual `ArtifactAdmission`.
Classification and availability checks remain physical. Each classified current
Original must match the admitted supplied metadata, and custody issuance receives
that reconciled incoming facade. It never substitutes raw producer certificates.

New real-source regression:
`artifact_inventory::tests::current_cohort_preserves_incoming_original_beside_equal_prior_authority`.
It checks incoming admission, shared immutable facts and identical native witness
Arc before issuance, then own-offer and compiler-selected Original authority.
Owned-resident routing is registered. Production and cfg(test) compilation passed,
and this control executed 1/1 successfully at `f50e098a66`; two earlier compile
errors were corrected and retained. No joined success is claimed for this repair
yet. The test-only fail-before run is in progress on preserved branch
`e995e3f666` (parent `8aa9096080`); no outcome is claimed here.

Adjacent observed refusal at `6fbc37dc6e`:
`artifact_inventory::compiler_projection::acquisition_tests::authored_promotion_preserves_actual_offered_acquisition`
fails during initial checked-input construction with
`CompilerEvidence(Mismatch("native row differs from authenticated scope"))`.
The physical row guard requires exact group Arc identity. Whether this is latent,
a regression, or fixture construction is unresolved; do not infer from chronology.
Evidence: `rust-ca/producer-controls/`, case
`b1fb396207929216caf1752e78eb74d8e0cc1896b491272cd6b08fac99c412a7.json`.

## Newly reached boundary: operand retirement

The early operand actor case now passes actual operand admission, the non-primary
`Tidepool.Aeson.Value` TYPE premise, native preparation and committed execution.
After `drop(dependencies)`, its existing ten-second retirement wait times out:
`ephemeral host input retires after its dependencies release`.
The subsequent public-unchanged assertion is not reached.

Source leads, not a root-cause verdict:

- `PrivateHostBindingLease::drop` queues the exact write receipt.
- `Resident::settle_dropped_custody` retries private-host retirement and retains
  any refused receipt; the `Err(_)` branch currently does not expose the cause.
- Production publication has explicit `retire_input_for_publication`; this test
  exercises dependency Drop. An existing JSON operand control uses the same recipe.
- A remaining intentional owner, missing last Drop, and an exact retirement
  refusal still need to be distinguished. Do not increase the timeout or remove
  the assertion to treat successful execution as successful lifecycle completion.
- `RuntimeCellAdmission.compiler_basis._operand` holds a separate runtime binding
  lease. This alone does not explain a current name: `BindingTable::retire_owner`
  removes the current-frame name before deciding whether leased data survives.
  The new control also performs explicit parser-plan admission; its retained
  owners versus the older JSON control deserve comparison.

## Evidence ledger

| Scope | Exact source | Executed result |
| --- | --- | --- |
| Constructor/cache compiler-free controls | `6fbc37dc6e` | 30/30 passed |
| Actual producer controls | `6fbc37dc6e` | 3/4 passed; authored-promotion refusal above |
| Actual operand stage/primary control | `8aa9096080` | 1/1 passed |
| Package context and answer-evidence controls, production unchanged | `ca803756a7` | Each executed and failed as intended |
| Repaired package controls | `802c88fdc7` | 2/2 passed; production worker and test compile passed |
| Admitted package facts and TYPE lifecycle controls | `b388546f2d` | 4/4 passed; production worker and test compile passed |
| Early joined nominal/operand actor pair | `41a21ea7b6` | 0/2 passed; later boundaries above |
| New current-cohort producer control | worker `f50e098a66` | 1/1 passed; production and cfg(test) compile passed |
| Compiler corpus | integrated `70e11ee38e` | Running; no result claimed |
| Build-generator unit checks | candidate before final test-only premise | 88/88 passed; Haskell generated-resource check passed |
| Integrated actor pair and notebook five | current candidate | Not yet rerun |

The package control uses one recovered monomorphic `itos` component and an
unchanged reached census, so the forall fix cannot mask the context bug. Baseline
transitive SHA256 was `c5dfea8b1ab5a20607db8f3baafb7d9283c870688b3f3a1e7018e8f60a9cc377`;
adding `itos` as an auxiliary changed the old bytes. Repaired projections all
match that transitive hash. Independent source review accepted the bounded
package repair at `70e11ee38e`; it did not run tests.

`haskell-inputs/result.json` records four tests, exact commands, artifact hashes,
and confirmed owned daemon/forkserver shutdown. Early actor receipts record
process cleanup and compiler cleanup confirmed, hosted cleanup not observed.
Those runner observations do not resolve the failed ephemeral-binding assertion.
Actor executable SHA256:
`f6720c5d8c525683f432931fcae4a00129819e477cfe63b804d71419b3bdb207`.
Nominal receipt: `actor-ca-early/4301afca863f92aa3a4e41a9ad345047e4679ee416570e1c9b483756f9ab0268.json`.
Operand receipt: `actor-ca-early/287277109b343c0349a03622fd45412df06872de4f8e58c83a1b84c997d33db8.json`.
Consolidated early actor source, commands and ownership observations:
`actor-ca-early-controller/result.json` and
`actor-ca-early-controller/retirement-source-review.txt`.

## Continuation

Finish the already-running corpus, retaining outcomes at its actual revision.
Resolve the producer/native-witness and retirement
findings through owning entry points, then rerun the genuine actor pair and all
five notebook cases on one integrated candidate. Frozen-bundle G2 qualification
remains separate and uncompleted.
