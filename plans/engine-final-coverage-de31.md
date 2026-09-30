# Final delivery coverage at de31

Source audit of `de31d0e213e055b50789c7aeb248a719d96a2af4`, against
`engine-harness-final-delivery.md:173–237`, on 2026-09-30. This is a reused-context
source/evidence inventory, not milestone acceptance or a fresh adversarial review.
No builds or tests were run for this audit. Later owner candidates require their
own joined verification. An existing fixture is not evidence that it executed.

## Ordered critical path

1. Preserve selected candidates/WIP and finish mixed-cell ordered planning. The
   current protected original-declaration path accepts one initial declaration
   group; a typed rejection for other orders does not complete mixed cells.
   Bind the opaque parser receipt to the original body/specification, allocate
   Lib/Val identities in parser order, and verify the authoritative final recipe
   against that selection. Planning must not run earlier effects or replay
   original declarations. Owners: `tidepool/toolchain/src/checked_cell.rs`,
   `bridge/haskell/app/Main.hs`, `tidepool/runtime/src/session/admission.rs`,
   `exomonad/actor/src/resident_workbench.rs`.
2. Close current single-admission production/browser/cold-restart gates and the
   complete cancellation/publication matrix. Private execution is implemented;
   acceptance remains open. Compiler/native primitives do not prove the actor
   composition or browser journey.
3. Convert the remaining resident serial ingress and effect paths before keyed
   admission. Then replace the singleton with the admitted-execution map and
   run the full M2 interleaving, cancellation, drain, and shutdown matrix.
4. Execute joined cache/demand/capture/recovery/scaling and required broad
   verification/package gates, then commission the separately required fresh
   Sol 6.1 milestone reviews. Prepare the live-trial packet afterward; live
   execution, pushes, and default changes remain outside this wave.

## Production conversion coverage

`local_actor.rs` has `pending_task: Option<PendingActorTask>` and one monotonic
generation. Workbench and kernel tasks share full-step completion admission,
but this is still one admission. `ActorTaskExecution::Serial` transfers the
original behavior and `OwnedWorkbenchTask::serial` remains a production path.

| Owning source | Implemented boundary | Remaining obligation |
| --- | --- | --- |
| `resident_actor/owned_workbench.rs:827` | Same cursor yields typed unit/native/effect/after-tool/background tasks with original resources, budgets and fences | `continue_owned_task` still calls borrowed `execute_workbench` through Serial; remove resident fallback rather than counting captured waits as complete conversion |
| `resident_actor/owned_workbench.rs:503` | Normal workbench admission begins private execution and retains original publication decision | Reload-spec/reload-helpers explicitly return Sequential and await source publication; convert builtin/reload ingress |
| `resident_actor.rs:1398`, `:6610`; `owned_workbench.rs:1758` | Owned wait variants cover Watch, Drain, Exit, PollExit, Sleep, External, Jev and Commands; command presentation has separate captured prepare/apply | Other `ResidentActorBoundary` variants still reach awaited `resolve_effect`: child/fork/workspace, requests beyond Watch, source/attachment, notifications, tool settlement, and remaining inspection/native-resume branches. Synchronous owner decisions may stay synchronous; their waits must become owned tasks |
| `resident_actor.rs:9142`, `:9413`, `:9434`, `:9458`, `:9488`, `:9925` | Start, ReleaseFork, cast/call, direct tool and resume implementations exist | Direct kernel ingress still awaits behavior/kernel continuations; ReleaseFork uses the default Serial task and awaited initialize. Shared output ABI does not implement durable child activation |
| `resident_actor/owned_workbench.rs`; `resident_tools.rs`; runtime `admission.rs`, `publication.rs`, `resident.rs` | Original private decision, freeze-once intent, staging/restage, exact abort acknowledgements and confirmation-only uncertainty are implemented | Joined real tests must cover both commit/cancel orders, lost/failed acknowledgements, terminal/after-tool cleanup, checkout exclusion and resource last-owner release. Owner fixtures and extractor-free races cover subsets |

Named inspection/background helpers and after-tool tasks have been converted;
this does not imply their direct effect and kernel ingress matrices are complete.

## Requirement to source and retained evidence

