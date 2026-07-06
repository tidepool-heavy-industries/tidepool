# Plans

Queued cleanup work. Each plan is one worktree.

| Plan | Focus |
|------|-------|
| [future-plans](future-plans.md) | Idea backlog (effect-log resume, content-addressed Core, `compile_to_callable`, heap verifier, …) |
| [llm-continuation-patterns](llm-continuation-patterns.md) | Design catalog: `ask`/`llm` continuation patterns (aperture, interview, escalator, tribunal, …) |
| [haskell-interface-polish](haskell-interface-polish.md) | GHCi-parity surface checklist (`it` in progress; `generic-lens`/`validation`/echo-type open) |

## Done

_Completed plans are deleted once landed — git holds the record. Most recent: a batch of shipped/superseded plans (ghci implementation + session persistence, diagnostics-flow recon, workflow-parity, stack-safety, qq-horizon, code-health-method, the repo-tools/ tool-framework spike) and the FIXED proptest-findings docs (cache, freer-queue, ghc-idioms, heap-layout, host-arrays) — their bugs now live as regression tests, and standing limits as `STANDING:` probe tests (e.g. `cycle` in `haskell_verified/error_family.rs`), never as prose. doc-pass (shipped `8b271a95`). mcp-hardening (orphan eval-thread cleanup + `.lock().unwrap()`), CONFIRMED-FIXED by `ff07cdd` (#269) + `97c6108` (pause-gate)._
