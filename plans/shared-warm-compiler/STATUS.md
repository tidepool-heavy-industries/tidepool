# Shared warm compiler campaign — current snapshot

State: **implementation active; first qualification tranche integrated and tested**.
No measured compiler speedup, semantic Ready/hit, frozen release or deployment yet.

Campaign owner `/root/warm_compiler_campaign` (gpt-6.1-sol high), checkout
`/srv/swarm/checkouts/tidepool-shared-warm-campaign-20261019`, branch
`campaign/shared-warm-compiler`. Parent `/root` coordinates the performance fork;
external pre-fork release/main/live-host owner remains unconfirmed. Campaign
publication is authorized; release integration/deployment needs that owner.

Baseline: deliberately refreshed public release
`545d9950e5c3c3940f5b6bf84dfc5d16d6b1a89d` → imported peer
`70a92c65a02a591f76590daf4e5bfa3e6fcbf723` → reviewed plan
`5b74c2f1fe010c5c343e0c6f331d54328fbd5090`. Peer delta127files/6774+/1520-
is prerequisite implementation, not campaign output. Optional-root repair
`d24b5b20b04b70c61416d5bf68858916b7b54848` contained. Contract C2 amendment
`e2bd2611102566085d0c573568d820a50ed62fb3` is acknowledged by all area owners.

## Integrated checkpoint and exact checks

Campaign integration `6d3b1745b465b6d0fe14104cb4020ead282f1620` contains complete
qualification area `7db82af3f13292ce3677ee6e87272653c7d7bbf5` (reviewed leaf
cec2a3361 roster + ac3bf6e03 capture; area joined code64636e485a).
It fails closed on invalid physical compiler observations through cycle and
workload reporting, registers existing profile tests, and adds the stronger
shared-workspace Persona case to the seven-case frozen startup roster.
Independent Sol-high full-delta review accepted; campaign joined source-only
checks executed **89 reporter/profile +436 bundle script cases, all passed**,
exit0, no skip/omission. These do not execute hosted/native compiler workloads.

Exact campaign command: `swarm-build bash scripts/buck2-run.sh test --local-only
-c remote.enabled=false -c tidepool.profile=fast-dev --print-passing-details
//scripts:compiler_reuse_report_tests //scripts:native_bundle_qualification_tests`.
Evidence `/srv/swarm/evidence/inanna/shared-warm-compiler-20261019/campaign/`:
joined-script-tests.json/log; logSHA
`de33f88a43a7261165f187345fe8409e9555919d712aecfc6fe4d9897d0125be`.
Clean tested source6d3b; configure exit0, elapsed16.57s; script checks22.92s.

Pinned admitted `scripts/integration-status.py --strict` ran at6d3b, exit1 only
`local_main_missing` in this intentionally isolated campaign checkout; Harness
manifest/lock pins agree at1893773e14b02b3015d99ba63114decfd3223949.
No main ref was manufactured or parent branch changed. Report integration-status.json.
Its origin/main observation is cached local provenance, not public freshness.

## Capacity and live continuation ownership

Subtree ceiling14 including campaign: five supervisors and eight reserved leaves.
All use gpt-6.1-sol high except owned_inputs/rows medium; fresh contexts.
Grants persist through event-bound yields, maximum2direct children per area.
Leaf grants remain reserved during dependency waits. Host and qualification have
settled their current source/reconciliation work; owned rows, warm repair and
reload R1 remain active. Completed contexts are not duplicate-task capacity.

