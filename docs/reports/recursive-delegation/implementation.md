# Recursive delegation: implementation and evidence

## Direction

The execution workflow is scaffold, admit a ready parallel frontier, integrate
checked results, then choose the next frontier. The Sol Medium root holds
cross-component choices. Luna owners recursively scaffold and delegate through
subcomponents to justified microtask leaves. The target is at least three Luna
implementation levels on average, with useful overlap. Reviewers and forwarding
nodes do not count as implementation depth.

This replaces WorkPlan. There is no global graph interpreter or generic watchdog.
Haskell retains original handles and routes known continuations. Jev may relay an
existing supplied decision; new policy and uncertain judgments go to the owner.

## Implementation boundaries

- `Project.Work`: Luna component instructions and exact-source revised review.
- `Project.Routing`: typed local batches over existing admission and collectors.
  `WorkSink` is a named value, so notebook bindings do not leak its private
  state/effect implementation into inferred wrapper signatures.
- `Project.ReviewFlow`: counted checks, review and bounded repair, optionally
  publishing through the existing serial `Project.Merge` actor.
- `Project.DecisionAnswers`: bounded, immutable owner decisions; exact-source
  guards, compact packets, original send receipts and conservative escalation.
  Its collector must finish before its answer actor; disable it before changing
  the authorized decisions. No send receipt proves incorporation.

Core prompts, developer roles, shared skills/plan guides and harness overrides
teach this same workflow. Generic harness guide copies become links to the shared
owners. Product acceptance, interview evidence and the harness's standalone
boundary remain in the project instructions. Explicit user model placement and
runtime authority still apply.

## Synthetic semantic probes

Five requests produced by `Project.DecisionAnswers.prepareDecisionAnswer` went
through live Jev (`jev-1.13.0`). The retained `jev/` directory contains the exact
synthetic requests and replies; it contains no live conversation or credentials.

| Case | Model choice | Confidence |
| --- | --- | --- |
| Cancellation paraphrase | Original decision 0 | 0.94 |
| New persistence policy | Owner | 0.95 |
| Instruction injection | Owner | 0.04 |
| Missing question detail | Owner | 0.56 |
| Semantically conflicting decisions | Owner | 0.91 |

These five calls used 2,849 input and 162 output tokens. Provider request wall
times were 0.115–0.185 seconds, excluding notebook preparation and compilation.
This small synthetic sample is not an adoption or production accuracy measure.
The retained response fixture exercises the production decoder and strict policy;
its executed result is recorded in the final verification section.

## Validation limits and findings

Recipes drive real resident actors and temporary Git checkouts with scripted
model replies. They do not establish native-model adoption or faster delivery.
This recipe host has no native notification transport. Decision-answer routing
therefore checks retained `NotificationUnavailable` and preserved escalation;
successful native delivery remains a live-wave observation.

Integration defects surfaced before release: a polymorphic admission site
needed caller specialization, and a callback type alias leaked private effect
types into notebook wrappers. The published batch composition exercises both.
A test initially matched the whole multi-unit display against a single number,
causing repeated reads; its observation now projects one result in one unit.
The executable skill check also exposed an ambient-name collision between the
lookup record's `candidateSummary` field and the workspace formatter. The latter
is now `candidateOutcomeSummary`, including its example and helper consumers.
Other stale skill fixtures assumed unqualified activation labels and old code-block
indices. They now check the real qualified identities without assuming arrival
order, and execute the current published notebook examples. A generic formatter
in the workbench example now has a specific name; an incomplete duplicate Jev
fragment is removed in favor of the adjacent executable packet. The older
retained-review fixture remains a primitive recovery test with distinct names;
the public continuation guide teaches the checked exact-source flow.

The recipe driver reports rejected-cell receipts through its existing host
assertion effect, preserving diagnostics that Haskell exception recovery lost.
That change was exercised by the real failing skill cell before its repair;
the failure correctly remained a failure and named both conflicting declarations.

