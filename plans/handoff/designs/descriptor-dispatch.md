# Descriptor dispatch: calls and enter through the object's info table

Refines the "if header chains dominate…" step of the STG completion sequence.
The G1 application semantics are fixed and must not regress: full-signature
resolution, non-failing probes, exact PAP completion, terminal saturation,
excess application, logical `Void`, and `NoSuccess`. This note changes how a
dynamic call or force finds its code, not what it does.

This fixes the program/runtime call boundary. The descriptor dispatch row
(decision 1) and machine-wide signature ids (decision 2) are one-way doors:
once generated code of several installed programs reads the row layout and
compares ids, a change requires rebuilding every artifact that one machine
can hold. Everything else here is reversible.

## Problem

- **Call dispatchers.** `apply::declare_dispatchers` (apply.rs:198) declares
  one dispatcher per demanded signature, closed over owner offers, prefixes,
  suffixes and terminal prefixes. `emit_dispatchers` (apply.rs:327) emits, per
  dispatcher, a header-compare chain over every function (apply.rs:370) and
  every PAP layout (apply.rs:468) that `classify` admits, then a list of host
  probes (apply.rs:597). Blocks are proportional to offers. Measured on suite
  artifact 502 (297 KB): 235 dispatchers, 1.30 MB code, about 400 ms of a
  900 ms compile (test build), 6,522 offers. Artifact 187: 209 dispatchers,
  0.95 MB, about 305 of 770 ms. At run time a call walks the chain; a foreign
  callee walks it all and then makes one host probe per candidate shape.
- **`prepared_enter`.** `entry::emit_prepared_enter` compares the header
  against every evaluated descriptor (entry.rs:133: constructors, functions,
  PAP layouts) and then every thunk (entry.rs:146, about 12 blocks each)
  before the host fallback (entry.rs:297). Each force of an untagged object
  costs O(program size).

## Source facts

- The header word is the pinned descriptor's address:
  `ObjectDescriptor::initial_header_word` (tidepool-heap
  execution_descriptor.rs:309). Thunks keep `DescriptorState` in the low 3 bits
  (entry.rs:150, :157). `ObjectKind` (execution_descriptor.rs:29) already
  separates Function/Pap/Thunk/Constructor/Continuation/External.
- `ObjectDescriptor` derives `Clone, Eq, PartialEq` (execution_descriptor.rs:64).
  `EntryMetadata` holds the full `Signature` and a `code_identity` (the
  binding id), not a code address (:41). The type's documentation says never
  to dereference an untrusted header address (:327-328).
- Function and thunk descriptors are minted per compile with `Arc::new`
  (plan.rs:344, :384), and so are PAP layouts, one per `(function, pending)`
  (apply.rs:162, plan.rs:426). Only constructors and the three external
  wrappers are machine-interned (interner.rs:1-11, a22f51ba1). A
  function/PAP/thunk descriptor therefore belongs to exactly one program.
- A caller-result function has one compiled instance per result instance:
  `functions: BTreeMap<ValueId, BTreeMap<ResultContract, FuncId>>`
  (prepared_program.rs:744, `callee_instance` in apply.rs). A function
  descriptor therefore has no single entry address.
- A PAP stores the original function in field 0 and a flattened prefix.
  Completion needs layout-specific field loads (`load_pap_field`), and
  partial application needs the target layout `(f, pending + k)`
  (apply.rs:502-560).
- Code addresses exist only after `pipeline.finalize()`
  (prepared_program.rs:870). Heap tops and statics that carry these headers
  are initialized at install, after compile (machine.rs install, ~780).
