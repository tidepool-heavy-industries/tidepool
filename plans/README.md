# Plans

This directory describes the current Tidepool work. Completed plans, design
history, experiment receipts, and superseded handoffs are intentionally kept
in git history rather than here.

## Active work

- [Turn latency: state injection + compile incrementalism](turn-latency-state-injection.md):
  **approved (2026-08-20), standing go-ahead for incremental compile
  improvements toward eventual daemon mode.** ~67% of a companion turn is the
  fused outer compile, cold every turn because state rides the source text.
  Sequence: module-level attribution (in flight) → state injection via a
  stable-name inject-val (memo learns that one shape) → persistent shared
  build-products dir if attribution shows unchanged-module recompilation
  dominates → resident extract daemon as the eventual form (readiness rules
  in the plan).
- [The Effect Protocol](self-iterating-harness/22-effect-protocol-prd.md):
  **proposed (2026-08-17), approved direction.** One data-only schema crate
  generating every artifact the effect contract currently maintains by hand
  in four registries (macro DSL strings, positional wire mirrors, extractor
  verb tables, harness constructor-name classification) — byte-compatible
  golden migration, one effect at a time, smallest first. Prerequisite
  compile-pipeline consolidation in flight.
- [Recursive Companion](self-iterating-harness/21-recursive-companion-prd.md):
  **proposed (2026-08-17).** The companion track's successor: reasoning as a
  recursively discovered typed program — one model invocation per node as the
  coalgebra (finish locally or define one layer of branches), children forked
  from the frozen post-coalgebra context, a second invocation as the algebra
  folding typed results. Locked dataflow: prompt text for meaning, Haskell
  values for behavior/identity/authority/composition — briefs render down,
  closures ride up as live artifacts applied by the caller under receipts,
  and the algebra's window gets a rendered view by default with the real
  value mounted as an invocation-local binding when higher-order access
  earns it. Lanes C0–C6; C1 (mounting a function-bearing value into a
  window) is the de-risk spike.
- [Exomonad v3 — the typed swarm](self-iterating-harness/20-exomonad-v3-prd.md):
  **proposed (2026-08-15).** The full-scope successor to exomonad: swarm
  coordination as a compiled resident Haskell program — a monadic
  hylomorphism whose algebra and coalgebra are agents, with policies as
  middleware over the two seams; concurrent agent cycles, one event algebra,
  green threads over parked continuations, node residents with typed
  mailboxes over lexically scoped handles, git-as-persistence with a
  journaled resume and eager rebase cascades, the trust ladder + fold
  receipts, and the Stage-2 resident factory. Stage-1 lanes S1-L1…L6 are
  LANDED (green threads + capability mailboxes last, 2026-08-17); dev-tree is
  the executable design target. Stage-2 is next.
- [One session](one-session.md): **Phases 0–5 LANDED (2026-08-12)**; Phase 6
  (repl/one-shot conversion + slot deletion) is deliberately parked behind
  the production-soak gate. The self-harness runs collapsed on one resident
  session: answerer nodes are realms on the outer machine, finalize closures
  are delivered by `ValueHandle` into the loop's parked continuation, and
  `runLLMTurn @(State -> State)` works end to end (standing acceptance:
  `tidepool-harness/tests/selfharness_fn_finalize_spike.rs`; the companion
  dogfood finalizes edits). Machine lifetime is ceiling-bounded with
  CI-exercised rotation. Substrate contract (amended):
  [the parking contract](post-restart/realm-lanes/continuation-parking-contract.md).
- [One-spawn turn protocol](one-spawn-turn-protocol.md) + [Phase B
  contract](one-spawn-turn-protocol-phase-b.md): LANDED, both phases. One
  extract spawn per turn on every caller; `--emit-stmt-binders` and
  `--emit-binders` deleted (a WIRE BREAK — `scripts/redeploy.sh` must ship
  extract and servers together); the extract-side `classify` phase live and
  `classify_extract` retired. Read the Phase B contract for the classify
  lane's surviving per-BLOCK shape.
- [GHCi affordances](ghci-affordances-todo.md): deferred `:t` and multi-item
  turn support, revisited after Phase B.
- [Post-restart execution](post-restart/): the current implementation lanes,
  gates, and benchmark track.
- [Extract-side latency wave](post-restart/extract-wave.md): folded.
  `extract-wave/OPERATIONAL.md` is the wave's operating doctrine — read it by
  ref, never from a worktree copy. Two standing hazards it left unfixed:
  `haskell_suite_differential` and `corpus_report` **never invoke the
  extractor** and cannot gate extractor changes; the "pinned id-stability"
  trio is three DataConId guards observing no VarIds. Wave 3 (render+loop
  fusion) and D2 are cut and routed forward ready-to-spawn, D2 with a
  hand-off at `extract-wave/spawn-latency/03-d2-handoff.md`.
- [Generic askUser PRD](self-iterating-harness/14-generic-derived-askuser-prd.md)
  and [generic surface wave](self-iterating-harness/15-generic-surface-wave.md):
  the current typed interaction surface.
- [Companion memory](companion-memory.md): agent-curated memory store (a git
  repo of markdown the PRD 18 curator agent edits) + the `[Directive]`
  finalize contract — the first LIVE subagent exercise, and the outer-row
  work dev-tree is blocked on.

The post-restart directory contains the operational source of truth for work
in flight. The numbered self-iterating-harness documents above are the current
forward-facing design documents; no chronology is implied by their numbers.

## Reference

- [Decision archive](decision-archive/README.md): the narrow exception to
  "inline doc-history is deleted, git is the store" — backstory whose loss
  would invite re-tripping a hazard already fixed once. Current architecture
  contracts are NOT here; they stay in each `CLAUDE.md` (root's Key Decisions
  Reference is authoritative and verbatim).
