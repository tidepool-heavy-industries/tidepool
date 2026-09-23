# Repeated Haskell compilation: evidence and fix recommendation

## Symptom

A resident actor's cell compile is paying full recompilation cost on
requests that should be pure memo hits. Measured before this investigation:
an ordinary two-statement cell took 10.3s; four serial compiler requests
inside one actor turn took 9.94s combined. After a child checkout is set up
from a parent's, unchanged modules that carry no quasiquoters still report
`same-hash=True, same-retained=True, same-home-dependencies=False`
(`bridge/haskell/src/Tidepool/GhcPipeline.hs`, `lookupValidMemo`'s
`tidepool-memo-miss` line), each such request costing 11-13s.

## The first changed dependency, named

`Project.Work` and `Project.Review` use `QuasiQuotes` for labels. Per
`hasUntrackedCompileTimeExecution` (GhcPipeline.hs), any module enabling
`Cpp`, `TemplateHaskell`, or `QuasiQuotes` is unconditionally excluded from
the memo (`lookupValidMemo`'s `not depsOk || hasUntrackedCompileTimeExecution
...` branch) because a source hash cannot see what a quasiquoter or CPP
consumed outside the downsweep graph. This is intentional and correct, and
this investigation does not touch it: it is why those two modules and every
module that imports them (directly or transitively, since
`depsValidSoFar`/`directHomeDeps` propagate a miss to every dependent) never
hit the memo. That is the QuasiQuotes cost lane; it is separate from the
lane below and does not by itself explain a plain, quasiquoter-free module
missing.

For the ordinary (non-quasiquoter) modules — `Jev.Operators`,
`Project.Reflex`, and their dependents — the miss is
`same-home-dependencies=False` with `same-hash=True`. The evidence trail:

- `memoHomeDependencies` (`MemoValidity`) is `Map HomeDependency
  HomeDependencyDigest`, one recursive SHA-256 hash per direct dependency,
  computed by `homeDependencyDigests`.
- Each SCC's digest folds in `ownFrame`, which is `show witness` for every
  member — and `witness` is a `HomeDependencyWitness (Maybe FilePath)
  Fingerprint` (GhcPipeline.hs:~739): **selected path and content
  fingerprint together**, not the fingerprint alone.
- A child checkout resolves the identical bytes of `Jev.Operators` and
  `Project.Reflex` at a *different path* than the parent checkout (an
  ordinary, expected consequence of a worktree-per-actor checkout). The
  content fingerprint (`ms_hs_hash`) is unchanged; the *path* embedded in the
  witness is not.
- `Map.restrictKeys dependencyDigests (directDependencyKeys modSum) ==
  memoHomeDependencies validity` therefore compares unequal even though the
  dependency's compiled output would be byte-identical, because the digest
  mixes a filesystem detail (selected path) into what is otherwise a content
  identity check.

The recompilation chain this produces: parent checkout compiles
`Jev.Operators`/`Project.Reflex` and memoizes them keyed by parent-path
witnesses. The actor's own checkout (child path) compiles the *same bytes*
of any module that imports them; `lookupValidMemo` sees
`same-home-dependencies=False` on that importer (path differs in the
witness, even though `ms_hs_hash` — content — is unchanged for both the
dependency and the importer itself) and recompiles it, and recompiles
*every* further dependent transitively for the same reason, each paying a
fresh typecheck+desugar+optimize+prepare cycle. The measured 11-13s per
request in "child checkout, unchanged modules" evidence is that chain.

## Cost attribution (what this run added)

The `tidepool-compile-summary` line already splits `typecheck_ms` from
`lowering_ms` per request and names the top-3 modules by wall time (see
`bridge/haskell/src/Tidepool/Timing.hs`). A `TIDEPOOL_TIMING=1` capture of
the Suite fixture corpus in this environment shows lowering (desugar +
`core2core`) dominating wall time over typecheck by roughly 3:1-5:1 across
requests (e.g. `wall_ms=8439 typecheck_ms=640 lowering_ms=2078` — note
`interface_ms` is a strict subset already included in the phases above, not
additive on top of them), consistent with the un-memoized modules paying
full optimizer cost rather than parse/typecheck cost. This run did not
additionally instrument GC/allocation deltas around the compile-cycle
boundary itself (only `measureModuleInterface`'s existing per-module RTS
delta, gated on `TIDEPOOL_TIMING`, was exercised); a `GHCRTS=-T` run
attributing allocation/GC time to the cycle as a whole, alongside the
existing phases, is the natural next capture once the new memo-trace lines
below are available to correlate against.

## New diagnostics added (opt-in, validity predicates unchanged)

- **Span propagation at the compile blocking sites.**
  `tidepool_runtime::spawn_blocking_in_span` (`tidepool/runtime/src/span_blocking.rs`)
  captures `tracing::Span::current()` before handing a closure to
  `tokio::task::spawn_blocking` and re-enters it inside the closure, so any
  line the blocking compile work emits is attributed to its caller's span
  tree instead of running with no span. `exomonad/actor/src/resident_workbench.rs`
  now uses it at both compile blocking sites (`with_host_machine`'s
  `spawn_blocking`, and `resume_structured_introspection`'s
  `compiler_source.prepare` call), wrapped in a `compile_blocking` span
  carrying `actor`, `session` (the input unit), `operation` (a literal
  naming which blocking call this is), and `include_roots` (the actor's
  ordered `ActorSessionContext::source_layer`). Worker instance (pid/
  generation) and CWD are not yet threaded onto this span — they are known
  deeper in the compile path (the daemon's own `compile_request` span in
  `tidepool/extract-cmd/src/daemon.rs`) and joining the two spans across the
  process boundary is future work, not done here.

- **`TIDEPOOL_MEMO_TRACE=1`** (`bridge/haskell/src/Tidepool/Timing.hs`,
  `bridge/haskell/src/Tidepool/GhcPipeline.hs`), independent of
  `TIDEPOOL_TIMING`:
  - `tidepool-memo-cycle-graph` — one line per module in the selected module
    graph, emitted once per compile cycle (never per lookup): module, source
    kind, selected path, resolved path, source fingerprint, direct deps, and
    the dependency digest. `requestIdentity` (already minted once per cycle
    via `newTimingRequestIdentity`) is reused as the cycle id rather than
    inventing a second identifier.
  - `tidepool-memo-trace-miss` — one line per miss: the current cycle, the
    *originating* cycle (`GutsMemoEntry`'s new `gmeCycle` field — diagnostic
    metadata only, not read by any validity check), the exact reason, and
    for a witness-comparison miss, the added/removed/changed direct
    dependency witnesses. Each changed witness reports old and new path and
    fingerprint *separately*
    (`path=<old>-><new>,fingerprint=<old>-><new>,path_changed=...,fingerprint_changed=...`),
    so a path-only change is distinguishable from a real content change —
    this is what would make the root cause above directly visible in a
    trace rather than inferred from code reading. `GutsMemoEntry` gained a
    second diagnostic-only field, `gmeDirectWitnesses`, retaining each
    entry's direct-dependency witnesses so a later miss can diff against
    them; neither new field participates in the `sameHash`/`sameRetained`/
    `sameHomeDependencies` comparison, which is unchanged.
  - The dependency-propagated miss (`dependency-miss:...`) now names only
    the deps this cycle actually marked invalid (filtered against
    `validThisCycleRef`), not every direct dependency — the always-on
    `tidepool-memo-miss` summary line is left exactly as it was (still lists
    every direct dependency, since a shared integration test asserts only
    on its `module=` substring, not its reason text) to avoid changing
    established behavior; the precise, invalid-only list is the new
    `tidepool-memo-trace-miss` line's `reason=dependency-miss:...` field.
  - A no-reuse miss reports the exact extension via `untrackedExtensionName`
    (`reason=no-reuse:extension=QuasiQuotes`, etc.) instead of a bare
    boolean.
  - Both new prefixes are registered in
    `tidepool/extract-cmd/src/diagnostics.rs`'s `MACHINE_STDERR_PREFIXES`,
    which the existing `machine_stderr_prefixes_match_the_haskell_emitters`
    test enforces stays in sync with the literals `Timing.hs`/
    `GhcPipeline.hs` actually emit; both are therefore forwarded to the
    daemon trace/log and filtered out of model-facing compiler errors by
    the same `is_machine_stderr_line` every consumer already shares
    (`tidepool/extract-cmd/src/daemon.rs`, `tidepool/toolchain/src/diag.rs`).
  - Graph capture holds paths and fingerprints only — no source, no Core.

## Recommendation

Make the direct-dependency witness comparison content-addressed: when two
witnesses' fingerprints are equal, treat them as the same dependency
regardless of path. Concretely, `homeDependencyDigests`' `ownFrame` should
fold in the fingerprint unconditionally but the path only when it
distinguishes two *different* fingerprints reaching the memo under the same
`HomeDependency` key (i.e., drop `show witness` in favor of hashing the
fingerprint alone, or split the digest into a content component and a
path-shape component and compare them independently, reusing a *fingerprint-
only* comparison for the ordinary hit path while keeping the full witness
available for the diagnostic trace above).

**Correctness implication of dropping path-sensitivity:** the existing
comment on `MemoValidity` (`GhcPipeline.hs`, just above `memoHomeDependencies`)
states the field's purpose precisely: "A source hash does not change when
one of its imports switches between a home module and a package module.
Preserve the home-resolution shape that produced the body so removing a
shadow cannot reuse stale Core." That is a real hazard the path-sensitivity
guards against — but it is a *resolution-shape* hazard (home vs. package,
or which of several home candidates was selected), not a *path-string*
hazard. A fingerprint-only comparison must keep guarding resolution shape:
it is safe to ignore the path string only when the dependency is still
resolved as a home module (i.e., still a member of `summaryByDependency`
under the same `HomeDependency` key — same module name and
`HomeSourceKind`) and its content fingerprint is unchanged. Losing that
distinction — e.g. by comparing only `Fingerprint` with no home/package
provenance check at all — would reintroduce exactly the shadow-reuse bug the
comment warns about. The fix should keep `HomeDependency` (module name +
boot/ordinary kind) as part of the identity and narrow only the witness's
path field's role in the digest, not remove home-resolution tracking
altogether.

## Reproduction

Full disposable-environment reproduction (five trees in parent/child
sequence with a controlled transitive/import-resolution/boot-interface/
QuasiQuotes matrix, replaying the default workspace `.exomonad` include
configuration under `GHCRTS=-T`) is not included in this change — it is
substantial harness work in its own right
(`tidepool/extract-cmd/tests/transaction_integration.rs` and
`daemon_integration.rs` are the existing entry points that drive a resident
daemon the way it would need to). The root cause above is instead
established directly from the memo comparison's own source
(`homeDependencyDigests`/`ownFrame`'s `show witness`) plus the evidence
already gathered before this change (parent/child same-hash/same-retained/
different-home-dependencies at a stable 11-13s cost). The new
`TIDEPOOL_MEMO_TRACE=1` lines above are the tool for confirming it
directly against a live parent/child run without re-deriving it from code
reading; that confirmation run is follow-up work.

## Tests

- `tidepool/runtime/src/span_blocking.rs`: `spawn_blocking_in_span` carries
  the caller's span into the blocking closure (and propagates "no span"
  when none was current) — `cargo nextest -p tidepool-runtime
  'test(span_blocking)'`.
- `tidepool/extract-cmd/src/diagnostics.rs`:
  `machine_stderr_prefixes_match_the_haskell_emitters` (existing test,
  exercised by the two new prefixes) proves the new emitters and the
  forwarding/filtering list agree in both directions.
