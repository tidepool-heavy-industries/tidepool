# Current implementation and qualification

Updated 2026-10-23. **Regional closeout is open; G2 and G3 are unqualified.**
Integration: `/srv/swarm/checkouts/tidepool-domain-runtime-join-20261023`, branch
`integration/g2-domain-runtime-20261023`. `LIVE-COMPLETION.md` owns accepted
boundaries; `REGIONAL-GATE.md` owns qualification before the next G2 run.

## Observed checkpoint

At `3e177d665a`, all 32 selected production/test consumers compiled. The regional
queue subsequently attempted **120 distinct leaves across `3e177d665a` and
`4967e50f31`: 101 passed, 19 failed** (89 Rust and 31 Haskell). One failure was an
invalid compiler-diagnostics setting in a lifetime measurement; its corrected
retry is pending. These counts describe a mixed-revision investigative sweep,
not a single passing joined candidate. All 13 distinct paired compile contracts
have positive/refusal evidence across recorded pins, including the repaired
mutable-session pair at `4967e50f31`.

Exact commands, discovered/executed counts, artifacts, hashes and failures:
`/srv/swarm/evidence/inanna/c-ownership-20261022/validation/4967-regional-checkpoint.json`.
Rust process and resident compiler cleanup were confirmed. Haskell launcher exits
were retained; they do not provide a separate teardown receipt. The validator
released `4967e50f31` cleanly with no active clients.

The next warm checkpoint is `35e077d2c5`: 34-consumer compilation (previous 32,
plus host library and unit tests) and the package refusal diagnostic. Later
reviewed repairs are integrated through `73f68b0d56`; they require a subsequent
immutable validation pin. Do not relabel older artifacts with the current HEAD.

## Purge and preserved boundaries

The live notebook is World-only. The finite notebook entry points, checked-cell
DTOs and runtime check path, Rust/Haskell `--cell` transport, request field 31,
finite recipe/state/item receipts, fallback reconstruction and exclusive test
issuers are deleted. The fabricated successful SOURCE promotion factory was
removed after its real replacement passed. The bounded joined source audits
found no surviving finite notebook route; regional qualification remains open.

Ordinary native compilation for bootstrap, unprepared installers and package
Original entries, parser-only `--cell-plan`, and activation preview are supported
production consumers. They do not restore the deleted notebook implementation.
`TurnRequest.target` was an unsupported leftover and is now deleted, including
47 empty caller fields. Target selection is restricted to Source requests at
request admission; the legacy CLI option is explicitly refused.

The immutable-session compile fixture was a stale caller, not an alternate API.
Both session facade and state owner require `&mut self` and `SettledCellProgram`.
The positive pair now compiles and the negative pair still refuses raw
`Arc<CellProgram>` at the intended boundary.

## Regional failure disposition

These are source-reviewed repairs unless execution is explicitly stated.

