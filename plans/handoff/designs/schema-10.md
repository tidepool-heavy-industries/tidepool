# Schema 10: site table and structural type evidence

Implements decisions 1–3 of `resume-contract-v2.md` (delivery mode per site,
structural type evidence, constructor closure with a `host_id` index). This
document is the implementation contract both halves are written against; the
Haskell producer and the Rust reader are built in parallel from it and meet at
`just fixtures-update`.

Source facts this rests on (checked on `engine/stg-production-cutover` at
`7b136ecbb`):

- `SCHEMA_VERSION = 9` in `tidepool-repr/src/execution_schema.rs:9` and
  `schemaVersion = 9` in `haskell/src/Tidepool/ExecutionSchema.hs:24`.
  The program array has 13 fields (`codec.rs` `program`, `array(value, 13)`;
  `ExecutionEncode.encodeWireProgram`): magic, schema, profile, toolchain,
  ABI, target, signatures, globals, constructors, operations, expressions,
  bindings, entry.
- Sites today: `PreparedSites.buildYieldSite` records `YieldSite { ysSite,
  ysOrigin, ysOrdinal, ysAnswer :: SiteType, ysInputs, ysReplyDeclaration }`
  with `SiteType { stType (rendered), stModules, stHeads }`; they reach only
  the Core `asks.json` sidecar (`Main.hs:426`) and `pmYieldSites` on
  `PreparedModule`. Nothing in the prepared artifact names a site.
- `VerbSpec` (`EffectSchema.hs:55`) has no delivery mode. `sitedVerbs` rows:
  `runLLMTurn`, `runLLMTurnFork`, `runLLMTurnFanout`, `finalize`, `fork`,
  `forkAll`, `forkMap`, `forkCata`, `request`, `requestWith`,
  `requestWithProgress`, `requestWithProgressInto`, `child`,
  `childWithProgress`, `receive`, `serve`.
- Constructors are interned only when built or matched
  (`ExecutionProjection.internConstructor`, `P` state `constructors`/
  `constructorDecls`). `DescriptorInterner` (`tidepool-codegen/src/
  prepared_program/interner.rs`) is keyed by `SymbolIdentity` only; per-artifact
  `host_id` uniqueness is `validation.rs:1350`.
- The type-graph must be built while Core still carries types (elaboration
  in `PreparedSites`, pre-CorePrep) but constructor ids are minted in
  projection (`P`). So elaboration produces a GHC-typed intermediate and
  projection lowers it to the wire.

## 1. Delivery mode (producer: `EffectSchema`, `PreparedSites`)

```haskell
data SiteDelivery = DeliverHostAnswer | DeliverLiveReentry | DeliverExitCellFill | DeliverTerminalCapture
data SiteWireSource
  = SelectedAnswer | ListAnswer | InvocationAnswer | InvocationAnswers
  | ResponseResultEvidence
-- VerbSpec gains: vsDelivery :: SiteDelivery, vsWireSource :: SiteWireSource
```

`spAnswer` remains the concrete selected type from `vsAnswerSource`; the
wire-source strategy constructs a GHC `Type` from it, before erasure. The
following table is exhaustive:

| verbs | delivery | wire-source strategy / evidence |
|---|---|---|
| `runLLMTurn`, `fork` | `DeliverHostAnswer` | `SelectedAnswer`: `T` |
| `runLLMTurnFork` | `DeliverHostAnswer` | `InvocationAnswer`: `Either InvocationExit T` |
| `runLLMTurnFanout` | `DeliverHostAnswer` | `InvocationAnswers`: `[Either InvocationExit T]` |
| `forkAll`, `forkMap`, `forkCata` | `DeliverHostAnswer` | `ListAnswer`: `[T]` |
| `receive`, `serve` | `DeliverLiveReentry` | `SelectedAnswer`: `next` / `state` |
| `request`, `requestWith`, `requestWithProgress`, `requestWithProgressInto`, `child`, `childWithProgress` | `DeliverExitCellFill` | `ResponseResultEvidence`: `ResponseResult result` |
| `finalize` | `DeliverTerminalCapture` | `SelectedAnswer`: captured `v` |

**The evidence has delivery-specific meaning.** For HostAnswer and
LiveReentry, `siteWire` is the exact value supplied to the suspended
continuation. For ExitCellFill it describes the value filled into the exit
cell, not the `()` acknowledgement of `submitRequest`; no host-built result
is accepted at that suspension. For TerminalCapture it describes the captured
value, and no resume is permitted. These meanings are part of the schema;
`siteWire` must never be interpreted without `siteDelivery`.

