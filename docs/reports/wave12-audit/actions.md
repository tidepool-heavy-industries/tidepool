# Wave 12 audit action queue

These actions are authorized by the user. Audit reports describe an observed
prefix of the run, not a final product verdict. Verify evidence before promoting
a recommendation into a standing rule.

## Implementation and validation

| Work | Owner / status | Acceptance evidence |
|---|---|---|
| Responsive compiler control under one busy worker | cf6a5fa8f, integration review | Focused tests passed; review whether busy rejection can trigger fallback workers before merge |
| Reuse recovered reachability in projection | f4252f72d, awaiting integration review and corpus gate | Focused equivalence test and extractor build passed; full fixtures gate outstanding; speedup unmeasured |
| Cancel fork imports without fallback launch | cancellation branch through 5e61feecc, awaiting review | Build, pre-cancel, in-flight copy kill/reap and captured-source cleanup tests passed |
| Failed admission releases provisional resources | 5e61feecc, awaiting review | Concurrent tmux insertion tested; provisional Git receipts deliberately retained by owner contract |
| Managed root baseline, private overlays and explicit integration | queued for next implementation slot | Approved managed-root plan; policy/importer exist, production composition and acceptance remain |
| Clear producer contracts and completion evidence | harness a6a39c3, isolated prompt branch | Clarification, expected-red prerequisites, retained checkout and pending-update guidance corrected; diff checked |
| Reconcile audit evidence and action ownership | root, active | Correct actor identities and resolve conflicting helper-import claims before treating reports as settled |
| Jev shared-service breaker | main 136790a14 | 14 focused tests passed; facade compiled; no running host changed |
| Stale notice filtering and accurate queue logs | compiler_control, active | Same watch-observation checks for both delivery paths; presentation remains separate |
| Overlap independent resource-release waits | review_finish, active | Preserve descendant retirement order; separate within-group fix from multi-group experiment |

## Investigation and experiments

- **Stale compile snapshots:** audit_abstractions investigates locked fallback,
  bounded retries and source sequencing. Preserve compile-view validation;
  discuss fairness, consistency and cancellation before changing the strategy.
- **Coordination helpers:** queued. Try small notebook compositions using the
  existing typed response/review surfaces, including a compact settlement
  collector. Compare actual calls, output and useful consumers; discard trials
  that do not repay their complexity. Multiple trials are welcome.
- **Implementation clarification:** the node messages its parent with the
  concrete question. Continue independent work when available. Do not treat a
  legitimate question as a failure or require a complete contract before every
  fork. Parent decisions and source changes still need explicit incorporation.
- **Repeated unavailable Jev judgments:** breaker implemented at the existing
  shared JevClient owner. Account failures pause provider calls with one recovery
  probe; callers receive explicit failures, never invented judgments.
- **Command handle failure:** traced to a mistyped ID, not registry loss. Audit
  handle reuse/transcription ergonomics before proposing lifecycle changes.
- **Broader Luna audits:** repeated compilation dispatched; context cost and
  artifact amplification queued for worker slots. See [experiments](experiments.md)
  for bounded trials and their acceptance evidence.

## Dispatch policy

Use isolated worktrees for implementation. Reuse agent slots as tasks finish.
Keep live hosts and their compiler daemons untouched. Run focused checks during
parallel work and the necessary broader gate at integration boundaries.
