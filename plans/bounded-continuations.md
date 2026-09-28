# Bounded continuations: dependency, investigation and review

Approved 2026-09-28. Implementation in isolated worktrees under
`/home/inanna/dev/rsi-continuations`; wave22 keeps its frozen runtime and source.

## Decisions

Carry an operation through its mundane steps and return control at a meaningful
decision boundary. Existing Haskell actors own state and continuations; Jev
answers bounded semantic questions. Do not add an orchestration engine.

- Execute declared diagnostics and bounded repair requests without another lead
  model round. Integration and new scope decisions remain with the lead.
- Declare local producer/consumer dependencies at admission. Route relevant
  checkpoint publications directly; preserve required replies, questions and
  failures. Do not discover undeclared relationships or infer incorporation.
- Use all three procedures in all suitable component trees next wave. Evaluate
  opportunities and outcomes over three waves, repairing harmful behavior at once.

## Work and ownership

1. Dependency routing: one non-model actor per batch, fed asynchronously by the
   existing collector. Preserve original checkpoints, distinguish candidate from
   reviewed source, deduplicate episodes, retain judgments and send receipts,
   expose failed delivery and actor lifecycle, drain before finish.
2. Investigation: retain the original job, recover structured verbatim evidence
   and explicit omissions, execute at most two supplied diagnostics, return a
   typed report to a model or actor continuation. Never rerun the original job.
   The focused runner must retain source and phase evidence before compilation;
   compilation failure must not become passing or executed-test evidence.
3. Review: use the report for bounded same-implementer repair, preserve exact
   source and independent review, share the existing two-repair budget, and return
   reviewed delivery or a specific owner decision. No automatic integration in
   the default procedure.
4. Prompts/examples: compiled admission-to-review procedure; task/check/dependency
   intent supplied once. Replace superseded guidance rather than adding a second
   workflow. Preserve full evidence for inspection and deliberate cleanup.

Two Sol Medium worktrees implement dependency routing and investigation plus its
harness-runner companion. Root owns interface review, ReviewFlow integration,
prompts, final review, one compiler slot and live semantic probes. No shared
daemon restart, live-run source edit or destructive worktree cleanup.

## Verification and evaluation

Exercise exact examples in resident recipes, including duplicates, mixed updates,
refused delivery, pending cleanup, compile/assertion failure, missing evidence,
source drift, diagnostic limits, repair limits and stale candidates. Run focused
runner tests, compile affected consumers, check prompt/catalog contracts and
formatting. Inspect bounded live Jev requests personally and replay their actual
responses through the typed interpreter; offline actor checks do not establish
live semantic quality.

Publish the reviewed workspace pin, sync template and rebuild the matched runtime
before next-wave exposure. Record revisions and actual verification separately
from drafts. Measure parent relays, diagnostic followups, missed obligations,
wrong routes, Jev usage and cleanup. Estimated saved rounds are not measurements.
