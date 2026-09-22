# Prepared-STG projection inventory

This is the execution handoff after the pinned GHC 9.12.2 `stg2stg` and
unarisation pipeline. The wire format is a finite prepared representation; it
is not rendered STG, a Core compatibility format, or a production-execution
parity claim.

## Wire contract (schema 10, execution ABI 5)

`ProgramEnvelope.schema_version` is 10 and `execution_abi_version` is 5.
The artifact carries a site table: each `SiteRow` names its site id, its
`SiteDelivery` mode, and the structural `TypeNode` of the wire value a host
answer must build, so a parked continuation's answer is validated against the
answer type itself rather than nominal heads. The constructor closure of
every host-answer type is declared, so an answer constructor the program
never matches still has a descriptor.

`Capability` and `WiredInError` operation identities are part of the contract. Capabilities
retain GHC's `Returns` contract but fail with a typed reusable error when
executed; only exact catalogued identity/signature pairs are admitted. Wired-in
errors carry their kind and an authoritative `NoSuccess` contract. The producer
recognizes GHC builtin keys, not user spelling, and synthesizes ordinary callable
tops so bare references and partial applications retain their meaning.

`ResultContract` values (`Returns` versus authoritative `NoSuccess`) are
carried on case scrutinees. An external record-parent identity is optional,
and an operation identity is typed (`PrimOp` versus an intrinsic symbol with
a calling convention). ABI 5 carries
the explicit terminal-result distinction through prepared calls while keeping
the physical `NoSuccess` shape status-only. Lowering keeps the semantic result
contract beside the physical register/area layout: status-only does not mean a
successful zero-result value. See
[`entry_abi.rs`](../tidepool/codegen/src/entry_abi.rs).
Cross-language fixture freshness is a separate check; these version
declarations alone are not freshness evidence.
`RuntimeRep` is the physical representation boundary: `Void`, lifted/unlifted
references, addresses, fixed-width `Int`/`Word`, and 32/64-bit floats. Layouts
are canonical for the target pointer width, alignment, payload size, and root
mask; validation recomputes and compares them.

Global declarations carry five wire fields. Nonreturning behavior belongs to
the callable's `ResultContract::NoSuccess`, not to a global-side boolean; it
is never interchangeable with `ResultContract::Returns([])`, which is a
successful zero-result call. Linking compares the typed entry contract. See
[`codec.rs`](../tidepool/repr/src/execution_schema/codec.rs),
[`validation.rs`](../tidepool/repr/src/execution_schema/validation.rs), and
[`link.rs`](../tidepool/repr/src/execution_schema/link.rs).

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

GHC's `tagToEnum#` is not carried as an unimplemented operation. The producer
requires an enumeration result type and machine-`Int#` argument, then lowers
the complete GHC constructor family to a primitive `Int` case with zero-based
literal alternatives and ordinary nullary `Construct` results. It invents no
default or family member; the existing case-failure path handles an invalid
tag. The focused fixture checks three-way `Colour` and two-way `Bool` families
in [`ExecutionProjectionTest.hs`](../bridge/haskell/test-prepared-stg/ExecutionProjectionTest.hs).

Prepared `Enter` uses the shared `prepared_enter` provenance-checked
inspection path; it does not grow a second inline header-chain path. Top-level
internal identities use the `local` namespace, external exact names use
`value`, and suffix reservation remains namespace-local before target
filtering. Retained-generation matching is external-name-only, so an internal
same-spelled identity cannot capture an import generation.

The validator checks bounds, ownership, scopes, unique wire binders, canonical
layouts, constructor family/tag evidence, host-ID uniqueness, callable
saturation, and result-contract evidence before a program can be linked.
Link-time imports must match the declared representation, entry signature, and
evaluatedness. These checks establish a valid artifact; they do not promise
that every validated form is executable by the native connected compiler.

## Connected native execution boundary

This is the current Wave 5 executable subset. It includes exact defining-module
body recovery, generated lazy entry and settlement, generated PAP/application
dispatch, forcing, invocation-local selective promotion, scoped old-space
admission, authenticated external payloads, and admitted scalar and array
families. It is still a subset: this inventory does not claim full imported,
primitive, effect, or session-retention execution.

