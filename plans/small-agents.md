# Small agents as typed, programmable components

## Intent

Extend Shoal with lightweight agents whose context, tools and result contract are
constructed by the parent in Haskell. They should feel as convenient as native
subagents, but participate in Tidepool's typed requests, authority, cancellation,
retained evidence and orchestration rather than forming a second agent system.

This is a design plan, not documentation of an already implemented public API.
We already have substantial enabling machinery: resident Haskell, typed actor
requests/replies, custom host tools, backend adapters, and actor supervision.
Inspect current owners before deciding what actually needs to be added.

## User preferences to preserve

- Small-context, Low-effort workers for bounded tasks; Sol Low is an initial
  model choice to evaluate, not a claim about measured cost or quality.
- Display them in the **same tmux pane as the parent**, with clear subordinate
  activity, not another top-level pane/window for every tiny task. This is a
  presentation preference, not a request to merge their identities or histories.
- Use the **same worktree as the parent** by default for these workers. They
  are a different mode from independent specialists with their own worktrees.
- Give them useful Tidepool tools and task-specific custom tools. Let the tool
  interface itself shape behavior, not only a paragraph telling them what to do.
- Define reusable helpers and tool-wrapper specifications in ordinary on-disk
  `.hs` files, so agents can improve them instead of retyping them each run.
- Construct initial context from typed input using a formatting function or a
  fmt-like template. Context selection should be authored, explicit and small.
- Keep native Codex goals disabled. The parent/Shoal owns continuation.

## Two complementary context modes

**Exact-context specialist:** inherit the rich parent prefix and own substantial
implementation, semantic judgment or review. This is the recursive scaffold /
unfold / review / integrate model; do not replace it with lossy task summaries.

**Small typed worker:** receive selected input, a bounded context and a tailored
interface for one obligation. The parent has already done the shared reasoning
needed to select that obligation. It deliberately does not inherit everything.

These must be explicit choices. Small input does not imply weak authorization,
and full context does not grant the parent's capabilities. Neither mode should
silently replace the other based on a heuristic token threshold.

## Desired programming experience

Illustrative shape only; these names are not a proposed committed API:

```haskell
verdicts <- traverse (runSmall classifierSpec) cases
let disputed = filter needsInvestigation verdicts
inspectFull (map summarize disputed)
```

A specification would connect:

1. Typed task input and typed success/failure result.
2. Stable behavioral instructions plus an input-to-context rendering function.
3. Model/effort and bounded execution policy.
4. Available Tidepool operations and task-specific tool exports.
5. Explicit workspace/capability policy and parent presentation placement.

The parent should be able to create the specification during a session, apply it
to several inputs, retain the replies, and compose them with ordinary functions,
`traverse`, and existing `Await`/`Watch` dependencies. Effect traversal does not
itself imply parallel execution: scheduling must use the owning actor mechanism.

## Custom interfaces as executable task design

Instead of exposing a giant generic toolbox and asking a worker to behave, give
it a small vocabulary tied to the problem. A floating-point classification worker
might receive a case plus operations to observe it under selected engines,
request a nearby counterexample, and submit a typed discrepancy.

Those operations should plug directly into the parent's authored orchestration:
arguments are validated, results are typed, and the implementation can reuse
retained functions and effectful helpers. The tool is not merely a prose wrapper
around another unconstrained model call.

Prefer familiar resident Haskell where it fits. Custom tool transport/schema
projection should reuse the existing host machinery, not require agents to hand-
maintain JSON protocols. Keep a useful standard Tidepool surface available where
appropriate; a tiny task need not expose every native tool or arbitrary access
to its parent's entire workbench.

A parent's exported closure is **not** an ambient authority grant. Bind each
export to an explicit allowed operation and calling principal. Rust must enforce
resource access; child-originated calls must not accidentally execute with the
parent's unrestricted authority. Reject out-of-contract calls truthfully.

## Reusable on-disk Haskell

Support the path from experimental live definition to reusable source:

- Shared data types, input renderers, tool wrappers and acceptance functions live
  in task/project `.hs` modules that can be imported or loaded through the owning
  workbench/toolchain path.
