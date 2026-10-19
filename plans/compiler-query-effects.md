# Compiler-query effects

Status: accepted direction, rewritten 2026-10-19 against integration base
`8cb2114c2d`. The schema in `bridge/protocol/src/effects/introspection.rs` is
authoritative; the runtime projection is in
`tidepool/runtime/src/session/inspection.rs`. This document states the contract,
not that every consumer is wired. No native build or execution was run for this
revision.

## Decision

There is one extended `Introspection` effect, not a parallel compiler-query
family. `Introspection` is the migrated effect registered in
`bridge/protocol/src/effects/mod.rs:84`, declares its own actor handler, and
owns the name-query, type, declaration, provenance, and error records
(`bridge/protocol/src/effects/introspection.rs:11-23`).

The extension adds six verbs to that effect:
`currentScopeView`, `scopeBindings`, `moduleExports`, `rowFit`,
`typeOfExpression`, and `typeSearch`. Their constructors and return types are
declared at `bridge/protocol/src/effects/introspection.rs:46-97`; their
generated helper signatures are fixed by the schema test at
`bridge/protocol/src/effects/introspection.rs:526-537`.

The existing `info` and `typeOf` verbs remain part of the same effect
(`bridge/protocol/src/effects/introspection.rs:34-45` and
`bridge/protocol/src/effects/introspection.rs:100-112`). This
surface replaces a separate stringly-typed batch discovery effect; it does not
define one here.

## Surface

Every verb returns `Either QueryError` with its result type
(`bridge/protocol/src/effects/introspection.rs:177-185`).

| Verb | Signature | Evidence |
| --- | --- | --- |
| `currentScopeView` | `Eff effs (Either QueryError ScopeProvenance)` | `bridge/protocol/src/effects/introspection.rs:46-51, 526-537` |
| `scopeBindings` | `Eff effs (Either QueryError NameList)` | `bridge/protocol/src/effects/introspection.rs:52-57, 526-537` |
| `moduleExports` | `Text -> Eff effs (Either QueryError NameList)` | `bridge/protocol/src/effects/introspection.rs:58-67, 526-537` |
| `rowFit` | `IdentifierRef -> Eff effs (Either QueryError Fit)` | `bridge/protocol/src/effects/introspection.rs:68-77, 526-537` |
| `typeOfExpression` | `Text -> Eff effs (Either QueryError ExpressionType)` | `bridge/protocol/src/effects/introspection.rs:78-87, 526-537` |
| `typeSearch` | `Text -> Eff effs (Either QueryError TypeMatches)` | `bridge/protocol/src/effects/introspection.rs:88-97, 526-537` |

The helper docs also state the intended cost boundary: `scopeBindings` and
`moduleExports` enumerate "without checking a target"
(`bridge/protocol/src/effects/introspection.rs:121-134`), while
`typeOfExpression` and `typeSearch` compile a probe and never evaluate it
(`bridge/protocol/src/effects/introspection.rs:142-154`).

## Result types

The schema defines these result shapes. Field order and optionality are part of
the wire contract.

```haskell
data NameEntry = NameEntry
  { nameEntryName         :: Text
  , nameEntryModule       :: Maybe Text
  , nameEntryKind         :: Text
  , nameEntryDisplay      :: Text
  , nameEntryAvailability :: Fit
  , nameEntryReferences   :: [IdentifierRef]
  }

data NameList = NameList
  { entries        :: [NameEntry]
  , namesObserved  :: Int
  , namesLimit     :: Int
  }

data Fit = FitAvailable | FitPolymorphic | FitUnavailable | FitUnknown

data ExpressionType = ExpressionType
  { expressionSource       :: Text
  , expressionDisplay      :: Text
  , expressionAvailability :: Fit
  , expressionProvenance   :: ScopeProvenance
  }

data TypeMatchQuality = TypeMatchExact | TypeMatchUsable

data TypeMatch = TypeMatch
  { typeMatchName         :: Text
  , typeMatchModule       :: Maybe Text
  , typeMatchSignature    :: Text
  , typeMatchQuality      :: TypeMatchQuality
  , typeMatchAvailability :: Fit
  , typeMatchReferences   :: [IdentifierRef]
  }

data TypeMatches = TypeMatches
  { typeMatches          :: [TypeMatch]
  , typeMatchesObserved  :: Int
  , typeMatchesLimit     :: Int
  }

data DiagnosticSpan = DiagnosticSpan
  { spanFile      :: Text
  , spanStartLine :: Int
  , spanStartCol  :: Int
  , spanEndLine   :: Int
  , spanEndCol    :: Int
  }

data DiagnosticSeverity = DiagnosticError | DiagnosticWarning

data Diagnostic = Diagnostic
  { diagnosticSpan     :: Maybe DiagnosticSpan
  , diagnosticSeverity :: DiagnosticSeverity
  , diagnosticMessage  :: Text
  }
```

