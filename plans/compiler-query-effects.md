# Compiler-query effects

Status: accepted direction, refreshed 2026-10-19 against integration code at
`c830d1f585` (schema `9329e7f216`, generated protocol snapshots `c830d1f585`;
plan commits through `6909452508`), with the handlers WIP folded in below. The schema in
`bridge/protocol/src/effects/introspection.rs` is authoritative; the runtime
projection is in `tidepool/runtime/src/session/inspection.rs`. This document
states the contract, not that every consumer is wired. No native build or
execution was run for this revision.

Citation basis: every line below is validated against committed integration code
at `c830d1f585`, unless the surrounding text names the handlers WIP. The schema
source was last changed by `9329e7f216`, which reordered
`nameEntryReference`/`typeMatchReference` after their `*References` fields and
shifted line numbers from the earlier `bc58326f30` correction; generated
snapshots were committed by `c830d1f585`. Citations to
`exomonad/actor/src/resident_workbench/compiler_query.rs`,
`exomonad/actor/src/resident_actor.rs`, and the identity changes in
`tidepool/runtime/src/session/inspection.rs` and
`bridge/haskell/src/Tidepool/Introspection.hs` are against the handlers branch
`cf4cd87e99` plus its working tree (`cf4cd87e99 + WT`); those paths are absent
or not current at integration HEAD, and their WIP line numbers can differ from
HEAD. Cite the committed blob for integration claims and the WIP tree for
handler claims. WIP working-tree citations are pinned by blob hash:
`compiler_query.rs` `1e6a5c41`, `resident_workbench.rs` `7c744a32`,
`resident_actor.rs` `a15a777d`, `inspection.rs` `bf72d0b0`, and
`Introspection.hs` `dab03861`. The WIP tree's protocol and generated snapshots
predate the identity schema, so schema citations always use integration HEAD.

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
handler captures the compile-view provenance before the query, recomputes it
afterward, and returns `ScopeChanged(before, after)` if the compile view moved,
instead of returning the stale answer. For the six new verbs this decision lives
in the handler module: `entry_scope_refusal` refuses a pin that never matched at
entry (`exomonad/actor/src/resident_workbench/compiler_query.rs:410-420`), and
`compiler_query_answer` refuses a view that moved while the query ran
(`exomonad/actor/src/resident_workbench/compiler_query.rs:423-440`). Thus
entry-versus-pre-reply drift is refused for **every** query, including a
`Nothing` query that supplied no expected scope. The older `info`/`typeOf`
structured path keeps its own pre/post comparison
(`exomonad/actor/src/resident_workbench.rs:14498-14509`; projection at
`exomonad/actor/src/resident_workbench/structured_introspection.rs:275-284,
300-305`), but that path does not cover the new verbs.

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
  mentions or resolves, not the entry's own identity. The handler fills the
  singular reference from the compiler payload's identity field
  (`exomonad/actor/src/resident_workbench/compiler_query.rs:223-235, 290-302`).

### Wire identity (handlers WIP)

The compiler's enumeration wire grew a seventh field. `InfoEntry` and
`TypeMatch` are encoded as 7-element CBOR arrays whose final element (index 6)
is the entry's canonical resolved identity. This is the source for the protocol
`nameEntryReference`/`typeMatchReference` above.

- Runtime `InfoEntry` carries `identity: IdentifierRef` after `references`
  (`tidepool/runtime/src/session/inspection.rs:159-172`, WIP); `TypeMatch`
  carries the same (`tidepool/runtime/src/session/inspection.rs:205-216`, WIP).
- `decode_info_entry` requires 7 fields and decodes `fields[6]`
  (`tidepool/runtime/src/session/inspection.rs:1609-1627`, WIP); the type-match
  decoder does the same (`tidepool/runtime/src/session/inspection.rs:1564-1586`,
  WIP).