| Required gate | Owning source / existing consumer | Retained evidence and remaining acceptance |
| --- | --- | --- |
| Protected hidden/original execution, replacement, qualified names, refusals, E/D publication | Compiler checked-cell/artifact owners; runtime admission/resident/publication | Native original/overlay/display/E tests and actual alternate-body/reserved/root refusals passed at owner revisions. Full actor D/E declaration/binding publication and mixed-cell execution on this joined tip remain required |
| M1 raw/typed calls, retained output/input, loss/retry/reconnect, reload, compaction/late output, control/retirement/host loss | Facade `m1_host_tests.rs`, `m1_browser_runner.rs`, `m1_real_host_late_output_tests.rs`, `embedded_pending_compaction_tests.rs`; harness Engine/Store | `budgeted-raw-host.log` proves one strict original-call committed 42 at an earlier checkpoint. Earlier weak contains-42 host assertions were superseded. Current 9986 projection tests passed 2/2 but are not browser tests. Joined real browser, typed Haskell/installed tools, real late-output/compaction and explicit host-loss cleanup remain gates |
| Distinct-process production cold startup and reconnect/no replay | Facade `embedded_recovery.rs`, `embedded_recovery_tests.rs`, actor_host startup; runtime owned recovery; harness Store successor CAS | Runtime fresh-process recovery primitive passed separately. Actual production cold gate previously failed receipt size and package Address installation; repairs and actual bootstrap seal are joined/reviewed, but no successful current production cold matrix is established here |
| M2 interleaving and independent control | `local_actor.rs`, owned workbench, private/publication owners | Pending singleton remains. No keyed production admission or full A/B/A, two parked plus third, completion-order shadowing, invalid joins, independent cancellation/drain/shutdown acceptance |
| Two capture children reply before parent returns, failure/delay/revocation/partial launch, ordinary deferred boundary | `start.rs`, `lineage/`, resident fork-group/publication; facade `embedded_captured_unfold_tests.rs`, checkpoint child tests; actor `capture_workspace_tests.rs` | Older two delayed children native capture fixture passed. It does not establish the distinct ordinary-Haskell facade before-parent-return and workspace/partial-launch cases on the joined tip. Existing exact fixtures must execute; ordinary deferred children must retain enclosing publication boundary |
| Nonempty persisted child after restart; corrupt/missing artifacts, full original inventories, rename faults, uncertain child provider exclusion | Runtime `recovery.rs`, `newrecovery_v2.rs`, publication/initialization owners; facade production recovery and child readiness | Root original-declaration cold fixture and nonempty child fixture exist; their production executions/complete negative matrix remain required. Primitive G0/Unconfirmed confirmation does not establish child provider readiness or browser restart |
| Compiler/native miss/hit, package/source/boot invalidation, fresh mutable state, demand and custody | GHC pipeline/exact hydration/SOURCE cache; toolchain artifact cache; native image registry/install/evacuation; prepared execution/differential suites | Native planned-declaration and SOURCE suites previously passed; narrow native literal/image/rollback/custody tests passed at owner revisions. Full joined demand omission, late demand, inherited versus fresh CAFs, rollback and final-owner tests plus package/source/boot invalidation must be selected/executed |
| Actual N=1/10/100, B=0/100; copy/hash/write/lease/time/memory, park/cancel/reaper | Runtime `turn_scaling_tests.rs`; binding membership/chunks/name owners; compiler interface IO counters | Frozen `acd5c5ee2c` B0 N1/10/100 native cases passed; N100 1,682.88s, 10,302 historical Val read/decode attempts, 1,863,140 historical payload bytes. B100 and park/cancel/reaper matrix remain open. Later single hydration/scoped lookup need matched reruns; primitive cost matrices are not 100 compiler settlements |
| Native execution/generators, harness Rust/web, warm and controlled input invalidation, corpus, verify, package startup, Codex | Buck focused targets and existing isolated libtest helper; `scripts/buck-cache-gate.py`; `scripts/verify.sh`; fixture corpus/embedded producer manifest; packaging flake; Codex backend outside Buck | Earlier process/native smoke/planned/SOURCE actions and generators passed bounded selections. Current consumer linking alone is not execution. Run complete structural corpus and canonical embedded producers after translation/serialization changes, required lint/default tests/suite-registration/deployable-extractor constituents, matched 9986 web/Rust/cache/package startup and independent default Codex path |

Relevant retained indexes are `engine-harness-completion-evidence.md`,
`engine-harness-m1-remaining-gates.md`, and
`target/completion-evidence/final-delivery/protected-prefix-scale/README.md`.
The scaling packet explicitly excludes later include/admission migrations and
reports unexecuted B100 cases. Its counters measure attempted decode/read calls
and returned/written payload bytes, not physical disk IO.

## Reporting and closure rules

Attach exact source, matched frontend/worker/assets hashes, command, executed
count, exit status, logs and cleanup to each row. Keep candidate source review,
compile-only, primitive execution, production acceptance and packaged acceptance
separate. Re-run affected gates after repairs; do not require unnecessary broad
reruns after unrelated changes. Preserve final source/WIP/bundles and reproduction
commands. Reused review threads cannot satisfy the fresh-context M1, engine and
M2 review gates required by the plan.
