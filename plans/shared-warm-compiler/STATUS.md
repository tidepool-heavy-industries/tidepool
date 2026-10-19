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
| host_inputs | 2 | Source frontier settled at b449632676 (code ff2c4577), cumulative independent review accepted, native0/0. Campaign reactivated supervisor to prepare the smallest issuer production/test gate. Warm terminal plus real admission triggers that gate; broad02/03 remains held. |
| owned_inputs | 2 | Rows implements dedicated value-row issuance at the original generic materializer. Supervisor/source_lifetime reserved awaiting exact repair; producer must reactivate both. Rejected26e association and6d42 complete-parent/backing retention; no accepted product fold/native checks. |
| warm_workers | 1 | Exact07c8 production and all five Haskell consumers compiled; Rust4/4 passed, Haskell execution0. Supervisor/protocol repair nested decoder/raw-owner commit atomicity without full snapshot forks. They own exact repair/review, then serial admitted recompilation and maintained counted wrappers. |
| reload | 2 | Directory/schema source8dca6d6a and report a0d293d2, std-only generator/snapshot checks passed (execution0). generation_admission implements coherentR1; authority_review reserved for exact rereview. Initial-publication uncertainty stays unavailable and existing explicit reopen recovery obtains actual head; no new live reconciliation API. BroadR2/R3 held at native gate. |
| qualification | 1 | Current reconciliation settled; all five Haskell hashes and four Rust passes independently verified at07c8. Supervisor/capture grant reserved for exact new checkpoint or campaign frontier-settled final scratch analysis. |

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

The coordinator owns the user's newly authorized broad storage cleanup. Its first
verified idle self-service retirement, tidepool-inline-after-tool-20261014,
preserved source/Git/evidence and raised actual free100.54→105.80GiB. Further
cleanup is not presumed complete. Recheck actual df/memory before every new lane.
Warm owns the next single native lane after atomicity repair/review: prospective
3GiB additional, only while real free covers100GiB+that allowance plus other live
growth. Its maintained wrappers include declared Rust/runtime fixture producers;
no direct test-bin bypass. Campaign observes warm terminal before the host issuer
lane. Broader host/toolchain/reload gates remain pending actual capacity/admission.

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
contains313f62bc0e6f5f8848016d975c0a3610335b88c0 (tested code6d3b ancestor);
this snapshot supersedes that documentation state. Next actionable event owners:

- warm_workers owns staged decoder/raw retention atomic commit, exact independent
  review and admitted native compilation/counting. Terminal candidate/results
  reactivate campaign; campaign then admits host issuer gate if capacity permits.
- host_inputs owns pending issuer gate preparation at b449/ff2c. Broad02/03 and
  source-adoption implementation stay held until production/tests compile and
  focused controls execute. Source-adoption bridge design only is retained.
- owned rows owns repaired original value-row factory and full retention controls;
  reactivate source_lifetime and owned_inputs for exact review/repair/fold.
- reload owns coherentR1 and exact independent review, then actual actor/runtime
  native gate. Unknown bootstrap publication uses unavailable truth and explicit
  existing run reopen recovery; no same-actor recovery is claimed.
- coordinator owns further settled storage receipts; campaign must recheck real
  headroom before admitting succeeding lanes. Qualification owns final analysis
  only after every approved campaign frontier has settled.

Campaign continues review/repairs/folds on those events. Event-bound yield retains
ownership/grants and does not mark this incomplete campaign complete.
