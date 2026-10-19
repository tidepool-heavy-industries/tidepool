# Warm worker implementation handoff

Area: `/root/warm_compiler_campaign/warm_workers`, Sol high.
Checkout: `/srv/swarm/checkouts/tidepool-shared-warm-warm-workers-20261019`.
Branch: `feature/shared-warm/warm-workers`. Base:
`5b74c2f1fe010c5c343e0c6f331d54328fbd5090`. Current compiled and executed
source: `2aa1f61d830a6495bb2158232f9a0f6d48a665a9`.
The C2 amendment `e2bd2611102566085d0c573568d820a50ed62fb3` was imported as
`6beec6fd58c5ae27ee47ff86fa5f2797895adb9c`, including its unchanged STATUS.
The campaign owner alone edits shared CONTRACTS/STATUS.

## Delivered slice

Matched TPREQ022 / TPEXO001 framing adds an exclusive source-free
EnsureExactInputs request and typed Ready/NeedsAcquisition/Rejected completion.
Complete carries the existing exact-scope manifest; Resident carries a worker-local
reference. These labels issue no root authority. The production source-free dispatch
currently returns typed UnsupportedOperation before source acquisition: no Ready
registration, resident hit, delta, pool migration or shared-warm performance gain is
claimed. Rust preserves the existing BEGIN/request/END transaction and poisons an
uncertain accepted response without replay. Haskell preserves diagnostic/cleanup
precedence and the separate human stdout channel.

The unused explicit completed HscEnv and redundant module-environment fallback
were removed after confirming the identical final-HPT owner lookup, including the
missing-HMI path. Lazy HMI, recovery and package owners remain.

The existing RequestIfaceDecoder now remembers inactive retained seals. A live
scope still decodes valid uncached data when its image is evicted; it cannot refill
the optional shared cache afterward. Standalone readExactScope owns an unrestricted
local decoder. Raw-input custody and decoder policy publish together: expensive
acquisition, validation and all staged timing writes run with cancellation restored,
then masked nested owner locks prune and publish without a fallible observer between
them. The decoded-interface count observer follows committed publication. A failure
there can leave a coherently committed state without a returned scope.

The owning genuine history proves the initial Dependency-only cache policy, injects
an IOException and external ThreadKilled during the old split point, checks two
actual old-live-scope decodes, then separately probes retained raw Dependency facts.
Two stale native-row test adapters were repaired to the current ten-field format;
classification remains unchanged and production admission still validates it. The
neighboring binder refusal checks the observed quoted `expected word` cause rather
than a generic parser failure.

## Review and execution

The single descendant grant was reused throughout; no second descendant was spawned.
Actual native task ID: `/root/warm_compiler_campaign/warm_workers/protocol`, configured
`gpt-6.1-sol`, high, fresh context. Initial implementation started at base5b74 in
`tidepool-shared-warm-worker-protocol-20261019`; successive concrete review/audit
reactivations used this same task. Its final source receipt settled at exact2aa1.
It independently reviewed area-authored publication/history repairs. Its authored
protocol/encoder pieces received separate campaign-supervisor review. Native event
history retains starts/settlements; no additional timestamp is inferred here.

Campaign source review accepted the initial full protocol and later production
atomicity. The only final source-review finding, an overly broad refusal predicate,
is repaired at exact2aa1. Child exact receipts are retained in
`/srv/swarm/evidence/inanna/shared-warm-compiler-20261019/warm-workers/protocol/WAVE2-OWNERS.md`.
The campaign owns final cumulative review/fold disposition.

Production worker, four affected Haskell binaries and Rust production/cfg(test)
consumers compiled through pinned fast-dev, local-only Buck actions. The maintained
wrappers, including their declared generator/runtime/fixture inputs, were built.
Final exact2aa1 counted execution passed **9 selected / 9 executed / 9 passed**:

- Worker response cleanup suppresses Ready: 1.
- Matched source-free request wire: 1.
- Owned scope reuse and failed-publication history, standalone finalized Core,
  and native ordinal refusal: 3.
