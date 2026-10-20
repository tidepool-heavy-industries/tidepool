# Make the compiler boundary carry its invariants

Status: source-reviewed design obligations, not implemented or execution-tested.
Reviewed plan checkpoint `3e21ebd0da3e5fcfa8e7e81caffc8338c27ff3be`, code
`92fa518105`, on `integration/compiler-query-warm-final`. Read the
[campaign plan](../plan.md) and [contracts](../contracts.md) first.

This is a refinement of the existing completion work, not a separate framework
or a second implementation queue. Each receiving leaf owns the relevant changes
below while replacing its current boundary. Five Sol reviewers traced independent
seams; the coordinator checked producers, consumers and the proposed deletions.
Detailed source/history notes are retained under
`/srv/swarm/evidence/inanna/shared-warm-compiler-20261019/idioms-review/`.
Source locations below refer to the reviewed code, not future line numbers.

## Judgment and scope

Keep the useful foundations: immutable restoration roots, exact native dependency
edges, runtime-issued admission, private checked-output constructors, affine
compiler transactions, and whole-cell typechecking before effects. Several
stronger checks correctly exposed invalid old fixtures. Do not remove those
checks to recover old passing counts.

The recurring weakness is a relationship being checked, split into independent
fields, then reconstructed by the next caller. Prefer one owner-derived value,
a small state enum, or a consuming transition. Remove the old assembly route.
Wrapping every argument in a newtype without changing its public constructor
does not authenticate its origin. Conversely, immutable evidence may be shared;
do not remove `Clone` indiscriminately or brand every value with a new lifetime.

Static prevention covers which states/arguments an ordinary caller can supply.
Decoded input authenticity, exact issuing identity, source revocation, concurrent
publication, worker liveness and physical END remain dynamic owner checks. A
private record is not proof of those temporal facts. Compilation, independent
history properties and actual joined execution remain separate obligations.

| Change | Receiving owner | Required checkpoint |
| --- | --- | --- |
| Q1: reuse semantic world identity | Service cells, native review | Before issuing the new checked World output |
| Q2: one admitted Haskell operation | Service cells | During full cell preparation, before G1 joins |
| Q3: correlated items and demanded closure entries | Service cells, native selection | Before the corresponding output/closure consumer joins |
| Q4: native product stages and recorded/live roles | Native selection | N1–N3; before runtime consumes those products |
| Q5: source descriptions versus runtime admission | Runtime publication, source/custody | R1/S1 shared contract, before execution cutover |
| Q6: compiler observations versus live compiler access | Service cells | Before the first G1 successor reuse |
| Q7: publication state and complete terminal drainage | Lifecycle, runtime publication | Source/custody tranche; early Persona cleanup controls |
| Q8: preserve settlement and compound failures | Lifecycle, runtime publication | With renderer/host migration, before G2 |
| Q9: typed query answers, shared pins and keyed questions | Queries, service cells | Pin/answer correctness before G2; keyed work removal by G3 |
| Q10: production construction and attributable oracles | Tests, all leaf owners | Alongside each receiving boundary, not after the wave |

## Q1: delete the second semantic frontier identity

**Confirmed defect in new identity construction; no new live failure reproduced.**
`tidepool/toolchain/src/compiler_world.rs:2425–2444` hashes frontier link/unlink
and decoded/verified-byte counters into `world_restoration_frontier`.
`exact_context.rs:339–347` preserves identical roots on a no-op replacement but
resets those counters. Equivalent semantic state can therefore acquire a different
frontier identity. This was added in `b6239c158e2`; the new checked World consumer
is still dormant, so do not report an executed recovery regression.

Reuse `CompilerWorldRestoration::logical_id`. Its existing `TPCWREST3` product
(`compiler_world.rs:1685–1698`) already binds context, association and binding-map
roots, captured source seal, query scope, producer and package profile. Delete
`world_restoration_frontier`, the duplicate checked logical-identity field/getter
and its duplicate certificate frame. Keep work counters in diagnostics. Do not
replace them with a whole-graph census or another digest owner.

Equal semantic roots/scope/source/producer/profile must retain identity across
cold materialization, cached projection, no-op replacement and transport ordering.
Different selected native identities remain different through their actual roots;
source-root precedence is semantic even though FD/block transport order is not.
Operation certificates may still identify distinct operations. Version any changed
certificate meaning explicitly; an old digest must not be silently reinterpreted.

