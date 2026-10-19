# Reload implementation handoff

Contract/base: `5b74c2f1fe010c5c343e0c6f331d54328fbd5090`.
Area: `/srv/swarm/checkouts/tidepool-shared-warm-reload-20261019`,
branch `feature/shared-warm/reload`, source checkpoint
`8dca6d6a89ec430b9841b7f3f44a9c202121019c`.
Campaign owner: `/root/warm_compiler_campaign`; area owner:
`/root/warm_compiler_campaign/reload`.

## Current disposition

This is uncompiled source work, not a usable or qualified reload feature.
The campaign explicitly holds broad dependent R2/R3 force publication and hosted
migration until issuing production/test consumers compile and the focused gate
executes. Disk admission blocks those checks. No force publication, selective
drain or complete changed-reload fence is wired at this checkpoint.

The area source contains:

- Existing-directory generation metadata and synchronous gated reservation
  enqueue before asynchronous startup. Population membership follows exact
  dependency ancestry. A RunOwned generation resource keeps its independent
  cleanup lifetime through `reserve_generation_actor`.
- Worker preparation derives dependency from its actual single-use Ready
  reservation. The existing supervised producer checks the produced worker
  identity before delivery; caller omission cannot erase that relationship.
- Typed startup refusal versus unconfirmed completion. Only a producer proving
  no startup can issue NotStarted. Worker loss/uncertain lifecycle startup keeps
  the identity claim and pending cleanup, including whole-run accounting.
- A narrow force-input foundation, independent of surface cache consent, in the
  owning schema/authored helper and existing codec controls. The exact native
  std-only generator compiled and refreshed its two mirrors; the owning snapshot
  check passed. Generated Rust/Haskell consumer execution and actual force
  semantics remain pending. The matched options
  constructor deliberately changes from two fields to three.

`dc2c85879579f5fa3543f4bde3d2c3fa2dce3b2b` was independently reviewed and
required repair: generic startup errors included worker loss, so they could not
all mean NotStarted. The complete repaired candidate is `96563ebbb95438e13faba7acf3ac2663d6ed4255`;
its exact independent source review found no new blocking defect. The narrow
`804a4d3e2f` successor carries reservation provenance into the producer compiler
scope; its independent source review also found no new blocking defect. The joined
R1 review remains pending. `6978429d8f` is the
narrow force-input source parcel.
The directory/R1 consumers have no passing compilation or test evidence; the
std-only generator and snapshot check do not establish those claims. See
[the independent review](REVIEW-BASE.md) for findings and untested boundaries.

## Owners and exact continuation

Both granted descendants use `gpt-6.1-sol`, high, fresh context, started
2026-10-19 07:52 UTC. Neither has descendant capacity. The area grant remains
two descendants across all depths; do not create replacements merely because
an informed worker yields.

- `/root/warm_compiler_campaign/reload/generation_admission` is implementing R1
  in `/srv/swarm/checkouts/tidepool-shared-warm-reload-admission-20261019`,
  branch `feature/shared-warm/reload-admission`. It incorporated area `dc2c858795`
  and owns the lower runtime source gate, context/callback provenance, real
  compile/install consumers and typed child reservation callers. Awaited event:
  coherent committed R1 candidate plus exact pending checks. The area owner then
  reviews/folds it and requests cumulative independent review.
- `/root/warm_compiler_campaign/reload/authority_review` is read-only except
  `REVIEW-BASE.md`. It reviewed the complete source delta through `804a4d3e2f`
  and completed the bootstrap-world/typed-child strategy review with the settled
  reopen-only recovery contract. It is dormant awaiting the exact joined R1
  candidate and actual compile/focused-gate evidence. The area owner uses
  `followup_task` to supply that checkpoint for cumulative review, then
  repairs findings, requests exact rereview and owns the fold.

Before yielding, retain the exact awaited event, producer/observer, recipient
and continuation. A terminal producer sends exact base/candidate/evidence/status
and next action to its area owner and affected dependents: `send_message` if
running, `followup_task` if idle/completed. Completion notification alone does
not restart a dormant recipient. No periodic wake/poll loop is authorized.
No builds or external commands are running for this area.

