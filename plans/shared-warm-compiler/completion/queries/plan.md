# Query observations, usable discovery and production acceptance

Status: implementation plan, not new execution evidence. Read [the campaign
plan](../plan.md) and [locked contracts](../contracts.md) first. Review baseline:
`6f7ae09b3bdbcfd8952d9a513db91564ea1b5858`, code tip `92fa518105`.
Record the actual received implementation base and any changed contracts at kickoff.
This leaf belongs to verification integration; it supplies the query portion of
the early Persona gate and remains part of the full mandatory campaign.

## Outcome and dependencies

An installed Persona discovers a callable through meaningful structured answers,
invokes that operation through its real effect handler, and continues in later
cells. All five verbs execute through the sole supported actor cell route, using
the actual compiler service where compiler knowledge is required. Query answers
preserve their precise observation, refusal and authority distinctions.

The runtime-publication and service-cells leaves must first supply the exclusive
world-cell entry point, complete supported cell syntax and actual item settlement.
The finite resident cell path is deleted before this leaf claims early Persona
acceptance. Native-selection supplies exact native dependency preservation and
the A1c repair. Lifecycle supplies fresh source admission and late-result refusal.
Queries may develop against those contracts, but a legacy hosted pass cannot
qualify their join. Reuse existing compiler transactions, END settlement, world
acquisition, checked output, evaluator, machine checkout and hosted tests. Add no
query framework, certifier, dispatcher, inventory or test harness.

## Owners and edited surfaces

| Owner | Exact owning files and responsibility |
|---|---|
| Query leaf | `exomonad/actor/src/resident_workbench/compiler_query.rs`, `world_enumeration.rs`, `structured_introspection.rs`: request planning, typed answer projection and query continuation. `exomonad/actor/src/resident_workbench.rs`: existing decode/resume integration and owning request tests; coordinate shared edits with runtime-publication. |
| Runtime-publication, with query review | `tidepool/runtime/src/session/inspection.rs`, `session/mod.rs`, actor `resident_workbench.rs` and `mount.rs`: semantic view observations, admitted binding enumeration and source/continuation admission. Runtime owns semantic identity; the query leaf does not invent a parallel world ID. |
| Compiler/service owner, with query tests | `bridge/haskell/src/Tidepool/Introspection.hs`, `InspectionRunner.hs`, `CompilerRequest.hs`, `bridge/haskell/app/Main.hs`: real GHC inspection, typed probe results, availability and search. `tidepool/runtime/src/session/inspection.rs` and `tidepool/extract-cmd/src/{lib,request}.rs`: the existing admitted transport. |
| Query/docs leaf | `bridge/protocol/src/effects/introspection.rs`, generated mirrors through their generator, `bridge/haskell/lib/Tidepool/Introspection.hs`: typed public contract. `exomonad/prompts/api-guide.md`, `exomonad/host/src/actor_host/fixtures/api-guide/introspection.hs`, `documentation_tests.rs`: matching published source and executed consumer. |
| Hosted acceptance | `exomonad/host/src/actor_host/startup_persona_acceptance_tests.rs`, its adjacent authored fixtures and existing hosted campaign helpers: actual installed Persona/tool/output path. `bridge/haskell/test-prepared-stg/IntrospectionFrontDoor.hs` and its `bridge/haskell/BUCK` compile-only target share the authored module with `exomonad/host/src/actor_host/documentation_tests.rs`; compilation does not establish resident effect execution. |

Read each nearest contributor guide before editing. Changes to declared Haskell
fixtures must update their existing Buck input owner/generator. Do not edit
generated protocol mirrors directly.

## Honest contracts

The five verbs share the `Introspection` effect, not one implementation strategy.
At baseline, three require real GHC and two answer from runtime-held facts:

| Verb | Required observation and returned semantic fields |
|---|---|
| `currentScopeView` | Entry snapshot of the admitted compile view. `ScopeProvenance` describes that observation; it is not compiler-world authority or a claim that nothing changes before the next request. No GHC query is needed. |
| `scopeBindings expected` | Published binding identities in one admitted scope, stable sorted/deduplicated refs, exact observed count, emitted limit and provenance. It does not promise the imported GHC reader environment or public-module export enumeration. No compiler request is needed. |
| `rowFit expected identifier` | Identifier, four-way advisory availability and exact view. For live targets, consume GHC-typed signatures and constraint checks against the actor's admitted effect row. A missing canonical identity returns `FitUnknown` from the actual queried environment; it must not inspect a replacement with the same spelling or browse a module as a fallback. |
| `typeOfExpression expected source` | Compile/check the probe, return original source, checked type display, advisory availability and exact view. Never evaluate the probe. Invalid source/type constraints produce the actual typed refusal. |
| `typeSearch expected pattern` | Compile/check the pattern, match visible value identifiers, return usable source spelling, canonical reference, signature, quality, availability, dependency references and provenance. Search is over the actual admitted environment, not a hardcoded catalogue. |

