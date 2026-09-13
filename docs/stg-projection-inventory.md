# Prepared-STG projection inventory

This is the execution handoff after the pinned GHC 9.12.2 `stg2stg` and
unarisation pipeline. The wire format is a finite prepared representation; it
is not rendered STG, a Core compatibility format, or a production-execution
parity claim.

## Wire contract (schema 5, execution ABI 3)

`ProgramEnvelope.schema_version` is 5 and `execution_abi_version` is 3.
`RuntimeRep` is the physical representation boundary: `Void`, lifted/unlifted
references, addresses, fixed-width `Int`/`Word`, and 32/64-bit floats. Layouts
are canonical for the target pointer width, alignment, payload size, and root
mask; validation recomputes and compares them.

Global declarations carry six wire fields. The appended `dead_end` bit is
evidence that a callable has no normal result after saturation, not a language
error or a synonym for an ordinary zero-result signature. A dead-end global
must carry an entry signature whose results are empty. Linking requires the
imported value to agree on entry signature and `dead_end`; ordinary zero-result
entries remain distinct. See
[`codec.rs`](../tidepool-repr/src/execution_schema/codec.rs),
[`validation.rs`](../tidepool-repr/src/execution_schema/validation.rs), and
[`link.rs`](../tidepool-repr/src/execution_schema/link.rs).

Constructor declarations carry an appended `host_id` (`DataConId`). It is the
stable internal constructor identity used by observation and is distinct from
the family-relative runtime tag. Validation rejects duplicate host IDs across
distinct declarations while allowing the same tag in different families.
The Haskell producer obtains this identity from
`varId (dataConWorkId con)`; it does not mint one from encounter order.

Character literals have no wire `Char` scalar. Projection maps `LitChar` to a
target-width `Word` literal using canonical big-endian bytes. Scalar tag 3 is
retired and stale tag-3 scalar decoding is rejected; it is not reassigned.
The remaining scalar forms are fixed-width `Int`, `Word`, `Float`, `Double`,
bytes/address literals, and typed `Rubbish` atoms. `NullAddress` and `Rubbish`
are preserved as explicit forms, but preservation does not imply native
execution support.

Wire identities are internal deterministic handles. `ValueId` and `JoinId`
are allocated by monotonic projection traversal and lexical environments are
restored when scopes close. Signature, constructor, operation, and global IDs
are interned deterministically by the producer. Symbol identities retain the
unit/module/namespace/occurrence tuple; top-level occurrence spelling is
reserved before target filtering and collision resolution is deterministic.
Constructor host IDs retain GHC identity, rather than family tags or local
table positions.

## Projected and validated forms

Projection preserves post-unarisation groups (`NonRecursive` and `Recursive`),
closure captures, function signatures and parameter representations, joins,
constructors, ordered cases, literal patterns, explicit `Void` positions, and
the flat postorder expression arena. Constructor declarations carry field
representations, strictness, canonical storage layout, family identity/tag,
and host identity. Unboxed tuple/multi-value results are represented by
multiple result components and `MultiValue` cases; no pre-unarisation sum
layout is reconstructed.

The validator checks bounds, ownership, scopes, unique wire binders, canonical
layouts, constructor family/tag evidence, host-ID uniqueness, callable
saturation, and dead-end evidence before a program can be linked. Link-time
imports must match the declared representation, entry signature, and
evaluatedness/dead-end evidence. These checks establish a valid artifact; they
do not promise that every validated form is executable by the native connected
compiler.

## Connected native execution boundary

`tidepool-codegen::prepared_program::CompiledProgram` is a closed, pinned
execution path for the Linux x86-64 little-endian 64-bit SysV profile. Its
whole-program admission pass rejects globals/imports, thunk RHSs, operations,
indirect/partial calls, and calls whose local callee signature is not exactly
the declared call signature. Admission walks nested expression ownership
iteratively and reports the owning binding and arena node for unsupported
expressions. `run_entry` also rejects managed host arguments.

The currently emitted strict subset is:

- constructor/function `Let` allocation, including recursive groups with one
  summed reserve and sibling initialization after the only possible safepoint;
- `Return`, saturated exact direct `Call`, evaluated `Enter`, `Case` in all
  four classifications, `LetJoins`/`Jump`, zero results, and multi-results;
- scalar physical arguments/results and managed references through the internal
  multi-result ABI, with status checked before payload publication.

This is an executable connected subset, not a producer cutover. Thunks,
globals/imports, effects, foreign/primitive operations, partial or indirect
calls, and managed host arguments remain outside this closed path. `Atom::Rubbish`
is represented by the schema but native `atom_value` demand currently reports
`Unsupported`; `NullAddress` has only the explicit `Address` lowering and is
still rejected by observation, which does not materialize addresses. No
placeholder value is fabricated. Legacy
Core CBOR and the reference evaluator are not fallback inputs to this path.

## Static image ownership

`prepared_program::image` owns compile-time immutable top construction. It
reserves every top object, initializes headers and payloads, records managed
relocations, and publishes a fallible `StaticImage` only after all validation
passes. `run_entry` instantiates that image per invocation and builds the
private prepared-top table; compiled code retains no invocation pointer.

The machine/collector owns nursery and static-region admission. Static exact
starts and descriptor/tag evidence are checked, static fields are not scanned
as mutable nursery storage, and nursery-to-static managed edges remain valid
through collection. Cyclic static relocations are valid when their declared
top objects exist; escaping or missing managed relocations remain image
construction errors.

## Non-forcing observation ownership

`prepared_program::observe::ObservationHeap` owns non-forcing materialization
of the rooted result vector. It proves nursery/static object membership and
descriptor state before reads, maps descriptor headers to constructor host IDs
and logical field representations, preserves source field order, and uses an
iterative recursion worklist. A single budget is shared across every result
and every expanded constructor/scalar occurrence, including duplicate DAG
occurrences. Budget exhaustion, functions/PAPs, addresses, bad tags, and
descriptor failures are typed errors; partially built `Value` trees are
dropped stack-safely. Observation performs no forcing, native call, or GC.

Focused tests cover deep small-stack chains, cycles, source-order child errors,
distinct host IDs sharing family-relative tags, static/nursery edges, and
zero/multiple results. They are contract evidence, not a claim that the
workspace or producer corpus is green.

## Explicit boundary gaps

The producer still rejects unsupported literal shapes such as `BigNat` and
relocatable labels, and rejects primitive/foreign calls without a wire/native
contract. Validated projection can therefore be broader than connected native
execution. Thunk forcing, imported/global resolution into owned executable
handles, effects, old-space/external payload integration, and full corpus
execution remain separate work. No generated fixture regeneration, production
cutover, or compatibility promise is implied by this inventory.

## Reviewed Wave 4 contracts — 2026-09-12

Prepared `Enter` uses the shared `prepared_enter_slow` provenance-checked
inspection path; it does not grow a second inline header-chain path. Top-level
internal identities use the `local` namespace, external exact names use
`value`, and suffix reservation remains namespace-local before target
filtering. Retained-generation matching is external-name-only, so an internal
same-spelled identity cannot capture an import generation.

Focused evidence is repr 2/2, runtime 6/6, comparator 7/7, and runner 6/6.
The prepared engine is 26/27: the remaining
`nested_function_rejection_reports_the_nested_expression_owner` fixture fails
with `InvalidScope("value ValueId(1) is out of scope")`. These counts are
boundary evidence only; they do not claim a green corpus or workspace gate.