Surface return types do not determine these types. `forkCataSited` returns
`M b` but its internal `forkAllSited` requests receive `[b]`
(`Answerer/Fork.hs:168-182`); `serveSited` returns `Eff effs exit` but its
internal receive receives `state` (`Actor.hs:349-357`). `request*` returns
`Response result` or a progress tuple, while `submitRequest` returns `()`
(`Agent/Reply/Internal.hs:262-270`). `childSited` constructs an `Unfold`
(`actors/Tidepool/Actors/Unfold.hs:469-475`). `finalizeSited` captures `v`
while returning unconstrained `a` (`tidepool-mcp/src/generated/finalize.rs`).

Build wrappers with real GHC TyCons and `mkTyConApp` / `mkListTy`, not rendered
names or reconstructed Rust types. Use GHC's built-in list/Either constructors
and resolve `InvocationExit` and `ResponseResult` from their authoritative
loaded definitions in the typed compiler environment. Resolve only strategies
present in the module; a missing or ambiguous required definition is a typed
site rejection. The resolved authority is supplied to `PreparedSites`; do not
import `GhcPipeline` there (it would create a dependency cycle).
`GhcPipeline.stripMonadHead` strips a generic last application argument and
must not be used for this policy. Source rendering's `vsListAnswer` is not
an authority for wire evidence.

`YieldSite` and `SiteType` remain unchanged, including positional shape,
`Eq`/`Show` instances, and Core sidecar consumers in `Binders`/`CborEncode`.
Prepared-only evidence lives alongside them:

```haskell
data PreparedSite = PreparedSite
  { psOwner :: Id                  -- enclosing top binder, same owner as SiteRejection
  , psSite :: YieldSite            -- unchanged presentation and stable site id
  , psDelivery :: SiteDelivery
  , psWireNode :: TypeNodeId
  , psInputNodes :: [TypeNodeId]
  }
```

Build the GHC wire/input `Type`s transiently during elaboration and intern
them into the module's graph before constructing `PreparedSite`. Do not put
GHC `Type` into an `Eq`/`Show` sidecar record or add ad-hoc textual equality.
`PreparedElaboration` and `PreparedModule` gain parallel prepared-site fields;
their existing `peYieldSites`/`pmYieldSites` remain for current consumers.

## 2. Type graph (producer: `TypePolicy`, projection lowering)

### 2.1 GHC-typed intermediate (elaboration side, `Tidepool.TypePolicy`)

```haskell
newtype TypeNodeId = TypeNodeId Word32          -- index into the graph
data TypeGraph = TypeGraph { tgNodes :: [TypeNodeG] }   -- index = id
data TypeNodeG
  = DataG TyCon [TypeNodeId] [(DataCon, [TypeNodeId])] -- ordered args, then constructor rows
  | TextG [DataCon] | IntegerG [DataCon] | NaturalG [DataCon] -- constructor dependencies
  | ScalarG RuntimeRep                          -- see below
  | UnconstructibleG Text Text                  -- reason, rendered type

-- Builder: hash-consed over normalized types; cycles allowed.
internType :: Type -> State TypeGraphBuilder TypeNodeId
```

Normalization before keying (all in `TypePolicy`, next to
`stabilizeEffectRows`):
1. Expand head synonyms with `coreView`. **First-slice contract amendment:**
   do not add family-instance environment plumbing in F3. A family application
   left after synonym expansion is `UnconstructibleG "type family" rendered`.
   This conservatively refuses types that the full contract would normalize;
   family normalization remains required before claiming its full acceptance
   scope. Never guess a family's representation.
2. Instantiate and erase newtypes to their representation type; the original
   name remains presentation only. Track instantiated newtype applications on
   the active normalization path and refuse an identical application re-entry
   as `"recursive newtype"`. Do not reject merely repeated TyCons: nested
   `Identity (Identity Int)` terminates. Limit one normalization path to 256
   steps and return `"type expansion limit"` on exhaustion, also bounding
   recursive newtypes whose arguments grow at every step. Return a typed
   refusal directly, never a residual expanded type for later rendering or
   hashing. Bound structural traversal before equality, indexing and rendering;
   a step budget alone does not bound exponentially growing arguments. Apply
   trailing newtype arguments with GHC's smart `mkAppTys` constructor.
