# GHC search integration obligation

Lookup-owned source supplies:

- `normalizeLookupWildcards :: ParsedModule -> ParsedModule`;
- `searchTypeMatches :: GlobalRdrEnv -> Type -> m [TypeMatch]`;
- closed exact/usable quality and deterministic result ordering;
- `introspection-search-test`, covering repeated variables, independent
  anonymous wildcards, exact/usable matching and order.

The coordinator-owned seam must connect it without widening normalization to
ordinary compilation:

1. Add a search inspection request carrying the original query text.
2. Synthesize an isolated inspection module containing a reserved signature
   binder for that query and an `undefined` equation. One module/query is
   permitted; never one module/candidate.
3. Add an explicit compilation-plan flag for lookup-query modules. In
   `GhcPipeline.compileFront`, after `parseModule` and before `typecheckModule`,
   apply `normalizeLookupWildcards` only under that flag. Do not recognize a
   magic binder name in general user source.
4. In `runInspection`, resolve the reserved checked binder from that query
   module's `GlobalRdrEnv`, obtain its `Id` type, and call
   `searchTypeMatches` against the same environment.
5. Carry `TypeMatch` through the explicit TPINSP migration and Rust decoder.
   Add origin metadata from the actor compile view/current Val.G modules; do
   not infer live provenance from rendered module strings.
6. Preserve `Main.runInspectionMode`'s per-query compile/rejection boundary so
   `[valid name, invalid type, valid type]` returns both successes.

Before incorporation, reconcile the declared Astra matching decision with the
current deliberately symmetric usable test:

```haskell
tcMatchTy queryBody candidateBody
  <|> tcMatchTy candidateBody queryBody
```

Symmetry admits a candidate more general than the requested type, which is
often useful, but constraints and higher-rank types require corpus evidence.
The coordinator/lookup parent owns any narrowed direction or quality split.
