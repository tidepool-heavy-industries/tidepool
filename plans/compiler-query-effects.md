# Compiler-query effects

Status: accepted direction, refreshed 2026-10-19 against committed integration
HEAD `c830d1f585` (schema `9329e7f216`, generated protocol snapshots
`c830d1f585`). The schema in `bridge/protocol/src/effects/introspection.rs` is
authoritative; the runtime projection is in
`tidepool/runtime/src/session/inspection.rs`. This document states the contract,
not that every consumer is wired. No native build or execution was run for this
revision.

Citation basis: every line below is validated against committed integration HEAD
`c830d1f585`. The schema source was last changed by `9329e7f216`, which
reordered `nameEntryReference`/`typeMatchReference` after their `*References`
fields and shifted line numbers from the earlier `bc58326f30` correction; the
generated snapshots were committed by `c830d1f585`. Cite the committed blob, not
the dirty worktree.

## Decision

There is one extended `Introspection` effect, not a parallel compiler-query
family. `Introspection` is the migrated effect registered in
`bridge/protocol/src/effects/mod.rs:84`. It declares its own actor handler
(`bridge/protocol/src/effects/introspection.rs:12-33`; `verb_with` sets
`HandlingClass::Actor` at `bridge/protocol/src/effects/introspection.rs:189-198`)
and owns the name-query, type, declaration, provenance, and error records
(`bridge/protocol/src/effects/introspection.rs:208-504`).

The extension adds six verbs to that effect: `currentScopeView`,
`scopeBindings`, `moduleExports`, `rowFit`, `typeOfExpression`, and
`typeSearch`. Their constructors and return types are declared at
`bridge/protocol/src/effects/introspection.rs:46-109`; their generated helper
signatures are fixed by the schema test at
`bridge/protocol/src/effects/introspection.rs:603-619`.

The existing `info` and `typeOf` verbs remain part of the same effect
(`bridge/protocol/src/effects/introspection.rs:34-45`). This surface replaces a
separate stringly-typed batch discovery effect; it does not define one here.

## Surface

Every verb returns `Either QueryError` with its result type: `verb_with` builds
the return as `Either QueryError ok` and marks it actor-handled
(`bridge/protocol/src/effects/introspection.rs:189-198`).

| Verb | Signature | Evidence |
| --- | --- | --- |
| `currentScopeView` | `Eff effs (Either QueryError ScopeProvenance)` | `bridge/protocol/src/effects/introspection.rs:46-51, 587, 612` |
| `scopeBindings` | `Maybe ScopeProvenance -> Eff effs (Either QueryError NameList)` | `bridge/protocol/src/effects/introspection.rs:52-57, 588, 613` |
| `moduleExports` | `Maybe ScopeProvenance -> Text -> Eff effs (Either QueryError NameList)` | `bridge/protocol/src/effects/introspection.rs:58-70, 589, 614` |
| `rowFit` | `Maybe ScopeProvenance -> IdentifierRef -> Eff effs (Either QueryError Fit)` | `bridge/protocol/src/effects/introspection.rs:71-83, 590, 615` |
| `typeOfExpression` | `Maybe ScopeProvenance -> Text -> Eff effs (Either QueryError ExpressionType)` | `bridge/protocol/src/effects/introspection.rs:84-96, 591, 616` |
| `typeSearch` | `Maybe ScopeProvenance -> Text -> Eff effs (Either QueryError TypeMatches)` | `bridge/protocol/src/effects/introspection.rs:97-109, 592, 617` |

The shared first argument is `expectedScope :: Maybe ScopeProvenance`
(`bridge/protocol/src/effects/introspection.rs:200-206`).

### Pin rule

The five query verbs take `Maybe ScopeProvenance` first; `currentScopeView` is
nullary and returns the provenance a caller may pin
(`bridge/protocol/src/effects/introspection.rs:46-109, 200-206`).

- `Nothing` observes the actor's current compile view. It is not a promise that
  the view will stay current while the query runs.
- `Just p` is a pin. If `p` no longer names the actor's current compile view,
  the query is refused with `ScopeChanged p current` rather than answered from
  a different view (`bridge/protocol/src/effects/introspection.rs:133-167`; the
  rule is restated in each helper doc, e.g. lines 137, 144, 151, 158, and 165).

Independently of the pin, the answer is checked after the compiler replies. The
actor captures the compile-view provenance before the query, recomputes it
afterward, and returns `ScopeChanged(before, after)` if the compile view moved,
instead of returning the stale answer
(`exomonad/actor/src/resident_workbench.rs:14498-14509`; projection at
`exomonad/actor/src/resident_workbench/structured_introspection.rs:275-284,
300-305`). Thus entry-versus-pre-reply drift is refused for **every** query,
including a `Nothing` query that supplied no expected scope.

## Result types

