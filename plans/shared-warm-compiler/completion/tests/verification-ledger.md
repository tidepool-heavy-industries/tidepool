# Joined compiler/runtime verification gates

Source-disposition checkpoint: `5d501a98b9f2f0fc2cd9795e6b3c1e4d5df1558c`,
on `integration/compiler-query-warm-final`. The executed compiler/custody cohort
below remains exact `b856a75d5c`; later source changes do not inherit its results.
Integrated source is not executed acceptance. This ledger separates compiler/custody checks,
predecessor results and held ordinary-workflow gates. The
[G2 contract](../g2-completion.md) owns acceptance; the
[compiler map](../../../../docs/compiler-work-map.md) owns current dataflow.

## Exact b856 compiler/custody qualification

`engine_completion` owns this bounded qualification at exact `b856a75d5c` in
`/srv/swarm/checkouts/tidepool-demand-products-20261020`. Its
[run plan](/srv/swarm/evidence/inanna/canonical-completion-products-20261020/package-custody-b856-run-plan.json)
retains complete command arrays, exact selectors, expected counts and output
paths. Expected counts below are selection obligations, not executed counts.
The [terminal observation](/srv/swarm/evidence/inanna/canonical-completion-products-20261020/package-custody-b856-observation.json)
records 19 executed/passed tests: finite 5, components 12, snapshot 1 and Haskell
sidecar 1, all exit 0. It retains per-case report and launch-input hashes. The
Haskell binary hash was measured after execution on unchanged source/output;
its Tasty result is not owned-service cleanup evidence. No World, installer or
actual catalog acceptance follows from this cohort.

| Gate | Exact command or plan entry | Recorded disposition |
|---|---|---|
| Production and cfg(test) consumers | `swarm-build just check tidepool-toolchain tidepool-runtime exomonad-actor` | Passed: exit 0, 837 local actions, Build `d85a2fec-150c-4104-a586-be3c06106364`. [Log](/srv/swarm/evidence/inanna/canonical-completion-products-20261020/package-custody-b856-check-01.log). Compilation executed no tests. |
| Finite output/custody, expected 5 | Run-plan `commands.finite`: `swarm-build just test-native //tidepool/toolchain:toolchain_finite_output_tests` with the recorded fresh output root | Passed: selected 5, executed 5, passed 5, exit 0, Build `791de4d4-7f61-4ef8-a3e4-b08ee8f7c041`. [Log](/srv/swarm/evidence/inanna/canonical-completion-products-20261020/package-custody-b856-finite-01.log); [counted reports](/srv/swarm/evidence/inanna/canonical-completion-products-20261020/package-custody-b856-finite-01/). All five record confirmed process/compiler cleanup and complete diagnostics. Actual package-only closure/owner assertions ran. No World/catalog/installer acceptance follows. |
| HOME/interop/completion/publication, expected 12 | Run-plan `commands.components`: exact library selectors, expected count 12, serial jobs | Passed: executed/passed 12, exit 0, Build `c086ea56-3257-4198-bec8-27fe1c1fb906`. HOME 4, interop 1, completion row index 2, publication/refusal 5, including the revised late-HOME refusal. Process cleanup confirmed; compiler cleanup not observed for these data consumers. [Log](/srv/swarm/evidence/inanna/canonical-completion-products-20261020/package-custody-b856-components-01.log). |
| Runtime snapshot refusal, expected 1 | Run-plan `commands.snapshot`: exact `snapshot_readers_refuse_previous_package_custody_format` | Passed: executed/passed 1, exit 0, Build `efdab6bf-072d-4eca-9536-95041d9d3656`. Process cleanup confirmed; compiler cleanup not observed. [Log](/srv/swarm/evidence/inanna/canonical-completion-products-20261020/package-custody-b856-snapshot-01.log). |
| Haskell paired output, expected 1 | Run-plan `commands.hsidecar`: `//bridge/haskell:source_boot_product_reuse_test`, pattern `finite native output separates package products with captured interfaces` | Passed: executed/passed 1, exit 0, Build `ba88cb9c-5174-43a7-9005-4dc26c8d519b`. Tasty case wall 2.32 s; retained binary/hash in the terminal observation. No owned-service cleanup claim. [Log](/srv/swarm/evidence/inanna/canonical-completion-products-20261020/package-custody-b856-hsidecar-01.log). |
| Actual native catalog | `native_build_service` owns resource admission and the production command | No terminal catalog-production receipt. Resource/reporting changes after `b856` require their own execution; the 19 passing cases do not qualify them. |

