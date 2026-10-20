# Compiler-query effects

Status: accepted direction, refreshed 2026-10-19 on
`feature/compiler-query/world-enum-on-handlers`, which carries the handlers
integration (`5eb2b3dbed` plus its bounded enumeration work) and the four
world-enum commits (`374a56aa1b`, `033210cfa8`, `1316247a62`, `f1fbbcc6f2`).
The runtime-owned `scopeBindings` route and the removal of the compiler-side
verb are now committed code, not a pending handoff. Evidence executed on this
branch is named in the evidence table; claims still unexercised say `UNRUN`.
The schema in
`bridge/protocol/src/effects/introspection.rs` is authoritative; the runtime
projection is in `tidepool/runtime/src/session/inspection.rs`. This document
states the contract, not that every consumer is wired. No native build or
execution evidence for this branch is listed in the acceptance table; a claim
with no recorded selector is marked `UNRUN`.

Citation basis: line numbers are validated against this branch
(`feature/compiler-query/world-enum-on-handlers`), whose base is the handlers
commit `5eb2b3dbed` plus the four world-enum commits listed above; both layers
are committed here. Where a claim describes history that is not on this
branch, the text names the revision. The enumeration verdicts come from
`/srv/swarm/evidence/inanna/shared-warm-compiler-20261019/compiler-query/handlers/ENUMERATION-SPIKE.md`
(`REPORT_SHA256 3e28ca250aebdc4cadb8b430257f2009e387003bfe4e46f8ef734481ae2a4c2b`),
a read-only source inspection at `52337d6220` with that checkout's in-flight
worktree. Its line numbers describe that working tree, not a commit, so this
plan cites the spike for its verdicts and re-derives every code anchor against a
committed blob. The schema source was last changed by `9329e7f216`, which
reordered `nameEntryReference`/`typeMatchReference` after their `*References`
fields and shifted line numbers from the earlier `bc58326f30` correction;
generated snapshots were committed by `c830d1f585` and refreshed on this branch
by `1316247a62`.

## Decision

There is one extended `Introspection` effect, not a parallel compiler-query
family. `Introspection` is the migrated effect registered in
`bridge/protocol/src/effects/mod.rs:84`. It declares its own actor handler
(`bridge/protocol/src/effects/introspection.rs:12-33`; `verb_with` sets
`HandlingClass::Actor` at `bridge/protocol/src/effects/introspection.rs:189-198`)
and owns the name-query, type, declaration, provenance, and error records
(`bridge/protocol/src/effects/introspection.rs:208-504`).

The extension adds five verbs to that effect: `currentScopeView`,
`scopeBindings`, `rowFit`, `typeOfExpression`, and `typeSearch`. Their
constructors and return types are declared at
`bridge/protocol/src/effects/introspection.rs:46-109`; their generated helper
signatures are fixed by the schema test at
`bridge/protocol/src/effects/introspection.rs:603-619`.

