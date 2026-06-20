# RESOLVED: `send (effect) >> pure` → unresolved `GHC.Magic.nospec`

Found + fixed 2026-06-20 via dogfooding. Regression guard:
`tidepool-mcp/tests/repro_print_unresolved.rs` (4/4 green).

## Symptom (was)
A unit-returning effect followed by a `pure` continuation failed at runtime with
`unresolved variable VarId(0xfe75fa6b4241aaa3)`; `run >> pure` (tuple result)
worked. Broke `say`/Console on the dogfooding server.

## Root cause (confirmed)
The var `0xfe75fa6b4241aaa3` is **`GHC.Magic.nospec`** — the specializer's
identity wrapper (`nospec x = x`), which GHC inserts to block over-specialization
of dictionary / class-method code. It has **no unfolding**, so `resolveExternals`
can't inline it; the extract emitted it as a plain `NVar` (it believed it
resolved), and the JIT — which only special-cased `runRW#` among the magic
functions — had no binding/con for it → forced it as an unresolved external.
`run >> pure` differs in its dictionary path, so `nospec` isn't inserted there.

It is a **regression from `1dc47d1` "feat(mcp): expression-first eval contract"**
(empirically bisected). 1dc47d1 moved user code out of an inline nested `do`
into a separate top-level `__user` binding; GHC optimizes that standalone binding
in isolation and leaves the `nospec`-wrapped recursive `Member Console '[…]`
dictionary unsimplified, whereas the old inline form let GHC inline `send` and
simplify `nospec` away. The real axis is raw `send` (live polymorphic Member
dict) vs `run` (monomorphic via the `M` alias) — NOT unit-vs-tuple.
NOT `Opt_Specialise` (16f0014): putting those flags back still reproduces, so
`nospec` comes from desugaring the recursive instance dict, not the Specialise
pass. (Both the earlier "a9a0082 pruned ()" and the "Opt_Specialise" guesses
were wrong — disproved empirically; the fix is correct regardless of the date.)

## Fix
Treat `GHC.Magic.nospec` as the identity in `Translate.hs` (like `runRW#`):
- App form: `nospec @t f x… → f x…` (drop the wrapper, apply its first value
  arg to the rest).
- Bare/zero-value-arg form (`translateHead`): emit the identity `\x -> x`.
- New predicate `isNospecVar` (occ `nospec` + module `GHC.Magic` after
  normalizeMod).
Validated: repro_print_unresolved 4/4; haskell_suite 217; and the LIVE server now
runs `send (Print x) >> pure y` (and bare `send (Print …)`) correctly — `say`
restored.

## Follow-up — RESOLVED (was misdiagnosed as a harness gap)
`eval_partial_output_failclass::say_then_stack_overflow` is now un-`#[ignore]`d
and green (commit e008314). The earlier "harness-fidelity gap" framing was WRONG:
the `>> (pure $! (go 5_000_000))` form dropped the marker EVERYWHERE (live server
too — verified by the harness-fix worker, who checked the captured marker, not
just the yield class). `pure $! x` = `x \`seq\` pure x`; GHC's strictness/
let-floating on the standalone `__user` binding forces the recursion *while
evaluating `__user`*, BEFORE the `Print` effect yields → overflow first, the
`say` never fires. The `[CASE TRAP]` lines were benign StackOverflow
poison-cascade unwinding (error flag already set → poison return, no SIGILL; no
"raw bytes" line follows). Fix: drop the `$!` → `pure (go 5_000_000)` (lazy
thunk; Print yields first, the final toJSON forces it after the say → clean
runtime-yield). Not a harness bug, not a codegen bug — a test that forced its own
crash too early.