**Evidence:** independent equal-final-state histories with different work counts,
plus changed-root/source/profile controls. Assert identity and work separately.

## Q2: admit one Haskell operation with its validated plan

**Confirmed repeated parsing and representation hazard.**
`bridge/haskell/src/Tidepool/CompilerRequest.hs:81–84` retains raw `WorkerRequest`,
derived operation, independent input basis and optional cell inputs. Private
construction checks them, but consumers repeatedly inspect their combinations.
`Main.hs:949` obtains a validated `CellSourcePlan`; `GhcPipeline.hs:6133` validates
and parses the same recipe again. Rechecking held-world liveness is necessary;
reparsing an unchanged admitted recipe is different work.

Replace the product inside the existing bound request with one admitted-operation
sum. Common scope/grant/executor stay common. A World preparation case owns its
question, held correlation, validated immutable plan, matched slots and normalized
settings available at that admission stage. Typed-segment preparation, generated
operations and instance-retry settings are still produced at their actual GHC
stages; bind them there rather than demanding all stage outputs at request bind.
Other exact/build/query operations carry their own required
inputs. A failed acquisition returns a typed refusal, not a usable operation with
missing required fields. Raw requests remain transport data, not an alternative
post-admission source of truth.

Normalize the existing `CompilePurpose` at this boundary. Its recursive wrappers
and `closedWorldCellPurpose` currently make one nesting order a hidden contract.
Reject duplicate/conflicting settings once; retain any intentionally ordered
transformations explicitly. Do not flatten genuinely different operations into
flags or assume every wrapper order is equivalent. Delete recursive rediscovery
of fixed facts and the redundant unchanged-recipe parse. Generated-instance retry
may change its own compilation attempt, not the admitted authored recipe.

**Evidence:** one admission parse within a real preparation, mixed cell execution,
conflicting-purpose refusals and unheld-request refusal before expensive work.
Keep scope phase, executor, exact held realization, cancellation and END checks at
consumption. Those are not replaced by the stored plan.

## Q3: keep one correlated item and one demanded closure entry

**Confirmed duplicated retention and partial producer joins; no wrong item was
shown to pass admission.** `compiler_world.rs:1968–1979` retains raw CBOR items,
typed proofs and segments; `:2357–2394` clones proof fields into another checked
item table. `Main.hs:1068–1111` joins item/artifact/source lists with `zip3` and
ordinal lookups ending in `error`. Equal lengths do not establish association.

The producer should construct one complete item record from its actual typed
item, parsed source, captures/signatures, issued thin interface and artifact.
Validate exact coverage/uniqueness before exposing the terminal item manifest or
response. Private GHC/native staging may already have written files; do not buffer
all outputs into a new giant snapshot to postpone every write. The receiver authenticates
this into the existing checked-item owner and moves its fields, rather than
keeping equivalent proof and item collections. Item accessors borrow that one
collection. Retain original bytes/seals where needed for evidence and custody;
remove unused decoded trees and duplicate maps. Charge genuinely retained
allocations at their existing budget owner before allocation where possible.

Apply the same change to `ExactScope.hs:2264–2373` and
`GhcPipeline.hs:7088–7125`: a checked World binding row should carry its checked
value case, not a wider role enum followed by an unreachable `error`. The admitted
demanded closure should return entries coupling artifact, proof, interface and
package evidence, instead of parallel maps reconstructed with `Map.!`. Remove
unused retained views only after checking their custody and accounting roles.
Worker-bound decoded interfaces remain with the compatible worker, not in a
portable logical-world descriptor.

**Evidence:** genuine multi-item production plus reordered equal-length entries,
duplicate ordinal, missing capture, foreign interface, role mismatch and omitted
closure parts. Decoder negatives remain untrusted data. Exact reservation,
segment membership, binder/native identity, seal and END checks remain real.

## Q4: let native owners complete their own products

**Crossed-owner assembly is representable; the inspected production chain uses
the right inputs.** `declaration_context.rs:6964` accepts demand, groups, support,
binding, context and checked values independently. Its real caller at `:8918`
already obtains them from one output/demand chain. Delete `from_demand` and
construct the private result in that owner. Derive `ProductCompilerInputs` as its
view; delete the always-true `is_world_compilation` assertion/API.

