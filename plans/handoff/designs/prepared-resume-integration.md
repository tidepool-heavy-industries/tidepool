# Prepared resume integration: owners and acceptance

This refines F4/F5 of `../next-wave-2026-09-15b.md` against the F1/F2
implementation. It preserves `resume-contract-v2.md`'s peek, validate, build,
take, enter sequence. The reviewed F3 wire contract is `schema-10.md`.
Implementation starts after producer, decoder and fixtures agree.

## Two program identities on a parked turn

Consider three notebook turns:

1. A defines and retains an effectful closure with typed site S.
2. B calls that closure, adds more continuation work, and suspends at S.
3. C produces an answer and resumes B.

The answer evidence belongs to A; the resume/settlement entry and notebook
completion obligation belong to B. C supplies a value and changes neither.
An invocation's `ProgramId` is therefore insufficient to select site evidence.
The retained continuation can reach code from both A and B.

`PreparedMachine` owns this distinction. Preserve immutable site/type metadata
with each compiled/installed program. Extend the existing install transaction
with a machine-owned site index. Resolve an observed site through that index
to an opaque witness naming the installed evidence owner. Do not create a
parallel resume-authority map in the resident session or use presentation-only
`ProgramProvenance` as authority.

A missing site is a typed refusal. A duplicate ID can share an evidence owner
only after structural equivalence is established across program-local tables:
compare delivery, input/wire types, full constructor identities and ordered
arguments, never local node numbers or rendered names. If equivalence cannot
be established, installation refuses before publication. The evidence witness
keeps its owner; installing a newer copy does not retarget an existing frame.
Canonical-owner replacement when that program retires belongs to the lifetime
owner and requires the same evidence proof.

## Frame and root ownership

`ResourceLedger` remains the sole continuation identity/consumption owner.
Extend frame evidence with Core or prepared evidence. A prepared frame must
also retain the runner context for B: admitted resume/settlement entry and
completion policy. The evidence program and runner program may differ.

Retirement pins include the evidence owner, runner, and programs reached by
continuation/live-payload heap edges. A prepared value's future type evidence
is `(ProgramId, TypeNodeId)`, not an artifact-local index alone. Do not enable
program retirement before these edges are in the census.

Keep the existing resident hole/completion machinery. Bindings, generations,
lexical scopes, provenance and output capture remain with
`PersistentSession`/`ResidentSession`. The prepared projection completion plan
has per-field data/closure tiers; converting it to a field count would discard
required forcing policy. Initial and resumed projection must preserve those
tiers and the initiating scope/generation.

## Explicit executable entries

`Tidepool.Internal.Resume.resumeLifted` already exists. It is not admitted by
current turn templates: `prepared_scaffold_binding` emits only
`__prepared = TidepoolResume.settle __result`, and the extractor projects only
that root. Importing the module does not retain an unreachable entry.

Add an explicit auxiliary projection root for a generated resume wrapper.
Its row-specific shape is:

```haskell
__prepared = TidepoolResume.settle (settleEff __result)
__resume q x =
  TidepoolResume.settle (settleEff (TidepoolResume.resumeLifted q x))
```

The template/compiler owner establishes the row and entry signatures. The
artifact must retain both entries through an explicit root set, rather than
an artificial reference intended to survive optimization. This requires
checking the actual generated artifact, not merely the source text. The wire
program still has one initial entry; auxiliary admitted tops are existing
compiled entries, not another notebook frontend.

Factor prepared settlement into invocation plus a single settled-layer
decoder. Initial and resumed invocations pass through that decoder and one
completion routine. The generated effects owner supplies request forcing;
Rust does not reconstruct freer computations or parse Haskell signatures.
The precise forcing-generation parcel follows a source survey of existing
protocol metadata and the effects-module generator.

## Ordinary handled effects also need answer evidence

The dynamic site table alone covers only the typed suspension vocabulary.
Ordinary requests such as Print and file reads carry no `typedSite`. Core
materializes handler replies directly from `Response::Complete(Value)` or
`Response::List`; neither response carries the expected instantiated Haskell
result type. Prepared handlers therefore need static reply contracts before
using the same validated answer builder.

Use explicit synthetic prepared sites, reusing schema 10's `SiteRow` and
`TypeNode` tables. The protocol generator derives the complete result from
`Verb::result_type()` (including the error/Either wrapper), substitutes the
compiled row's parameters, and emits a typed internal evidence marker
`replySite @ReplyType q`. Its type ties evidence to the captured continuation:

```haskell
replySite :: forall reply row result. Arrs row reply result -> Int
replySiteSited :: forall reply row result. Int -> Arrs row reply result -> Int
```