| Area/task under campaign | Reserved leaves | Current concrete state / next event owner |
| --- | ---: | --- |
| host_inputs | 2 | Source frontier settled at b449632676 (code ff2c4577), cumulative independent review accepted, native0/0. Supervisor owns active -j1 issuer production/test gate plus eight exact counted controls; broad02/03 remains held until results. |
| owned_inputs | 2 | Original value-row factory successor49b65c8f82 independently source-accepted; prior efd compile exit3 caught three stale cfg(test) callers. Same target set retry active under actual12GiB memory allowance (prior scope peak7.18GB), rows owns terminal. No accepted product fold or passing native gate yet. |
| warm_workers | 1 | Exact07c8 production and all five Haskell consumers compiled; Rust4/4 passed, Haskell execution0. Staged atomic repair38a749fb02 independently reviewed; exact production/all five Haskell consumers and Rust cfg(test) compile0. Supervisor owns active serialized declared-wrapper graph and counted execution; Haskell execution still0. |
| reload | 2 | Directory/schema source8dca6d6a and report a0d293d2, std-only generator/snapshot checks passed (execution0). Coherent leaf fe3ef791 remains unaccepted: reviewer caught uncertain compiler-close claim loss and test lease-custody defect, repair active. Area02e888309c awaits exact repair/review; producers own supervisor reactivation. Initial-publication uncertainty stays unavailable and existing explicit reopen recovery obtains actual head; no new live reconciliation API. BroadR2/R3 held at native gate. |
| qualification | 1 | All five Haskell hashes and four Rust passes independently verified at07c8. Supervisor reactivated approved current-baseline frontier after storage recovery: exact reusable frozen bundle/representative history or smallest build/resource gate, no historical-speedup substitute. Final scratch analysis waits frontier settlement. |

Every leaf has an isolated real checkout/index and per-area HANDOFF. Campaign alone
edits shared STATUS/CONTRACTS. Only owning parent integrates its branch. Full
base-to-candidate source review and meaningful compile/count evidence precede
acceptance; source-only reviewed candidates remain explicitly unqualified.

## Native evidence and current storage admission

Warm exact `07c8efbe507a0624317e458f0e687e9d55410934`:
production Rust and cfg(test) compiled, selected4/executed4/passed4, exit0.
All five Haskell compile-only targets passed: tidepool_extract_bin,
worker_response_child, worker_response_test_bin, cell_splitter_test_bin and
source_boot_product_reuse_test_bin. **Haskell execution0**. Transport source-free
route refuses Unsupported; no realized exact-root Ready/hit or speedup claim.
Receipt `warm-workers/NATIVE-VALIDATION-07c8.json` under the campaign evidence root;
independent reconciliation `/srv/swarm/evidence/inanna/shared-warm-qualification-20261019/NATIVE-RECONCILIATION-07c8.json`.

Haskell lane actual incremental output268353536B within640MiB prospective budget;
final warm buck-out931864576B; observed enclosing MemoryPeak3156279296B.
Warm daemon3810824/fork3810865 remains in admitted build.slice. Live GHC PID/cgroup
sample was not captured; no stronger process-accounting claim is made.
The first production attempt caught a local guard syntax error; subsequent compile
caught no errors, and independent review repaired a standalone-decoder policy
regression. Current staged atomicity repair remains uncompiled/unexecuted.

Reload source8dca6d6a compiled its std-only protocol generator (4local actions)
and owning generated-snapshot check (3local actions), exit0, execution0.
Artifact6598656B SHA143f28f99a457a86aad2bbb52269a8f432bd95670bbe1d84b07e7f6911a047f4;
own output56938496B, observed MemoryPeak836239360B. Receipt
`reload/protocol-generator-receipt.json`; idle daemon3835600 retained in its
admitted build.slice. Actor/runtime/R1 production and tests remain uncompiled.

Installed swarm-build is the older scope launcher; --storage-status is unsupported.
No replacement admission wrapper or staged reservation interface was invented.
Preserve100GiB unreserved including expected output/scratch growth. All campaign
commands have settled; live Persona hosts/shared workers remain untouched.

The coordinator owns the user's newly authorized broad storage cleanup. Verified
idle self-service retirements preserve source/Git/evidence; actual free grew from
~100GiB to266944032768B at latest campaign transition. Cleanup continues; no final
batch completion presumed. Actual build.slice20.35GB/host available96.95GB before
lane transition supports two independent bounded native lanes, with room to80GiB
pressure threshold. Warm owns3GiB additional output/16GiB conservative memory;
host owns32GiB prospective output/24GiB memory, both -j1 and separate outputs.
Owned first check settled (exit3); repaired49b65 same-target retry was admitted
on fresh build.slice19.81GB/free277802762240B with12GiB own memory allowance
(prior measured7.18GB peak). Host24+warm16+owned12+current stays below80GiB.
The owning check recipe lacks a jobs switch; actual retry used the same recipe
before cross-message direct -j1 guidance arrived. No duplicate/cancel was issued;
future retry uses exact same check-plan labels via the existing pinned -j1 runner.
Latest available330640728064B; cleanup still owner-coordinated, not presumed done.
Reload is source repair/review pending next lane; qualification prepares current
baseline evidence and a concrete resource gate without launching a fourth lane.
Warm maintained wrappers include declared Rust/runtime fixture producers; no
direct test-bin bypass. Every owner observes terminal source/count/resource evidence.
Recheck actual disk/memory at transitions; broad migration still waits native gates.

