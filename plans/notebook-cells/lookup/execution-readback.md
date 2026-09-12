# Lookup execution readback

## Verified current boundary

`tidepool-runtime/src/session/inspection.rs` owns transport-neutral
`InspectionQuery`, `InspectionResult`, the one-worker-request batch, and receipt
decoding. It writes one assembled `Expr.hs` per query. This isolates rejection,
but type search must not compile one module per candidate.

`haskell/src/Tidepool/Introspection.hs` receives the compiled inspection
module's `HscEnv`, `GlobalRdrEnv`, and captured expression types.
`inspectName` enumerates `globalRdrEnvElts`, resolves qualified/unqualified
reader names, and renders `TyThing`s with GHC. It currently filters by an
already-known name; it does not enumerate value candidates or distinguish live
Val.G bindings in its receipt.

`tidepool-actor/src/resident_workbench.rs` builds the exact actor compile view
and invokes the batch without mutating live values. Existing `:type`, `:info`,
and `:browse` are thin consumers. `tidepool-actor/src/resident_interactive.rs`
declares only the raw `haskell` built-in. Other hosted tools are application
tools whose handlers are compiled Haskell; `WorkbenchRequest::for_tool` routes
only to those registered handlers. A dedicated actor-side lookup therefore
needs an explicit trusted inspection request path; it cannot masquerade as an
application handler.

`haskell/app/Main.hs` currently compiles one source per inspection query, then
calls `runInspection` on that compiled module. The extractor request/CBOR owners
are `haskell/src/Tidepool/ExtractRequest.hs`, `tidepool-extract-cmd`, and the
runtime decoder. Any new search query changes all those consumers and its
versioned receipt deliberately.

## Checked feasibility

`probes/TypeQuery.hs`, integrated from the bounded feasibility child, ran on
the repository's GHC 9.12.2. Against one parsed/typechecked `Expr` module it
enumerated 268 in-scope reader entries, resolved
`forall a. a -> Maybe a` once, and matched the visible polymorphic `useful`
with `tcMatchTy` without compiling candidate modules.

The same run demonstrated that raw partial-signature holes are not suitable
match variables: `_ -> Maybe _` zonked to two `ZonkAny` types and did not match
`useful`. Production must normalize the GHC-parsed query AST: preserve repeated
named variables, replace each anonymous wildcard node with a distinct fresh
variable, and explicitly quantify query variables before checking. String
substitution is excluded.

The probe's `IIModule Expr` context failed because this compiled target is not
interpreted; reproducing imports as `IIDecl`s worked but would duplicate scope
assembly. Production may synthesize normalized query signatures in inspection
modules, harvest their checked types, and match the reader environment in memory.
It must retain independent per-query rejection: a batch containing a valid name,
valid type, and syntactically valid ill-kinded or unknown-type query returns both
successes. One compile per query is acceptable; never compile per candidate.
Single-module batching is not a requirement.

## First shared interface

The lane proposes one new domain query:

```text
InspectionQuery::SearchType { source: String }
InspectionResult::SearchType {
  query: String,
  matches: [TypeMatch]
}
TypeMatch {
  name, module, kind, signature, live_binding, match_kind
}
```

Names remain `Info`; leading `::` is stripped only by the lookup adapter and
becomes `SearchType`. `match_kind` is a closed exact/usable ordering, not a
rendered-string control signal. Each query retains an independent typed
success/failure. Result limit and truncation metadata belong in the search
receipt rather than being inferred from display text.

Canonical hosted input is an object with `queries: [string]`. The output is
ordered and keyed by echoed query. Raw-string compatibility is excluded unless
the actual provider/hosted declaration can advertise and validate it. The
public result should be concise Haskell suitable for copying into the next
cell; structured internal results remain available for deterministic rendering.

The coordinator owns common built-in declaration/catalog registration. This
lane supplies the declaration schema/description and owns dispatch from that
trusted invocation to actor inspection. The notebook lane does not consume or
define lookup wire types.

## Recursive implementation frontier after review

The lookup lead retains:

- GHC scoped-type resolution and matching semantics;
- the end-to-end actor-side inspection request and hosted response;
- cross-child protocol integration, exact-scope behavior, deterministic limits,
  and the real query-to-next-cell acceptance.

After the scoped feasibility proof lands, recursively unfold:

1. **Protocol and scope child:** add the search request/result across extractor
   request encoding, Haskell CBOR, Rust decoding, candidate provenance, and
   focused round-trip/batch-isolation tests.
2. **Hosted adapter child:** implement canonical query parsing, per-query
   presentation, schema and dispatch over the trusted actor inspection path.
   Coordinate shared registration edits with the campaign coordinator.
3. **Matching coverage child:** from the parent's checked resolver/matcher,
   expand realistic polymorphism, repeated variables, independent wildcards,
   constraints, qualification, ambiguity, shadowing and deterministic ranking.

These children start from the checked resolver/matcher scaffold, not in parallel
with deciding whether GHC can resolve the query type. Each may choose local
fixtures and private helpers. None may add a second type parser, per-candidate
compiler invocation, fuzzy ranking, unimported-module index, or Haddock
dependency.

## Falsifiable first proof and checks

The first source checkpoint must execute through the inspection worker:

1. assemble the same preamble/imports/session Lib.G and injected current Val.G
   modules as the next cell;
2. resolve a query type once;
3. enumerate visible value `TyThing`s from that environment;
4. match without compiling candidate modules;
5. return a polymorphic function and use that named function in the next
   resident submission.

Focused checks then cover:

- `:: Response result -> Await (Settlement result)` finding the current
  `awaitSettled`; the assertion is not weakened to an arbitrary result;
- repeated named variables versus independent anonymous wildcards;
- exact before usable polymorphic matches and deterministic limiting;
- qualified/ambiguous names, shadowed Val.G generations, and live-binding label;
- three-query batch with one failure preserving both successes;
- malformed type query rejected without mutating or poisoning the next query;
- hosted schema and actual actor dispatch, not only CBOR or rendering tests.

## Open technical questions

1. Which GHC matcher gives Hoogle-natural directionality for argument/result
   polymorphism while preserving repeated variables and constraints?
2. What authoritative marker distinguishes a current injected live binding from
   an imported value? Module generation metadata may need to travel into the
   inspection request rather than infer provenance from rendered module names.
3. What default limit fits the hosted output budget, and how is truncation
   continued without coupling this release to notebook `last.more`?

Question 1 is a declared fresh-Astra slot if the initial matching corpus does not
settle it. Questions 2–3 are retained integration engineering for the lookup
lead.

This file is the authoritative lookup execution readback. The earlier detailed
feasibility readback was consolidated here and removed to avoid two frontiers.
