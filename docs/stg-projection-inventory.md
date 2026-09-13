# Prepared-STG projection inventory

This is the execution handoff after the pinned GHC 9.12.2 `stg2stg` and
unarisation pipeline. The wire format is a finite prepared representation; it
is not rendered STG, a Core compatibility format, or a production-execution
parity claim.

## Wire contract (schema 6, execution ABI 4)

`ProgramEnvelope.schema_version` is 6 and `execution_abi_version` is 4.
Schema 6 adds optional external record-parent identity and typed operation
identity (`PrimOp` versus an intrinsic symbol with a calling convention).
ABI 4 includes the invocation's prepared native-stack limit in VMContext.
The Wave 5 integration fold must regenerate and verify cross-language fixtures;
these version declarations alone are not freshness evidence.
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
unit/module/namespace/occurrence tuple and optional record parent; top-level occurrence spelling is
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

This is the current Wave 5 executable subset. It includes exact defining-module
body recovery, generated lazy entry and settlement, generated PAP/application
dispatch, forcing, invocation-local selective promotion, scoped old-space
admission, and the admitted scalar families. It is still a subset: this
inventory does not claim full lazy, imported, primitive, array, external-edge,
or session-retention execution.

`tidepool-codegen::prepared_program::CompiledProgram` is a closed, pinned
execution path for the Linux x86-64 little-endian 64-bit SysV profile. Its
whole-program admission pass rejects globals/imports, unsupported thunk
signatures, unsupported operations, and application signatures/forms without
an admitted exact/partial/excess classification. Admission
walks nested expression ownership iteratively and reports the owning binding
and arena node for unsupported expressions. `run_entry` also rejects managed
host arguments.

The currently emitted strict subset is:

- constructor/function `Let` allocation, including recursive groups with one
  summed reserve and sibling initialization after the only possible safepoint;
- `Return`, saturated exact direct `Call`, evaluated `Enter`, `Case` in all
  four classifications, `LetJoins`/`Jump`, zero results, and multi-results;
- generated thunk entry/settlement with blackhole, update, final poll, and
  failure-before-publication handling;
- exact, partial, and excess application. PAPs flatten the original callee
  and supplied prefix; logical `Void` arguments advance arity but occupy no
  payload slot;
- forcing of managed roots through the prepared force adapter, with roots
  snapshotted before each force and the heap reader reconstructed afterward;
- invocation-local selective promotion with complete-root sibling fixup in one
  no-mutator interval, exact-start old/static admission, and terminal
  `IncompletePromotion` handling;
- scalar physical arguments/results, including `Int`, `Word`, `Float`,
  `Double`, and the `rintDouble` intrinsic. Logical `Void` positions remain in
  signatures and layouts even when omitted from physical ABI payloads.

The implementation anchors for these claims are
[`entry.rs`](../tidepool-codegen/src/prepared_program/entry.rs),
[`apply.rs`](../tidepool-codegen/src/prepared_program/apply.rs),
[`forcing.rs`](../tidepool-codegen/src/prepared_program/forcing.rs),
[`old_space/prepared.rs`](../tidepool-codegen/src/old_space/prepared.rs),
[`gc/promotion.rs`](../tidepool-heap/src/gc/promotion.rs),
[`gc/raw.rs`](../tidepool-heap/src/gc/raw.rs),
[`floating.rs`](../tidepool-codegen/src/prepared_program/floating.rs), and
[`execution_schema.rs`](../tidepool-repr/src/execution_schema.rs).

This is an executable connected subset, not a producer cutover. Globals/imports,
effects, unimplemented foreign/primitive operations, and managed host arguments remain
outside this closed path. `Atom::Rubbish`
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
execution. The producer/runtime `NoSuccess` local-bottoming contract is not
yet represented in the connected native success path. Array primops and GC tracing of external
boxed-array payload edges are also not implemented; existing array
representations must not be read as evidence that their collection/update
semantics are connected. Imported/global resolution outside the owned
executable subset, effects, foreign calls, unsupported primitive operations,
and full corpus execution remain separate work. No session-retention or
effect-support contract is asserted here. No production cutover or
compatibility promise is implied by this inventory.

## Wave 5 checkpoint — 2026-09-13

At `50beeb099`, the focused checkpoint passed 79 prepared-program tests, 64
heap tests, and the workspace test compilation. The generated fixtures were
then refreshed with the extractor variables unset and the freshness check
passed; only `.source-fingerprint` changed and all 695 generated fixture files
were byte-identical. Freshness is not semantic-green evidence: the latest
Suite report has 109 comparison matches out of 812 tops, 67 missing
expectations, 11 projection failures, 501 admission failures, 124 execution
failures, and zero comparison mismatches. The optimized divergent
`thunk_blackhole` row still hit the 120-second watchdog. Test anchors are
[`settlement_tests.rs`](../tidepool-codegen/src/prepared_program/settlement_tests.rs),
[`apply_tests.rs`](../tidepool-codegen/src/prepared_program/apply_tests.rs),
[`entry_tests.rs`](../tidepool-codegen/src/prepared_program/entry_tests.rs),
[`retention_tests.rs`](../tidepool-codegen/src/prepared_program/retention_tests.rs),
and the heap GC tests under
[`tidepool-heap/src/gc`](../tidepool-heap/src/gc).

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

## Identity and corpus follow-up — 2026-09-13

Reachability now follows GHC binder `Unique` values throughout the prepared
inventory walk. Only the complete-module `VarEnv` converts that identity to a
stable wire symbol. A projected home top that is absent from that map is a
typed projection failure; an unknown internal name cannot be turned into an
import declaration. Genuine external package imports remain valid.

Corpus manifest version 2 records every actual prepared top in the selected
module before any requested-target filtering. Program names are the complete
`unit:module:namespace:occurrence` identity. Only a projected external
(`value` namespace) top receives an exact occurrence `expectation_key`; local
tops and rejected rows carry no key. The supplied historical target list is a
separate ledger whose entries map to an exact external identity or `null`.
Suffix stripping and guessed aliases are not mapping rules; duplicate or
ambiguous exact external matches reject the producer. All-tops compilation or
identity-enumeration failure aborts instead of writing a successful empty
corpus. Consumer validation likewise rejects suffix-alias oracle keys.

The current Suite run contains 812 actual tops: 801 projected and validated,
with 11 projection rejections. Historical coverage is a separate denominator:
255 of 347 legacy names mapped and 92 remained unmapped. The comparator has
109 matches and 0 failures among reached rows, but 67 rows have missing
expectations, so this is not full corpus coverage. The original closed-global
cohort has 0 of 516 matches; this is not a semantic parity claim. Runtime
admission, execution, and workspace freshness limits remain.
