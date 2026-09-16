# Prepared STG: known issues

Costs and gaps of the prepared-STG engine that are understood but not yet
fixed. Each entry says what happens, why, and where the design or evidence
lives. Remove an entry when it is fixed. Prepared is the default route;
`TIDEPOOL_ENGINE=core` selects the old engine.

## Front-end cost

### Every turn re-projects and recompiles its library closure
`recoverPreparedClosure` (`haskell/app/Main.hs`, `PreparedRecovery.hs`) starts
from a fresh interface cache on each request and re-prepares every module
that contributes bodies. Library code is compiled into each turn's program;
only session bindings are imported. Every live program also registers its
own top-level values as persistent roots, which every minor collection
scans. Likely the largest remaining Rust-side cost per turn.
Direction: install the library closure once per machine as a shared program
and import its tops by identity; keep the interface cache warm in the
daemon. Architectural; not designed yet.

### Closure recovery recomputes everything each round
Each recovery round rebuilds the top-identity map, dependency map and
reachability closure over all modules (`PreparedRecovery.hs`,
`selectPreparedTarget` in `ExecutionProjection.hs`), and inserts use O(n)
appends. Top-level identity naming is quadratic in top binders and cubic for
repeated occurrence names (`ExecutionProjection.hs`, `chooseOccurrence`).
Signature, constructor and global interning use list lookups.
Direction: an incremental worklist; accumulators with a final reverse; maps
keyed by signature and `DataCon`.

### Prepared requests still build the Core artifact
A prepared turn also writes the closed Core translation and its metadata
(`writeWholeModuleClosed`), and the runtime reads them only for free
variables, the constructor-table merge and request sites.
Direction: derive those from the prepared program and skip Core emission on
prepared requests.

## Generated code

