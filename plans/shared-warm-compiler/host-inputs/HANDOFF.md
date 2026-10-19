# Host inputs current handoff

Owner: `/root/warm_compiler_campaign/host_inputs`. Isolated checkout
`/srv/swarm/checkouts/tidepool-shared-warm-host-inputs-20261019`, branch
`feature/shared-warm/host-inputs`. Original base `5b74c2f1`.

## Qualified checkpoint

`9f086d0d23901115260caf37dd908e095f671cfa` joins the accepted demand-service
baseline with complete counter history through `8424c144` and runtime fact-key
history through `70120cc3`. These are independently source reviewed. Supervisor
read the critical production changes and actual execution reports.

- Runtime fact key: production/cfg-test compilation passed, two exact controls
  selected/executed/passed, including same-tip binding/catalog invalidation,
  frozen child isolation and overflow refusal. No full-view construction or
  semantic hashing on repeated key reads; ordinary full-view path calibrates it.
- Counter producers: toolchain production/cfg-test compilation passed at `9f086d`;
  two exact controls selected/executed/passed. Explicit thread/unwind propagation,
  failed manifest write after successful encoding, FD export and empty retained
  value fold were exercised. Test executable SHA256
  `1ac34d45180652b3861cd3d065b2108129ddd9c539a4c3ef27e9e0ce8391d6ea`.

Key evidence: `/srv/swarm/evidence/inanna/shared-warm-compiler-20261019/host-inputs/host-world/commands.json`
and `key-counted-summary.json`. Counter commands, exact reports and limits:
`/srv/swarm/evidence/inanna/shared-warm-host-inputs-20261019/counters-8424/`.
Independent receipts are under that host evidence root's `oracle/` directory.
Neither parcel establishes an implemented service query, worker hit or speedup.

All owning native clients are terminal. Counter scope peak was 5,269,278,720 B;
its daemon remains idle in `build.slice` (PID 4122402, scope
`inanna-build-1792405723-4122308.scope`). The six-GiB lane was released to the
campaign. New native work requires fresh aggregate admission. Buck output remains
an owned per-checkout XFS bind mount. Installed storage-status remains unsupported.

## Current source-only successor

`0aa94eca24326f5feb53bf1c84333ef3e9592426` adds actual source-owner capture in
actor mount forwarding and host source/generation/workspace. It uses the existing
RetainedSourceGraph and traversal, hashes and retains bodies from the same read,
retains root/prepared/catalog owners, and caches the completed cold data on that
immutable graph. No new source issuer or scanner is introduced. Missing or
interrupted observation maps to existing Unavailable; unreadable/mismatched data
remains Unreadable. Read counters charge successful reads before cancellation.
Fresh ambient catalog or uncovered catalog home roots return unsupported.

The actual owner control covers unimported source A -> B, captured absence then
creation, cold restore A and last-owner release. It has not compiled or executed.
Pinned Rust 1.93 formatting and diff check passed. Independent source receipt:
`oracle/source-capture-review-e9c96d86.txt`, four-path diff SHA256
`e9c96d86de6aa785ab4d3ae960a41dbd2331c1ede81a636ef1d9294b2825ace9`.
Warm's real CapturedCompilerSources factory was read as mutable source; its exact
committed candidate is still a dependency. Do not fold this source-only parcel
as qualified. Two CLI preflight migrations remain separate uncommitted WIP,
depending on owned API `00744ee1` and warm's actual identity association.

## Accepted next boundary

The persistent logical world is the normal production compiler boundary. The
first actual PublicModule info/type query precedes capture_value_interfaces,
checked_value_context, structured provenance, source preparation, hashing and FD
export. Full cell, paired publication and recovery migration remains required;
the first query is the delivery sequence, not a final optional query cache.

C1 correction: cold issuance also omits checked_value_context and all retained
value support merging. Actual current declaration/source facts and published
binding-row owners/visibility are separate inputs. Historical native/support
references remain exact custody and do not become current source roles. Rust
retains runtime provenance, native edges, instance/prototype proofs and source
admission; Haskell owns compiler world selection and demand-specific resolution.
Strict native required edges remain validated at actual consuming operations.

First source policy supports captured non-CPP/nonexternal-preprocessor modules
and a fixed package profile. Warm checks the whole actually demanded closure,
including module-local options. Old graph-v2 does not authenticate a new
header-only revision. Captured bytes may retain actual A; they cannot bless B.
Unsupported after accepted submission is typed refusal, never old-route replay.
Lookup hydration does not advance the logical world or notebook publication.

Warm owns compiler_world and the raw private cold builder, all protocol/Haskell;
host owns actual runtime/actor query routing, source capture, work observations
and a generated execution group. Only that group binds the rooted service
resource with existing owned-resident mode; generic finite tests stay unchanged.
Owned owns deployment/package producers. Reload's original-source transaction
owner is an unqualified dependency before accepted production query submission.

## Grants and continuation

Two descendants remain reserved: selected_root and selection_oracle, both Sol
high. Implementation checkout is `tidepool-shared-warm-host-world-20261019`, clean
at tested `70120cc3`; it awaits actual warm API/source/counter/admission dependencies
before resuming the actor query and subsequent publication migration. Oracle is
settled after source capture review and will review the complete joined candidate.
No grandchildren or extra descendants are authorized.

Paused 02A is untouched in the older selected-root checkout at `b5ebb643`. Exact
incomplete patch: `/srv/swarm/evidence/inanna/shared-warm-compiler-20261019/host-inputs/selected-root/02a/paused-wip.patch`,
SHA256 `264bee547c2f497e44e69f29bd1bd76e66bf957c02a0650d562b439c972d5f1d`.
It has no build/test acceptance and must not be revived under the new boundary.

No periodic polling. Producers notify a running owner with send_message and
reactivate a dormant owner with followup_task, carrying exact source, evidence
and next action. Every command keeps an observer through its terminal result.
Current wake dependencies: warm committed source/client carrier, owned rooted
resource/API, reload reviewed source transaction owner, then admitted host native
consumer gate and the actual cold/hot actor query controls.
