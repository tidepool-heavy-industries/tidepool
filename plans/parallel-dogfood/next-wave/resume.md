# Next run: resume the consolidated trees on main

The previous coordinator finished wind-down. Both lane requests and its own request
settled partial/Blocked; no engineering request needs to be kept alive to recover
committed work. The latest coordinator scrollback confirms the final state, which
supersedes older in-progress sentences inside the saved handoffs.

## Starting source

| Input | Commit |
|---|---|
| Applications checkpoint | `326255bc32f589f9352c181d3245812f9e630862` |
| Engine checkpoint | `1aabf25cb9393817c0c11d34595b92c96d669b8b` |
| Combined coordinator handoff | `e0c37ff398def968e57350f8538b7990a8f7ee87` |
| Applications' external Codex candidate | `72f7b60008452a1f63c5cb92f42c731dc67fbb49` |

Read `plans/parallel-dogfood/next-wave/coordinator-partial-checkpoint.md` and the two
lane handoffs at the combined commit using `git show`. These consolidated commits
are the integration inputs; leaf hashes are provenance, not a reconstruction list.
Retained failed/uncertain resources remain separate from accepted product source.

The launch record selects current **main** for the running harness and its frozen
prompt/Haskell package. It includes the routing improvements from `6b502116` and
explicitly unbounded default concurrency. The running native Codex remains the
main pin, independently of the applications candidate above.

Before implementation forks, each Sol lead creates a new continuation branch from
its lane checkpoint and **rebases it onto the launch-recorded main commit**. Preserve
the original checkpoint refs and existing worktrees. Preserve meaningful merges;
resolve conflicts against current owners and keep main's orchestration package.
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
| Applications delivery | Complete the existing native/host round trip | Socket submit/query/withdraw/seal/ack and Remote behavior; real PTY lost-ack/stale-generation; host late-poll/no-overtaking |
| Applications custody/completion | Connect checked A4 process ownership to A5/A6 | Hosted-work retirement, completed-call/context release, adversarial owner-loss consumers |
| Applications recovery | Integrate A7 against the checked engine disposition | Source recovery and lost live-state reporting; join evolving custody/completion without waiting for all M2 reclamation |
| Engine prepared frontend | Close remaining M1 worker/corpus and malformed-site recovery evidence | Use the integrated elaboration/facts; diagnose the distinct provider and pre-binding failures without replacement chains |
| Engine memory | Complete M2 beyond machine-lifetime retention | Establish wrapper-complete root ownership; split reclamation/accounting from retirement and mixed-lifetime checks after agreeing the contract |

The existing native bridge, prepared frontend and memory-safety repair are starting
assets, not assignments to implement again. Each lead owns substantive integration
and forks meaningful implementation subtrees; reviewers are not the only children.

M3 schema/execution work opens from real prepared evidence. M4 codegen and M5 heap
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