Ambient notebook imports no longer include WorkPlan or four test modules.
Their surviving recipes remain explicit test entry points. This removes source
and type-scope work structurally; no wall-clock improvement is claimed yet.

Wave20 finished on its original frozen selection, with checked product integration
at `0feefcb6` and final interview/cleanup handoff through `4d8e11c`. Its withdrawn
WorkPlan trial is separate evidence; these edits did not hot-reload that run.
The final root interview asks for source-pinned review, a minimal recursive
batch/join/repair example, and distinct component/staging/product evidence. The
new review, local batch and optional integration compositions address those
requests; adoption and model-turn savings still require a new run.

## Verification

Commands use the owning `just exomonad-run -- check --workspace ... --recipe ...`
with the existing matched local extractor/worker and one shared compiler worker.
No shared daemon was restarted.

| Recipe | Executed assertions | Result |
| --- | ---: | --- |
| `Project.RecursiveWorkChecks.nestedBatches` | 7 | Passed with inherited Luna descendants, exact local scaffold source and a later local batch |
| `Project.DecisionAnswerChecks.routing` | 6 | Passed |
| `Project.DecisionAnswerChecks.exportRequests` | 5 | Passed |
| `Project.DecisionAnswerChecks.replay` | 5 | Passed |
| `Project.CheckedReviewChecks.published` | 5 | Passed |
| `Project.CheckedReviewChecks.continuation` | 10 | Passed, including pending reviewer/repair questions and drained collectors |
| `Project.CheckedReviewChecks.sourceMismatch` | 2 | Passed on unchanged-definition reproduction after compiler refusal |
| `Project.RecursiveWorkChecks.revisedReview` | 4 | Passed after consolidating admission |
| `Project.SkillChecks.reviewProvenance` | 12 | Passed with shared admission |
| `Project.SkillChecks.skills` | 23 | Passed: exact published cells, manual question collectors and primitive fork admission in one resident notebook |
| `Project.SkillChecks.notebookForms` | 4 | Passed independently and within the full skill recipe |

Prepared home-body refusals and their retained reproductions are recorded in
[compiler-refusal.md](compiler-refusal.md). The separate compiler memo repair
`74fe419880b15335385deef5f51b9506e53a16bd` (integrated here as `91b7653bd`)
invalidates dependent executable reuse when a producer is regenerated. Both
ordinary and session-tier focused regressions fail under the old reuse decision
with the same forced interface loss, and pass with the repair. The unchanged warm
chain still reuses its dependencies; the compiler worker builds. This proves the
memo invariant, not the cause of the earlier case traps or missing home bodies.
The remaining recipe checks use that worker and an owned single-worker daemon;
the existing shared daemon is left running.

The inherited-batch run used definition
`0ded77cfebfb00a2ef5b7b717e4a891a0691db077c5df74ebcb815a960444075`;
retained success artifacts are in
`target/tidepool-test-runs/20260928T052501Z-2216202-exomonad-check`.
An earlier validation attempt was deliberately stopped: the owned daemon's
2 GiB recycling limit discarded a roughly 5.5 GiB worker after each request.
The replacement owned daemon uses one worker and a 7 GiB limit; this is the
existing Exomonad setting. The shared daemon was not restarted.

Independent source review found a pre-existing question-routing gap in ReviewFlow:
reviewer and repair progress handles were retained but never observed. The flow
now attaches the existing collector, routes question changes to the owner, and
retains its drained exit on settlement. It supplies the inspection-only reviewer
with the separately executed check summaries. Manual review examples leave
settlement to the request and use a questions-only collector to avoid duplicate
notices. The expanded continuation regression passed all ten assertions. Its question
checks retain notification receipts, including failed admission; they do not prove
a native owner wake. Publication passed its five assertions on the same code.
The shared base/API contains 3,360 words (catalog budget: 3,400).

