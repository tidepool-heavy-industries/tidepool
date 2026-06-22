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

- [x] **1. `runtime_case_trap` sig** (primop.rs ×2 + **case.rs** — tidepool found
  a 3rd site the report missed). 100% identical ABI sig. → `runtime_case_trap_sig`
  in `emit/mod.rs`; each site keeps its own declare + `.expect`/`.map_err`.
  GREEN (build/clippy/test).
- [ ] 2. `from_value` codegen — bridge-derive `codegen.rs:194,357` (92%) →
  `generate_from_to_core_base`. (Near-dup; verify the 8% delta first.)
- [ ] 3. unbox heap pointer — primop.rs:2134,2205 (90%) → `emit_unbox_heap_ptr`.
- [ ] 4. pipeline fn-decl setup — pipeline.rs:271,295 (90%) → `declare_fn_common`.
- [ ] 5. HS path resolution — macro `expand.rs:46,171` (90%) → `resolve_hs_path`.
- [ ] 6. effect-machine apply — `effect_machine.rs` apply_cont variants (87%) →
  `apply_cont_common`. (Verify carefully — apply_cont is #313-adjacent.)
- [ ] 7. emit force_fn — expr.rs:585,2141 (84%) → `emit_force_fn`. (Lowest
  overlap; most likely to be a judgment call.)

## Notes

- Items 2–7 are near-dups (84–92%), NOT 100% — each needs the "confirm identical
  enough" eval before extracting; some may be SKIP-as-intentional.
- #6 (apply_cont) touches the effect machine — gate on the full
  `tidepool-codegen` + `tidepool-runtime` suites, not just a build.
- One commit per extraction (or per small batch) so a regression bisects cleanly.