Evidence for each shape:

- `NameEntry` is the enumeration entry, not a bare identifier:
  `bridge/protocol/src/effects/introspection.rs:337-350`.
- `NameList` carries `entries`, `namesObserved`, and `namesLimit`; the count
  observed before admission is separate from the retained entries:
  `bridge/protocol/src/effects/introspection.rs:352-359`.
- `Fit` has exactly the four constructors shown:
  `bridge/protocol/src/effects/introspection.rs:361-369`; the runtime names the
  same states in `tidepool/runtime/src/session/inspection.rs:169-180`.
- `ExpressionType` carries source, rendered display, row availability, and
  compile-view provenance:
  `bridge/protocol/src/effects/introspection.rs:370-378`.
- `TypeMatchQuality`, `TypeMatch`, and `TypeMatches` use the fields shown; a
  type match carries a rendered signature and references, not a
  `TypeExpression`:
  `bridge/protocol/src/effects/introspection.rs:380-403`.
- `DiagnosticSpan`, `DiagnosticSeverity`, and `Diagnostic` are as shown:
  `bridge/protocol/src/effects/introspection.rs:405-430`.
- `QueryError` includes `ScopeChanged ScopeProvenance ScopeProvenance` and
  `ExpressionRejected [Diagnostic]` in addition to the pre-existing error
  variants:
  `bridge/protocol/src/effects/introspection.rs:432-458`.

`TypeExpression` remains the canonical type payload used by `TypeInfo` and
declaration records, with `typeCanonical`, `typeVariables`, and
`typeConstraints`
(`bridge/protocol/src/effects/introspection.rs:230-247`;
`tidepool/runtime/src/session/inspection.rs:79-91`;
`bridge/haskell/src/Tidepool/Introspection.hs:181-193`). It is not the result
of `typeOfExpression`; `ExpressionType` is the separate record above.

## Per-world provenance and staleness

`ScopeProvenance` has a scope, a publication generation, and a fingerprint
(`bridge/protocol/src/effects/introspection.rs:221-228`;
`tidepool/runtime/src/session/inspection.rs:57-62`). Runtime inspection builds
it per admitted world:

- A returned world whose `logical_id()` differs from the admitted published
  world is rejected, so an answer cannot be attached to another world
  (`tidepool/runtime/src/session/inspection.rs:582-604`).
- `generation` is the admitted compile-view key's publication generation
  (`tidepool/runtime/src/session/inspection.rs:608-610`); `CompileViewKey`'s
  `generation()` returns its declaration `tip`
  (`tidepool/runtime/src/session/persistent.rs:312-323`).
- `fingerprint` is runtime-owned: the `compiler-world:` prefix followed by the
  lowercase hexadecimal bytes of `CompilerWorld::logical_id()`
  (`tidepool/runtime/src/session/inspection.rs:611-618`). The compiler-supplied
  payload must carry generation `0` and an empty fingerprint, so the runtime
  attaches the authoritative observation rather than trusting the service
  (`tidepool/runtime/src/session/inspection.rs:685-697`).