The implemented compiler-backed effect route is the closed World query operation:
actual Workbench capture, runtime source admission, held lexical observation,
checking-only GHC stage, one lower answer decode, exact compiler END, and final
source/view validation in the checkout that resumes the effect. Public-module
info/type share the World transport and retain precise module receipts. Scoped
info/type, rowFit, expression and search use the actual retained observation
scaffold. No actor effect falls back to the finite inspection batch or a browse
then filter. See [the context contract](../contracts/WORLD-OBSERVATION-CONTEXT.md)
for worker-v27 pairing, private input TYPE custody and operand confinement.
Compilation/local decoder controls are separate from actual hosted acceptance.

`rowFit` is evidence about type/row compatibility, not permission to read a file,
spawn a process or consume another resource. The Haskell implementation currently
substitutes the actor row and uses GHC constraint solving. Preserve `Available`,
`Polymorphic`, `Unavailable` and `Unknown`; absence is not unavailability. Likewise,
type-search `Usable` denotes the implemented type-matching relation, not proof that
every later instantiation/application will compile or have resource grants.

The search budget bounds emitted results. GHC currently matches and ranks the
environment before truncation. A full-budget observed count is a lower bound;
do not advertise a true total or a work/time bound. Binding enumeration instead
reports its exact deduplicated count and truncates only emitted refs. Keep the
existing presentation budget explicit; do not raise it to conceal missing discovery.

Keep structured types: `IdentifierInfo`/`DeclarationInfo`, `TypeExpression`,
`IdentifierRef`, `Fit`, `ExpressionType`, `TypeMatches`, `NameRefList` and typed
`QueryError`. Identity-only enumeration must not fabricate signatures, references
or availability. Detail queries and matched compiler results supply those facts;
do not add nullable guesses or generic documentation maps.

## Representation and demand changes

Apply [quality Q9](../quality/plan.md). Admit World answer structure once into a
coupled typed outcome; raw retained bytes are evidence, not another internal
interpretation. Resolve the current `Absent + Unsupported` validator disagreement.
Keep wire-domain types below runtime and its association/END/view checks in runtime.

Carry one semantic view pin issued by the runtime owner across public `info` and
all compiler-query verbs, separately from the compiler-world receipt identity.
The differing current fingerprint schemes are a source-confirmed mismatch whose
cross-verb control must execute. Do not fix it by ignoring fingerprints.

Replace `rowFit`'s broad browse-and-filter plan with a keyed question on the same
compiler service and existing GHC solver. Resolve public aliases/re-exports to
canonical identity before checking; preserve namespace, ambiguity and stale-view
refusals. Reuse the held-world resolver's failure-versus-closed-absence distinction;
do not convert any module-loading `SourceError` into `UnknownModule`. This is
bounded work removal, not a second lookup engine. Use existing
solver functions for a test oracle without keeping a callable legacy resident
route. Pin/answer correctness belongs to G2; measured keyed work removal to G3.

## Observation and continuation changes

1. Consume the runtime owner's semantic compile-view identity. Scope labels are
   presentation; `CurrentScope` and `PublicModule` labels alone do not make the
   same view stale. Allocation-only generation reservations do not change semantic
   provenance. Actual private-prefix binding/import/declaration/source changes do.
   Preserve the whole-cell contract: private items can observe earlier private
   items, but failure/cancellation publishes no new cell names. Earlier committed
   cells and completed external effect receipts remain intact.
2. `currentScopeView` stays an entry snapshot. Snapshot-only `rowFit` outcomes are
   likewise explicitly identified as entry observations. Do not add a universal
   post-reply promise to these immediate results.
3. For compiler-backed answers, compare a supplied pin at entry before compiler
   admission. Capture the precise semantic view, release the machine while GHC
   runs, then compare it inside the existing continuation checkout immediately
   before guest resumption. Drift refuses for both `Nothing` and `Just view`.
   Preserve pinned-entry mismatch versus movement during work in the error's
   before/after observations. Source revocation/uncertain END must retain their
   actual refusal and owners; successful compiler text is not publication authority.
4. For `scopeBindings`, perform its final view comparison and guest resumption
   inside one existing `with_continuation_machine` checkout. Baseline separately
   checks and resumes; remove that structural gap without holding a checkout over
   an asynchronous wait or introducing another lock/admission abstraction.
