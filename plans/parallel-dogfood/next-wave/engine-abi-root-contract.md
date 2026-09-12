# M4/M5 contract proposal (review base 98f99b0)

This amendment specifies the shared scaffold, not completed M4/M5 execution.
The engine lead accepts the contract, owns its initial scaffold and public
cutover; M4 integrates emitters; M5 integrates heap/layout/collection. M2 retains
MachineState, complete roots, failure disposition and PersistentSession policy.
No LinkedProgram-to-CoreExpr adapter and no second root registry.

## Evidence and corrected premises

- Supplied evidence: `prepared_worker_bytes_link_and_execute` passed 1/1 on
  `98f99b0ef4304e44bc60deca61f33385c2e500ff`. This establishes the M3 reference
  vertical, not native ABI safety.
- `execution_schema::{Signature,CheckedLayout,PreparedProgram,LinkedProgram}`
  already carry semantic signatures and constructor layouts. Reuse them.
  `tidepool-heap/src/layout.rs` owns existing physical offsets; codegen derives
  them. Extend that owner, not a parallel numeric ABI in codegen.
- **Superseded assumption: LinkedProgram is already representation-safe input
  to native emission.** Validation uses sets of in-scope ValueIds, not typed
  environments. Return/Call check atoms, Operation/Jump check counts, but they
  do not establish all argument/result representations. A public parse+link
  probe declares a function result LiftedRef and returns Int64(42): accepted.
  Thus trusting its declared result as a GC pointer can manufacture a root.
- `engine-abi-review-probe.patch` is a reproducible temporary test overlay on
  the exact review base, not a desired acceptance test. Command:
  `bash scripts/dev-shell.sh cargo test -p tidepool-repr --test execution_schema_codec abi_review_probe_accepts_wrong_result_rep -- --exact --nocapture`.
  Executed: 1 passed, 6 filtered; successful acceptance is the observed defect.
  Overlay removed after execution. No production source changed by this review.
- `validation::check_layout` checks field order, bounds and root classes but
  not natural field alignment or alignment sufficient for pointer slots.
  This is source evidence, not an executed malformed-layout probe.
- `ImportedValue.signature` is a program-local SignatureId. `link_program`
  compares numeric IDs; it has neither another signature table nor actual
  machine value custody. Equal IDs across modules are not shape equality.
  The reference consumer separately supplies values. Native imports must use
  the actual binding owner; linking metadata alone is not live-value linking.
- Existing `MachineState::complete_root_snapshot` joins stack, Rust scoped,
  persistent, stowed, code and VM tail slots, separating remembered edges.
  Stack roots come from the checked frame walk. This capability already exists;
  the new emitter must supply precise slots, not replace root policy.

## Shared scaffold before dependent implementation forks

The signatures below specify the intended Rust interfaces; they are not claims
that these new names are implemented. Private constructors and immutable accessors
are required. Build the scaffold with actual M4 and M5 consumer imports before
forking. Do not introduce a second recursive execution language.

1. **repr owns semantic construction and typed evidence.** Strengthen the existing
   parse/validation boundary. Preserve signatures' semantic positions (including
   Void), and retain per-binding/per-occurrence rep evidence in PreparedProgram.
   Resolve captures in their existing declared order; do not recompute free vars.
   Functions/joins obtain parameter reps from signatures; constructor alternatives
   from field declarations; cases from checked result vectors; globals from
   checked import contracts. Validate body returns and known calls, operation
   operands, joins and construction against this evidence. Dynamic application
   needs checked argument reps and expected result reps; add missing projection
   facts where inference is ambiguous rather than guess LiftedRef. Raw captures
   require the same typed environment. An expression/site identity, if needed,
   is assigned once here, not separately by heap and codegen.

   Keep `parse_program(bytes, requirements, limits) -> Result<PreparedProgram,
   ParseError>` and `link_program(prepared, imports) -> Result<LinkedProgram,
   LinkError>` as the public invariant-bearing constructors. The lane lead/M3
   owner repairs these joins; M4 must not add a compensating private type checker.
   Compare imported semantic Signature shapes (or owner-qualified interned
   shapes), not local SignatureIds. Identity/generation/evaluatedness remain
   mandatory. Production dispatch resolves all imported rooted slots atomically
   from the existing binding owner and retains that owner before entry.

