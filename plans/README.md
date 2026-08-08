# Plans

**Active — the self-iterating harness line** (`render`/`loop` +
`RunLLMTurn`/`Finalize`, dogfooded via `harness-dogfooding/`):

- **Landed:** `AskUser` typed-form effect + minimal web operator GUI
  ([`self-iterating-harness/09-askuser-form-gui.md`](self-iterating-harness/09-askuser-form-gui.md));
  `Fork` as a distinct effect with a fork-free leaf row; finalize pinned to
  the hole's answer type (shim mechanism, being replaced by the row-indexed
  `Finalize t` — in flight).
- **In flight (exo swarm):** row-indexed `Finalize t`; harness durability
  (lifecycle/checkpoint/turn-lease + crash recovery); turn-latency
  instrumentation + report; one-spawn-per-turn (extract classifies raw turn
  text in-session, returns one rich result; removes `--emit-stmt-binders`/
  `--emit-binders`); extraction-fidelity fixes (Translate/GhcPipeline);
  classified differential runner + lane consolidation; GC write-barrier +
  soundness hardening; stdlib semantic-fidelity fixes.
- **Findings ledger:**
  [`self-iterating-harness/10-external-review-findings.md`](self-iterating-harness/10-external-review-findings.md)
  — status of externally-reviewed defects; stays active until the durability
  wave folds its rows.

**Prior (self-iterating-harness Wave 1):** `01`–`08` + `W1-IMPLEMENTATION-MAP.md`
— thesis, runtime, agent surface, compaction, siteid, finalize closures.
Landed on `harness-interaction-surface`.

**Superseded:** [`harness-r0/`](harness-r0/README.md) — typed yield
(`returnControl @T`), session tree, 7-pane observatory (replaced by the
focused operator GUI). The older Fork/Dialog interaction-surface plan is
retired outright. `repo-review-2026-07-06` is closed; see git history.