Do not provision a cold lane or duplicate these runs from documentation work.
Update a row only from its terminal report: source OID, actual executable/resource
hashes, selected/executed counts, exit status, logs and separate cleanup evidence.
The plan's output names must not be reused; a new run needs a fresh root.

## Later resource and request-owner checks

These are exact-source results, not reruns of the full b856 cohort or G2.
`engine_completion` retains commands, executable/resource hashes and cleanup
fields in the linked observations.

| Source / consumer | Executed result and limit |
|---|---|
| `abf4a739ff`: toolchain/runtime compilation; direct Portable; accepted join custody | Compilation passed, 34 actions. Portable 1/1 passed with process cleanup confirmed, compiler cleanup not observed. Owned accepted-join custody 1/1 passed with process/compiler cleanup confirmed. Source pairing failed 1/1 with `IoNotFound` after inspection; retained-entry selection executed zero because of an incorrect ignored selector. Neither was a pass. Logs are under [resource evidence](/srv/swarm/evidence/inanna/canonical-completion-products-20261020/). |
| `6c61c6dcb1426877a98ea9d20cf707e010fd02a1`: actual request materialization | [Request-owner observation](/srv/swarm/evidence/inanna/canonical-completion-products-20261020/exact-request-owner-observation.json): `swarm-build just check tidepool-toolchain` passed, 11 actions; `//tidepool/toolchain:toolchain_source_proof_pairing_test` executed/passed 1, exit 0, process/compiler cleanup confirmed. Actual request rows replace persistent-context relookup; existing consumed-source receipts passed unchanged. No World/catalog qualification. |
| `3c86ac3682`: corrected retained-entry selector | [Corrected observation](/srv/swarm/evidence/inanna/canonical-completion-products-20261020/package-resources-retained-fixed-observation.json): `//tidepool/toolchain:toolchain_retained_entry_refusal_test` executed/passed 1, exit 0, process/compiler cleanup confirmed. The earlier zero-execution selection remains invalid evidence. No World/catalog qualification. |

## Predecessor evidence, not joined acceptance

The [c75 observation manifest](/srv/swarm/evidence/inanna/canonical-completion-products-20261020/package-interface-custody-c75-joined-observation.json)
belongs to exact `c75b761909b35948a009c304e424cf357a752960`, despite “joined” in
the filename. It records standalone predecessor compilation and execution:

| Observation | Scope and limit |
|---|---|
| `swarm-build just check tidepool-toolchain`, exit 0, 1024 actions | Production/cfg(test) compilation at `c75`; no joined runtime/actor acceptance. Build `84ae6aed-6f1a-4092-82c7-63df7dd6e0ee`. |
| HOME 3 plus Rust/Python selection interop 1: selected 4, executed 4, passed 4, exit 0 | The manifest retains each counted report and executable/resource evidence. The later `b856` cohort above additionally executes the revised late-HOME refusal and indexed completion histories. |
| Python catalog controls: executed/passed 34, exit 0 | Qualification-script checks, not actual native catalog production. |
| Optional publication control | Proves the production-used helper's outcome policy. It does not prove an actual completed compile survives injected candidate-publication failure; that consumer fault-injection boundary remains unqualified. |

Earlier actual private-input receipts remain at
[`initial-rejection-644c`](/srv/swarm/evidence/inanna/world-json-input-acceptance-20261020/initial-rejection-644c/):
two separately counted cases executed/passed, exit 0, one case per receipt,
compiler cleanup confirmed and diagnostics incomplete. They cover used/unused
JSON input, initial wrong-payload rejection without publication, retirement,
crossed/retired owners and unchanged public state. They do not execute the
current bundle. Internal JSON variants are not additional counted tests.