- Haskell `InfoEntry` carries `infoNominalName`/`infoNamespace`
  (`bridge/haskell/src/Tidepool/Introspection.hs:93-103`, WIP) and `TypeMatch`
  carries `typeMatchNominalName`/`typeMatchNamespace`
  (`bridge/haskell/src/Tidepool/Introspection.hs:142-152`, WIP).
  `nominalIdentity` renders those as an `IdentifierRef`
  (`bridge/haskell/src/Tidepool/Introspection.hs:798-806`, WIP), and
  `encodeEntry`/`encodeTypeMatch` emit 7 fields with identity last
  (`bridge/haskell/src/Tidepool/Introspection.hs:1113-1122, 1126-1138`, WIP).
- A local binding has no defining module, so `nominalIdentity` writes the empty
  string as `identifierModule`
  (`bridge/haskell/src/Tidepool/Introspection.hs:801-806`, WIP; the sentinel is
  line 803). The empty string means a local/session-owned identity, not a public
  module named `""`.

Evidence for each protocol shape:

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
declaration records: protocol fields `typeCanonical`, `typeVariables`, and
`typeConstraints` (`bridge/protocol/src/effects/introspection.rs:250-258`;
Haskell record `bridge/haskell/src/Tidepool/Introspection.hs:181-186`), projected
at runtime as `canonical`, `variables`, and `constraints`
(`tidepool/runtime/src/session/inspection.rs:79-84`). It is **not** the
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
(`bridge/protocol/src/effects/introspection.rs:489-495`). For the six new verbs
the handler makes this decision in `entry_scope_refusal` and
`compiler_query_answer`
(`exomonad/actor/src/resident_workbench/compiler_query.rs:410-440`), before any
projection (`exomonad/actor/src/resident_workbench/compiler_query.rs:423-506`).
A stale view must never become an empty success.

There is deliberately no synthesized compiler-world identity in this contract.
`ActorCompileView` exposes the session compile view, imports, source roots, and
compile inputs, but has no compiler-world accessor
(`exomonad/actor/src/mount.rs:731-743`,
`exomonad/actor/src/mount.rs:745-1033`). The adjacent public-module
world-inspection path still builds a `compiler-world:<hex(logical_id)>`
*display* fingerprint
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
  (`bridge/protocol/src/effects/introspection.rs:144`). **Known gap:** the
  handler's `ScopeBrowse`/`Browse` batch still goes through
  `run_admitted_inspections`, which compiles its inspection module
  (`exomonad/actor/src/resident_workbench/compiler_query.rs:116-141, 1059-1079`,
  WIP); it is not the zero-compile world read the effect docs describe. The
  mapping table marks those rows as the known gap.
- `rowFit` does not run the solver in the actor. The handler resolves the
  identifier with `row_fit_query` to `Browse {expanded: true}` (or `ScopeBrowse`
  for a session-reserved module), reads the typed
  `InfoEntry.identity`/`availability` entries, and selects with
  `fit_from_entries`
  (`exomonad/actor/src/resident_workbench/compiler_query.rs:144-163, 460-466,
  562-592`, WIP). Its cost is interface load plus module enumeration. Its result
  is the `Fit` record above, and its availability component still comes from
  compiler-typed evidence, never rendered text: `AvailabilityContext` carries
  the `Eff` type constructor and row
  (`bridge/haskell/src/Tidepool/Introspection.hs:240-246`); the row is read from
  the sentinel's typed `Proxy` and rendered text never determines it
  (`bridge/haskell/src/Tidepool/Introspection.hs:285-307`);
  `availabilityFor` compares the rows and solves predicates
  (`bridge/haskell/src/Tidepool/Introspection.hs:358-395`);
  `signatureAvailability` splits the sigma type
  (`bridge/haskell/src/Tidepool/Introspection.hs:409-412`); and
  `thingAvailability` uses that evidence for values, returning `Unknown` when it
  has none (`bridge/haskell/src/Tidepool/Introspection.hs:1013-1017`). Missing
  or disagreeing evidence yields `FitUnknown`, never an inferred success.