`ScopeChanged` is the stale-view refusal: the error carries the provenance the
caller used and the provenance now current
(`bridge/protocol/src/effects/introspection.rs:446-452`). The actor compares the
two before returning any answer and emits `ScopeChanged` instead of an empty
success (`exomonad/actor/src/resident_workbench.rs:14498-14509`); the typed
answer projection emits the same constructor
(`exomonad/actor/src/resident_workbench/structured_introspection.rs:275-305`).

## Enumeration and row fit

- Enumeration must not compile a checked target. `scopeBindings` and
  `moduleExports` are specified as target-free enumeration
  (`bridge/protocol/src/effects/introspection.rs:121-134`); `moduleExports`
  also keeps generated session modules private
  (`bridge/protocol/src/effects/introspection.rs:132`). The current runtime
  routes `ScopeBrowse` and `Browse` without adding a probe expression to the
  generated query source (`tidepool/runtime/src/session/inspection.rs:856-864`)
  and dispatches them directly
  (`tidepool/runtime/src/session/inspection.rs:890-895`). A mixed batch that
  also contains `info` currently shares one checked target
  (`tidepool/runtime/src/session/inspection.rs:1964-2008`), so
  enumeration-only zero-target behavior remains an acceptance item.
- `rowFit` is an observation against the executing actor's row, not a runtime
  resource grant (`bridge/protocol/src/effects/introspection.rs:135-140`).
  Availability must come from typed availability evidence, never from rendered
  text: `AvailabilityContext` carries the `Eff` type constructor and row
  (`bridge/haskell/src/Tidepool/Introspection.hs:240-246`); the row is read
  from the sentinel's typed `Proxy` and rendered text never determines it
  (`bridge/haskell/src/Tidepool/Introspection.hs:285-307`);
  `availabilityFor` compares the rows and solves predicates
  (`bridge/haskell/src/Tidepool/Introspection.hs:358-395`);
  `signatureAvailability` splits the sigma type
  (`bridge/haskell/src/Tidepool/Introspection.hs:409-412`); and
  `thingAvailability` uses that evidence for values, returning `Unknown` when
  it has none (`bridge/haskell/src/Tidepool/Introspection.hs:1013-1017`).
- `typeOfExpression` and `typeSearch` compile an explicit probe but never run
  it (`bridge/protocol/src/effects/introspection.rs:142-154`). The existing
  runtime queries are `InspectionQuery::TypeOf` and
  `InspectionQuery::TypeSearch`
  (`tidepool/runtime/src/session/inspection.rs:18-20, 856-875, 887-889`); the
  `TypeOf` inspection reads the probe's typed identifier rather than
  evaluating the expression
  (`bridge/haskell/src/Tidepool/Introspection.hs:586-593`).

## Mapping to current runtime paths

The schema is authoritative; these are the existing projection points a
consumer should reuse.

| Verb | Current runtime path | Cost |
| --- | --- | --- |
| `currentScopeView` | No query; build provenance from the admitted world (`tidepool/runtime/src/session/inspection.rs:608-618`) | none |
| `scopeBindings` | `InspectionQuery::ScopeBrowse` (`tidepool/runtime/src/session/inspection.rs:21, 890-892`) to `InspectScope` (`bridge/haskell/src/Tidepool/Introspection.hs:597-602`) | enumeration |
| `moduleExports` | `InspectionQuery::Browse { expanded: false }` (`tidepool/runtime/src/session/inspection.rs:22-25, 893-895`) to `InspectModule` (`bridge/haskell/src/Tidepool/Introspection.hs:603-605, 885-901`) | interface load |
| `info` | `InspectionQuery::StructuredInfo` (`tidepool/runtime/src/session/inspection.rs:26-29, 896-898`) | interface load |
| `typeOf` | `InspectionQuery::StructuredType` (`tidepool/runtime/src/session/inspection.rs:30-33, 899-901`) | interface load |
| `rowFit` | `signatureAvailability` / `thingAvailability` (`bridge/haskell/src/Tidepool/Introspection.hs:409-412, 1013-1017`) | solver |
| `typeOfExpression` | `InspectionQuery::TypeOf` (`tidepool/runtime/src/session/inspection.rs:18, 857, 881-883`) | probe compile |
| `typeSearch` | `InspectionQuery::TypeSearch` (`tidepool/runtime/src/session/inspection.rs:20, 871-875, 887-889`) | probe compile |