The sibling returns its supplied site ID without forcing `q`. A phantom
`forall reply. Int` marker would not check that the declared reply type matches
the actual continuation. Prepared elaboration recognizes and rewrites the
marker before type erasure, records a HostAnswer site, and retains its exact
owner. The marker belongs to the prepared-only compiler vocabulary; it does
not join the public `sitedVerbs` or the Core presentation sidecar.

The generated settlement branch carries an explicit evidence-source sum:
`StaticReply site` or `DynamicTypedSite`. It constructs the static witness
alongside that branch's request and continuation. Do not infer this distinction
from an integer sentinel, request name, or response contents. A dynamic site's
actual ID still comes from the observed typed request. The template's admitted
roots must preserve the marker-owning settlement function.

Static contracts authorize result shape only. Existing nominal dispatch and
`EffectRunPolicy` remain unchanged: handled requests use the same validator,
answer builder and resume entry as external answers; HandleOrSuspend parks
only unhandled requests, and HandleOrError refuses unhandled requests.
Preserve the iterative list-response path.

The Bool-first request uses native Bool constructors: the generated
`runLLMTurnSited` maps `unsafeCoerce` over the nominal Value reply, so it must
receive `True`/`False`, not a JSON Value constructor wrapping a Bool. The real
resume test establishes this representation contract.

## Request forcing belongs to the generated schema owner

`EffectDecl` currently carries rendered declaration strings. Extend the
existing protocol declaration generator (`tidepool-protocol/src/gen/decl_rs.rs`)
to produce a per-effect forcing function from structured `Verb.args`, `HsType`
and `RustBinding`. Carry that generated source/name through `EffectDecl`;
`effects_shim_module_source` assembles row-specific settlement from the same
ordered row used to declare `M`. Do not parse signature strings or add a second
tag registry. Legacy macro-owned effects require equivalent generated metadata.

Force concrete data fields fully, using schema-generated supporting instances
and instances in foreign types' defining modules. Vendored Value, Scientific,
Duration and generated records do not all have NFData today; blindly emitting
`deepseq` is not a complete implementation. Unsupported concrete field shapes
must fail generation explicitly. CoreValue fields are forced only to WHNF,
without adding constraints to arbitrary authored types.

CoreValue is not live-retention authority. RunLLMTurnWith's ordinary JSON
payload is CoreValue, and its nested site ID is materialized by observation.
Only `LivePayloadPolicy` selects a field for retained heap custody. Do not turn
every CoreValue into an opaque handle or stop observing ordinary JSON data.

## Rejection and cleanup

Before consuming the frame, verify realm, delivery and structural answer type,
then build/root the answer. Host construction resolves constructors through
the machine interner, reserves once, and rolls nursery/external allocations
back on failure. Invalid input leaves the same frame resumable with unchanged
resource counts and a clear machine latch.

Only after preparation succeeds does the ledger take the continuation. Enter
the recorded runner's resume wrapper, then settle through the shared routine.
A new suspension resolves its newly observed site afresh. Abort consumes the
frame without entering it. Unknown-site or malformed-request failure before
parking releases all temporary request/continuation handles.

## Bounded delegation after F3

The lead fixes the shared frame/evidence and invocation interfaces first.
Then independent parcels may implement:

- Generator/template auxiliary roots and forcing from fixed schema metadata.
- Mechanical Core `frame.table` migration to `FrameEvidence::Core`, retaining
  existing Core tests and policy behavior.
- Codec/fixture propagation and acceptance tests against the delivered API.

Machine parking, root custody, allocation rollback and the cross-program
provenance contract require lead integration and fresh review. Do not split
those owners into independently invented registries or APIs.

## Acceptance before widening routing

1. A real prepared notebook Bool request parks and resumes to the expected
   value; string/number answers are rejected without consuming it.
2. B invokes A's retained effectful closure without a local copy of S; C
   resumes it after unrelated installation. Evidence comes from A and binding
   completion uses B's original scope/generation.
3. Resumption suspends again at a different site; that site's evidence is used.
4. Duplicate conflicting site installation leaves program/root/index counts
   and the machine latch unchanged.
5. Unknown-site parking failure releases temporary handles.
6. A projected bind with mixed data/closure tiers behaves the same on initial
   completion and completion after suspension.
7. Actual turn artifacts contain the required resume and forcing entries.
8. A consumed continuation cannot resume twice; cancellation/abort and realm
   retirement clean up exactly once without disturbing a sibling.
9. Ordinary effects before and after a typed suspension route through actual
   handlers: Print/Unit, file-read/Either error, and optional/list results.
   Verify all three EffectRunPolicy modes.
10. Evidence and runner owners remain pinned until completion/abort; retirement
   cannot free code reached by the continuation.

The existing freer fixture loop remains supporting engine evidence. It does
not replace the production resident/actor request path in step 2's exit.
