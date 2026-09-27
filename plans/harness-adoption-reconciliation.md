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

Two source audits are checking production consumers and standalone seams. Their
findings must resolve ownership and admission details before implementation.

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