## Shared decisions, remaining frontier and acceptance limits

Home census uses authenticated selected canonical/value/lexical row witnesses,
separate from home_units type classification, dormant custody and executable demand.
Actual complete baseline context owner is reused; no wrapper/registry minted only
for identity. Changed transitions require exact old/new owner evidence; no stale
ordered-prefix source support or hash/equal-payload substitute.

Physical rows promote value-sized custody before existing source-support narrowing.
Rejected full-frontier refresh would add O(history) work at publication. Current
package path/hash and deployment schema1 do not own already-decoded EPS/lazy bytes:
no group-validation skip or pinned-package claim. Source-backed/clean-universe
options and actual global strong-owner release remain under bounded review.

Reload uses lower RuntimeLexicalScopeLease source admission distinct from custody
liveness. Actual new compile/install/registration gates preserve Continuing and
independent LLM worlds. Force cannot kill independent owned children as collateral;
real cleanup closure/fence or typed refusal required. Unknown lifecycle cleanup
must retain identity and uncertainty. Candidate preparation stops nobody.

All remaining approved issuer, physical-row, reload, review/repair/fold and joined
checks stay in frontier. Hard strategy questions close only their cone after
bounded independent review; drain all other approved successors and running checks
before MORNING handoff. No native goal/deployment created here.

Event-driven continuation: no periodic polling/status wakes. Max useful work per
activation; if locally blocked, record awaited event/producer-or-observer/recipient/
next action then yield. EVERY terminal outcome sends exact source/evidence/status/
next action; running recipient→send_message, dormant non-root→followup_task.
Auto-completion supports delivery, never replaces resumption. Command owners must
observe terminal outcomes; no orphan process. Reserved grants/no duplicates.

After all campaign subagent frontier settles, qualification MUST analyze scratch
space bounded top-down by checkout/output owner, active versus idle use, independent
required products/evidence, potential reclaimable outputs and exact safe boundaries.
Analysis only: no deletion, unmount, GC or stopping others. Admission checks continue
throughout work. Final bundle/performance qualification and deployment remain separate.

## Campaign continuation handoff

Campaign has no running commands. Public github campaign/shared-warm-compiler
contains718e7864ad (tested code6d3b ancestor);
this snapshot supersedes that documentation state. Next actionable event owners:

- warm_workers owns staged decoder/raw retention atomic commit, exact independent
  review and active declared-wrapper native compilation/counting. Terminal
  candidate/results reactivate campaign and release/adjust lane accounting.
- host_inputs owns active issuer production/test/count gate at b449/ff2c. Broad02/03 and
  source-adoption implementation stay held until production/tests compile and
  focused controls execute. Source-adoption bridge design only is retained.
- owned rows owns repaired original value-row factory, source association and
  stale cfg(test) caller repairs plus full retention controls. Exact49b65 source
  review accepted; rows observes active same-target retry and reports terminal
  to campaign/area, then area reviews/folds passing exact evidence.
- reload owns coherentR1 and exact independent review, then actual actor/runtime
  native gate. Unknown bootstrap publication uses unavailable truth and explicit
  existing run reopen recovery; no same-actor recovery is claimed.
- qualification owns current matched baseline bundle/history gate preparation and
  eventual exact semantic/performance execution, with heavy build admission separate.
- coordinator owns further settled storage receipts; campaign must recheck real
  headroom before admitting succeeding lanes. Qualification owns final analysis
  only after every approved campaign frontier has settled.

Campaign continues review/repairs/folds on those events. Event-bound yield retains
ownership/grants and does not mark this incomplete campaign complete.