- Cross-program resolution: `MachineState::prepared_callables` (header →
  signature → code, machine_state.rs:308) and `prepared_enters` (header →
  owner's `prepared_enter`, :318). Both are registered at install
  (machine.rs:847), removed at retirement (machine.rs:1185 →
  machine_state.rs:1316) and read by `prepared_resolve_call` /
  `prepared_resolve_enter` (prepared_program.rs:209, :253).
  `ENTER_EVALUATED` (:244) is how a foreign constructor is reported as a value.
  `prepared_unresolved_call` (:223) classifies a miss as `UnresolvedCallee`
  or an integrity failure through `owns_prepared_entry`
  (machine_state.rs:1404).
- Retirement runs only after `mark_live_programs` (machine.rs:1063), which
  traces live objects to their header owners. A live object never carries a
  retired program's descriptor.
- Indirect calls already appear in generated code (entry.rs foreign path and
  apply.rs probes), so the pipeline's `user_stack_maps` join covers them.
- 6be949e50 already routes the zero-argument lift through `prepared_enter`.
  `foreign_dispatch_cost_on_freer_artifact` (foreign_apply_tests.rs:785,
  `TIDEPOOL_COST_ARTIFACT`) and the `tidepool::prepared_apply` debug event
  report dispatchers, code bytes, offers and elapsed time.

## Decisions

1. **Dispatch row in the descriptor.** `ObjectDescriptor` gains a
   `dispatch: DispatchRow` whose field offsets generated code reads through
   `std::mem::offset_of!`, with `repr(C)` on the row itself. Contents:
   - `kind: u8` (a copy of `ObjectKind`, for use in a native switch);
   - Function/PAP: `arity: u32` (remaining logical arguments, including
     `Void`), `arguments_id: u32` (the interned remaining argument vector),
     `results_id: u32` (the interned declared result contract, or a
     caller-result marker), `prefix_ids: *const u32` (the id of
     `remaining[..k]` for each k from 0 to arity), `exact: *const [(u32, usize)]`
     (result id → entry: one row, or one per result instance for a
     caller-result function), and `partial: *const usize` (the builder for
     supplying k arguments, for k from 1 to arity-1);
   - Thunk: `enter: usize` (a per-thunk wrapper; see decision 4).
   Constructors, externals and continuations carry only `kind`.
   The row is written once, after `finalize` and before `compile_with`
   returns, through an `UnsafeCell`/atomic publication. This is sound
   because these descriptors are never interned or shared across programs
   (fact 3), and no object with such a header exists until install. The
   row's side arrays are owned by `CompiledProgram`, beside `_dispatchers`,
   and share the program's lifetime. `Clone`/`Eq` are implemented by hand
   and exclude the row. `Clone` of a descriptor with a published row is
   refused (a debug assertion), so a copy can never carry stale code.
   Constructors keep their interned identity, and their row carries no
   code, so sharing them is unaffected.
2. **Machine-wide signature ids.** A `SignatureCatalog` interns argument
   vectors and result contracts separately:
   `HashMap<Box<[RuntimeRep]>, u32>` plus `Vec`. The hash only indexes; the
   stored key is the full vector, so equal ids mean equal signatures. `Void`
   is a distinct `RuntimeRep`, so it is part of identity even when physical
   registers agree. The catalog is `Arc`-shared like `ExternalDescriptors`.
   `compile_with` takes it next to the `DescriptorInterner`, the first
   install adopts it, and a later program compiled against another catalog
   is refused at install (a new `AbsorbConflict::Catalog`). Ids are never
   reused. The catalog grows with the machine's signature vocabulary and is
   reported in `ResidencyCounts`.
3. **Unknown call = fast exact check, then one generic slow path.** For
   demand D, with argument id A and result id R known at compile time, the
   call site emits the sequence below. Today every call, including one whose
   callee `plan.callee` classifies as `Known`, goes through D's dispatcher
   (`emit::emit_exact_call`); lowering a `Known` exact call to a direct call
   is new work, a later optimization this design enables but does not
   require. The emitted sequence:
   enter the callee → load the header → mask 3 bits → load
   `dispatch.arguments_id` and the first `exact` row → if the id is A and the
   row's result is R, `call_indirect` the entry with the D ABI (the callee
   is the environment, as in `call_arguments`) → otherwise call
   `apply_slow[D]`.
   `apply_slow[D]` is generated once per demanded D. Its blocks depend on
   |D|, not on program size. Given the entered callee's row:
   - kind other than Function/Pap → `prepared_unresolved_call`, which
     classifies the miss (below);
   - `n = arity`. If `n > |D|`, `prefix_ids[|D|] == id(D.args)` and D returns
     `[LiftedRef]` → `call_indirect partial[|D|]` (**Partial**). A zero-length
     D keeps 6be949e50's `prepared_enter` answer;
   - otherwise switch on `n` over `0..=|D|`. Arm `n` requires
     `arguments_id == id(D.args[..n])`, then:
     - declared `NoSuccess` → indirect entry with the first n physical
       arguments, terminal via `emit_call_results` (**NoSuccess**; excess is
       never applied);
     - `n == |D|` → search `exact` for R, or for a caller-result function
       with a `Returns` demand, take the instance row for R (**Exact**);
     - `n < |D|`, the declared result is `[LiftedRef]` and D is not
       `NoSuccess` → indirect entry with the first n arguments, propagate
       failure, then the unknown-call sequence for `D[n..]` on the result,
       with pending arguments kept as rooted SSA (**Excess**);
   - no arm matches → `prepared_unresolved_call`.
   The arms are `classify` (apply.rs:110) rewritten over ids. The
   rewrite keeps `classify` as the one semantic source: a unit test checks,
   for every (entry, pending, demand) in the existing tables, that
   `classify` and the id predicate agree. Lookup records no failure. Only
   `prepared_unresolved_call` records one, once. It classifies with the live
   descriptor space (a live descriptor means `UnresolvedCallee`; anything
   else is `BadThunkState`) instead of `owns_prepared_entry`.
   The demand closure (suffixes `D[n..]`) is still computed, but only over D
   itself. Owner-side offer enumeration (apply.rs:241-275) goes away.
   **Where the slow path lives (changed from the brief).** Slices 2-3 emit
   `apply_slow[D]` per program. `CompiledProgram::compile` has no machine,
   and machine-owned code would need its own pipeline, stack-map
   registration and retirement rule. Per-program generic stubs already
   remove the Θ(offers) term. Hoisting them into a catalog-owned module is
   slice 5 and optional, justified only by measurement.