Likewise, `CompletedRequestNativeProducts` (`:1112`) should retain its actual
completion inputs and issue through a consuming method. Delete independent
inventory/baseline/producer arguments to `issue_completed_native_availability`
(`:857`). Use the continuation's projection and actual request/output producer,
not a producer copied from the first result and compared back to itself. If the
only caller immediately issues, fold these private steps into its existing output
owner instead of exporting a builder.

`ReceiptNativeSelection` (`certified_products.rs:2619`) independently represents
origin, version, pending/bound identity and new/issued availability. Replace only
this private lifecycle with input versus current-product cases and explicit
pending, bound-to-output and issued-availability stages. The existing availability
issuer consumes physically bound current output and produces issued current
availability; it must not require its own output as an input. Execution consumers
require that issued state, and pending products cannot enter the availability
issuer. Completion must retain whether a
product originated in this request; acquiring it does not turn it into an old
input. Build a batch replacement before swapping, or consume an owned builder,
so a failure on product B cannot expose a half-updated reusable selection A.

Use existing `RecordedCompilerRole` as a genuine fact/wire DTO. Its current
`Deref` into the live-looking `CompilerInputRole` defeats the intended stage
distinction (`artifact_inventory/compiler_projection.rs:124–320`). Deserialize
records, authenticate them through the existing acquisition, and return complete
opaque live published roles. Delete recorded-to-live dereference and zero-digest
live-role construction (`declaration_context/published_source.rs:521–547`).
Serialization projects facts explicitly; no new certifier is needed. A source-only
API cleanup need not change the wire if its meaning remains identical.

**Evidence:** two real outputs with equal-looking facts but different origins,
one changed association per refusal, a two-product late failure, and real
recorded-facts acquisition/reopen. Preserve exact historical edges and competing
custody; never reconstruct selections from canonical equality or module names.
The two pending-original red histories need real completion, not another private
fixture binding call. Immutable completed facts may remain cloneable.

## Q5: keep source descriptions descriptive

**Dormant authority seam and misleading documentation, not a demonstrated live
execution bypass.** `WorldSourceSelection::new` checks shape; hashing its caller
labels into a checked certificate does not authenticate source ownership.
`WorldCellAdmissionSubject::for_source_transaction` (`cell_plan.rs:245`) checks
compiler-origin identity, then accepts caller-supplied owner/epoch/scope facts.
Generic compiler transactions do not thereby become runtime source issuers.

The actual runtime admission (`session/admission.rs:3734–3945`) already checks
real owner, epoch, scope, target lease, original-A transaction and world relation,
and creates opaque `RuntimeCellAdmission`. Preserve and complete that boundary.
The runtime preparation method derives A/B and reservation facts from captured
owners and retains them with checked output and matching settled transaction.
Install consumes that runtime-owned preparation. It must not accept bare compiler
DATA/token or an output plus independently supplied source labels as authority.
Delete the comment claiming compiler origin authenticates source facts. Do not
introduce a toolchain dependency on runtime or a public trusted-label constructor.

**Evidence:** valid production preparation/install, equal-byte foreign owners,
revoked A/B, racing bind/revoke, stale scope and lost waiter. A compile-fail client
can prove bare data cannot call install; runtime histories prove identity/liveness.
The existing positive setup that ends in `MissingPublishedCompilerWorld` is not
an executed success control. Parser-token tests prove allocation/slot checks only.

## Q6: expose compiler facts without exposing live compiler access

**Over-wide API, not demonstrated post-close mutation.**
`CompletedCompilerResult` (`GhcPipeline.hs:5816`) and `PreparedCompilation`
(`CompilerRequest.hs:215`) represent correlated World inputs and pending dependency
completion as independent `Maybe`s. `preparedCompilationResult` returns the raw
pipeline result without the guarded accessor; that result exposes `prHscEnv`.
A private constructor alone does not make an exported mutable environment safe
after its owning executor closes.

Use an ordinary-completion versus World-completion case, with World inputs and
its pending/consumed dependency state together. Remove the early `maybe Map.empty`
interpretation of required World data. Bind portable/native output intent at the
same compilation owner rather than supplying it again during artifact emission.

Move remaining effectful interface/product/signature work into concrete operations
of the existing guarded request/product owner. Return immutable observations and
owned emitted products. Narrow/remove the raw result/environment getter. A generic
callback allowed to return `HscEnv` would preserve the escape; it is not the fix.
Keep shared package caches and worker environments with their actual existing
owner, and retain its phase/executor/interpreter compatibility checks. Internal
live-stage callbacks and lazy GHC facts may remain where their owning stage needs
them; do not serialize `HscEnv` or relabel it an immutable observation.