- Compile/typecheck definitions against their real production consumer. A mock
  implementation must not advertise unavailable runtime behavior as working.
- Capture the module/source revision and compiled definition used for a worker.
  Editing a file must not silently change the meaning of an existing closure or
  an already admitted assignment. Apply revised definitions to subsequent work.
- Provide legible compile diagnostics and an explicit reload/redefinition path;
  do not invent a second compiler daemon or file watcher.
- Let results include usable functions/strategies where the resident value and
  custody model supports it. Do not require everything to collapse into a text
  report, or pretend arbitrary live closures survive restart/serialization.

Initial-context rendering is an explicit `input -> Text`-like boundary. Reuse
available interpolation/formatting support if suitable; a new fmt macro is not a
prerequisite. Rendered repository/task content remains data, not permission to
change the worker's behavioral contract.

## Shared worktree and parent-pane semantics

Same worktree must not mean uncontrolled concurrent edits. The parent owns the
shared workspace; scope writes by file/task, serialize conflicts, or have workers
return proposed patches for parent application. Choose a clear policy before
allowing mutation. Read-only classification can be the first vertical slice.

Keep each worker's actor/request/provider identities, tool-call attribution and
result independent even when visually nested in one pane. Use one presentation
owner; do not let several TUIs write concurrently to the same terminal stream.
The parent should be able to expand a worker's output without importing its full
transcript into the parent's model context.

Cancellation must stop the actual worker and settle its request exactly once.
Preserve completed effects and evidence; a timeout is not rollback. The parent
cannot await a worker from inside an admission block that prevents it starting,
nor may a child deadlock waiting for an occupied parent workbench to service its
custom tool. Resolve scheduling/reentrancy at existing runtime owners.

## Cache and cost discipline

Keep the shared base/API guide stable, not role- or task-specialized. A tailored
tool schema can itself change the beginning of a provider request: this is an
explicit tradeoff, not a free specialization. Prefer stable interfaces reused
across a family of small tasks, with varying typed input in the suffix. Do not
silently give every tiny worker a uniquely rewritten tool-description prefix.

Measure first-response and subsequent cache reuse separately. Include output
and tool costs, latency and acceptance quality; short context is not sufficient
proof of a better workload. Reuse existing opt-in request tracing and bound
private captures. Do not create a competing log or assume saved context metadata
proves final provider-input equality.

## First vertical slice: numeric repair wave

See [the floating-point bug report](../FLOATING_POINT_BUG_REPORT.md).

1. A full-context coordinator defines numeric cases, observations and discrepancy
   semantics, along with real engine/oracle adapters.
2. A small worker receives one discrepancy and a stable, narrow Haskell tool
   vocabulary. It classifies or minimizes it without modifying shared source.
3. The parent maps the specification over a bounded case collection and validates
   typed results independently. Larger semantic questions go to retained exact-
   context specialists; routine execution stays ordinary code.
4. Improve the `.hs` helpers/tool specification against observed failures and
   reuse them in the next batch. The output is new executable testing ability,
   not merely more reports.

Acceptance should cover typed input/result errors, capability rejection,
parent-pane attribution, same-worktree write policy, cancellation and partial
failure, definition-version stability, and actual cache/usage observations.
Compare the small-worker path with deterministic execution and a full-context
specialist where relevant; do not add model calls to tasks that code solves well.

## Implementation boundaries / decisions still needed

- Exact small-worker admission interface and relationship to existing actor roles.
- How exported Haskell functions become authorized tools without parent-workbench
  reentrancy hazards or new registries.
- Loading/versioning `.hs` specs using the existing compiler/runtime.
- Minimal standard tool surface, custom schema stability and context renderer.
- Shared-worktree write arbitration and parent-pane presentation implementation.

Owners to inspect: `tidepool/src/actor_host.rs`, `tidepool/src/host_dynamic_tools`,
`tidepool-actor`, `tidepool-agent`, `tidepool-runtime`, `tidepool-node`,
`tidepool-worktree`, and the Haskell library. Root and nested `AGENTS.md` govern
implementation. Reuse those owners instead of creating another agent framework.
