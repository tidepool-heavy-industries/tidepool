# Harness adoption: current reconciliation and experimental gates

2026-09-27. Companion to `harness-adoption.md`; the original plan remains
unchanged. This document records current evidence and proposed integration gates,
not a claim that the adapter exists or is accepted.

## Authority and source baseline

Inanna authorized reconciliation now, then a narrow vertical slice and a small
worker tree. Production waves remain on Codex until a later explicit migration
decision. Preserve wave19 while it completes acceptance and its closing interview.

Initial inspection: Tidepool `96cdcae4a6bf5dbd9a1c29b6bfdb84adbdfddd20`;
wave19 harness `6aab0df5039118f122ecf9a5f2278828a831716f`. The live harness has
ongoing component work: implementation candidates are not an integrated baseline.
Freeze an accepted revision before integration tests.

The standalone harness is `/home/inanna/dev/exomonad-harness` and its run
checkouts. Tidepool's `exomonad/harness` is historical, excluded source according
to `bridge/facade/CLAUDE.md`; its old guide does not establish a supported adapter.

## Initial source reconciliation

| Old proposal | Current evidence | Consequence for the experiment |
|---|---|---|
| Freeform async Haskell cell | Standalone `crates/harness/src/cell_job.rs` exposes `CellJob::run`, `CellJobProvider`, and a function tool with a JSON `source` field | Reuse the evaluator/job seam; explicitly resolve the freeform presentation path before claiming the intended model-facing API |
| Cancellation yields a typed outcome | The same module requires evaluator work to stop when its future is dropped; wave19 is changing progress/cancellation | Verify resident execution cancellation and resource release separately from cancelling the harness future |
| Replace `InteractiveAgentBackend` | Current `exomonad/agent/src/interactive.rs` includes bound input submission, query, withdrawal and producer control | Inventory the behaviors required by production consumers; avoid implementing obsolete process protocol merely to fit the old trait |
| Offline readiness precedes integration | Standalone `tests/adapter_readiness.rs` drives production Engine/Store with `FakeResidentCell` and `ReplayTransport` | Retain this deterministic test foundation, then substitute real resident execution; fake-cell evidence is not live-provider evidence |
| Delete watch/wake and related mechanisms | Current authored programs use retained responses, watches and non-model actors | Decide each deletion from its consumers after the replacement flow works; asynchronous provider calls alone do not prove all those semantics redundant |

### Findings from the production-consumer audits

- `InteractiveAgentBackend` (`exomonad/agent/src/interactive.rs:481`) is a
  process/input-control boundary, including rollout observation and queue-ready
  threads. An in-process model driver should not emulate tmux and socket control
  simply to implement that trait. Start with a separate experimental composition
  path and reuse the actual actor workbench boundary.
- `exomonad/actor/src/resident_actor.rs` owns workbench admission and execution;
  `resident_workbench.rs` owns compilation against a captured source view.
  `mount.rs` binds exact actor placement/source, and the runtime session registry
  owns fenced machine checkout. Preserve these owners in the first experiment.
  Cancelling compilation, cancelling execution and settling a model call are
  distinct obligations; a dropped adapter future is insufficient evidence.
- `CellJobProvider` currently has test consumers, including the offline readiness
  test, but the audit found no production consumer. The readiness test exercises
  a pending fake cell across three boundary envelopes, finalization and durable
  reopening. It does not exercise GHC or live provider traffic.
- The standalone canonical `Provider` has no proposed `JobVerbs` interface.
  Agent verbs pass through `call_agent_verb`; the concrete tree driver lives in
  `crates/harness-demo/src/tree.rs`. Its provider refuses inherited/checkpoint
  starts, and the CLI browser-serving path does not expose tree mode. These are
  explicit small-worker-tree gaps, not details an adapter may silently fill in.
- The old plan's wholesale actor/mailbox deletion and shared subtree checkout
  proposals are architectural migrations. Current exact-incarnation identities,
  typed requests, source isolation and owned machine handles have real consumers.
  Decide their eventual ownership separately; retain them for the first slice.

The audits inspected source without running tests. The canonical harness and
live wave19 checkout differ; bounded progress/cancellation candidates remain
pending integration. Recheck these seams on the frozen post-wave19 revision.

## Experimental progression

1. **Wave19 acceptance.** Retain exact component reviews, combined checks,
   production browser journey, recovery evidence and kaizen interview. Record
   incomplete work explicitly if a stop boundary is reached.
2. **One real resident cell, deterministic transport.** Drive the standalone
   Engine through the existing job path into the actual Exomonad resident
   workbench. Prove binding persistence, delayed completion on the original call,
   intervening input, execution failure, cancellation and resource release.
   Reuse runtime/session owners; do not build another cell scheduler or registry.
3. **One interactive root.** Exercise browser input through model continuation
   and real Haskell execution. Confirm freeform tool presentation and provider
   ordering against the actual request path. Record what remains untested live;
   do not infer provider behavior from replay.
4. **Small worker tree.** Exercise one root with independent bounded children,
   retained typed results, source inheritance, steering, review and cancellation.
   Choose one authoritative owner for actor identity and each lifecycle transition
   before wiring harness verbs to Exomonad effects.
5. **Migration decision later.** No production default change or removal of the
   functioning Codex path is implied by these experiments.

## Structural analysis for every issue

Each issue or proposed improvement carries:

- The model's intended operation and evidence for that intent.
- Its available instructions, interfaces, source and authority at that moment.
- The actual sequence across model, Haskell, runtime and child actors, including
  prerequisites, retained handles, ownership and the event that could resume it.
- The observed divergence and its downstream cost; separate causal hypotheses.
- Alternative flows, including deleting work, preparing it earlier, changing
  ownership, or retaining a continuation instead of asking the model again.
- The selected intervention, real consumer, failure case and next-wave check.

Current wave19 cases: incoherent historical tooling layers; scaffold admission
before required contracts; reminders settling legitimate waiting; cross-owner
`CallContext` migration; unavailable browser test tooling; recovery continuation;
review scope and missing application wiring. Analyze successes too: independent
review found concrete defects, and reports preserved unexecuted checks as unknown.

This applies to adapter design as well: start from what an actor needs to do and
trace the end-to-end flow before deciding which existing interface to adapt.

## Shared harness memory hypothesis — 2026-09-28

Inanna's architectural target is **one shared exomonad-harness instance for all
agents**, multiplexing actors instead of launching a harness per actor or per
component tree. Actor identities, authority and lifecycles remain distinct
inside that host, replacing the swarm of native client processes. Wave21 retained 37 native
clients with roughly 18.1 GiB combined PSS plus SwapPSS in the sampled footprint.
This motivates the migration, but is not a measured saving from a replacement.

A shared executable launched separately for every actor still duplicates private
runtime allocations. The useful design shares process infrastructure, provider
clients and immutable context prefixes where semantics permit; each actor keeps
its own authority, cancellation, conversation suffix and lifecycle. Completed
actors must release those private allocations. One process also concentrates
failure impact, so preserve actor supervision and bounded retained evidence.

Use the next run's external resource observations as a baseline: PSS plus SwapPSS
by client/host/compiler/build, cgroup current and pressure, live versus retired
actors, and release after local integration. Later compare the same useful work
and concurrency through the standalone harness. Compiler/JIT memory remains a
separate owner and does not disappear by sharing the model driver. This is future
architecture guidance, not an expansion of the current three-change batch.
