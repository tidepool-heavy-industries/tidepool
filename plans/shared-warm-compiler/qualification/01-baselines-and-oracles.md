# Baselines and independent oracles

Establish what the current production path does and which checks can detect a
wrong rewrite. Read [property testing](../../../docs/property-testing.md),
[compiler profiling](../../../docs/compiler-profiling.md),
[build admission](../../../docs/swarm-builds.md) and
[bundle qualification](../../../build/package/README.md). Extend their existing
runners, producers and report contracts.

## Exact baseline and execution status

Use `70a92c65a02a591f76590daf4e5bfa3e6fcbf723` as the planning/source starting
point; record the actual baseline selected for execution and any intervening
semantic changes. Build and freeze that exact baseline through the central
qualification owner before a paired performance comparison. Record profile,
workspace/shared gitlinks, compiler and Harness identities, descriptor hash,
worker topology, job allowances and trace profile. Current source observations
and historical frozen runs are separate evidence.

The `20d7`/B108 measurements are historical and use different source/workload
contracts. Do not use their phase totals or 404-second denominator as a current
baseline, combine the two cohorts, or predict a current speedup from them.
The research basis is retained privately in
`/srv/swarm/evidence/inanna/perf-fork-map-audit-20261019/incremental-research/shared-warm-performance.md`
and `shared-warm-lifecycle.md` beside it.

At the planning base, the stronger
`actor_host::startup_persona_acceptance_tests::persona_with_shared_workspace_keeps_lookup_and_later_cells_usable`
case is pending. Check its actual terminal report before calling the baseline
passing. Earlier successful startup or native/notebook lookup prefixes do not
establish the later pure cell. Keep presentation assertions aligned with each
actual native/notebook renderer and inspect the internal typed outcome where
that is the contract.

Compile the smallest changed packages and all directly affected test consumers
through `swarm-build just check PACKAGE...`, after inspecting
`just check-plan PACKAGE...`. Discover exact tests with the existing runner;
then execute focused selections, for example:

```sh
swarm-build just test-lib tidepool-toolchain \
  --exact declaration_context::native_availability_tests::direct_native_advertisement_follows_canonical_binding_and_keeps_historical_permissions \
  --expected-count 1
swarm-build just test-lib tidepool-toolchain \
  --exact declaration_context::native_availability_tests::private_transport_refuses_selected_dependency_product_and_interface_conflicts \
  --expected-count 1
swarm-build just test-lib exomonad-host \
  --exact actor_host::startup_persona_acceptance_tests::persona_with_shared_workspace_keeps_lookup_and_later_cells_usable \
  --expected-count 1
```

These are commands to execute at the assigned revision, not claims that they
passed. Configure declared tests/browser resources as required by the host test
target. Use the current package/target roster; linking or listing is not a test.
Frozen production qualification later uses the frozen owner's sealed cohorts.

## Root policy: independent of the repaired helper

Read the retained counterexample and provenance:
`/srv/swarm/evidence/inanna/persona-1895-launch-20261019/compiler-failure/HLS-REWRITE-HANDOFF.md`.
The confirmed defect admitted a retained equal-interface historical Original
as an optional direct root despite a different explicit Original selection.
The strict consumed-native-namespace validator was correct to reject the
resulting genuine conflict. Do not weaken it.

Build independent expectations from small semantic facts, using production
issuers to construct actual certificates and inputs:

| Independent expectation | Required positive and negative observations |
| --- | --- |
| Direct roots agree with canonical role and any explicit Original | Exercise both possible explicit winners under equal canonical interfaces; dormant alternative stays retained but is not a root |
| Required issued edges remain exact | An excluded direct root can still be a required historical child of surviving code; preserve its Original/group/carrier identity |
| Permission follows required closure | Keep the required child ordinal, exclude unrelated child groups; Full/subset/explicit-empty and no decision remain distinct |
| Genuine consumed conflict refuses | A required dependency contradicting explicit selection must refuse through the real request frontdoor, before cell effects/publication |
| Custody and selection are separate | Retained bytes/leases survive where required without introducing compiler names, native roots or execution grants |

