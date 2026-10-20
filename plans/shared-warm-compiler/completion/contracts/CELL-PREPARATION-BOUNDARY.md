# Received boundary for the sole compiler-service cell path

Status: corrected implementation contract after review of `de12466597`.
This describes required behavior, not completed APIs or passing execution.
Read [the review](../review-de12466597.md), [next assignments](../next-wave.md)
and [locked decisions](../contracts.md).

One engine integrator owns the complete R1/S1/actor join. Haskell, toolchain,
runtime and lifecycle leaves supply reviewed owner changes; file ownership must
not strand the production consumer. This replaces the earlier draft's duplicated
signature inventory and contradictory D2/D3/D8/D11 instructions. Git retains
that draft. Existing detailed service/runtime plans remain applicable.

## Values and owners

| Stage | Existing owner | What crosses the boundary |
| --- | --- | --- |
| Capture request/base | Runtime session and live source admission | Exact public base, original A, receiving placement B, actual source authority and purpose |
| Acquire compiler world | Compiler work/source owner and compiler service | A derived compiler-world association for that captured base; cold and warm use the same route |
| Plan | Held compiler world and parser | Authored recipe, parsed execution items, grouped source ordinals and expression collision evidence |
| Reserve | Runtime admission owner | The same plan, purpose, source relationship, operation origin, slots and item generations |
| Prepare/emit | Guarded Haskell compiler operation | One correlated collection of real item outputs and demanded source/native dependencies |
| Admit physical output | Existing toolchain output/checked-item issuers | Same captured files, certificate, producer, plan/reservation, source descriptions and operation origin |
| Settle/seal | Existing compiler transaction owner and runtime finish | Opaque settled runtime bundle containing the same admission, program and cancellation observer |
| Prepare native/install | Existing operation owner, then runtime machine | Coupled image/metadata/item; fresh liveness checks at install |
| Advance private world | Item settlement and held-world successor owner | The successful item's own successor and actual runtime delta |
| Publish public state | Existing atomic publication owner | Final private result, post-removal proof, exact expected public base and retained obligations |

There is one evaluator and one native resolver. The compiler supplies derived
facts and products; Rust owns runtime grants, source liveness, resources and
publication. Descriptions and byte digests do not become authority by being
wrapped in a type whose caller chooses the fields.

## 1. Cold first use and warm reuse

`begin_private_execution` requires an existing world. Its actor caller acquires
that world without requiring prior introspection:

1. Capture the exact public/source base under the existing checkout owner.
2. Release checkout. Admit source-bound compiler work, using captured immutable
   sources for cold acquisition or the existing matching world for warm work.
3. Retain the operation result and actual close/cancellation outcome.
4. Reacquire checkout, revalidate the captured base/source and publish the derived
   association before using it for private admission.

Reuse the existing inspection acquisition pattern and compiler-work owner,
not an independent launcher or lifetime registry. A missing realization may be
reacquired from its owned logical state. An accepted operation with uncertain
completion must not be retried as a cold miss.

The engine area's caller now follows this acquisition sequence; actual hosted
execution remains a separate gate. Delete the synchronous cold helper and the
old finite bootstrap when receiving that caller. A component fixture may use
the same public acquisition sequence to establish its initial World, but must
not claim that this proves the actor's cold first-cell behavior. Do not expose
private constructors to manufacture the association.

### Association observations and operation custody

An installed association is a cache of compiler facts about runtime owners. It
must not strongly retain either its own target lease or original-source owner.
Otherwise the cache can keep an abandoned private scope or unpublished
reload candidate alive after its actual owner disappears.

Keep exact weak observations of both owners beside the existing immutable
base/key/epoch facts. A new admission upgrades those observations through the
canonical owner and checks the required gates. Pointer equality can compare two
observations of the same owner; it cannot manufacture missing ownership or
replace semantic compiler identity.

The admitted preparation then owns the actual strong pair, transferring it
through compiler completion, END and installation. Existing generation,
candidate and execution owners retain their actual leases independently.
Do not weaken the runtime lease's transitive prerequisite DAG or reconstruct a
source from an ambient scope map. A completed output waiting for installation
must remain usable even though installed associations themselves hold no lease.
The [source lifetime contract](SOURCE-AUTHORITY-LIFETIME.md) narrows the original
member to a canonical source token while the target remains a real lexical lease.