**Evidence:** all formerly raw-result consumers refuse effectful work after scope
or executor close, including empty/cache-hit operations. A new live executor does
not revive the old completion. Immutable diagnostics remain readable. Test
one-shot completion, cancellation, output-intent mismatch and no writes on refusal.

## Q7: represent publication alternatives and terminal ownership explicitly

`ActorPublicationGuard` (`resident_workbench.rs:1938`) represents an exclusive
World-publication versus target-fence choice using separate `Option`s, then
`expect`s one in settlement. Replace those fields with one private enum carrying
the relevant fence and prerequisite. Consuming settlement takes that payload;
Drop retains the existing conservative unresolved outcome. Do not replace exact
operational attempt identity with semantic equality or remove the concurrent
claim check. This is a representation simplification, not a new state framework.

**Confirmed omitted cleanup axis:** `InstalledToolsState` has an installation
slot and a separate reply-activation slot. `clear` (`:2694`) clears only the first;
terminal `release_session_state` calls it. An idle external
`ResidentInteractivePolicy` retains the shared state, which can therefore retain
reply-owned custody after terminal cleanup. No stale tool execution was proved;
the ordinary call snapshot refuses an empty installation.

Give the existing owner one terminal drain that takes all state-owned installation
and reply custody under a consistent terminal transition. Distinguish terminal
Retired from fresh Empty: bootstrap may publish into Empty, but a later publisher
must not reinstall into Retired. Drop extracted owners outside guards; do
not run teardown callbacks while holding installation locks. Keep separately
issued in-flight request snapshots alive intentionally. A single private state
record may make this easier than independently drained slots; no global registry.

**Evidence:** real typed request, active reply, retained otherwise idle policy,
actor retirement and physical session release. Separately retain an issued request
snapshot and prove its custody lasts until release. Race terminal drain with reply
publication through the actual owner; assert no repopulation or collateral release.

## Q8: carry settlement and compound failures to their actual consumer

**Confirmed diagnostic loss, inherited before this join.** Runtime already emits
`RendererProductionFailure::{Compile, Publication, CloseUnconfirmed}` with primary
and secondary evidence (`session/resident.rs:1558`). The actor immediately applies
`action_error.unwrap_or_else` (`resident_workbench.rs:7963–7968`), discarding close
or publication failure whenever an action failed too. Physical owner state may
still retain uncertainty; do not claim an unsafe retry was demonstrated.

Carry this existing typed result through the actor error/receipt boundary, either
directly or in a lossless existing-domain variant. Render summaries at presentation.
Recovery consumes both the primary cause and independent cleanup/publication
obligations. A language error plus unconfirmed END is not ordinary unavailable
preview evidence. Avoid recursive string matching or another classifier registry.

Delete `PendingPreparedInstall::compile_off_checkout` and the old split route with
the finite-path cutover. It uses a fresh unrelated cancellation token, drops the
close sink and extracts only `.action` from the result (`session/resident.rs:2832–2849`);
the actor enters through a bare blocking task. Its current action is Cranelift-only,
so this is not a demonstrated uncertain GHC-END bypass. Any surviving native
acquisition uses the existing operation-owned cancellation and settled-draft path.
Do not repair the obsolete route into a second supported mechanism.

**Evidence:** action failure plus unconfirmed close, action failure plus publication
failure, cancellation before/during preparation and lost waiter. Assert retained
primary and secondary facts, publication refusal and the exact request's terminal
outcome through the real actor. Successful presentation alone is insufficient.

## Q9: answer one typed question about one pinned view

`decode_world_answer` validates the structured CBOR payload, retains raw bytes,
then runtime `run_world_inspection` decodes and validates it again. The validators
even disagree on `Absent + Unsupported`: the first admits it, the second refuses.
Resolve that combination at one typed wire-admission owner. Keep raw bytes for
evidence, not as a second internal control-flow representation. The result sum
must distinguish resolved structured answers/errors, proved module absence and
service rejection. Runtime still authenticates the world association, END and
current scope. The existing `tidepool-extract-report` owner is the first place to
consider for dependency-neutral wire types/decoding; toolchain already depends
on it. Move only the pure domain, not runtime capabilities or session ownership.
Do not introduce a toolchain-to-runtime dependency or another query framework.

