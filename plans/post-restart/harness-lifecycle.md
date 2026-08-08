# Spec: harness-lifecycle TL (respawn post-restart)

Close the confirmed lifecycle/session findings the durability wave did not
cover, and collapse the duplicate lifecycle machinery. Source: external
review 2026-08-09, every finding independently verified at HEAD; five of
seven were fixed in-flight by harness-robustness — this TL owns the residue.

## TOP ITEM, BEFORE ALL OTHERS: golden_path-on-main triage

`binary(golden_path)` fails identically on unmodified main — pre-existing,
case-trap-flavored (found by the decl-plane-scoping dev, which substituted
acceptance_cross_turn as its real-compile proof). The harness golden path
failing on base is a dogfood blocker IF the path is broken; if the TEST is
stale (exercises a pre-finalize-row shape), true it up. Reproduce,
root-cause, fix-or-reclassify — this verdict gates the dogfood launch and
outranks everything else here.

## Anti-patterns

- DO NOT re-fix what harness-robustness landed (F4 append-fails-turn,
  generation-tagged checkpoint, F6/F7 ordering, turn lease) — verify their
  mutation coverage instead if touching adjacent code.
- DO NOT keep both lifecycle machineries "for safety" — the hedged hybrid
  is the jank. One mechanism: SessionRegistry.
- A busy node returns a distinct Busy error, never NoSession.

## Scope (verified, with pre-drift anchors — re-locate, don't trust lines)

1. **F1 ghost nodes**: `retire_answerer` (selfharness/driver.rs ~802) only
   drops the NodeConvo; the NodeTree entry stays Running/Suspended forever —
   the forever loop accumulates ghost nodes every cycle. Fix: ONE
   `terminate_node` lifecycle op (terminalize tree + log event + remove
   session atomically) used by cancellation, finalization, AND retirement.
2. **F2 checkout unification**: take_session/put_session (harness.rs ~2766;
   call sites ~953/1052/1239/1546/2543) is panic/JoinError-unsafe (session
   wedged None forever) and observably None mid-turn (harness.rs ~604
   admits it). registry.rs already models Idle/Running/RunningChild/
   Suspended and is UNUSED by the live harness. Fix: store real sessions in
   SessionRegistry (or a strictly-RAII checkout guard as fallback).
   Eliminates NodeConvo.session: Option<Session>, take/put, transient
   NoSession. The turn lease harness-robustness added mitigates but does
   not remove the dual machinery.
3. **F5 residual**: fold_tree_state/FoldedTree has zero production
   consumers (tests only). Either wire the fold into startup as a read-only
   historical tree, or rename/document as offline inspection. Decide with
   the landed generation-tagged checkpoint in view.
4. **block_in_place cleanup** (Inanna-endorsed): ~8 live sites in
   selfharness/driver.rs wrapping sync OperatorGate calls + turn drives.
   Convert SelfHarnessDriver toward async; remove the bridges.

F3 (decl-plane collision) is DONE — landed separately (run-scoped
harness-sessions/<run_id>/node-<id>, mutation-checked). Strike.

## Verify

Change-class rule applies: full harness acceptance for the registry
unification (semantics-touching); mutation checks per fix (ghost-node
repro terminal-after-retire; panic-mid-turn recovers-or-Busy). Known
open-intermittents in the allowlist (see post-restart README) — anything
else red is new. Standard contention rules verbatim in every dev spec.
