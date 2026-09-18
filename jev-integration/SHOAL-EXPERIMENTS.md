# Synthetic Shoal judgments

These fixtures test the six applications from the Jev-in-Shoal example set
(uncompiled mockups in the abandoned DSL; superseded by the measurements in
this directory, which is why they are recorded here rather than linked).
They are API calls, not a running Haskell effect, actor, or graph traversal.
All state and source excerpts are synthetic. No selected action is executed.
Each case runs once, with no automatic retries. Expectations below were recorded
before the live calls. Probabilities are observations, not calibrated thresholds.

| Probe | Expected behavior |
| --- | --- |
| shoal-route | `recipient = actor_a`: reviewer of retained offsets |
| shoal-route-no-match | `recipient = unassigned`: PDF typography is not tax logic |
| shoal-attention-urgent | Both Nouls favor yes; delay rubric concentrated toward incorrect approval (index 3) |
| shoal-attention-routine | Both Nouls favor no; delay rubric concentrated toward background information (index 0) |
| shoal-expand | `step = edge_7`: inspect publication, not cancellation telemetry |
| shoal-fold-gap | `conclusion = gap`: publication implementation missing |
| shoal-evidence | `evidence = group_2`: original type mismatch |
| shoal-evidence-missing | `evidence = no_match`: do not substitute downstream symptoms |
| shoal-question-duplicate | `relationship = duplicate` |
| shoal-question-distinct | `relationship = distinct`: HTTP retries versus actor repair |
| shoal-repair | `next = repair`: reproducible contract violation |
| shoal-repair-unavailable | `next = owner`: select replacement through coordinator |
| shoal-experiment | `experiment = trace_invalidation`: differing predictions within budget |
| shoal-experiment-insufficient | `experiment = none`: remaining plans have identical predictions |

The attention fixtures explicitly supply a recipient; they are not a claim that
same-request questions can consume each other's answers. The fold fixture supplies
child evidence directly. Routing-to-attention and expansion-to-fold composition
remain later experiments. The experiment-selection fixtures supply predictions;
they do not test Jev's ability to invent accurate experimental predictions.

Reproduce using `show <probe>` or `run <probe> --output <new-private-file>` with the
existing CLI. New probes are defined in `src/scenarios.rs`, included in the harness
fingerprint. Raw evidence remains under ignored `evidence/` with mode 0600.

## Observations — 2026-09-16

All 14 requests returned HTTP 200, resolved `jev-latest` to `jev-1.13.0`, and
passed the existing response interpretation checks. All matched the qualitative
expectations above on their single run. There were 12 Choice answers plus two
attention records, each containing two Nouls and a Score.

| Probe suffix | Selected answer | Selected option probability |
| --- | --- | --- |
| route | actor_a | 0.96 |
| route-no-match | unassigned | 0.96 |
| expand | edge_7 | 0.91 |
| fold-gap | gap | 0.99 |
| evidence | group_2 | 0.88 |
| evidence-missing | no_match | 1.00 |
| question-duplicate | duplicate | 0.87 |
| question-distinct | distinct | 1.00 |
| repair | repair | 0.93 |
| repair-unavailable | owner | 1.00 |
| experiment | trace_invalidation | 0.97 |
| experiment-insufficient | none | 0.93 |

| Attention observation | Urgent | Routine |
| --- | --- | --- |
| P(changes next action) | 0.90 | 0.09 |
| P(contradicts assumption) | 0.95 | 0.04 |
| Reported delay Score, indices 0–3 | 2.98 | 0.07 |
| Probability at rubric index 0 | 0.00 | 0.94 |
| Probability at rubric index 1 | 0.00 | 0.05 |
| Probability at rubric index 2 | 0.01 | 0.01 |
| Probability at rubric index 3 | 0.99 | 0.00 |

Measured request latency was 164–380 ms, median 218 ms. Total reported usage:
7,784 input tokens and 686 output tokens. This is a sequential sample of short
requests, not a load test. No cost claim is inferred from token counts alone.
Evidence filenames are `evidence/<probe>-001.json`; all fourteen have mode 0600.
Harness fingerprint:
`0217261c1c188d91019b465df25b27fa53bc4359474e5012314364e78d53138e`.

### Implications and limitations

- Explicit no-match and insufficient-evidence alternatives worked in these cases.
  Removing the direct diagnostic moved selection to no-match; removing the
  discriminating experiment moved selection to none.
- Structured responsibility descriptions distinguished the affected reviewer from
  the index benchmarker and distinguished tax logic from invoice typography.
- Attention dimensions remain separate: the urgent Noul was 0.90 while the Score
  assigned 0.99 to incorrect approval. Neither value should replace the other.
- The duplicate-question answer retained 0.12 for distinct, and evidence selection
  retained 0.12 for no-match. Keep distributions accessible to application policy.
- Expansion put 0.08 on finish. The candidate description already exposes the
  cancellation gate's source, so finishing is arguably defensible. A harder graph
  experiment should distinguish an edge preview from a verified source witness.
- Fixtures are deliberately clear, with candidate coverage and some missing data
  explicitly labeled. Repair unavailability is enforced by removing the candidate;
  this does not test whether Jev would reject an ineligible candidate left in the
  list. Experimental predictions are supplied. These results establish basic
  behavior, not realistic accuracy, calibration, or robustness to noisy evidence.
- No actual actor notification, diagnostic execution, Haskell DSL execution, or
  dependent multi-step traversal ran. Those require composition tests after this
  per-judgment baseline.

Verification: package build passed; all 12 existing unit tests passed, including
the expanded all-probes JSON roundtrip test; package clippy with warnings denied
passed; package formatting and `git diff --check` passed. No workspace battery ran.
The TypeSafe skill informed narrow questions, structured descriptions, and explicit
no-match controls; the API client and one-operation DSL direction are unchanged.