## Accepted owner boundaries

The mandatory HLS rewrite handoff was read. Retained bytes, selected compiler
roots, exact issued dependency edges and executable permission stay distinct.
Preserve `RequestNativeAvailability::for_compiler_roles` and strict consumed
namespace validation.

Campaign accepted explicit private context provenance and lower runtime source
admission under `RuntimeLexicalScopeLease`; actor holds owner references, and
runtime never depends on actor. Checks belong at actual compilation/install/
mutation/registration consumers. A compiler machine ticket can serve only
continuation custody: rejecting every ticket would block self-reload receipt
delivery. Conversely, continuation completion can bind/compile and ready
renderers can install; those real consumers require the lower gate. Derived
scopes and transferred SourceCallback contexts must preserve original source
prerequisites separately from recipient placement.

Every actual actor capture issues a private bootstrap-world wrapper. Typed
dependent captures retain immutable external parent prerequisites; independent
root/LLM captures have no external prerequisite. The wrapper binds once to that
actor's first published generation. Its old aliases then retain the closed first
generation, while replacement generations exclude their own predecessor and
keep external prerequisites. Missing provenance cannot establish independence.

Independent cleanup worlds also have explicit lifetime ownership. A dependent
service P can own an independent ActorOwned LLM child C. Stopping P normally
stops C; force may not silently promote C to RunOwned or kill it as collateral.
Campaign accepted a typed cleanup refusal naming P and C. Before stopping P,
its actual owner must atomically fence lifetime/resource admissions and inspect
its cleanup closure; a check followed by an unfenced stop can race new C.
An explicit prior lifetime transfer permits the corresponding positive path.

Forms, WatchRoute callbacks, SourceBinding connections and ActorDisplays have
existing owners outside the run directory. Their generation-filtered inspect,
stop and physical settlement paths remain joined R2 obligations. Closing an
entire invocation/actor to substitute for selective cleanup is too broad.
A Closing/no-executor lifecycle snapshot is not terminal cleanup proof: a
cleanup executor can attach later. Retain uncertainty until actual settlement.

Bootstrap contexts issued before first generation A currently retain an unbound
independent-world alias. That alias must not continue admitting fresh source work
after A retires. Campaign accepted one-time binding on the same captured world
owner: begin a Publishing/source-admission fence at the real first publication
claim before IO can become uncertain; bind once to first actually visible A;
old aliases forever consult A. Only confirmed nonpublication/no-effects may
reopen an unbound world. Unknown publication remains unavailable even when the
visibility callback never ran. Candidate preparation must not fill the binding.
B retains external parent prerequisites only, excluding its retired bootstrap A.
The R1 leaf implements this closure; the independent high reviewer checks actual
atomicity and initial failure/uncertainty plus typed-child histories. An unbound
world must retain real admission custody across the first-publication cutoff;
returning None after a state precheck creates a race.

Initial publication uncertainty must retain private candidate facts distinctly
from a selected ActorGeneration: binding an owner with no runtime readiness
would falsely grant current authority. The existing durable store remains fenced
after unknown publication and has no live reconciliation API. Campaign selected
the existing explicit host/run reopen recovery path. Retry on the same actor and
store refuses; old bootstrap aliases stay permanently closed. Reopening selects
the actual durable head and issues a new recovered context from its real selected
references and readiness. No live reconciliation mechanism, fresh gate from an
old generation integer, or prospective generation bound without readiness is
part of this repair. The leaf is implementing this settled contract.

## Verification admission and pending commands

The area buck-out bind mount is provisioned and was verified; account is
`inanna`. Observed `/srv/build` available space was 102 GiB with a required
100 GiB unreserved floor, insufficient for a cold 32 GiB lane. Host memory was
97 GiB available. No build bypass, cache deletion, broad cleanup or shared
compiler/daemon restart is authorized. Native test configuration/materialization is deferred. The already-realized
pinned development shell supports light formatting and Cargo source metadata
without native compilation or a Buck daemon.

