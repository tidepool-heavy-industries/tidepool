# A smaller Haskell engine built from GHC-prepared STG

Status: proposed implementation plan; engine implementation has not started.
Written 2026-09-07 against GHC 9.12.2. The original engine review used commit
971d3f55a4963f97077d772ef8bcca429ba64c06. The second pass also inspected
0348b3493ef9428305112e155d765e6bbafa3097; intervening committed changes were
planning documents, not changes to the reviewed engine.

The chosen direction is **GHC CorePrep followed by STG preparation, feeding
Tidepool's Rust/Cranelift engine**. GHC's runtime is outside this plan's scope.
Rust continues to own execution, memory, effect interpretation, runtime
authority, and session policy.

The requested outcome has four parts: fewer lines of maintained engine code,
clearer contracts, better performance, and a more coherent conceptual design.
A change that adds another permanent repair layer does not satisfy that goal.
Tests and essential checks may grow while the production engine shrinks.

The destination is an engine designed throughout around prepared STG. A working
STG importer and a passing semantic suite are intermediate milestones. The
completed work must carry useful GHC facts through Rust types, generated calls,
object layouts, and collection, and use them to remove machinery and cost.
It also includes evaluating the remaining relevant GHC production techniques
against Tidepool's workloads. Correctness coverage and use of the available
information are separate completion obligations.

## 1. Recommendation and intended boundary

Move the handoff from optimized, partly reconstructed Core to prepared STG.
Use GHC's implementations of preparation and closure analysis, then translate
their results into a small execution language with explicit runtime facts.
Keep CoreExpr a recursive tree. Erase GHC types, casts, and ticks before CBOR,
as required by the current language boundary.

The intended pipeline is:

~~~text
Haskell source and resident compile view
    |
GHC typechecking and Core optimization
    |
Typed Tidepool site elaboration and exact dependency resolution
    |  preserve module identity, valid bindings, and imported-value contracts
GHC CorePrep
    |
GHC Core-to-STG
    |
GHC STG preparation
    |  unarisation, selected optimizations, dependency/capture analysis,
    |  tag inference and its strictness-preserving rewrites
Tidepool execution schema
    |  recursive expression tree + explicit declarations/layouts/imports
Rust parsing and linking into invariant-bearing program types
    |
Cranelift functions, blocks, and managed objects
    |
Existing Rust effect/session owners
~~~

