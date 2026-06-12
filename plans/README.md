# Plans

Queued cleanup work. Each plan is one worktree.

| Plan | Focus |
|------|-------|
| [doc-pass](doc-pass.md) | Doc comments on `pub` items across library crates |
| [error-consolidation](error-consolidation.md) | `thiserror` derives + cut 72 `.expect()` calls |

## Done

| Plan | Focus | Verdict |
|------|-------|---------|
| [mcp-hardening](mcp-hardening.md) | Orphan eval thread cleanup + residual `.lock().unwrap()` | CONFIRMED-FIXED by `ff07cdd` (#269) + `97c6108` (pause-gate). Verified 2026-06-11 — see plan for evidence. |