- `typeOfExpression` and `typeSearch` compile an explicit probe but never run
  it (`bridge/protocol/src/effects/introspection.rs:154-167`). The existing
  runtime queries are `InspectionQuery::TypeOf` and
  `InspectionQuery::TypeSearch`
  (`tidepool/runtime/src/session/inspection.rs:18-20, 856-875, 887-889`); the
  `TypeOf` inspection reads the probe's typed identifier rather than evaluating
  the expression (`bridge/haskell/src/Tidepool/Introspection.hs:586-593`).

## Mapping to current runtime paths

The schema is authoritative; these are the existing projection points a
consumer should reuse. Costs are as measured by the source path, not the target.

| Verb | Current runtime path | Cost |
| --- | --- | --- |
| `currentScopeView` | `current_scope_provenance` → `structured_provenance` (`exomonad/actor/src/resident_workbench/compiler_query.rs:173-179`; `exomonad/actor/src/resident_workbench.rs:14545-14572`) | prepare+hash, no compiler query |
| `scopeBindings` | `InspectionQuery::ScopeBrowse` (`tidepool/runtime/src/session/inspection.rs:21, 890-892`) to `InspectScope` (`bridge/haskell/src/Tidepool/Introspection.hs:597-602`); **known gap:** handler compiles the inspection module (`exomonad/actor/src/resident_workbench/compiler_query.rs:130, 1059-1079`, WIP) | interface/scope enumeration, compiles today |
| `moduleExports` | `InspectionQuery::Browse { expanded: false }` (`tidepool/runtime/src/session/inspection.rs:22-25, 893-895`) to `InspectModule` (`bridge/haskell/src/Tidepool/Introspection.hs:603-605, 885-901`); **known gap:** handler compiles the inspection module (`exomonad/actor/src/resident_workbench/compiler_query.rs:131-134, 1059-1079`, WIP) | interface load, compiles today |
| `info` | `InspectionQuery::StructuredInfo` (`tidepool/runtime/src/session/inspection.rs:26-29, 896-898`) | interface load |
| `typeOf` | `InspectionQuery::StructuredType` (`tidepool/runtime/src/session/inspection.rs:30-33, 899-901`) | interface load |
| `rowFit` | `row_fit_query` → `Browse`/`ScopeBrowse`, then `fit_from_entries` over typed entries (`exomonad/actor/src/resident_workbench/compiler_query.rs:154-163, 460-466, 562-592`, WIP) | interface load + module enumeration |
| `typeOfExpression` | `InspectionQuery::TypeOf` (`tidepool/runtime/src/session/inspection.rs:18, 857, 881-883`) | probe compile |
| `typeSearch` | `InspectionQuery::TypeSearch` (`tidepool/runtime/src/session/inspection.rs:20, 871-875, 887-889`) | probe compile |

## Known limitations (handlers WIP)

- `ExpressionRejected` carries `Diagnostic` values whose `span` is always
  `None`: probe compilation returns rendered text with no usable location, and
  `Diagnostic::error` constructs `span: None`
  (`exomonad/actor/src/resident_workbench/compiler_query.rs:347-377`, WIP;
  rejections built at 447-448 and 480-483). `DiagnosticSpan` remains in the type
  only so the wire shape can carry a span later
  (`exomonad/actor/src/resident_workbench/compiler_query.rs:350-361`, WIP).
- `NameNamespace` has no `Field` constructor
  (`bridge/protocol/src/effects/introspection.rs:215-218`), so a `rowFit`
  refusal for a field identifier reconstructs the query namespace as `Any`
  (`exomonad/actor/src/resident_workbench/compiler_query.rs:523-532`, WIP;
  the `Field => Any` arm is line 532).
- `QUERY_RESULT_LIMIT = 256` is provisional
  (`exomonad/actor/src/resident_workbench/compiler_query.rs:41-47`, WIP). The
  contributor guide requires each resource limit to record its workload/resource
  basis and observed usage; the 256-row boundary does not yet have that recorded
  basis, so it is an open item, not an accepted limit.

