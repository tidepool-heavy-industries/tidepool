# Follow-up: retire the `takeWhileT` / `dropWhileT` Prelude shadows

**Status:** PROPOSED — do NOT execute as part of the gotcha-#14 fix. This is a
separate, independently-verified follow-up. The #14 commit only pins the
mechanism fix (`repro_takewhile_pap.rs`) and trues up the docs; the shadows
stay in place until this plan is carried out with its own verification.

## Background

`Tidepool.Prelude` defines two load-bearing shadows (haskell/lib/Tidepool/Prelude.hs):

```haskell
takeWhileT :: (Char -> Bool) -> Text -> Text
takeWhileT p t = T.pack (go (T.unpack t))
  where go [] = []
        go (c:cs) | p c = c : go cs | otherwise = []

dropWhileT :: (Char -> Bool) -> Text -> Text
dropWhileT p t = T.pack (go (T.unpack t)) where ...
```

They existed *solely* to work around gotcha-audit #14: real `T.takeWhile` /
`T.dropWhile` were silently wrong under partial application. That bug is **dead**
as of 2026-06-11 (fixed in passing by the EPS unpoison, commit 9a827a3 — GHC now
loads unfoldings, so a lifted PAP worker `pap = takeWhile p` inlines the real
fused `Data.Text` definition; Core-verified, and `repro_takewhile_pap.rs` pins
all PAP shapes green). The shadows are now redundant correctness machinery.

## Why not just delete the names

User library code and existing evals reference `takeWhileT` / `dropWhileT` by
name (they were the documented "safe" spelling). Deleting the identifiers would
break that code with a compile error. So the retirement is a **re-definition to
delegation**, not a removal:

```haskell
-- Retained as thin aliases for source compatibility; the PAP bug they guarded
-- against is fixed (gotcha-audit #14, repro_takewhile_pap.rs).
takeWhileT :: (Char -> Bool) -> Text -> Text
takeWhileT = T.takeWhile
{-# INLINE takeWhileT #-}

dropWhileT :: (Char -> Bool) -> Text -> Text
dropWhileT = T.dropWhile
{-# INLINE dropWhileT #-}
```

Note `takeWhileT = T.takeWhile` is itself an eta-reduced PAP — exactly the shape
that used to corrupt. It is safe now (that is the whole point of the fix), but it
makes the shadow's correctness *depend on* the unpoison rather than being
self-contained. That coupling is the entire risk surface of this change.

## Steps (when executed)

1. Replace the two `T.pack . go . T.unpack` bodies in
   `haskell/lib/Tidepool/Prelude.hs` with the delegations above. Drop the now-
   inaccurate do-not-delegate doc comments; keep a one-line pointer to
   gotcha-audit #14.
2. Rebuild the worktree extract binary; clear that binary's cache entries.
3. **Verification gate (must all be green before merge):**
   - `repro_takewhile_pap.rs` — the underlying `T.takeWhile`/`T.dropWhile` PAP
     matrix (unchanged; this is the foundation the aliases now stand on).
   - A NEW sister matrix exercising the *shadow names* through PAP:
     `map (takeWhileT p) ts`, `let tw = takeWhileT p in map tw ts`,
     `map (dropWhileT p) ts`, etc. — proving the aliases inherit the fix.
   - Full `tidepool-eval` haskell_suite + `text_suite` (the existing
     `text_takeWhileT_*` / `text_dropWhileT_*` fixtures use the Suite-local
     `myTakeWhileT`, NOT the Prelude shadow, so they are unaffected — but run
     them to confirm no incidental Core-id churn).
   - `cargo clippy` / `cargo fmt` clean.
4. Fixture check: grep for evals/fixtures that import `Tidepool.Prelude` and call
   `takeWhileT`/`dropWhileT`. If the change alters any checked-in `meta.cbor` /
   closed-Core fixture, regenerate per CLAUDE.md "Regenerating Test Fixtures" and
   report the delta.

## Decision for the human

Three options, in increasing aggressiveness:

- **A — keep shadows as-is** (pure reimpls). Zero risk, mild redundancy. Default
  if the unpoison's durability is still being established.
- **B — delegate** (the plan above). Removes ~15 lines of redundant code, keeps
  the names. Couples shadow correctness to the (now-fixed) PAP path; guarded by
  the new verification gate.
- **C — delegate + deprecate** (B, plus a `{-# DEPRECATED takeWhileT "use
  T.takeWhile" #-}` and a doc nudge steering new code to `T.takeWhile`). Path to
  eventual removal; most churn for callers.

Recommendation: **B** once the unpoison has a few weeks of soak. Not part of the
#14 fix.