Public-module export enumeration (`moduleExports`) is **cut for this delivery**.
The principled held-HPT world route is pending (see "The held-HPT route for
public module exports"); it must land with a non-compiling world query and its
own oracle before the verb returns.

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
| `scopeBindings` | `Maybe ScopeProvenance -> Eff effs (Either QueryError NameRefList)` | `bridge/protocol/src/effects/introspection.rs:52-57, 588, 613` |
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
entry (`exomonad/actor/src/resident_workbench/compiler_query.rs:446-456`), and
`compiler_query_answer` refuses a view that moved while the query ran
(`exomonad/actor/src/resident_workbench/compiler_query.rs:461-544`). Thus
entry-versus-pre-reply drift is refused for **every** query, including a
`Nothing` query that supplied no expected scope. The older `info`/`typeOf`
structured path keeps its own pre/post comparison
(`exomonad/actor/src/resident_workbench.rs:14514-14525`; projection at
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
  (`exomonad/actor/src/resident_workbench/compiler_query.rs:255-267, 325-337`,
  and the equivalent type-match arm).

### Enumeration answer shape

Enumeration does not return `NameEntry`. `NameEntry` is the `info` answer, and
three of its fields are structurally unavailable world-side: `nameEntryDisplay`
needs GHC `ppr` or an interface decode, `nameEntryAvailability` needs the
executing actor's effect row, and `nameEntryReferences` needs GHC `NameInfo`
(`bridge/protocol/src/effects/introspection.rs:357-377`). Encoding them would
force either a fabricated value or a compile, which defeats the split.

Enumeration therefore narrows to a distinct reference record. The committed
shape is `NameRefList` over the existing `IdentifierRef`:

```haskell
data NameRefList = NameRefList
  { refs               :: [IdentifierRef]
  , refsObserved       :: Int
  , refsLimit          :: Int
  , refListProvenance  :: ScopeProvenance
  }
```

`IdentifierRef` is the identity to chain into `info`, `typeOf`, or `rowFit`.
`NameEntry`/`NameList` are the rich enumeration records retained for the
pending public-module export route; no verb in this delivery returns them.
`NameRefList` is committed at
`bridge/protocol/src/effects/introspection.rs:56, 389` and regenerated in
`bridge/mcp/src/generated/introspection.rs`; there is no separate `NameRef`
record. A nullable availability or display in this record would let callers
read "absent" as "unknown", which is why the world-owned answer carries
identity only.

### Wire identity

The compiler's enumeration wire grew a seventh field. `InfoEntry` and
`TypeMatch` are encoded as 7-element CBOR arrays whose final element (index 6)
is the entry's canonical resolved identity. This is the source for the protocol
`nameEntryReference`/`typeMatchReference` above.

- Runtime `InfoEntry` carries `identity: IdentifierRef` after `references`
  (`tidepool/runtime/src/session/inspection.rs:174-187`); `TypeMatch`
  carries the same (`tidepool/runtime/src/session/inspection.rs:220-233`).
- `decode_info_entry` requires 7 fields and decodes `fields[6]`
  (`tidepool/runtime/src/session/inspection.rs:1650-1668`); the type-match
  decoder does the same (`tidepool/runtime/src/session/inspection.rs:1605-1628`).
- Haskell `InfoEntry` carries `infoNominalName`/`infoNamespace`
  (`bridge/haskell/src/Tidepool/Introspection.hs:93-102`) and `TypeMatch`
  carries `typeMatchNominalName`/`typeMatchNamespace`
  (`bridge/haskell/src/Tidepool/Introspection.hs:142-153`).
  `nominalIdentity` renders those as an `IdentifierRef`
  (`bridge/haskell/src/Tidepool/Introspection.hs:821-826`), and
  `encodeEntry`/`encodeTypeMatch` emit 7 fields with identity last
  (`bridge/haskell/src/Tidepool/Introspection.hs:1142-1151, 1155-1167`).
- A local binding has no defining module, so `nominalIdentity` writes the empty
  string as `identifierModule`
  (`bridge/haskell/src/Tidepool/Introspection.hs:821-826`; the sentinel is
  line 823). The empty string means a local/session-owned identity, not a public
  module named `""`.

Evidence for each protocol shape:

- `NameEntry` carries the availability, dependency references, and canonical
  reference shown: `bridge/protocol/src/effects/introspection.rs:357-377`;
  generated text `bridge/mcp/src/generated/introspection.rs:35`.
- `NameList` adds `nameListProvenance` beside the observed and retained counts:
  `bridge/protocol/src/effects/introspection.rs:378-387`; generated text
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
(`tidepool/runtime/src/session/inspection.rs:80-84`). It is **not** the
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
  (`exomonad/actor/src/resident_workbench.rs:14571`;
  `exomonad/actor/src/mount.rs:835-837`). The published tip for a compile view
  is exposed by `CompileViewKey::generation()`
  (`tidepool/runtime/src/session/persistent.rs:312-323`).
- `fingerprint` is a content digest over the prepared preamble, imports,
  generation, include paths, and injected modules, finalized into
  `ScopeProvenance { scope, generation, fingerprint }`
  (`exomonad/actor/src/resident_workbench.rs:14572-14588`). It commits to the
  exact source/import view, so two answers with the same scope and generation
  but different content do not compare equal.

`ScopeChanged` is the stale-view refusal. It carries the provenance the caller
pinned or that was captured before the query (`used`/`before`) and the
provenance now current (`current`/`after`)
(`bridge/protocol/src/effects/introspection.rs:489-495`). For the six new verbs
the handler makes this decision in `entry_scope_refusal` and
`compiler_query_answer`
(`exomonad/actor/src/resident_workbench/compiler_query.rs:419-512`), before any
projection (`exomonad/actor/src/resident_workbench/compiler_query.rs:461-544`).
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

### The compile-path enumeration reads the probe target's scope

`scopeBindings` no longer takes this path; it answers from the admitted world.
The description below applies to `rowFit`.

`InspectScope` enumerates `globalRdrEnvElts rdrEnv`
(`bridge/haskell/src/Tidepool/Introspection.hs:597-602`), where `rdrEnv` is the
checked target's `tcg_rdr_env`
(`bridge/haskell/src/Tidepool/GhcPipeline.hs:4968`, carried as
`crTargetRdrEnv` at `:4832`, and handed to the inspector at
`bridge/haskell/app/Main.hs:418`). The checked compile returns `(tcg, probes)`
only for the target module (`GhcPipeline.hs:4866, 4951`), so the generated
probe participates in the normal target compile.

The cost is real, but the correctness defect matters more: the answer is the
**probe module's** scope, not the actor's. Bindings that exist only in the
actor's imports can be missing, and probe-only bindings can appear. Do not
describe today's `ScopeBrowse`/`Browse` path as a correct answer that merely
costs a compile.

The compile is observable, and an existing test pins it:
`qualified_info_and_browse_share_one_checked_target` requires exactly one
`tidepool-checked module=Expr target=True` in worker stderr
(`tidepool/runtime/src/session/inspection.rs:1968-2013`, assertion at
`:1992-1998`).

### Accepted split: three owners, one rule

The schema specifies `scopeBindings` as target-free enumeration and keeps
generated session modules private
(`bridge/protocol/src/effects/introspection.rs:133-167`, `:144`). The held-world
spike establishes that `scopeBindings` and the deferred public-module export
route reach that goal by two different routes, and that `rowFit` cannot:

- `scopeBindings` is **runtime-owned**. It answers from the admitted world's
  published value bindings with no compiler call and no extract worker. The
  bindings are already materialized per admission: `CompilerWorldBinding`
  carries occurrence, `SymbolIdentity`, `SessionVarId`, defining scope, and
  `Arc<CheckedValueArtifact>` (`tidepool/toolchain/src/compiler_world.rs:331-353`),
  built by `capture_inspection_world_in`
  (`tidepool/runtime/src/session/persistent.rs:1712, 1782, 1792`) and attached
  as `InspectionWorldInputs::Cold { bindings, .. }`
  (`tidepool/runtime/src/session/inspection.rs:397-404`). The table is keyed to
  one `query_scope` and holds published value bindings only
  (`compiler_world.rs:369-370, 390-448`), so other scopes, types, classes,
  constructors, and imported-but-unpublished names are not enumerable.
  Exposure is the missing piece: `ActorCompileView` has no world accessor
  (`exomonad/actor/src/mount.rs:731-743`) and
  `CompilerWorldRestoration.bindings` is private
  (`compiler_world.rs:530-543`). One runtime accessor plus one actor route is
  the minimal addition; no new cache is required.
- `moduleExports` is **held-HPT world query** (deferred design for the cut
  verb). It resolves the realized HPT
  (`bridge/haskell/src/Tidepool/GhcPipeline.hs:1777`), loads a package module
  through `findImportedModule` when it is not home (`:1822-1826`), and reads
  `getModuleInfo`/`modInfoExports` on a fresh context with `hsc_targets=[]`
  (`:1831-1838`). A miss — a home module absent from the realized HPT, or a
  module whose interface is missing from the package DB — is a **typed
  refusal**, never a compile. The cold-demand branch that calls
  `runCompileCycle CheckedPublicModule` (`:1783-1816`) is what must be
  replaced; it is invisible to `inspection.compile_attempts`, so a
  zero-compile test would not catch it. Package modules pay an interface load
  (I/O plus deserialization), a distinct cost class. Do not fabricate
  availability: `availabilityFor` returns `Available` whenever the output does
  not resolve to an `Eff` row
  (`bridge/haskell/src/Tidepool/Introspection.hs:358-372`), a false success
  when no row evidence exists.
- `rowFit` is **NO-GO world-side** and stays solver-backed in the actor. The
  retained row is an authority artifact for a checked value interface
  (`tidepool/toolchain/src/checked_cell.rs:1312-1321`), not the executing
  actor's effect row, and it carries no solved-predicate evidence. `rowFit`
  needs `AvailabilityContext`
  (`bridge/haskell/src/Tidepool/Introspection.hs:240-246`), the typed row from
  the sentinel binder (`:576-582`), and predicate solving (`:397-407`); none
  exists world-side. A target-free `rowFit` needs a new retained row plus an
  instance-context contract, which is its own parcel.

The acceptance rule "enumeration does not compile a checked target" holds for
`scopeBindings`, which answers from the admitted world without an extraction
request. Public-module export enumeration is out of this delivery's surface, so
the rule is not claimed for it; the deferred held-HPT route must satisfy it
before the verb returns. `rowFit` keeps its compile and its compiler-typed
availability evidence by design.

### `compile_attempts == 0` is vacuous today

`inspection.compile_attempts` is emitted by `withInspectionWork`
(`bridge/haskell/src/Tidepool/Introspection.hs:255-267`) and incremented only by
`inspectionCompilationWork` at the inspection compile callback (`:269-271`;
callback `bridge/haskell/app/Main.hs:405-412`). `runBoundWorldQuery` never
enters `withInspectionWork`
(`bridge/haskell/src/Tidepool/CompilerRequest.hs:336-340`), and the cold-demand
`runCompileCycle` (`GhcPipeline.hs:1783-1816`) is not counted. So
`compile_attempts == 0` on the world path passes vacuously. The real assertion
is: no extract worker is spawned (`scopeBindings`). The deferred held-HPT
route's oracle is a stderr assertion in the style of
`tidepool/runtime/src/session/inspection.rs:1996-2000`: no
`tidepool-checked ... target=True` line. Keep the counter check as a secondary
observation, not as the evidence.

`typeOfExpression` and `typeSearch` still compile an explicit probe and never
run it (`bridge/protocol/src/effects/introspection.rs:154-167`). The existing
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
| `currentScopeView` | `current_scope_provenance` → `structured_provenance` (`exomonad/actor/src/resident_workbench/compiler_query.rs:205-211`; `exomonad/actor/src/resident_workbench.rs:14561-14588`) | prepare+hash, no compiler query |
| `scopeBindings` | runtime-owned: `IntrospectionReq::ScopeBindings` → `ResidentActorBoundary::WorldScopeBindings` → `resume_world_scope_bindings` → `public_module_query_source` gate → `capture_inspection_world_in` → `AdmittedInspectionWorld::scope_bindings` (`exomonad/actor/src/resident_workbench.rs:10871-10879`; `exomonad/actor/src/resident_workbench/world_enumeration.rs:110-215`; `tidepool/runtime/src/session/inspection.rs:490-535`). There is no `CompilerQuery::ScopeBindings` and no `InspectionQuery` for it | no compiler query, no extract worker; identity-only answer. `refsObserved` is the exact count of bindings in the admitted scope; `refs` may be truncated at `refsLimit` |
| `info` | `InspectionQuery::StructuredInfo` (`tidepool/runtime/src/session/inspection.rs:26-29, 896-898`) | interface load |
| `typeOf` | `InspectionQuery::StructuredType` (`tidepool/runtime/src/session/inspection.rs:30-33, 899-901`) | interface load |
| `rowFit` | `row_fit_query` → `Browse`/`ScopeBrowse`, then `fit_from_entries` over typed entries (`exomonad/actor/src/resident_workbench/compiler_query.rs:186-195, 496-503, 605-635`) | interface load + module enumeration; NO-GO world-side, so this remains the only route |
| `typeOfExpression` | `InspectionQuery::TypeOf` (`tidepool/runtime/src/session/inspection.rs:18, 857, 881-883`) | probe compile |
| `typeSearch` | `InspectionQuery::TypeSearch` (`tidepool/runtime/src/session/inspection.rs:20, 871-875, 887-889`) | probe compile |

## Known limitations

- The world-owned `scopeBindings` answer is narrowed to `NameRefList`
  (`[IdentifierRef]` plus observed/limit/provenance; see the answer-shape
  section): world-side there is no `display`, no actor-row `availability`, and
  no `NameInfo` reference set to report. Those stay with the compiling `info`
  verb.
- The two enumeration answers count differently, and callers must not read one
  rule into the other. `scopeBindings` counts every binding in the admitted
  scope after dedupe and reports that exact total as `refsObserved`, then
  truncates `refs` at `refsLimit`; the total stays exact when the list is
  shorter. `typeSearch` stops the worker at `QUERY_RESULT_LIMIT`, so its
  observed count is only what was retained: `observed == limit` is a lower
  bound, not the true environment size.
- `scopeBindings` covers one `query_scope`'s published value bindings only
  (`tidepool/toolchain/src/compiler_world.rs:369-370, 390-448`). Other scopes,
  non-value names (types, classes, constructors), and imported-but-unpublished
  names cannot be returned by this route.
- The pending public-module export route must refuse, not compile, when a home
  module is absent from the realized HPT
  (`bridge/haskell/src/Tidepool/GhcPipeline.hs:1777, 1820-1826`) or when a
  package interface cannot be loaded (`:1836-1838`). The refusal is typed
  (`WorldQueryUnavailable`); an empty export list would misreport a missing
  interface as a module with no exports.
- `ExpressionRejected` carries `Diagnostic` values whose `span` is always
  `None`: probe compilation returns rendered text with no usable location, and
  `Diagnostic::error` constructs `span: None`
  (`exomonad/actor/src/resident_workbench/compiler_query.rs:402-416`;
  rejections built at 484 and 518). `DiagnosticSpan` remains in the type
  only so the wire shape can carry a span later
  (`exomonad/actor/src/resident_workbench/compiler_query.rs:402-416`).
- `NameNamespace` has no `Field` constructor
  (`bridge/protocol/src/effects/introspection.rs:215-218`), so a `rowFit`
  refusal for a field identifier reconstructs the query namespace as `Any`
  (`exomonad/actor/src/resident_workbench/compiler_query.rs:556-584`;
  the `Field => Any` arm is line 575).
- `QUERY_RESULT_LIMIT = 256` is the enumeration budget, and it is now sent to
  the worker rather than applied only after transit
  (`exomonad/actor/src/resident_workbench/compiler_query.rs`, handlers branch).
  `typeSearch` plans the bounded extraction question (`TypeSearchBounded`), so
  the worker stops rendering at the cap; the handler keeps its trim only as a
  defense against a decoder that ignores the request budget. The world-owned `scopeBindings` makes no
  extraction request, so `InspectionQuery::ScopeBrowseBounded` was removed
  with the compiler-side verb; `ScopeBrowse` remains only for `rowFit` on a
  reserved session module. Basis: 256 is a
  presentation/transport budget for one notebook answer, not a measured scope
  statistic; it is the boundary the provisional limit already used, and
  enforcement at the worker is the property that was missing.
  For `typeSearch`, `observed == limit` therefore means "at least limit": the
  true environment size is not observable once the worker stops at the cap.
  `scopeBindings` is not capped that way and keeps an exact count.
- `rowFit` is the deliberate exception. It keeps the complete, unbounded
  `Browse`/`ScopeBrowse` read because a cap can drop the asked-for identifier
  and turn a real answer into `FitUnknown`; its wire answer is a single `Fit`.
  A keyed availability query is the follow-up that removes this last unbounded
  compiler read. Type search's budget bounds the encoded match list; matching
  every candidate still costs work proportional to the environment, because a
  partial search would silently miss matches.

## Not in this surface

- Public-module export enumeration (`moduleExports`). Cut for this delivery; the
  principled held-HPT world route is pending and must land with its own
  non-compiling oracle.
- Defining source spans. `IdentifierRef` carries only protocol
  `identifierModule`, `identifierName`, and `identifierNamespace`
  (`bridge/protocol/src/effects/introspection.rs:232-240`), projected at runtime
  as `module`, `name`, and `namespace`
  (`tidepool/runtime/src/session/inspection.rs:73-78`). `DiagnosticSpan` exists
  for compiler diagnostics, not for every answer
  (`bridge/protocol/src/effects/introspection.rs:448-458`).
- Batching. Each verb asks one typed question and returns one `Either`
  (`bridge/protocol/src/effects/introspection.rs:189-198`); `NameList` and
  `TypeMatches` expose observed and retained counts but no continuation token
  (`bridge/protocol/src/effects/introspection.rs:378-387, 438-447`).
- A second effect per cost class. Cost is documentation and prompt guidance;
  the effect and its `Member Introspection effs` constraint stay single
  (`bridge/protocol/src/effects/introspection.rs:603-619`).

## Current implementation state

- The protocol schema is committed, and its test fixes the seven helper
  signatures and the required declarations
  (`bridge/protocol/src/effects/introspection.rs:577-671`).
- Campaign execution at the earlier integration revision `7c906242c1` reached
  `published_introspection_example_returns_structured_declaration`
  (`exomonad/host/src/actor_host/documentation_tests.rs:1490`) and failed only
  because `Introspection` was not a member of the actor's effect row. The
  handlers base on this branch claims the family as an intrinsic actor effect
  (`5eb2b3dbed`); the end-to-end actor documentation test was not re-run in
  this lane. The decode → dispatch → resume path is exercised for
  `scopeBindings` by the executed tests named below.
- Generated protocol snapshots were committed at integration HEAD `c830d1f585`
  and now list all seven constructors with `Maybe ScopeProvenance`,
  `FitAvailability`, and the `Fit` record
  (`bridge/mcp/src/generated/introspection.rs:11-19, 35-46, 50-57`;
  actor request enum at `exomonad/actor/src/generated/introspection.rs:23-32`).
- The handlers base of this branch (`5eb2b3dbed`) adds
  `exomonad/actor/src/resident_workbench/compiler_query.rs` with
  `entry_scope_refusal`/`compiler_query_answer`, the five-verb decode dispatch
  (`exomonad/actor/src/resident_workbench.rs:10861-10939`), and the
  service call (`exomonad/actor/src/resident_actor.rs:6496`). It also
  carries the 7-field identity wire described above.
- The enumeration split is implemented for `scopeBindings` on this branch: the
  actor decodes `IntrospectionReq::ScopeBindings` into a world-owned boundary,
  `resume_world_scope_bindings` reads the admitted world through
  `AdmittedInspectionWorld::scope_bindings`, and the compiler-side
  `CompilerQuery::ScopeBindings` no longer exists. `rowFit` still reads browsed
  typed entries.
- Before this branch, integration HEAD was still the pre-identity, unpinned
  form (kept as history, not a claim about this branch):
  `InfoEntry` has six fields (`tidepool/runtime/src/session/inspection.rs:159-167`)
  and `TypeMatch` six (`tidepool/runtime/src/session/inspection.rs:200-208`).
  `InspectionQuery` has no variants named `ScopeBindings`, `ModuleExports`,
  `RowFit`, or `TypeOfExpression`, and its `TypeSearch` variant is the
  pre-existing unpinned form
  (`tidepool/runtime/src/session/inspection.rs:16-34`). The runtime `QueryError`
  enum still lacks `ScopeChanged` and `ExpressionRejected`
  (`tidepool/runtime/src/session/inspection.rs:151-157`).
- The acceptance matrix below separates the selectors that executed on this
  branch from the integrated request-path evidence that is still missing.

## Acceptance evidence matrix

Every row is `UNRUN` unless its status says otherwise. `PASS` means the named
selector executed and passed on the branch named in the status column, through
`python3 scripts/native-workflow.py --profile fast-dev test-lib exomonad-actor
--exact <name> --expected-count 1`. "Oracle" names the independent observation
that decides the row; a component pass alone is not evidence for the integrated
claim, and no row below is an integrated request-path run.

| Claim | Observation | Oracle | Status |
| --- | --- | --- | --- |
| `published_introspection_example_returns_structured_declaration` passes | the published introspection fence executes through the real tool and returns a structured declaration | the test itself at `exomonad/host/src/actor_host/documentation_tests.rs:1490` | `UNRUN` on this branch; `blocked` at `7c906242c1` because `Introspection` was absent from the actor's effect row |
| `scopeBindings` calls no compiler | enumerating the admitted scope spawns no extract worker | process-wide `tidepool_extract_cmd::extract_spawn_count()` delta is zero, and the result equals the machine's `binding_names_in` set (names only) | `PASS` — `resident_workbench::request_tests::scope_bindings_reads_the_admitted_world_without_an_extract_worker`, 1 executed. Limit: the test now satisfies `public_module_query_source` with a `Complete` captured owner, but it still calls `capture_inspection_world_in`/`scope_bindings()` directly; the decode → `resume_world_scope_bindings` → `ToHaskell` path is `UNRUN`, and the oracle compares names, not `(module, name, namespace)` |
| `moduleExports` compiles no target | the held-HPT query answers with no `tidepool-checked ... target=True` and no `tidepool-target` line on worker stderr | stderr assertion in the style of `tidepool/runtime/src/session/inspection.rs:1996-2000` | `NOT-IN-SURFACE` — cut for this delivery; the principled held-HPT route is pending |
| Persona flow: view, then declaration | `currentScopeView → info` in one admitted view answers both from the same view and spawns no extract worker for enumeration | the two oracles above, read from one request's worker accounting and stderr | `UNRUN` |
| Pinned stale view refused before the world is read | a `Just p` captured before a publication returns `ScopeChanged p current`, never an empty list | `pinned_scope_moved` (`exomonad/actor/src/resident_workbench/world_enumeration.rs:92-98`) plus the decoded `Either QueryError` (`bridge/protocol/src/effects/introspection.rs:489-495`) | `PASS` for the pure helper — `pinned_scope_moved_refuses_a_stale_view` and `pinned_scope_moved_refuses_a_fingerprint_move_at_the_same_generation`, 1 executed each. Limit: the actor-path `ScopeChanged` projection is `UNRUN` |
| Entry-vs-pre-reply drift refused even for `Nothing` | the view moves between capture and reply; the answer is `ScopeChanged(before, after)` rather than the stale result | post-read re-observation in `resume_world_scope_bindings` (`exomonad/actor/src/resident_workbench/world_enumeration.rs:168-190`) | `UNRUN` |
| `typeOfExpression` never evaluates | a compiled probe returns `ExpressionType` with no evaluation or side effect | probe compile and evaluation boundary (`bridge/protocol/src/effects/introspection.rs:154-167, 406-415`) | `UNRUN` |
| `typeSearch` never evaluates | a compiled type-pattern probe returns `TypeMatches` with no evaluation or side effect | probe compile and evaluation boundary (`bridge/protocol/src/effects/introspection.rs:154-167, 438-447`) | `UNRUN` |
| Enumeration is bounded at the worker | a type-search answer larger than `QUERY_RESULT_LIMIT` returns at most `limit` entries, and `observed == limit` is read as "at least limit" rather than a true count. `scopeBindings` instead reports the exact admitted-scope count and truncates only `refs` | handler test `type_search_cannot_encode_more_than_the_budget` (`exomonad/actor/src/resident_workbench/compiler_query.rs`) plus the bounded `TypeSearchBounded` plan. `scopeBindings` is bounded runtime-side by `SCOPE_BINDING_LIMIT` with no extraction request | `UNRUN` (compiled with the unit-test target; no selector executed). `InspectionQuery::ScopeBrowseBounded` was deleted with the compiler-side verb |
| No compiler-side `scopeBindings` survives | no `CompilerQuery` names `scopeBindings`; `CompilerQuery::ScopeBindings` is deleted | structural assertion inside `compiler_verbs_plan_their_owner_query_and_no_verb_enumerates`; the explicit empty enumerating set pins that `BrowseBounded` has no producer | `UNRUN` until the five-verb gate runs; rename-only row, no new behavior |
| No Lookup survives | repo-wide assertion finds no Lookup effect in prompts, fixtures, or generated artifacts | search/assertion over `exomonad/prompts`, fixtures, and generated outputs | `UNRUN` |
