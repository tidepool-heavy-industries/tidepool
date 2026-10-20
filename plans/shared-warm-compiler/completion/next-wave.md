# Next wave: close the actual compiler-service path

Status: reviewed-work assignments at code `de12466597`, not executed repairs.
Start from this integration branch's latest published plan commit and record its
code base. Read [the checkpoint review](review-de12466597.md),
[locked contracts](contracts.md) and the corrected
[received boundary](contracts/CELL-PREPARATION-BOUNDARY.md).

The full campaign and G2/G3 acceptance remain in [plan.md](plan.md). This file
changes the immediate dispatch order and closes specific gaps in the folded work;
it does not establish a smaller supported Persona language or waive lifecycle
work. Existing area plans supply the detailed remaining requirements.

## Supervision and receiving ownership

Use the existing three area branches and one campaign coordinator. The engine
integrator owns the complete functional path across Haskell, toolchain, runtime
and actor files. It may use a Haskell leaf and a runtime/output leaf, but remains
responsible for receiving both and making the joined history run. A leaf that
needs a neighboring function sends the exact change to that owner immediately;
"outside my file limits" is not the final state of a functional assignment.

| Owner / leaf | Immediate assignment | Receiving checkpoint |
| --- | --- | --- |
| Lifecycle / publication | P: preserve unresolved settlement and terminal failures | Before engine's joined cleanup controls |
| Engine / native | N: current product, exact parent edges, A1c custody | Before checked World output is accepted |
| Engine integrator + Haskell leaf | H: owner-issued preparation and guarded stages | Alongside service seal; no raw overload remains |
| Engine integrator + runtime/output leaf | V: cold first cell, seal, successor, publication, cutover | The first complete service-only history |
| Verification / tests | T: valid source setup and actual operation oracles | Pair with each owner; run V's first history early |
| Verification / queries | Q: one answer admission, public pin and all-five use | Before early Persona |
| Lifecycle / source-custody | L: A/B, preview, physical cleanup and recovery | Shared contract before V; G2/G3 histories afterward |
| Verification / qualification | E: counted execution, exact bundle and timing | First joined V result, then G2 and G3 |

P, N, H, T and Q have independent ready work. L receives shared A/B edits with V
instead of concurrently rewriting admission constructors. E prepares commands
and evidence, not repeated bundles of an unexecutable route. Start no more than
twelve nodes including integrators; use the same context-loaded workers for
repair and a different reviewer for acceptance.

## P — preserve publication obligations through failed settlement

Own `ActorPublicationGuard`, its private authority enum, the lower publication
owner and the existing renderer slot/observer boundary. Receive shared-file edits
through lifecycle integration before V changes actor preparation.

1. Replace consuming `bool` settlement with a transition that either settles or
   retains the unresolved owner obligations. Preserve exact attempt comparison.
   Cover failure and unwind after authority is taken; do not invent a reusable
   fence when a lower operation has already consumed it.
2. Keep the lower World Drop backstop and restore the outer installed-state
   unavailable transition. TargetFence must also retain the real external
   prerequisite uncertainty. Successful settlement releases once.
3. Keep terminal Retired/drain behavior. Carry typed terminal renderer refusal
   through existing, parked and later slot observers, without a new producer.

Acceptance: both authority variants; confirmed/refused legs; actual external
prerequisite; owner loss at a controlled barrier; unwind; producer action plus
uncertain END; producer disappearance; normal success. Assert exact installed,
world and prerequisite states, not merely that another snapshot is unavailable.
Then exercise the production publisher/receipt consumer.

Do not broaden this into a new error or cleanup framework. The owner already
exists; the missing data must stay with its consuming transition and slot.

## N — repair native provenance at its issuing owner

Own native availability, product/source completion, graph/custody acquisition
and publication selection. Coordinate S1's touched declaration functions by
named functions and small receiving commits, not concurrent policy rewrites.

1. First add the paired real-owner control: physically bound current product
   with no self dependency proof succeeds; inherited private Full support with
   no current product cannot become a current root.
2. Derive current membership from actual output origin and bound product
   identity. Preserve current completion separately from already-issued own
   group reuse. Delete parallel completion collections that infer membership.