4. **Owner-emitted adapters, reached through the row.** Each program
   emits:
   - for a PAP layout × result instance, one completion adapter. It uses the
     remaining-signature ABI with the PAP as environment, loads the prefix
     fields, and makes a direct call to the function instance. This is today's
     PAP Exact arm, moved out of the chain;
   - for a function or PAP layout with supplied k, one partial builder. It
     reserves, copies the prefix, stores the k arguments and tags the
     result. This is `emit_partial` against layout `(f, pending + k)`;
   - for a thunk, one enter wrapper. It sets the Evaluating header, calls the
     body directly, handles `NoSuccess`/`UnexpectedSuccess`, forces the
     result through `prepared_enter`, and commits with a static policy. This
     is today's per-thunk arm from entry.rs:146-280, unchanged in content.
   The counts are the same as the current per-layout offers. Their size
   depends on arity alone, and no program-wide chain is emitted.
5. **`prepared_enter` becomes O(1) and program-independent.** Tagged
   reference → return. Null → integrity failure. Otherwise load the header
   and branch on the state bits: Updated follows the forwarding pointer and
   loops; Evaluating reaches `blackhole`; Live continues. Then switch on
   `dispatch.kind`: Thunk → `call_indirect enter`; Function, Pap or
   Constructor → return the reference. External and Continuation are
   integrity failures, as today, where they reach `prepared_resolve_enter`
   and miss. Keep the entry poll and preflight. Any program's copy is
   correct for any object, so the foreign branch and `ENTER_EVALUATED`
   disappear.
6. **Trust.** Generated code already treats the header of a managed
   reference as trusted (entry.rs loads it). The new step is dereferencing
   it. Every header in the managed heap and static regions is written by
   the allocator or host marshalling from a pinned descriptor. Liveness
   (fact 7) keeps that descriptor alive. The heap comment at :327 is
   narrowed to *host* code inspecting arbitrary words. Debug builds add a
   host check that a read header is in the live descriptor space, called
   once per slow path and enter.
7. **GC and ABI.** An indirect call is a safepoint exactly like a direct
   call. The entered callee, the pending arguments and any intermediate
   excess result are declared with `declare_value_needs_stack_map` before the
   call, as `emit_dispatch_entry` does now. No safepoint may sit between the
   header load and the call. The loaded row fields are raw words, not
   references, and are not rooted. The row is immutable after publication,
   so a collection between load and call cannot invalidate it. Equal ids
   imply equal logical signatures and therefore an equal
   `EntryAbi::lower_internal` result, so `import_signature` for D is correct
   for every row that passes the check. Cost: one or two memory loads, one
   compare and an indirect branch, against today's chain walk. Known callees
   stay direct.

## Migration slices

Each slice lands and is tested alone. Behavior stays constant across them.

1. **Enter via kind (local).** Add `DispatchRow` with `kind` and Thunk
   `enter`, including publication after finalize. Emit the thunk enter
   wrappers and rewrite `emit_prepared_enter` as decision 5. Delete the
   `evaluated` list (prepared_program.rs:811), `prepared_resolve_enter`,
   `ENTER_EVALUATED`, `prepared_enters`, `enter_owned_headers` and the
   enter half of `register/retire_prepared_entries`. Replace
   `owns_prepared_entry` with a live-descriptor-space query. Tests:
   `entry_tests` (all, including `shared_constructor_rows` and
   `prepared_program_retiring_a_declarer_keeps_shared_constructor_entry`),
   `no_success_tests`, `lifetime_tests`, and `foreign_apply_tests` (imported
   thunk returning a closure). Measure `prepared_enter` code size.