## Not in this surface

- Defining source spans. `IdentifierRef` carries only `identifierModule`,
  `identifierName`, and `identifierNamespace`
  (`bridge/protocol/src/effects/introspection.rs:212-219`;
  `tidepool/runtime/src/session/inspection.rs:72-77`). `DiagnosticSpan` exists
  for compiler diagnostics, not for every answer
  (`bridge/protocol/src/effects/introspection.rs:405-415`).
- Batching. Each verb asks one typed question and returns one `Either`
  (`bridge/protocol/src/effects/introspection.rs:177-185`); `NameList` and
  `TypeMatches` expose observed and retained counts but no continuation token
  (`bridge/protocol/src/effects/introspection.rs:352-359, 396-403`).
- A second effect per cost class. Cost is documentation and prompt guidance;
  the effect and its `Member Introspection effs` constraint stay single
  (`bridge/protocol/src/effects/introspection.rs:526-537`).

## Current implementation state

- The protocol schema test fixes the eight helper signatures and the required
  declarations (`bridge/protocol/src/effects/introspection.rs:521-613`).
- The generated snapshot at `bridge/mcp/src/generated/introspection.rs:11-35`
  still lists only `info` and `typeOf`, and its `QueryError` line omits
  `ExpressionRejected` (`bridge/mcp/src/generated/introspection.rs:29`).
  Regeneration is pending; this source-only revision did not run it.
- The runtime `InspectionQuery` enum has no variants named `ScopeBindings`,
  `ModuleExports`, `RowFit`, `TypeOfExpression`, or `TypeSearch`
  (`tidepool/runtime/src/session/inspection.rs:16-34`); the six verbs are
  schema-complete but not yet routed by the actor decode handler. The runtime
  `QueryError` enum also lacks `ScopeChanged` and `ExpressionRejected`
  (`tidepool/runtime/src/session/inspection.rs:151-157`), while the actor
  answer enum represents `ScopeChanged` explicitly
  (`exomonad/actor/src/resident_workbench/structured_introspection.rs:275-284`).
- Therefore every behavior below is an acceptance requirement, not evidence
  already produced by this checkout.

## Acceptance

- `currentScopeView` followed by `moduleExports` and `info` answers from one
  admitted view without compiling a probe; the observable measure is
  `inspection.compile_attempts` staying at zero
  (`bridge/haskell/src/Tidepool/Introspection.hs:262-272`).
- A pinned `ScopeProvenance` from before a publication is refused with
  `ScopeChanged`, never converted into an empty enumeration
  (`bridge/protocol/src/effects/introspection.rs:446-452`;
  `exomonad/actor/src/resident_workbench.rs:14503-14509`).
- Enumeration of the current scope or a public module does not compile a
  checked target (`bridge/protocol/src/effects/introspection.rs:121-134`);
  a batch mixing enumeration with `info` is not accepted as evidence for this
  item by itself (`tidepool/runtime/src/session/inspection.rs:1964-2008`).
- `rowFit` reports `FitAvailable`, `FitPolymorphic`, `FitUnavailable`, and
  `FitUnknown` only from the typed row and solved-predicate evidence above;
  missing evidence yields `FitUnknown`, never an inferred success
  (`bridge/haskell/src/Tidepool/Introspection.hs:358-395, 1013-1017`).
- `typeOfExpression` returns the compiled probe's `ExpressionType` and never
  evaluates the probe (`bridge/protocol/src/effects/introspection.rs:370-378,
  142-154`).
- Negatives remain typed: `UnknownIdentifier` carries the searched `NameQuery`;
  absence from one searched scope is not absence from the installed API
  (`bridge/protocol/src/effects/introspection.rs:432-458`).
