# Next run: sleep MVP and applications reconciliation

Resume applications from the retained R7 sources below on launch main. Implement
[resident sleep](../../next/resident-sleep.md) from main. Engine is paused; its
refs remain preservation inputs, not an instruction to spawn an engine lead.
Git preserves source and handoffs; it does not recreate live TUI/Haskell handles.
The checked main runner and frozen `.shoal` package remain separate from product
candidates throughout the run.

## Starting source

Use the R7 coordinator `0c1fb83f2285775cb16b212ce2e152bbe9a07374` as the
complete preservation source. It includes applications handoff
`8eda2640b5047786f5dcf2af8b7eae9760e5e767`, engine handoff
`0c1ff8d9995dddfc30756a2d26d5278749571826`, and checked fixture baseline
`6bef6363d95ef5c9bb4749cfd5304c89767d9cb7`.

Read `plans/parallel-dogfood/next-wave/coordinator-wind-down-handoff-r7.md`
with `git show` at that coordinator commit, then the relevant lane handoff in
that directory. Current applications source/evidence and native
`d84cda697a8dac2842bec09dbd7562a3fab4c926` are inventoried in
[applications](../../interactive-applications/current-state-review.md).
The older R6 refs remain Git history, not the launch inputs.

Applications A0–A7 are implemented and matched A8 passed on the retained pair;
reconciliation and final-source acceptance remain. Engine M0–M5 and a bounded
M6 seam are retained; general production M6/M7 remain open. Structured
introspection is included in the retained engine work, not a new third assignment.
The combined checkpoint includes unfinished engine code and must not be merged
wholesale into main as an applications release.

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

The coordinator integrates sleep and the reconciled applications candidate on a
checked integration branch; no engine branch is combined in this wave.
Do not rebase main onto the combined candidate or merge unfinished product changes
into main to obtain a runner. Candidate compiler/native executables used by product
checks must be selected explicitly, separately from the running swarm's binaries.

## First useful work

Applications follows its [wrap-up allocation](applications.md): reconcile the
existing pair, review actual conflicts, rerun owning recovery/full-TUI checks,
and repair concrete failures. Do not commission admission/completion/recovery
from scratch.

Sleep follows its PRD: suspend fifteen minutes with no intermediate inference,
resume once, preserve normal interruption and reuse existing runtime owners.
The engine's retained frontier remains documented in [engine.md](engine.md) for a
later wave. External RSI handles historical custody and infrastructure.

## Who does what

A Shoal-managed Astra planner refines the recursive graph and context packaging,
then reviews the two Sol leads' understanding once. Sol Medium leads/coordinator
own execution; bounded workers also use Sol Medium. Keep effort stable across Sol forks. Related workers inherit a useful
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
