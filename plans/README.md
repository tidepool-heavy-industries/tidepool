# Plans

**Read first after the exo restart:** [`post-restart/README.md`](post-restart/README.md)
— the respawn agenda, gate runbook, and per-lane specs written during the
2026-08-09 quiesce. Everything below it is context.

**Active — the self-iterating harness line** (`render`/`loop` +
`RunLLMTurn`/`Finalize`, dogfooded via `harness-dogfooding/`):

- **Landed (through 2026-08-09):** `AskUser` typed-form effect + web operator
  GUI; `Fork` as a distinct effect (now in the standard roster, tag 11);
  row-indexed `Finalize t` (shim machinery deleted, live in-heap crossing
  proven); harness durability wave (append-failure fails the turn,
  generation-tagged checkpoint, turn lease, crash-recovery acceptance);
  decl-plane run-scoping (two harnesses can no longer delete each other's
  planes); one-spawn-per-turn **Phase A** (`--turn` extract mode + Rust seam +
  equivalence corpus; Phase B spec: [`one-spawn-turn-protocol.md`](one-spawn-turn-protocol.md));
  extraction-fidelity fixes; classified differential runner (13 mutation
  probes); GC write barrier; stdlib fidelity wave (98 probes); the
  build-system cache-invalidation batch (default-deny extract fan-out cap,
  toolchain pin, cabal freeze, workspace-dep centralization, dev-profile
  opt3 for the JIT hot path); turn-latency instrumentation + stage
  attribution (extract_spawn ~70%, jit_codegen ~28%, classify 33-75ms).
- **In flight at quiesce:** harness-robustness submit (durability receipts +
  the bounded GC_POISON discriminator run); jit-chain submit (JIT-side
  latency wave 1: free-vars index, batched table ingestion, measured
  reachability ratio 6.8:1→11.1:1).
- **Findings ledger:**
  [`self-iterating-harness/10-external-review-findings.md`](self-iterating-harness/10-external-review-findings.md);
  ConTags/constructor-identity findings:
  [`self-iterating-harness/12-contags-staleness-findings.md`](self-iterating-harness/12-contags-staleness-findings.md).

**Prior (self-iterating-harness Wave 1):** `01`–`08` + `W1-IMPLEMENTATION-MAP.md`
— thesis, runtime, agent surface, compaction, siteid, finalize closures.
Landed on `harness-interaction-surface`.

**Superseded:** [`harness-r0/`](harness-r0/README.md) — typed yield
(`returnControl @T`), session tree, 7-pane observatory (replaced by the
focused operator GUI). The older Fork/Dialog interaction-surface plan is
retired outright. `repo-review-2026-07-06` is closed; see git history.