2. **repr owns one representation-to-storage calculation.** Proposed:
   `StorageLayout::for_reps(target: &TargetDescriptor, reps: &[RuntimeRep])
   -> Result<StorageLayout, LayoutError>`.
   Fields are private; accessors expose logical-to-stored indices, field reps,
   byte offsets, payload size/alignment, and managed-reference offsets.
   Void occupies a semantic position but no bytes/root slot. Preserve field
   order. Size/alignment: scalar bits/8, references and Address pointer_width/8;
   align each stored field naturally; round payload size to maximum alignment,
   checked arithmetic throughout. Root offsets select only LiftedRef/UnliftedRef;
   Address is never a managed root. Constructor CheckedLayout must agree with
   this constructor (including padding and Void omission), not merely bounds.
   Haskell emits these same layout facts; Rust is the acceptance owner.
   No consumer builds a second bitmap or alignment formula.

3. **codegen owns one native ABI lowering.** Proposed:
   `EntryAbi::lower(profile: &NativeAbiProfile, signature: &Signature,
   environment: EnvironmentMode) -> Result<EntryAbi, AbiError>`.
   This consumes checked semantic signatures and StorageLayout; it alone emits
   Cranelift signatures for definitions, calls, generic apply and Rust adapters.
   Keep semantic arity separate from physical components; Void contributes to
   saturation but emits none. Integer widths remain raw (128-bit integers lower
   to a documented low/high i64 pair where necessary); floats stay f32/f64;
   references and addresses are pointer-width values with distinct root classes.
   No boxing to satisfy an old unary pointer ABI.

   Generated entries use Tail. Parameter order is vmctx, optional environment,
   optional ordinary result-area pointer, then physical argument components.
   Environment presence is fixed by the entry, not inferred again at call sites.
   Return status is an explicit integer ABI discriminant; map it to M2's existing
   first-cause/disposition at host boundaries. Define success versus failure once;
   never read payload results on failure. Rust boundaries use generated platform
   C adapters, never call a Tail address as an extern-C function.

   ResultTransport is `Registers` or `CallerArea(StorageLayout)`, selected once
   per native signature/profile. Register selection includes the status register
   and every physical component; exceeding the verified profile selects the area
   for the entire result vector. Do not use StructReturn. Freeze exact register
   budgets only with the M4 profile probe; do not let sibling emitters choose
   their own thresholds. Profile construction rejects a target without that
   explicit selection. The scaffold may be compiled before that native proof;
   generated execution cannot be accepted before it.

   A caller area is non-Haskell stack/run storage. Compute rooted results first;
   write output slots in a no-GC success epilogue; caller checks status then loads
   typed values before the next safepoint. Forward only an ancestor/caller-owned
   area through tail calls, never one owned by the eliminated frame. If populated
   results survive a safepoint, register initialized reference slots with RootScope.

4. **heap owns physical descriptors and header encoding.** Proposed:
   `ObjectDescriptor::new(kind: ObjectKind, payload: StorageLayout,
   entry: Option<EntryMetadata>) -> Result<ObjectDescriptor, LayoutError>`.
   EntryMetadata references an owned semantic/native signature and entry addresses;
   heap never constructs Cranelift signatures. Fixed-layout descriptor contains
   immutable payload base, allocation alignment/extent, state-specific trace
   offsets and kind metadata. Allocation, bridge, emitter and collector consume
   that exact descriptor. Descriptor addresses stay stable under compiled-module/
   machine custody; surviving function/PAP/continuation objects retain their code,
   descriptors and code-to-global roots through the existing owner.

   Retain design §7.1: one descriptor/state header word; managed pointers untagged;
   at least two words total for forwarding; memoizing thunks reserve a result/cause
   slot. HeaderWord and masks live only in heap. With 16-byte scalar payload
   alignment, payload base must be align_up(header_size, payload_alignment), not
   unconditionally one word. Total extent rounds to object alignment. Header
   state transitions preserve original extent, including forwarding. Collectors
   never derive object length from its current reduced trace shape. M5 establishes
   exact masks/alignment with state/forwarding probes before M4 emits constants.

   PAP is flat: underlying function, semantic prefix count and stored prefix
   components from that function's signature. Void positions count but do not
   allocate. Extending combines prefixes without mutating shared PAPs/chaining.
   Root pending oversaturation arguments across entry/forcing/allocation.