5. Preserve real error distinctions: unknown identifier/module, ambiguous query,
   unsupported declaration/source, expression rejection, unavailable compiler,
   source revocation and view movement. Baseline probe diagnostics have error
   severity and text but no usable span; document that until the compiler supplies
   actual structured locations. Do not fabricate spans or classify errors by
   rendered message text.

## Production-route acceptance

Extend the existing hosted Persona scenario at
`startup_persona_acceptance_tests.rs::persona_with_shared_workspace_keeps_effects_and_later_cells_usable`.
Use its real source-selected spec, shared-source Gitlink, startup memory/context,
scripted provider, named `haskell` operations, `while_operation_succeeds`, and
retained Store output receipts. Scripted inference is allowed; canned query
responses or replacement effect handlers are not. Record existing operation,
world, worker, checked output and settlement identities to prove the new route.

The first discovery cell must actually invoke all five helpers and consume their
semantic fields. Seed known declarations through ordinary accepted prior cells:
a pure `Int -> Int` callable, a polymorphic callable, and effectful callables whose
required effects respectively fit and do not fit the installed actor row. Use an
existing narrowed Persona fixture if needed; do not mint effect/resource authority
for the test. Assert canonical refs and namespace, expected type/signature,
availability, match quality, provenance and exact counts for the bounded fixture.
Chain the discovered identity into detail/fit inspection. Invoke a discovered
allowed operation through its real handler, check its visible result, then reuse
the discovery and bound values in a later cell. A successful constant result,
`Right _`, operation-name list or raw record display is insufficient.

Use existing actor request tests for the bounded failure histories below. All
claim-bearing cases enter through compiled guest requests, real boundary decode,
the production resume method and guest field/error projection:

| History | Required discriminating evidence |
|---|---|
| All-five success and later use | Real nonempty references/matches and exact semantic fields; positive allowed operation executes. No fake dispatch or generic `Right` acceptance. |
| Fit controls | A known available callable, missing-effect callable, polymorphic constraints, unknown identifier, namespace mismatch and stale session identifier. Preserve four outcomes and canonical identity; resource grants remain independently enforced. |
| Probe rejection/no evaluation | Invalid expression, malformed type pattern and unknown spelling return their actual typed errors. Type-check a probe whose evaluation would cause an observable effect, and prove that effect never occurs. Required compiler failure must fail the test, not skip. |
| Pin and asynchronous drift | Stale pin refuses before a new physical compiler request. Hold an actual admitted compiler operation at an existing bounded seam, change semantic state through its owning publication path, release it and observe `ScopeChanged` in guest code for pinned and unpinned requests. Do not manufacture a trusted view receipt. |
| Allocation versus semantics | Burn an allocation reservation without publishing semantic changes: pin remains valid. Publish actual binding/source change: pin becomes stale. Also check a scope-label-only change stays the same view. |
| Binding enumeration | Compare full `(module,name,namespace)` identities against an independent expected set, including shadow/removal/private-prefix behavior. More than 256 actual bindings gives exact observed count and bounded emitted refs; snapshot/read issues no compiler query. |
| Search cap | Populate more than the response budget with real matching declarations. Assert bounded output and lower-bound semantics, canonical identities and deterministic rank. Worker traces must not be interpreted as a search-work bound. |
| Refusal and recovery | Incomplete source, revoked source, unavailable compiler and uncertain END keep distinct results/cleanup ownership. After settled refusals, a valid subsequent query/cell remains usable. Cold restoration/worker replacement queries the same authenticated logical product. |

Use operation terminal barriers, bounded compiler traces and existing fixture
seams. Do not assert process-global spawn counts under unrelated concurrent work;
correlate existing operation/compiler receipts or isolate the counted interval.
Raw malformed representation fixtures remain valid decoder/authenticator controls,
but cannot become positive compiler/world/native authority.

## Published example and historical corrections

Replace the authored `introspection.hs` source and its matching `source=introspection`
guide fence together with a complete compiled example that projects semantic
fields, handles typed errors, uses `Nothing`/`Just view` correctly and invokes a
discovered operation. Keep the installed Persona's actual effect row and grants
visible in what the example claims. The current raw `display detail` deliberately
has no specialized declaration rendering; it cannot teach usable discovery alone.
If a helper module owns the example, declare and execute that exact source through
the existing fixture machinery rather than suggesting a nonexistent import.

`IntrospectionFrontDoor.frontDoor` is shared by the declared compile-only
prepared fixture and the hosted documentation campaign. Its effectful `result`
specialization is no longer a constant execution test. Acceptance requires the
hosted campaign to consume the actual symbol, type, availability and scope
fields for all five verbs, invoke the discovered ordinary operation through its
runtime interpreter, and refuse a real prior pin after publication. Building
the shared source is compilation evidence only. The actor dispatch fixture currently
checks four `Right` envelopes, including absent-binding `FitUnknown`; improve its
semantic assertions or clearly retain it as dispatch-only coverage.

