# GHC search integration obligation

Lookup-owned source supplies:

- `normalizeLookupWildcards :: ParsedModule -> ParsedModule`;
- `searchTypeMatches :: GlobalRdrEnv -> Name -> Type -> m [TypeMatch]`
  (the `Name` excludes the reserved query binder from its own results);
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

The GHC 9.12 matrix in `probes/MatcherMatrix.hs` supports the current
conservative rule:

```haskell
null queryPredicates
  && null candidatePredicates
  && (tcMatchTy queryBody candidateBody
      <|> tcMatchTy candidateBody queryBody)
```

Full-sigma `tcMatchTy`/`tcUnifyTy` matched only alpha-equivalent polymorphic
types in the tested specialization cases. Body matching admitted useful
specialization in one direction and a more-general candidate in the other.
Predicates are therefore retained as a gate: only full-type alpha equality is
accepted when either side is constrained. Nested foralls stay in the body and
must match there; a rank-2 query does not match a monomorphic argument.