Fencing prevents new admission. It does not erase an already earned claim:
accepted work may settle while its external prerequisite is fenced, provided
that prerequisite is still the same live owner. Target publication remains tied
to its exact attempt and gate. Retired, replaced or uncertain owners are distinct
refusals. Retirement captures and waits for pre-fence accepted publications,
including dependencies they establish before final stop selection.

Acceptance must drop an actual successfully produced private birth before
delivery, abandon a real unpublished B, and hold a real completed operation
through the END/install gap. Observe subsequent scope/root retirement and
independent generation usability; reference counts alone do not establish these
contracts.

## 2. Source A, receiving target B and descriptive wire facts

Original A comes from the live issuing source transaction and remains retained
through preparation, installation and any lost-waiter cleanup. B is the actual
receiving private/lexical placement. Neither the caller's current transaction nor
the old target-only `source_lease()` accessor substitutes for A.

The owning runtime preparation captures and checks both. It admits the actual
original source and the receiving authority where consumed, including gate-only
revocation. Rechecking B alone is insufficient. Preview binding's Open admission
must enclose the transition, not merely precede it.

Compare the expected original and held World before earning source or target
claims or invoking admission hooks. Use the existing source owner's
`admit_original(expected, target)` operation; reading an admitted original and
then comparing it is too late. A crossed-owner refusal must leave both owners'
claim counts and hooks unchanged, as well as publishing no native output.

`WorldSourceSelection` and the toolchain's source/subject records can be
descriptive transport data. A `CompilerTransactionOrigin` comparison proves the
transport operation, not the actor/source/grant facts a caller placed beside it.
The runtime's opaque admitted preparation must therefore retain the source pair,
reservation and resulting checked output through final consumption.

Do not reverse crate dependencies or propose a toolchain `pub(crate)` constructor
that runtime must call. Do not move runtime grants into the compiler merely to
make a constructor private. Narrow mutation/issuance in the actual owner and let
lower layers consume descriptions; only the runtime owner authorizes execution.

## 3. Plan once, keep source and execution identities distinct

`WorldCellRecipe` describes authored source and protected templates. Includes,
current imports, retained binding types and compiler environment come from the
held world, not a reconstructed all-values baseline.

The compiler-scope validator issues an opaque preparation containing the actual
recipe, parsed plan, matched slots and held identity. `scopedCompileWorldCell`
must require that preparation. Put its issuer/type beside `CompilerScope` in
GhcPipeline; CompilerRequest already depends on that module. Remove the raw
compile overload and duplicate request-layer record, without adding another
parse or a dependency cycle.

Executor openness, current held identity and stage-specific producer facts remain
dynamic owner checks. A validated plan cannot promise future filesystem access
or an unchanged live executor.

The existing parser owns grouping: multiple declaration source items can form
one execution item. Correlation uses that execution item/segment identity and
retains its source ordinals. Never compare independently indexed lists or assume
one source item per executable item.

Expression observation names come from the compiler world's collision domain
and remain tied to that plan. Do not enumerate all historical names in Rust.
Declarations, imports, expressions, mixed cells and retained source-defined
types are required; blanket refusals are temporary WIP, not the language contract.
Host-carrier shape restrictions apply only to their actual purpose.

### Operation-owned private operands

A mounted host input is a private operand of this admitted operation, not a
public World binding. Its actual `PendingHostValueWrite` and captured interface
own the input identity. Bind that operand to the same plan/reservation and retain
it through END and native installation. Do not mutate the public compiler World
or add a free-standing extra-bindings API to make the private module visible.

Input references distinguish BaseWorld, actual PriorStage and CellOperand.
PriorStage names both producer and consuming stage, allowing the receiver to
verify that the producer precedes its consumer. References use actual captured
row/interface identities; none may fabricate a World identity. Runtime execution
still requires the matching opaque write, source and target admission. Descriptive
wire fields alone grant no access.

The runtime-owned operand retains the actual execution, checked input proof and
binding lease. Its opaque parser plan keeps that same operand across parser END;
the later preparation must export the operand's retained files again through its
own request. An FD borrowed by the parser is not a durable reference for the next
operation. Native admission consumes this closed pair and revalidates the actual
mounted write before reserving identities. A plain compiler plan cannot enter
the runtime admission route.

Physical custody and logical mount ownership remain distinct. Dropping the host
input owner schedules its ordinary retirement; a retained compiler plan can keep
bytes alive without authorizing another use of the retired mount. Both the held
owner positive case and the retired owner refusal need actual lifecycle controls.
Activation input has its own mounted/preview owner; it must derive from that
owner, not an arbitrary `Input`/`SessionInput` flag on the host-write constructor.