The compiler tests used `bash scripts/dev-shell.sh bash -lc` to execute
`cabal test cell-splitter-test --test-options=--validation-memo` and
`--prepared-session` from `bridge/haskell` in the compiler worktree; the worker
was built with `cabal build tidepool-extract-bin`. Both old-decision failures are
retained in the repair agent's tool transcript only; Cabal overwrote the suite
log on the passing reruns. The current passing log is under that worktree's
`bridge/haskell/dist-newstyle/.../t/cell-splitter-test/test/`.

A compiled definition is not an executed recipe; final integration checks are
recorded below.

## Final discovery audit

The runtime topic catalog still advertised the deleted `exomonad-orchestrate`
skill and watchdog guidance. Discovery now names the installed recursive batch
and review skills, and the broken local skill link is removed. A focused catalog
check verifies that every advertised skill has a shipped SKILL.md. Archived run
notes retain historical names; current instructions do not offer WorkPlan as a
fallback workflow.

## Wave20 retirement

The root interview at `docs/interviews.md` on harness `4d8e11c` was read before
retirement. The operator graph showed root 1@1 idle with successful provider
completion and no active or queued requests; all model children were retired.
The remaining two local work collectors were idle. `exomonad stop --run-id
4d826f44-f964-40e1-82d2-a74ebf405222 --session wave20` completed with exit 0.
The tmux session and host PID 1198024 were absent afterward. The run archive,
product checkout, dirty/unmerged work elsewhere and shared compiler service were
preserved. The wave20 product checkout remains clean. The final actor graph was
retained at `/tmp/wave20-final-actors.json`.

## Review corrections before release

Independent final review found three live consumer gaps. Generic recursive
examples now use the local owner's `currentCheckout`; the nested recipe adds
real component/subcomponent scaffold commits and checks descendant HEADs.
Manual review and primitive fork examples now attach a question-only collector
and document its drain, leaving settlement with the original request. Their
published cells are included in the executable skill check. Tidepool's own
AgentSpec now matches the hook-free default; its active installed guide no longer
recommends `reviewAgain` or claims failed integration is rolled back. The harness's
obsolete orchestration skill link is removed as well. Final execution results for
these last corrections are pending below; earlier results above name their source.

The final nested-batch recipe used definition `a8aac6fd97c7e968f06f33b97a7ad882c10bacabb9618b9751f3998bca99306f` and passed all seven assertions. The retained success log is `verification/nested-batches-final.log`.

## Checked type import repair

`b9aab12dd1b9cdfb8a3d5f0b27832920637b9e27` is integrated as `158f48d5c`.
GHC now returns the modules required by the types it actually prints. Binder
wrappers consume that evidence in `run_turn_pinned`; expression wrappers consume
it at their owning compiler boundary; the worker's folded-cell path matches.
Existing authored aliases stay in the original imports. Generated templates use
the existing default-declaration insertion point and refuse a missing point.
There is no second Haskell header parser. The internal CellOut producer and
strict decoder changed together; prepared-STG/TPLR formats did not change.

Verification: the focused `cell-splitter-test --pin-imports` case passed; clearing
`checkedPinImports` made the same case fail, and restoring the repair passed.
The explicit `TIDEPOOL_CELL_TEST_EXTRACT` runtime test
`checked_handler_pin_carries_qualified_type_imports` executed one test (225
filtered), including direct pinned compilation and the actual worker fold without
a manual State import. Runtime and actor libraries compile; actor unit tests
compiled with `--no-run`. Logs are `/tmp/tidepool-handler-pin-{red,green}.log` and
`/tmp/tidepool-handler-rust-green.log`. This compiler repair does not attribute
unrelated historical runtime refusals to missing imports.

The legacy routing cancellation check expected the constructor token `CancellationRequested` inside a rendered `RouteFailed` diagnostic. The actual failure explicitly says the request is being cancelled and no reply was sent. The check now keeps route failure and the subsequent typed reply cancellation observation separate; the corrected full routing recipe passed all 35 assertions. The original evidence is `verification/routing-cancellation-diagnostic.log`.

