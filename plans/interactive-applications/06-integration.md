# Applications wrap-up checklist

The lead owns a matched, reviewed Tidepool/Codex release. The
[source inventory](current-state-review.md) supplies candidates, not acceptance.
A0–A7 are already implemented and A8 passed on the retained pair. The sections
below are revalidation boundaries; open implementation work only for a concrete
failure or uncovered contract. Keep a small evidence table beside the integrated candidate: behavior, owning
check, exact source, result. Do not regenerate the old milestone plan.

## 1. Consolidate once

- [ ] Preserve coordinator `0c1fb83f2`, applications code `cff1ce527` / handoff
  `8eda2640b`, and linear native `d84cda697`. Older tips/WIP are provenance;
  consult them only for a demonstrated missing delta, not as extra stacks to replay.
- [ ] Reconcile the net applications change onto launch main and the native
  change onto its pinned fork revision. A provenance-recorded squash is suitable;
  do not replay an entire historical swarm's bookkeeping commits unnecessarily.
- [ ] Identify what current main already supersedes. Preserve command-job
  admission, scoped process ownership, build/workspace snapshots and frozen
  prompting. Resolve wire/storage compatibility at the actual two consumers.
- [ ] Publish the exact paired baseline and concrete missing checks before
  implementation forks. Run only checks proving changed integration seams.

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
  into the existing deployment owner. Prove retirement releases settled resources
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
- [ ] Return a short migration handoff naming session, completion and execution-
  environment owners plus concrete remaining coupling. No shared server is built.

A blocker needs an exact failing boundary and preserved source, not another broad
planning wave. Scope additions require an actual unmet contract or human decision.
