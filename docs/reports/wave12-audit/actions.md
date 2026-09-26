# Wave 12 audit action queue

These actions are authorized by the user. Audit reports describe an observed
prefix of the run, not a final product verdict. Verify evidence before promoting
a recommendation into a standing rule.

## Implementation and validation

| Work | Owner / status | Acceptance evidence |
|---|---|---|
| Responsive compiler control under one busy worker | compiler_control, implementing | Barrier tests for control response, bounded admission, acknowledgment and shutdown |
| Reuse recovered reachability in projection | f4252f72d, awaiting integration review and corpus gate | Focused equivalence test and extractor build passed; full fixtures gate outstanding; speedup unmeasured |
| Cancel fork imports without fallback launch | review_finish, implemented; extending cleanup | Build and pre-cancel test passed; in-flight copy kill/reap and partial-launch cleanup still required |
| Failed admission releases provisional resources | review_finish, active | Deterministic collision/failure cases and no orphaned resources |
| Managed root baseline, private overlays and explicit integration | queued for next implementation slot | Approved managed-root plan; policy/importer exist, production composition and acceptance remain |
| Clear producer contracts and completion evidence | root, queued | Future assignments distinguish component checks from integrated behavior, expected-red tests and actual acceptance |
| Reconcile audit evidence and action ownership | root, active | Correct actor identities and resolve conflicting helper-import claims before treating reports as settled |

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
- **Repeated unavailable Jev judgments:** investigate the issuer and structured
  error classification before choosing suppression or retry policy. The report
  observed repeated HTTP 402 responses; do not silently disable authored calls.

## Dispatch policy

Use isolated worktrees for implementation. Reuse agent slots as tasks finish.
Keep live hosts and their compiler daemons untouched. Run focused checks during
parallel work and the necessary broader gate at integration boundaries.