`tidepool-codegen::prepared_program::CompiledProgram` is a pinned execution
path for the Linux x86-64 little-endian 64-bit SysV profile. Its
whole-program admission pass admits a global per declaration when its
representation is `LiftedRef` or `UnliftedRef` and rejects any other global
representation with the declaring `GlobalId`; it rejects unsupported thunk
signatures, unsupported operations, and applications it can refute. A call
site is classified once (`plan.rs::Callee`: a locally declared function or
thunk, an import with its link-proven entry signature, or a dynamic
value), and admission and emission consume the same classification. A
known callee is refuted when no exact/partial/excess split serves the
demanded signature; a dynamic callee, or an import without entry
information, is admitted whenever the demanded signature itself lowers,
because the machine resolves the actual callee at run time. Admission walks
nested expression ownership iteratively and reports the owning binding and
arena node for unsupported expressions. `run_entry` also rejects managed
host arguments.

An admitted global is an executable import: `plan.rs` assigns it a slot in
the importing program's own fixed-address root block, immediately after the
program's own top slots; `ValueRef::Global` lowers to a load of that slot,
whose address the generated code embeds, and
`PreparedMachine::install_program` takes an `ImportBindings` map of one
retained `PreparedHandle` per declared identity. Identity, signature and
generation agreement are `link_program`'s contract; install re-verifies only
the live handle's runtime shape (representation, and weak-head-normal-form
settledness when the declaration requires an evaluated value, where a
function or PAP counts as evaluated exactly as a constructor does) before
publishing the slot from the handle's current pointer and registering the
slot as its own persistent root. A rejected import leaves the candidate's
root block, the machine's roots and every installed program untouched. The import is read by
identity, not copied: the slot resolves to the producing program's own
object on the shared heap.

Constructor descriptors are interned per machine (`DescriptorInterner`,
one descriptor per constructor identity; a conflicting later declaration
is `CompileError::DescriptorShape`): a program compiled through
`PreparedMachine::compile_for_install` shares every earlier program's
constructor descriptors, so its `Case` (including `seq`) and
evaluated-constructor enter recognise cells another program built.
Calling a function or forcing a thunk that another installed program
produced resolves through machine-wide tables,
`MachineState::prepared_callables`/`prepared_enters`, filled at
`PreparedMachine::install` from every installed program's function and
thunk descriptors (`tidepool/codegen/src/prepared_program/machine.rs`).
Generated code reaches this as the terminal fallback of its own
per-program fast chain: `apply.rs::emit_dispatchers` falls through to the
host fn `prepared_resolve_call(vmctx, header, demand)`, and
`entry.rs::emit_prepared_enter` to `prepared_resolve_enter(vmctx,
header)` (`prepared_program.rs`). Demand metadata is boxed and owned by the
calling compiled program. Lookup compares full logical signatures, including
`Void` positions and the result contract; no hash establishes ABI equality.
Application of a foreign function -- whether the callee is the
import reference itself (`ValueRef::Global`, Haskell's ordinary
`producerFn x`) or a local value that happens to hold one (a managed
argument, a case-bound name) -- and forcing a foreign thunk all work this
way. A dispatcher whose demanded shape matches none of the compiling
program's own callables is legal and consists of the fallback alone.
Each owner predeclares exact remaining signatures and partial prefixes for
its function/PAP layouts, including scalar and multiple-result PAP completion.
The owner applies and flattens PAP fields under its own rooting discipline.
On a full-demand miss, the caller probes terminal prefixes, then lifted-result
prefixes for excess application; a successful lifted result is rooted before
suffix application. Lookup misses do not record a failure. Exhausted resolution
reports `UnresolvedCallee`, disposition `Reusable`; a header no installed
program can enter is `BadThunkState`, machine `Unavailable`. Terminal saturation
never applies excess arguments. Focused coverage lives in
`prepared_program/foreign_apply_tests.rs`, alongside the imported-thunk and
signature-mismatch regressions in `prepared_program/machine.rs`.

A top-level constructor whose field is an import is a heap top
(`image.rs::heap_top_partition`), initialised from the published import
slot (`run.rs::write_atoms`); `install` publishes import slots before
initialising heap tops and before the install-time collection, with
rollback on every later failure arm. Default-only algebraic `Case` skips
descriptor matching (`emit.rs::emit_algebraic_dispatch`), so a
`seq`-shaped case works on an import even in a standalone-compiled
program.

