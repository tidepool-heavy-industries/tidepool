# Reload implementation handoff

Contract/base: `5b74c2f1fe010c5c343e0c6f331d54328fbd5090`.
Area: `/srv/swarm/checkouts/tidepool-shared-warm-reload-20261019`,
branch `feature/shared-warm/reload`, source checkpoint
`804a4d3e2f253f69ef81ca76eacc6229cfe7deea`.
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
  owning schema/authored helper and existing codec controls. Generator refresh,
  compilation and actual force semantics remain pending. The matched options
  constructor deliberately changes from two fields to three.

`dc2c85879579f5fa3543f4bde3d2c3fa2dce3b2b` was independently reviewed and
required repair: generic startup errors included worker loss, so they could not
all mean NotStarted. The complete repaired candidate is `96563ebbb95438e13faba7acf3ac2663d6ed4255`;
its exact independent source review found no new blocking defect. The narrow
`804a4d3e2f` successor carries reservation provenance into the producer compiler
scope; its independent source review also found no new blocking defect. The joined
R1 review remains pending. `6978429d8f` is the
narrow force-input source parcel.
Neither source parcel has passing compilation or test evidence. See
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
  and is dormant with its grant reserved. Awaited event: the area owner
  `followup_task` supplies the exact joined R1 candidate and actual compile/
  focused-gate evidence, or a concrete source exception requiring review.
  The area owner repairs findings, requests exact rereview and owns the fold.

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

Typed dependent children retain parent prerequisites plus their own generation;
a replacement excludes its own retired predecessor while keeping inherited
prerequisites. Independent LLM captures receive a new world at their actual
issuer. Missing provenance cannot establish independence.

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

## Verification admission and pending commands

The area buck-out bind mount is provisioned and was verified; account is
`inanna`. Observed `/srv/build` available space was 102 GiB with a required
100 GiB unreserved floor, insufficient for a cold 32 GiB lane. Host memory was
97 GiB available. No build bypass, cache deletion, broad cleanup or shared
compiler/daemon restart is authorized. Configuration/materialization is deferred.

Once storage admission permits, use this checkout's pinned tools and regenerate
new Rust source mappings through `scripts/buck2-first-party.py`. Build the exact
protocol generator, refresh its tracked mirrors through
`scripts/protocol-snapshots.py --generator EXACT_BUILT_GENERATOR --refresh`,
then check the mirrors. Generated files must not be hand edited.

```sh
swarm-build bash scripts/buck2-configure.sh --tests --browser
swarm-build just build //bridge/protocol:tidepool_protocol_gen
swarm-build just build //bridge/protocol:snapshots_check //tidepool/bridge-effects:tidepool_bridge_effects_unit_tests //bridge/mcp:tidepool_mcp_unit_tests //bridge/mcp:mcp
swarm-build just build //exomonad/actor:exomonad_actor_unit_tests //exomonad/host:exomonad_host //exomonad/host:exomonad_host_unit_tests //tidepool/runtime:tidepool_runtime_unit_tests
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
source review and `git diff --check` only; no compilation/test acceptance exists.
