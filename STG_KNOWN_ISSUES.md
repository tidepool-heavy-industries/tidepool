# Prepared STG: known issues

Costs and gaps of the prepared-STG engine that are understood but not yet
fixed. Each entry says what happens, why, and where the design or evidence
lives. Remove an entry when it is fixed.

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

### `Value`-carrying replies are unconstructible
`kvGet`, `httpGet`/`httpPost` and form `ask` reply with Aeson `Value`, whose
`KeyMap`/`Map`/`Vector` spines the type policy refuses, so the answer is
refused with the frame still parked. Options (a compiled decode adapter or
an Aeson-aware host builder) are in `synthetic-sites.md`, decision 4.
Boundary decision pending.

### Handle and framed answers
Live handles, framed custody and `LiveReentry` deliveries are not answerable
on the prepared route yet (`NotYetSupported`).

## Residency

### Programs with pinned byte storage are never retired
A program whose code embeds literal-pool addresses is kept live and reported
in `RetirementReceipt::pinned_by_bytes`; raw `Address` fields carry no header
edge back to it. Lifetime contract edge (c).

### External payloads are not swept from old space
Boxed-array and byte payloads reachable only from dead old-space objects stay
in the ledger; compaction keeps remembered slots outside the arenas as roots
for the same reason.

### A nursery/old-space cycle defers retirement
A retiring program reached only through a cycle between a nursery object and
an old-space object cannot be dropped by the minor and compacting collectors
separately; it is deferred (`RetirementReceipt::deferred`) and retried at the
next major collection. Untested.

### Major collection runs after every run
`PreparedEngine::quiesce_and_collect` performs a full mark and compaction on
every completed run, so a session that binds a large value once pays for it
on every later turn.
Direction: collect past a threshold (programs installed, or promoted bytes
relative to old bytes).

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