Host observation (`inspect_outer`, `run_entry`'s result observation)
resolves imported values, including another program's static cells,
through the machine-wide descriptor/static union. Failure classification
is orthogonal to this resolution: `MachineState` separates the call
outcome (`runtime_error`, the first failure cause of one call, settled
when that call ends) from the machine latch (`last_failure`, the first
`Unavailable`-class cause the machine has seen, never cleared). Reusable
causes -- cancellation, language-level failure, `UnresolvedCallee` among
them -- never latch, so an `inspect_outer` observation made between calls
sees only the latch, not a reusable failure from inside a completed call.
Retained-generation matching is still external-name-only: a retained
symbol whose body GHC inlines into the consumer (small static data with
an exposed unfolding) is consumed as a recovered copy, not through the
import; the producer side must withhold the unfolding, and this
constraint is not yet lifted (S5 is in flight).

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
  `Double`, exact-signature integer/floating families, `double2Int#`,
  `plusAddr#`, `chr#`, `eqChar#`, `clz8#`, `subWordC#` (wrapped `Word64`
  difference and `Int64` unsigned borrow), and the `rintDouble` intrinsic.
  `ord#` preserves a `Word64` character's bits as `Int64`; `geChar#` compares
  two `Word64` characters unsigned and returns `Int64` 0 or 1. `clz#` returns
  the leading-zero count of a `Word64`, including 64 for zero. The exact
  `plusWord2#` and `timesWord2#` contracts return `[Word64, Word64]` in
  high/low order; `quotRemWord2#` takes high, low, divisor and returns
  quotient/remainder in that order. Division by zero and high greater than or
  equal to divisor are typed failures that publish neither result. No 128-bit
  wire representation or general 128-bit arithmetic is admitted. The exact
  `dataToTagSmall#` contract is `[LiftedRef] -> [Int64]` within the primop's
  defined small-constructor domain: generated Tail entry forces the reference
  first, and failure status prevents scalar publication. A noncollecting host
  reads the full descriptor family tag and returns its zero-based value;
  pointer low tag bits are checked as evidence, not used as the answer.
  `indexCharOffAddr#` checks a signed offset against retained, NUL-terminated
  pinned byte storage before returning a `Word(64)` character. The exact
  C-call `strlen` intrinsic likewise scans only within that pinned owner for
  a NUL. Neither operation dereferences an unauthenticated address. Logical
  `Void` positions remain in signatures and layouts even when omitted from
  physical ABI payloads;
- exact-signature boxed small/ordinary array new/read/index/write/size/freeze,
  small-array shrink, and boxed CAS; and byte-array new/freeze/size plus
  Word8, Word64, and Int64 read/index/write, `shrinkMutableByteArray#`, and
  `resizeMutableByteArray#`, `copyAddrToByteArray#`, `copyByteArray#`, and
  `compareByteArrays#`. Resize admits exactly
  `[UnliftedRef, Int64, Void] -> [UnliftedRef]`: it reserves a new managed
  wrapper, allocates a fresh external byte payload, copies the common prefix,
  zeroes growth, and revokes the old payload only after successful allocation
  and copy. Old aliases remain structurally traceable until a successful sweep
  but reject mutator access and observation. Invalid sizes leave the old
  payload active. Byte shrink instead preserves payload identity and capacity
  while shortening its logical length. Address copy admits only a complete
  span of compiled-program-owned pinned bytes, including an empty span at the
  owner's end; it does not admit arbitrary host pointers. Array host paths
  authenticate active descriptor-backed payloads, check bounds, and report
  typed failures. `copyByteArray#` admits exactly
  `[UnliftedRef, Int64, UnliftedRef, Int64, Int64, Void] -> []`, validates both
  complete active byte spans before any write, and rejects source/destination
  aliases (including an empty copy) with a typed failure. Its noncollecting
  copy updates the external revision only for nonempty writes.
  `compareByteArrays#` admits exactly
  `[UnliftedRef, Int64, UnliftedRef, Int64, Int64] -> [Int64]`; it accepts
  aliases and compares unsigned bytes, returning -1, 0, or 1 only after both
  spans validate. Both paths return status failures without a partial copy or
  comparison result. The exact C-call `_hs_text_memchr` intrinsic admits
  `[UnliftedRef, Word64, Word64, Word8, Void] -> [Int64]` over one
  descriptor-backed byte array: the host authenticates the active payload
  through the external-storage ledger, requires the offset/length span to lie
  entirely within the array's logical extent, and returns the first needle
  index relative to the span start, or -1 when absent. It is read-only and
  noncollecting; a span or authentication failure is typed and publishes no
  result. Focused resize tests in
  [`bytes_tests.rs`](../tidepool/codegen/src/prepared_program/bytes_tests.rs)
  and [`machine_state.rs`](../tidepool/codegen/src/machine_state.rs) cover
  reserve-time collection, prefix/growth, revoked aliases, failed sizes, and
  deferred reclamation; they are not corpus-progress evidence. These operations
  do not imply parity with every GHC array primop;
- the shipped `Tidepool.Double` `renderDouble`/`renderDoublePrec` wrappers.
  Their real Haskell bodies use `show`/`showsPrec` and are not bottoming
  placeholders. Projection replaces only wrappers whose resolved source bytes
  match the extractor's compile-time shipped source, whose full GHC module
  identity and types match, and whose Text constructor has the expected
  `[UnliftedRef, Int64, Int64]` fields. The generated body preserves lazy
  precedence demand and calls exact-signature native formatting intrinsics;
- admitted `raise#` and saturated `NoSuccess` calls. Generated terminal code
  records the raised exception or unexpected return and exits with failure
  status without publishing a result. PAP completion and excess application
  stop at the saturated prefix; a partial application returns a lifted value.

A read-only audit of operation declarations in the 709 projected artifacts of
`suite.Ej6U9S` found all 77 distinct exact operation-identity/signature pairs
recognized by the current native operation catalog. This is recognition
coverage for that finite artifact set, not coverage of all GHC primops or a
new corpus execution/comparison result.

The implementation anchors for these claims are
[`entry.rs`](../tidepool/codegen/src/prepared_program/entry.rs),
[`apply.rs`](../tidepool/codegen/src/prepared_program/apply.rs),
[`forcing.rs`](../tidepool/codegen/src/prepared_program/forcing.rs),
[`old_space/prepared.rs`](../tidepool/codegen/src/old_space/prepared.rs),
[`gc/promotion.rs`](../tidepool/heap/src/gc/promotion.rs),
[`gc/raw.rs`](../tidepool/heap/src/gc/raw.rs),
[`arrays.rs`](../tidepool/codegen/src/prepared_program/arrays.rs),
[`byte_arrays.rs`](../tidepool/codegen/src/prepared_program/byte_arrays.rs),
[`data_tag.rs`](../tidepool/codegen/src/prepared_program/data_tag.rs),
[`wide_words.rs`](../tidepool/codegen/src/prepared_program/wide_words.rs),
[`static_bytes.rs`](../tidepool/codegen/src/prepared_program/static_bytes.rs),
[`formatting.rs`](../tidepool/codegen/src/prepared_program/formatting.rs),
[`floating.rs`](../tidepool/codegen/src/prepared_program/floating.rs), and
[`execution_schema.rs`](../tidepool/repr/src/execution_schema.rs). The terminal
path is owned by
[`no_success.rs`](../tidepool/codegen/src/prepared_program/no_success.rs),
[`primitives.rs`](../tidepool/codegen/src/prepared_program/primitives.rs),
[`apply.rs`](../tidepool/codegen/src/prepared_program/apply.rs), and
[`invocation.rs`](../tidepool/codegen/src/prepared_program/invocation.rs).
Focused native cases for exact, PAP, excess, logical `Void`, and unused-join
behavior are in
[`no_success_tests.rs`](../tidepool/codegen/src/prepared_program/no_success_tests.rs).
Focused settlement, application, entry, and retention tests are in
[`settlement_tests.rs`](../tidepool/codegen/src/prepared_program/settlement_tests.rs),
[`apply_tests.rs`](../tidepool/codegen/src/prepared_program/apply_tests.rs),
[`entry_tests.rs`](../tidepool/codegen/src/prepared_program/entry_tests.rs), and
[`retention_tests.rs`](../tidepool/codegen/src/prepared_program/retention_tests.rs),
alongside the heap GC tests under
[`tidepool/heap/src/gc`](../tidepool/heap/src/gc).

This is an executable connected subset, not a producer cutover. Imports are
admitted as described under the compiled-program path above (read by slot;
not yet callable or case-dispatched from generated code); effects, other
foreign/primitive operations, and managed host arguments through
`run_entry` remain outside this path. `Atom::Rubbish`
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

### Fingerprinting and address ownership

The pinned `ghc-internal` CCall leaves `__hsbase_MD5Init`,
`__hsbase_MD5Update`, and `__hsbase_MD5Final` retain their exact signatures
through projection and execute in `prepared_program::fingerprint`. They are
live Typeable/exception machinery, not deferred stack capabilities. The kernel
is the pinned GHC implementation; context bytes cross into aligned owned Rust
storage before C runs. `MachineState` authenticates mutable address spans using
its existing external-storage ledger, while immutable inputs use `PinnedBytes`.
Neither path accepts an arbitrary non-null address as authority.

The pinned text package's `_hs_text_memchr` C call takes the managed
byte-array route, not an address route: the operand is a `ByteArray#`
descriptor whose payload the ledger must prove active before any byte is read.
Projection admits it by exact label and exact signature; the text unit id
carries a version and package hash, so only its `text-` package-name prefix is
a stable guard, and the emitted intrinsic identity is the bare symbol with the
CCall convention. Unit identity is a projection-time guard only and is not
part of the schema identity.

`newPinnedByteArray#` uses stable external byte storage. Contents addresses are
scalars and do not root their wrappers. `keepAlive#` therefore retains its
managed owner across the generated callback using a post-call opaque SSA use;
its admitted operation contract is `Returns`, not `NoSuccess`. Signed byte
offsets remain confined to the base address's original allocation. These
contracts cover byte reads/writes and the three fingerprint leaves, not general
foreign pointer access or arbitrary foreign calls.

### Remaining forms

`raiseIO#` retains its state-threaded successful wire signature but native
execution is terminal: the existing raised-operand root and failure settlement
owner handles it exactly as `raise#`. It does not publish a result or implement
catch/masking. A failure restores reusable thunks for retry without replacing
the first cause. The recovered fingerprint allocation helper references this
operation on its invalid-alignment branch, independently of IPE capabilities.

`newAlignedPinnedByteArray#` uses the existing external-storage owner with a
recorded published offset. Padding precedes the capacity/length prefixes;
the actual allocation base and Layout remain the reclamation authority. Valid
power-of-two alignment is honored by the contents address and preserved through
resize. Invalid size/alignment fails without publishing a wrapper payload.

The pinned `GHC.Internal.Stack.CloneStack.$wgo` is the recursive
`getDecodedStackArray` worker: `[UnliftedRef, Int64, Void] -> Returns[LiftedRef]`.
It owns IPE lookup and InfoProv string decoding, including UTF8 cleanup through
`bracket`. It is represented by `ghc:decodeStackEntries`, an exact deferred
function capability, not by pretending its internal catch/masking operations
are no-ops. The pinned `collectBacktraces` unfolding calls it only in the IPE
branch; the default configuration enables HasCallStack and disables IPE.
Changing that configuration can reach a typed capability failure, never a
fabricated decoded stack. The native catalog checks the signature separately
from the lower-level `ghc:decodeStack` primcall signature.

The producer still rejects unsupported literal shapes such as `BigNat` and
relocatable labels, and rejects primitive/foreign calls without a wire/native
contract. Validated projection can therefore be broader than connected native
execution. Haskell projects `RaiseOp` as `NoSuccess`, and uses demand evidence
with the actual prepared entry arity to mark only saturated bottoming calls.
The focused prepared-STG test checks named bottoming callees with exact
`[Int64, Float64]` tuple, unary `Int64`, and `Void` entry/call signatures. Its
partial-call assertion uses a test-local prepared-STG variant because CorePrep
eta-expands the source PAP; it does not claim a source-retained partial
`StgApp` or synthetic `LFUnknown` negative coverage. See
[`ExecutionProjection.hs`](../bridge/haskell/src/Tidepool/ExecutionProjection.hs) and
[`RecoveredBodyTest.hs`](../bridge/haskell/test-prepared-stg/RecoveredBodyTest.hs).

External-payload graph support is a separate boundary. The machine ledger
authenticates pointer-slot views through
[`external_storage.rs`](../tidepool/heap/src/external_storage.rs) and
[`machine_state.rs`](../tidepool/codegen/src/machine_state.rs); descriptor
copying and selective promotion traverse those edges in
[`gc/raw.rs`](../tidepool/heap/src/gc/raw.rs) and
[`gc/promotion.rs`](../tidepool/heap/src/gc/promotion.rs). The prepared minor
collector consumes that descriptor path in
[`host_fns/gc.rs`](../tidepool/codegen/src/host_fns/gc.rs). The prepared minor
collector sweeps Young payloads only after its final successful copy;
promotion retains selected payloads independently of their young wrappers.
The machine's checked stores update the external revision, invalidating stale
sweep plans. These are connected mechanisms for the admitted arrays, not a
claim that every external-storage operation or retention path is implemented.

### `noDuplicate#` execution invariant

Prepared execution lowers `noDuplicate#` to a no-op only while one invocation
is the sole evaluator of its private heap, turns are serialized, thunk entry
installs a blackhole before evaluation, and no scheduler can begin another
evaluation on that heap while the first is active. Cancellation restores a
thunk to `Live` and retries it from the beginning; it is not GHC's suspended
stack resumption. This can repeat allocation-only `unsafePerformIO` work, which
is currently the only admitted use requiring `noDuplicate#`.

Session integration must not preserve this lowering if an effect-suspended
evaluation can coexist with another cell turn or sibling realm on the same
heap. In that model, entering the suspended evaluation's blackhole means wait,
not `<<loop>>`, and evaluator identity plus wakeup/settlement must become an
explicit runtime contract before concurrency is admitted.

Wave 6B's compiled-`qApp` resume path (`freerRequest`/`resumeInt`, projected
via `bridge/haskell/test-prepared-stg/FreerResume.hs`) exercises exactly this
boundary without yet needing an evaluator-identity contract, because
suspension there is a plain call/return: `run_entry`/`run_entry_retained`
returns the freer `E` constructor at WHNF, no native stack is captured, and
every thunk on the path to that return has already settled -- the only
`Evaluating` headers a resume can find are single-entry thunks deliberately
retained after consumption (pinned by
`prepared_program::freer_boundary_tests`). A parked continuation is therefore
inert heap data, and `&mut self` on `PreparedMachine` already serializes
every call, so two parked continuations sharing one heap do not create the
blackhole-ownership ambiguity this section describes -- there is still only
ever one evaluator, taking turns. `tidepool/runtime/tests/prepared_execution.rs`
pins this directly: `parked_continuations_resume_out_of_order_with_a_collection_between`
resumes two independently-parked continuations in reverse order with a forced
collection between them; `unrelated_entry_runs_while_a_parked_k_stays_untouched_and_machine_reusable`
runs an unrelated entry to completion while a continuation sits parked and
asserts the machine's disposition stays `Reusable` throughout, plus that
inspecting a parked continuation's own closure field is a typed
`ObservationFailure::Unobservable` refusal, never a forced value; and
`cancellation_before_commit_leaves_a_parked_k_valid_for_retry` drives the
compiled adapter directly (not through the session's own precondition
check) to confirm cancellation observed at `resumeInt`'s `prepared_poll_at`
safepoint leaves the continuation handle valid for an uncancelled retry.