### Call dispatchers grow with call offers, not signatures
Each demanded call signature gets a linear header-compare chain over every
function and partial-application layout (`prepared_program/apply.rs`). On a
297 KB suite artifact: 235 dispatchers, 1.3 MB of code, about 400 ms of a
900 ms test-build compile. At run time a dynamic call walks the chain.
Design: `plans/handoff/designs/descriptor-dispatch.md` (dispatch through the
object's descriptor, as GHC info tables do). Boundary decision pending.

### Forcing an untagged object is O(program size)
`prepared_enter` (`prepared_program/entry.rs`) is one function whose chain
compares the header against every constructor, function, PAP layout and
thunk. Design: the same note, slice 1 (switch on the descriptor's kind).

### Representation-polymorphic functions compile once per result shape
A function whose result the caller chooses is compiled for every result
shape in the program's signature table plus the lifted one
(`plan::result_instances`), whether or not a call demands it. Computing the
demanded set needs a new fixpoint analysis, and shrinking it must not turn a
supported foreign call into `UnresolvedCallee`. No measured artifact has such
functions yet.

### Compile timings from test builds are inflated
Test builds keep a copy of every function's IR (`prepared_program.rs` with
`pipeline.rs`), so compile numbers taken from test binaries overstate release
cost. Compare only like with like.

## Host answers and effects

### Ordinary effects
Only the verbs in `EffectSchema.sitedVerbs` carry a dynamic site; ordinary
effects (`say`, file, KV, HTTP, form `ask`) get synthetic sites keyed by the
request constructor. Design: `plans/handoff/designs/synthetic-sites.md`.

### `Value`-carrying replies go through a leaf adapter
`kvGet`, `httpGet`/`httpPost` and form `ask` reply with Aeson `Value`, whose
`KeyMap`/`Map`/`Vector` spines the type policy still refuses field-by-field.
A `Value`-typed answer node is instead lowered whole as rendered JSON text
and decoded through the turn's admitted `__decodeValue` root
(`Tidepool.Aeson.Value.eitherDecodeValue`), spliced into the outer answer as
a borrowed handle (design: `synthetic-sites.md`, decision 4 — taken). A
decode failure (`Left`) is a typed `AnswerRejected` refusal with the frame
left parked; nothing else host-side builds a `KeyMap` directly.
`Tidepool.Aeson.Scientific.Scientific` (`Value`'s `Number` field) is
force-refused by `TypePolicy.isForbidden` rather than classified: its
strict-field source-vs-runtime layout is not stable across independently
compiled programs (a compile that recovers `eitherDecodeValue`'s body fresh
computes different reps than one that only reaches `Scientific` through a
reply type's structure), and the prepared runtime refuses to install a
second program whose declaration disagrees with an earlier one's.

`decode_json_leaf` projects the decode root's `Either Text Value` result by
comparing `inspect_outer`'s returned `DataConId` against `Left`/`Right`
resolved from the session-wide `DataConTable` (`get_by_qualified_name`).
Fixed: `ExecutionProjection.hs`'s `lowerAuxiliaryRootEvidence` now
force-interns type evidence for every admitted auxiliary root's own answer
type (read off the binder's GHC `Type` via `splitFunTys`, not its STG
result type — `__decodeValue = Aeson.eitherDecodeValue` is an
eta-unexpanded, zero-arity CAF whose STG result type is the whole function
arrow), the same way a declared site's answer type is interned. A related
naming gap was fixed alongside it: `Either`'s defining module is
`GHC.Internal.Data.Either`, not `Data.Either` (the qualified name the Rust
reader queries); `Tidepool.Identity.moduleAliasTable` now normalizes it,
mirroring the existing `GHC.Internal.Maybe`/`Data.Text.Internal` entries.

### `__decodeValue`'s own execution is not implemented on the prepared route
Distinct from the evidence gap above, and still open. `__decodeValue`'s
projected entry is a zero-arity thunk (`eitherDecodeValue` carries
`{-# OPAQUE #-}` specifically so GHC never exposes its arity to callers for
eta-expansion, per its own doc comment in
`haskell/lib/Tidepool/Aeson/Value.hs`), but the session's prepared-runtime
caller (`decode_json_leaf` / `run_entry_retained`) calls it as if it were an
arity-1 function entry — `prepared engine: prepared execution failed: entry
arguments: expected 0 physical scalar slots, got 1`, reproduced by
`notebook_value_answers_on_prepared_stg`'s `kvGet`/`Just`-`Object` case.
Separately, and regardless of arity: the legacy Core path intercepts calls
to `eitherDecodeValue` in `Translate.hs` and lowers them to the `JsonDecode`
primop (Rust `serde_json` builds the `Value` ADT directly; the Haskell body
is a stub, `eitherDecodeValue _ = Left T.empty`, that "never actually runs
at a call site" per its own comment). No such interception exists for the
prepared-STG path — `PreparedBuiltins.hs`'s `deferredFunction` table has no
`eitherDecodeValue` entry, and no `JsonDecode`-equivalent primop exists in
`tidepool-codegen`'s prepared-program machinery
(`tidepool-codegen/src/prepared_program/*.rs`) — so even with the arity
fixed, `__decodeValue`'s projected body would run the OPAQUE stub verbatim
(always `Left`), not a real JSON parse. Fixing this is a prepared-machine
feature addition (a real primop or an equivalent `DeferredFunction`-style
registered replacement, plus the arity/eta handling above), not a
projection-evidence fix; out of scope for the evidence fix above.

### `LiveReentry` deliveries are not answerable yet
Handle and framed-handle answers are supported (bare and framed delivery by
borrow, mirroring Core's handle-delivery contract). A site whose delivery is
`LiveReentry` rather than `HostAnswer` still has no prepared-route producer.

## Residency

### Interned literal bytes are never freed
Literal `Addr#` bytes are interned once into one permanent, content-keyed
machine-wide pool (`MachineState`'s `PinnedBytes`); a program's own literal
addresses always resolve against it, and two programs declaring the same
content share one address. Content is never removed, even once every
program that referenced it has retired: the pool is bounded by the number
of distinct literal contents compiled across the session's programs, not by
which programs are still installed. A program with no other root now
retires by ordinary reachability exactly like one with none -- its own
literal storage is no longer a liveness edge (lifetime contract edge (c) is
closed).

### External payloads are not swept from old space
Boxed-array and byte payloads reachable only from dead old-space objects stay
in the ledger; compaction keeps remembered slots outside the arenas as roots
for the same reason.

### A nursery/old-space cycle defers retirement
A retiring program reached only through a cycle between a nursery object and
an old-space object cannot be dropped by the minor and compacting collectors
separately; it is deferred (`RetirementReceipt::deferred`) and retried at the
next major collection. Untested.

### Shadowed bindings keep their programs
Root bindings and handles pin every program they reach, so nothing is
reclaimable until a shadowed notebook binding retires. Lease re-keying and a
shadowed-binding policy are open (lifetime contract decision 9).

## Session cost growth

### Per-turn scans grow with session length
Import resolution scans all live bindings per declared import; the retained
set is rebuilt and sorted per compile; site-witness re-homing is
O(sites × programs); displayed cells add names that every later compile
imports and injects (`PersistentSession::compile_view_in`).

### A displayed cell costs several compiles and installs
Rendering a cell's observation compiles three extra turn modules and installs
two extra programs. Design: `plans/handoff/designs/cell-compiles.md`.
