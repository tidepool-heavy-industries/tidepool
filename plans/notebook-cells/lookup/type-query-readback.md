# Lookup execution readback

Source: `0db96b2b6979e369a44e62da45fdde3f297e56b6`.

## Verified current boundary

`tidepool-actor/src/resident_workbench.rs::actor_compile_view` is the authority
for the exact session modules, injected current value generations, preamble and
imports visible to the next workbench submission. `inspect_compile_view` passes
that immutable view to
`tidepool-runtime/src/session/inspection.rs::run_inspections`. The runtime starts
one extractor command for a batch, but writes one `Expr.hs` per query.
`haskell/app/Main.hs::runInspectionMode` compiles each file independently and
then calls `Tidepool.Introspection.runInspection`.

The typechecked inspection target already supplies both values needed for search:
`prTargetRdrEnv` is the target `tcg_rdr_env`, and `prHscEnv` is the finalized GHC
session. Name inspection enumerates `globalRdrEnvElts`, resolves `TyThing`s with
`getInfo`, and renders declarations with `pprTyThingInContext`. This is the
authoritative searchable set for this release: things the next cell can name,
including its imported current `Val.G` generations and excluding unimported or
shadowed generations. Tests in `tidepool-runtime/src/session/inspection.rs`
already cover name ambiguity, qualified aliases, type display and browsing, but
do not identify live-binding provenance.

The existing wire is:

```
InspectionQuery (Rust)
  -> tidepool-extract-cmd request flags
  -> Tidepool.ExtractRequest.InspectionRequest
  -> Main.runInspectionMode
  -> Tidepool.Introspection.InspectionResult
  -> TPINSP002 CBOR
  -> tidepool-runtime InspectionResult
  -> resident_workbench rendering
```

The hosted boundary is not a configured Haskell handler. Built-in `haskell` is
declared and specially dispatched by
`tidepool-actor/src/resident_interactive.rs`; all other projected tools currently
become `WorkbenchRequest::for_tool` and run a named Haskell handler. A dedicated
lookup function therefore needs an explicitly owned actor-side path to the
immutable compile view. It must not smuggle colon commands into a cell or
reimplement scope assembly in the frontend.

## Feasibility result

`probes/TypeQuery.hs` executed on the repository's GHC 9.12.2. It:

1. parses and typechecks a real `Expr` module;
2. enumerates its `tcg_rdr_env`;
3. installs import declarations as an interactive context;
4. resolves `forall a. a -> Maybe a` once with `typeKind`;
5. resolves candidate `Id` types from the same reader environment; and
6. finds the visible `useful` using `tcMatchTy`, without candidate compilation.

Observed:

```
query=forall a. a -> Maybe a
visible=268
match=Just "useful"
```

`IIModule Expr` failed because the ordinary compiled target is not interpreted.
`IIDecl` imports worked, but reproducing every preamble import as interactive
context would create a second scope-assembly mechanism. Production should instead
synthesize all batch type queries as signature binders in the same inspection
module (with per-query line pragmas), harvest their checked `Type`s once, then
match the target reader environment in memory. This reuses the scope already
proved by name inspection and changes today's N compiled query files to one
compiled lookup module per batch.

Anonymous holes are not match variables. With `PartialTypeSignatures`,
`typeKind "_ -> Maybe _"` produced:

```
ZonkAny 0 -> Maybe (ZonkAny 1)
```

and `tcMatchTy` did not match `useful`. The public query parser must transform
type syntax before checking: retain repeated named variables as the same
variable, replace each anonymous `_` with a distinct fresh variable, and
explicitly quantify all query variables. Do this over GHC-parsed syntax/tokens,
not string substitution. The checked explicit query then provides template
variables to `tcMatchTy`. This remains an implementation obligation; the probe
proves both the viable typed path and why raw partial signatures are wrong.

## Shared contract before fan-out

The lookup lead retains the end-to-end contract and integrated GHC matching:

```
LookupRequest { queries :: nonempty ordered [Text] }
LookupResult  { query, outcome }
outcome = found [LookupEntry] | ambiguous [LookupEntry]
        | not_found | rejected Diagnostic
LookupEntry = name, defining module, kind, rendered declaration/signature,
              origin (module export | live binding)
```

Results stay ordered and echo the query; one rejected query does not hide other
results. Exact matches sort first, followed by deterministic useful polymorphic
matches; cap and truncation are explicit. Canonical hosted input is the structured
`queries` list. The present `HostedTool::Function` acceptance boundary does not
support a raw string simultaneously, so raw-string convenience is out of this
release unless that owner demonstrates a compatible declaration.

The parent first lands the query/result types, one-module batch worker request,
CBOR version decision, and shared fixtures. From that source it recursively forks:

- **GHC search child:** query syntax normalization, batch synthesis/type harvest,
  candidate matching, limits and deterministic ranking.
- **Hosted surface child:** `lookup` declaration/schema, actor-local dispatch,
  structured result presentation and partial-batch errors.
- **Corpus/acceptance child (later):** realistic hot-set queries and hosted
  query-result-next-cell fixtures after both implementations integrate.

The parent retains worker/runtime/actor wiring, authoritative scope preservation,
live-binding provenance, merge conflict resolution and the actual hosted
query-to-next-cell proof.

## First failing-today acceptance

Through the hosted tool:

```json
{"queries":[":: forall r. Response r -> Await r"]}
```

returns `awaitSettled` (or the actual checked usable surface match), and the model
uses the returned name in its next Haskell cell. The fixture must also cover a
query containing independent `_` holes, repeated named variables, an ambiguous
name and one rejected query in a successful batch.

## Open implementation questions

1. Whether `tcMatchTy` alone gives the desired directional/subsumption behavior
   for constrained and rank-polymorphic candidates. Decide from a small corpus
   built from the shipped hot-set and recent inspection traffic, not compiler
   terminology.
2. Which existing actor request owns direct immutable inspection so lookup
   preserves execution IDs, retries and machine checkout without producing a
   fake workbench cell receipt. The actor lead must verify the public hosted
   response boundary.
3. How current `Val.G` names are marked as live bindings. The compile view knows
   injected/current modules; preserve that identity through the worker result
   rather than infer it from rendered names.
4. Whether the wire can evolve `TPINSP002` in place or requires `TPINSP003`.
   Serialized shape requires an explicit migration decision.