| Boundary | Finding and repair | Remaining evidence |
|---|---|---|
| Parallel SOURCE discovery | Live Make child threads hit the handler-only ThreadId check. Request/stage-bound SOURCE-only borrowing serializes complete frames and basis union; native acquisition stays handler-owned. | Compile; transport/cancellation controls; real-row memo coalescing; mixed-stage, preview and cached-HOME histories with jobs=2 |
| Authored warnings | Pipeline completion lost structured warnings and WorldPrepare returned an empty list. Paired rendered/structured diagnostics now survive actual completion; concurrent collection is atomic. | Actual authored-warning positive and imported-warning exclusion |
| Ordinary request targets | Unsupported target fields and stale CLI/input assumptions removed. | Joined host/test compile; ordinary positive and Source-only target refusal |
| Package occurrence refusal | Test expected deleted rendered wording and may choose an invalid counterfactual too early. Diagnostic now retains the actual error. | Observe exact refusal before repairing the oracle; preserve a discriminating target-slot control |
| Typed Haskell fixtures | Stale protocol golden and bare generated-template purposes. Fixtures now use the production recipe issuer. | Seven affected leaves, with exact negative tag refusal |
| Authored SourceBoot resources | Raw child invocation lacked its owned compiler service. Authored producer now uses existing counted isolated libtest runner and declared resources. | Owned bootstrap, count, cleanup and SOURCE reuse execution |
| Scaffold/preview fixture roles | Tests conflated available groups with selection, and SOURCE refusal with orphan-instance isolation. Both contracts now have separate controls. | Two SourceBoot leaves |
| Checked-value inspection codec | Nonempty value rows were paired with empty injected-owner inventory. Derive owners from the same rows; retain ordered/duplicate baseline rows. | Empty/one/two-owner positive, exact duplicate refusal and two neighboring real consumers |
| Cancellation materialization | Fixture prepared another view but expected the old cache owner. It now consumes the actual issued inputs, checks exact identity/bytes and final cleanup. | Interrupted/fresh recovery test |
| Selected-custody lifetime | A live selection legitimately retained its payload. Test now proves retention, then exact last-owner release without graph traversal. | Lifetime/work-count test |
| Empty source fixture | Empty Target incorrectly claimed native readiness although only Fresh emitted a native product. Target stays interface-only with source evidence; false Ready claim must refuse. | Both exact-source component histories |
| Repeated RetainedCore | Sharing compared current SOURCE admission with immutable source identity. Repair compares canonical identity while retaining absent admission. | Repeated-promotion test and adjacent refusal controls; independently source-reviewed and integrated; execution pending |
| Native history oracle | Model dropped exact peer carriers once one same-ID carrier existed. Full nominal dependency custody requires all prior exact providers, without adding executable roots. | Deterministic six-binding history, generated histories and mutation controls; independently source-reviewed and integrated; execution pending |
| Test registration | Two renamed index selectors were stale. Source generator and generated target now match actual discovery. | Counted native cohort; 86 lightweight generator checks passed |

Actual checked-value authority, initial ownership controls, capture/materialization,
promotion, 11 FatIface cases and many SOURCE/selection controls passed in the
recorded sweep. Their exact source and scope remain in the checkpoint. Source
acceptance of a repair does not replace its execution, and additional controls
added by repairs increase the next queue beyond the original 120 leaves.

## Next actions and ownership

1. Validator finishes the immutable `35e077d2c5` compile and package diagnostic,
   then releases it. Root reviews the observed package refusal with its owner.
2. Integrate the remaining reviewed SOURCE concurrency and promotion/oracle
   controls; compile every affected owner and cfg consumer at one next pin.
3. Execute affected failures and their meaningful neighbor/refusal controls;
   repeat scoped adversarial review of the consolidated changes. Preserve
   independent raw-data oracles without claiming they issue compiler authority.
4. Close the rebuilt region only when its required checks pass and no in-scope
   finding remains. Then run maintained G2 histories and record actual progress
   and first failure. No frozen bundle or Persona launch is claimed.

Root owns architectural judgment and integration. `interface_backing_impl` owns
SOURCE borrowing and actual-row tests; `astra_selection_spine_design` owns
RetainedCore identity and independent custody-model repair; `validation_resume`
owns warm compilation/execution. Completed fixture owners remain available for
failures and review. Auditors challenge authority, obsolete routes and sensitivity.

Warm checkout:
`/srv/swarm/checkouts/tidepool-latent-original-stage-join-20261020`.
Do not change its source while its validator has active commands. Preserve
100 GiB scratch plus prospective growth; the last observation left only about
4.5 GiB above that floor. Use warmed, bounded admitted commands, not cold workers.

## Separate G2 frontier

The earlier World history passed capture/declaration/publication, then
`segmentRecord historyCaptured` failed with `UnavailableOriginalHomeDependencies`
for `Val.G1.historyValue0`. Exact retained Original references were absent at
final demand; the responsible transformation was not established.
`NATIVE-REUSE-FRONTIER.md` preserves the evidence and competing explanations.
This investigation resumes after regional closeout with a current reproduction.
No SOURCE grants, name matching or eager broad acquisition may hide the failure.

## Deferred adjacent findings

- `CertifiedRecoveryProduct.source_sha256()` means current admission, unlike the
  canonical interface accessor. An explicit admitted-source name with old-name
  deletion is a bounded followup; approximately 40 callers need classification.
- Joined-interface materialization keys `.hi`/`.packages` by the interface digest
  alone despite committing both. Existing mismatch refusal prevents overwrite;
  a real same-owner witness and paired key should precede a repair. No live
  collision has been reproduced.

Historical checkpoints remain in Git and private evidence, not the active plan.
