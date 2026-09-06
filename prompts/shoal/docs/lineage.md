Shoal tracks distinct supervisor, context-parent, provider-parent, fork-group,
and Git branch relationships. Use the compact view first:

```text
:lineage
```

Use `:status` for current work and failed actors, `:status!` for full terminal
history, and `:trace`
for provider usage samples, prompt fingerprints, exact identities, and deeper
diagnostics. The canonical workspace path is actor-relative; actor, worktree,
and branch identities establish custody.

First and latest provider observations are distinct: inspect `contextFirstUsage`
and `contextLatestUsage` on `actorContext`, or `rosterFirstUsage` and
`rosterLatestUsage` in a group roster. Each observation retains its source ID,
optional provider timestamp, and cached/uncached input counts. `Nothing` means
unavailable, not zero reuse. Later hits cannot establish first-inference reuse;
equal counts alone do not identify the same response. Polling time does not
establish which actor activation produced historical usage.

Compact actor status shows `first_observed_input=cached/uncached` separately
from `thread_usage`, its completeness, response count, and cumulative input
counts. These are token counts, not percentages. A fork's first observed sample
is from the child's provider thread; parent rollout records are excluded.
`ForkedPrefix` records launch provenance, not a provider-confirmed cache hit.

Use `:lineage` for first/latest source IDs and `:trace` for sample history and
thread/latest-turn summaries. Typed access is available through
`contextFirstUsage`, `contextLatestUsage`, `contextUsageSummary`, and
`contextLatestTurnUsage` on `actorContext`; inspect their types before composing
a query. Aggregates deduplicate provider response IDs and can be `UsagePartial`:
missing usage, unfinished turns, or inconsistent records must not imply zero
cost. Legacy token-count notifications supply samples but no reliable aggregate.
The first observed sample is the earliest available usage evidence, not proof
that an earlier inference had no missing record. A reattached thread can expose
usage from before the current attachment. This telemetry measures provider
reported input reuse, not causal savings against an unforked alternative.

Each actor entry includes a typed workbench posture. `WorkbenchRunningUnit`
means hosted Haskell is executing; `WorkbenchAwaitingEffect` names the effect
boundary currently suspended in its Rust interpreter. Neither should be
inferred from elapsed time or notification prose.

`observeForkGroup (forkGroupHandle worker)` inspects exact admitted group
ancestry. It returns `Maybe ForkGroupSnapshot`; `Nothing` means that the group
or retained roster is unavailable to this actor. `groupRoster` contains exact
actor incarnations and their observation watermarks. It is an observation of
one frontier and its descendants, not a whole campaign inferred from names.
Compose observations of several groups when your campaign spans several waves.
Git branch-prefix queries select a namespace, not runtime group membership.
