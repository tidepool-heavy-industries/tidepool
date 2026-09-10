# Applications wrap-up checklist

The lead owns a matched, reviewed Tidepool/Codex release. The
[source inventory](current-state-review.md) supplies candidates, not acceptance.
A0–A7 are already implemented and A8 passed on the retained pair. The sections
below are revalidation boundaries. Repair failures and simplify unnecessarily
complicated implementation while preserving required behavior. Keep a small
evidence table beside the integrated candidate: behavior, owning check, exact
source, result. Do not regenerate the old milestone plan.

## 1. Consolidate once

- [ ] Preserve coordinator `0c1fb83f2`, applications code `cff1ce527` / handoff
  `8eda2640b`, and linear native `d84cda697`. Older tips/WIP are provenance;
  consult them only for a demonstrated missing delta, not as extra stacks to replay.
- [ ] Select the useful applications behavior for launch main and its pinned
  native revision. Reuse, adapt or omit candidate code by the rules below. A
  provenance-recorded squash is suitable; historical patches are not obligations.
- [ ] Identify what current main already supersedes. Preserve command-job
  admission, scoped process ownership, build/workspace snapshots and frozen
  prompting. Resolve wire/storage compatibility at the actual two consumers.
- [ ] Publish the exact paired baseline and concrete missing checks before
  implementation forks. Run only checks proving changed integration seams.

### Select behavior, not a patch count

The operator authorizes selective integration and simplification. Retained refs
preserve the work; there is no requirement to merge every commit or keep every
candidate abstraction. Inspect the net production change and its real consumers.

| Finding | Integration choice |
|---|---|
| Main already satisfies the behavior | Keep main; bring only missing useful checks |
| Candidate supplies a clean missing behavior | Reuse it with its owning tests |
| Candidate behavior is useful but its owner conflicts with main | Adapt it into the existing owner; remove the redundant path |
| Candidate scaffolding has no production consumer or repeats policy | Omit it; do not keep an adapter just to preserve the patch |
| Simplification would remove an actual user-visible guarantee | Surface the concrete tradeoff to the human before dropping it |

Ordinary refactoring within these boundaries needs no separate approval. Keep a
brief integration note beside the evidence: consequential omission/adaptation,
original ref, reason, and the check proving retained behavior. No per-commit ledger
or reporting for trivial edits. Preserve exact native input, completion and custody
semantics even if the implementation is substantially smaller.

Treat runtime/engine dependencies the same way. Name the actual interface needed
by applications; do not pull the whole prepared engine through a convenient merge.
Prefer the current runtime owner when it can satisfy the contract directly. A
substantial new compatibility layer is a design finding, not an automatic task.

Required recovery targets include the registry-state/race and source-only replay
cases, `abnormal_root_reuses_only_a_queue_ready_conversation`, and
`forest_operator_survives_model_root_recovery`; obtain exact owning commands from
the retained handoff/source. Compile `tidepool-runtime`, `tidepool-actor` and
`tidepool` on the reconciled source.

## 2. Revalidate input and binding

- [ ] Verify the integrated real submit/query/withdraw/seal/ack path using one input identity
  and one executing native owner. Check generation changes and stale producers.
- [ ] Exercise lost acknowledgments, consumed inputs, compaction, conflicting
  payloads and uncertain dispatch. Missing evidence must not authorize redispatch.
- [ ] Verify ordinary human steering and approvals remain usable through the
  normal TUI. No fallback may silently start another executor.

Contract: [binding](01-native-session.md), [delivery](02-delivery.md).

## 3. Revalidate completion and retirement

- [ ] Preserve the integrated native completion implementation; reuse the canonical
  persisted-result boundary and existing host fork gate.
- [ ] Test delayed/lost/duplicate completion, native persistence failure, and
  cancellation with accepted work. A callback acknowledgment must not wait for
  a child's expensive compilation; failure must not repeat Haskell execution.
- [ ] Prove exact-context forks contain the completed result and selected
  source/tool/prompt bytes. Check the real provider request boundary where needed.
- [ ] Verify producer sealing, hosted-work drain and exact process/job cleanup
  through the existing deployment owner. Prove retirement releases settled resources
  and retains uncertain custody, including interrupted launch and lost waiters.
- [ ] Verify hosted-coordination failure leaves ordinary native interaction usable.

Contracts: [completion](04-hosted-completion.md), [custody](03-process-supervision.md).

## 4. Revalidate recovery at the supported boundary

- [ ] Reconcile retained reconnection and source-recovery work against the actual
  current runtime contract; fence old producers and old incarnations.
- [ ] Test native loss, host loss, reconnect and deliberate new execution from
  history. Report lost live state explicitly; do not reconstruct runtime handles.
- [ ] Keep the engine join narrow: verify the source/recovery contract against the
  engine candidate separately. Final-engine revalidation gates engine integration;
  an independently separable applications pair can ship after its own checks.
  Unrelated engine milestones do not gate that applications-only release.

Contract: [recovery](05-recovery.md). Recovery from Git remains the operational
fallback after Shoal loss; do not expand this wave into transparent live recovery.

## 5. Ship and stop

- [ ] Execute the combined failure paths above with actual native/Shoal binaries
  and local scripted providers. Extend existing full-TUI/resource fixtures and
  owning tests. Compile changed test targets; no full workspace suites.
- [ ] Record source, native pin, package selection, executed checks, compile-only
  evidence and any unresolved defects. Unverified behavior is not accepted.
- [ ] Review the integrated diff for duplicate owners, superseded paths, leaks,
  stale prompts and unnecessary new APIs. Remove obsolete implementation paths.
- [ ] Integrate the accepted pair through the existing main/release workflow;
  do not hot-replace a running swarm's executables or canonical package.
- [ ] After acceptance, move enduring contracts to their owning source/guidance
  and remove superseded temporary plans and launch maps. Preserve requirements
  while integration is still underway; Git remains the archive.
- [ ] Return a short migration handoff naming session, completion and execution-
  environment owners plus concrete remaining coupling. No shared server is built.

A blocker needs an exact failing boundary and preserved source, not another broad
planning wave. Scope additions require an actual unmet contract or human decision.