3. Hash-cons the normalized `Type` in `GHC.Core.Map.Type.TypeMap TypeNodeId`.
   Allocate its node id before visiting arguments or fields, so ordinary
   recursive types (`[a]`, `Tree`) form cycles. An existing node returns
   immediately without descending again.
4. Exact-type hash-consing alone does not terminate expanding recursion such
   as `data Nest a = Nest (Nest [a])`. Bound a module graph to 65,536 nodes
   (including one reserved `UnconstructibleG "type expansion limit"` node)
   and new-node descent to 128 levels. On reaching either limit, use that
   refusal node and stop that expansion. Its rendered text is diagnostic only;
   sharing this node establishes no type equality. Depth/normalization limits
   apply before recursive work, including argument normalization.

Classification of a normalized type `ty`:
- `Text` (`Data.Text.Internal.Text`) → `TextG`; `Integer`
  (`GHC.Num.Integer.Integer`) → `IntegerG`; `Natural`
  (`GHC.Num.Natural.Natural`) → `NaturalG`. Matched by defining module and
  occurrence, not by rendered name. Carry every constructor of the leaf
  TyCon as a dependency (`Text`, `IS`/`IP`/`IN`, `NS`/`NB`, respectively);
  special leaves bypass ordinary field-graph expansion, not closure emission.
- `TyConApp tc args` with `isAlgTyCon tc && not (isPrimTyCon tc)` and every
  data constructor free of existentials and constraint/dictionary arguments
  (GHC's `isVanillaDataCon`, including absence of GADT equality constraints;
  pinned GHC exposes these through the third component of `dataConFullSig`,
  not a public `dataConEqSpec` accessor) → `DataG tc argumentNodes rows`, with all instantiated
  arguments recorded in order, including phantom and kind arguments. Each
  row's source field nodes come from `dataConInstOrigArgTys con args`; a constructor
  with `dataConUnivTyVars` arity mismatching `args` is a projection defect, not
  `Unconstructible`. `Map`, `Set` (`Data.Map.Internal.Map`,
  `Data.Set.Internal.Set`) and `Tidepool.Internal.ExitCell.ExitCell` are
  `UnconstructibleG` by name even though they are algebraic (host construction
  is not supported by the host builder).