The schema defines these result shapes. Field order and optionality are part of
the Haskell wire contract, and the schema test pins the rendered declarations
(`bridge/protocol/src/effects/introspection.rs:621-640`).

```haskell
data NameEntry = NameEntry
  { nameEntryName         :: Text
  , nameEntryModule       :: Maybe Text
  , nameEntryKind         :: Text
  , nameEntryDisplay      :: Text
  , nameEntryAvailability :: FitAvailability
  , nameEntryReferences   :: [IdentifierRef]
  , nameEntryReference    :: IdentifierRef
  }

data NameList = NameList
  { entries            :: [NameEntry]
  , namesObserved      :: Int
  , namesLimit         :: Int
  , nameListProvenance :: ScopeProvenance
  }

data FitAvailability = FitAvailable | FitPolymorphic | FitUnavailable | FitUnknown

data Fit = Fit
  { fitAvailability :: FitAvailability
  , fitIdentifier   :: IdentifierRef
  , fitProvenance   :: ScopeProvenance
  }

data ExpressionType = ExpressionType
  { expressionSource       :: Text
  , expressionDisplay      :: Text
  , expressionAvailability :: FitAvailability
  , expressionProvenance   :: ScopeProvenance
  }

data TypeMatchQuality = TypeMatchExact | TypeMatchUsable

data TypeMatch = TypeMatch
  { typeMatchName         :: Text
  , typeMatchModule       :: Maybe Text
  , typeMatchSignature    :: Text
  , typeMatchQuality      :: TypeMatchQuality
  , typeMatchAvailability :: FitAvailability
  , typeMatchReferences   :: [IdentifierRef]
  , typeMatchReference    :: IdentifierRef
  }

data TypeMatches = TypeMatches
  { typeMatches            :: [TypeMatch]
  , typeMatchesObserved    :: Int
  , typeMatchesLimit       :: Int
  , typeMatchesProvenance  :: ScopeProvenance
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

Two distinctions are load-bearing:

- `FitAvailability` is the closed four-way outcome enum. It is the type of
  every per-entry availability field (`nameEntryAvailability`,
  `expressionAvailability`, `typeMatchAvailability`). `Fit` is not that enum:
  it is the `rowFit` result record pairing an availability with the identifier
  it measured and the compile view it was measured in. Do not render `Fit`
  where the schema says `FitAvailability`.
- `nameEntryReference`/`typeMatchReference` are each entry's canonical resolved
  identity, suitable to chain into `info`, `typeOf`, or `rowFit`.
  `nameEntryReferences`/`typeMatchReferences` are dependencies the declaration
  mentions or resolves, not the entry's own identity.

Evidence for each shape:

- `NameEntry` carries the availability, dependency references, and canonical
  reference shown: `bridge/protocol/src/effects/introspection.rs:357-377`;
  generated text `bridge/mcp/src/generated/introspection.rs:35`.
- `NameList` adds `nameListProvenance` beside the observed and retained counts:
  `bridge/protocol/src/effects/introspection.rs:378-388`; generated text
  `bridge/mcp/src/generated/introspection.rs:36`.
- `FitAvailability` has exactly the four constructors shown:
  `bridge/protocol/src/effects/introspection.rs:388-396`; generated text
  `bridge/mcp/src/generated/introspection.rs:37`.
- `Fit` is the record `{fitAvailability, fitIdentifier, fitProvenance}`:
  `bridge/protocol/src/effects/introspection.rs:397-405`; generated text
  `bridge/mcp/src/generated/introspection.rs:38`.
- `ExpressionType` carries source, rendered display, row availability, and
  compile-view provenance: `bridge/protocol/src/effects/introspection.rs:406-415`;
  generated text `bridge/mcp/src/generated/introspection.rs:39`.
- `TypeMatchQuality`, `TypeMatch`, and `TypeMatches` use the fields shown; a
  type match carries a rendered signature plus its canonical reference and
  dependency references, not a `TypeExpression`:
  `bridge/protocol/src/effects/introspection.rs:416-447`; generated text
  `bridge/mcp/src/generated/introspection.rs:40-42`.
- `DiagnosticSpan`, `DiagnosticSeverity`, and `Diagnostic` are as shown:
  `bridge/protocol/src/effects/introspection.rs:448-474`; generated text
  `bridge/mcp/src/generated/introspection.rs:43-45`.
- `QueryError` keeps the pre-existing variants and adds
  `ScopeChanged ScopeProvenance ScopeProvenance` and
  `ExpressionRejected [Diagnostic]`
  (`bridge/protocol/src/effects/introspection.rs:475-502`; variants at
  `489-499`). The schema test asserts both names
  (`bridge/protocol/src/effects/introspection.rs:656-670`).

`TypeExpression` is still the canonical type payload used by `TypeInfo` and
declaration records, with `typeCanonical`, `typeVariables`, and
`typeConstraints` (`bridge/protocol/src/effects/introspection.rs:250-258`;
`tidepool/runtime/src/session/inspection.rs:79-84`;
`bridge/haskell/src/Tidepool/Introspection.hs:181-186`). It is **not** the
result of `typeOfExpression`; `ExpressionType` is the separate record above.

## Compile-view provenance and staleness

The accepted ruling is compile-view provenance only. `ScopeProvenance` has a
scope, a publication generation, and a content fingerprint
(`bridge/protocol/src/effects/introspection.rs:241-249`;
`tidepool/runtime/src/session/inspection.rs:57-62`). It describes the immutable
compile view an answer was computed against, not a compiler-world identity.

The actor builds provenance from the `ActorCompileView` it actually compiled
from (`exomonad/actor/src/mount.rs:731-743`):

- `generation` is the compile view's value generation
  (`exomonad/actor/src/resident_workbench.rs:14555`;
  `exomonad/actor/src/mount.rs:835-837`). The published tip for a compile view
  is exposed by `CompileViewKey::generation()`
  (`tidepool/runtime/src/session/persistent.rs:312-323`).
- `fingerprint` is a content digest over the prepared preamble, imports,
  generation, include paths, and injected modules, finalized into
  `ScopeProvenance { scope, generation, fingerprint }`
  (`exomonad/actor/src/resident_workbench.rs:14556-14572`). It commits to the
  exact source/import view, so two answers with the same scope and generation
  but different content do not compare equal.

`ScopeChanged` is the stale-view refusal. It carries the provenance the caller
pinned or that was captured before the query (`used`/`before`) and the
provenance now current (`current`/`after`)
(`bridge/protocol/src/effects/introspection.rs:489-495`; actor comparison
`exomonad/actor/src/resident_workbench.rs:14498-14509`; typed projection
`exomonad/actor/src/resident_workbench/structured_introspection.rs:275-284,
300-305`). A stale view must never become an empty success.

There is deliberately no synthesized compiler-world identity in this contract.
`ActorCompileView` exposes the session compile view, imports, source roots, and
compile inputs, but has no compiler-world accessor
(`exomonad/actor/src/mount.rs:731-743`,
`exomonad/actor/src/mount.rs:745-1033`). The adjacent public-module world-inspection path still builds a
`compiler-world:<hex(logical_id)>` *display* fingerprint
(`tidepool/runtime/src/session/inspection.rs:608-619`). That is a runtime
observation of a private world base, not the compile-view fingerprint, and an
actor query cannot obtain such an id. Substituting it (or any constant) would
make `ScopeChanged` compare a value that does not track the compile view, so the
refusal would be meaningless. Correlating an answer with a compiler-world
publication identity — and attaching that identity to actor answers — is a
separate runtime-owner follow-up, not current compiler-query behavior.

## Enumeration and row fit

- Enumeration must not compile a checked target. `scopeBindings` and
  `moduleExports` are specified as target-free enumeration
  (`bridge/protocol/src/effects/introspection.rs:133-167`); `moduleExports`
  also keeps generated session modules private
  (`bridge/protocol/src/effects/introspection.rs:144`). The current runtime
  routes `ScopeBrowse` and `Browse` without adding a probe expression to the
  generated query source (`tidepool/runtime/src/session/inspection.rs:856-864`)
  and dispatches them directly
  (`tidepool/runtime/src/session/inspection.rs:890-895`). A mixed batch that
  also contains `info` currently shares one checked target
  (`tidepool/runtime/src/session/inspection.rs:1964-2008`), so
  enumeration-only zero-target behavior remains an acceptance item.
- `rowFit` is an observation against the executing actor's row, not a runtime
  resource grant (`bridge/protocol/src/effects/introspection.rs:151`). Its
  result is the `Fit` record above, and its availability component must come
  from typed evidence, never rendered text: `AvailabilityContext` carries the
  `Eff` type constructor and row
  (`bridge/haskell/src/Tidepool/Introspection.hs:240-246`); the row is read from
  the sentinel's typed `Proxy` and rendered text never determines it
  (`bridge/haskell/src/Tidepool/Introspection.hs:285-307`);
  `availabilityFor` compares the rows and solves predicates
  (`bridge/haskell/src/Tidepool/Introspection.hs:358-395`);
  `signatureAvailability` splits the sigma type
  (`bridge/haskell/src/Tidepool/Introspection.hs:409-412`); and
  `thingAvailability` uses that evidence for values, returning `Unknown` when it
  has none (`bridge/haskell/src/Tidepool/Introspection.hs:1013-1017`). Missing
  evidence yields `FitUnknown`, never an inferred success.
- `typeOfExpression` and `typeSearch` compile an explicit probe but never run
  it (`bridge/protocol/src/effects/introspection.rs:154-167`). The existing
  runtime queries are `InspectionQuery::TypeOf` and
  `InspectionQuery::TypeSearch`
  (`tidepool/runtime/src/session/inspection.rs:18-20, 856-875, 887-889`); the
  `TypeOf` inspection reads the probe's typed identifier rather than evaluating
  the expression (`bridge/haskell/src/Tidepool/Introspection.hs:586-593`).

## Mapping to current runtime paths

The schema is authoritative; these are the existing projection points a
consumer should reuse.

| Verb | Current runtime path | Cost |
| --- | --- | --- |
| `currentScopeView` | No query; build compile-view provenance with `structured_provenance` (`exomonad/actor/src/resident_workbench.rs:14545-14572`) | none |
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
  (`bridge/protocol/src/effects/introspection.rs:232-240`;
  `tidepool/runtime/src/session/inspection.rs:72-77`). `DiagnosticSpan` exists
  for compiler diagnostics, not for every answer
  (`bridge/protocol/src/effects/introspection.rs:448-458`).
- Batching. Each verb asks one typed question and returns one `Either`
  (`bridge/protocol/src/effects/introspection.rs:189-198`); `NameList` and
  `TypeMatches` expose observed and retained counts but no continuation token
  (`bridge/protocol/src/effects/introspection.rs:378-388, 438-447`).
- A second effect per cost class. Cost is documentation and prompt guidance;
  the effect and its `Member Introspection effs` constraint stay single
  (`bridge/protocol/src/effects/introspection.rs:603-619`).

## Current implementation state

- The protocol schema is committed, and its test fixes the eight helper
  signatures and the required declarations
  (`bridge/protocol/src/effects/introspection.rs:577-671`).
- Generated protocol snapshots were committed at integration HEAD `c830d1f585`
  and now list all eight constructors with `Maybe ScopeProvenance`,
  `FitAvailability`, and the `Fit` record
  (`bridge/mcp/src/generated/introspection.rs:11-19, 35-46, 50-57`;
  actor request enum at `exomonad/actor/src/generated/introspection.rs:23-32`).
- The actor query handler module
  `exomonad/actor/src/resident_workbench/compiler_query.rs` is **not** at
  integration HEAD. It exists only on the WIP handlers branch
  (`wip/compiler-query/handlers-20261019`, currently `cf4cd87e99`).
- Runtime `InspectionQuery` at HEAD has no variants named `ScopeBindings`,
  `ModuleExports`, `RowFit`, or `TypeOfExpression`, and its `TypeSearch`
  variant is the pre-existing unpinned form
  (`tidepool/runtime/src/session/inspection.rs:16-34`). The runtime `QueryError`
  enum still lacks `ScopeChanged` and `ExpressionRejected`
  (`tidepool/runtime/src/session/inspection.rs:151-157`). The six verbs are
  schema-complete but not yet routed by the actor decode handler.
- Therefore every behavior below is an acceptance requirement, not evidence
  already produced by this checkout.

## Acceptance

- `currentScopeView` followed by `moduleExports` and `info` answers from one
  admitted view without compiling a probe; the observable measure is
  `inspection.compile_attempts` staying at zero
  (`bridge/haskell/src/Tidepool/Introspection.hs:262-272`).
- A pinned `ScopeProvenance` from before a publication is refused with
  `ScopeChanged`, never converted into an empty enumeration
  (`bridge/protocol/src/effects/introspection.rs:489-495`;
  `exomonad/actor/src/resident_workbench.rs:14498-14509`).
- Entry-versus-pre-reply drift is refused even for `Nothing` queries: the
  post-reply comparison returns `ScopeChanged(before, after)` rather than the
  answer (`exomonad/actor/src/resident_workbench.rs:14498-14509`).
- Enumeration of the current scope or a public module does not compile a
  checked target (`bridge/protocol/src/effects/introspection.rs:133-167`); a
  batch mixing enumeration with `info` is not accepted as evidence for this item
  by itself (`tidepool/runtime/src/session/inspection.rs:1964-2008`).
- `rowFit` reports `FitAvailable`, `FitPolymorphic`, `FitUnavailable`, and
  `FitUnknown` only from the typed row and solved-predicate evidence above, and
  returns the `Fit` record with the measured identifier and provenance; missing
  evidence yields `FitUnknown`, never an inferred success
  (`bridge/protocol/src/effects/introspection.rs:397-405`;
  `bridge/haskell/src/Tidepool/Introspection.hs:358-395, 409-412, 1013-1017`).
- `typeOfExpression` returns the compiled probe's `ExpressionType` and never
  evaluates the probe (`bridge/protocol/src/effects/introspection.rs:406-415,
  154-167`).
- Negatives remain typed: `UnknownIdentifier` carries the searched `NameQuery`;
  absence from one searched scope is not absence from the installed API
  (`bridge/protocol/src/effects/introspection.rs:475-502`).
