# Provider contract evidence

Baseline and authenticated observations recorded 2026-09-16.

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

All 63 probes have been executed once. `list` enumerates exact names;
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
| `state-empty-*`, `state-omitted` | Distinguish required presence from nonempty content. |
| `model-omitted`, `model-null`, `model-unknown` | Establish the model selector's presence, type, and service validation. |
| `questions-omitted`, `questions-null`, `questions-array` | Establish the outer question collection grammar. |
| `question-type-*`, `question-empty-key` | Establish discriminator and identifier constraints. |
| `choice-empty-*`, `choice-deep-description` | Exercise empty identifiers/descriptions and recursively structured descriptions. |
| `score-empty-*`, `score-array-level` | Exercise empty and recursively structured non-null Score levels. |
| `instructions-empty-*` | Determine whether empty structured instructions are valid values. |
| `noul-unknown-criterion` | Observe treatment of fields beyond `true` and `false`; acceptance alone cannot establish semantics. |
| `mixed-valid-invalid-questions` | Determine whether validation is atomic across the question map. |
| `questions255`, `questions256` | Check a large dynamic question map and exact answer correspondence. |
| `bounding-box-empty` | Investigate an undocumented discriminator revealed by validation; explicitly out of current scope. |

For each run, record the evidence filename, harness fingerprint, requested
and returned models, status, relevant findings, and a narrowly stated conclusion.
Acceptance of an extra field does not prove the model used it. Success on one
model does not establish a permanent model-independent cardinality rule.

## Authenticated observations — 2026-09-16

All requests used `jev-latest`; all 21 successful responses reported `jev-1.13.0`.
There were three HTTP 400 responses, ten HTTP 422 responses, and no transport
failures. Successful responses had no provisional interpretation findings.
Raw captures are private, ignored files at `evidence/<probe>-001.json`.
The initial captures record harness BLAKE3
`6e3f42c3b46b4fa9580922ef0040c2f642662c7c327d7fbdd881f132f109eff4`.

| Position | Accepted (HTTP 200) | Rejected |
| --- | --- | --- |
| Structured positive control | Object state, structured instructions, nested Choice descriptions, object Score levels, all three question kinds | — |
| State | Array with nested values | Null, bare number, bare boolean: 422 |
| Instructions | Omitted, null, array | Bare number/boolean: 422 |
| Noul criteria | Omitted, null, empty object, true-only, false-only, null outcome descriptions | — |
| Score levels | 1, 2, 10 | Zero: 422; 11: 400; null level: 422 |
| Choice alternatives | 1, 255; null/array descriptions | Zero/256: 400; bare numeric/boolean descriptions: 422 |
| Questions | Escaped and Unicode keys round-trip | Empty map: 422 |
| Unknown properties | Extra request and question properties | Semantics not established by acceptance |

The structured routing example selected `configuration_owner` with probability
0.98, returned blocker probability 0.97, and urgency score 1. Its end-to-end
client measurement was 197 ms, with 534 input and 74 output tokens. This is a
synthetic sanity check, not evidence of calibrated confidence or routing quality.
Across successful calls the observed median was 174 ms, range 153–218 ms;
these serial single samples are not a latency benchmark. Reported usage summed
to 12,775 input and 3,227 output tokens. Rejections supplied no usage; these totals
are response accounting, not a billing statement.

### Constructor-shape matrix

A second matrix added 29 calls: 16 HTTP 200, four HTTP 400, nine HTTP 422,
and no transport failures. Its ordinary probes record harness BLAKE3
`412583468a617ccd5dd2dacd31bc643765a1cdfea8503b90fd681bc5c7805b6b`;
the final BoundingBox discovery probe records
`5c651803ca65b4fd95b738ec81a6f4079fa3f491ba21a03a652fa3dd13a543a8`.

| Position | Observed contract |
| --- | --- |
| Request spine | `state`, `model`, and `questions` are all required. Model must be a string and an unknown name is rejected. Questions must be an object. |
| Empty state | Empty string, object, and array are accepted. Presence is required; semantic usefulness is not structurally enforced. |
| Question discriminator | Required and case-sensitive. The documented `noul`, `choice`, and `score` tags are closed constructors for the current scope. |
| Question identifiers | Empty identifier rejected. Unicode and punctuation round-trip exactly. |
| Choice identifiers | Empty identifier accepted and round-trips, though it is usually a poor authored label. Choice description admits null, empty string/object, array, and deeply nested JSON containing scalar/null leaves. |
| Score levels | Empty string/object and structured arrays with nested null/scalars are accepted. The level itself may not be null. Returned legends preserve the structured values. |
| Instructions | Omitted, null, empty string/object/array, and recursively structured values are accepted. Bare number/boolean remain invalid at the outermost position. |
| Noul criteria | The service accepts an unknown `maybe` member, but the response cannot establish that it has semantics. The DSL should expose the documented closed `true`/`false` record, not claim open extension behavior. |
| Request atomicity | One invalid Choice in a mixed map rejects the whole request; there is no partial answer map. |
| Dynamic question maps | 255 and 256 Noul questions both returned exactly the corresponding number of keyed answers. No maximum is established. |

The 255- and 256-question calls took 256 ms and 398 ms and reported 255 and
256 answers respectively. These deliberately repetitive calls establish native
map shape and correspondence only; they are not a recommendation to build a
batching layer or a performance benchmark.

Validation errors unexpectedly listed a fourth `bounding_box` discriminator.
It is absent from the live public OpenAPI 0.2.0, documentation index, and pinned
JavaScript/Python SDK sources. A minimal request was recognized but returned HTTP
400: “Bounding-box questions are not enabled for your organization.” Its intended
image-region semantics and wire shape therefore remain an inference, not an
observed contract. Per user direction, this likely vision primitive is explicitly
outside the present text/glue integration and is not part of the DSL target.

## DSL implications from initial evidence

Keep state, optional instructions, nullable Choice descriptions, Score levels, and
optional Noul criteria as distinguishable positions. Preserve omission and null.
State and Score levels exclude outer null; Choice descriptions admit it;
instructions admit null and omission. Nested JSON scalars remain allowed.
The observed bounds are Score 1–10 and Choice 1–255, with nonempty questions and
nonempty question identifiers. Choice identifiers themselves may be empty.
Represent runtime collection validation with smart constructors and retain their
evidence in types; static declarations can additionally use type-level checks.
Do not publish a universal entry constructor solely because an SDK alias uses one.
The required request spine should be total in the one operation: model, valid
outer state, and nonempty questions cannot be optional fields. Invalid members
fail the entire operation. These observations constrain the initial target
contract, not all future models.

Response association is checked separately from typed JSON decoding. Research
diagnostics compare question kinds/keys, Choice candidates, Score indices/legends,
probability ranges, and approximate sums/means. These diagnostics intentionally
do not discard the raw observation or claim final production validation semantics.

## Local feasibility evidence

The GHC sketch demonstrates record modes and heterogeneous result elimination.
Mixed scopes and cross-scope `coerce` are rejected. Missing handlers require a
warning-as-error policy; record construction alone is not total. This is evidence
about GHC 9.12.2, not evidence about the Tidepool engine or TypeSafe's API.