### Ordered full-language stages

The existing `HeldWorldCellAdmission` owns the operation phase, executor and
ordered stage cursor. Extend that owner with private immutable prefix facts
and closed stage kinds. Do not introduce separate public start/advance
capabilities, arbitrary compiler-environment callbacks or a second prefix
registry.

`CompilerRequest` folds the parser's actual inference segments in source order.
Each completion is paired with its exact stage and global reserved ordinals.
Only actual prior stage products can extend the next prefix; a final environment
cannot stand in for every earlier prefix. Generated-instance retries reuse the
same pre-stage facts and existing retry owner.

Compilation consumes the stage rendered by that admitted owner. Do not accept a
caller-supplied source path, purpose or text independently of the plan. The
existing generated-instance retry operation derives the permitted omission from
the same pre-stage facts; a failed attempt never advances the prefix.

The source owner captures and parses the module once, retaining byte identity,
effective flags and quote-free evidence. A typed executable wrapper additionally
proves its actual unique root. Declaration/prologue stages use their genuine
planned-original proof; they do not fabricate a typed executable plan.

The paired wire/receiver contract has closed executable, declaration and
prologue variants. Shared reservation and captured-file facts remain shared;
typed segment entry, native ordinal and executable program fields belong only
to their actual executable variant. A source receipt identifies its exact stage
and that stage's named dependency/package outputs, rather than assuming one
global file pair or a typed digest for every kind.

Reuse existing checked-item, original/native certificate and declaration
adoption owners. The whole cell finishes compilation and certification before
any authored effect executes. A later stage error discards the private prepared
prefix; it cannot expose a partially admitted program. Tests must exercise the
shipped template, genuine declarations/imports, later-stage failure and exact
earlier-prefix isolation.

### Compiler availability and executable demand

Every retained helper RHS needs valid compiler names and types, including a
helper not reachable from the selected executable root. Keep those compiler
references intact. Derive runtime obligations separately from the actual emitted
root and its certified native dependency closure, including transitive references
through earlier declaration stages.

Execution may select a subset of the whole cell's already-issued references.
Retain each selected reference's original origin, variable, scope, generation and
seal. A symbol/generation key is a correlation key, not authority; incompatible
references must still refuse. Do not reattribute an earlier captured reference
to a later consumer or infer native demand from the whole helper inventory.

An authored stage's Lib certificate proves that stage's exports and closure.
It does not prove a cumulative lexical facade. Use the existing authenticated
declaration-join issuer for cumulative publication and advance the runtime World
from the actual join and name delta. Never relabel the stage output or reconstruct
a finite compile view to fill the gap. Count and measure any remaining cumulative
join work as its owner's optimization obligation.

## 4. Same physical output through the existing executable issuer

The prepared World output retains:

- The same reservation, logical world/producer and operation origin.
- Original and target source descriptions tied to runtime admission.
- Exact common certificate, typed segments and physical output custody.
- One correlated record per planned execution item: generation, entry, symbol,
  native ordinal where applicable, binders, signatures and manifest.
- Demanded source/native evidence and the item's own successor description.

Use `WorldCellOutputAdmission`, `DecodedWorldSourceReceipt`,
`CertifiedSegmentOriginals`, `PreparedCheckedItemOutput`, `ExactCheckedItem`,
`ExactCompiledItem` and `CellProgram` through their actual owning constructors.
`issue_segment_originals` is a partial adapter at this checkpoint, not the
finished executable seal.

Decode and authenticate the captured output once in the existing issuer. Never
manufacture an `ExactCompilationRequest`, finite baseline or independently
assembled "trusted" output to satisfy old inputs. Adapt those inputs to the
already-established World facts instead. No second certifier or evaluator.

Runtime authority is not established merely by `CheckedWorldCellProgram` having
a nonzero admission digest. The runtime consumer requires the same retained
source-bound preparation and reservation. Corrupt descriptive fields must fail
correlation; a valid descriptive packet alone still cannot install code.

## 5. Native support is not compiler source selection

The native owner distinguishes:

- A module available in retained custody.
- Its compiler interface/Original role.
- Physical production by this exact request.
- Its already-issued implementation/group identity and exact outgoing edges.
- The current executable demand.

Current physical membership comes from the completed product owner, not absence
of a continuation or presence in issued availability. Already-issued historical
traversal keeps the parent's exact binding, child edge and carrier. New output
dependencies use the actual request's selected inputs.

