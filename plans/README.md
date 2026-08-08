# Plans

**Active: [`self-iterating-harness/09-askuser-form-gui.md`](self-iterating-harness/09-askuser-form-gui.md)**
— Wave 2: the `AskUser` typed-form effect (`askUser :: Form a -> M a`,
enum/int/text/bool) + a fresh minimal operator GUI (Swiss-minimal, lifts the
Datastar/SSE transport). The self-iterating harness (`render`/`loop`,
`RunLLMTurn`/`Finalize`) is the target; the older Fork/Dialog
interaction-surface plan is superseded. Exo fan-out; frozen contracts in the
plan's "Frozen contracts" section.

**Prior (self-iterating-harness Wave 1):** `01`–`08` + `W1-IMPLEMENTATION-MAP.md`
— thesis, runtime, agent surface, compaction (C1–C4), siteid, finalize
closures. Landed + green on `harness-interaction-surface`.

**Superseded:** [`harness-r0/`](harness-r0/README.md) — typed yield
(`returnControl @T`), session tree over the E2 stow engine, durable event
log, Datastar observatory skeleton. The 7-pane observatory it built is being
replaced by Wave 2's focused GUI.

The prior plan (`repo-review-2026-07-06`, a full-repo bug hunt: ~60
findings across every crate, fixed and merged to `ghci-session`) is
closed out; see git history (`git log --all -- 'plans/repo-review-2026-07-06/*'`)
for the full writeup.