The existing differential transport reference called the same filtering helper
as production and missed this defect. Preserve it only for the narrower claim
it can prove. Root seed/closure expectations must use an independent list or
fixed-point model with explicit winning Original and issued child relations;
do not call production filtering, normalization, identity maps or their cached
answers. Check missing **and** extra roots/permissions. Demonstrate sensitivity
against retained unfixed evidence or a bounded isolated mutation of the faulty
root policy, without changing the active shared checkout. A passing round trip
or two algorithms sharing the same seed policy is insufficient.

Run a frontdoor refusal control through actual checked input capture → request
selection/receive → compiler/runtime consumer. Assert no compiler/native/effect
publication beyond the permitted failure boundary, and unchanged prior selected
state. Distinguish refusal before acceptance from an accepted indeterminate
operation; the latter never authorizes replay.

## Extend maintained semantic histories

Reuse the adjacent Rust proptest/Haskell QuickCheck drivers, shrinking and saved
regressions. Compare maintained indexes and selected support with straightforward
full recomputation from owner-issued primary facts; compare language semantics
with the declared GHC oracle and existing prepared-corpus machinery. Keep model
identities independent of returned handles. The replay driver maps symbolic
recipe identities to objects earned through normal production issuers; model
validity flags, invented IDs or fabricated certificates never mint production
proofs or enter the compiler as authority. Add a bounded history family where
existing coverage cannot reach a changed boundary; do not create a giant new
framework or test-only authority factory.

Cover these neighboring laws early:

- Same payload/different issuer or scope is not interchangeable evidence;
  selected exact dependency/carrier relations survive fork and cold recovery.
- Home membership comes from the recipe's independently declared admitted-world
  set, never production inventory/helper output or executable demand. Merely
  retaining a dormant historical owner for the same `(unit,module)` leaves a
  Package import legal; selecting/declaring a zero-group home invalidates it
  even without native demand. Include issuer-established interface-only homes
  where applicable. Removing one of several support contributions for the same
  declared home identity stays home until the last disappears; these are not
  conflicting selected Original versions. Forked worlds remain isolated. Absence and
  inaccessible storage are different observations.
- Repeated identical references and distinct overlapping roots preserve sharing,
  while removal releases only facts no remaining owner needs.
- Failed typecheck, cancellation, stale generation or failed commit cannot select
  a private successor. Successful END belongs to its original operation; P6 still
  supplies template/original-instance facts before later runtime publication.
- Cached observations cannot supply current purpose, reservations, execution,
  physical accessibility or effect completion.

For every property report generation support, observed coverage, oracle
independence and mutation sensitivity separately. Retain minimized construction
history and first complete failure diagnostics. Do not report a missing runtime
input as a passing skip.

## Resource admission before execution

No build is authorized by a planning resource snapshot. On 2026-10-19 inspection,
the host had 125 GiB RAM, approximately 95 GiB available, 3.8 GiB swap used;
`build.slice` held about 24 GiB with 80 GiB high/96 GiB maximum and a 32-CPU
quota. `/srv/build` had about 102 GiB available, near the documented unreserved
floor. These observations do not reserve headroom or prove a future command
will be admitted. The observed user slice had unlimited memory settings, so do
not claim the historical 104 GiB user ceiling protects these commands.

Reobserve memory, swap/pressure, disk reservations, running clients and slice
limits immediately before work. Use the accepted `swarm-build` launcher,
record its exit status and actual command/compiler descendant cgroups, and
inspect reused Buck/Nix/compiler daemons separately. Materialize large closures
serially and avoid competing checkout configuration. Start with one expensive
build/test process; increase only against measured process-tree peaks, actual
headroom and cohort limits. Test concurrency, compiler worker count and job
grants are distinct controls. A low-space refusal is a blocker to resolve,
not permission to bypass admission or clear shared caches.
