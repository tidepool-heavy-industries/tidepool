# Jev effect and Haskell DSL — research and design handoff

Recorded 2026-09-16. Status: exploration and agreed design direction, **not an
implementation-ready specification**. Preserve the uncertainties below for API
experiments and further design. All 63 initial authenticated probes have now run;
see [observed contract](../jev-integration/CONTRACT.md#authenticated-observations--2026-09-16).
An isolated GHC feasibility sketch and Rust probe harness now exist; see the
continuation below. The unrelated Core → STG cutover is outside this work.

### Isolated implementation continuation

Worktree: `/home/inanna/dev/tidepool-jev-integration`, branch
`research/jev-integration`, based on `00f8f3332`. The `jev-integration` crate is a
member of the larger Cargo workspace, with no engine dependencies. Compile/run it
with `-p jev-integration`; it is not a separate Cargo workspace. See its
[README](../jev-integration/README.md) and
[experiment matrix](../jev-integration/CONTRACT.md).

The single-operation production DSL is still pending. A GHC-only sketch now
demonstrates mode-interpreted records, heterogeneous handlers, recursive keyed
collections, and hidden scope evidence. Its positive example executes and its
negative fixtures reject mixed scopes, wrong payloads, cross-scope coercion, and
missing handlers. The last requires `-Werror=missing-fields`; records alone do not
enforce total construction. The sketch is not generic serialization or resident
engine acceptance.

The research client captures raw bytes and separate provisional interpretations,
has no automatic retries/redirects, and uses synthetic probes for disputed or
invalid forms. These raw builders deliberately are not the future valid-by-type
public request surface. A public OpenAPI fetch succeeded through the client;
authenticated experiments succeeded against `jev-latest`, resolving to `jev-1.13.0`.

## Intent and conversation decisions

Concrete follow-on applications, source anchors, and proposed evaluation cases are
recorded in [Jev in Shoal: practical semantic decisions](jev-shoal-examples.md).

The clarified focus is internal glue for many small workers with occasional larger
reasoners, not an operator conversational frontend or intent compiler. Follow-on
[structured swarm experiments](../jev-integration/WORLD-EXPERIMENTS.md) exercise
contract-sensitive delegation and evidence selection over artificial agent graphs.

[Notebook microprogram mockups](jev-notebook-microprograms.md) show complete cells
with two or three dependent command/Jev decisions, typed continuation boundaries,
and the proposed record DSL. They assume the effect exists and are not compiled.

[Bash and code-exploration examples](jev-bash-lsp-examples.md) extend that shape
with five potential LSP-assisted notebook workflows: symptom tracing, focused
verification, implementation reuse, migration archaeology, and reproducer selection.

[Core Shoal semantic-control examples](jev-core-shoal-uses.md) pair proposed
Servant-style Haskell usage with representative Jev wire requests for investigation
compilation, code-graph traversal, swarm traffic, evidence construction, adaptive
verification, semantic stopping, and cheap supervision.

[Decision-frontier experiments](../jev-integration/FRONTIER-EXPERIMENTS.md) now
bound the architectural split with live calls. Jev handled 255-way six-field
selection, explicit absence, seven-edge semantic path validation, temporal
exceptions, and overlapping judgments. Exact opaque pointer traversal degraded
at depth four, was unstable at eight, and failed consistently at 16–64. This
supports a strong division: Haskell performs exact traversal, joins, and state
transitions; Jev receives the deterministically computed semantic frontier and
selects or judges a continuation. Wide requests also intermittently returned
rounded probability maps that failed the provisional sum check despite choosing
the expected key, so response-contract validity and action policy must remain
separate from selected-answer identity.

The efficacy-maximizing follow-up substantially raises the expected request
surface. A 640-question speculative fan-out returned every expected judgment in
586 ms, and a seven-answer Shoal decision microprogram succeeded in five observed
runs at 135–208 ms. Prefer one question-rich request when all evidence is already
available; consume branch-relevant answers afterward. Shared-state size remains
bounded: a single question passed with 32,568 input tokens but larger test states
were rejected. Choice alternative keys are model-facing labels and measurably
bias duplicate alternatives, so coalesce equivalent continuations before calling
Jev and give retained alternatives intentional semantic names.

The user wants TypeSafe's Jev available as a fluent, expressive Haskell effect for
resident LLM-authored programs. Previously considered FunctionGemma, Haiku, and
Luna for this role. The motivating workloads are small semantic decisions embedded
in deterministic programs:

- Route messages to an appropriate existing agent.
- Decide whether to wake an agent now or queue a message until its next wake.
- Navigate complex LSP and other result graphs, using intelligence to steer
  deterministic traversal.
- Use Jev inside algebras and coalgebras, with Haskell providing structure,
  routing, rules, and recursion. The coalgebra already produces the next seeds;
  expansion can depend on its judgment. Algebra and coalgebra may use different
  questions, context, and candidate structures.

The user's formulation of the interface goal:

> their full capability set expressed elegantly as a haskell DSL that admits every
> valid option but does not allow what is invalid, as encoded in the haskell type system

### Corrections that must survive the handoff

1. **One effect operation**, matching a native Jev evaluation request. An earlier
   proposal for separate effectful `choose`, `assess`, and `score` verbs was rejected.
   Question constructors/builders are ordinary data construction, not separate
   effects.
2. **Expose the real API's full structure.** A simplified router or a
   `(Text, a)`-only description interface is insufficient. Structured instructions,
   criteria, state, results, and metadata are central.
3. **Do not center the design on batching.** Native requests contain a map of one
   or more questions; preserve that contract. Do not introduce a batching workflow,
   speculative execution framework, or request coalescer. Dependent calls in a
   Haskell traversal are a primary use case.
4. **Servant-style record syntax is explicitly preferred.** The existing agent
   contract's mode-interpreted records are the local precedent.
5. **The current deliverable is this document.** Continue research and refine the
   plan later, while unrelated implementation work finishes. Do not start an engine
   integration or opportunistic cleanup from this handoff.

### Explicit answers to design polls

| Topic | User decision |
| --- | --- |
| Structured data authoring | Ordinary Haskell records with derived structural encoding, plus literals/builders for ad hoc and heterogeneous content. User suggested that this may itself use the same record under different modes. |
| Question composition | Support nested reusable records and runtime-sized keyed collections in the initial design. Compile to the provider's flat question map and reconstruct the authored result structure. |
| Candidate membership | Preserve candidate-set identity and let structure flow through to results, beyond merely returning the payload type. |
| Criteria record modes | Yes: use the same record for Choice alternatives or Score levels in description mode and probability mode, in addition to outer question/answer modes. |
| Scoped identity ergonomics | Hide scopes in abstract results. Ordinary result binding; typed accessors or an eliminator when membership evidence is needed. No mandatory `withRequest` continuation around every call. |
| Heterogeneous alternatives | Permit different alternatives to retain different Haskell payload types, e.g. `Follow Edge` versus `Finish Evidence`. Consume through an exhaustive typed handler record; also support homogeneous choices. |
| Conflicting provider contracts | Record uncertainty now. Run experiments against the actual API later and use the results to refine the plan. Do not silently choose the union, intersection, or OpenAPI as an unquestioned authority. |

## Provider facts and evidence

Read the installed TypeSafe skill and its linked live documentation. The skill was
installed with the requested non-Claude method, using Nix to supply the missing
`npx` executable:

```sh
nix shell nixpkgs#nodejs --command npx --yes skills add typesafe-ai/skills --skill typesafe-ai --agent codex --global --yes
```

The installer reported a successful Codex installation at
`/home/inanna/.agents/skills/typesafe-ai/SKILL.md`. Do not assume it was installed
under `.codex/skills`; that path did not exist when checked. Read the installed
skill when continuing this work. Its guidance led this investigation to the live
API, structured-input docs, and traversal cookbooks. User instructions about the
single operation and lack of batching emphasis take precedence over cookbook
workflow suggestions.

### Evaluation contract

`POST https://api.typesafe.ai/v1/systemone`, bearer authentication, JSON body:

- `state`: shared input to the questions.
- `model`: model name or alias; examples use `jev-latest`.
- `questions`: nonempty map of identifiers to typed questions.

The response contains `model`, `answers` keyed by the supplied question identifiers,
and `usage`. Answers discriminate on `type`. Choice returns a selected label,
probabilities, and confidence. Score returns an expected level index, legend,
probabilities, and confidence. Noul returns the probability of yes, without a
separate confidence field. The SDK expresses the relationship as
`SystemOneRequest<Q>` → `SystemOneResult<Q>` with `ResultFor<Q[K]>` per field.
[HTTP reference](https://docs.typesafe.ai/api),
[SDK source](https://github.com/typesafe-ai/typesafe-sdk-js/blob/main/src/types.ts).

### Structured descriptions are first-class

The advanced documentation permits strings, objects, arrays, and null for
instructions, Choice descriptions, Score descriptions, and Noul outcome
descriptions. Nested objects and arrays contain recursive JSON, including numbers
and booleans. Top-level numeric and boolean entries are not admitted by the SDK's
`EntryType`; see the contract discrepancies below for null distinctions.

The taxonomy example passes each child's **subtree as its Choice description**.
The model can inspect what lies below a branch before code traverses it. This is
directly relevant to supplying graph neighborhoods, agent responsibilities, and
other structured observations from Haskell. Structured fields are model input,
not an executable provider-side program or a new recursive answer type.
[Advanced structure](https://docs.typesafe.ai/primitives/advanced).

### Semantics worth preserving

- Question identifiers associate answers with questions; the model does not see
  them. Choice labels and descriptions are model-visible. Replacing labels with
  generated IDs must retain their meaning in descriptions where needed.
  [Choice](https://docs.typesafe.ai/primitives/choice).
- Questions in a request independently evaluate the same state. They cannot read
  one another's answers. Later evidence or dependent options require another call.
  [State](https://docs.typesafe.ai/concepts/state).
- Score is the expectation of zero-based rubric positions, not an arbitrary
  generated number. The same expectation can arise from different distributions.
  Levels must be independently meaningful; the docs say the model does not see
  a level's index or neighboring levels. Structured descriptions are returned in
  `legend`; reducing that field to text would be lossy.
  [Score](https://docs.typesafe.ai/primitives/score).
- Confidence summarizes the distribution's shape. It is not a proof of truth,
  resource authority, or overall workflow success. Preserve the distribution and
  let authored code choose its policy.
  [Confidence](https://docs.typesafe.ai/confidence).
- A Choice distribution ranks supplied alternatives even if none is appropriate.
  A no-match alternative or a separately meaningful presence question can express
  that situation. The semantic-search example selects existing line IDs rather
  than generating text spans.
  [Semantic search](https://docs.typesafe.ai/cookbooks/semantic_find).

The hierarchy cookbook illustrates keeping several paths using geometric-mean
edge probabilities. This is an application search heuristic, not a calibrated
probability that the full path is correct. Its prose describes parallel questions,
but the shown implementation issues separate one-question requests through a
thread pool. Do not infer request composition from the headline alone.
[Hierarchy cookbook](https://docs.typesafe.ai/cookbooks/hierarchical_classification).

### Performance context, not acceptance evidence

The launch post advertises $0.042 per million input tokens with free output and
70–500 ms end-to-end calls. At that price, 100,000 requests averaging 1,000 billed
input tokens would cost $4.20. These are published claims and arithmetic, not our
measurements. Questions and criteria contribute input tokens.

Their Doom description uses structured state, reports ten queries per second,
and gives roughly $7/hour for that particular workload. Their Wikiracing demo is
also relevant to graph traversal. We read the published descriptions; we did not
inspect a public Doom implementation or independently run the demos.

The frontier-intelligence comparison is task-specific vendor evidence. The
workflow evaluations use larger models' judgments as references, not independent
ground truth. Typed outputs can still be wrong decisions.
[Launch post and demos](https://typesafe.ai/blog/introducing-system-one-models-and-jev).

## DSL direction to develop

These are design constraints and illustrative shapes, not compiled signatures or
final names. Keep the wire contract and the Haskell interpretation distinct.

### One interpretation operation

The public effectful entry point should accept a complete typed request and return
the corresponding typed response, with typed failure. Schematically:

```haskell
jev
  :: (Member Jev effects, JevSchema schema)
  => JevRequest (schema Questions)
  -> Eff effects (Either JevError (JevResponse (schema Answers)))
```

Dynamic construction may require a checked preparation step before a request is
inhabited. The final signature must reflect that rather than accepting unchecked
collections and claiming they are valid. Construction, projection, inspection,
and result elimination are pure; this remains one effect operation.

### Contract-shaped value algebra after live probes

The initial wire target can now be stated precisely enough to prototype. Keep
these position types distinct even when they share encoders:

```haskell
-- Illustrative names, not the final API.
data Structured = SText Text | SObject (Map Text Json) | SArray [Json]
data NullableStructured = Structured Structured | StructuredNull

data Instructions
  = InstructionsOmitted
  | InstructionsPresent NullableStructured

data NoulCriteria
  = NoulCriteriaOmitted
  | NoulCriteriaNull
  | NoulCriteriaPresent
      { trueWhen  :: Maybe NullableStructured
      , falseWhen :: Maybe NullableStructured
      }
```

`Json` inside an object or array is fully recursive and admits strings, objects,
arrays, numbers, booleans, and null. The outer constructors are position-specific:

| Position | Outermost forms |
| --- | --- |
| State | String, object, array; required; empty forms valid; no null/number/boolean |
| Instructions | Omitted, or string/object/array/null; empty forms valid |
| Choice description | String/object/array/null; empty forms valid |
| Score level | String/object/array; empty forms valid; no null |
| Noul `true`/`false` description | Independently omitted, or string/object/array/null |

The collection invariants are also position-specific. Questions are a nonempty
map with nonempty text keys; no upper bound was found through 256 entries. Choice
criteria are a map of 1–255 alternatives, whose text keys may even be empty.
Scores contain an ordered 1–10 sequence. Static record schemas should prove these
facts structurally. Dynamic maps/sequences need total checked constructors that
return the validated opaque collection or a construction error. They must not
silently drop entries, synthesize labels, or defer known invalidity to the effect.

`Model` remains an evolvable text identifier or alias, not a closed promoted enum:
presence and string shape are local facts, while availability is service/account
state and therefore a typed operation failure. The response model remains the
resolved concrete model and must not be overwritten with the requested alias.

The response is all-or-nothing at the request level. Before producing typed
answers, validate that every requested question has exactly one same-kind answer,
Choice selections and probability keys belong to the exact submitted candidate
set, Score legend/probability indices cover the exact submitted levels, and no
unexpected answer keys exist. Preserve the full distributions, structured legend,
resolved model, and usage. This validation is the runtime bridge from untrusted
JSON to the mode-indexed answer record.

### Records interpreted under modes

Use one authored question record across request and response modes. Its leaves
declare question kinds; nested records and keyed collections retain their shape
in the result. Use another mode-interpreted record for alternatives or rubric
levels, so their descriptions and probabilities have corresponding fields.

An alternative can retain a local Haskell payload and a separate model-visible
structured description. Local handles, seeds, and closures must not need a JSON
codec merely to be selectable. Different fields may retain different payload
types. An exhaustive handler record should consume the selected alternative with
the correct payload type. Homogeneous collections should remain convenient.

Descriptions can themselves be ordinary records or literals. Do not require
authors to encode every domain fact as a type-level string, or stringify JSON
records into prompts. Preserve omission versus explicit null where the contract
distinguishes them. Avoid a universal `ToJSON a => a` entry point that can encode
an invalid outer shape while claiming static validity.

### Evidence and abstraction boundaries

- Types should enforce question/criteria shape and static record relationships.
- Checked constructors establish runtime cardinality, uniqueness, and content
  invariants once, returning opaque valid values. No partial constructors.
- Abstract results own their candidate mappings. If candidate witnesses are
  exposed, their scope parameter must prevent mixing candidates from different
  runtime sets, including sets with identical payload types. Hide generativity
  behind result access/elimination; ordinary calls must not require CPS syntax.
- Validate external responses against the actual request before constructing
  typed answers. Haskell types do not make network bytes trustworthy.
- Do not erase structural descriptions, criteria identity, distributions, or
  model/usage metadata on the way back.
- Static validity means structural validity for the eventual verified contract.
  It cannot prove semantic quality, account permissions, current model
  availability, successful transport, or arbitrary Haskell termination.

The isolated sketch now typechecks a subset of the mode algebra and candidate
eliminators. Generic derivation, full criteria modes, valid entry construction,
and the complete effect signature still need prototypes. The sketch does not
establish that the illustrative signature supports all of these goals.

## Declared contract disagreements and initial experiments

The table below preserves the original research questions. Live observations now
resolve the tested forms for the current endpoint: null state and null Score
levels are rejected; Scores accept 1–10 levels and Choices 1–255 alternatives;
omitted/null/structured instructions and all tested optional Noul forms succeed.
Usage contains input/output token counts. Extra properties are accepted, but their
meaning remains unknown. See the crate contract for exact status codes and
provenance. These are single samples, not promises about future model versions.

The live [OpenAPI document](https://api.typesafe.ai/openapi.json) was successfully
read without credentials. It identifies version `0.2.0` and the `/v1/systemone`
and `/v1/models` paths. It disagrees with both prose and SDKs:

| Position or constraint | Evidence observed | What to establish later |
| --- | --- | --- |
| Null state | JS `EntryType` permits null; live OpenAPI excludes it; Python question docs prohibit `None` state. | Whether the service accepts explicit null state. |
| Null Score levels | Advanced docs and JS tuple type permit null; live OpenAPI and generated Python models exclude it. | Whether null levels and null legend entries are valid. |
| Score cardinality | Prose says 2–10; JS validates at least two; live OpenAPI has `minItems: 1` and no maximum. | One, two, ten, and eleven levels, including model-specific rejection. |
| Choice cardinality | Prose says up to 255; live OpenAPI declares no bounds on the criteria map. | Empty, singleton, 255, and 256 alternatives. |
| Instructions | Short HTTP prose says required; SDKs and live OpenAPI allow omission and explicit null. | Omitted/null/structured instructions, without conflating the encodings. |
| Noul criteria | Live schema permits omission, null, and independently optional true/false entries. | All combinations, including structured and null descriptions. |
| Usage | Live OpenAPI and JS expose required input/output tokens; checked-in generated Python models instead include required billing units and optional tokens. | Actual response shape and which accounting fields are stable. |
| Extra request/question properties | SDKs describe forwarding extra fields. | Whether extras have semantics, are ignored, or are rejected; forwarding alone is not a new model capability. |

The user's decision is to **retain these uncertainties in the plan**, then run
experiments and revise it. Do not prematurely encode disputed boundaries as
universal type-level facts. Do not claim that an SDK's permissive annotation proves
server acceptance. Keep confirmed structural distinctions separate from
model-dependent limits when refining the design.

Experiment with small synthetic inputs, recording model, exact request shape,
status, response, usage, and SDK/OpenAPI revisions. Include valid structured
objects/arrays, nested numbers/booleans/nulls, and invalid bare numeric/boolean
entries. Never include credentials in captures. The first live matrix is complete;
behavioral quality, calibration, concurrency, and production integration remain
unverified. A second matrix established that the operation's request spine is
total: model, state, and a nonempty question map are required, and one invalid
question rejects the whole request. Question identifiers are nonempty; Choice
identifiers may be empty. Empty string/object/array descriptions are accepted,
and recursively nested scalar/null leaves work everywhere structure is admitted.
The precise position-specific outer grammar still matters: Score levels and state
exclude null, while Choice descriptions include it.

Server validation also revealed an undocumented `bounding_box` discriminator,
absent from public OpenAPI, docs, and pinned SDK sources. The account rejected it
as not enabled before exposing its schema. It likely concerns image localization,
but that is unverified and explicitly outside the current text/glue scope by user
decision. Preserve the finding as capability drift evidence; do not include a
speculative BoundingBox constructor in this DSL.

## Repository fit and later implementation sequence

Read the nearest `AGENTS.md` again before implementation. Owning sources inspected:

- `haskell/lib/Tidepool/Agent/Contract.hs`: `Generic` record traversal and
  mode-interpreted `(:-)`. Its family is **closed and agent-specific**, so it cannot
  simply receive an external Jev instance. Decide a qualified Jev operator versus
  a deliberately shared abstraction; avoid an accidental global name collision.
- `haskell/lib/Tidepool/Aeson/Value.hs` and `Schema.hs`: existing generic codecs and
  structural data. Reuse these owners. `Value` admits more outer shapes than some
  Jev positions; use appropriate typed wrappers. JSON objects use `Map`, with
  sorted keys, while Score level order is semantic and must remain explicit.
- `tidepool-protocol`: single-source concrete effect/verb and bridge contracts.
  Rich polymorphic DSL interpretation belongs in the authored Haskell library,
  not raw source strings embedded in protocol schema declarations.
- `tidepool-handlers`: interpreter and capability-stack assembly. Existing `Llm`
  uses chat plus JSON Schema through `genai`; Jev's endpoint requires its own
  contract. Keep provider transport, credentials, cancellation, and resource
  policy in Rust. Do not put network mechanics in Haskell.
- `tidepool-actor`: actor lifecycle and mailbox authority remain here. A selected
  route or wake policy does not create authority or transfer ownership.

Suggested continuation sequence:

1. Reconcile future provider/schema revisions against the captured capability matrix.
2. Prototype the record modes, position-indexed structured entry construction, heterogeneous
   alternative elimination, and hidden candidate scopes under GHC. Review real
   authored examples before committing to names and syntax.
3. Settle nested record flattening, collision-free question IDs, meaningful Choice
   labels, rubric ordering, and dynamic collections. Support arbitrary valid
   provider keys without forcing them to be Haskell identifiers. Define how fully
   dynamic heterogeneous question maps retain their answer relationships.
4. Specify typed construction, service, and response-contract errors; model
   selection; deadlines/retries; and capability-stack registration. These are not
   settled in this conversation. Avoid silently falling back to another provider.
5. Add the single concrete protocol operation, Haskell interpretation, and Rust
   handler, then verify the normal resident call path once unrelated engine work
   permits it. Do not change engine internals to satisfy this handoff.

### Current state and later work

The `Jev` effect (`JevAskWith`, JSON text in and out) is available to every
Shoal role. `haskell/lib/Jev/Core*` is vendored from `~/dev/jev-dsl` by
`scripts/sync-jev-dsl.sh` (the source commit is in `haskell/lib/Jev/VENDORED`).
`Jev.Operators` binds the operators to Tidepool's `Value` and the host transport.

Later: make jev-dsl a nix flake input in the GHC package set, and drop the
vendored copy and the sync script. Add a `[jev]` workspace config table
(`base_url`, `timeout_ms`, `max_calls_per_run`) if the defaults stop fitting.

### Acceptance examples and checks to design

- One-question call with nested structured instructions and criteria.
- Nested question records and runtime collections reconstruct matching answers.
- Criteria/rubric records interpreted as descriptions and probabilities.
- Heterogeneous Choice with exhaustive handlers and local opaque payloads.
- Dynamic LSP-like candidate sets with no-match or termination alternatives.
- Small authored traversal using fresh seeds and full distributions; no new
  traversal runtime or mandatory recursion-scheme library.
- Compile-fail fixtures for mismatched modes, invalid static shapes, incomplete
  alternative handlers, and cross-scope candidate misuse.
- Checked failures for invalid dynamic collections and malformed responses:
  missing/extra identities, wrong answer kinds, unknown selections, invalid
  probabilities, mismatched legends, and invalid numeric values.
- Wire-shape tests for nesting, order, omission/null, and escaping; transport
  tests for timeouts, cancellation, authentication, validation, and rate limits.
- Preserve metadata and test request/response association, not only pretty output.

Use repository Nix/GHC setup for Haskell checks and the smallest owning-crate
checks for Rust. The original handoff was documentation-only; verification of the
later research scaffold is recorded in `jev-integration/VALIDATION.md`.

## Additional reading already consulted

- [TypeSafe skill](https://github.com/typesafe-ai/skills/blob/main/skills/typesafe-ai/SKILL.md)
- [Documentation index](https://docs.typesafe.ai/llms.txt)
- [System One](https://docs.typesafe.ai/concepts/system-one)
- [Building guide](https://docs.typesafe.ai/concepts/how-to-build-with-system-one)
- [Function calling cookbook](https://docs.typesafe.ai/cookbooks/function_calling)
- [JS question builders and validation](https://github.com/typesafe-ai/typesafe-sdk-js/blob/main/src/questions.ts)
- [Python question reference](https://docs.typesafe.ai/sdk/python/api/types/questions)
- [Generated Python models](https://github.com/typesafe-ai/typesafe-sdk-python/blob/main/src/typesafe_sdk/_schemas/models.py)

Sources above were read on 2026-09-16; links generally track moving documentation
or `main`. Pin revisions when building conformance fixtures. Some docs URLs failed
in the browser tool but their Markdown versions were successfully fetched with
`curl`; documentation access was not blocked.