Carry one runtime-issued semantic view pin separately from compiler-world receipt
identity and descriptive scope labels. Public-module answers currently use
`compiler-world:<logicalid>` while actor queries use a prepared-source fingerprint;
`same_view` compares those strings. This is a confirmed static mismatch; the
cross-verb user failure still needs execution. Confirm and repair the composition,
including passing an unchanged public `info` answer's pin into `rowFit`. Do not
make mismatch disappear by ignoring fingerprints or accepting either scheme.
Use the existing publication owner to issue the shared pin; allocation counters
and worker residency are not a semantic view change.

`rowFit` currently plans `ScopeBrowse`/`Browse`, GHC solver-checks signatures of
the browsed entities, and Rust selects one identifier afterward. The solver work
is real; claiming it is missing would be wrong. Add the keyed question to the same
compiler service, resolving the exact requested identity in that view and running
the same constraints only where needed. Preserve ambiguity, stale-generation
and namespace behavior. Resolve a public `NameQuery`, including aliases/re-exports,
to its canonical `IdentifierRef`, then check that identity. Do not retain the
current ambiguity between canonical identity and public re-export lookup.
No new index/cache or runtime grant is implied by a positive compiler fit result.

Keep failed observation distinct from proved absence. The current broad
`inspectModule` catches `SourceError` for the whole operation and returns
`InspectionModuleNotFound` (`Introspection.hs:924–928`). The held-world resolver
already distinguishes a clean finder `NotFound` from hidden, unusable or failed
modules (`GhcPipeline.hs:1818–1826`). Reuse that distinction in the keyed path;
do not copy the broad error collapse or classify rendered strings. This is a
source-confirmed collapse with user impact still requiring a concrete run.

**Evidence:** paired wire negatives and actual public-verb composition through
the new service; an unchanged shared pin succeeds and changed semantic view
refuses. For keyed `rowFit`, a large module with one requested entity should no
longer check every unrelated value. Count actual selected solver work and compare
semantic answers using the existing GHC solver functions on the same checked
scope. Cover re-exports, same-spelling ambiguity, a genuinely absent module and
a present-but-failing module. This oracle must not preserve
a callable legacy resident browse/finite route. Retire the old production
browse/filter route once parity is established.

## Q10: tests must inherit the repaired boundaries

Move positive behavioral histories out of crate-private authority fabrication and
through existing production constructors/integration targets. Keep raw malformed
codec inputs internal and explicitly untrusted. Delete authority factories,
merge/restore replicas and vacuous tag assertions as their consumers move. Do not
replace them with a generic fixture backend or another test framework.

`artifacts::test_support::with_settlement` accepts an empty close list through
`all(...)`; this is not submitted-and-settled evidence. The second-cell timing
test accepts any `compiled_off_checkout` row, allowing unrelated bootstrap work
to satisfy it. Use the existing operation/origin/world/terminal correlation and
daemon census, with actual executed results as a separate oracle. Known refusal
before submission requires zero submissions; a successful submitted compile
requires its own terminal evidence. Do not demand exactly one event globally
across legitimate retries or treat missing telemetry as zero work.

For each receiving change retain three kinds of evidence: external caller
compilation proving removed assembly routes cannot be used; independent runtime
histories proving freshness/ownership/atomicity; and a joined real consumer proving
the claimed path executes. Use compile-fail controls only for meaningful public
misuse, not tests mirroring every private field. Reuse a small genuine immutable
compiler cohort within each test process and fresh mutable state per history;
the counted runner's separate processes do not share an in-memory cache.

## Acceptance and escalation

Workers report the old surface deleted, real consumer migrated, impossible states
removed, remaining dynamic checks, exact compile/test evidence, and known limits.
Review the joined API, not just its helper tests. Update the compiler work map
when implementation changes flow; this source-only review does not describe the
future path as shipped or claim a measured speedup.

Changes here should reduce representations and reconstruction. If a proposal
adds a proof registry, parallel certifier, generic capability tower, unrestricted
escape getter or legacy fallback, return it for a smaller owner-based design.
If a required relationship cannot be carried without a dependency cycle, identify
the minimal pure fact/codec boundary and review it before moving modules.
Preserve existing trusted owners and genuine format/refusal distinctions.