3. Historical traversal carries the exact parent `NativeGroupBinding`, its edge,
   child and carrier. A new parent consumes its actual request input selections.
   Two equal-payload parents may legitimately choose distinct exact children.
4. Confirm the first six-versus-four divergence; preserve a carried own binding
   instead of reminting the parent. Keep the independent exact-identity oracle.
5. Trace A1c before and after selected-owner filtering. Acquire/retain the exact
   demanded native closure without adopting temporary `effective` compiler
   roles. A canonical-only module does not magically acquire executable bytes.
6. Finish the existing narrow birth/current-projection obligations. Do not
   discard exact historical support or re-add all ambient implementation roots.

Acceptance: real producer → native admission → publication → cold recovery;
equal-payload different issuers; parent/child/carrier substitution refusals;
explicit empty choice; genuine incompatible root choices; A1c followed by another
request. Assert retained Core stays InterfaceOnly when its receipt says so,
and unrelated Full custody adds neither compiler roles nor executable roots.

Escalate if exact demanded bytes are absent at the original producer. Do not
reconstruct an old closure from latest module versions to make the downstream
fixture pass.

## H — complete the Haskell owner boundary without duplicating work

Own CompilerRequest, GhcPipeline and their Main stage consumers. Work directly
with V on actual emitted World output.

1. Move the opaque admitted preparation beside the compiler-scope owner.
   Validation issues the inseparable recipe/parsed plan/slots/held identity;
   `scopedCompileWorldCell` requires it. Remove the raw overload and duplicate
   request-layer admission record. Keep dynamic executor/hold checks.
2. Migrate live World, preview and extraction stage operations through the
   guarded preparation owner. Retain selected output intent; return immutable
   observations/artifact products. Remove unrestricted `preparedCompilationResult`
   after consumers migrate. Finite consumers disappear with V.
3. Keep Q3 keyed records. Extend full syntax using the existing parser's execution
   item/segment identity plus its grouped source ordinals. Do not equate a source
   item ordinal with execution item ordinal or invent another grouper.
4. Execute real prepare/emission and the Rust consumer. The existing parse
   counter test does not reach those stages.

Acceptance: corrupt real recipe/receipt/slot data refuses; valid bind, declaration
group followed by bind/expression, imports, mixed cells, source-defined retained
types, pattern binds and zero-capture items; foreign/closed executor and replaced
hold; missing/duplicate/permuted output records. Verify actual stage operations
refuse after owner close, not only an empty callback. Typecheck before effects
and exact output custody remain required.

## V — deliver one joined production behavior, then delete the old route

The engine integrator owns this assignment, including neighboring functions
required to complete it. There is no separate accepted R1 or S1 release.

### V1: make the first and second ordinary cells work

- Capture the real public base and original source A under machine checkout.
  Cold source capture/world acquisition belongs to the existing compiler-work
  owner outside checkout; consume its actual settlement and revalidate the
  same base before installing the derived world association. Warm uses the
  same owner with existing world evidence.
- Remove the uncalled synchronous cold helper as the implementation is joined.
  Do not require a prior lookup or a test-created inspection association.
- Plan/reserve through the source-bound preparation; original A and receiving
  target B stay distinct. Independently supplied descriptions do not authorize
  a runtime reservation or executable publication.
- Complete `CheckedWorldCellProgram` → existing checked-item/`CellProgram`
  issuance from the same captured physical output and demanded native evidence.
  Keep the existing `finish_cell_program_admitted` outer END/origin/cancellation
  owner. `CompilerTransactionSettled` and `HostAdmissionCancellation` are owned
  by extract-cmd; no runtime-to-toolchain dependency inversion is needed.
- Native preparation runs outside machine checkout under that operation owner.
  Installation freshly checks source/scope/binding/item/generation and keeps
  image, metadata and completion coupled. Do not restore the deleted split API.
- Adopt each successful item's own successor; final publication consumes the
  existing post-removal proof. A checked output or executed private item alone
  does not advance the public world.

The first discriminating history starts with no inspection world, calls the
actual actor `haskell` handler, publishes a binding, and uses it in the next cell.
Assert service operations, world/worker/output/transaction identities and the
observed value. A compiler-direct helper or ignored slice is not that history.

