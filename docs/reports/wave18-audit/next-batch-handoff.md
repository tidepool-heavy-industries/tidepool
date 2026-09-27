# Wave 18: next-batch planning handoff

Gathered 2026-09-27 UTC. The user approved the inter-wave implementation plan after this handoff.
Implementation resumed in isolated worktrees; verification and integration remain
explicit gates. Existing work is preserved. Wave 18 product work and the user's Astra conversation remain live.

## Evidence and priorities

The measured prefix in `jev-cost.md` contains 15,033,124 successful-request input
tokens. The generic after-tool watchdog accounts for 91.3%; no useful intervention
has yet been attributed to it. This is historical attribution, not a measured
future saving. Lookup enrichment is the next large consumer: 1,297,041 tokens.
Failed-request usage is absent from those totals.

Astra's 01:43:55 UTC reply ranks reviewed checkpoints as the largest observed relay
saving, preservation of failed integrations as the immediate safety prerequisite,
and a bounded candidate/check/review continuation as the next experiment.
Standalone actually claimed acceptance from implementer prose while its exact
review was pending, then retracted it. Actual review responses must be authoritative.

Reflection includes recorded active turns at deployed d707d18ed. Earlier claims
otherwise were retracted. Pending unpaired results are a separate limitation.

## Preserved work and validation

| Work | Location / revision | Evidence / outstanding work |
| --- | --- | --- |
| Cost attribution and RSI pruning policy | Tidepool db9a44341 | Fixed-prefix parser reproduced aggregates. |
| Shared bounded non-tool context for Lookup/Sift | rsi-jev/context, 51c818b7b; equivalent 330e049 | Five focused assertions passed; configured definitions compiled. No future-wave savings measurement yet. |
| Remove default blanket watchdog | rsi-wave18/shared-integration, e02e8b3; canonical harness 9f1ace7 | Shared AgentSpec compiled. Shared pin/template not integrated. Live adoption failed; old installed tools remain. |
| Preserve failed integration source | rsi-wave18/preserve-red, 7f31af0, 162aa03, ccc3c5f | Definitions compiled; strengthened recipe includes post-check edits, execution pending. No hidden reset; publication stays separate. |
| Reviewed checkpoints and mutable notification policy | rsi-wave18/reviewed-checkpoints | Definitions compiled; seven admission assertions passed. Routing checks pending fixture corrections. Polls original response, verifies request/source/basis and clean review source; retains one history. |
| Spec-reload regression | rsi-wave18/spec-reload | Minimal test passed 1/1. Faithful source/config test unexecuted after interrupted dependency rebuild; no production fix. |
| Optional generic watchdog packet redesign | rsi-jev/watchdog | Uncommitted. Definitions compiled; two recipe attempts stopped after expensive compilation without completion. Held, not a proposed launch dependency. |

## Reload blocker

The no-watchdog source edit published successfully, but installation failed with
missing imported value `Project.Shell.presentSelected`. The old installed record
remains active; no children switched. Linking lacked a binding or code export.
Whether extraction or compilation/retention caused that absence is unresolved.
The separate prepared-helper draft/active mismatch has not been causally linked.
Run the isolated regression before choosing a production fix; no live retry yet.

## Approved experiments

1. Extend existing ReviewFlow admission/composition: exact candidate -> supplied
   counted checks -> exact review. One shared repair budget; uncertain semantics
   interrupt. No second review owner, automatic acceptance or automatic integration.
   Real replay: recovery candidate 2032950. Before the root's recovery decision,
   interrupt. After that decision, repair only recovery_tests.rs via retained
   owner, rerun the named count-1 test, review repaired tip. Store API changes or
   infrastructure admission failures must not become automatic product repairs.
2. Shadow semantic WorkSink over actual collector state: retain all events,
   propose record/interrupt for relevant free-text arrivals. Test mixed stale
   status plus new questions and missing evidence. Astra's three-item Jev replay
   plus deterministic missing-context case is exploratory, not live savings.
3. Source-aware lookup: investigate enrichment eligibility and compact usable
   import/expression output. Do not remove actual result evidence merely to meet
   a token cap. Compare total calls/tokens and successful use.

## Design and verification questions

- Checkpoint interface should stay short while retaining original settled review
  evidence. Preserve new questions even when raw candidate chatter is silent.
- Notice policy changes affect future notifications; history/cursor and unhandled
  reviewed slices remain inspectable.
- Test preservation of red integration by observing no destructive command,
  including edits made after a check, not just a returned constructor.
- Keep the generic-watchdog draft out of integration. Test a bounded declared
  checkpoint reminder; it does not restore the default after-tool hook.
- Choose a bounded experiment rather than launching every proposed mechanism.
  Include setup/recovery cost, frontier turns, Jev input, and useful outcomes.

## Wave acceptance at the retained audit checkpoint

