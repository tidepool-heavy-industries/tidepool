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
`selectPreparedTarget` in `ExecutionProjection.hs`). An actor turn takes
about 37 rounds. Signature, constructor and global interning use list
lookups.

Measured on a warm daemon for one Shoal actor turn (`TIDEPOOL_TIMING=1`;
the compiler log records every phase):

| phase | before | after the spelling fix |
|---|---|---|
| `prepared_recover_refs` (37 rounds) | 18.6–31.6 s | 3.4–4.6 s |
| `prepared_recover_prepare` | 1.4 s | 1.4–1.8 s |
| `prepared_recover_lookup` | < 0.1 s | < 0.1 s |
| GHC `core` | 3.5–5.5 s | unchanged |
| whole request | 26–42 s | 15–16 s |

The spelling fix replaced a per-entry list append in
`assignTopIdentitySpellings`, which made each round quadratic in top
binders.
Live Shoal session (2026-09-17, `shoal-console`, warm daemon): every
workbench statement costs 9–10 s of daemon time against ~40 ms of GHC
(`compile summary ... wall_ms=38 ... top=Expr:15`); a four-statement cell
takes about 75 s end to end. Recovery is the whole gap.
Direction: an incremental worklist so a round touches only the modules it
added; maps keyed by signature and `DataCon`.

### `lookup` typechecks the library closure once per query
The actor's `lookup` tool answers each query with its own GHC request
(`compiler request started` per query in the daemon log), and each request
re-typechecks the 66-module library closure (~4 s). Queries run
sequentially, so a seven-query batch took 27.6 s in the live Shoal session
(2026-09-17). Nothing is executed; this is inspection only.
Direction: one GHC request per batch, or answer name queries from the
warm interface cache without a typecheck.

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

### JSON decode runs a real Haskell parser on prepared, `serde_json` on Core
Fixed. `__decodeValue`'s two remaining gaps — the arity mismatch
(`entry arguments: expected 0 physical scalar slots, got 1`, from treating
the projected zero-arity thunk as an arity-1 entry) and the OPAQUE stub body
(`eitherDecodeValue _ = Left T.empty`, which "never actually runs at a call
site" on Core but is the whole story on prepared, since no
`JsonDecode`-equivalent primop or `deferredFunction` interception exists for
prepared-STG) — are both closed. The arity fix names the scaffold's
parameter like `__resume` does (`tidepool-runtime/src/session/turn.rs`); the
stub is now a real, total RFC 8259 recursive-descent parser over `Text`
(`haskell/lib/Tidepool/Aeson/Value.hs`, unit-tested by the
`aeson-value-spec-test` cabal test-suite).
Remaining, load-bearing difference: the two routes are NOT one
implementation wearing two hats. Core intercepts `eitherDecodeValue` by name
in `Translate.hs` and lowers it to the `JsonDecode` primop, which dispatches
to Rust `serde_json` and builds the `Value` ADT directly on the heap; no such
interception exists on prepared-STG, so a prepared program compiles and runs
the Haskell parser for real, on every decode. Both must accept/reject the
same grammar and produce `Scientific` values that compare and render
identically (`Eq`/`Show` are representation-independent — see that module's
Haddock) — checked by `notebook_either_decode_renders_the_same_on_both_engines`
(`tidepool-runtime/tests/prepared_turn.rs`) — but they are not required to
agree on `Left` error MESSAGE text, and the prepared route pays a real
interpreted-parser cost per decode that Core's native primop does not.

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