### V2: required language and sole-route cutover

Extend immediately to declarations, imports including empty import sets,
expressions with the service collision witness, mixed cells and source-defined
retained types. Keep original parser grouping and whole-cell checking. Remove
temporary missing-capability refusals only when their actual owner issues the
replacement value.

Migrate startup/host carriers, renderer work and recovery through their genuine
purposes on this service. Delete finite admission variants, getters, reconstruction
and ordinary callers together. Do not preserve an old path for bootstrap, tests
or recovery. Intermediate feature commits may be incomplete; no such intermediate
is a Persona candidate.

Acceptance: valid first/second histories plus late type failure before any effect;
crossed world/source/reservation/output/END; source revocation before installation;
blocked compiler with independent permitted machine progress; cancelled/lost
waiter; no replay of completed effects; successor reuse after predecessor
realization eviction and disposable output removal. Keep genuine immutable
source/native custody alive.

## T — repair fixtures by entering the real source and operation owners

Use the existing registered runners and adjacent fixture files; no new framework.

- Repair residency setup with the real source transaction and public session
  wrapper. Retain the independent live-bindings/collection bound. Run it beyond
  the former first failure, then port its compile half to V without widening
  private APIs.
- Replace `service_cell_slice`'s unconditional error helpers and ignore marker
  when V supplies the missing production output. Several advertised private
  admission API gaps are already obsolete.
- Connect the operation oracle to an actual joined history and remove deleted
  span assumptions. Distinguish GHC work, native compilation, waiting and install.
- Preserve B-03 residency, B-04 unchanged-unit native code reuse, and B-05 affine
  execution. Also preserve off-checkout responsiveness, A/B revalidation,
  lost-waiter cleanup and no replay. These are separate assertions.
- Distinguish preflight refusal from consumption: a crossed foreign request
  refused before admission must not burn the rightful owner's still-valid item.
  A started/consumed item is never silently replayed.
- Close the 32-case ledger by running past every first failure. A stale error
  string or fixture role does not establish that later recovery/closure checks
  pass. Delete only obsolete mechanisms, with named replacement coverage.

Emit an executed-count ledger with exact commands, OIDs and artifacts. Do not
copy the S1 handoff's `--exact declaration_context::tests::` as a valid selector:
discover actual full test names and require nonzero counts.

## Q and L — parallel work with explicit receiving points

Q finishes [queries](queries/plan.md): consolidate pure wire decode/envelope
correlation in the existing lower response owner, preserving the union of checks;
use precise answer variants; share a runtime semantic pin across public verbs;
implement keyed rowFit in the existing GHC service; distinguish genuine absence
from compiler failure. The meaningful all-five authored composition must use V,
inspect fields and invoke an allowed discovered operation. Local enum tests remain
local coverage.

L finishes [lifecycle](lifecycle/plan.md): source A/B and Open admission around
preview bind, SAME custody on known refusal, physical final-receiver cleanup,
late producer ownership, reload/retirement and cold recovery. Give V the existing
owner-derived interface early. Transport origin and serialized source facts are
not a substitute for live runtime authority. Preserve independent snapshots.

Q and L may fold independently reviewed fixes before V. They do not qualify V
by passing their component controls.

## Integration and qualification

1. Fold P and the reviewed N/H shared contracts; compile changed consumers,
   including test targets and actor/facade callers.
2. Fold V1 with its real two-cell control immediately; T observes this actual
   history. Continue V2 and the independent Q/L work without repeated full bundles.
3. Run the smallest meaningful refusal/concurrency/recovery histories on the
   joined revision. Remove stale supported entry points and update the engine map
   only after production callers move.
4. Apply the unchanged G2 gate, freeze the matched bundle, and offer fresh Persona.
   Record source/spec/memory/tool selection and the real DeepSeek smoke. Do not
   call the finite hosted history evidence for this candidate.
5. Complete G3, affected suites and matched-bundle M1–M3. Time the packaged
   ordinary workloads with cold acquisition, warm preparation, native compilation,
   wait and install separated. Claim only measured improvements.

Publish consolidated feature checkpoints and the current ledger. Do not advance
main on the strength of local library compilation. Reconcile actual remote main
and rerun joined acceptance before final main integration.