2. **Catalog and row data.** Add `SignatureCatalog`, its install
   adoption/conflict, and the function/PAP row fields, completion adapters
   and partial builders. Nothing reads them yet except a test that checks,
   for every descriptor in every fixture program, that the row agrees with
   `EntryMetadata` and `classify`.
3. **Call sites use the row.** Emit the fast check plus `apply_slow[D]`.
   Keep `prepared_callables` only as a debug cross-check that the row's
   answer equals the old offer. Tests: the full G1 list below.
4. **Delete.** Remove chain emission, the offer bookkeeping, `CallableExport`
   (resolve.rs), `CompiledProgram::callables`, `prepared_callables`,
   `resolve_prepared_call`, `prepared_resolve_call`, the probe list, and the
   `callable_headers` retirement rows. `ResidencyCounts` loses the
   callable/enter rows and gains catalog entries.
5. **(Optional) Hoist `apply_slow[D]` machine-wide.** Only if slice 4's
   measurement shows the stubs are material.

## What is deleted

Header-compare chains in `emit_dispatchers` and `emit_prepared_enter`; the
owner offer enumeration in `declare_dispatchers`; `resolve.rs`;
`MachineState::{prepared_callables, prepared_enters, register_/retire_/
clear_prepared_entries, resolve_prepared_call, resolve_prepared_enter,
owns_prepared_entry}`; `prepared_resolve_call`, `prepared_resolve_enter`,
`ENTER_EVALUATED`; and `CompiledProgram::{callables, enter_owned_headers}`.
`prepared_unresolved_call` and `prepared_recorded_failure` stay.

## Risks

- **One-way layout.** The row layout and id semantics become ABI across
  programs on one machine. Version the row with `execution_abi_version`.
- **Weaker integrity detection.** A corrupt header used to fail a compare
  chain cleanly; now it is dereferenced. Mitigation: decision 6's debug
  check, plus the existing poisoning diagnostics (machine.rs ~6088).
- **Caller-result instance search.** A demand not in the first `exact` row
  always takes the slow path. If effect code is dominated by caller-result
  calls, put the most-demanded instance first or index `exact` by a
  per-machine result-instance slot. Measure before choosing.
- **Arity-cubic adapters.** Partial builders per `(layout, k)` are O(n²)
  per function with O(n) bodies, the same as today's per-layout offers.
  They can be emitted lazily if a high-arity artifact shows up.
- **Catalog growth.** Ids are never freed. The vocabulary is bounded by the
  distinct signatures a machine has seen. Report it, and do not refcount
  unless residency tests show growth across repeated identical turns.
- **Write-once publication.** A descriptor cloned or interned in the future
  would break the "one program" premise. Enforce it with the refused-clone
  assertion and a plan-time check that only Function/Pap/Thunk rows get code.

## Acceptance

- The G1 list from stg-completion step 1 passes unchanged: foreign exact PAP
  completion to lifted, scalar and multiple results; partial-then-complete;
  partial application of an existing PAP; over-application through another
  owner's returned closure; logical `Void`; `NoSuccess`; mismatched
  signatures; an imported thunk returning a captured closure; collection
  during foreign execution with caller roots and the intermediate excess
  result live; a successful prefix probe leaving no recorded failure; and an
  invalid call leaving a sound, reusable machine. Concretely:
  `foreign_apply_tests` (all non-ignored), `apply_tests`,
  `apply::tests` (`classify` plus the new id-agreement test), `entry_tests`,
  `no_success_tests`, `caller_result_tests`, `lifetime_tests`,
  `retention_tests`, `tidepool/codegen/tests/apply_acceptance.rs`,
  `apply_cont_heap_composition_gc.rs`, the runtime `prepared_execution` and
  `prepared_resident_composite` suites, `placement_retirement`, and the
  repeated-install residency test (`repeated_installs_retire_and_keep_residency_flat`).
- Prepared corpus (`scripts/prepared-corpus.sh` via `just fixtures-check`):
  execution and comparison counts are at or above the recorded floors.
- Measured before and after on artifacts 502 and 187 with
  `foreign_dispatch_cost_on_freer_artifact` and
  `TIDEPOOL_COST_ARTIFACT=<artifact>`, recording code bytes for dispatchers,
  stubs and adapters, the offer or adapter count, and total compile time.
  Test builds keep Cranelift IR copies, so measure both revisions in the
  same build profile with the same command, and record which profile was
  used. Expected result: dispatch code bytes independent of function count,
  and `prepared_enter` size constant.
- A run-time microbenchmark (forcing a thunk list; applying an unknown
  closure) shows no regression over the chain for a one-function program.
