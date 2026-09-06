# Shoal iteration after the numeric tree

## Decision

Keep the recursive shared-scaffold / implementation / independent-review model.
The completed numeric run demonstrated correctness value, not cost superiority.
Prioritize reliable steering and launch custody before increasing fan-out.
Second priority: remove avoidable build/setup cost at existing test owners.

Verified completion: source 95e5eed6, outcome 1746e715. Root final status finds a
clean checkout and 30 roster entries with no current assignments. This includes
root, retained idle contexts and a failed launch, not peak concurrency. Product
checks: 67 focused tests, 217 fixtures, independently executed hosted test with
17 unchanged probes at the same source. Live root binary remains unreplaced.

## What to preserve

- Exact committed seed, executable shared fixture, one integration owner.
- Separate implementation and independent acceptance; native oracle in addition
  to reference/JIT agreement. Classifiers passing did not close finite Show.
- Fresh review and local repair loops. Review caught constant-folded tests,
  cleanup holes, and unsafe eager evaluation in the initial primitive-laziness fix.
- Retained specialists and typed follow-up results tied to exact tested commits.

## Next independent obligations (proposed; not launched)

### 1. Live steering and launch integrity

Owner: agent Codex active_update/process transport; worktree/actor custody for
the distinct launch failure. Do not combine their state machines or paper over
failures through unconditional retries. Split diagnosis/proof by those owners
before implementation and fresh review.

Observed: repeated UpdateNotPresented during proxy initialize; one allocated
child failed WorktreeUnauthorized before provider execution. Source trace:
active_update::present uses process::Session::connect_proxy, spawning the same
installed executable with `app-server proxy`, an allowlisted environment and
initialize handshake. WorktreeUnauthorized comes from the owning worktree grant
boundary; exact cause of this launch remains unproven.

Acceptance: (a) real active and watch-waiting targets receive a correlated update
on the same request, (b) admission/presentation/incorporation remain distinct,
(c) pre-submission disconnect vs uncertain submission have correct typed outcomes,
(d) timeout/disconnect reaps the exact proxy, with useful retained diagnosis,
(e) newly allocated child can exercise its legitimate worktree grant while a
sibling remains denied, including launch-failure cleanup. Include a live canary;
mock protocol success alone would repeat this run's blind spot.

### 2. Pure-IR testing cost boundary

Owner: tidepool-testing generator dependencies and existing scripts/just commands.
repr uses tidepool-testing only for arb_core_expr in two integration test files,
but the test crate unconditionally depends on runtime, codegen, toolchain and MCP.
The standard battery also resolves/starts extractor infrastructure for pure tests.

First measure a cold/warm focused repr build and inspect its dependency tree;
then choose between isolating the pure generator module behind dependency features
or moving it to a narrow shared owner. Preserve one generator implementation;
do not create a second test launcher. Keep extractor-backed commands mandatory
for extractor consumers. Acceptance: same tests and coverage, changed consumers
compiled, reduced dependency/setup evidence, explicit before/after conditions.
Do not claim a time improvement from warm-versus-cold comparisons.

## Coordinator-owned iteration

- Use task-local RevisionCheck's execution outcome, expectation, tested revision
  and retained evidence. Existing captured response types stay valid; no mutation
  of contracts under active agents. Promote a helper only after reuse proves it.
- Fork reviewers from acceptance-focused context before unrelated history grows.
  Avoid broad process listings/full output in shared prefixes. Selected-context
  leaves are a separate experiment, not equivalent full-prefix inheritance.
- Keep watches for independent results but fold user-facing updates around changes
  in diagnosis, integrated baseline or blockers, not every acknowledgment notice.
- Prefer native long waits; document existing output controls. Already landed:
  NEXTEST_SUCCESS_OUTPUT=immediate and expected-failure terminology in guidance.
- Match workspace formatting via cargo fmt; the reported churn came from explicit
  edition 2024 despite workspace edition 2021, not proven missing configuration.

## Measurement and remaining product work

Capture bounded existing provider tracing/usage only with intentional privacy and
storage scope. Report actual normalized input/cache usage, cold/warm build time,
review-discovered defects, repair rounds, root intervention and final cleanup.
No serial-vs-tree or cache/cost benchmark exists for the completed run.

Separate numeric follow-ups: benchmark extra thunking, audit old formatter
workarounds without changing JSON contracts casually, decide reference boxed-array
support and canonical in-memory Float representation. Do not conflate these with
Shoal reliability fixes or imply the focused numeric acceptance was a release
battery. Useful actors remain retained; complete retirement/resource reclamation
is still an unexercised campaign boundary.

Evidence: FLOATING_POINT_BUG_REPORT.md and the numeric-tree sections of
plans/actor-model/live-context-unfold-dogfood-followups.md; full typed surveys and
deliveries remain in the root and owning lead contexts.