Cold checking and producer removal are separate observations. The earlier
canonical cold test's producer-removal argument used a synthetic source token's
parent rather than the actual scoped producer root; that portion is invalid.
`package_envelope_identity` owns the actual-root observation correction. Preserve
the valid cold result separately; no repaired removal result is recorded here.

## Held discovery and notebook gates

`closed_recipe_pairing` owns the ordinary joined history; `audit_reload_ownership`
owns public reload. These named targets declare the compiler deployment and
owned-resident resources. Each command below uses
`swarm-build just test-native LABEL --output-dir FRESH_ABSOLUTE_DIR --retain-artifacts`.
Use a new private evidence directory and the target's existing selection; do not
append another `--exact` or substitute an uncounted generic wrapper.

| Gate / label | Required observation | Current limit |
|---|---|---|
| Discovery, expected 1: `//exomonad/host:facade_introspection_service_test` | All five structured effects, exact semantic fields, read-only state, real `readFile`, actual publication then stale-pin refusal | Held; no hosted all-five pass on `b856`. Earlier source/fixture compilation is not execution. |
| Answer settlement, expected 1: `//exomonad/actor:actor_world_status_test` | A real answer completes, publication intervenes, final validator refuses ScopeChanged | Held. Prior `51216eb9d6` failed during literal-query parsing before publication; no current settlement result follows. |
| Notebook contract, expected 4: `//exomonad/actor:actor_notebook_contract_tests` | Parked A allows B control; malformed/unavailable reload releases invocation and preserves independent cells/bindings | Held. Prior `51216eb9d6` failed during initial source ownership; that old failure is not a current-source result. |
| Public-module queries, expected 2: `//exomonad/actor:actor_compiler_world_queries_test` | Exact answer/pin, wrong-name refusal, source invalidation, hot counters, closed-source refusal | Canonical source `9c18cceebc` adds actual rowFit/typeOfExpression/typeSearch effects. Execution remains held; the `b856` cohort predates this change and does not execute these actor cases. |
| Finite source → World, expected 1: `//tidepool/runtime:runtime_package_source_history_test` | Remove the observed actual producer output, preserve completed products, execute public B through A cutoff and retained callback | Finite-removal/retained-callable source is canonical as `5097c09cd7`; historical parent publication/recovery controls are canonical through `fb34ffe421`. The latest executed frontier reaches public B publication but loses selected package-root domains; see the runtime frontier below. Request capture/mount remains a separate companion case. |
| Default reload | Real staged B despite draft C; success/refusal; captured-A refusal; caller loss after typed publication leaves B callable | Held for integrated replay. Earlier startup failures before Reload and separate SourceSlot controls cannot establish this workflow. |
| Uncertain END, expected 1: `//tidepool/runtime:runtime_compiler_end_history_test` | Genuine prepare before physically withheld END acknowledgement; typed Unconfirmed, no execution/publication, exact daemon resumed | Passed at exact `e574be71b531a5be21d54838f7d03247a9aca86d`: selected/executed/passed 1, exit 0, Build `e2b8a836-7c4b-48d4-b88e-63f0084b56b9`. [Terminal receipt](/srv/swarm/evidence/inanna/g2-history-tests-20261020/end-e574be71b5-terminal.json) retains binary/compiler/deployment hashes and counted report. Actual successful compiler response, 2 physical requests, 0 replays, publication refused and public state unchanged. Process/compiler cleanup confirmed; diagnostics incomplete because directory-resource hashes are UNKNOWN. This qualifies the END behavior/physical-cleanup gate at its source, not the ordinary route or G2. |
| Eviction | Actual cold owner eviction and recovery through the ordinary history | Held for measured resource/budget selection and source extension. Retained-cache configuration alone is not evidence of actual eviction. |

