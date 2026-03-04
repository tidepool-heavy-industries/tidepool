# Pre-Release Checklist: Orchestration Guide

## What this is

7 independent fixes for janky resource/memory patterns in the tidepool codebase. Each can be implemented in its own git worktree and merged independently. They don't conflict — each touches different files/functions.

## How to execute

Spawn 7 worktree agents in parallel, one per plan file. Each plan is fully self-contained with exact file paths, line numbers, code snippets, and verification steps.

| Plan | Branch name | Key file(s) | Independence |
|------|------------|-------------|--------------|
| `plan-01-orphaned-threads.md` | fix/orphaned-eval-threads | `tidepool-mcp/src/lib.rs` | Independent |
| `plan-02-cap-exec-output.md` | fix/cap-exec-output | `tidepool/src/main.rs` | Independent |
| `plan-03-signal-closure-leak.md` | fix/signal-closure-leak | `tidepool-codegen/src/signal_safety.rs` | Independent |
| `plan-04-emit-panic-to-result.md` | fix/emit-panic-to-result | `tidepool-codegen/src/emit/expr.rs`, `emit/mod.rs` | Independent |
| `plan-05-mutex-poisoning.md` | fix/mutex-poisoning | `tidepool-mcp/Cargo.toml`, `tidepool-mcp/src/lib.rs`, `tidepool-eval/Cargo.toml`, `tidepool-eval/src/eval.rs` | Independent |
| `plan-06-letrec-alloc-hints.md` | fix/letrec-alloc-hints | `tidepool-codegen/src/emit/expr.rs` | Conflicts with plan-04 (same file, different functions) — merge plan-04 first |
| `plan-07-deep-force-iterative.md` | fix/deep-force-iterative | `tidepool-eval/src/eval.rs` | Independent |

## Conflict note

Plans 04 and 06 both touch `tidepool-codegen/src/emit/expr.rs` but at different line ranges (04: lines 450, 1318; 06: lines 735-1308). They should merge cleanly but if not, merge 04 first since it's the smaller change.

## Verification after all merges

```bash
cargo test --workspace
```

All ~1351 tests should pass. No new test files needed — these are all internal safety improvements.

## Commit style

Each fix gets its own commit with prefix `fix:` and a message explaining the resource/safety issue it addresses. Example:
```
fix: cap Run/RunIn exec output to 2MB to prevent OOM
```
