# Live context-unfold dogfood follow-ups

Status: active. This is the root plan and checklist for hardening the landed
cache-preserving context-unfold surface from live Shoal Console use. It does
not reopen the accepted architecture in
[cache-preserving context unfold](cache-preserving-context-unfold.md).

## Scope and evidence

The evidence source is the 2026-09-04 read-only observation of Shoal run
`c7580ee6-f6e8-4fa7-a4ab-a11b714d7132` in `shoal-console`. The campaign used a
root discovery unfold, retained-actor recovery, a recursive coordinator-owned
implementation unfold, typed worktree folds and merges, independent reviews,
and a retained-child refinement wave.

The feature campaign itself completed: `shoal-console/main` fast-forwarded to
verified integration head `57e57ffe3e5956174c6c718e57a45fe08bb1a877`, and
the source checkout's pre-existing untracked exercise note remained untouched.
The subsequent lifecycle-cleanup campaign did not complete because stopping
the first recursive leaf was followed by loss of the dynamic-tool host.

The core interaction model worked:

- a dirty source checkout rejected the whole first unfold before partial
  publication; retrying from clean `HEAD` launched the group;
- root-launched children inherited the root provider thread and Haskell
  snapshot, while recursively launched children named the retained coordinator
  as supervisor, context parent, and provider-thread parent;
- hierarchical actor paths projected to readable `shoal/<path>/branches/<leaf>`
  Git branches and distinct managed worktrees;
- every actor used the same actor-relative
  `/tmp/tidepool-actor-workspace` path without path-change instructions;
- narrowed research actors had inspection-only native tools and no build or
  mutation behavior; coding leaves had writable bound worktrees but no launch
  or integration effects;
- retained follow-up requests reused typed values and model context instead of
  replacing actors;
- `ResponseResult` plus `WorktreeEvidence` carried exact request, actor,
  worktree, base, head, committed paths, and dirty-state evidence;
- typed merges integrated a fast-forward followed by two merge commits without
  requiring a manual Git mutation fallback;
- the coordinator and root independently reproduced formatting, 48 tests,
  strict Clippy, legacy smoke compatibility, and the new smoke contract;
- review findings flowed root -> retained coordinator -> retained presentation
  child, producing and merging a focused follow-up commit without spawning a
  replacement.

Cache reuse was not merely assumed. Reported cached/uncached input tokens were:

| actor/activation | cached | uncached | cached share |
|---|---:|---:|---:|
| retained product recovery | 57,984 | 384 | 99.3% |
| retained architecture recovery | 66,688 | 1,501 | 97.8% |
| retained coordinator recovery | 57,472 | 316 | 99.5% |
| recursive domain child | 89,600 | 615 | 99.3% |
| recursive presentation child | 107,136 | 675 | 99.4% |
| recursive smoke/failure child | 94,464 | 742 | 99.2% |
| retained adversarial reviewer | 90,240 | 229 | 99.7% |
| retained integration reviewer | 85,632 | 215 | 99.7% |

These measurements, provider-parent thread identities, fork-group identities,
and inherited transcripts together establish that the run exercised context
forking with prefix-cache reuse rather than unrelated fresh sessions.

## Preserve these boundaries

- Keep the canonical actor-relative workspace path. Models should not receive
  relocation prose or physical cache paths when actors fork or resume.
- Keep context inheritance exact and authority inheritance explicit. Effect-row
  narrowing and Rust grants remain separate mechanisms.
- Keep actors persistent. A reply settles one request; it does not terminate
  the actor or discard its context, bindings, or worktree.
- Keep Git legible. Typed worktree and merge operations provide the safe common
  path and authoritative receipts; ordinary Git remains available for review
  and exceptional conflict handling.
- Keep the GHCi-shaped Haskell surface. Improve its types, diagnostics, and
  prelude rather than replacing it with serialized tool-call records.
- Keep root synthesis and integration explicit. Contexts fork exactly; evidence
  rejoins through typed results and receipts.

## P0: correctness and fault containment

### Keep child teardown subordinate to the permanent host

The final inside-out cleanup successfully forgot the coordinator's two watches
and four responses. Its next input unit issued three reverse-order `stopAgent`
operations. The unit returned only `host dynamic-tool infrastructure failure`;
three subsequent `:status` calls failed identically. The log's last lifecycle
event was actor 8 retiring as `Completed`, its pane disappeared, and actors 6
and 7 remained, proving that the effectful batch partially executed. The host
pane also disappeared while the root and remaining child TUIs stayed open.
No typed stop outcomes or host exit cause were available.