Retain demanded native Core support without promoting InterfaceOnly/RetainedCore
to an Original compiler role. The temporary `effective` projection used while
qualifying availability must not replace semantic compiler selection. Preserve
an already-carried own group binding instead of reminting it.

Lexical/type/instance/scaffold dependencies must come from the real producer.
Empty placeholder fields and a whole cold `snapshot_roles()` census are not the
finished demanded-output contract. The service supplies obligations; the native
owner resolves exact bindings once.

## 6. END, cancellation, native preparation and installation

Keep `finish_cell_program_admitted` as the runtime outer settlement owner. It
matches the compiler transaction origin and close outcome, retains close evidence
on failure, and applies the owner-issued cancellation checkpoint before sealing.
`CompilerTransactionSettled` and `HostAdmissionCancellation` live in extract-cmd;
the existing dependency direction supports their use without importing runtime
into toolchain. Preserve the existing Clean/NotStarted policy with its actual
owner evidence; raw close reporting is not a settled authority constructor.

The existing `PreparedRuntimeCell` path must consume real checked World output
and produce an opaque runtime `SettledCellProgram`. Its private issuer runs only
after matched origin, accepted END, the original cancellation checkpoint and
native sealing. It bundles the actual runtime admission, program and same
cancellation observer. `begin_cell_program` consumes that bundle; it has no
overload accepting independently supplied admission and raw program values.
Toolchain products remain inspectable compiler evidence, not execution
authority. Cloning the bundle preserves the existing admission's one-shot gate.

Every item-consuming mutation must observe that retained cancellation state,
including native execution, declaration adoption and checked host mounting.
Cancellation after reservation must refuse before mutation. A refused foreign
consumer must leave that same bundle usable by its actual owner; a test starting
another unrelated good bundle does not establish this property.

Immutable native compilation belongs outside machine checkout under that same
operation's ownership. Reacquire and check current source, scope, hidden bindings,
generation and item before installing. Keep the prepared image, native metadata,
item and completion coupled. Do not restore `PendingPreparedInstall` or a bare
blocking task with its own cancellation protocol.

Preflight refusal is not consumption. A foreign/crossed attempt that fails
before admission must not burn another owner's valid item. Once an item starts
or its execution capability is consumed, it must not silently replay, including
after failure or waiter loss.

## 7. Successor, publication and uncertain cleanup

Only actual item success adopts that exact item's successor with its runtime
delta. Preparation success does not imply execution or public publication.
Final publication consumes the existing promotion/post-removal proof and checks
the expected public base atomically.

Derive visible binding changes from the binding owner's prepared post-promotion
projection. Deleting a local name can reveal an ancestor; a list of removed
names is not the resulting visible state. Build all fallible World changes
before durable commit, then install the binding projection and matching World
association in the same publication callback.

Hidden host bindings can affect descendant scopes and retained aliases.
Do not remove global invalidation merely because the immediate caller uses a
private scope. Any narrower transition must prove that a retained public World
cannot observe the exact changed binding, while preserving ordinary ancestor,
alias and orphan-cleanup invalidation. It must not bless unrelated stale keys
or enumerate every value to reconstruct an already-issued relationship.

A consuming settlement either settles or retains its unresolved obligations.
Failure and unwind must mark exact installed/world state unavailable and retain
external prerequisite uncertainty. Taking an enum out of a Drop guard must not
disable that fallback. Keep independent issued snapshots valid and distinct from
terminal actor-owned state.

Lost waiters do not end resource ownership. SAME custody return, final receiver
release, late output cleanup, worker replacement and recovery use existing
owners and retained observations. Absence of a callback is not clean release.

## 8. Acceptance and cutover

First run a real actor history from no inspection world: first cell, binding
publication, second cell using it. Observe actual worker/world/output/operation
identities and values. Then run all supported syntax and the refusal/cleanup
histories from [next-wave.md](../next-wave.md).

The accepted tranche deletes the finite route, its getters, tests' private
authority shortcuts and obsolete split entry points. An intermediate feature
commit may be incomplete, but finite fallback cannot qualify an early Persona
candidate. Startup, host carriers and recovery use this service with their real
purposes.

Port B-03/B-04/B-05 controls through real source admission and production issuers.
Preserve separate off-checkout progress, stale-install and lost-waiter controls.
Haskell request binding, local answer classification and synthetic operation
histories do not establish the joined path. Measure only after it actually runs.