This is a narrower claim than the general problem above, not a resolution of
it: it holds only because nothing here ever captures a native stack or lets
a second call begin before the first returns. The evaluator-identity /
blackhole-vs-loop distinction this section already names remains required
before any design admits concurrent native-stack suspension on one heap, and
before rung 3's registry-generalization work (porting the old engine's
parked-continuation registry onto this heap) is attempted.

`MutVar#` has its own object descriptor but reuses the external boxed-storage
owner. Any future external-value observation must classify by descriptor
identity; `ExternalStorageKind::BoxedArray` is storage layout, not language
meaning.

There is no GHC stack-snapshot intrinsic in this prepared projection/native
operation catalog. Runtime root snapshots used by forcing and observation are
internal mechanisms, not an authored primitive. Source-less package/interface
copies of `Tidepool.Double` cannot pass the source-byte authority check and
remain on ordinary recovery; no formatter replacement is inferred from a
module name alone. Native formatting currently builds a Rust `String` before
allocating its checked external byte payload; that temporary formatting
allocation is not fallible and can abort on process OOM rather than returning
the runtime's typed heap-overflow status.

Generated-code invocation and case dispatch over imported values, effects,
foreign calls, unsupported primitive operations, and full corpus execution
remain separate work. No session-retention or effect-support contract is
asserted here. No production cutover or compatibility promise is implied by
this inventory.

## Identity and corpus manifest contract

Reachability follows GHC binder `Unique` values throughout the prepared
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