## Current compiler milestone and open runtime frontier

The [compiler checkpoint](/srv/swarm/evidence/inanna/latent-original-stage-join-20261020/plan-b-current.json)
records the following at exact `5d501a98b9f2f0fc2cd9795e6b3c1e4d5df1558c`.
Its command manifests retain the exact selectors and logs. These results qualify
compiler preparation/sealing and the named laws, not runtime or release.

| Gate | Terminal result and scope |
|---|---|
| Production/cfg consumers | Passed, exit 0, Build `990a17fb-55c8-4b38-b7bf-96b69bd19f4e`; compilation executes no tests. |
| Actual mixed World preparation | Executed/passed 1, exit 0, Build `94305e7c-4c05-489b-92dc-08beee161fb6`. [Command](/srv/swarm/evidence/inanna/latent-original-stage-join-20261020/actual-producer-helper-mixed-36-command.json). Replaces earlier mixed-stage failures for this compiler history, not runtime installation. |
| Carried TYPE/Name law | Executed/passed 1, exit 0, Build `8fd289ec-5654-4a8c-9245-e275189ed0fa`. [Command](/srv/swarm/evidence/inanna/latent-original-stage-join-20261020/matched-carried-type-law-36-command.json). |
| Prefix recipe/history | Executed/passed 1, Build `330ed355-ea59-4a09-986c-8abcf51573b6`. [Command](/srv/swarm/evidence/inanna/latent-original-stage-join-20261020/matched-prefix-history-law-36-command.json). |
| Support/replay refusal | Executed/passed 2, Build `aa3f2cde-4095-4ba1-93ed-29e5d8e33b7a`. [Command](/srv/swarm/evidence/inanna/latent-original-stage-join-20261020/final-support-negatives-36-command.json). |
| Publication/exact occurrence, predecessor `d870d7ef93` | Executed/passed 3, Build `c2f5f50b-62d8-4a9b-9b2e-d027e7e2a311`; same production source according to the checkpoint. [Command](/srv/swarm/evidence/inanna/latent-original-stage-join-20261020/source-owner-fixtures-controls-35-command.json). Keep this exact test-source distinction. |
| Selected-HOME edges | Executed/passed 1 at `5d501a98b9`, within Build `f5e57add-513d-413c-a220-97a4dbc84b9b`. The other selected leaf failed on a fixture premise and is superseded by the exact package sentinel below. |
| Exact original-package sentinel | Executed/passed 1 at test-only correction `961db189f4`, Build `5dc6aceb-c7bb-42ef-8ab7-74cf362bda37`. [Command](/srv/swarm/evidence/inanna/latent-original-stage-join-20261020/exact-package-sentinel-37-command.json). Actual native package witness baseline, missing/wrong defining interface refusals and restored selection all pass. Production code is unchanged from `5d501a98b9`. |
| Completed current-stage shortcut | Executed/passed 1 at `6fee9095d0`, Build `0172db06-9f31-405f-aefa-90a8c2c7e5aa`; production/cfg compilation passed Build `cccded87-c3a4-4233-ba2f-6a1a4668ebe2`. [Command](/srv/swarm/evidence/inanna/latent-original-stage-join-20261020/completed-shortcut-mixed-38-command.json). Actual stage 1 observes refusal 1, producer publication 1 and retry refusal 1. The same completed receipt remains available without authorizing current-stage execution. |
| `fixtures-check` | All 12 declared cohorts passed at `5d501a98b9`, Build `8a6662c2-f604-4c33-9a96-2f625c7e69fb`, exit 0; no failure, timeout or skip. [Command](/srv/swarm/evidence/inanna/latent-original-stage-join-20261020/fixtures-check-36-command.json). The suite cohort verified 1004 programs against the independent GHC oracle. |

The compiler checkpoint does not retain complete cleanup/artifact fields for
every law; no stronger cleanup or frozen-resource claim is inferred from its
passing counts.

