# Code Cleanup — action plan

Action companion to `code-health.md` (findings) + `code-health-method.md` (the
tidepool patterns). Dogfoods tidepool: every item is *located/verified* via a
tidepool eval, then edited in Rust and gated on `cargo build`/`clippy`/`test`.

## Scope decision (triage of the 4 finding categories)

- **Duplication → DO.** Real, low-risk DRY wins. This is the whole scope below.
- **Long functions → SKIP.** `emit_primop` (1968) / `dispatch_primop` (1672) are
  one-arm-per-primop match dispatch — the match *is* the structure; splitting
  trades clarity for indirection. The mid-size ones (`build_preamble`,
  `emit_letrec_phases`, `run`) are coherent phase machines. No action.
- **Panic family (650) → MOSTLY SKIP.** Most are invariant guards in codegen
  that *should* panic loudly (a compiler-bug invariant must be loud — CLAUDE.md).
  The genuinely-recoverable few are already scoped in `error-consolidation.md`.
  Not re-litigated here.
- **Cross-crate name collisions → SKIP.** `BridgeError`/`HeapError`/`RuntimeError`
  are intentional per-crate types (already noted in memory). Cosmetic.

## Duplication work-list (the 7 "extractable" pairs)

Each: tidepool eval to confirm the blocks are *actually* identical (not just
similar) → extract a named helper → `cargo build`/`clippy`/`test`. A pair that
turns out non-trivially different on inspection is SKIPPED with a note (like the
report's `oracle.rs` pair).

- [x] **1. `runtime_case_trap` sig** — DONE. primop.rs ×2 + **case.rs** (tidepool
  found a 3rd site the report missed); 100% identical ABI sig. →
  `runtime_case_trap_sig` in `emit/mod.rs`. GREEN, committed `1465027`.
- [x] **7. `heap_force` sig** — DONE. The report saw 2 (expr.rs); tidepool found
  **5** (case.rs, expr.rs ×3, primop.rs), all the same `(vmctx,obj)->I64` sig. →
  `heap_force_sig` in `emit/mod.rs`; trimmed case.rs's now-unused import. GREEN.
- [ ] **5. `resolve_hs_path`** — TODO (real). `tidepool-macro/src/expand.rs`
  `expand_hs`/`expand_expr_hs` share the `::binding`-suffix + manifest-dir path
  prologue (pure string logic). Extract a shared `resolve_hs_path`.
- ~~2. from_value codegen~~ — SKIP. bridge-derive `codegen.rs` — two *different*
  `quote!` generators (multi-con match-arms vs single-con+arity). 92% lines but
  divergent templates; factoring the scaffolding out of `quote!` blocks isn't a
  net win.
- ~~3. unbox heap pointer~~ — DEFER. `unbox_addr`/`unbox_bytearray` share the
  HeapPtr-unbox prologue, but it's delicate Cranelift block-building feeding
  divergent continuations, and there's a whole `unbox_*` family — a real but
  riskier refactor for later, not a quick win.
- ~~4. pipeline fn-decl setup~~ — SKIP. Both are `#[test]` fns
  (`test_get_function_ptr_after_finalize`/`test_build_lambda_registry`) sharing
  setup boilerplate. Test code; marginal.
- ~~6. apply_cont~~ — SKIP (mislabeled). Not apply_cont — two identical
  `impl FromValue for TestReq` fixtures in `#[cfg(test)]` modules of
  `tidepool-effect/src/machine.rs`. Test code.

## Findings about the LLM line-similarity report

Tidepool's structural pull + judgment found the report's "7 extractable" was
~50% false-positive for *production* wins: 2 were test boilerplate (#4, #6), 1
divergent `quote!` (#2), 1 risky family (#3). The genuine wins were the two
ABI-signature dedups (#1, #7) — which the report UNDER-counted (3 and 5 sites,
reported as 2 each), because the in-file pairwise detector can't see the
cross-file repeats. Lesson: line-overlap % flags candidates; it can't tell
test-from-production or identical-from-merely-similar — that needs the pull +
a human/structural check, which is exactly the tidepool loop.

## Notes

- One commit per extraction so a regression bisects cleanly.