Pinned rustfmt check and the source generator plus `--check` passed for the
current area source; `exomonad/actor/BUCK` now declares the directory module.
Required workspace submodule `ce60f76888c94e9d7229615f27762d02933549b1` is
initialized from the verified local campaign repository. Receipts are under
`/srv/swarm/evidence/inanna/shared-warm-compiler-20261019/reload/` in
`formatting.json`, `source-generation.log` and `source-generation-check.log`.
The campaign admitted a separate 128 MiB std-only protocol-generator lane.
Default pinned configuration, `just build -j1 //bridge/protocol:tidepool_protocol_gen`,
exact generator refresh and `just build -j1 //bridge/protocol:snapshots_check`
all exited 0. The native artifact SHA-256 is
`143f28f99a457a86aad2bbb52269a8f432bd95670bbe1d84b07e7f6911a047f4`,
6,598,656 bytes. The generator build ran four local cold actions; the snapshot
check ran three local actions. Zero tests executed. Receipt and logs:
`protocol-generator-receipt.json` in the evidence directory above. The generated
mirrors are committed, with no hand edits. The joined R1 modules still need
owning source generation after integration.

Own buck-out final allocated bytes were 56,938,496 (initially zero); lowest
observed global available bytes were 107,887,243,264, above the 100 GiB floor.
Scratch peak and shared-cache attribution were not instrumented. Buck daemon
PID 3835600 is retained idle in `/build.slice/inanna-build-1792398682-3835490.scope`;
its enclosing observed MemoryPeak was 836,239,360 bytes. No commands are running.
The serial native lane was released. Root owns new storage cleanup. Its first
settled retirement reports 105.80 GiB real free; warm-workers owns the next
serial lane. This is not admission for a larger reload graph. The R1 leaf owns
the next source-candidate event; campaign/root owns the subsequent native-lane
admission event. No reload command is running or awaiting terminal collection.

After the coherent R1 candidate is joined and source maps are regenerated, the
smallest first compilation stage is the runtime and actor production libraries
plus their unit harnesses. Libraries are named explicitly to retain production
and cfg(test) evidence separately. Host production and unit consumers follow
with browser configuration before describing the parcel as integration-ready:

```sh
swarm-build bash scripts/buck2-configure.sh --tests
swarm-build just build -j1 //tidepool/runtime:tidepool_runtime //exomonad/actor:exomonad_actor //tidepool/runtime:tidepool_runtime_unit_tests //exomonad/actor:exomonad_actor_unit_tests
swarm-build bash scripts/buck2-configure.sh --tests --browser
swarm-build just build -j1 //exomonad/host:exomonad_host //exomonad/host:exomonad_host_unit_tests
swarm-build just build -j1 //tidepool/bridge-effects:tidepool_bridge_effects_unit_tests //bridge/mcp:tidepool_mcp_unit_tests //bridge/mcp:mcp
swarm-build just test-lib tidepool-bridge-effects --exact tests::reload_options_and_typed_receipts_roundtrip_and_reject_wrong_arity --expected-count 1
swarm-build just test-native //exomonad/actor:actor_notebook_contract_tests
```

Discover the actual generated-surface integration selector before executing
its ReloadCodec/ReloadSync consumers. New starter-loss controls must enter the
real supervised producer and show uncertainty cannot release identity or let
whole-run preparation accounting confirm. A lost producer's rendered error is
not evidence of no startup. The independent history/control design is in
REVIEW-BASE.md; these tests are pending, not executed.

The hosted old-A late-new-call success assertion must migrate only with the
joined gate and refusal controls. Preserve immutable source/prompt observation,
old compiled pure continuation, independent IO and exact historical dependencies.
Retain source/artifact/profile identity, nonzero selected/executed counts,
exit/logs, compiler END and lifecycle cleanup reports. Current verification is
source review, pinned formatting, source metadata, the std-only protocol
generator build, snapshot check and `git diff --check`. No R1/actor/runtime/
Haskell consumer compilation or behavioral acceptance exists.
