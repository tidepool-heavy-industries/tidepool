# JSON optics in the resident workbench

Status: future direction, recorded from operator discussion. This is not a
prerequisite for the shoal-repl build or an implementation commitment.

## Intent

Make JSON-shaped data a first-class working material in Tidepool's persistent
Haskell environment. Agents already understand many tool payloads through their
JSON schemas. They should be able to retain those values, explore and transform
them with lens/Aeson-style optics, compose functions over them, and use the
results in typed orchestration without defining a Haskell record for every shape.

JSON may remain the right representation indefinitely. Introduce domain types
where their distinctions improve decisions, not as a mandatory graduation step.
Optimize expressions for agent utility and legibility; the operator is also
comfortable reading lens code. Do not simplify away useful composition merely
to make all agent-authored code introductory Haskell.

This supports the shared human/agent workbench envisioned for shoal-repl and
[recursive context collaboration](recursive-context-collaboration.md): useful
investigations leave executable functions and values behind, and those become
material for later scaffold/fork/fold cycles.

## Existing foothold and discovery

The live Shoal default scope was checked during this discussion: `(^.)`, `lens`,
`traverseOf`, `_Just`, `preview`, `key`, and `_String` were available. Evaluating
`(1 :: Int, "hello" :: Text) ^. _2` returned `hello`. This establishes availability
and one simple execution, not comprehensive compiler support for arbitrary optic
compositions. Inspect existing library exports and translation support before
adding surface or dependencies.

A possible discovery interaction is:

```haskell
thing <- getThing
schemaOf thing
```

These names are illustrative, not an existing API. Distinguish a declared schema
from a shape inferred from one observed value. An empty array cannot establish
its element schema; absent fields do not establish that those fields are forbidden.
A bare `Value` cannot recover schema provenance that was never retained. Decide
whether the producing interface exposes schema separately or retains associated
metadata when an actual consumer needs this. Reuse existing schema owners.

## High-utility scenarios

### Retain a dataset, investigate repeatedly

Fetch CI or tool results once. Query failures, group diagnostics, correlate them
with changed files, and hand relevant slices to repair agents. Retain the full
value while printing only useful projections. Subsequent questions become local
expressions rather than repeated fetches and large tool transcripts. Refresh
explicitly when current external state matters.

### Leave an investigation as a function

Preserve a useful query and apply it to fresh snapshots later: for example, find
requests whose provider failed while their responses remain pending. Start with
task-local definitions. Repeatedly useful vocabulary may eventually justify
committed helpers such as `.shoal/Helpers.hs`; automatic helper loading is a
separate, deferred decision.

### Derive a recursive frontier from evidence

Group structured failures by owning subsystem and construct child assignments
from those groups. Children receive relevant diagnostics, data, and reusable
transformations through supported live-value/context mechanisms. Fold typed
outcomes, refresh the remaining failures, and repeat. JSON queries express the
task-specific decomposition; existing Shoal actor operations own scheduling,
authority, and settlement. There is no new JSON scheduler or task registry.

### Derive fixtures from real interactions

Capture a backend response, redact sensitive fields, reduce it to the interesting
case, and derive variants: missing optional fields, unknown event kinds,
duplicates, empty batches. Use the results in mocks and integration tests.
For shoal-repl, event traces could supply realistic replay and recovery cases.
Keep deliberately invalid fixtures distinct from valid protocol examples.

### Carry executable acceptance with an assignment

Retain a predicate over an event trace or tool result and share it with the
implementer and reviewer through supported function-valued collaboration.
Return useful counterexamples, not only a Boolean. This keeps the acceptance
criterion executable across contexts without translating it into several scripts.
Passing a predicate is evidence for its stated obligation, not proof of every
product requirement. Do not serialize live Haskell closures into JSON.

### Invent operator views during work

Compose queries for actors needing attention or candidates ready for review with
their evidence. A future shoal-repl structured renderer could present those
projections. Useful views can emerge from workbench expressions before becoming
dedicated UI. Runtime state remains owned by the backend; views are snapshots.

## Boundaries that keep this useful

- Preserve typed handles, principals, lifecycle distinctions, request settlement,
  and authorized effects. JSON identifiers cannot manufacture authority.
- Use JSON freely for documents, external results, configuration, fixtures, and
  heterogeneous task data. Promote consequential fields into validated decision
  inputs where useful; do not hide control flow in rendered diagnostic strings.
- Make expected shape/cardinality convenient to assert. An optic finding nothing
  can mean an empty result or a mistaken path. Distinguish missing, null, wrong
  type, and unexpected multiplicity when acceptance or actions depend on it.
- Treat declared schemas as contracts to validate against where needed, not as
  evidence that every received payload already conforms.
- Pure updates change retained values, not external systems. Applying a patch or
  mutating a running resource still requires the owning effect and its authority.
- Retain originals and use small projections. JSON access should reduce context
  consumption rather than encourage dumping whole datasets into model history.

## A useful first slice, when this work is selected

Use a real shoal-repl event trace to connect retention, optic queries, fixture
generation, and executable acceptance. Inspect the existing JSON/schema/optic
owners and select one production consumer before designing helpers. Commit a
small interface and example scaffold; split independent mock, implementation,
consumer, and test obligations only where that boundary helps.

Verify the actual compositions through Tidepool, including missing/wrong-shaped
data and a useful failing acceptance result. This is focused improvement through
real work, not a broad experiment campaign or a universal schema framework.

The longer-term direction is gradual replacement of repeated Python/Bash glue
with persistent, composable Haskell operations. Existing programs can remain
behind effectful interfaces; move work inward when doing so improves reuse,
composition, evidence, or interaction cost.
