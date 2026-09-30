# Bounded model turns and parallel operating tools

Approved 2026-09-30. Implementation baseline: Tidepool
`d9d82f46ff5f702a16257c2dd9e42fe745e27265`, harness
`814b1697226344e8fd16196666e41c184a73531d`, workspace
`5248b927e7b432d1747891df5285d6827eace7d6`.

## Contracts

One model turn may contain several provider requests and supplied tool calls.
It ends with a final answer or explicit failure. Authored Haskell owns a typed
turn description and ordinary composition, not a conversation handle, actor,
worktree or new orchestration framework. Reuse AgentSpec tools, schemas,
dispatch and explicitly supplied after-tool hooks. Rust's existing harness
Engine owns provider progression, retained evidence and tool scheduling.
A private callback trampoline runs Haskell handlers in the caller's original
continuation and full effect stack; no recursive machine entry.

Each admitted cell supplies one shared model allowance: initially 16 provider
requests, 64 tool attempts, 128000 reported tokens, and 300 seconds starting at
its first invocation. Nested and concurrent calls share it. Individual calls
may narrow it. Exhaustion latches and prevents further model work; token usage
is a cutoff, not a monetary guarantee. In-flight or missing usage is explicit.
Already-admitted callbacks finish cooperatively before the wrapper can return
a typed failure. This breaker does not cancel the parent cell or undo effects.
Jev and spawned agents retain their own independent resource ownership.

Retain invocations beneath their calling cell, with tool/request identities,
usage, terminal outcome and evidence references. Do not add actor-tree nodes.
The engine owner supplies the retained cell identity/budget/cancellation hook;
do not derive authority from tracing context or handler clone count.

## Worktrees and owners

All worktrees are under `/home/inanna/dev/rsi-model-turns/`.

| Directory / branch | Owner and responsibility |
| --- | --- |
| infra / rsi/remote-buck | Sol: project NativeLink worker, client configuration, remote execution/cache qualification |
| harness / rsi/bounded-model-turns | Sol: existing Engine invocation profile, callback transport, shared budget, terminal schema support |
| model-effect / rsi/model-effect | Root: Haskell surface, protocol, adapter contract, integration and review |
| observability / rsi/run-observability | Luna: existing run-map filters, Perfetto export, evidence coverage |

Release preparation is a grounded handoff to the existing deployment and run
owners: no matched release/compatibility interface exists yet. Four authoring
examples and a native Haskell contract check are implemented.
At most three implementation agents plus root. One expensive check at a time;
remote-build owner has the first slot. No shared daemon restart, broad cache
deletion, dirty-work removal, push, provider trial or application activation.

## Remote builds and release preparation

Author locally; use the server's existing NativeLink scheduler/cache through
an SSH tunnel. Register a separate project worker using the exact Nix closure
and existing sandbox owner without restarting shared services. Initially one
action, four CPUs and 12 GiB. Qualify actual Rust build/test, Haskell compile,
independent-client cache reuse and changed-input invalidation. Retain action
evidence. Admitted SSH builds in isolated server checkouts are the fallback.

Release scripts consume the engine team's matched packages. Stage immutable
closures, validate identity/assets/state compatibility, atomically select new
releases for future runs, and retain closures referenced by existing runs.
Resume existing runs on their recorded release. Selection rollback never
silently downgrades a database. Production activation remains deferred.

## Observability and authoring exercises

Extend run-map's existing readers with actor/execution/call/time selection,
timeline export and usage reconciliation where evidence exists. Preserve
source references, cutoff, observed/inferred/unknown, omissions and incomplete
tails. No invented causal edges, summed overlapping spans, or inferred zero
cost. Store observation must not invoke migrations on a live database.

Use four compiled procedures to refine the API: change routing from actual
diff/consumer evidence; dependency clarification and handoff through supplied
tools; concise handoff preparation retaining check/source references; bounded
failure investigation without rerunning the original job to recover output.
Routing and coordination are primary. Deterministic owners enforce destination,
identity and authority checks. Model judgment does not establish acceptance.

## Verification and finish

Use scripted provider tests through the real Engine and compiled dispatcher.
Cover multiple tools, nested calls, shared budgets, malformed calls, typed
completion, callback failure, cancellation, late outcomes and cleanup. Exercise
observability on sanitized retained evidence and import the exported timeline.
Record missing release selection/retention interfaces without inventing a
second deployment owner. Compile changed
targets, run exact focused checks, format and diff-check; distinguish executed
tests from compilation and integration still owed to the engine owner.

Finish with independent exact-commit review and a dependency-ordered handoff.
Pause before any live wave or default-backend change.
