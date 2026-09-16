# Provider contract evidence

Baseline read 2026-09-16. No authenticated evaluation observations yet.

## Provenance

- Public schema: <https://api.typesafe.ai/openapi.json>, title TypeSafe, version
  `0.2.0`. Stored as formatted JSON in `fixtures/openapi.json`.
- Snapshot SHA-256:
  `72452d6951dbaadd1030af76434917ef103e470bf0cd6ac035b02b111bfd4d24`.
- JavaScript SDK revision: `66880ccded6cb642dc1809620c2b108c33730214`.
  [Types](https://github.com/typesafe-ai/typesafe-sdk-js/blob/66880ccded6cb642dc1809620c2b108c33730214/src/types.ts)
  and [builders](https://github.com/typesafe-ai/typesafe-sdk-js/blob/66880ccded6cb642dc1809620c2b108c33730214/src/questions.ts).
- Python SDK revision: `420ef4ffb612d5a539a1e0f0fe883ff6770340af`.
  [Generated models](https://github.com/typesafe-ai/typesafe-sdk-python/blob/420ef4ffb612d5a539a1e0f0fe883ff6770340af/src/typesafe_sdk/_schemas/models.py).
- Human documentation is moving:
  [advanced structures](https://docs.typesafe.ai/primitives/advanced),
  [HTTP](https://docs.typesafe.ai/api),
  [Choice](https://docs.typesafe.ai/primitives/choice),
  [Score](https://docs.typesafe.ai/primitives/score).

`fixtures/structured-response.json` is **authored synthetic test data**, not a
provider capture. Its extra billing field tests preservation of unknown usage.

## Experiment matrix

All evaluation outcomes below remain unobserved. `list` enumerates exact names;
`show NAME` renders the full request without network access. Use `structured` as
the positive control before interpreting failures of other probes.

| Probe(s) | Question to resolve |
| --- | --- |
| `structured` | Mixed question kinds, structured instructions, nested Choice subtrees, structured Score legends, nested scalar/null state, returned model and usage. |
| `state-null` | JS admits null state; live OpenAPI and Python documentation exclude it. |
| `state-array` | Array state containing nested structured/scalar/null values. |
| `state-number`, `state-boolean` | Negative controls for forbidden outermost scalars. |
| `instructions-omitted`, `instructions-null`, `instructions-array` | Confirm optionality and structure; short HTTP prose is narrower than live schema. |
| `instructions-number`, `instructions-boolean` | Negative controls for instructions' outermost type. |
| `noul-criteria-omitted`, `noul-criteria-null`, `noul-criteria-empty` | Preserve three distinct forms; observe validity separately. |
| `noul-true-only`, `noul-false-only`, `noul-outcomes-null` | Independently optional outcomes and explicit null descriptions. |
| `score-null-level` | Advanced docs/JS permit null; live OpenAPI/Python exclude it. |
| `score-zero`, `score-one`, `score-two`, `score-ten`, `score-eleven` | Live schema says at least one; JS requires two; prose says 2–10. |
| `choice-zero`, `choice-one`, `choice255`, `choice256` | Prose caps choices at 255; live schema does not expose bounds. |
| `choice-null-description`, `choice-array-description` | Null and arrays as alternative descriptions. |
| `choice-number-description`, `choice-boolean-description` | Negative controls for scalar criteria descriptions. |
| `empty-questions` | Required nonempty question collection. |
| `escaped-keys` | Response correspondence with Unicode, separators, newline, and quoted Choice keys. |
| `extra-request-property`, `extra-question-property` | SDK forwarding does not establish whether extras are meaningful, ignored, or rejected. |

For each eventual run, record the evidence filename, harness fingerprint, requested
and returned models, status, relevant findings, and a narrowly stated conclusion.
Acceptance of an extra field does not prove the model used it. Success on one
model does not establish a permanent model-independent cardinality rule.

## DSL implications awaiting evidence

Keep state, optional instructions, nullable Choice descriptions, Score levels, and
optional Noul criteria as distinguishable positions until experiments resolve
their boundaries. Preserve omission and null. Do not publish a single universal
entry constructor solely because a SDK alias uses one.

Response association is checked separately from typed JSON decoding. Research
diagnostics compare question kinds/keys, Choice candidates, Score indices/legends,
probability ranges, and approximate sums/means. These diagnostics intentionally
do not discard the raw observation or claim final production validation semantics.

## Local feasibility evidence

The GHC sketch demonstrates record modes and heterogeneous result elimination.
Mixed scopes and cross-scope `coerce` are rejected. Missing handlers require a
warning-as-error policy; record construction alone is not total. This is evidence
about GHC 9.12.2, not evidence about the Tidepool engine or TypeSafe's API.
