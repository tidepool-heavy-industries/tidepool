# Next run: resume the consolidated trees on main

Resume the final r6 checkpoints below on the launch-recorded main revision.
Git preserves source and handoffs; it does not recreate live TUI/Haskell handles.
The checked main runner and frozen `.shoal` package remain separate from product
candidates throughout the run.

## Starting source

| Input | Commit |
|---|---|
| Consolidated coordinator checkpoint and final census | `4ac97b13c1b524e2ca050f37c5160210d9ffed40` |
| Engine lane and final preservation ledger | `059c1677c3dcb71f9afd1a98e522325c8c378beb` |
| Applications lane and custody handoff | `c19aaa84aa5cd0b99431457b2ec0c7b2d75a6a31` |
| Introspection partial implementation | `82400a24b88c67b103245f367983e0511839ad84` |
| Native A1/A2 candidate (Codex repository) | `b2163064d3b52b3e1a7ee458603f661d6162a165` |
| Native completion tested sibling (Codex repository) | `d140e7ecf4df19d69ad200be405f5866d4b4d4a0` |
| Native A6 continuation (Codex repository) | `6648c73f5d921e47fe81ce26e4caa99ef2a2fdd3` |
| Preserved six-file recovery WIP (Codex repository) | `9d68c0dbb1c48614392a0e0079ce8fcb70c75c94` |

Read the coordinator's `plans/parallel-dogfood/resource-wave/coordinator-wind-down.md`
at its exact commit, then the assigned lane's handoff in that tree. Use `git show <commit>:<path>` when the file is not in main. Engine additionally reads
`engine/engine-preservation.md` and `engine/m2-preservation.md`; applications reads
`applications/applications-handoff.md`. These supersede the older OOM recovery
inventory and old launch/release messages.

The coordinator has already incorporated applications, engine and introspection
partials. Engine includes M0/M1, M2 candidate `362094cd` and M3 scaffold `cb67a840`;
its M2 preservation ledger is `1ecdbafb`. Do not implement those foundations again.
The native refs are divergent candidates, not one accepted linear implementation.
Reconcile them explicitly and preserve the recovery WIP ref before selecting a
matched native candidate. No product megatask is accepted by these checkpoints.

Main owns command-resource admission, supervisor/disk repairs and the curated
actor/coordination package. Preserve those owners during reconciliation. The
launch record selects exact main, native pin and package fingerprints; older
hashes in product plans do not select the running harness. Full applications,
engine and introspection acceptance remains on the product branches.

Before implementation forks, each lane must descend from the launch-recorded main
commit. Use an already-reconciled continuation when supplied in the launch record;
do not repeat its rebase. Otherwise create a new continuation from the preserved
lane checkpoint and rebase it onto that main revision. Preserve original refs and
existing worktrees. A consolidated continuation may squash the lane's net change
since its shared main ancestor, recording the exact original head and ancestor in
the commit message. This preserves source provenance without replaying hundreds
of intermediate handoff commits. Resolve conflicts against current owners and
keep main's orchestration package.
Record the resulting head and check the affected integration seams. Do the same
source reconciliation for external Codex against its recorded tooling baseline.
If that baseline is already an ancestor, no redundant rewrite is needed.

The coordinator combines the rebased lane heads on a product integration branch.
Do not rebase main onto the combined candidate or merge unfinished product changes
into main to obtain a runner. Candidate compiler/native executables used by product
checks must be selected explicitly, separately from the running swarm's binaries.

## First useful work

| Owner | Opening result | Parallel work after the local contract is usable |
|---|---|---|
| Applications delivery | Review the preserved host partial and reconcile native candidates | Real native/host failure paths and matched protocol checks |
| Applications completion | Complete A6 durable completion/restart and exact release | Hosted-work retirement and adversarial owner-loss consumers |
| Applications recovery | Recheck A7 against the resulting engine source | Source recovery and truthful lost-live-state reporting |
| Engine memory | Independently review M2 and finish its consumer/failure matrix | Reclamation/accounting and retirement/mixed-lifetime checks |
| Engine schema | Extend the preserved M3 scaffold and accepted design | Writer, bounded decoder/validator, linker and reference vertical |

Introspection remains a bounded additional obligation: execute the real resident
Eff reentrancy path and obtain independent review of the preserved implementation.
Allocate it where its engine/actor ownership fits; do not create a third megatask.

The existing native bridge, prepared frontend and memory-safety repair are starting
assets, not assignments to implement again. Each lead owns substantive integration
and forks meaningful implementation subtrees; reviewers are not the only children.

Use the retained prepared evidence and accepted M3 amendment for schema/execution work. M4 codegen and M5 heap
work fork from a shared signature/layout/root contract; M6/M7 follow real production
consumers. Applications A8 owns the matched full-TUI package and failure-path join.
These are dependencies, not globally synchronized rounds. The objective remains
both complete megatasks; useful partial commits arrive throughout the run.

## Who does what

A Shoal-managed Astra planner refines the recursive graph and context packaging,
then reviews the two Sol leads' understanding once. Sol Medium leads/coordinator
own execution; bounded workers default to Sol Low. Related workers inherit a useful
completed reasoning prefix and the current bound checkout. Fork before unrelated
investigations fill the parent's context. Each parent keeps real engineering work.

Use fresh, compact Astra consultations for consequential native identity/custody,
wrapper/root, GHC schema or ABI questions. Return answers directly to their Sol
owner. The initial planner has no routine progress subscription after agreement.

Use the frozen `.shoal/plans/coordination.md` and its checked `followWork` examples:
one local collector for each wave's progress and terminal receipts; typed forwarding
between subtrees; normal steering only for useful checkpoints, changed decisions,
failures or final outcomes. Known review handoffs can execute without a model relay.
Keep failed/uncertain notification receipts, and retire incorporated collectors
once remaining obligations have owners. Do not operate a watch-rearming loop.

## Settled decisions and acceptance

There are no external TPLR consumers. Follow engine design §13: change the writer,
reader and affected contracts together, reject stale artifacts and regenerate them.
There is no external compatibility-window question. Preserve working main and our
Shoal usage: test the matched candidate extractor/runtime against that usage before
merging or selecting it for a later swarm. Source recompilation does not restore
lost live values or grant authority.

Only this x86_64 box is available. Proceed with its checks; native aarch64 acceptance
is unavailable and must not be claimed. Do not repeatedly ask for a missing runner.

There is no default concurrency quota or forced cancellation of in-flight Astra.
Depth and authority still constrain delegation; explicitly requested finite widths
remain enforceable across subtrees. External RSI watches builds, snapshot reuse,
disk, models/context ancestry, messages, compactions and available usage evidence.
Use existing tracing/observations and preserve missing coverage as unknown.

External RSI may steer any owner through its TUI and verifies that input was sent.
It owns harness investigation; the product trees own product checks. Have the Sol
coordinator warm its main build once before admitting the leads;
a new root has no inherited build snapshot. After each lane rebase, its lead runs
the first useful owning build/check and publishes a completed warm snapshot before
its implementation forks. Run focused checks that select actual tests, share completed warm build snapshots, and avoid
broad batteries before every fork. Keep the running tools and canonical .shoal
fixed. Launch is a separate operator action, not authorized by reading this plan.
