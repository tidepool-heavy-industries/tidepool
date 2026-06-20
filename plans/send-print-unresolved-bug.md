# Bug: `send (Print …)` resolves to an unresolved variable

Found 2026-06-20 via dogfooding while fixing the `eval_partial_output_failclass`
suite hang. Real behavior on the *current* build (extract + server rebuilt this
session), not a stale artifact.

## Symptom
`send (Print …)` type-checks (the Console GADT + `Print` constructor are in
scope) but resolves to an **unresolved variable at runtime**:

```
yield error: unresolved variable VarId(0xfe75fa6b4241aaa3) [tag='þ', key=33207910855191203]
```

The VarId is deterministic across distinct sources.

## What works vs fails (live server, full effect stack)
- `pure (1 + 1)` ✓
- `run "echo hi"` ✓ → `[0,"hi\n",""]`  (so `send`/effect machinery is fine)
- `pure (go 5000000)` and `pure $! (go 5000000)` ✓ → clean stack-overflow yield
  (`go n = if n <= 0 then 0 else n + go (n-1)`)
- `send (Print (T.pack "hi"))` ✗ → unresolved variable
- `send (Print …) >> (pure 42)` ✗ → unresolved variable
- `do { send (Print …); pure 7 }` ✗ → unresolved variable

So: **only the Console/`Print` constructor is unresolved**; `run` (another
effect via `send`) resolves fine.

## Fresh minimal-stack compile (the test, single ConsoleHandler)
- `send (Print marker) >> (error "boom")` ✓ — `say_then_haskell_error` passes,
  marker captured, classifies as `haskell-error`. **So `Print` CAN resolve.**
- `send (Print marker) >> (pure $! (go 5000000))` ✗ — `[CASE TRAP]` +
  unresolved external VarId(0xfe75…), empty captured output.

So in a minimal stack the trigger is the *continuation shape*: `>> error`
resolves `Print`, `>> (pure $! <deep>)` does not. On the full live stack even
bare `send (Print …)` fails. Possibly two faces of one root cause, or two
bugs with the same symptom.

## Hypotheses (unverified — for the hunt)
1. **Closed-Core reachability**: the reachable-closure meta walk drops `Print`'s
   binding from the closed Core when the eval's continuation is
   `pure $! <recursion>` (vs `error …`). The binding is in scope at type-check
   but absent from the JIT-linked set → unresolved at runtime.
2. **DataConTable VarId collision** (the class that once evicted freer's
   `Union`): the full effect stack pressures the table and `Print`'s 56-bit id
   collides → unresolved. Minimal stack (Console-only) avoids the pressure, so
   `Print` resolves there (case 1). Run TIDEPOOL_VARID_AUDIT=1 first.
3. **effects_module_source surgery**: this module (now in
   `tidepool-mcp/src/eval_prep.rs`) was moved + sed-edited during the eval-prep
   merge-conflict resolution. Diff its generated Console/Print against a
   known-good emission. (Type-checks, so not a gross corruption — but worth a
   look.)

## Repro (live server)
```
send (Print (T.pack "hi"))      -- ✗ unresolved variable
run "echo hi"                    -- ✓
```

## Status
- Test `say_then_stack_overflow_is_captured_and_classified` is `#[ignore]`d
  pending this fix (the worker-thread timeout already removed the prior hang).
- First step of the hunt: `TIDEPOOL_VARID_AUDIT=1` on a `send (Print …)` eval to
  rule in/out a VarId collision (hypothesis 2) before chasing reachability.