- [ ] Reproduce stopping one leaf and a three-leaf stop batch under a permanent
  root; prove leaf retirement cannot terminate the host service, root tool
  socket, or sibling tool sockets.
- [ ] Audit linked-task ownership so child application/process completion is a
  supervised event, not a failure propagated into the actor host's serving
  loop.
- [ ] Preserve one typed `StopOutcome` per attempted child even when a later
  operation or the enclosing workbench call fails.
- [ ] Make the successful-prefix boundary visible for effectful batches: actor
  8 retired, while later stop outcomes were unknown.
- [ ] Log host shutdown/failure with the triggering actor, lifecycle event,
  task/link relationship, exception/panic, exit status, and affected tool
  sockets before any pane is removed.
- [ ] Keep `:status` and root supervision available after a child teardown
  failure so cleanup can be resumed authoritatively.

### Repair typed reply settlement across resident generations

Two inspection-only reviewers successfully constructed `ReviewResult` values,
but their first settlement attempts produced three runtime failures:

- a JIT heap-shape constructor-tag mismatch;
- a JIT `SIGSEGV`;
- subsequent `no continuation scont_* parked` errors from `respond`,
  `attemptReply`, and a raw `Replies` effect.

Both replies remained open until their deadlines expired. After cancellation,
fresh requests to the same retained actors settled the same retained values
successfully. This localizes the failure to the activation/reply continuation
boundary rather than the value type or actor context.

- [ ] Reduce both failures to fixtures using an inherited user-defined sum and
  record result, a fork activation, persistent declarations, and `respond`.
- [ ] Make the reply operation's result type fingerprint, compiled generation,
  activation generation, and parked continuation agree before execution.
- [ ] Ensure accepted settlement becomes observable before any terminal control
  transfer; a post-settlement JIT fault must not leave an apparently open reply.
- [ ] If an activation continuation is irrecoverably poisoned, atomically mark
  that request unavailable with a typed runtime failure or remount a safe
  settlement boundary. Do not leave an impossible-to-settle `ReplyOpen` until
  deadline.
- [ ] Prove that a fault in one request cannot corrupt later requests or other
  actors sharing an immutable Haskell snapshot.
- [ ] Add the first-attempt failure, retry, cancellation, and retained-actor
  recovery paths to focused acceptance tests.

### Replace untyped deadline integers

`requestDeadline :: Int -> Either Text RequestDeadline` interprets the integer
as milliseconds. Models naturally read `600` as seconds; this caused all three
discovery replies and both initial review replies to miss settlement.

The model-facing API should make time dimensional:

```haskell
withRequestDeadline (after (minutes 10)) options
withBranchDeadline (after (seconds 90)) branch
```

The exact names can change, but bare integers must not carry an implicit unit.
If a compact convenience remains, its human default is seconds; millisecond
precision requires an explicit constructor.

- [ ] Introduce a shared positive `Duration` type with `milliseconds`,
  `seconds`, and `minutes` smart constructors.
- [ ] Accept `Duration` at request, branch, and watch deadline boundaries;
  convert to runtime milliseconds only in the owning interpreter.
- [ ] Show original duration, absolute expiry, and remaining time in `:status`.
- [ ] Include the label and current terminal state in deadline notifications.
- [ ] Test sub-second deadlines explicitly without making milliseconds the
  ordinary LLM-facing unit.

## P1: observability as a typed runtime surface

The current host log correlates failures by actor, model turn, and tool call,
but the reply/JIT incident required reconstructing request state from several
panes. Raw heap addresses, unknown tags, and a list of parked continuation names
are insufficient operational evidence.

Define one structured, sequenced event model owned by the existing actor/runtime
mechanisms. It is an observation projection, not another scheduler or registry.

- [ ] Correlate every event with `RunId`, `ActorRef`, `ActivationId`, optional
  `RequestId`/`ReplyId`/`WatchId`, provider thread, Haskell snapshot/generation,
  fork group, and bound `WorktreeId`.
- [ ] Record reply transitions (`Open`, cancellation requested, accepted,
  ready/unavailable), the effect operation, result type fingerprint, and state
  before/after the interpreter call.
- [ ] Record continuation allocation, park, consume, and invalidation with the
  owning activation/request—not only an `scont_*` name.
- [ ] On heap-shape failure, log expected and observed constructor identity,
  compiled function, source input unit, declaration generation, and type
  fingerprint. Keep raw bytes/addresses as optional deep diagnostics.
- [ ] Expose `:trace request <handle>`, `:trace watch <handle>`, and a compact
  `:lineage` tree rather than requiring pane/log archaeology.
- [ ] Have `:lineage` distinguish supervisor, context parent, provider parent,
  fork group, actor path, Git branch, worktree, role/effect row, and state in one
  tree-shaped view.