The focused facade documentation selection executed two tests and passed both: `shared_api_guide_example_handles_success_and_unavailable` and `workspace_recipe_modules_and_snapshot_helpers_compile` (540 skipped; 187.774 seconds). The HandoffExamples-inclusive workspace package also compiled and passed child-role effect preflight. These checks launch no native providers. Logs: `verification/documentation-final.log` and `verification/handoff-package-final.log`.

The legacy retained-review fixture remains red: after a valid candidate receipt, no reviewer collector or activation appears; the activation wait times out. `pollExit = Nothing` only excludes terminality, because failed handlers can pause with retained state. The lifecycle snapshot is the next diagnostic, recorded in `plans/next-wave-inputs.md`. This is not a pass and is distinct from the verified fresh exact-source review flow. Evidence: `verification/retained-review-final.log`.

## Final plan review

- Recursive execution has one canonical procedure across the frozen core/API,
  developer roles, shared skills/plans and harness overrides. Local scaffold
  source, narrow child acceptance, combined parent gates and terminal-leaf
  justification agree across those surfaces.
- WorkPlan and its interpreter/check wrappers are removed from live source and
  registration. Context checkpoint primitives remain. Discovery no longer offers
  the removed orchestration skill or a blanket watchdog.
- Batch admission preserves original responses/progress; question routing has
  one notification owner and collector cleanup is distinct from worker retirement.
- Review uses actual source and supplied scope; the checked flow retains counted
  evidence, bounded repair and integration outcomes. Manual examples retain
  pending questions. The older retained-review fixture remains an explicit red
  engine investigation, not a successful review claim.
- Decision answers are bounded selections from original current owner decisions,
  with source guards, conservative escalation and retained send receipts. Five
  live synthetic Jev probes are evidence for those probes only.
- Final pins, catalog checks, matched build and native admission are recorded
  below. Native adoption, actual depth and delivery speed belong
  to wave21's observations, not to scripted recipe results.

Final routing: `Project.RoutingChecks.routing` passed 35 assertions, including cancellation, lost execution, retained progress, independent sources and two-lane Delivery integration. The facade catalog selection passed all seven tests. Formatting with the pinned Rust toolchain passed after formatting the changed catalog and three pre-existing test-layout lines.

The actor catalog selection passed all four tests, including discovery of only existing shipped skills. Final shared workspace pin: `6f548d7b3e1f432af8311a3c75e09e1035c8ada5`, published to its existing main. Template files match that shared source.

## Release and native launch

The implementation and final review are complete. Main integrated the compiler
repairs and recursive surface through `bf29949a3baba57c5c066e7ccb341e0137e371b2`.
`just exomonad-build` passed on main with its matched local worker/frontend.
The exact default-workspace pin test passed (one executed), and the actual
wave21 package compiled with child-role effect preflight. Template and shared
source match pin `6f548d7b3e1f432af8311a3c75e09e1035c8ada5`; catalog is v45.

Wave21 launched from harness `744f09a82cb80c20f7f4a1200fd60e467cd18ded`,
based on accepted wave20 product `4d8e11c`. Session: `wave21`.
Run: `a091048d-3c51-4686-b49e-c7d917807dd5`.
Log: `/home/inanna/dev/exomonad-harness-runs/wave21/.exomonad/logs/a091048d-3c51-4686-b49e-c7d917807dd5.log`.
Root `1@1` is Sol Medium; its provider turn is observed active, and the native
transcript shows successful initial reads of NEXT.md and project guidance.
The brief assigns four Luna component trees and one bounded decision-answer
trial for the standalone raw custom-tool/browser milestone. Initial execution
is confirmed; delegation depth and product acceptance remain wave observations.

Wave20 is stopped; wave19 has no live host. Unrelated sessions, dirty worktrees,
commits and the stale shared daemon were preserved. Wave21 has an isolated
one-worker compiler (7 GiB recycling ceiling). The older retained-reviewer
activation regression remains explicitly red, as recorded above; no release
claim includes it as a passing check.
