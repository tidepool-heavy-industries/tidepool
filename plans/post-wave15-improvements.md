# Post-wave15 improvements

## Evidence and acceptance

Run `2518edd8-8a6c-439c-b823-5055b96096d9`, session `wave15`.
Launch source `ecd96f35d`, workspace `d07eefb8`. Product source
`40bd399e274c95d56374cb36efe09f66eb74b43a`; handoff `4660fdc`.
Supervisor read the native root outputs confirming nine focused checks each
selected/executed/passed 1/1, web 14/14, and browser journey 1/1. These were
existing results, not supervisor reruns. Test source was dirty with helper
and documentation edits; do not describe the checks as clean-tree acceptance.

Primary records: harness run checkout `docs/wave15-handoff.md`,
`docs/interviews.md`, `docs/exomonad-friction.md`, and
`docs/automation-trials.json`; supervisor launch record lives in the harness
main checkout at `docs/wave15-launch.md`, not the run admission snapshot.
Root native thread: `01a0dd1d-1a36-7f83-a175-d84fcd5dacd6`.
Luna audit cutoffs preceded final gates; later root outputs supersede their
pending verdicts. The automation journal also loses earlier menu-exposure
facts: a null first-exposed field is not evidence the prompt omitted a helper.
Reconcile exposure, opportunity and actual use before scoring adoption.

Preserve: useful parallel Engine, provider/Store and independent acceptance
work; exact-source independent review; expected-red acceptance; component and
production checks. Review caught genuine policy bugs. Repair rounds that found
those bugs are useful work, not automatically overhead to eliminate.

## Main experiment: Bash owns routine continuation

Target the graph segment command -> observe -> wait/read -> report. One tool
invocation should start once, await useful evidence, collect bounded diagnostics,
and return a result or a genuine unresolved decision. Haskell composes the
workflow; Rust retains process, resource, cancellation and output ownership.
Jev selects contextual follow-ups where a deterministic rule is insufficient.

Concrete baseline: root job `2a61439d-0090-4198-903a-5d796b24e8e2`
started at 10:05:13 UTC, followed by five one-second observation calls through
10:05:35. The first three returned only running; the fourth partial results;
the fifth exit zero with repeated output. No stdin or intervening decision.
Earlier bounded audit counted 24 empty-input waits across five model actors,
including 16 root waits across four jobs; this is not the whole-wave total.

Implementation slices:

1. Completion-oriented default: await internally and preserve cancellation.
   Separate foreground attention budget from process execution deadline.
   At handoff, retain the original job and arrange one completion notification.
   Preserve deliberate interactive observation as an explicit operation.
2. Evidence collection: gather exit, cleanup, relevant retained output and known
   test artifacts before returning. Preserve source and missing-evidence states.
   Avoid repeatedly presenting unchanged output.
3. Bounded Jev follow-up: select among provided observations, diagnostic sections,
   artifact reads or authored diagnostic actions. Use command intent and context.
   Begin with one or two judgments, bounded work, and an explicit unresolved
   fallback. Do not silently rerun, change test scope, write stdin or repair code.
4. Update tool descriptions, receipts and compiled examples together; remove
   ordinary long-check examples that encourage one-second model polling.

Reuse `Tidepool.Command.Tools`, `Project.Shell`, the existing command owner and
settlement machinery. The presenter currently ignores command/intent; extend the
workflow boundary deliberately rather than adding a second process supervisor.
Check that pending tool calls do not block sibling progress or cancellation.

Evaluate ordinary success, failure with noisy output, queued execution, long
running work, input required, cancellation, unavailable/expired evidence, and
Jev unavailable. Replay captured decisions where possible and then run a live
consumer trial. Count frontier rounds, repeated bytes, result completeness,
latency and added judgments. A few rounds saved on a fraction of opportunities
is enough to be useful; no requirement for universal automation.

## Complementary fixes and investigations

| Priority | Finding | Next action and proof |
|---|---|---|
| High | Watcher summary could not read evidence, while root later could | Trace exact file/access failure before choosing retry. Browser-helper investigation found unbound actors accessing a managed checkout without its owned handle. Reproduce check collection under correct worktree authority; prove success and refusal, without rerunning original tests. |
| High | Raw-output recovery returned HTTP 409 | Trace owner/retention lifecycle. Retention expiry is the run's interpretation, not an independently established cause. Preserve terminal facts and durable artifact recovery. |
| High | Review source omitted required sibling Engine change | Declare required source dependencies and validate composition before product review. Keep partial component review legitimate. Prove stale composition is identified without mislabeling a provider defect. |
| Medium | Typed Repair contradicted no-defect prose | Prefer a verdict shape requiring an actual finding; inspect existing API before adding checks. Semantic prose contradictions may need a bounded judgment. Measure root clarification rounds rather than hiding reviewer work. |
| Medium | Initial contract named ask although provider advertised sleep | Include a small executable consumer example with advertised names, decision/evidence shape and capture seam before delegation. Keep production acceptance independent. |
| Medium | Review attempts exited 137 or lost command handles | Trace resource and job lifecycle evidence. Exit 137 alone does not prove OOM. Carry exact focused filters, asset prerequisites and realistic memory in review plans; avoid redundant builds. |
| Medium | Helpers advertised but little observed reuse | Reconcile prompt exposure and actual opportunities; interview root on non-use. Trial a useful component-specific composition in a child, preserving three exposed waves before judging usefulness. |

## Interview and next-wave design

Root and reviewer interviews are retained. A focused root follow-up asks why it
chose short polls, which observations changed decisions, why helpers were skipped,
which follow-ups it would delegate, and which missing contract/source facts drove
churn. Read that answer before finalizing the batch or retiring root.

Do not expand the product scope merely to exercise every helper. The next product
slice should offer real command/evidence opportunities; test the Bash workflow
there and keep other changes distinguishable. No successor launch or repeated
timer is authorized by the one-shot supervision check itself. Preserve wave12
demo, Tailscale, shared daemons, dirty checkouts and all commits.
