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
| High | Watcher summary could not read evidence, while root later could | Retain JSON in the original job before exit; collect it without a second actor opening the originating checkout. Root was WritableBound, not a managed checkout, so binding the watcher to an unrelated worktree would not fix this case. Prove both root and managed-child collection without rerunning tests. |
| High | Raw-output recovery returned HTTP 409 | Exact 10:29:38 trace reports retained command output expired. Preserve terminal facts and capture evidence before expiry; unavailable retained output remains explicit and must not cause an automatic test rerun. |
| High | Review source omitted required sibling Engine change | Declare required source dependencies and validate composition before product review. Keep partial component review legitimate. Prove stale composition is identified without mislabeling a provider defect. |
| Medium | Typed Repair contradicted no-defect prose | Prefer a verdict shape requiring an actual finding; inspect existing API before adding checks. Semantic prose contradictions may need a bounded judgment. Measure root clarification rounds rather than hiding reviewer work. |
| Medium | Initial contract named ask although provider advertised sleep | Include a small executable consumer example with advertised names, decision/evidence shape and capture seam before delegation. Keep production acceptance independent. |
| Medium | Review attempts exited 137 or lost command handles | Trace resource and job lifecycle evidence. Exit 137 alone does not prove OOM. Carry exact focused filters, asset prerequisites and realistic memory in review plans; avoid redundant builds. |
| Medium | Helpers advertised but little observed reuse | Reconcile prompt exposure and actual opportunities; interview root on non-use. Trial a useful component-specific composition in a child, preserving three exposed waves before judging usefulness. |

## Interview and next-wave design

Root and reviewer interviews, including the focused follow-up, are retained and
read. Root reports no still-running observation changed a product decision. It
describes polling as manual completion management, and the watcher failure as a
reason to distrust compact acceptance summaries. Several unused helpers did not
fit the task; their absence is not proof of discovery failure. Root recommends
mechanical source/evidence collection and explicit invocation contracts.

## Delivery batch and acceptance

The API-quality interview strengthens the Haskell-first contract: longer shell
waits alone leave evidence/source/review work with the model. Extend the existing
authored command and review entrypoints, with the direct Bash tool as an adapter.
Ship working compositions for start -> completion -> evidence, preparation ->
readiness -> check, and candidate -> source preflight -> review. Each encodes a
small desired workflow while retaining typed unresolved outcomes and callbacks.
Avoid making agents discover and assemble ten independent utilities before the
first useful action. The Engine worker did not recall noticing the specific API
names; the root knew the menu but lacked a complete working composition. These
are distinct findings, not evidence that the unused abstractions lack utility.

Use isolated worktrees with three parallel implementation lanes after fixing the
shared observation/report contract. No new process owner or general agent loop.

1. **Command continuation:** own the Bash default, cancellable waiting, automatic
   handoff of the same job and one completion notice. Ordinary calls expose
   completion intent rather than requiring a yield interval. Keep explicit
   observation for interactive callers. Start with a 60-second foreground budget;
   expiry hands off without killing the process. Existing explicit background
   remains immediate. Treat existing explicit yield requests as observation mode
   during migration; remove them from normal examples. Prove completion/timeout
   races cannot lose or duplicate delivery and siblings still make progress.
2. **Evidence and contextual follow-up:** fix worktree authority and trace HTTP
   409 before adding recovery policy. Extend the existing focused evidence owner
   and shell presenter. Exact results remain code-derived. On ambiguous terminal
   failure or insufficient relevant output, Jev may select at most two bounded
   read-only follow-ups from supplied candidates, using command intent and evidence.
   Default to no extra inference for complete small results. Unknown judgments or
   service failure return the original facts. No generated arbitrary commands.
3. **Review handoffs:** review and integrate the existing `3501f1f` ReviewFlow
   implementation rather than creating another coordinator. Add declared required
   sibling commits to its deterministic source preflight. Reject empty repair
   findings at the owning review boundary and return correction to the reviewer.
   Default to at most two implementation repair rounds; escalate scope changes,
   missing dependencies or exhausted budget. Notify root on accepted candidate or
   escalation; root retains integration authority and independent review remains.

Integration owner reviews the three lanes and updates shipped tool descriptions,
compiled examples and project prompting against the resulting source. Introduce
one component-specific check function composed from the repaired evidence API;
give its actual name and arguments to a child with a real repeated consumer.
Retain unused helpers for their agreed exposure window; do not enlarge the menu.
Include concrete provider/schema examples and prerequisite checks in the next
shared contract, and reconcile the automation journal's exposure/use statuses.

Verification uses focused owner checks and one combined real-consumer gate. Cover
completion racing handoff, cancellation, sibling progress, bounded output and
Jev failure; managed-worktree evidence access and explicit access refusal;
stale review source, contradictory/empty repair, accepted exact candidate and
repair exhaustion. Reuse immutable fixtures; do not run a full battery per lane.
Browser-helper publication stays held until its owned-worktree consumer executes.

Build the full-wave baseline before comparing savings: deduplicate call IDs and
inherited history, count command starts, observation rounds, diagnostic rereads,
review relays and outcomes per opportunity. Aim to remove at least half the
routine command-observation rounds in comparable episodes; this is an experiment
target, not a promised whole-wave reduction. Report whole-wave frontier usage,
Jev overhead, latency and correctness separately. Replay demonstrates potential;
the next wave demonstrates adoption and realized savings. Give the review flow
one real bounded component trial and keep integration review explicit.

Do not expand the product scope merely to exercise every helper. The next product
slice should offer real command/evidence opportunities; test the Bash workflow
there and keep other changes distinguishable. No successor launch or repeated
timer is authorized by the one-shot supervision check itself. The user has now
authorized stopping the old wave12 run/demo; perform that as separate scoped
cleanup after confirming its identity and retained interviews. Preserve demo
data, unrelated Tailscale routes, shared daemons, dirty checkouts and all commits.


## Integration observation

The focused evidence recipe's compiler reached 7,488 MiB after eight requests
and rotated at the scoped 7,168 MiB ceiling (11:38:20Z; compiler log under
`/tmp/tidepool-dev-shell.Xhdqr3/nix-shell.6AEevV/tidepool-extract-daemon.pX2hRM/`).
A preceding 2 GiB scoped gate rotated after almost every request. The latter was
rerun with the existing run-sized one-worker budget; shared daemons were untouched.
The larger observed residency is a measurement card for the next performance
audit, not justification for raising every run's memory budget. Separate broad
import/fixture compilation from ordinary notebook-cell cost before changing policy.