## Root contract: use M2, do not wait for all M2 acceptance

Every live managed reference at a collecting call is in a mutable, stable slot:
JIT stack-map slot, scoped Rust root, or an existing persistent/parked/code owner.
Frame walker success is required before `complete_root_snapshot`; never substitute
an empty or partial walk on failure. A collecting helper has an authoritative
MayCollect classification; its caller spills all live references before the call
and reloads moved values afterward. Raw scalars/addresses/code/descriptor pointers
are not traced as heap pointers. An interior/foreign address into managed storage
requires an explicit pinned/external owner or managed base+offset protocol, not
classifying Address as a root. Scratch must not move while its slots are registered.

M4 owns liveness and stack maps, safepoint placement and reloads. M5 owns descriptor
trace interpretation and barriers. M2 owns assembling root categories and cleanup.
Use existing RootScope for initialized reference scratch slots; no generated-root
registry, no per-call partial root installs. Ordinary heap pointer stores use the
existing store/barrier owner; emitted/bulk/atomic writes call write_barrier. Omit
barriers only for proven nursery initialization with no intervening collection.
Remembered edges are minor-GC inputs, not independent major liveness roots.

Expected failure/cancellation follows status and existing cleanup; integrity failure
uses M2 poison/disposition. Do not rely on Rust RAII across siglongjmp. Park only
heap continuations through existing ContinuationId custody, not native stack frames.

## Decisive joins and limits

- First invert the saved malformed-result probe; reject wrong operand/capture/join
  reps and misaligned managed fields at parse. Check actual prepared Void/raw
  capture/case output; any wire/projection changes require `just fixtures-check`.
  Import test: equal local IDs/different shapes rejected; differing IDs/equal
  shapes resolve correctly with machine/generation custody.
- ABI probe uses the pinned 0.129.1 Cranelift crate (not plan links to v42): compile
  both architectures; execute x86_64 C adapter -> Tail entries -> adapter for
  mixed reference/int/float results, register-budget overflow to area, narrow and
  128-bit values, and changing stack-argument sizes under bounded tail recursion.
  Local pinned aarch64 ABI source explicitly rejects Tail+StructReturn. Source
  inspection is not native execution proof. No native aarch64 runner available.
- Shared awkward case: `[Void, Word8, LiftedRef, Float64, UnliftedRef, Address]`
  on a 64-bit profile has stored payload offsets `[0,8,16,24,32]`, size 40,
  alignment 8, root offsets `[8,24]`. Capture it in a PAP, extend through the Void
  position, force collection while oversaturated arguments remain pending, then
  consume mixed raw/ref results. Numeric pointer-looking scalar/address bits must
  never be traced; moved references must reload correctly. This is a required
  future execution test, not a proof produced by this review.
- M4/M5 integration: real PreparedModule bytes -> linked program -> direct native
  backend -> tiny-nursery moving collection -> materialized/resumed result. Exercise
  updated large thunk extent, error/cancel before result initialization, missing
  frame map refusing collection, old-to-young writes, retirement/full collection
  and escaped closure/code-root survival. Existing reference vertical is not this
  evidence. The lead owns public dispatch, cross-lane session seam and final joins.

Independent descriptor and ABI scaffolding need not wait for unrelated M2 tests.
Unsafe native execution must wait for typed construction, verified profile and the
complete-root consumer join. No generated execution, compact-header implementation,
aarch64 native execution or final production acceptance is claimed here.