- Rust exclusivity, response framing/correlation, and poisoned transaction custody: 4.

The authorized negative branch
`review/shared-warm/decoder-publication-negative-20261019` retains exact
`5ac20115af4eadccc4fc0aad0d5fe766921f15ef`: production ExactScope/ExactHydration
bytes from07c8 and unchanged repaired test265. It compiled, then selected/executed
one genuine owned-scope case and **failed at the intended decoder-policy assertion**:
Source read once/reused once, Dependency read twice. The feature branch was restored
before final compilation and all nine final controls. This proves IOException-mutant
sensitivity; the negative run stops there and does not establish async-mutant
sensitivity. Positive execution covers both failures.

Full exact commands, source/artifact hashes, selected/executed counts, actual exit
statuses, negative-control identity and hashed logs:
`/srv/swarm/evidence/inanna/shared-warm-compiler-20261019/warm-workers/NATIVE-VALIDATION-2aa1.json`.
Prior failures and reports are preserved, including the native-row adapter's
pre-regression failure and the binder fixture's pre-ordinal failure. Compilation
and execution are distinct in this receipt.

## Resource and hold policy

The per-checkout buck-out bind and pinned --tests generation `generation.dVqJhqs0`
remain provisioned. The exact ce60 workspace submodule is initialized. The area
Buck daemon3810824 remains owned/idle under
`/build.slice/inanna-build-1792397236-3810751.scope`; direct production GHC3842958
was observed in the same scope. No command observer remains outstanding.

The admitted native lane allowed3GiB prospective total additional output. Own
allocated output changed931872768→2665410560 bytes (growth1733537792);
sampled high2712383488. The daemon scope's cumulative memory peak was5718179840
bytes, below the granted16GiB allowance. Scratch/shared-cache-upload peak was not
independently instrumented, and concurrent root cleanup prevents attributing global
free-space changes to this lane. The installed launcher lacks the staged storage
switches; no bypass, arbitrary output deletion or shared daemon restart was used.

Actual strong owners and release/hold policy are recorded in private
`WAVE2-POLICY.md` and `protocol/WAVE2-OWNERS.md` beside the native evidence.
Dropping a future realization index cannot release monotone source/original maps,
last HPT/module graph, recovery contexts, lazy cohydrated HMI, EPS/NameCache or the
interpreter's separate loaded state. Keep required live borrows and existing
RSS/request rotation. No cohort decoder snapshots, parallel registry, new cache
budget or transitive compatibility equivalence were introduced. The planned
41/disjoint/42 native interpreter control remains unexecuted.

## Event-driven continuation

No periodic wake or polling loop is scheduled. The ready implementation/review/
repair/test frontier above is drained; final campaign source review/fold is the
next receipt. Awaited producers and exact continuation:

- **host-inputs:** actual complete authenticated owner/root and portable cold
  manifest/base relation. Its process-local selected Arc or root text is not an
  issuer. Once supplied, bind worker-local realization identity through the existing
  owner and make a genuine hit precede all whole-baseline encoding/FD enumeration.
- **owned-inputs:** admitted immutable row/byte reference and END/absence/release
  transport seam. Extend the existing physical owner; preserve all custody.
- **campaign/package-source owner:** actual retained consumed package/native-source
  proof at new worker boot, or an accepted scoped fallback/control decision. Existing
  deployment hashes frontend/worker and libdir text; fresh hashes cannot bless old
  EPS/native state. Per-operation/default reset remains held, not silently enabled.
- **qualification/campaign:** joined exact bundle/Persona/cell/lookup acceptance,
  then measured semantic/performance controls. The nine local controls do not qualify
  a release or a production Ready/hit implementation.

A producer sends exact candidate/evidence/next action to this area with send_message
if running, or followup_task if idle/completed. The same terminal-event rule applies
to this area and its child. Reactivate the existing reserved child for concrete
ready implementation or independent review; no duplicate slot or orphan command.