- [ ] Emit split/fork metrics: context snapshot identity, shared-prefix tokens,
  cached/uncached tokens and ratio, fork-to-first-token latency, queued/running
  shards, and aggregate incremental cost.
- [ ] Pair the canonical workspace alias with actor/worktree/branch identity in
  diagnostics, while suppressing long physical cache paths in ordinary model
  transcripts.
- [ ] Give durable notifications a sequence/watermark, label, and authoritative
  current state so stale delivery is recognizable immediately.

## P1: workspace and build-cache reliability

Concurrent coding actors correctly saw independent source worktrees through the
same canonical path. However, the runtime-provided actor-specific
`CARGO_TARGET_DIR` disappeared during dependency metadata writes for two actors.
Both recovered by selecting private `/tmp` target directories, paying cold build
cost and leaving disposable directories that their tool policy would not remove.

- [ ] Give each actor a stable build-cache identity and lease for its lifetime.
- [ ] Never clean or replace an active actor's target directory; coordinate
  cleanup through the resource owner after actor teardown.
- [ ] Mount the target at one stable actor-relative location and set toolchain
  environment consistently, without requiring models to invent fallback paths.
- [ ] Log build-cache allocation, mount, lease owner, cleanup, and unexpected
  disappearance as structured resource events.
- [ ] Decide whether compatible siblings may share immutable dependency
  artifacts safely; keep mutable output/lock custody actor-local.
- [ ] Provide a recoverable typed cleanup operation for actor-owned disposable
  verification resources instead of encouraging `rm -rf` workarounds.

## P1: workbench ergonomics

- [ ] Make failed observational commands (`:type`, `:info`, `:browse`,
  `:bindings`, `:status`) report their own diagnostic and continue to later
  units. Preserve stop-on-failure for Haskell evaluation and effectful units.
- [ ] Put the documented `[fmt|...|]` quasiquoter in scope, or remove it from
  model guidance until it is actually mounted. Add one live canary.
- [ ] Make every name printed by `:browse` resolvable by `:info`; in this run
  `EffectWitness` violated that invariant.
- [ ] Recognize the common `f Constructor { ... }` parse/type-error shape and
  suggest `f $ Constructor { ... }` or parentheses.
- [ ] Investigate fenced declaration parsing that rejected an otherwise normal
  signature plus binding, forcing a long single-line `let` fallback.
- [ ] Render successful terminal reply/cancellation transfer explicitly instead
  of ambiguous `<no output>`.
- [ ] Filter inherited client-only noise such as stale “conversation
  interrupted,” MCP-login, and usage-reset notices from forked semantic context.
  Preserve actual user/developer/model/tool history.

## P2: status, names, and campaign hygiene

- [ ] Rename or type `withBranchPrefix` so it is clear whether it accepts an
  actor path or the projected `shoal/...` Git namespace. Prefer conversion from
  an `ActorPath`/`CampaignPath` value over reconstructed text.
- [ ] Add acknowledgement/archive operations for terminal responses and watches
  so completed campaign state remains durable but does not dominate `:status`.
- [ ] Make notifications identify watch/request labels, not only numeric IDs.
- [ ] Keep `bound_worktree` distinct from the managed worktree registry in
  status language.
- [ ] Summarize completed-prefix semantics structurally: committed/rejected/not
  run units, installed bindings, and completed effects.

## Acceptance campaign

The hardening wave is complete only after a fresh campaign demonstrates:

1. a root applicative unfold and a coordinator-owned recursive unfold;
2. provider-parent and snapshot evidence for every fork;
3. at least 95% cached input share for representative sibling forks and
   retained follow-ups, surfaced without manual pane inspection;
4. identical actor-relative workspace paths with distinct worktree/branch and
   stable build-cache receipts;
5. a user-defined typed review value settling on its first attempt across an
   inherited Haskell snapshot;
6. seconds/minutes deadline syntax with truthful remaining-time status;
7. one typed review-driven retained-child refinement and typed merge;
8. no unexplained JIT trap, missing continuation, lost wake, stale unlabeled
   notification, disappearing active resource, or child-triggered host loss;
   and
9. an operator-readable lineage/trace report sufficient to diagnose the whole
   campaign without scraping tmux panes.

## Out of scope

- replacing Git with a large integration API;
- automatically merging model contexts back together;
- narrating physical workspace paths to forked or resumed models;
- turning effect membership into runtime authority;
- eager actor teardown after one request;
- weakening source-custody checks to make dirty checkout admission convenient;
- introducing a second lifecycle, cache, log, or resource registry beside its
  existing owner.