- `TyConApp tc _` with `isPrimTyCon tc`: representation via the same
  `repsForType` policy projection uses; `IntRep _`/`WordRep _`/`FloatRep _`
  → `ScalarG rep` (THIS IS AN AMENDMENT to the contract's "unlifted fields
  other than the leaves are unconstructible": without it `Int`, `Char`,
  `Double` and records with scalar payloads could never be built, contradicting
  the contract's own acceptance list "scalar ... answers all resume"). Every
  other primitive (`Addr#`, `Array#`, `ByteArray#`, `MutVar#`, `State#`…) →
  `UnconstructibleG "primitive" rendered`.
- `FunTy`, `ForAllTy`, `TyVarTy`, `CastTy`, `CoercionTy`, `LitTy`, anything
  containing `Eff` (`Control.Monad.Freer.Internal.Eff`), and anything else →
  `UnconstructibleG reason rendered` with a short fixed reason word
  (`"function"`, `"polymorphic"`, `"effectful"`, `"type family"`,
  `"existential"`, `"constraint"`, `"primitive"`, `"unnormalized"`).

**Source fields versus runtime layout.** `ConstructorDecl.field_reps` comes
from flattened `dataConRepArgTys` (`ExecutionProjection.internConstructor`),
whereas bridge constructor values and the graph above have source fields.
F3 supports only a one-to-one mapping: each instantiated source field must
produce exactly one runtime representation, and the ordered representations
must equal the declaration's fields. No field may be unpacked, flattened,
removed as void, or split; constructor results must be `LiftedRef`. Check
GHC's representation/unpacking metadata as well as counts/reps, so equal
counts cannot hide a changed mapping. If any constructor fails this rule,
lower the whole Data node to `Unconstructible "layout" rendered` before
publishing any of its declarations. Ordinary boxed records, lists, Maybe,
Either and primitive scalar wrappers remain supported. A general
source-to-runtime field map is deferred; runtime-field rows must not silently
replace the source-field schema.

**Identity versus construction.** Ordered argument nodes are part of type
equality: `data P a = P` must distinguish `P Int` from `P Bool` even though
neither has fields. Unconstructible nodes (including unsupported phantom or
kind arguments) provide no equality authority. Future handle validation must
refuse equality if its comparison reaches such a node, or first add explicit
structural opaque evidence. Never compare their rendered text or reason to
approve a handle. This restriction does not make a supported nullary host
constructor require a value for a phantom argument.

Every `PreparedSite` records the root of its delivery evidence and the
roots of its live input types. The graph is built per module during
elaboration and stored on `PreparedElaboration`/`PreparedModule` as
`peTypeGraph`/`pmTypeGraph :: TypeGraph` (empty for recovered package bodies).
The sidecar's presentation-only `ysInputs` are not reparsed to recover types.

### 2.2 Wire (`ExecutionSchema.hs` + `execution_schema.rs`)

```haskell
newtype TypeNodeId = TypeNodeId Word32
data CtorRow = CtorRow { rowConstructor :: ConstructorId, rowFields :: [TypeNodeId] }
data TypeNode
  = TypeData SymbolIdentity [TypeNodeId] [CtorRow] -- family, ordered args, ALL constructor rows
  | TypeText | TypeInteger | TypeNatural
  | TypeScalar RuntimeRep
  | TypeUnconstructible Text Text          -- reason, rendered
data SiteDelivery = HostAnswer | LiveReentry | ExitCellFill | TerminalCapture
data SiteRow = SiteRow
  { siteId :: Word64, siteOrigin :: Text, siteOrdinal :: Word64
  , siteDelivery :: SiteDelivery, siteWire :: TypeNodeId, siteInputs :: [TypeNodeId] }
-- WireProgram gains: programTypes :: [TypeNode], programSites :: [SiteRow]
schemaVersion = 10
```

Rust mirror in `tidepool_repr::execution_schema`: `TypeNodeId(pub u32)` via
the existing id macro, `CtorRow { constructor: ConstructorId, fields:
Vec<TypeNodeId> }`, `enum TypeNode { Data { family: SymbolIdentity,
arguments: Vec<TypeNodeId>, rows: Vec<CtorRow> }, Text, Integer, Natural, Scalar(RuntimeRep),
Unconstructible { reason: String, rendered: String } }`, `enum SiteDelivery {
HostAnswer, LiveReentry, ExitCellFill, TerminalCapture }`, `SiteRow { site:
u64, origin: String, ordinal: u64, delivery: SiteDelivery, wire: TypeNodeId,
inputs: Vec<TypeNodeId> }`; `WireProgram { …, types: Vec<TypeNode>, sites:
Vec<SiteRow> }`; `PreparedProgram::types()`, `sites()`, `site(u64) ->
Option<&SiteRow>`, `type_node(TypeNodeId) -> Option<&TypeNode>`.
`SCHEMA_VERSION = 10`.

**Encoding** (definite-length arrays, same conventions as
`ExecutionEncode`'s `tag`/`tagged`/`array`/`list`): the program array grows
to 15 fields; fields 0–12 are unchanged, field 13 is `list encodeTypeNode
programTypes`, field 14 is `list encodeSiteRow programSites`.

```
TypeNode:   TypeData family args rows -> tagged 0 [encodeSymbol family, list encodeWord32 args, list encodeCtorRow rows]
            TypeText                -> tag 1
            TypeInteger             -> tag 2
            TypeNatural             -> tag 3
            TypeScalar rep          -> tagged 4 [encodeRep rep]
            TypeUnconstructible r t -> tagged 5 [encodeString r, encodeString t]
CtorRow:    array [encodeWord32 constructor, list encodeWord32 fields]
SiteRow:    array [encodeWord64 site, encodeString origin, encodeWord64 ordinal,
                   encodeWord (HostAnswer 0 | LiveReentry 1 | ExitCellFill 2 | TerminalCapture 3),
                   encodeWord32 wire, list encodeWord32 inputs]
```

`tag n` / `tagged n [..]` are whatever `ExecutionEncode` already uses for
sum types (read `encodeRep` and mirror it; the Rust decoder's `tagged`
reader is the counterpart in `codec.rs`).

### 2.3 Lowering (projection, `ExecutionProjection`)

After all modules are projected (in `projectPreparedWithTopSymbols`, before
the `WireProgram` is assembled), select and lower prepared evidence:

- Filter `PreparedSite`s by `psOwner` against the actual selected executable
  top closure, using the same binder identity/recovery exclusion as
  `pmSiteRejections` in `projectPreparedTarget` (ExecutionProjection:236-247).
  A retained import's unexecuted body and an unrelated polymorphic helper
  contribute neither artifact site rows nor constructor dependencies. Never
  infer ownership from rendered origin strings.
- Starting from the selected sites' wire and input roots, retain only reachable
  graph nodes (including argument and field edges). Compact/rebase the selected
  per-module graphs into one program table in module order, then original node
  order. Rewrite every argument, field and site reference through that mapping.
  No cross-module hash-consing is required.
- Emit one site row per selected stable id. Repeated references to an existing
  `PreparedSite` do not create new rows; two distinct selected site records
  claiming the same id are a typed projection defect, even if their presentation
  text matches. A wrapper's reused runtime site id does not authorize copying
  its definition into a second metadata row. Unselected generic wrapper sites
  must be filtered before collision checks.
- `DataG tc args rows` → `TypeData (nameSymbol "type" (tyConName tc)) args [CtorRow
  (internConstructor con) fields | (con, fields) <- rows]`. Interning here
  is what declares `Left` even when no code matches it (decision 3). Do it
  for EVERY `DataG` node reachable from EVERY site (all four deliveries),
  not only `HostAnswer`/`LiveReentry`. Reachability includes argument nodes
  and source-field nodes. The existing Core table minting/merge path remains
  the authority for host ids; verify new closure-only declarations participate
  in the shared table contract.
- `TextG`/`IntegerG`/`NaturalG` lower to the unchanged leaf wire tags, but
  first intern all recorded constructor dependencies, even when no executable
  code builds or matches them. Do not traverse their byte-array/scalar fields
  as ordinary Data fields. Failure to declare a required leaf constructor
  turns the leaf into `TypeUnconstructible "representation" rendered`, with
  the same transactional rollback as Data nodes. F5 may then resolve the
  authoritative constructor declarations through the interner; it must not
  invent missing descriptors.
- If `internConstructor` fails (`failRepresentation`) for any constructor of
  a `DataG` node, that WHOLE node becomes `TypeUnconstructible
  "representation" rendered` and projection continues: wrap the node's
  interning in a state-restoring `tryP :: P a -> P (Either ProjectionError a)`
  (`StateT` over `Either`: run the inner computation on the current state,
  on `Left` keep the pre-state). A `TypeUnconstructible` node's rows are
  gone, so nothing half-interned leaks. Apply the source/runtime layout test
  in the same transaction. Catch only the representation/layout refusals
  authorized here; identity defects and unrelated projection errors propagate.
- Nodes made unreachable by a later layout/representation refusal may remain
  after initial reachability selection; they remain valid declarations and
  are not authority for constructing through the refused node.
- The Core `asks.json` sidecar is unchanged.

### 2.4 Validation (`validation.rs`, `DecodeLimits`)

- `DecodeLimits` gains `max_type_nodes` and `max_sites` (defaults sized like
  the existing table limits; charge work per node/row).
- Every `TypeNodeId` in Data arguments, a `CtorRow` or `SiteRow` is `< types.len()`; every
  `CtorRow.constructor` is `< constructors.len()`.
- For `TypeNode::Data { family, arguments, rows }`: validate/charge argument
  references independently of field references. For nonempty rows, `rows.len() == family_size` of its
  constructors; the rows are in ascending `tag` order 1..=family_size; every
  row's constructor has `ConstructorDecl.family == family`; `rows[i].fields.
  len() == constructors[rows[i].constructor].field_reps.len()`; for each
  field position, if the field's node is `Scalar(rep)` then
  `field_reps[i] == rep`, and if the field's node is `Data`/`Text`/`Integer`/
  `Natural` then `field_reps[i]` is `LiftedRef` (Text/Integer/Natural are
  lifted boxes) — otherwise `ParseError::InvalidLayout("type node field
  representation")`. A field pointing to Unconstructible carries no
  representation assertion; any host construction reaching it is refused.
  An empty constructor family may have an empty row list and admits no host
  constructor values.
- `TypeNode::Scalar(rep)` must be `Int(_)`/`Word(_)`/`Float(_)`.
- `TypeNode::Unconstructible { reason, .. }`: `reason` non-empty.
- `SiteRow.site` unique across the table (`DuplicateDefinition("site")`);
  `site != 0`.
- Schema version other than 10 → `ParseError::UnsupportedVersion(v)`. Read
  and validate magic/version before enforcing schema 10's 15-field array
  shape: the current decoder checks its expected array length first. Pin this
  ordering with a schema-9-shaped 13-field array that must return
  `UnsupportedVersion(9)`, not a generic array-length failure.

## 3. `host_id` index (`tidepool-codegen/src/prepared_program/interner.rs`)

```rust
pub struct DescriptorInterner {
    constructors: BTreeMap<SymbolIdentity, (ConstructorDecl, Arc<ObjectDescriptor>)>,
    by_host: BTreeMap<DataConId, SymbolIdentity>,
}
```

- `intern`: after the identity check, if `by_host.get(&declaration.host_id)`
  is `Some(other)` with `other != declaration.identity` →
  `CompileError::HostIdConflict { host_id, identity: Box<SymbolIdentity>,
  existing: Box<SymbolIdentity> }` (new variant, rendered as "constructor
  host id … already names …"). Insert into both maps together.
- `absorb`: the first loop additionally checks `by_host` conflicts for every
  entry (and conflicts WITHIN `entries` themselves, which the per-artifact
  validation already rules out but is cheap to re-check); nothing is
  absorbed if any check fails (all-or-nothing is preserved). The error type
  of `absorb` becomes an enum `AbsorbConflict { Identity(SymbolIdentity),
  HostId { host_id, identity, existing } }`; update the one caller.
- New accessor: `pub fn by_host(&self, id: DataConId) -> Option<&(ConstructorDecl,
  Arc<ObjectDescriptor>)>` (the answer builder in F5 resolves a bridge
  `Value`'s `DataConId` through it). Add no identity lookup accessor before
  a production consumer needs it.
- Tests: intern two declarations with the same `host_id` and different
  identities → `HostIdConflict`; absorb with a host conflict absorbs nothing;
  a re-intern of an identical declaration is idempotent in both maps.

## 4. Qualified freer resolution (test path only)

`tidepool-runtime/tests/prepared_execution.rs` (and `prepared_resident_
composite.rs`) resolve `E`/`Val`/`Union` by bare occurrence from the
artifact's `constructors()`. Replace with a lookup by defining module AND
occurrence using constants added to `tidepool_repr::freer_names`:
`VAL_DEFINING_MODULE = "Control.Monad.Freer.Internal"`, `E_DEFINING_MODULE`
(same), `UNION_DEFINING_MODULE = "Data.OpenUnion.Internal"`,
`LEAF_DEFINING_MODULE = NODE_DEFINING_MODULE = "Data.FTCQueue"` — verify each
against the `SymbolIdentity.module` the committed fixtures declare (grep the
fixture's decoded constructors in the test) and correct the constant if the
fixture says otherwise; plus one helper
`pub fn find_declared<'a>(constructors: &'a [ConstructorDecl], module: &str,
occurrence: &str) -> Option<&'a ConstructorDecl>`. The existing
`freer_names::resolve` over `DataConTable` is unchanged.

## 5. Sequencing

1. Haskell producer and Rust reader land together (they cannot be tested
   against each other until both exist); each is unit-tested on its own side
   (Haskell: `prepared-stg-pipeline-test` gains a site-row case; Rust: codec
   round-trip through hand-built `WireProgram`s, the validation rejections
   above, `Either Int Text` vs `Either Text Int` producing different node
   rows, phantom `P Int` versus `P Bool` retaining distinct arguments).
   Producer regressions cover every delivery row (especially `forkCata`,
   `serve`, progress variants and `finalize`), UNPACK scalar/product layout
   refusals, recursive newtypes, expanding recursion, and leaf constructor
   closure when no code builds or matches the answer type. Also pin that
   unrelated helper sites are absent and selected duplicate ids are refused.
2. `just fixtures-update` regenerates the prepared corpus (schema 10) and
   `just fixtures-check` passes; the seven prepared fixtures in
   `FreerRetention.md` are regenerated with it; the corpus oracle is
   resealed by its script. Until this step, every schema-9 fixture is
   rejected with `UnsupportedVersion(9)`: that is expected and is the reason
   the two halves and the regeneration are one wave.
3. `interner.rs` and the freer-name lookup are independent of 1–2 and may
   land first.

## Not in this schema

`settleEff`, `FrameEvidence`, parking, host construction (`answer.rs`), and
`RootedValueRef` provenance are decisions 4–8 and follow in F4/F5.