A1c reached the initial published display check, then failed compiling the later
structured probe because an exact native dependency was unavailable. That is a
native-selection/output-closure investigation, not proof that the module or query
is absent, and not an expression rejection. Preserve the initial display success
and later native failure as separate facts. Coordinate the repair and rerun the
same structured semantic oracle; do not broaden available inventory or weaken
the assertion. The older `reviews/A1C-TIP-FINDING.md` and milestone language that
the example never ran are superseded by this distinction.

As implementation lands, update `plans/compiler-query-effects.md` and the relevant
query comments/guidance, replacing obsolete claims rather than adding warnings.
In particular: `mid-plan-notes/MILESTONE.md` claims universal entry/post-reply
fencing; compiler-query comments claim the full provenance triple defines identity;
older budget wording says search stops at the response limit. None is the locked
contract. Keep historical logs unchanged and label their actual execution scope.

Recheck production consumers before deleting unused surface. At the reviewed base,
`InspectionQuery::BrowseBounded` has no producer outside structural tests; remove
its unused variant and corresponding extract request/worker transport support if
that remains true at implementation base. Keep complete browse while `rowFit`
still needs it. Runtime inspection's exported `ViewIdentity` and `view_identity()`
have no consumers; remove that unused wrapper if confirmed, retaining the owning
semantic identity comparison. Follow protocol/version owners for wire changes;
do not confuse this with the unrelated mount-namespace `ViewIdentity`.

## Verification, review and handoff

Compile affected actor/runtime/host test configurations and Haskell consumers
early through the admitted pinned repository entrypoints. Run exact nonzero query
selectors, the actual all-five Persona case, affected documentation examples and
source-equality/prompt-catalog tests. Run protocol snapshots when schema/generated
surface changes, and required fixture checks after compiler serialization changes.
Use the owning guides' native targets; no routine full `just verify`.

Independent review checks the cumulative candidate: actual GHC/service consumption,
no finite resident fallback, no authority fabrication, semantic provenance versus
allocation, snapshot versus async fences, real positive/negative queries, public
example usefulness and surviving dead routes. The qualification owner then runs
the joined example and Persona history on the exact frozen bundle. Component
passes do not substitute for that gate.

Retain base/head, actual source/bundle/artifact identities, exact commands,
selected/executed counts, terminal status, semantic projections and bounded trace
locations under the campaign evidence owner. Report dispatch, compilation,
semantic execution and frozen qualification separately. No newly executed pass is
claimed by this plan.

No unresolved public query semantic decision remains. One shared implementation
dependency needs an explicit received API: the runtime-publication owner must
supply semantic compile-view provenance that remains stable across allocation-only
reservations and changes with actual private-prefix source/binding facts. Escalate
if existing owners cannot expose that observation without reconstructing authority;
do not invent a second query identity or reuse the allocation counter.


## Deletion ledger

| Surviving path | Owner and concrete follow-up | Acceptance boundary |
|---|---|---|
| `resident_workbench.rs` synchronous status `Bindings` branch → `inspect_actor_batch` → `inspect_compile_view` → runtime `run_admitted_inspections` | D's separately reviewed `fde2a6db47` + `1e9ef6f488` + `e26d6384f6` replace the actor status fallback with asynchronous held-World observation and delete the actor finite helpers. Receive and verify that parcel alongside this query wave; remove the now-unconsumed runtime export separately after checking its remaining consumers. Preserve honest unavailable display and typed operational errors. | This status path supplies no all-five or Persona qualification. Its actual joined race control currently fails at observation parsing before reaching publication/final validation; that failure is an open producer gate, not evidence about the race. |

The obsolete `compiler-query-verbs.hs` / `compiler_query_verbs_decode_dispatch_and_resume` four-Right smoke is deleted, including its fixture catalog/input registrations. Its runtime replacement is the declared `facade_introspection_service_test` semantic hosted program, whose execution remains mandatory and unqualified until run. The shared authored program consumes all five semantic results and the discovered operation, checks genuine canonical absent-name `FitUnknown`, rejects malformed canonical identity with a typed admission refusal, and checks a retained pin after a real publication. The actual GHC selector leaf independently tests namespace/qualification/ambiguity; lower codec tests cover wrong World/question/pin/kind/candidate framing. These local controls do not substitute for the hosted effect loop or invalid-expression/source/closed-owner campaign controls.
