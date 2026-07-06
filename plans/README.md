# Plans

Queued cleanup work. Each plan is one worktree.

| Plan | Focus |
|------|-------|
| [doc-pass](doc-pass.md) | Doc comments on `pub` items across library crates — doc-comment counts across repr/eval/optimize/effect/runtime now far exceed the plan's baseline table (e.g. repr 94→399, runtime 78→809); looks shipped by `8b271a95`, left for root to confirm + delete |
| [error-consolidation](error-consolidation.md) | `thiserror` derives + cut 72 `.expect()` calls — queued |
| [future-plans](future-plans.md) | Idea backlog (effect-log resume, content-addressed Core, `compile_to_callable`, heap verifier, …) |
| [llm-continuation-patterns](llm-continuation-patterns.md) | Design catalog: `ask`/`llm` continuation patterns (aperture, interview, escalator, tribunal, …) |

## Done

_Completed plans are deleted once landed — git holds the record. Most recent: a batch of shipped/superseded plans (ghci implementation + session persistence, diagnostics-flow recon, workflow-parity, stack-safety, qq-horizon, code-health-method, the repo-tools/ tool-framework spike) and the FIXED proptest-findings docs (cache, freer-queue, ghc-idioms, heap-layout, host-arrays) — their outstanding facts now live as Known Limits in `haskell/CLAUDE.md` or as regression tests. mcp-hardening (orphan eval-thread cleanup + `.lock().unwrap()`), CONFIRMED-FIXED by `ff07cdd` (#269) + `97c6108` (pause-gate)._
