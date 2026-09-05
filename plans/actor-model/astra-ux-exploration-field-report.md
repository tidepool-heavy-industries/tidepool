# Astra UX exploration and root interview

Observed 2026-09-05 after the retained-value write-barrier repair.

## Outcome and evidence

A medium-effort Astra root completed a computation-driven exploration with two
low-effort children, reused resident functions and values, introduced a follow-up
type after the fork, interviewed the retained researcher, and stopped both
children through typed handles while preserving their worktrees and history.
There was no recurrence of the heap failure. The observer then interviewed the
retained root directly through tmux, including a follow-up challenging proposed
new APIs. This was a bounded run, not proof of days-long reliability.

- Workspace: `/tmp/shoal-console-ux-exploration`, an independent clone of Shoal
  Console. Shared source checkout and main branch were not modified.
- tmux: `shoal-console-ux-exploration`; retained root pane `%443`.
- Run: `62f1be60-63e5-4943-9c5c-6ab39bca91a8`.
- Root: `01a070ce-99d5-76c3-8838-69ba779e3601`.
- Coding child: `01a070d0-7999-7fc2-b28d-17be9283eff3`.
- Research child: `01a070d0-7a4b-7321-8068-1f9346f6d8cf`.
- Codex runtime: `118e1cfcd1d7dd120460ff0685e976f0d17327dc`.
- Prompt: `/tmp/shoal-ux-exploration-prompt.md`.
- Host log: workspace `.shoal/logs/62f1be60-63e5-4943-9c5c-6ab39bca91a8.log`.
- Full transcripts: Codex session JSONL files dated 2026-09-05 containing the
  thread UUIDs above.

## What it discovered

At seed 42, time zero, viewport 64×9, pinned amplitude A=0 versus live B=5,
all 64 exact samples differed while all 64 columns rendered the overlap diamond.
The maximum measured sample difference was 56 thousandths. The experiment
distinguished numerical equality from coincidence after integer row projection.

Candidate `a0a964f990022e3a9e462798dcabb1073376bc49`, branch
`shoal/overlap/experiment/branches/counterexample`, adds a real-renderer test
and an evidence note. Child output reported
`cargo test complete_cell_overlap_can_hide_distinct_domain_samples -- --nocapture`
passed, with formatting and default build-directory policy. The root reviewed
the exact commit and ran `cargo run --quiet -- --smoke-reference` successfully.
The observer inspected the candidate but did not independently rerun the child
test. Its assertions establish divergence plus complete visual overlap; the
64/64 count and maximum difference are measured output, not locked assertions.
The candidate is unmerged. The root's original experiment note remains an
uncommitted scaffold in the disposable clone.

The model retained and reused these ordinary Haskell definitions:

```haskell
sameCell h a b = a /= b && row h a == row h b
collisionCount h =
  length [s | s <- [-2000..1999], sameCell h s (s+1)]
data Followup = Followup
  { newEvidence :: [(Int,Int,Int)], question :: String }
```

`row` supported examples, enumeration, and child reasoning. The researcher used
the inherited helpers with the newly introduced `Followup`; `usageView` condensed
provider observations. These task-specific definitions were useful without
shipping a campaign schema or constraining values to a serialization format.

## Runtime observations

The typed first-usage values match the first token-count events independently
read from each child's durable rollout:

| Child | First cached | First uncached | First total input |
| --- | ---: | ---: | ---: |
| Coding | 16,640 | 14,304 | 30,944 |
| Research | 16,640 | 14,297 | 30,937 |

That is about 53.8% first-child reuse. Larger later counts describe within-child
reuse, not the original fork. Initial missing observations remained `Nothing`.
These measurements do not prove that the entire inherited prefix was cached.

Both `stopAgent` calls returned `StoppedNow`. The host retired both applications
at 09:06:51 UTC; its root activation log contains the two expected watch events,
both queued before retirement, and no additional child-stop activation. A late
interview-watch notification arrived after the root had already read the result.
It safely polled again without replaying an accepted effect.

The run exposed one source error: an unresolved `requestWith` result type was
misclassified as compiler infrastructure failure. Receipts showed no request
acceptance; `requestWith @Finding` succeeded. The finishing pass replaces that
exception with a typed source rejection and actionable annotation guidance.

## Root interview

The root called the experience “intellectually enjoyable and effective ... but
uneven.” Its strongest observation was that residency changed the investigation:
turning a suspicion into `row`, `sameCell`, and enumerated counterexamples felt
better than reconstructing prose arguments or disposable scripts. Full context
mostly accelerated coordination; retained executable models affected reasoning.

It would choose Shoal for differential testing, protocol analysis, competing
implementations, and other campaigns with a durable conceptual model. It would
currently prefer an ordinary harness for a localized or mostly sequential fix.
Managing repository, actor, response, and language state imposed real attention
cost. It wanted the system quieter, not less expressive.

Experienced friction: the initial 800-plus-line `:browse` dump, large nested
receipts before it wrote projections, the misleading request diagnostic, and
completion-shaped interim messages while waiting. It would preserve definitions,
typed handles, settled dependencies, authoritative receipts, and custody.

Initially it proposed selective inheritance. When asked how much of that benefit
could come from ordinary projections, an authored index, and a cleaner prefix,
it called selective inheritance premature. It could not separate token carrying
cost from attention/navigation cost in this experiment. Its revised recommendation
was to retain exact-prefix forks, improve authored working habits, fix the type
diagnostic first, and defer new inheritance machinery. It explicitly rejected
mandatory indexes and standard campaign/finding records.

Waiting mostly made the human-facing transcript awkward. It did not lose evidence,
mis-handle dependencies, or replay effects. Better interim wording should precede
lifecycle changes. Generic bounded rendering remains a possible later experiment;
task-specific projections should be tried first.

## Other uses worth exploring

- Keep a small executable reference model beside the implementation; fork
  children to find counterexamples, then fold and shrink typed failures.
- Retain exact datasets and compare alternative renderers or resolutions using
  local functions instead of repeatedly extracting and parsing printed output.
- Explore equivalent keyboard/mouse histories and shrink divergent domain states.
- For maintenance scripting, experiment with resident typed edit plans and
  postconditions over existing file/Git owners. This could replace disposable
  parsing and bookkeeping scripts without creating another Git or process API.

The next development milestone is a real Shoal improvement authored and reviewed
inside Shoal, with the outer harness available for observation and recovery.