## Not in this surface

- Defining source spans. `IdentifierRef` carries only protocol
  `identifierModule`, `identifierName`, and `identifierNamespace`
  (`bridge/protocol/src/effects/introspection.rs:232-240`), projected at runtime
  as `module`, `name`, and `namespace`
  (`tidepool/runtime/src/session/inspection.rs:72-77`). `DiagnosticSpan` exists
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
- The handlers WIP (`cf4cd87e99 + WT`) adds
  `exomonad/actor/src/resident_workbench/compiler_query.rs` with
  `entry_scope_refusal`/`compiler_query_answer`, the six-verb decode dispatch
  (`exomonad/actor/src/resident_workbench.rs:10865-10946`, WIP), and the
  service call (`exomonad/actor/src/resident_actor.rs:6489-6496`, WIP). It also
  carries the 7-field identity wire described above. None of this is at
  integration HEAD.
- At integration HEAD the runtime is still the pre-identity, unpinned form:
  `InfoEntry` has six fields (`tidepool/runtime/src/session/inspection.rs:159-167`)
  and `TypeMatch` six (`tidepool/runtime/src/session/inspection.rs:200-208`).
  `InspectionQuery` has no variants named `ScopeBindings`, `ModuleExports`,
  `RowFit`, or `TypeOfExpression`, and its `TypeSearch` variant is the
  pre-existing unpinned form
  (`tidepool/runtime/src/session/inspection.rs:16-34`). The runtime `QueryError`
  enum still lacks `ScopeChanged` and `ExpressionRejected`
  (`tidepool/runtime/src/session/inspection.rs:151-157`).
- Therefore every row in the acceptance matrix is an acceptance requirement,
  not evidence already produced by this checkout.

## Acceptance evidence matrix

Every row is currently **no evidence yet**: none of these observations has been
run for this revision. "Oracle" names the independent observation that decides
the row; a component pass alone is not evidence for the integrated claim.

| Claim | Observation | Oracle | Status |
| --- | --- | --- | --- |
| Persona flow compiles no target | `currentScopeView → moduleExports → info` under one admitted view keeps `inspection.compile_attempts == 0` | worker's `inspection.compile_attempts` count (`bridge/haskell/src/Tidepool/Introspection.hs:262-272`) | no evidence yet |
| Pinned stale view refused through the actor path | a `Just p` captured before a publication returns `ScopeChanged p current`, never an empty list | handler `entry_scope_refusal` (`exomonad/actor/src/resident_workbench/compiler_query.rs:410-420`, WIP) and the decoded `Either QueryError` (`bridge/protocol/src/effects/introspection.rs:489-495`) | no evidence yet |
| Entry-vs-pre-reply drift refused even for `Nothing` | the view moves between capture and reply; the answer is `ScopeChanged(before, after)` rather than the stale result | handler `compiler_query_answer` (`exomonad/actor/src/resident_workbench/compiler_query.rs:423-440`, WIP) | no evidence yet |
| `typeOfExpression` never evaluates | a compiled probe returns `ExpressionType` with no evaluation or side effect | probe compile and evaluation boundary (`bridge/protocol/src/effects/introspection.rs:154-167, 406-415`) | no evidence yet |
| `typeSearch` never evaluates | a compiled type-pattern probe returns `TypeMatches` with no evaluation or side effect | probe compile and evaluation boundary (`bridge/protocol/src/effects/introspection.rs:154-167, 438-447`) | no evidence yet |
| Truncation is visible, not silent | `observed > limit` returns the complete observed count with `limit = 256` and a retained prefix | `NameList::from_entries`/`TypeMatches::from_matches` (`exomonad/actor/src/resident_workbench/compiler_query.rs:245-262, 312-327`, WIP) | no evidence yet |
| No Lookup survives | repo-wide assertion finds no Lookup effect in prompts, fixtures, or generated artifacts | search/assertion over `exomonad/prompts`, fixtures, and generated outputs | no evidence yet |