Prepared STG is **also after CorePrep**. These are successive stages, not
competing alternatives. GHC's native pipeline establishes that order in
[Driver.Main](https://github.com/ghc/ghc/blob/ghc-9.12.2-release/compiler/GHC/Driver/Main.hs).

The target is a small STG execution backend, not a serializer for every GHC
internal datatype. We do not need GHC's Cmm, native calling convention, pointer
tag bits, cost-centre runtime, or RTS object ABI. We do need the semantic facts
that those mechanisms currently consume.

Three distinctions must remain explicit:

1. A value's representation is different from whether evaluating it may demand
   work. An unlifted managed reference and a lazy lifted reference can both
   occupy a pointer-sized slot.
2. Semantic argument count is different from the number of physical registers
   or payload words. Zero-width arguments can still affect saturation.
3. A parsed program invariant is different from a dynamic heap invariant.
   Parsing can establish scope and representation compatibility; it cannot
   prove that a raw pointer will remain live across a future collection.

### 1.1 Use prepared information throughout the engine

For each fact the frontend provides, identify the production consumer and the
work it removes. Preserving an annotation in CBOR without consuming it does
not discharge this obligation.

| Prepared fact or structure | Required use in Tidepool | Observable consequence |
|---|---|---|
| Atomic arguments and explicit evaluation | Emit the prepared thunk/case structure directly | No argument-shape repairs or accidental eager evaluation |
| Function parameters, representations, and known entries | Direct saturated calls with the full signature | No unary closure chain, generic apply, or boxing solely to fit the old ABI |
| Unboxed fields and multiple-result operations | Raw payloads and SSA/ABI components through calls, cases, captures, and returns | No intermediate Lit objects or tuple allocation solely for transport |
| Nonescaping bindings | Local blocks with typed parameters | No heap closure for a join |
| Closure captures and dependency groups | Exact environments and explicit module linking | No per-subtree capture reconstruction or artificial global Rec |
| Update policy | Distinct function, updatable-thunk, and single-entry execution paths | Memoization machinery only where required |
| Evaluatedness established at an occurrence | Direct use of the value where its contract permits | No redundant entry/force check at that occurrence |
| Constructor identity, layout, and case information | Shared descriptors, dense dispatch, and compatible bridge construction | No repeated representation guessing or recursive unboxing repairs |
| Core worker/wrapper and specialization results that reach STG | Preserve the prepared workers and their signatures | Upstream unboxing and specialization survive Tidepool's execution boundary |

Some facts belong to a particular use or control-flow path. Do not turn them
into global properties of a VarId. Project the necessary evidence into the
owning parsed types and use it at that scope; distinguish known values from
unknown values with semantic variants rather than unrelated booleans. Import
facts must agree with the actual resident binding contract.

The support inventory in section 12 is a truthfulness requirement, not a way
to shrink the language until a small importer passes. The prepared forms
needed by the agreed production corpus must have working implementations.
Marking required joins, partial applications, or raw-value paths unsupported
does not meet this plan's goal. Excluded GHC runtime facilities remain an
explicit boundary.

Section 9.1 also requires decisions on optimizations whose value depends on
Tidepool's ABI, collector, compilation latency, or workloads. The final design
must account for those opportunities rather than leaving them as unspecified
future work. Enabling every GHC flag is not itself an optimization result.

### 1.2 Architectural choices made by this plan

The following are the implementation direction now. Measurements validate
them and tune bounded choices; they do not postpone deciding what engine to
build.

- Compile prepared functions through a representation-aware internal calling
  convention with native tail calls, one body per RHS, and ordinary C adapters
  only at Rust boundaries. Select the entry operation once from parsed facts.
- Keep raw values raw through the entire computation. Allocate Haskell boxes
  when the prepared program or a real language boundary requires a box.
- Put immutable kind, entry, arity, and layout information in shared
  descriptors. Use a compact common object header and kind-specific payloads.
- Use one generic partial-application representation and protocol. Generate
  adapters from authoritative signatures rather than interpreting arguments
  through unary heap-pointer calls.
- Use occurrence-level evaluatedness, local SingleEntry policies, joins, and
  self-tail loops in the first direct backend. These are required code paths.
- Retain inline nursery allocation; group safe adjacent allocations, track
  only live managed references, and elide barriers only for proven nursery
  initialization without an intervening collection.
- Share eligible immutable static values and function objects. Retained
  bindings become stable roots managed by ordinary collection; discharge the
  current graph-tenure mechanism's obligations before deleting it.
- Preserve GHC's worker/wrapper, specialization, and fusion results and
  restore the normal Core optimization profile after the comparison baseline.
  Tune late STG lambda lifting through GHC's existing controls.
- Keep heap references untagged initially. Compact descriptor/header state
  and compile-time evaluatedness provide the first gains. Dynamic pointer
  tagging, collector selector rewrites, and extra apply specializations have
  specific adoption criteria in section 9.1.

The detailed choices are in sections 6.5, 7, and 9.1. They deliberately
share signatures, descriptors, root owners, and call emission; each should
reduce the number of independent mechanisms an implementer must understand.

## 2. Evidence and confidence

This is a design recommendation grounded in source inspection and focused
regressions, not a completed performance study. No percentage speedup or
whole-program memory reduction has been established.

### 2.1 Reproduced semantic failures

The review added [engine_review.rs](../tidepool-codegen/tests/engine_review.rs)
and [EngineReview.hs](../tidepool-codegen/tests/fixtures/EngineReview.hs), with
registration in the existing codegen suite.

| Test | GHC result | Current Tidepool result |
|---|---|---|
| Ignore a division-by-zero argument | 42 | Reference evaluator raises division by zero |
| Ignore a recursively divergent argument | 42 | JIT runs until watchdog cancellation |
| Locally defined append returns its first list | Length 1 | JIT returns length 2 |
| Character-code sum of a string containing embedded NUL | 195 | Extractor fails decoding modified UTF-8 as ordinary UTF-8 |

The committed-style tests use GHC's interactive evaluator as their oracle.
During the second review a separate temporary driver was also compiled with
GHC 9.12.2, -O2, and -fforce-recomp. Its native executable returned
[42,42,1,195] for those four expressions. This confirms the answers against
native optimized execution as well.

Native CorePrep and final-STG dumps of the same fixture were inspected.
They show:

- keep as one function with two parameters;
- the ignored computations as separate updatable thunks;
- applications passing references to those thunks;
- a numeric list worker taking and returning Int# directly.

Those dumps demonstrate the useful GHC output shape. They do not demonstrate
that a Tidepool STG adapter or its new backend already works.

### 2.2 Source findings

| ID | Finding | Evidence and confidence | Design response |
|---|---|---|---|
| F1 | General App evaluates its argument expression eagerly | Reproduced; [eval](../tidepool-eval/src/eval.rs), [emission](../tidepool-codegen/src/emit/expr.rs) | Atom-only argument positions and explicit thunk bindings |
| F2 | Built-in behavior is substituted using occurrence names | Append failure reproduced; [Translate](../haskell/src/Tidepool/Translate.hs) | Exact symbol identities and ordinary function bodies |
| F3 | GHC literal encoding is mistaken for ordinary UTF-8 | NUL failure reproduced; same translator | Preserve literal bytes or use GHC's modified UTF-8 decoder |
| F4 | Failed allocation returns shared writable scratch and continues initialization | Source-confirmed; [alloc](../tidepool-codegen/src/alloc.rs), [errors](../tidepool-codegen/src/host_fns/errors.rs) | Explicit failure edges before stores |
| F5 | External byte/boxed-array payloads lack final reclamation ownership | Allocation, tracing, resize, and drop paths inspected; [array host functions](../tidepool-codegen/src/host_fns/primops.rs) | Machine-owned allocation lifecycle and collection accounting |
| F6 | Stack-walk integrity failure can return partial roots and collection proceeds | Source-confirmed; [walker](../tidepool-codegen/src/gc/frame_walker.rs), [collector](../tidepool-codegen/src/host_fns/gc.rs) | Collection requires a complete root snapshot |
| F7 | Unary heap-pointer calling convention loses arity and raw-value benefits | Source-confirmed; [application](../tidepool-codegen/src/emit/apply.rs), [joins](../tidepool-codegen/src/emit/join.rs) | Multiple parameters/results and representation-aware calls |
| F8 | Extractor converts GHC recursive joins into ordinary closures | Source-confirmed; [Translate](../haskell/src/Tidepool/Translate.hs) | Preserve StgLetNoEscape groups as control flow |
| F9 | Broad recursive groups create elaborate initialization dependencies | Source-confirmed; [Resolve](../haskell/src/Tidepool/Resolve.hs), LetRec emission | Preserve minimal groups and explicit global linking |
| F10 | Constructor-worker/wrapper representations are blurred and repaired later | Source-confirmed; translator and [normalizer](../tidepool-repr/src/normalize.rs) | Exact post-preparation representations |
| F11 | Per-subtree free-variable sets can require quadratic total storage/work | Algorithmic worst case; [analysis](../tidepool-repr/src/free_vars.rs); production magnitude unmeasured | Consume GHC's closure captures, avoid redundant whole-tree analyses |
| F12 | Memoizing a thunk before its result reaches WHNF permits evaluated-indirection cycles | Source and existing mutual-alias regression inspected; [forcing](../tidepool-codegen/src/host_fns/force.rs) | Update-frame semantics through WHNF |
| F13 | Initial compilation and add_function do not run identical preparation | Source-confirmed; [JIT machine](../tidepool-codegen/src/jit_machine.rs); no new Haskell failure demonstrated | One parsed/linked program entry contract |
| F14 | Heap data, code, roots, and external buffers have different lifetimes and incomplete aggregate accounting | Source-confirmed; existing [JIT lifetime plan](actor-model/jit-memory-lifetime.md) | Separate observable categories within existing owners |
| F15 | Retired old-space values are not reclaimed until machine drop | Source-confirmed; [OldSpace](../tidepool-codegen/src/old_space.rs) and the owning [scope retirement implementation](../tidepool-runtime/src/session/persistent.rs) establish the limitation; old-space comments referring to a major pass do not describe an implemented collector | Collector-owned promotion, full collection, and code-to-data reachability |

Additional concerns require targeted evidence before being called reproduced
bugs: re-demanding a failed retained thunk after per-run error state is cleared;
Template Haskell side-input cache invalidation; module/unit identity collisions;
and native-signal recovery across host frames. They appear in the acceptance
work below with their uncertainty intact.

The existing design has useful foundations: GHC optimization is already enabled,
Cranelift is configured for speed, GC roots have explicit owners, write barriers
exist, and executable allocations are released when their owning module dies.
This plan preserves those strengths and removes compensating mechanisms.

## 3. The exact STG handoff

Use the pinned GHC API, not parsing of pretty-printed Core or STG. Dumps are
evidence and debugging aids only.

### 3.1 Pass selection

The first adapter should use the native STG pipeline configuration, with a
small explicit record of the selected options:

| Stage | Initial choice | Reason |
|---|---|---|
| Core optimization | Preserve the existing optimized frontend initially | Establish an attributable baseline |
| Typed Tidepool elaboration | Before preparation and type erasure | Site answer/input types must still be available |
| CorePrep | Required | Make lazy/strict evaluation and application preparation explicit |
| Core-to-STG | Required | Obtain STG applications, RHS forms, and nonescaping bindings |
| STG unarisation | Required | Flatten unboxed tuple/sum binders and establish runtime argument components |
| STG CSE | Use the pinned native configuration; measure its effect | Reuse GHC's existing implementation |
| Late STG lambda lifting | Initially disable, then benchmark separately | Its cost model targets a different calling convention |
| STG bytecode preparation | Disabled | The destination is Cranelift, not GHC bytecode |
| Dependency sorting and capture annotation | Required | Obtain ordered groups and non-global capture sets |
| Tag inference and rewriting | Include, with an audited import contract | Rewriting also establishes strict-field/call-by-value invariants |
| GHC Cmm/RTS code generation | Not used | Tidepool owns the backend and execution |

The relevant exported APIs are corePrepPgm, coreToStg, and stg2stg, with the
matching GHC.Driver.Config initializers. The exact signatures are pinned
integration details, not a cross-language public API.

[CorePrep](https://github.com/ghc/ghc/blob/ghc-9.12.2-release/compiler/GHC/CoreToStg/Prep.hs)
arranges nontrivial lazy arguments through let bindings, strict arguments through
case, saturates primitive/constructor applications, and supplies implicit
constructor bindings. Its output already addresses several current translator
responsibilities.

The native STG configuration runs unarisation before optional CSE and lambda
lifting. stg2stg then performs dependency/capture analysis and tag rewriting.
Use that implementation rather than recreating a superficially similar pass
list. Any option overrides should be in one Haskell adapter.
[Pipeline configuration](https://github.com/ghc/ghc/blob/ghc-9.12.2-release/compiler/GHC/Driver/Config/Stg/Pipeline.hs),
[pipeline implementation](https://github.com/ghc/ghc/blob/ghc-9.12.2-release/compiler/GHC/Stg/Pipeline.hs).

### 3.2 Details that a naive STG importer would get wrong

**Zero-width values and arity.** After unarisation, function applications retain
void arguments for saturation, while constructor arguments omit them. Unboxed
tuple/sum expansion can change representation arity relative to the original
Id arity. Derive the call signature from the prepared binding/argument
representation; do not use type-erased argument length or idArity alone.
[Unarisation and its arity notes](https://github.com/ghc/ghc/blob/ghc-9.12.2-release/compiler/GHC/Stg/Unarise.hs).

**All four update forms.** GHC 9.12.2 has ReEntrant, Updatable, SingleEntry,
and JumpedTo. JumpedTo is a control-flow binding with no associated heap
closure. SingleEntry is an optimization justified by usage information, not
an interchangeable spelling of Updatable.
[STG update flags](https://github.com/ghc/ghc/blob/ghc-9.12.2-release/compiler/GHC/Stg/Syntax.hs).

**Tag inference has semantic work.** It can turn a constructor RHS into an
updatable thunk that first evaluates strict fields. It also inserts evaluation
for call-by-value arguments. Taking an earlier STG form and discarding these
obligations would recreate a strictness bug. We need the rewritten program;
retain its useful occurrence-level evaluatedness information as well. This
can justify omitting a subsequent entry without implementing pointer tag bits.
[Tag rewriting](https://github.com/ghc/ghc/blob/ghc-9.12.2-release/compiler/GHC/Stg/InferTags/Rewrite.hs).

**Imported facts are conditional.** Native GHC predicts facts about imported
closures and their evaluatedness. Retained Tidepool values must only receive
facts guaranteed by their actual entry/representation contract. A thin
interface or placeholder must not falsely advertise an already evaluated
value, known function entry, or single-use binding.
[Tag inference](https://github.com/ghc/ghc/blob/ghc-9.12.2-release/compiler/GHC/Stg/InferTags.hs).

**Top-level references are not local captures.** GHC capture annotations omit
global variables. A top-level closure can have no local captures and still
depend on many globals. Those dependencies must be linked and kept alive.
They cannot simply disappear when Rust stops computing free variables.
[STG free-variable analysis](https://github.com/ghc/ghc/blob/ghc-9.12.2-release/compiler/GHC/Stg/FVs.hs).

**Module identity matters to dependency ordering.** The same analysis classifies
references using the supplied module and each Name's defining module.
Feeding all currently resolved bindings to it under the target module's name
can classify definitions present in the pool as imports, omitting them from
local dependency edges. A flattened pool is not a valid replacement for
module-aware preparation and linking.

**Prepared STG is not a serialized GHC heap.** DataCon, PrimRep, Type, IdInfo,
and source metadata must be projected to Tidepool's finite execution schema.
The projection must not assume every retained GHC field is meaningful at this
stage: upstream rewriting can deliberately leave unused fields unevaluated.
Compute necessary runtime facts through the supported stage-specific APIs.

## 4. Restructure the Haskell frontend around a valid prepared program

Owning sources: [GhcPipeline](../haskell/src/Tidepool/GhcPipeline.hs),
[Resolve](../haskell/src/Tidepool/Resolve.hs),
[FatIface](../haskell/src/Tidepool/FatIface.hs),
[Translate](../haskell/src/Tidepool/Translate.hs), and
[Artifacts](../haskell/src/Tidepool/Artifacts.hs).

### 4.1 Preserve module context through preparation

Retain each source module's identity, location, type constructors, optimized
bindings, and required interface context until its preparation finishes.
The current flat prBinds view can be a temporary compatibility projection
during implementation, but should not remain the owning model.

The preferred design prepares recovered bindings in their defining module
context, then combines prepared artifacts through an explicit dependency
linking step. Preserve recursive groups inside each module and account for
cross-module edges, including hs-boot cycles.

An alternative that deliberately localizes the complete closed program into a
synthetic module would require a correct GHC substitution of every owned
definition and reference, including metadata and recursive edges. Do not
improvise that by renaming strings. Prefer preserving modules unless the
integration spike demonstrates a materially simpler correct localization.

Resident deferred modules and source-less value interfaces remain under the
existing GhcPipeline and session compile-view owners. Changing the backend
must not introduce a second source sequencing or cache policy.

### 4.2 Elaborate typed sites before erasure

The current translator discovers typed sites and swaps calls to generated
sited siblings while it emits flat nodes. Split this into:

1. A typed Core elaboration that resolves the exact sibling Id, inserts the
   explicit site argument, and produces the sidecar site record.
2. An STG projection that no longer needs to rediscover type applications or
   synthesize effect payload behavior.

Both the original and elaborated bindings must pass appropriate Core linting.
Ensure inserted siblings participate in dependency resolution. Keep the
generated helper's real Haskell implementation as the implementation of the
payload construction.

Preserve site identity through cloning, inlining, and sharing. Define the
relationship between an authored site and any duplicated or eliminated
occurrences; do not key persistent provenance on GHC's transient Unique.
Test all sited verbs and their input/answer types, not only Ask.

Metadata collection should read the prepared program and the elaboration
receipt. Delete paths that run translation again with incomplete state merely
to discover constructors and emit throwaway poison nodes.

Retain compact source/site provenance for diagnostics before ticks are
erased. Do not retain the GHC tick or Type object itself. Include partially
applied and polymorphic helper cases in the elaboration tests so a change
in application shape cannot silently lose a typed site.

### 4.3 Resolve bodies by identity

Prefer exact bodies from available unfoldings or fat interfaces. Remove
specialization-name reconstruction as a semantic fallback. A generic parent
with different dictionaries or arity is not an implementation of a specialized
binding until a type-correct wrapper has been established.

Make lookup results a sum type that distinguishes found bodies, missing
implementation, unsupported external capability, and interface-loading
failure. Broad exception-to-empty-map behavior loses information needed to
make an honest decision.

CorePrep can introduce implicit bindings and new references. Resolve those
through a dependency worklist with module ownership and deduplication, then
establish a closed supported program before final serialization. Do not use
an arbitrary whole-program retry count.

Maintain a narrow, explicit intrinsic/FFI support table keyed by authoritative
identity and signature. Ordinary append, constructor wrappers, and string
unpacking should use their real bodies where supported. Any remaining
intrinsic must justify why it belongs at the boundary and identify the code
it replaces.

An unsupported method that was historically tolerated because it remained
undemanded needs an explicit policy. First try exact resolution and dead-code
elimination. Where preserving a lazy unsupported operation is necessary,
represent a named deferred runtime failure with defined demand semantics.
Never substitute a guessed implementation or a generic successful value.

### 4.4 Normalize identity once

Use module unit, module name, occurrence namespace/name, and required record
parent disambiguation for external symbol identity. Distinguish these from
local binder ordinals, dense runtime constructor tags, and display names.

Preserve the existing collision checks and strengthen their inputs. Keep
aliases only where they denote an explicitly supported identity equivalence.
Perform deterministic local numbering after passes that introduce binders.
Check cold versus warm compilation and resident versus direct compilation.

Source-defined constructors and session values need stable identities across
fragments; temporary compiler locals do not need globally meaningful names.
Keeping those roles separate should shrink identity-repair logic.

### 4.5 Review optimization flags after semantics is correct

The current frontend uses -O2 but disables full laziness, with eager evaluation
given as a reason in its comments. Keep that profile only for the attributable
M0/M1 comparison. The production candidate restores full laziness and retains
GHC's ordinary optimized demand, worker/wrapper, specialization, and rewrite
pipeline. Verify both execution and resident retention before cutover; record
a specific measured exception if a normal optimization is harmful here.

The pinned GHC pipeline schedules CPR with worker/wrapper under the relevant
optimization path rather than consulting the current Opt_CprAnal override
as an independent switch. Remove that ineffective override and its misleading
configuration surface. Check the resulting worker signatures, not the flag's
name, to establish that unboxing is being preserved.
[GHC Core pass selection](https://github.com/ghc/ghc/blob/ghc-9.12.2-release/compiler/GHC/Core/Opt/Pipeline.hs).

Late lambda lifting is also an experiment after the calling convention is
defined. It trades captures for arguments; its profitability depends on the
new backend's measured costs.

## 5. A Rust execution model that is parsed into valid states

Owning sources: tidepool-repr for the serialized execution language and its
construction boundary; tidepool-codegen for linking, machine layouts, and
Cranelift emission. Extend existing owners instead of introducing parallel
registries or another public compiler front door.

### 5.1 Keep one recursive execution language

Change the current CoreExpr node vocabulary through an explicit format
migration. Preserve its recursive-tree structure and stack-safe traversal.
Do not add a permanent STG-shaped language that is immediately expanded back
into today's arbitrary-expression App and unary Lam forms.

The conceptual vocabulary should be approximately:

| Domain | Forms and invariants |
|---|---|
| Atom | Local reference, global reference, scalar literal, or semantic void argument; never an arbitrary expression |
| Heap RHS | Function with a nonempty semantic parameter list; thunk with an update policy; saturated constructor |
| Binding group | Nonrecursive binding or nonempty recursive group |
| Control-flow binding | Join group with parameters and expression bodies; separate from heap RHS |
| Expression | Return values, enter a lifted value, call, primitive/foreign call, constructor construction, case, let group, join group, jump |
| Call target after linking | Known compatible entry or dynamic callable value |
| Runtime representation | Lifted managed reference, unlifted managed reference, raw address, fixed-width scalar/float, or zero-width component |

Use Rust enums for these alternatives. Do not encode Function/Thunk/Join
using a shared struct whose meaning depends on combinations of empty vectors
and unrelated booleans.

In particular, a join cannot be inserted into a heap field because it is not
a value reference. A constructor field cannot contain an expression because
it accepts Atom. A raw scalar cannot be followed as a managed pointer because
its representation is different.

### 5.2 Parse, resolve, and retain the evidence

The boundary should have the shape:

~~~text
bytes + metadata + site sidecar
    -> parse_program(...)
    -> PreparedProgram

PreparedProgram + owning machine's imports
    -> link_program(...)
    -> linked executable input / compiled entry
~~~

These names are conceptual, not a requirement for a large typestate framework.
The important property is that the emitter consumes the successful result,
not the original unchecked tree after a separate validate call.

A possible Rust outline is:

~~~rust
enum ValueRef {
    Local(ValueId),
    Global(GlobalId),
}

enum Atom {
    Reference(ValueRef),
    Literal(ScalarLiteral),
    Void,
}

enum HeapRhs<A> {
    Function(FunctionRhs<A>),
    Thunk(ThunkRhs<A>),
    Constructor(ConstructorRhs),
}

enum BindingGroup<T> {
    NonRec(T),
    Rec(NonEmpty<T>),
}

enum UpdatePolicy {
    Memoize,
    SingleEntry,
}

enum ExecutionOutcome<R> {
    Returned(R),
    TailCall(TailPacket),
    Failed(RunFailure),
}

pub struct PreparedProgram {
    // Private, internally consistent tree, declarations, and layouts.
}
~~~

FunctionRhs and ThunkRhs have different private fields and construction
requirements. JoinId is deliberately absent from ValueRef. NonEmpty is a
small checked collection abstraction if needed, not a new public framework.
ExecutionOutcome is a semantic outline; it does not specify the native ABI.

Use private constructors, typed IDs, checked indexing, checked layout
arithmetic, and Result with structured error variants. Parse recursively
scoped declarations and their uses into a program whose binder classes,
representations, and references are resolved. Validate encoded summaries
while constructing the authoritative descriptors; do not retain contradictory
copies of the same arity or layout.

Use dense typed indices for local bindings, layouts, and signatures after
identity resolution. Freeze variable-length program metadata into owned
slices once built; reuse temporary maps and emission buffers within their
existing compilation owner. Do not add a hash lookup to every value access
or inflate every recursive node with a large inline buffer in the name of
avoiding allocations. Store execution facts once at their natural owner;
only occurrence-specific facts belong at a use.

The parsing/linking boundary establishes:

- the existing child-index/root invariants and resource bounds;
- lexical scope, uniqueness, and complete imports;
- exact constructor field shapes and case alternatives;
- supported primitive signatures and result component counts;
- closure parameter/capture layouts and update-form compatibility;
- join visibility, same-function scope, tail use, and parameter signatures;
- semantic arity including void arguments;
- agreement between imported signatures and the machine's actual bindings.

Cheap shape errors should return a typed diagnostic before code generation.
It is acceptable for schema checking to perform internal validation work:
the improvement is that success constructs a stronger type and callers cannot
accidentally bypass it.

Both initial compilation and incremental add_function must consume this same
construction path. Hand-built tests should construct the valid language;
malformed-input tests should exercise the parser's rejection path. Delete
backend repair code retained solely to make malformed synthetic IR executable.

There are three relevant arities: source-level function arity, prepared STG
application/saturation arity, and physical ABI slot count. In this document,
"semantic arity" at the execution boundary means the second. Unarisation can
expand the first into the second; dropping void storage reduces the second
to the third. Keep these conversions in one signature constructor.

### 5.3 Types at Rust boundaries, explicit layout at JIT boundaries

Rust enums and ownership types describe the implementation's semantic states.
The JIT-facing ABI uses deliberate repr(C)/repr(integer) layouts, checked
offsets, and compile-time layout assertions. Do not expose a Rust enum's
unspecified memory layout to generated code.

A NonNull pointer proves non-nullness, not ownership or liveness. Host-side
managed references should be obtained through existing rooted handles and
short-lived borrows. Prefer a no-GC access scope for direct memory access
where the borrow can prevent a collecting operation while the reference is
live. Re-load a rooted slot after a safepoint.

JIT references continue to use Cranelift stack maps. Only managed references
are roots; scalars, code pointers, and foreign addresses need their own
explicit treatment.

Scope temporary roots and allocation ownership with guards/RAII on normal
return and error paths. Do not assume RAII survives siglongjmp. Keep raw
pointer escape hatches narrow, private where possible, and attached to an
owner's actual entry points.

For retained entries and handles, use the existing machine/generation identity
to reject stale or foreign values. Introduce a new owner type only if it
replaces an existing convention, rather than duplicating its state.

### 5.4 State the small operational contract

Write down the transition rules for atom lookup, entering a thunk, applying a
function/PAP, selecting a case, allocating a recursive group, and jumping to
a join. The reference evaluator should follow those rules visibly.

The key obligations are:

- Looking up an atom in an argument or lazy field position does not enter it.
- Entering an updateable thunk retains an update obligation through WHNF.
- A case demands the scrutinee and binds exactly its represented components.
- Constructor allocation stores values according to the declared layout.
- A jump changes control within the owning function and allocates no closure.
- A safepoint preserves every live managed reference, including pending
  arguments, update frames, globals, and parked continuations.
- Failure cannot publish an ordinary result or continue using invalid storage.

For supported programs, assess observational agreement with GHC through
chosen WHNF/NF contexts and result values. For effect programs, also assess
the Rust-handler-visible request sequence and existing settlement semantics.
Do not require identical GHC error wording, timing, or behavior for operations
whose inputs are outside their defined primitive contract.

This operational contract is small enough to maintain with its source.
It is not a proposal for another general-purpose compiler framework.

## 6. Compile the execution language directly

### 6.1 Atoms, evaluation, and functions

Atom positions never start evaluating arbitrary expressions. A lifted
computation appears there through a reference to an explicitly bound thunk.
Entering a lifted value demands WHNF; returning a raw value does not inspect
a heap header. Case evaluates its scrutinee according to its result shape.

A function RHS has one parameter list and one compiled body. For a known
saturated call, emit one compatible call to its entry. Preserve raw arguments
and results through that entry. A captured local function can still require
a self/environment pointer; knowing its code entry does not erase captures.

For unknown calls, implement one eval/apply mechanism:

1. Enter the callee as needed.
2. Distinguish a function from an existing partial application.
3. Compare semantic supplied arguments with the entry's representation arity.
4. Call once when saturated.
5. Allocate a partial-application object when undersaturated.
6. Apply leftover arguments to the result when oversaturated.

The dynamic slow path can use a typed argument area and a generated generic
entry stub that invokes the same function body. It must track which physical
slots contain managed references. Do not call an arbitrary native signature
through an unchecked Rust function-pointer cast, or turn the slow path back
into one closure allocation per argument.

Separate optional tracing from required execution checks. Known-entry type
and arity facts can remove dynamic shape checks; they do not eliminate stack
limits, cancellation, or a real resource budget. Put necessary entry/stack
checks in one cheap generated protocol with cold exits, rather than retaining
always-called debug hooks on every application.

Consume GHC's evaluatedness evidence as well as its strictness rewrites. For
a lifted reference known to point directly to a value at this occurrence,
emit a direct return or case dispatch without the general force path. Preserve
strict-field and imported-entry contracts that justify this. Returning a
function value still differs from applying it to arguments.

Do not map every non-unknown TagSig to a universal AlreadyEvaluated flag.
GHC's domain includes result-component information and an analysis bottom;
function result facts and value-occurrence facts have different uses. Specify
the projection against the pinned APIs, and add an optional assertion mode
that checks the projected promises at their consumers under stress GC.
The parser checks the structure, scope, and import contracts carrying these
facts; it does not re-prove GHC's demand/tag analysis. Keep that compiler
trust boundary explicit and test false or stale imported promises rather
than describing an unchecked serialized hint as a Rust proof.
[Tag signatures](https://github.com/ghc/ghc/blob/ghc-9.12.2-release/compiler/GHC/Stg/InferTags/TagSig.hs)
and [GHC call selection](https://github.com/ghc/ghc/blob/ghc-9.12.2-release/compiler/GHC/StgToCmm/Closure.hs)
provide the reference contracts. This is required use of prepared information;
the physical pointer-tag experiment is a separate decision in section 9.1.

This adopts the relevant distinction in GHC's
[eval/apply machinery](https://github.com/ghc/ghc/blob/ghc-9.12.2-release/rts/Apply.cmm)
without adopting its machine ABI.

Represent multiple results as multiple SSA values where supported, with one
defined ABI fallback when necessary. Unboxed tuple returns are not heap
constructors. Use one primitive invocation for a multiple-result operation;
retire projection-specific primitive families where the new signature makes
them redundant.

### 6.2 Joins and tail calls

Translate a StgLetNoEscape group to one group of Cranelift blocks. Predeclare
all blocks and their parameter layouts before emitting recursive bodies.
Translate references in that scope to Jump, not heap-variable lookup.

Join parameters and case merges preserve their component representations.
Do not ensure_heap_ptr at every merge. A join's lexical environment consists
of SSA values available in the containing function, not a new capture object.

Keep cancellation on recursive backedges. Prefer an inexpensive flag check
with a cold failure path; eliminate the always-called host hook when the
generated path can enforce the same contract.
Install the active run's cancellation-flag address through the existing
registry guard and keep its owner alive for the activation. Read it with the
required atomic semantics; an ordinary immutable load that optimization can
hoist out of a loop is not a cancellation check. The cold branch records the
first cause and follows the same update/unwind protocol as other failures.

Ordinary function tail calls remain distinct from local joins. Use Cranelift's
supported tail-call facilities only after verifying the required conventions
on x86_64 and aarch64. Keep one explicit trampoline fallback where necessary.
Its pending arguments must have precise rooting and representation metadata.
Do not promise stack safety based merely on a syntactic tail position.

Recognize known saturated self-tail calls within the same function/environment
and compile them as branches to a loop header with updated parameters. This
also applies to ordinary recursive functions that arrive as heap bindings.
Keep this in the call emitter with the same backedge/root contract as local
joins; it does not require a second whole-program lowering pass. GHC performs
the corresponding loopification during backend call selection, after STG
preparation, so taking prepared STG alone does not supply the final loop.
[GHC self-tail-call selection](https://github.com/ghc/ghc/blob/ghc-9.12.2-release/compiler/GHC/StgToCmm/Closure.hs).

### 6.3 Thunk updates and errors

Adopt an explicit update discipline:

~~~text
Suspended(code, captures)
    -> Evaluating
    -> Evaluated(WHNF reference)
       or Failed(persistent error reference)
~~~

An update remains pending until the demanded computation reaches WHNF.
Do not memoize a pointer to another unevaluated thunk and discard the active
update obligation before entering it. Self and mutual aliases should be
detected through the active evaluation state, rather than through the current
64-million-indirection follow limit.

An explicit update-frame stack can preserve stack safety while rooting each
pending target. It must integrate with the existing run/root owner and avoid
holding a mutable Rust borrow across a callback into JIT code. GHC uses update
frames for this purpose; Tidepool needs the invariant, not a copy of the RTS
frame layout.
[GHC update frames](https://github.com/ghc/ghc/blob/ghc-9.12.2-release/rts/Updates.cmm).

Keep source-level deferred errors as ordinary lazy computations that fail
when entered. A Failed heap state must preserve its cause after a run's
pending error slot has been consumed. A generic poison closure containing no
cause is not a sufficient persistent exception representation.

Treat cancellation separately from a Haskell computation's failure. Decide
and test whether an interrupted update can be restored safely or whether
the affected machine must become unavailable. Do not silently leave shared
blackholes or turn cancellation into a permanent, unrelated Haskell error.
The first implementation may conservatively retire a machine when it cannot
establish a reusable state; it must report the effect/session consequences.

Use SingleEntry only where its assumptions survive the resident API. Entry
points, exported values, and bindings callable in later turns must be
advertised to the compiler accordingly. Conservatively use updateable
storage when a usage guarantee cannot be retained. Local verified
SingleEntry thunks must use a path without ordinary memoization overhead by
M4 completion. SingleEntry alone is not a nonescape guarantee and does not
justify stack allocation. A conservative resident import contract must not
erase valid local usage information throughout the program.

### 6.4 Reference semantics

Adapt tidepool-eval to the same parsed execution language with an explicit
evaluation/update stack. Keep its implementation simple enough to inspect.
It may use Rust-owned values rather than the production heap representation.

The reference evaluator is a debugging and differential aid. GHC remains the
independent language oracle. Sharing the schema, primitive signatures, and
identity definitions is useful; copying the JIT's execution algorithm into
the oracle would reduce its ability to expose shared mistakes.

### 6.5 Select calls once and give fast paths a concrete ABI

**One call decision.** During emission, combine the parsed target class,
argument signature, occurrence facts, and tail context into one enum-like
decision: return an available value, enter a thunk, call a known entry,
jump to a join/loop, or use dynamic apply. The corresponding branch owns
argument preparation, safepoints, the call, and its outcome. Do not repeat
the callee classification in the expression emitter, host force function,
application helper, and trampoline.

For example, the selected internal signature for a worker taking a list
reference and an Int# accumulator is conceptually:

~~~text
worker(vmctx, [environment], list_ref, accumulator_i64)
    -> (status, result_i64)
~~~

The environment parameter is present only when the entry requires one. A
known code address does not remove a needed captured environment. A static
capture-free function has no per-call environment allocation. Locals and
the accumulator remain SSA values; passing through a Haskell wrapper happens
only where the actual prepared program calls that wrapper.

**Internal convention.** Use the pinned Cranelift Tail convention for generated
entries and its return_call operations for compatible interfunction tail
calls. This chooses the convention up front; M3 includes a small ABI proof
covering both supported architectures before the main emitter depends on it.
Keep platform C conventions on Rust host functions and generate the adapters.
Preserve the frame-chain/root contract and verify it with tail calls that
change stack-argument size. The pinned
[Cranelift calling conventions](https://github.com/bytecodealliance/wasmtime/blob/v42.0.1/cranelift/codegen/src/isa/call_conv.rs)
distinguish Tail from Fast; a fast conventional call is not evidence of
bounded tail-call stack usage.

**Results and failure.** Adopt a status discriminant plus the typed scalar or
reference result registers that fit the verified ABI profile. For larger
result vectors, pass an ordinary pointer to a caller-owned result area and
return status. This area is stack/run storage, not a Haskell tuple object.
Forward the caller's area through a compatible tail call; never pass storage
from a frame the tail call removes. Compute the result as rooted SSA values,
then write all output slots and return in a no-GC epilogue. The caller checks
status and loads the successful components into typed SSA values before its
next safepoint. Thus temporary result storage needs no separate root registry.
If a boundary must keep a populated result area across a safepoint, register
its initialized reference slots through the existing scoped root owner.
Never inspect unwritten result slots on failure.

Do not depend on Cranelift's StructReturn parameter for this fallback: its
pinned AArch64 Tail implementation rejects that combination. An ordinary
pointer argument avoids that special ABI contract. Test mixed scalar/float/
reference results and enough components to exceed the register budget.
[Pinned AArch64 ABI implementation](https://github.com/bytecodealliance/wasmtime/blob/v42.0.1/cranelift/codegen/src/isa/aarch64/abi.rs).

The signature constructor selects the result transport once. Use the same
selection for the definition, direct calls, dynamic adapters, and Rust entry
adapters. Expected errors propagate through status and the existing
machine-owned first-cause record. A TailPacket is needed only where a verified
native tail call is unavailable, not as the normal path for every call.

**Dynamic calls.** Give every first-class callable a generic adapter generated
from that same signature. A caller writes atom components to a precisely
described argument area and invokes a uniform entry; the adapter loads those
components and calls the typed body. Stack/run scratch is sufficient for the
transient argument area. Its live references need roots across forcing, PAP
allocation, or a host call, including oversaturation's pending arguments.
Register that area with the existing run/root owner for its dynamic lifetime;
storing a pointer in native scratch does not keep its source SSA value live.

Use one flat PAP containing the underlying function, the supplied semantic
prefix length, and the prefix's packed runtime components. Its layout follows
the function signature, including zero-width positions. Extending a PAP
constructs one combined prefix when still undersaturated; reaching saturation
calls the underlying entry without an intermediate PAP. Do not mutate a
shared PAP or build chains of PAPs. The function reference retains its code
and signature owner, so tracing the prefix does not depend on an unowned
metadata pointer.

Generate generic adapters on demand for first-class/exported functions;
direct-only functions need only their typed entry. Deduplicate signature
marshalling routines in the existing compiled-module owner when that removes
code. A separate application cache or a handwritten family of arity-specific
interpreters is unnecessary.

**Primitive and effect boundaries.** Emit simple supported arithmetic,
comparisons, conversions, and tag operations as Cranelift operations with
their specified Haskell widths and failure conditions. Keep complicated or
external operations in their current Rust owners. A single authoritative
primitive/host signature describes representation, failure, and collection
behavior. Avoid a host call or a heap result merely to project one component
of a primitive result.

Let GHC optimize ordinary effect-library definitions and sited helper bodies.
Carry the resulting raw fields and function signatures up to the existing
Rust suspension boundary. Preserve nominal effect identity, unboxed union
indices, typed site arguments, and authority checks; do not introduce a new
effect interpreter or assume every application can suspend. Parked Haskell
continuations remain heap values under the existing root owner. This design
does not require capturing arbitrary native execution stacks.

## 7. Representation and memory ownership

### 7.1 Use immutable layout descriptors

Replace implicit all-fields-are-pointers conventions with immutable object
descriptors owned by the compiled module/machine. A descriptor states object
kind, physical field offsets, pointer bitmap or equivalent trace layout,
constructor identity/tag, and entry signatures where applicable.

Do not import GHC's info-table ABI. Tidepool can use a much smaller descriptor
appropriate to its own object kinds and collector. Rust constructs the
descriptor through checked layout types; the emitter, collector, and bridge
all consume that same descriptor.

**Selected compact layout.** Use one word to reference an immutable descriptor
for ordinary fixed-layout objects. Put kind, entry points, field/capture
offsets, trace layout, and signatures in that descriptor. Do not repeat code
pointers, field counts, arity, and constructor identities in every instance.

The proposed header encoding reserves low alignment bits of the descriptor
word for mutable object state. Heap references themselves remain ordinary
untagged addresses. Construct this encoding through one private HeaderWord
type and a checked, aligned descriptor owner; generated loads/stores use the
same layout constants. The representation profile must establish the exact
alignment, masks, and supported target width before use.

| Object | Selected payload after the descriptor/state word |
|---|---|
| Constructor | Its declared raw/reference fields |
| Function | Its declared captures; entry and arity live in the descriptor |
| SingleEntry thunk | Its declared captures, with no memoized-result machinery |
| Updatable thunk | Capture area large enough also to hold one eventual result/error reference |
| PAP | Underlying function reference, supplied semantic count, and packed argument prefix |
| Array or byte buffer | Length/capacity and inline payload, or an explicitly owned external payload reference |

Every movable object is at least two words so forwarding can use a header
and destination reference. For an updatable thunk, the first payload word can
be reused for the final result after its captures are no longer needed.
Suspended/Evaluating states trace captures; Evaluated/Failed states trace the
result/cause slot and stop retaining dead captures. Updates and their barriers
must complete in a region where collection cannot observe a half-transition.

Keep the original descriptor's allocation extent available across update
and forwarding states. Changing a large thunk's trace shape must not make a
linear heap scan advance by a smaller size into its stale payload. The first
implementation preserves its allocated extent until collection; reclaiming
or shrinking an updated object is collector work with an explicit extent
contract. Variable-size objects derive size through their checked owning
layout and length, not through arbitrary field-count arithmetic in consumers.

This is a selected Tidepool layout, not a claim of existing implementation or
an imported RTS ABI. Its tests must exercise every header state, forwarding,
large captured environments, mixed-width fields, and variable-size scanning.
If the compact state encoding fails the target's ownership/provenance checks,
use an explicit state word on thunks as the reviewed fallback; keep ordinary
constructor/function headers compact.

Separate stable constructor identity from a dense runtime tag within its
constructor family. Case dispatch can then use switches or compact comparisons
without conflating hash identity with dispatch order. Adding GHC-style
pointer tags is a measured decision in section 9.1, not a prerequisite for
this gain or for exploiting compile-time evaluatedness.

The representation must cover scalar widths, floats, raw addresses, lifted
and unlifted managed references, and zero-width components. Reject unsupported
representations explicitly. Initially target the currently supported 64-bit
platforms, with a recorded word-size/endianness profile rather than an implicit
claim of portability.

### 7.2 Expected layout changes

| Value | Current general path | Intended path |
|---|---|---|
| Unboxed integer through a call or join | Often materialized as a Lit heap object | Raw SSA/register or physical argument slot |
| Dynamically boxed Int | 32-byte constructor plus 24-byte Lit: 56 bytes | One object, potentially 8-byte descriptor pointer plus 8-byte payload: 16 bytes |
| Multiargument saturated function call | Unary entries and potentially intermediate closures | One entry call; no PAP solely because the function has multiple parameters |
| Recursive local join | Ordinary closures/applications after extraction | Blocks and jumps; no heap object for the join |
| Unboxed tuple/sum components | Ad hoc erasure, wrapping, and result splitting | Explicit scalar/reference components |
| Nullary constructor | General constructor allocation paths | Shared immutable representation where lifetime permits |

These are structural targets. The boxed-Int calculation follows the current
[heap layout constants](../tidepool-heap/src/layout.rs); it is not a claim
that every current Int costs 56 newly allocated bytes, or that total program
memory will fall by the same ratio. Literal sharing and optimized-away
allocations change whole-program results.

Account for alignment, minimum moving-object size, forwarding representation,
and descriptor lifetime before freezing the physical layout. Do not sacrifice
collector correctness to achieve the illustrative 16-byte target.

Bridges must construct the declared representation. That lets us delete
recursive unboxing and bare-Lit constructor-case tolerance from general
execution. Conversion belongs at the actual Rust/Haskell value boundary,
not throughout the evaluator.

### 7.3 Own external allocations before compacting them

Fix array ownership independently of any object-size optimization. Extend
the existing machine/heap owner to own every external allocation, with
typed cases for byte storage and reference-bearing storage. An allocation
record must retain the actual allocation layout/capacity, logical length,
trace behavior, and the information needed to retire remembered slots.

Use the owning VM context explicitly. Garbage-collector state must not be
selected through a process-global registry or an ambiguous thread-local
current-machine lookup.

Establish these contracts:

- Allocation is registered before a subsequent safepoint can observe or lose
  it. Failure while constructing its wrapper releases or retains it through
  a defined owner.
- Freeze and aliases share storage ownership. They do not create independent
  deallocation rights.
- Resize uses the real allocation layout, updates ownership consistently,
  and obeys the primitive's alias/invalidation contract.
- An external payload with references participates in tracing and barriers.
  Shared/cyclic payloads are traced without infinite recursion.
- Before releasing reference-bearing storage, remove remembered slot
  addresses into that storage.
- Drop releases all remaining owned storage, including after failed runs and
  partially failed compilation/materialization.
- External bytes contribute to the machine's resource accounting and
  collection pressure.

A machine-lifetime allocation owner is an acceptable first repair, but is
not the completed solution for long-lived sessions. Add reachability-based
reclamation before calling the array lifetime work complete.

Raw addresses into byte arrays require special care. Audit byteArrayContents#,
pinning/alignment, keepAlive#/touch# behavior, foreign calls, and the last
owning reference. A raw address is not automatically a GC root. Do not free
backing storage merely because one wrapper is unreachable while a supported
operation still has a live address into it.

GHC distinguishes managed allocation, large objects, and pinned storage.
Use those distinctions as guidance rather than routing every allocation
through an unaccounted malloc.
[GHC storage implementation](https://github.com/ghc/ghc/blob/ghc-9.12.2-release/rts/sm/Storage.c).

After lifetime correctness is established, measure which small arrays should
store payload inline and which large/pinned objects should remain separately
allocated. A single representation for all array sizes is not a required goal.

### 7.4 Globals, roots, and promotion

Prepared modules need explicit global declarations and imports. Allocate or
register the necessary global cells before linking recursive references.
Function entries can be direct references; CAFs and retained values need
managed root slots. Keep module-code dependencies separate from local
closure captures.

Stage a module's declarations, descriptors, and constructor mappings before
publishing its entry or updating shared lookup state. A failed add_function
must not replace the interpretation of already compiled code or leave a
half-initialized global visible. Temporary initialization roots belong to
this same linking owner.

Global references must keep their defining code, descriptors, literal data,
and stack maps alive. Do not turn a zero-capture STG closure into a claim that
its module is self-contained or has no lifetime dependencies.

The current tenure operation measures and copies an entire reachable graph,
then performs a minor collection to repair sibling references. That work is
correctness-bearing today. Measure its contribution to repeated binding
costs before changing it.

Make binding persistence register a stable root, with promotion performed
by the ordinary collector. Retain the live binding reference in the
existing registry, collect it alongside all other roots, and let ordinary
promotion update that same slot. That removes a separate measure/copy/fixup
mechanism once sibling, age/promotion, and cross-run root obligations pass.
It requires a collector and root-lifetime design that handles persistent
nursery roots correctly; it is not safe to delete the fixup in isolation.

Coordinate code lifetime with the existing
[JIT memory lifetime plan](actor-model/jit-memory-lifetime.md). Actor or scope
retirement alone cannot free code still referenced by another actor's closure,
retained binding tip, or parked continuation.

### 7.5 Make allocation and collection follow the prepared shapes

**Static allocation.** Materialize nullary constructors and eligible immutable
global constructor graphs once per owning module. Share capture-free function
objects when no activation-specific state is needed. Code and literal/string
data belong to that same owner. Keep updatable CAF cells per machine;
single-entry assumptions and memoized failures must not leak across machines.
Direct-only code needs no function object until a value reference requires it.

**Nursery allocation.** Plan each fixed-size constructor, closure, or PAP from
its checked descriptor. Start with the existing inline bump path, with the
typed failure correction from M2. Within a straight-line region, combine
adjacent fixed-size allocations whose intervening operations cannot collect,
fail, call out, or select a different path. Check/reserve their total once,
derive object addresses by constant offsets, initialize every field, and
publish the resulting references only when they are valid.
Only group objects that must be live together at publication; do not extend
temporary lifetimes to justify a larger reservation. Keep initialization
stores and raw/reference layout checks inside the owning allocation emitter.

Keep the reservation bounded; preserve the per-object path for large groups
and variable sizes. Do not reserve the maximum of unrelated case branches
or trigger OOM in an otherwise nonallocating path. Tune the bound against
checks, spills, unused space, and failure behavior. This is a local allocation
emission plan, not another semantic normalization pass.

**Roots.** At creation of each managed SSA value or block parameter, attach
the stack-map requirement through the typed emitter. Raw addresses and raw
numeric values are different cases; they must not enter that map by virtue
of being pointer-sized. Let Cranelift's liveness determine which managed
values survive a safepoint. Reload moved values through its supported
spill/reload machinery and recompute derived field addresses afterwards.

The pinned frontend's safepoint analysis is opcode-based. Merely classifying
a host call as NoGc in Tidepool does not promise that Cranelift removes its
spills. Use that classification for our own rooting/allocation-region
contracts; eliminate trivial calls by emitting their operations directly.
Do not add a second liveness engine or change dependencies just to obtain
an unmeasured no-GC-call fast path.
[Pinned Cranelift safepoint analysis](https://github.com/bytecodealliance/wasmtime/blob/v42.0.1/cranelift/frontend/src/frontend/safepoints.rs).

**Stores.** A field store knows its layout and whether it contains a managed
reference. Omit barriers for raw fields and for proven initialization into a
fresh nursery object before any safepoint. Route all other reference writes,
including old thunk updates, mutable arrays, and imported-object mutation,
through the owning barrier protocol. Reuse the proof carried by the
allocation region; do not infer youth from a stale cached address after GC.

**Arrays.** Use managed inline storage for eligible small unpinned payloads
and owned external storage for large or pinned payloads. The byte threshold
is tuning; both paths share the same logical array operations, mutation
rules, and accounting. Pointer-exposing operations must follow the supported
address/lifetime contract in section 7.3. Inline placement is not permission
to move an object while an allowed raw interior address is still in use.

These choices improve allocation checks, tracing volume, root spills, and
write barriers using the same representation facts. Measure each category
independently; smaller objects do not establish fewer live roots or faster
collection by themselves.

### 7.6 Collect retired data and preserve globals reached through code

The current nursery collector and graph-tenure path do not supply a major
old-space collector. Scope retirement deregisters roots but leaves old-space
bytes and stable slot storage allocated. The target therefore includes actual
full collection, not just replacing tenure with a differently named copy.

Keep a copying nursery and use one descriptor-driven evacuation/tracing
implementation for promotion and full collection. The simple initial
promotion policy moves surviving nursery objects into old space at a minor
collection; it needs no per-object age field. Validate its retention cost
before adding an aging policy. A full collection traces all live movable
generations, compacts reachable objects, and sweeps unreachable owned
external storage. Pinned allocations retain their addresses and participate
in reachability without entering the moving-copy path.

Trigger full collection through the owning machine's allocation-pressure
policy, with an opportunity after substantial root retirement. Account for
copy-space peak memory. Reserve required destination capacity before
installing forwarding state; failure must follow the integrity rules in
section 8. Minor and major collections use the same complete-root contract.

The remembered set accelerates minor collection. During full collection,
an old-to-young slot in an unreachable old object is not an independent
strong root. Trace through live owners and rebuild or clear remembered
entries as their owners move or die. Include external mutable payload slots
in this rule, so neither dangling slot addresses nor unnecessary retention
survive reclamation.

**Code-to-data dependencies.** A function can refer to a global thunk without
capturing it. Record those direct dependencies with its compiled descriptor/
entry and traverse them when the code is reachable from an activation,
callable export, managed closure, or parked continuation. Follow dependency
cycles with ordinary graph marking; do not form permanent Arc cycles between
compiled modules. Use the existing code/module and root owners.

This adopts the purpose of GHC's static reference tables: code references
must keep relevant static computations alive. Their GHC representation and
Cmm construction pass are unnecessary here; Tidepool can project its own
direct dependency edges and consume them through the same collector.
[GHC static reference tables](https://github.com/ghc/ghc/blob/ghc-9.12.2-release/compiler/GHC/Cmm/Info/Build.hs).

A root-slot address baked into surviving JIT code is a lifetime dependency,
even when no binding-table entry names that value anymore. Keep both its cell
and referent valid while such a use is reachable. Conversely, do not root
every global forever merely because its code was once compiled. Retire cells
only after executable references are gone, coordinated with code reclamation.

Acceptance must include a closure escaping its original scope while using
a global, then major collection and invocation; mutually dependent modules;
an updated old thunk whose dead captures are reclaimed; unreachable cycles
through boxed arrays; and repeated bind/retire cycles whose live old-space
and external bytes return to the expected baseline. This full-collection
work is part of M5 and the production cutover gates.

## 8. Failure must have a control-flow representation

These fixes are urgent even before the STG cutover.

### 8.1 Eliminate allocation-shaped poison

A failed allocation must leave the generated function through a failure edge
before any header or field writes. Returning an oversized object-shaped
buffer is not failure propagation.

Reuse the machine's existing error owner. Distinguish normal return, pending
tail application, and failure through an explicit control discriminant or
equivalent typed protocol. Keep the error payload separate from ordinary
value registers. Do not overload null to mean several unrelated states.

Every fallible host call and generated operation must connect to this
protocol through one emitter helper. A caller cannot consume its value
results after failure. Pure arithmetic that cannot fail need not acquire
extra checks.

The existing host-function registration owner should supply each function's
ABI, whether it can collect, and whether it can fail. Emission should derive
rooting and failure handling from that contract rather than requiring each
primitive arm to remember them independently.

The JIT ABI representation of the control outcome must work for pointer,
scalar, floating-point, and multiple-result functions. An error path cannot
pretend a poison pointer is a valid Double result.

Resource exhaustion paths should preserve a nonallocating error code/site
where possible; reporting failure must not depend on allocating a diagnostic
string from the exhausted resource. Audit checked sizes and narrowing
conversions before emission so object sizes and capture counts cannot
silently truncate.

Delete the global writable poison scratch buffer and its size-accommodation
tests after meaningful propagation/cleanup tests replace them. An immutable
deferred error value, if required, is a different mechanism with an ordinary
managed lifetime and explicit demand semantics.

### 8.2 Require a complete root snapshot

Change the frame-walk/collection interface so successful construction means
the root walk completed under its contract. A failure carries a typed reason;
collection does not receive a partial vector masquerading as a valid result.

Distinguish a normal end of the JIT activation from a corrupt frame or missing
required safepoint metadata. Do not simply reject every native frame or every
address with no root slots: host/JIT sandwiches and genuine zero-root calls
must be represented correctly.

Make complete metadata an invariant of compiled-function registration.
Use explicit activation boundaries or an equivalent proven termination rule
for the frame walk. All live registry categories must be installed together
at the owning run/collection entry.

Failure before collection should abort the operation without moving objects.
Failure after heap integrity is uncertain must make the machine unavailable.
Logging and continuing with an incomplete root set is not an accepted path.

### 8.3 Separate language errors, cancellation, and native integrity failure

Expected Haskell errors are ordinary runtime outcomes. Cooperative cancellation
has explicit checkpoints and settlement. Native memory faults indicate an
integrity failure with a different recovery contract.

The current signal wrapper can jump across Rust host frames, bypassing their
destructors. Retaining a Vec or RefCell borrow across that boundary cannot
be made safe by claiming all surrounding references are raw pointers.
Audit the transitive callback path, not only the closure passed to the wrapper.

This plan does not assume catching a native signal restores a reusable heap
or process. Remove reliance on signal recovery for expected failures. At a
minimum, prevent reuse of a machine whose integrity is unestablished and
report the loss through existing session supervision. If a fault may have
corrupted process-wide Rust/allocator state, machine retirement alone is not
proof of safe process continuation; use the existing fatal/supervision policy.

Do not replay effects during recovery. Preserve committed prefixes, receipts,
and uncertainty under the current runtime contracts.

## 9. Compilation cost and observability

STG adds GHC passes but should remove Tidepool work. Measure the combined
path rather than reporting only a faster backend phase.

Consume closure capture annotations directly. Avoid storing a free-variable
set for every expression subtree. A parser can verify capture uses against
the closure's declared environment while traversing its body, using nested
capture declarations at nested boundaries. Work should scale with input,
dependency edges, and actual capture output, not all transitive sets at all
nodes.

Delete repeated indexed access through Haskell lists where an indexed
structure is required. Eliminate per-binding transitive dependency searches
once proper groups and declarations make them unnecessary. Do not cache a
needlessly expensive representation as the first response.

Use the existing timing and machine owners for measurements:

| Category | Measurements |
|---|---|
| Frontend | Load, typecheck, Core optimization, resolution, site elaboration, CorePrep, STG passes, projection, serialization |
| Rust compilation | Parsing/linking, descriptor construction, IR emission, Cranelift compilation/finalization |
| Execution | Execution separately from forced rendering/marshalling |
| Allocation | Objects and bytes by constructor, thunk, function closure, PAP, array descriptor, and external payload |
| Collection | Collections, pause time, copied/promoted bytes, root counts by existing category |
| Residency | Nursery, old space, external payloads, code, literal data, and compiler/stack-map metadata |
| Session behavior | Cold/warm compile, repeated bind, retained-value calls, effect suspend/resume, and scope retirement |

Do not interpret the current heap_stats live_bytes field as total live memory.
Ownership counters are required for leak tests; RSS is supplementary.

Reuse compiled definitions only through the existing compilation owner, with
keys that include resolved identities, capture layout, compiler/representation
profile, and relevant imports. Identical source text alone is insufficient.
Avoid adding another frontend cache.

Audit cache inputs for Template Haskell dependencies and other unenumerable
reads. Reuse GHC dependency evidence where available and follow toolchain
policy for explicitly uncacheable invocations. No stale-TH result has been
demonstrated by this review; add a reproducer before assigning it that status.

The standalone tidepool-optimize passes are not on the inspected production
execution path. Optimizing that crate is not a substitute for measuring and
improving the real frontend/backend boundary.

### 9.1 Optimization choices and acceptance experiments

Sections 1.2, 6.5, and 7 select the architecture now. The following table
records the production candidate for each remaining choice and the experiment
that can justify changing it. M7 verifies these choices with evidence; it
does not begin the architectural design after a merely functional importer.

| Opportunity | Selected implementation/candidate | Evidence that governs acceptance or revision |
|---|---|---|
| Core optimization profile | Restore full laziness in the production candidate, retain the normal optimized worker/wrapper/specialization/rewrite pipeline, and remove the ineffective CPR override | GHC output confirms workers/fusion survive; execution and resident-retention results can justify a specific exception |
| STG CSE and late lifting | Retain native CSE; compare lifting off with the existing GHC pass under conservative argument limits appropriate to our entry ABI | Captures and closure bytes saved versus added argument traffic, indirect calls, code size, and compile time |
| Dynamic application | Typed direct entries plus signature-generated generic adapters and flat PAPs; share marshalling code through the existing module owner | No generic calls for known saturated applications; generated adapter bytes and genuine PAP counts bound further specialization |
| Static allocation | Share eligible immutable constructors and function objects, keeping per-machine CAF state separate | Allocations disappear at the identified sites without preventing retirement of unrelated module state |
| Runtime pointer tags | Use untagged heap references and descriptor dispatch; compile-time known-value uses already bypass forcing | Add tags only if remaining unknown-value dispatch is a measured material cost and the prototype wins after GC, untagging, and code-size costs |
| Allocation checks | Implement bounded straight-line reservation over fixed-size objects; retain the existing per-object fallback | Fewer checks/spills with identical accepted failure paths; tune the bound without reserving across arbitrary branches |
| Standard thunk shapes | Start with the single correct updater and normal compiled bodies; use shared shape-specific entries only where body/code duplication is significant | Count selector/application shapes and retained captures first; require a net gain and no second update state machine |
| Array placement | Inline eligible small unpinned payloads; own large/pinned storage externally | Tune one placement threshold using copied bytes, memory pressure, pinning behavior, and reclamation |
| Binding persistence | Stable root registration and collector-owned promotion/full collection replace per-binding graph tenure/fixup | Sibling-sharing and old-to-young edges remain correct; new major-collection tests preserve retained closures and parked continuations while reclaiming dead old space |

**Late lifting.** Reuse GHC's pass and controls; do not write another Rust
closure-lifting analysis. Keep its protections for joins, updatable bindings,
and known calls. Set conservative argument limits from Tidepool's physical
entry budget after vmctx, environment, and result transport, rather than
pretending GHC's register budget is ours. The stock closure-growth estimate
also assumes GHC layouts, so actual capture allocation and argument traffic
must arbitrate the result. Compare lifted and unlifted prepared programs and
record the selected profile; do not change GHC platform constants globally
to manipulate one optimization's cost model.
[GHC lifting analysis and cost model](https://github.com/ghc/ghc/blob/ghc-9.12.2-release/compiler/GHC/Stg/Lift/Analysis.hs).

**Further dynamic dispatch work.** Separate unknown-thunk entry cost from
constructor-tag loads and unknown-function application cost. Pointer tags
cannot be credited with checks already removed statically, and apply-stub
specialization cannot be credited with turning a known call direct. If
dynamic tagging is justified, define tag-bearing references at all producers
and consumers, preserve tags through GC, and untag through one owning type.
Keep this a reviewed ABI revision, not a mask scattered through emitters.

For selector thunks, the code-sharing candidate is a descriptor carrying the
selected field/layout and a shared body using the ordinary force/update
protocol. A collector rewrite that shortens selector retention is a separate
candidate with separate correctness coverage. Do not combine the two into
an unexplained generic thunk optimization. Application-thunk sharing follows
the existing signature/argument-area protocol rather than introducing another
argument representation.

The current allocator already emits its bump-pointer fast path inline; do
not report that as a new STG benefit. Reservation changes need checked size
arithmetic, a defined state for unused space, complete roots on the slow path,
and no publication of partially initialized objects. Do not hoist a failing
allocation across arbitrary control flow or move cancellation checks out of
unbounded loops. GHC's backend also weighs these placement costs; prepared
STG does not perform Tidepool's allocation planning for it.
[GHC case/heap-check placement](https://github.com/ghc/ghc/blob/ghc-9.12.2-release/compiler/GHC/StgToCmm/Expr.hs)
and [heap emission](https://github.com/ghc/ghc/blob/ghc-9.12.2-release/compiler/GHC/StgToCmm/Heap.hs).

Shared selector/application entries are production GHC techniques described in
[GHC closure selection](https://github.com/ghc/ghc/blob/ghc-9.12.2-release/compiler/GHC/StgToCmm/Closure.hs).
Keep the ordinary correct thunk implementation when a special path does not
justify its complexity. In particular, do not introduce a duplicate thunk
state machine merely to share entry code.

For each row, record the owning consumer, baseline, selected candidate,
semantic coverage, measurements, code/deletion effect, and accepted revision.
Departures from the selected design need a concrete result or applicability
reason. A temporary deferral must name its prerequisite and be resolved before
M7 closes. Keep this review with the implementation evidence; it is not a new
permanent optimization registry or a collection of user-facing tuning modes.

### 9.2 Concrete paths that must become cheaper

These are worked structural targets for the implementation corpus, not
measurements of a backend that has already been built.

**Numeric worker loop.** A prepared worker with Int# index/limit/accumulator
becomes typed block parameters, comparison/addition, a cancellation backedge,
and a return. It allocates no Haskell numeric objects merely to carry those
values, uses no recursive native call for an eligible self-tail edge, and
does not enter the generic application protocol. Measure input construction
and final boxing separately from the loop.

**Lazy multiargument call and real partial application.** The prepared form
of keep 42 (loop 0) passes a thunk reference to one two-parameter entry; the
callee returns its first argument without entering the second. A first-class
keep 42 can legitimately require one PAP. Applying that PAP to the remaining
argument reaches the same body without another intermediate PAP. Verify the
semantic distinction with a nonterminating or failing unused computation.

**Unpacked data and strict reference fields.** An unpacked pair of machine
integers has two raw fields in one constructor object; extracting either field
loads a scalar directly. A strict lifted field remains a reference to a value,
and its evaluatedness can eliminate forcing before a subsequent case. Do not
confuse those two representations. The bridge builds each through the same
descriptor the emitter and collector consume.

**Resident reuse and suspension.** A retained function uses an existing code
entry and a live environment root. Binding another name to a shared graph
registers its root without copying that graph through a separate tenure pass.
A parked effect continuation keeps its heap environment and metadata owners
live through the existing registry; resumption uses the same call/materialize
route. Root counts, code bytes, and graph copying explain the cost of each
operation, and retirement demonstrably releases its unreachable state.

Each example needs both behavior coverage and structural evidence from the
selected output: allocation categories, emitted call kind, root/trace layout,
or absence of the obsolete mechanism. Timing noise cannot establish that a
PAP or boxed accumulator has disappeared.

## 10. Deletion ledger

The implementation should track deletion obligations alongside new code.
Do not count a move to a different crate, a generated copy, or a renamed
fallback as a deletion of a mechanism.

| Existing mechanism | Replacement owner | Required retirement |
|---|---|---|
| Arbitrary-expression App preparation and recognizable-error argument exceptions | GHC preparation and atom-only execution schema | Remove App-position shape heuristics |
| Unary lambda-chain compilation | Function RHS and one parameter-list emitter | Remove intermediate-lambda machinery for saturated calls |
| Recursive-join-to-lambda conversion in Haskell | GHC StgLetNoEscape projection | Delete tsRecJoinIds conversion and jumpCrossesLam repair where superseded |
| A second crossing-join lowering pass in Rust | Parsed join scope plus direct block emission | Delete lower_jump_crosses_lam; invalid synthetic shapes become parser tests |
| Constructor wrapper/worker shortcuts | GHC-prepared constructor functions and layouts | Remove worker substitution that stores boxed fields in raw slots |
| Rust synthesis of missing constructor lambdas | Prepared bindings/linking | Retire datacon_env wrapping once all production constructors are provided |
| Repeated boxing normalization | Correct producer/bridge representations | Delete relevant normalize passes and tolerance consumers |
| Per-node free-variable index for closure compilation | GHC capture annotation | Remove production compilation's full-subtree capture analyses |
| Huge flattened Rec and deferred-simple transitive ordering | Module-aware groups and explicit linking | Delete deferred-simple scheduling and patch policies no longer required |
| Occurrence-name append/error/magic-function guessing | Exact identity, supported primitives, real bodies | Delete ad hoc semantic name rules |
| Specialization-name reconstruction | Exact interface/unfolding bodies | Delete guessed aliases and dictionary-repair fallback |
| Handwritten string unfolding and UTF-8 assumptions | Real CString bodies or one pinned encoding adapter | Remove duplicate string expansion/decoding implementations |
| Split result primitive families | Explicit multiple-result primitive signatures | Delete redundant projections and repeated result computation |
| Object-shaped OOM scratch | Typed failure control | Delete writable poison allocation and size-accommodation policy |
| Early thunk memoization plus arbitrary indirection limit | Update-through-WHNF | Delete the 64-million-follow recovery mechanism |
| Repeated shape checks using names/field counts | Parsed representation descriptors | Remove duplicated semantic interpretation from emit, force, and bridges |
| Per-instance code pointers, field counts, and constructor identities | Shared immutable descriptors | Remove duplicated fixed metadata from constructor/function instances |
| Repeated entry/force checks at proven value uses | Scoped evaluatedness and one call selection owner | Remove redundant dynamic demand from those occurrences |
| Per-join subtree backedge discovery | Prepared binding groups and emitted loop structure | Remove rhs_contains_backedge where explicit group/control-flow facts supply the checkpoint obligation |
| Per-binding graph promotion/fixup | Ordinary collection plus stable root registration | Delete the separate mechanism after equivalent sibling/root coverage and full collection pass |

Some GC and object-construction code remains essential. SCCs still need
recursive initialization; dynamic calls still need runtime dispatch; unknown
input and native layout boundaries still need checking. The goal is to
remove unnecessary cases and duplicated policy, not to remove checks by
assuming away their obligations.

For scale, physical file line counts at the reviewed baseline include:

| File | Lines, including comments and in-file tests |
|---|---:|
| haskell/src/Tidepool/Translate.hs | 2,961 |
| haskell/src/Tidepool/Resolve.hs | 458 |
| tidepool-repr/src/normalize.rs | 1,316 |
| tidepool-repr/src/free_vars.rs | 506 |
| tidepool-codegen/src/lower.rs | 568 |
| tidepool-codegen/src/emit/expr.rs | 3,253 |
| tidepool-codegen/src/emit/apply.rs | 379 |
| tidepool-codegen/src/host_fns/errors.rs | 1,420 |
| tidepool-codegen/src/host_fns/force.rs | 792 |

These are a size baseline, not an estimate that all those lines can disappear.
Track production code, tests, generated code, and comments separately in the
implementation report. The completed migration should show a net reduction
in maintained production engine code and in the number of mechanisms.
Temporary parallel paths must have an identified deletion milestone.

## 11. Implementation sequence

The phases are reviewable obligations, not a request to implement the entire
design in one patch. Establish the shared schema/semantics before assigning
independent implementation work. Changes to tests and a plan have been
authorized so far; this document does not itself claim an implementation
or a live deployment.

### M0 — Make the baseline independently observable

Owners: tidepool-testing and the existing engine test suites.

Deliver:

- Retain the four review regressions with GHC answers.
- Add the missing focused semantic cases from section 12.
- Record native GHC, reference evaluator, and JIT outcomes separately.
- Measure representative cold/warm compile and execution workloads.
- Capture production code-size and allocation-accounting baselines.

Gate: the known failures are individually reproducible, and successful,
compile-error, runtime-error, timeout, and native-fault outcomes cannot be
collapsed into a single "both engines failed" result.

The current one-second cancellation watchdog is a bounded reproducer, not a
proof of divergence. Before relying on it as a durable gate, use the existing
watchdog infrastructure with a hard process-level bound and meaningful
evaluation-entry evidence where available.

### M1 — Prove the actual STG integration boundary

Owner: haskell, through the existing extractor entry.

Deliver:

1. Preserve module context and move site elaboration before preparation.
2. Resolve representative local, library, recursive, and resident dependencies
   without specialization-name guesses.
3. Run Core lint, CorePrep, Core-to-STG, the selected STG pipeline, and explicit
   post-preparation checks.
4. Inspect the resulting STG and project a provisional execution-schema
   fixture sufficient to check all supported node forms.
5. Record what current translator mechanisms the real output eliminates.

The probe must include strict/unpacked fields, multiargument calls, recursive
joins, unboxed tuples/sums, void arguments, strings with NUL, Text, Integer,
typed effect sites, and an imported retained session value.

Gate: a documented pass configuration and module/import contract work on the
real extractor path, including cold/warm and direct/resident modes. A CLI
GHC dump alone does not pass this gate.

If native tag inference depends on an import fact that Tidepool cannot honor,
make that fact conservative at the owning Haskell interface boundary or
explicitly handle its obligation in the projection. Do not disable strictness
rewriting silently. Record any deviation from the native configuration.

### M2 — Repair failure and external-storage ownership

Owners: tidepool-codegen and tidepool-heap, with runtime/session classification
through the existing runtime owner.

Deliver:

- Explicit allocation/host-call failure propagation before value use.
- A complete-root-snapshot requirement for collection.
- Ownership and teardown accounting for byte/boxed-array storage.
- Defined machine availability after integrity failure.
- Tests for failure, cancellation, cleanup, and concurrent machine isolation.

Gate: no generated post-failure initialization into shared scratch; no
collection with known incomplete roots; no remaining unowned array payload;
ordinary language errors leave a reusable machine only when its invariants
are actually preserved.

This work can proceed while M1 establishes the GHC boundary because the
current unsafe failure paths do not depend on adopting STG.

### M3 — Land the execution schema and Rust construction boundary

Owners: haskell projection/encoding, tidepool-repr parsing, tidepool-eval.
The codegen owner supplies the native ABI proof before emitter work starts.

Deliver:

- Recursive execution tree with atoms, distinct RHS forms, grouped joins,
  representation signatures, declarations, and explicit imports.
- Private invariant-bearing Rust types and structured parse/link errors.
- Scoped evaluatedness evidence, direct code/global dependencies, and one
  authoritative call/result-layout constructor.
- A small reference evaluator with correct lazy application and thunk updates.
- Round-trip fixtures and malformed-input rejection tests.
- The section 6.5 ABI proof: typed entries/adapters, scalar and large results,
  tail calls with changing argument sizes, and roots on both target platforms.
- A written format/ABI migration record.

Gate: the schema preserves the useful STG distinctions without serializing
GHC types. Tests cannot bypass the production constructor and accidentally
exercise a different preparation contract.

### M4 — Implement direct STG code generation

Owner: tidepool-codegen.

Deliver:

- One function body per function RHS with multiple parameters.
- Typed scalar/reference results and case/block parameters.
- Known saturated calls, dynamic eval/apply, and PAP handling.
- Local recursive joins and eligible self-tail calls as blocks/loops.
- Direct known-value paths from scoped evaluatedness evidence.
- Distinct local SingleEntry execution without ordinary thunk updates.
- Correct update frames, persistent thunk errors, and tail/cancellation paths.
- One explicit ABI for host calls and generated control outcomes.
- One call selection owner, flat PAPs, and on-demand generic adapters.

Gate: the independent semantic corpus passes, including mixed raw/reference
arguments across collections and oversaturated/partial calls. A known
saturated call creates no intermediate PAP solely to accommodate arity.
A local join has no heap closure solely to implement its control flow.
An eligible self-tail call has no recursive native call, a proven value use
has no general force call, and a verified SingleEntry thunk has no ordinary
memoization frame. Cover their failure/cancellation paths as well as their
generated shape.

### M5 — Use precise layouts, allocation, and full collection

Owners: tidepool-heap and tidepool-codegen bridges/collector.

Deliver:

- Checked descriptors used consistently by allocation, emission, tracing,
  and Rust value marshalling.
- The compact header/state protocol, including forwarding and preserved
  allocation extents after thunk updates.
- Raw constructor fields and scalar-preserving calls/joins.
- Rooting and barriers for mixed layouts and external reference payloads.
- Static immutable objects, bounded straight-line allocation groups, and
  the shared inline/external array placement policy.
- Collector-owned promotion and a major collector, with code-to-data edges
  keeping reachable globals and their slots alive.
- Reachability-based external-storage reclamation, beyond machine-drop
  cleanup alone.
- Dense constructor dispatch where profitable.

Gate: no pointer/scalar confusion under stress collection; retained aliases
and addresses remain valid under their documented contracts; external bytes
return to the ownership baseline after release/drop; the representative
boxing and PAP allocation counters demonstrate the intended reduction.
Repeated retirement followed by full collection must reclaim dead old-space
and external allocations while escaped closures still execute correctly.

M4 and M5 share an ABI. Introduce that shared contract before implementation;
do not independently invent incompatible call and heap representations.

### M6 — Cut over production and delete the old path

Owners: extractor/artifacts, toolchain, codegen, runtime, and tests.

Deliver:

- Initial and incremental production entry points use the same prepared
  program construction path.
- New wire/profile versions are propagated through artifacts, fingerprints,
  daemon compatibility, and fixture generation.
- Old translation/normalization/lowering mechanisms identified in the
  deletion ledger are removed or explicitly justified by a remaining consumer.
- Existing effect/session behavior passes the integration suite.
- The selected GHC optimization profile preserves workers, unboxing, and
  site behavior through actual production extraction.

Gate: one production execution path, four original regressions passing, no
fallback to the old engine on unsupported input, and a reviewed net code-size
and conceptual-complexity reduction.

Temporary comparison code may exist in test/probe work during M1–M5.
It must not become a permanent user-selectable backend or a hidden recovery
path. Preserve regression coverage while deleting obsolete implementations.

### M7 — Measure and simplify the remaining cost centres

Owners: their existing production consumers.

Deliver:

- End-to-end performance comparison with phase and allocation breakdowns.
- The section 1.1 facts have production consumers and demonstrated removal
  of the corresponding repairs, allocations, or dynamic work.
- Every section 9.1 opportunity has an evidence-backed adoption or rejection
  decision, with no unresolved "later" optimization bucket.
- Code-lifetime work coordinated with its existing plan.
- Validation of the chosen ABI/layout/collector and tuning of the specified
  allocation, array, and lifting limits, rather than postponing their design.
- A final deletion/accounting report and focused standing source contracts.

Gate: meaningful execution/allocation improvements on the designated workloads,
acceptable total interactive compilation latency, and no unexplained session
residency growth. If added GHC passes dominate latency, remove redundant
frontend/backend work and measure reuse before adding another cache.
M6 establishes the working production cutover; M7 is required to complete
the requested redesign and performance review.

## 12. Validation matrix

Test the smallest affected boundary during implementation. Use the broader
cross-boundary checks at schema/backend cutover, not after each small edit.

### 12.1 Define the supported contract before declaring completion

Maintain an exhaustive support inventory for the pinned GHC version:

- every STG expression, RHS, binding, alternative, and update form;
- every runtime representation admitted by the projection;
- every exposed primitive and foreign intrinsic, including its signature,
  defined input conditions, collection behavior, and failure behavior;
- the resident/effect operations that transport values across runs.

Each item must be classified as implemented with named acceptance coverage,
eliminated by a specified upstream pass with a checked output invariant, or
explicitly unsupported with a tested diagnostic. Do not use "probably removed
by GHC" as a fourth category. Ordinary functions resolved from libraries are
tested through their actual bodies and dependencies, not marked supported
merely because their names were recognized.

The inventory belongs with the owning implementation/signature definitions
where possible. Avoid a second handwritten table that can silently disagree
with executable support. Exhaustive matches and tests should expose additions
when the pinned GHC API changes.

A trustworthy implementation means agreement on this declared contract,
not an unsupported claim to implement every facility of the GHC RTS.
The plan is comprehensive work to reach that endpoint; its existence is
not evidence that the endpoint has already been reached.

### 12.2 Behavior, lifetime, and integration coverage

| Area | Required cases | Independent observation |
|---|---|---|
| Laziness | Ignored bottom/divergence, lazy constructor fields, strict fields, seq, partial applications | GHC native results or bounded observation contexts |
| Thunks | Sharing, self/mutual aliases, long chains, re-demand after error, cancellation during update | GHC for language results; explicit update/ownership assertions for host policy |
| Prepared facts | Scoped evaluatedness, strict fields, SingleEntry, false/stale imported promises | GHC results, optional fact assertions, and absence of redundant entry/update work |
| Calls | Zero/one/many arguments, under/exact/oversaturation, returned functions, mixed scalar/reference args | Results plus PAP/closure allocation counts |
| Arity | Void args before/between values, unboxed tuple/sum parameters and results | GHC prepared signatures and native behavior |
| Joins | Self/mutual recursion, nested joins, captured surrounding variables, nonescaping scope, backedge cancellation | Real extracted programs and absence of closure allocation for local joins |
| Constructors | Strict/unpacked fields, lazy fields, mixed pointer/scalar layouts, newtypes, nullary values | GHC results and descriptor-driven trace checks |
| Names | Local append/error/unpack-like names, reexports, same module names in distinct units | Exact defining identity and GHC results |
| Strings/chars | Embedded NUL, multibyte characters, empty strings, boundary codepoints, Haskell Char versus Rust char | GHC oracle; explicit encoding/bridge policy |
| Numbers | Int/Word boundaries, float width/NaN/infinity/negative zero, Integer/Natural workloads | Compare defined GHC behavior; no claim that unsafe primops have defined out-of-range behavior |
| Arrays | Allocate/clone/freeze/shrink/resize, shared payloads, cyclic boxed arrays, old-to-young writes, live interior addresses | Ownership counters and GC-stress results |
| Failures | OOM in headers/captures/arrays/marshalling, root-walk failure, host-call errors, independent machines failing concurrently | Typed outcomes, no post-failure stores, cleanup, survivor isolation |
| Resident values | Repeated use, shared binding tips, producer retirement with surviving consumers, cross-fragment calls | Existing session semantics and root/code liveness |
| Effects | Initial and resumed completion, several parked continuations, sibling work, typed site answers, row order | Handler-visible trace and existing receipts |
| Compilation | Cold/warm/direct/resident, missing interfaces, new prep dependencies, source/flag/unit changes | Artifact equality where promised and honest invalidation |
| Platforms | x86_64 and aarch64 calling/rooting/layout paths | Targeted native tests on both architectures |
| Native ABI | Large and mixed result vectors, result-area tail forwarding, changing stack arguments, dynamic argument roots | ABI probes plus source-level differential executions under stress GC |
| Full collection | Retired old graphs, escaped code/global dependencies, external cycles, rebuilt remembered sets, update trace-shape changes | Live-byte/root ownership counts after collection and successful survivor invocation |

Before cutover, run the complete production route from authored Haskell
through extraction, serialization, parsing/linking, JIT execution, Rust
effect handling, suspension, resumption, and later reuse of retained values.
Exercise both successful completion and failure after an already committed
effect prefix. Passing isolated emit tests cannot replace this acceptance.

Include stress modes that collect at eligible allocation/call boundaries,
vary nursery sizes, and interleave independent machines and sibling
continuations. The portable subset should also run through the reference
evaluator and native GHC oracle. Use deterministic seeds and retain minimized
regressions for every discovered mismatch.

The Haskell Char domain deserves explicit treatment: current Literal stores
a Rust char, which cannot represent surrogate codepoints. Determine the
supported Haskell observation and encoding behavior with tests, then choose
an appropriate codepoint type or an explicit boundary policy. Do not silently
substitute scalar-value semantics for Haskell Char.

For literal decoding, GHC provides a modified UTF-8 implementation including
utf8DecodeByteString. Use that implementation where the projection actually
needs codepoints; preserve raw bytes for address/string-literal representations.
[Pinned decoder](https://github.com/ghc/ghc/blob/ghc-9.12.2-release/libraries/ghc-boot/GHC/Utils/Encoding/UTF8.hs).

For generated tests, distinguish programs in the supported execution language
from malformed IR. Compile failures do not count as successful execution
equivalence for a valid supported program. Compare effect traces without
performing real provider, process, or resource mutations.

Performance workloads should include:

- a raw numeric accumulator and a fusion-friendly list fold;
- a higher-order workload that genuinely needs partial application;
- recursive local control flow with several parameters;
- Text processing and Integer arithmetic with array allocation;
- repeated resident compilation/binding and calls into retained closures;
- effect suspend/resume with live sibling continuations.

Add the worked paths from section 9.2 and repeated bind/retire/full-collection
cycles. Attribute pure execution, handler overhead, value marshalling, and
code/data retention separately; provider network time is not an engine
benchmark. Track generic calls, force entries, update frames, heap checks,
and root spills as well as objects/bytes, so each use of STG information has
a visible effect.

Use release builds, fixed input sizes, repeatable inputs, separate warm/cold
measurements, and enough repetitions to report distributions. Include
allocation counts and emitted code size so a timing fluctuation cannot
masquerade as a structural improvement.

## 13. Migration and compatibility

The current TPLR format is version 3.0. The new expression vocabulary and
required representation metadata warrant a coordinated major-version change,
not optional keys whose absence silently selects the old semantics.

In one reviewed migration:

1. Update the Haskell writer and Rust parser together.
2. Version every affected metadata/sidecar contract explicitly.
3. Include the execution schema, runtime layout/ABI, GHC/toolchain, and pass
   profile in compatibility/fingerprint decisions.
4. Reject stale artifacts clearly and regenerate fixture corpora through the
   repository commands.
5. Prevent a machine from accepting a fragment built for another execution
   ABI or incompatible constructor/global signature.
6. Coordinate installed extractor/runtime compatibility through existing
   deployment owners.

There is no general in-place migration of live closures between heap ABIs.
An implementation/deployment plan must state which running machines can drain,
which retained values are lost when their owner is replaced, and what existing
recovery receipts establish. Recompiling source does not recreate arbitrary
live values or undo/redo external effects.

Do not keep the old IR interpreter as an automatic compatibility fallback.
If an external consumer needs a transition window, make that a separate,
explicit migration decision with a removal date and tests.

## 14. Completion criteria

The work is complete when:

- The pinned STG/representation/primitive support inventory has no
  unclassified items and links supported behavior to acceptance coverage.
- GHC-prepared STG is the production handoff, including CorePrep.
- The Rust parser/linker constructs the sole invariant-bearing execution
  model; both initial and incremental compilation consume it.
- All four demonstrated semantic failures are fixed and independently tested.
- Joins, arity, representations, captures, and thunk updates retain their
  prepared meaning through execution.
- The useful facts in section 1.1 actively simplify production execution;
  preserving them in a schema without consumers is insufficient.
- The section 9.1 optimization decisions are resolved against Tidepool's
  costs, including measured reasons for techniques that were declined.
- Heap allocation, external payloads, descriptors, globals, and code have
  explicit, observable lifetimes.
- Full collection reclaims retired old-space data and external allocations;
  live code keeps its global data and root slots valid.
- Expected failures stop through typed control flow; incomplete GC roots
  cannot reach collection; integrity failure cannot be reported as recovery
  without evidence.
- The deletion ledger is discharged or remaining mechanisms are justified
  by a concrete production consumer.
- Maintained production engine code is smaller, the number of special cases
  is lower, and performance/allocation improvements are measured.
- The complete production source-to-resumed-result route passes on the
  supported platforms, beyond isolated parser/emitter tests.
- Temporary migration machinery is removed and final contracts live with
  their owning source. This plan can then be retired.

The useful conceptual simplification is that GHC establishes a prepared
execution program, Rust parses that program into types it can trust, and the
backend implements a small set of explicit operations. Neither side needs
to guess what a value or expression probably meant.

## 15. Verification performed for this plan

During the original review:

~~~sh
just test-target tidepool-codegen codegen 'test(lazy_let_guard::) | test(joinrec_differential::) | test(blackhole_differential::) | test(closure_compilation::)'
just test-target tidepool-codegen codegen 'test(engine_review::)'
~~~

The first command passed 14 existing focused tests. The second compiled the
changed test target and reproduced all four new failures, including on its
rerun. The engine was unchanged.

During the second review:

- Re-read the relevant production boundaries and pinned GHC implementation.
- Compiled and ran a temporary native GHC -O2 oracle for the four expressions;
  all returned the expected answers.
- Generated and inspected CorePrep and final-STG dumps for that fixture.
- Checked the plan's local links and whitespace before handoff.

During the STG optimization design refinement:

- Rechecked GHC's tag consumers, call selection, lifting cost model, Core
  pass selection, and static-reference-table purpose against pinned source.
- Inspected the repository's Cranelift 0.129.1 source for tail conventions,
  result limits, AArch64 StructReturn constraints, and safepoint analysis.
- Rechecked inline allocation, forwarding/scan extents, old-space ownership,
  and the production scope-retirement path; confirmed the absent major pass.
- Updated only this plan and its index description, and checked document
  links, whitespace, and the final diff. No engine tests were rerun for these
  documentation-only refinements.

The STG adapter, new Rust schema, runtime changes, performance claims, and
cross-platform implementation gates described above remain future work.
This planning pass does not claim those have been implemented or tested.