Repaired producer a7d742e8 has typed Accepted review 69 from actor 57; owner
integration a5409d0d reports seven focused 1/1 checks. Root product acceptance
remains separate. Request 71 owns missing native async tool-definition/call-item
metadata; operator request 7 and integrated browser/staged-binary evidence remain
open at that checkpoint. See `latency-delivery.md` for exact evidence and limits.
This is retained evidence, not a fresh live poll. Keep the root alive.

## Source records

- Tidepool: `docs/reports/wave18-audit/jev-cost.md` and `docs/rsi-loop.md`.
- Live wave checkout: `/home/inanna/dev/exomonad-harness-runs/wave18`.
- Astra notes there: `docs/wave18-rsi-coordination.md`,
  `docs/wave18-rsi-lab-notes.md`, `docs/wave18-notification-experiment.md`,
  `docs/wave18-automation-critique.md`, `docs/wave18-design-followup.md`.
- Run: 29e61b63-2bc9-45d5-b5d1-b67e20918bea; session wave18.

No new timer, retirement, push, pin or launch was performed during this pause.

## Approved implementation batch: implementation checkpoint

- Runtime reload: minimal regression passed (1 test); faithful source/config
  regression prepared. Its first build was interrupted during unexpected
  dependency compilation, before execution. No production fix or live retry.
- Preserve-red: commits 7f31af0, 162aa03, ccc3c5f plus a focused test amendment
  for edits made after the failed check. Definitions compiled; execution pending.
- Reviewed checkpoints: original typed review response, exact basis/candidate,
  clean reviewer submission, mutable future-notification policy, retained history.
  Seven admission assertions passed; routing assertions await fixture correction.
- Checked-review continuation: supplied counted checks before exact review, one
  repair budget. Only proven assertion failures can reach repair; unknown source,
  setup failures and missing/count-mismatched evidence stop at the parent.
- Semantic trials: bounded shadow-only notification proposals and one declared
  production-consumer-checkpoint policy. Deterministic mechanism checks and live
  synthetic judgment replay are separate gates. No automatic suppression.
- Audits: six axes completed. See `turns-helpers.md`,
  `coordination-confusion.md`, `latency-delivery.md`, and `audit-counts.py`.
  Reproduced 62 compiler/workbench diagnostics plus 2 runtime exceptions in 394
  sampled Haskell calls; 458/1608 host calls exceeded ten seconds. Compiler,
  checkout and execution spans are not additive or equivalent to CPU time.

Interface parcels A-F own background examples, runtime inspection/cancellation,
result presentation, lookup examples, and final prompt baseline respectively.
Our changes supply verified APIs and examples for reconciliation; do not duplicate
those owners. One expensive compilation slot across our active local lanes;
external interface checks use their own target and are observed before launching.

No implementation described here has been pinned/deployed by this batch.

## Finding from implementation checks

The preserve-red recipe reached a runtime `RecordConstruction` failure in
`Project.Merge`: its `MergeRequest` omitted `mergeSourceWorktree`. Definition
compilation had accepted that partial record. The repair carries the actual
source worktree from the original response through `Evidence` and `PublishRequest`.
Focused re-execution is pending. This is an existing publication-path defect,
not a failure of the new red-retention branch.

Follow-up worth investigating: surface missing-required-record-field diagnostics
at the authored-source boundary, before runtime. Do not silently impose a global
Haskell warning-as-error policy without checking intentional partial records and
which diagnostics the existing compiler owner already retains.

## Completion order and next-wave boundary

The user requests completion of this improvement set, then a discussion of the
current harness milestone and next product work before launching a Sol-root wave.
No successor launch is authorized merely by finishing these checks.

Interface A-F are reviewed and applied in dependency order to
`rsi/w18-interface-integration` at `7ab3f645b`; the original reviewed hashes,
exclusive ranges and test evidence remain in
`/home/inanna/dev/exomonad-interface/HANDOFF.md`. This is source integration,
not combined verification or deployment. The Codex test-only companion remains
separate. The integration draft corrects stale automatic rollback guidance in
`exomonad/prompts/docs/tree.md`.

Preserve-red final focused execution passed all seven assertions, including
source provenance and edits retained after a failed check; branch tip
`bf2881783ef0d83be309683a0fc040c5360aadfb`, integrated into shared-source batch
branch at `16b6f51`. This supersedes its earlier pending status above.
Reviewed-checkpoint final rerun is in progress; checked-review behavioral tests,
semantic trials and faithful reload regression remain pending.

## Superseding run status: explicit wind-down

Inanna ended wave18 incomplete; it is now stopped and release verified. See
[wind-down, final interviews and actions](wind-down.md). Harness run WIP/handoff
is committed at 9e3f1be. Do not wait for the stopped wave to close its product gate:
the remaining Engine/Store process-loss recovery failure needs explicit ownership
in the next work plan. Four final owner/worker interviews and the root's existing
critique are retained. The new helper design/live-trial evidence is in
[jev-pattern-trials.md](jev-pattern-trials.md).
