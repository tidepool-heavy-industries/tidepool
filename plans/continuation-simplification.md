# Compiler repair and coordinator simplification

Approved 2026-09-28. Implementation follows bounded continuations; the current
run remains frozen. Finish the matched build and handoff, then pause: no successor
wave is authorized by this batch.

## Ownership and sequence

- Compiler worktree: `rsi-continuations/site-sibling-cache`; repair the negative
  control before accepting the cache hypothesis. Restore module-owned generated
  siblings consistently on validated memo reuse. Root integrates and runs the
  required corpus gate. No cache disabling or shared daemon restart.
- Runtime worktree: `rsi-continuations/dynamic-sources`; extend the existing
  actor source owner with typed actor-local attachment after startup. Root and
  implementer freeze its interface before changing authored consumers.
- Workspace worktree: `rsi-continuations/coordination`; consolidate dependency
  and accepted-answer routing into one asynchronous policy owner. Root owns
  review/investigation simplification, prompts, live semantic probes and pins.
- One expensive compiler slot. Exact candidates, checks and limitations belong
  in `docs/reports/bounded-continuations/handoff.md`; drafts are not releases.

## Runtime and Haskell contracts

Dynamic attachment targets a declared Event through Self, never an externally
forgeable event endpoint. Reuse command, progress, settlement and lifecycle
sources, authorization, retained-current delivery and source ordering. Keep
refusal explicit. Connections belong to the receiving actor, survive replacement
without recapture, and release on retirement. Attaching does not transfer the
observed resource's ownership or roll back external effects after handler failure.

Delete diagnostic completion followers, follower observers and the review
investigation guard once direct attachment covers them. Keep actors that own real
workflow state. Convert other result forwarders only where the same primitive
directly replaces them, preserving exact response evidence.

Luna owns independent code review. Remove Jev's second judgment over reviewer
findings. Verified failed checks retain their original jobs and bounded diagnostic
reports; the original implementer receives repair under the unchanged assignment.
Missing evidence, source mismatch, uncertain execution, scope changes and pending
cleanup remain explicit handbacks. Repairs share the existing budget; new source
must pass checks and independent review. Integration remains owner-controlled.

## Coordination policy

Deterministic declared producer/consumer delivery is the fallback. Jev operates
on optional exact-candidate publication notes, declared interests, open questions
and existing owner decisions, not code diffs. Publication notes are written during
ordinary progress, without a separate model round.

Batch semantic correspondence and optional-notification triage where they share
evidence. Select existing material and recipients, then immediate notification or
retained digest. Do not invent an answer, grant authority, resolve a question or
infer incorporation. Preserve required questions, failures, final results and
delivery refusals. Missing notes, uncertainty, unavailable Jev and exhausted
budgets fall back to ordinary delivery. Flush digests with the next required
notice, explicit inspection/flush, source completion or batch finish. No polling
watchdog or new timer. Retain original publications, judgment provenance and send
receipts through the existing collector and coordination state.

## Acceptance

Prove compiler cold/warm/invalidation behavior and the observed cross-recipe
failure. Test late source delivery, attachment races/refusals, replacement, failed
handlers and cleanup. Execute composed check/investigation/repair/review recipes.
Exercise stale/conflicting correspondence, duplicates, digests, overflow and
refused delivery, including required events while Jev is busy. Root inspects
bounded live packets and replays actual responses through production decoders.

Update canonical prompts, skills and executable examples; delete superseded
guidance. Publish matched workspace/harness revisions, synchronize the template,
run focused pin/catalog checks and `just exomonad-build`. Record structural
deletions separately from measured savings. Preserve the three-wave evaluation
period. Pause before launch.
