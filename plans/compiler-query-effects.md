# Compiler-query effects

Status: accepted direction, revised 2026-10-19 after adversarial design review.
Alpha software: `Lookup` is removed outright; there is no compatibility window.

## Decision

Extend the existing `Introspection` effect; do not add a parallel family.
`Introspection` already owns `NameQuery`, `NameScope`, `NameNamespace`,
`IdentifierRef`, `ScopeProvenance`, `IdentifierInfo`, `TypeInfo`,
`DeclarationInfo` and `QueryError` (including `ScopeChanged`, which is the
stale-view refusal) in `bridge/protocol/src/effects/introspection.rs`.
The work is: add typed verbs for the questions the compiler can already answer,
then delete `Lookup` — the stringly-typed batch surface layered over the same
inspection machinery.

One effect, many typed verbs, composed in Haskell. No per-question hosted tool,
no second discovery registry.

## Surface

Existing verbs (unchanged):

```haskell
info   :: NameQuery -> Eff effs (Either QueryError IdentifierInfo)
typeOf :: NameQuery -> Eff effs (Either QueryError TypeInfo)
```

Added verbs:

```haskell
-- world identity and enumeration; never checks a target
currentScopeView :: Eff effs (Either QueryError ScopeProvenance)
scopeBindings    :: Eff effs (Either QueryError NameList)
moduleExports    :: Text -> Eff effs (Either QueryError NameList)

-- solver observation against the executing actor's row
rowFit           :: IdentifierRef -> Eff effs (Either QueryError Fit)

-- explicitly compiles a probe; never evaluates
typeOfExpression :: Text -> Eff effs (Either QueryError ExpressionType)
typeSearch       :: Text -> Eff effs (Either QueryError TypeMatches)
```

Added types:

```haskell
data NameList = NameList
  { names :: [IdentifierRef], namesObserved :: Int, namesLimit :: Int }

data Fit = FitAvailable | FitPolymorphic | FitUnavailable | FitUnknown

data ExpressionType = ExpressionType
  { expressionType :: TypeExpression, expressionProvenance :: ScopeProvenance }

data TypeMatches = TypeMatches
  { typeMatches :: [TypeMatch], typeMatchesObserved :: Int, typeMatchesLimit :: Int }

data TypeMatch = TypeMatch
  { typeMatchIdentifier :: IdentifierRef
  , typeMatchType       :: TypeExpression
  , typeMatchQuality    :: TypeMatchQuality }

data QueryError = -- existing variants
  | ExpressionRejected [Diagnostic]
```

Invariants:

- Requests carry no mini-language. One verb, one question.
- Every answer carries `ScopeProvenance`, so staleness is structural; a
  `ScopeChanged` refusal is never converted into an empty success.
- Failures are typed variants; rendered text is display only.
- `NameList`/`TypeMatches` keep observed and returned counts separate. No
  continuation token in this slice.
- `info`/`typeOf`/`rowFit` never evaluate; `typeOfExpression`/`typeSearch`
  compile a generated probe but never run it.
- Row fit is an observation; runtime resource grants remain separate.
- Module and scope enumeration must not pay for a checked query target, and
  must not bypass `findModule` visibility rules.

## Mapping to existing compiler queries

| Verb | Existing query | Cost |
| --- | --- | --- |
| `currentScopeView` | world handle / `ScopeProvenance` | none |
| `scopeBindings` | `InspectionQuery::ScopeBrowse` | none |
| `moduleExports` | `InspectionQuery::Browse { expanded: false }` | interface load |
| `info` | `InspectionQuery::StructuredInfo` | interface load |
| `typeOf` | `InspectionQuery::StructuredType` | interface load |
| `rowFit` | availability path (`signatureAvailability`) | solver |
| `typeOfExpression` | `InspectionQuery::TypeOf` | probe compile |
| `typeSearch` | `InspectionQuery::TypeSearch` | probe compile |

## Not in this surface

- Defining source spans: `IdentifierRef` has no span. This slice returns
  `(module, name)`; spans land with a later occurrence index.
- Constructor/consumer neighborhoods and semantic intent search: need an index
  over `CapturedCompilerSources`, not a GHC capability. Separate owner, later.
- Batching.
- A second effect per cost class. Cost classes are documentation and prompt
  guidance, not separate `Member` constraints.

## Delivery

One integration tranche, because removing `Lookup` before its replacement works
would leave no model-facing compiler path:

1. Extend `introspection.rs` schema and generated artifacts.
2. Add the actor-side verbs, constructing the mapped `InspectionQuery` against
   the actor's current published view.
3. Delete `Lookup` (effect, decode handler, hosted tool, Haskell modules, row
   witness, prompts, fixtures).
4. Compile affected targets and run one focused front-door test: `info`,
   `moduleExports`, `typeOfExpression` through the actor path, with a
   `ScopeChanged` control.

`typeSearch` may land in the same tranche or immediately after; it is additive
and does not block deletion.

## Acceptance

- A real persona task uses `currentScopeView` → `moduleExports` → `info`
  without compiling a probe (measure: `inspection.compile_attempts` zero).
- Stale view control: a pinned `ScopeProvenance` from before a publication is
  refused with `ScopeChanged`, never silently treated as current.
- Honest negatives: `UnknownIdentifier` carries the searched `NameQuery`;
  absence from the searched scope is not absence from the installed API.
- `typeOf` on a bound thunk does not evaluate it.
- No `Lookup` symbol, prompt text, or fixture survives; `Introspection` is the
  only compiler-query surface.