At exact `f0819562a7b7a63f3b68840f8cc7d0570ffb344d`, the
[source-policy checkpoint](/srv/swarm/evidence/inanna/g2-common-native-installer-20261020/ordinary-source-policy-checkpoint.json)
records two separately counted passes (1/1 each, exit 0): Ordinary startup,
effect suspension/callback capture/resume to 42; and captured-A cutoff refusal
with unchanged images/residency plus independent fresh-B execution. Both report
process/compiler cleanup confirmed and diagnostics incomplete. These are scoped
controls, not full launch or the finite → public World journey.

The runtime finite frontier remains an executed failure. The exact reviewed
World-selection transfer `9e998a113e31f5c94b26d3142fad8ed002c231f0` preserves
the admitted domain through private execution and publication. Native-instance
program custody `c36174148eed0bb6519339f72cf5829264287eb6` then preserves the
defining program after a pure CAF updates to a foreign constructor. Its real
CAF/release control and six neighboring GC/rollback/cleanup controls pass;
ordinary startup and captured-A cutoff controls separately pass. The finite
history now executes/displays 42, publishes and reads the retained public CAF.

The next cell exposed a legitimate visible checked TYPE row absent from the
World primary declaration/source namespace. Repair `c6b134bff8` projects only
actual stage-owned checked TYPE interfaces into a temporary compiler basis;
it leaves the World primary and native decisions unchanged. Toolchain/runtime
production and test compilation pass (Build `f22cce60`), two TYPE/conflict
controls pass (Build `de21c23a`) and the real mixed World case passes (Build
`32743815`). The actual finite rerun at `4b148ba0bc` clears the missing-role
refusal, but fails at a separate acquired-view selection guard. Its
[counted report](/srv/swarm/evidence/inanna/g2-history-tests-20261020/runs/4b148ba0bc/finite/da655f02a032183e95c87e77053c3ff53d731d57b3eec8b0b62a70bf13475a09.json)
records executed 1, failed 1 and confirmed process/compiler cleanup. Exact
missing-binding diagnostics are pending; no selection guard is relaxed.

The rooted cold recovery target initially refused before compilation because its
legacy fixture lacked the actual source transaction and published World owner.
Its migration to the existing captured World fixture has compiled, but actual
cold recovery remains unqualified. Earlier intermediate observations do not
qualify either complete history, catalog production or a frozen bundle.

## Remaining route disposition

The reviewed common source uses one `NativeInstantiationPlan` installation core
for captured/open code and the closed adapter. Machine-global `code_exports`
and the older certified-turn installer are removed. World segment certification
consumes the checked stage owner and its exact initial selection; the empty
prefix `ReceiptNativeInputDemand` supplies SOURCE/TYPE projection only. These
source corrections supersede the old deletion/reconstruction tasks; they do not
qualify the held ordinary histories.

Physical current-product issuance, ordered exact target selection, typed-entry
association and prior-stage support are integrated and exercised by the compiler
checkpoint above. Nominal metadata still grants no executable permission.
The common installer refuses unsealed Ordinary code before preparation; sealed
Ordinary code restricts its plan and carries the installing transaction origin.
Execution requires checked current source facts and retained-capture custody.
Compiler passes and scoped runtime controls do not qualify public runtime
selection transfer, the full finite journey or a frozen bundle.

Finite preparation is intentional for the explicit unprepared developer/fixture
installer and test surfaces. Ordinary actor checked cells require World.
Nonchecked binding/mount completion still reaches finite retraction/declaration
validation. These concrete callers, listed in the map, remain a separate route
disposition obligation; API names alone do not establish another ordinary path.

## G2 boundary

No complete G2 acceptance is recorded here. Ordinary/mixed/source-defined
cells, private inputs, discovery, default reload and caller-loss retention still
need joined evidence. Freeze one matched bundle, verify the selected Persona
spec/tools/initial memory, then run bounded real DeepSeek turns through those
same bytes. Full force/fork/callback/process-recovery matrices belong to G3;
unsupported operations must refuse explicitly. Neither a successful compilation
nor component execution is frozen release or deployment evidence.
