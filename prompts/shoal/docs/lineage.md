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

Compact actor status separates `first_observed` total/cached input,
`subsequent_usage` cached/uncached input and response count, and `thread_usage`
cumulative counts. These are token counts, not percentages. Subsequent totals
subtract the same identified first response from the provider's thread aggregate;
they are unavailable when that membership cannot be established (including a
change from legacy to durable source IDs). They never sum the bounded sample
history. A fork's observations are from the child's provider thread; parent
rollout records are excluded.
`ForkedPrefix` records launch provenance, not a provider-confirmed cache hit.

Each displayed scope is `Complete`, `Partial`, or unavailable. First-response
coverage is conservatively partial unless the matching aggregate is complete;
subsequent usage inherits aggregate completeness. Complete covers the observed
scope through durable completion, not future work on a persistent thread.
One observed response can give zero subsequent usage without proving the thread
will do no more work. An actor request or provider turn may contain several
provider responses; this split measures provider responses.

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

Inherited context size is unavailable: no current telemetry reports it with a
counting method. First-response input includes more than the inherited context,
and cached tokens do not identify which prefix content was reused. Do not use
either as a substitute context-size measurement or attribute all reuse to the
fork. Any reuse percentage must name its scope; a thread aggregate ratio does
not measure first-child-response reuse.

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
