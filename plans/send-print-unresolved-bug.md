# Bug: `send (effect) >> pure` → unresolved external VarId(0xfe75fa6b4241aaa3)

Found 2026-06-20 via dogfooding. Reproduced in tests
(`tidepool-mcp/tests/repro_print_unresolved.rs`). **Root cause NOT yet
identified** — the first diagnosis was wrong (see "Corrected" below).

## Symptom (confirmed)
A unit-returning effect followed by a `pure` continuation fails at runtime:
```
yield error: unresolved variable VarId(0xfe75fa6b4241aaa3) [tag='þ', key=…]
```
The VarId is deterministic.

## Trigger map (repro_print_unresolved.rs, single Console handler)
- `pure 123` ✓
- `send (Print x) >> (error …)` ✓ — resolves (error path masks a tag-0 issue)
- `send (Print x) >> (pure 123)` ✗ — unresolved external
- `send (Print x) >> (pure $! 123)` ✗ — same (the `$!` is irrelevant)
- `send (KvSet k v) >> (pure 1)` ✗ — KvSet also returns `()`
- `run "echo hi" >> pure 42` ✓ — `run` returns a tuple (live-server probe)
- `do { _ <- run a; _ <- run b; pure 9 }` ✓

So the failing shape is a (unit-returning) effect with a `pure` tail; `run`
(tuple result) works. `run` is a Prelude *wrapper*, the failing ones use raw
`send`, so the unit-vs-tuple split is suggestive but NOT proven to be the axis.

## Corrected diagnosis (what we now KNOW, after disproving the first guess)
- The unresolved var is **NOT `()`**. `stableVarId` is
  `(0xFE<<56) | (md5(normalizeMod modStr ++ ":" ++ occStr).hi64 & 56-bit)`;
  reproducing it, `()` hashes to `0xfeb080…`/`0xfeb9f6…`, NOT `0xfe75…`.
  (The `0xFE` high byte is the *universal* stableVarId prefix, not a kind tag —
  the `'þ'` in the error message is meaningless.)
- `TIDEPOOL_VARID_AUDIT=fe75fa6b4241aaa3` resolves it to **`<not a binding
  site>`** with **0 collisions** — so it's a global con/external reference, not
  a local let/lam/top binding, and not a varId collision.
- A wide hash brute-force over freer-simple / FTCQueue / Union / unit-variant /
  base con names did NOT match — identity still unknown.
- **`a9a0082` is probably NOT the cause.** It only changed the DataConTable
  *meta walk* input (closedBinds → reachable binds); it did NOT change emission
  or external resolution (`wrapAllBinds neededBinds` was already reachable-only).
  An unresolved *emitted external* is not produced by the meta walk. The
  "a9a0082 pruned ()" conclusion was mechanistic-only and is now retracted —
  a real bisect (rebuild the pre-a9a0082 extract, run the repro) is still owed.

## Fix attempts that FAILED (reverted)
Two `Translate.hs` meta-walk changes (extend `closeTyCons` to walk
`dataConOrigResTy`; seed the meta from used cons' result types) — both targeted
the wrong thing (`()` in the DataConTable) and did not move the failure. Reverted.

## Next concrete steps (a real sub-investigation)
1. **Identify the var**: instrument the NVar emit path to print the source
   `Name` when it emits `varId v == 0xfe75fa6b4241aaa3` (the audit only covers
   binding sites; this is a *reference*). OR dump the closed Core
   (`TIDEPOOL_DUMP_CLOSED=__user`) and hash every qualified identifier to find
   the match.
2. **Then** decide where the fix belongs: if it's a DataCon missing from the
   table → meta walk; if it's an external with no unfolding → resolveExternals;
   if it's emission/reachability → translateModule.
3. **Bisect for real** once identified (rebuild old extract) to date the
   regression (if it is one).

## Status / impact
- `repro_print_unresolved.rs`: the two failing cases `#[ignore]`d (un-ignore →
  regression guards). `eval_partial_output_failclass::say_then_stack_overflow`
  also `#[ignore]`d (hit this same bug via `>> (pure $! …)`).
- Live dogfooding: `say`/Console and any unit-returning effect followed by
  `pure` is broken until fixed. `run`, KV reads, and most effects work.
