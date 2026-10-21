# Joined compiler/runtime verification gates

Source checkpoint: `b856a75d5c7ba344632e1935d3c4c4d5192ea5b8`, on
`integration/compiler-query-warm-final`. Integrated source is not executed
acceptance. This ledger separates the current joined compiler/custody checks,
predecessor results and held ordinary-workflow gates. The
[G2 contract](../g2-completion.md) owns acceptance; the
[compiler map](../../../../docs/compiler-work-map.md) owns current dataflow.

## Current joined compiler/custody qualification

`engine_completion` owns this bounded qualification at exact `b856a75d5c` in
`/srv/swarm/checkouts/tidepool-demand-products-20261020`. Its
[run plan](/srv/swarm/evidence/inanna/canonical-completion-products-20261020/package-custody-b856-run-plan.json)
retains complete command arrays, exact selectors, expected counts and output
paths. Expected counts below are selection obligations, not executed counts.

| Gate | Exact command or plan entry | Recorded disposition |
|---|---|---|
| Production and cfg(test) consumers | `swarm-build just check tidepool-toolchain tidepool-runtime exomonad-actor` | Passed: exit 0, 837 local actions, Build `d85a2fec-150c-4104-a586-be3c06106364`. [Log](/srv/swarm/evidence/inanna/canonical-completion-products-20261020/package-custody-b856-check-01.log). Compilation executed no tests. |
| Finite output/custody, expected 5 | Run-plan `commands.finite`: `swarm-build just test-native //tidepool/toolchain:toolchain_finite_output_tests` with the recorded fresh output root | Passed: selected 5, executed 5, passed 5, exit 0, Build `791de4d4-7f61-4ef8-a3e4-b08ee8f7c041`. [Log](/srv/swarm/evidence/inanna/canonical-completion-products-20261020/package-custody-b856-finite-01.log); [counted reports](/srv/swarm/evidence/inanna/canonical-completion-products-20261020/package-custody-b856-finite-01/). All five record confirmed process/compiler cleanup and complete diagnostics. Actual package-only closure/owner assertions ran. No World/catalog/installer acceptance follows. |
| HOME/interop/completion/publication, expected 12 | Run-plan `commands.components`: exact library selectors, expected count 12, serial jobs | Preparing declared build resources; no executed result yet. Covers HOME 4, interop 1, completion row index 2, publication/refusal 5. The late inconsistent-HOME case now publishes a valid durable fixture before tampering; its new revision needs execution. |
| Runtime snapshot refusal, expected 1 | Run-plan `commands.snapshot`: exact `snapshot_readers_refuse_previous_package_custody_format` | Held; no snapshot execution claim at `b856`. |
| Haskell paired output, expected 1 | Run-plan `commands.hsidecar`: `//bridge/haskell:source_boot_product_reuse_test`, pattern `finite native output separates package products with captured interfaces` | Held. This compiler worker control does not replace actual catalog production or hosted history. |
| Actual native catalog | `native_build_service` owns resource admission and the production command | Held for declared-resource repairs; no successful catalog-production claim. |

Do not provision a cold lane or duplicate these runs from documentation work.
Update a row only from its terminal report: source OID, actual executable/resource
hashes, selected/executed counts, exit status, logs and separate cleanup evidence.
The plan's output names must not be reused; a new run needs a fresh root.

## Predecessor evidence, not joined acceptance

The [c75 observation manifest](/srv/swarm/evidence/inanna/canonical-completion-products-20261020/package-interface-custody-c75-joined-observation.json)
belongs to exact `c75b761909b35948a009c304e424cf357a752960`, despite “joined” in
the filename. It records standalone predecessor compilation and execution:

| Observation | Scope and limit |
|---|---|
| `swarm-build just check tidepool-toolchain`, exit 0, 1024 actions | Production/cfg(test) compilation at `c75`; no joined runtime/actor acceptance. Build `84ae6aed-6f1a-4092-82c7-63df7dd6e0ee`. |
| HOME 3 plus Rust/Python selection interop 1: selected 4, executed 4, passed 4, exit 0 | The manifest retains each counted report and executable/resource evidence. The revised late-HOME refusal case and the indexed completion histories at `b856` are additional obligations. |
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
| Public-module queries, expected 2: `//exomonad/actor:actor_compiler_world_queries_test` | Exact answer/pin, wrong-name refusal, source invalidation, hot counters, closed-source refusal | Held. History candidate `8da581b5c5` on base `784fd9fc1f` adds actual rowFit/typeOfExpression/typeSearch effects to an existing case; formatting/source checks only, executed 0. Prior partial results do not qualify this candidate. |
| Finite source → World, expected 1: `//tidepool/runtime:runtime_package_source_history_test` | Remove the observed actual producer output, preserve completed products, execute public B through A cutoff and retained callback | Stronger finite-removal/history changes are uncommitted receiving work on base `784fd9fc1f`; no current runtime acceptance. Request capture/mount remains a separate companion case. |
| Default reload | Real staged B despite draft C; success/refusal; captured-A refusal; caller loss after typed publication leaves B callable | Held for integrated replay. Earlier startup failures before Reload and separate SourceSlot controls cannot establish this workflow. |
| Uncertain END and eviction | Genuine prepare before END loss; no execution/publication on Unconfirmed; actual cold owner eviction and recovery | Held. END ordering and owned-daemon eviction controls are not implemented acceptance. A signal dependency or retained-cache configuration alone is not evidence of the history. |

## Remaining route disposition

The current closed installer retains a validated product/image recipe and scoped
instance authority. Captured/open installation still reaches scoped resolution
and the older package `code_exports` route. The common installer and once-only
issuance changes remain feature work; do not describe these consumers as unified.
World native-demand stage batches are retained but segment certification still
reconstructs demand from receipts. Nominal metadata membership grants no native
demand. See the compiler map for the owning callers.

Finite preparation/validation APIs remain. Query migration and removal of unused
raw Joined materialization APIs do not establish compiler-route exclusivity.
Property and component cases complement the real finite → installer → public
World journey; their counts cannot replace it.

## G2 boundary

No complete G2 acceptance is recorded at `b856`. Ordinary/mixed/source-defined
cells, private inputs, discovery, default reload and caller-loss retention still
need joined evidence. Freeze one matched bundle, verify the selected Persona
spec/tools/initial memory, then run bounded real DeepSeek turns through those
same bytes. Full force/fork/callback/process-recovery matrices belong to G3;
unsupported operations must refuse explicitly. Neither a successful compilation
nor component execution is frozen release or deployment evidence.
