# 02 — Haskell extract + stdlib (haskell/)

Silent-wrong-answer bugs on everyday model input. Findings 1 and 2 were
**reproduced against the live eval server** during the review. The API-is-the-
prompt rule (root CLAUDE.md) makes base-divergence a bug class here, not a
style question: models type canonical Haskell and trust canonical semantics.

## ANTI-PATTERNS

- Do NOT "fix" locked decisions: Cast/Tick/Type erasure happens in the Haskell
  serializer (not Rust); types are stripped at serialization.
- Do NOT change wire format arity (the "array of exactly 7" contract) — see
  `haskell/CLAUDE.md` one-format policy (stale extract must fail loud).
- After ANY `haskell/` change: follow `haskell/CLAUDE.md` rebuild steps and
  redeploy via `scripts/redeploy.sh` (NOT `scripts/deploy.sh` — stale shadow,
  see plan 09). Regenerate fixtures if the wire output changes.

## READ FIRST

- `haskell/CLAUDE.md` (rebuild/deploy, fixture regeneration, Known Limits)
- `haskell/src/Tidepool/Translate.hs` — literal-unpacking arms (~:979-1146)
- `haskell/lib/Tidepool/Prelude.hs` — the shadow-surface conventions

---

## HIGH — reproduced or trivially reachable

### H1. Non-ASCII `String` literals silently corrupted (REPRODUCED LIVE)

**Where:** `haskell/src/Tidepool/Translate.hs:979-993` (+
`emitRuntimeUnpackCString` at :155-189).

`unpackCStringUtf8#` expands one `Char` per BYTE with no UTF-8 decode.
Reproduced: `map fromEnum ("hé" :: String)` → `[104,195,169]` (should be
`[104,233]`); `T.pack "hé"` → `"hÃ©"` mojibake. Any accent/em-dash in a String
literal gives wrong answers with no error. `Text` literals are unaffected
(different path) — which is why it shipped.

**Fix:** UTF-8-decode `bytes` before emitting `LEChar` cons cells in the
static arm; give the runtime `Addr#` loop (`emitRuntimeUnpackCString`) a
decoding variant.

### H2. Non-ASCII literals in fusion contexts abort extraction with a misleading error (REPRODUCED LIVE)

**Where:** `Translate.hs:1090-1146` — `unpackFoldrCStringUtf8#` has no
non-static/zero-arg fallback arms.

Reproduced: `pure (T.length "héllo")` fails with *"Dangling NVar:
unpackFoldrCStringUtf8#… This is an extract-pipeline bug… not a user error"*
while the ASCII version works (falls through to bare `NVar`;
`Resolve.hs:304-314` refuses to resolve magic unpack vars).

**Fix:** add the fallback arms `unpackAppendCString#` already has
(Translate.hs:1003-1052), folding in the decode from H1.

### H3. `replicate` with negative n never terminates

**Where:** `haskell/lib/Tidepool/Prelude.hs:524-529`.
`go 0 = []; go !m = x : go (m-1)` skips the base case for negative `m` →
infinite list; the file's own comment (:930-933) says infinite lists SIGSEGV
the JIT. Trivially reachable: `replicate (n - length xs) pad` when `xs` is
longer. Base returns `[]`. **Fix:** `go m | m <= 0 = []`.

### H4. `splitAt` with negative n returns components exactly swapped

**Where:** `Prelude.hs:442-449`. Returns `(xs, [])`; base returns `([], xs)`.
Silent. **Fix:** `go m ys | m <= 0 = ([], ys)`.

## MEDIUM

### M1. Non-finite Doubles JSON-encode as garbage finite numbers

**Where:** `haskell/lib/Tidepool/Aeson/Scientific.hs:150-171`.
`fromDouble = readDecimalToScientific . show` folds the LETTERS of
`"inf"`/`"NaN"` as digits: `toJSON (1/0)` → `Number 6374` on the JIT. A
mean-of-empty-list NaN reaches the caller as a plausible number (upstream
aeson encodes `Null`). **Fix:** reuse `fmtFrac`'s NaN/Inf guards; map to `Null`.

### M2. Out-of-range integer JSON decode silently wraps

**Where:** `Aeson/FromJSON.hs:171-173` + `Aeson/Lens.hs:93-96`.
`eitherDecode "18446744073709551615" :: … Int` → `Right (-1)`; upstream aeson
bounds-checks. **Fix:** route through the currently-DEAD `toBoundedInteger`
(`Scientific.hs:189`); the `_Int` prism returns `Nothing` on out-of-range.

### M3. Patch.hs: standard `diff -u` timestamp headers break parsing

**Where:** `haskell/lib/Tidepool/Patch.hs:189-203`. `--- path\t2026-01-01 …`:
tab+timestamp become part of the path → spurious "renames unsupported"
rejection for plain in-place edits; `/dev/null` create-detection misses.
**Fix:** truncate paths at first `'\t'`.

### M4. Patch.hs: `\ No newline at end of file` parsed then DISCARDED

**Where:** `Patch.hs:394, 422`. Applying such a patch silently produces the
wrong trailing newline. **Fix:** track the marker per side, or reject loudly.

### M5. 2-result unboxed-tuple fallback binds BOTH result binders to the same primop node

**Where:** `Translate.hs:1550-1574`. E.g. `casSmallArray#`'s `flag` binder
would receive a heap pointer (JIT returns only the old value); stateful
primops risk double execution. No reachable stdlib path today — a landmine.
**Fix:** hard `error` naming the op on the 2-result arms (fail loud at extract
time instead of silently miscompiling).

### M6. `Slice [a]` negative-n semantics diverge from base AND from `Slice Text`

**Where:** `Prelude.hs:614-619`. `stake (-1) [1,2,3]` = `[1,2,3]` vs `[]` —
polymorphic code changes meaning by instance. **Fix:** clamp `n <= 0`.

### M7. `camelToSnake` emits a leading underscore on PascalCase

**Where:** `haskell/lib/Tidepool/TextFormat.hs:54-60`. `"HelloWorld"` →
`"_hello_world"`, contradicting its own doctest. **Fix:** suppress `_` at
position 0.

### M8. ASCII-only `isAlpha`/`isSpace`/etc. shadow Data.Char with divergent Unicode semantics

**Where:** `Prelude.hs:830-857`. Also internally inconsistent: the vendored
`T.words` (`Data/Text.hs:52`) uses real Unicode `isSpace`. **Fix:** match
Data.Char, or rename to `isAsciiAlpha` etc. (API-is-the-prompt: prefer match).

### M9. `center` pads the odd char on the RIGHT

**Where:** `Prelude.hs:1100-1108`. Contradicts both its haddock and
`Data.Text.center`. **Fix:** swap `lpad`/`rpad`; make `TextFormat.hs:116`
`centerWith` agree.

### M10. `Len [a]` non-guarded recursion — JIT stack death on long lists

**Where:** `Prelude.hs:583-586`. `1 + len xs`; the repo's own evidence
(`Data/Text.hs:273-282`) is ~20k depth kills the JIT stack; the adjacent
`length` was deliberately accumulator-strict. **Fix:** same accumulator shape.
(Note: plan 01 finding 5 may raise the real ceiling — this fix is still right.)

## LOW

- `Prelude.hs:735-765` — `parseDoubleM` overflows Int past ~19 digits →
  `Just <garbage>`; also rejects `1.0e-2` (can't round-trip `showDouble`).
- `Prelude.hs:1006-1017` — `nubBy` applies the predicate with FLIPPED arg
  order vs base (matters for non-equivalence predicates).
- `Aeson/Lens.hs:93-105` — `_Int`/`_Integer` truncate toward zero; lens-aeson
  floors (`"-3.7"` → `-3` vs `-4`); haddock claims to mirror upstream.
- `QQ/Fmt/Runtime.hs:180-186` — digit grouping uses size 3 for hex/oct/bin
  (Python: 4, and `,` is rejected for them); `:62-94` — `fmtFrac` overflows
  past 2^63 (`{1.0e19:.0f}` → saturated garbage); `fmtInt minBound` crashes on
  `negate minBound`.
- `haskell/lib/Tidepool/Data/Time.hs:149-151` — `addUTCTime` truncates the ms
  conversion (1.005s lands 1ms short; `diffUTCTime` doesn't round-trip) —
  fix: `round`. `:56-83` — negative year components render as garbage digits
  rather than erroring.
- `haskell/lib/Tidepool/Table.hs:39-50` — `parseCsv` splits quoted fields on
  embedded commas (documented "no quoting", but the NAME promises CSV; silent
  wrong fields on any RFC-4180 export). Quote-aware parse or rename.
- `haskell/src/Tidepool/CborEncode.hs:133-136` — stale comment claims the Rust
  reader accepts arity 5/6/7; `tidepool-repr/src/serial/read.rs:144-147`
  requires exactly 7. Comment licenses a future bug — fix comment.
- `haskell/app/Main.hs:182,189` — all-closed fixture path keys DataCon meta by
  `dcid` alone with left-biased union, silently collapsing varId collisions
  that `tsUsedDCs`' `(varId, qname)` keying was built to keep loud (masked
  today by `scanMeta` re-walk). Key it `(dcid, qname)`.
- `.tidepool/lib/RustSections.hs:19` (untracked WIP) — `nm = T.takeWhile (/=
  '_') rest` mangles underscore-free fn names into the signature (and `'_`
  lifetimes): `pub fn respond(cx: &mut EffCx<'_>)` → key `"respond(cx: &mut
  EffCx<'"`. Extract the identifier first. `:25-30` — `arrayLen` counts lines
  not entries; wrong for one-line arrays; scan doesn't terminate when the
  array closes with `]` (not `],`).

## Opportunities

1. **Non-ASCII test coverage is ZERO** — neither `test/Suite.hs` nor
   `test/TextSuite.hs` contains a single non-ASCII literal; that's how H1/H2
   shipped. Add `T.length "héllo"`, `map fromEnum "é"`, and a fusion-context
   probe to TextSuite AND `jit_surface.rs` alongside the fix.
2. **Harden occName-only magic matching** (`Translate.hs:2156-2314`) —
   `isAppendVar`/`isUnsafeTakeVar`/etc. match ANY var of that name with no
   module check; require a `GHC.`/`ghc-internal` prefix.
3. `[fmt|a\nb|]` keeps `\n` LITERAL (`Fmt.hs:129`) — biggest fluency tax in
   the QQ slice; interpret `\n`/`\t`/`\\`.
4. `[patch|]` has no `$$` escape for literal `$ident` lines — shell/Makefile
   content silently becomes a hole.
5. Scientific exponent DoS: `pow10`/`show` are linear in exponent;
   `scientific 1 1000000000` does a billion multiplications. Cap ~10^6 loudly.
6. Wire `toBoundedInteger` into the decode path (fixes M2, removes a dead
   export).

## Verified clean — do NOT re-audit

Root CLAUDE.md "Eval Records API" table matches `Tidepool.Records.Bridged`
field-for-field; `run`'s #335 contract (nonzero exit = `Right proc`) and
`readGlob` per-file `Either` isolation match docs and `effect_defs.rs`;
vendored `Data/Text.hs` bodies match text-2.1.2 semantics; `FilePath.hs`
matches System.FilePath (its two deviations documented); Data/Time civil-date
math correct both directions; Patch.hs Myers/hunk/phantom-line core; KeyMap;
QQ/Json escapes + surrogates; PyF Spec grammar; CborEncode node grammar;
Binders; Resolve fallback chain; Session iface injection; haskell/CLAUDE.md
claims (Opt_FullLaziness/Opt_CprAnal, stdlib embedding) hold.

## DONE CRITERIA

- [ ] H1–H4 fixed; non-ASCII lane added (TextSuite + jit_surface.rs) and green
- [ ] M1–M10 fixed (each with a pinning test where the harness reaches it)
- [ ] Lows fixed or filed; RustSections.hs items fixed in-place (it's WIP)
- [ ] Fixtures regenerated per haskell/CLAUDE.md; `scripts/battery.sh` green
- [ ] Redeployed via `scripts/redeploy.sh`; live-server spot-check of H1/H2
      repros now correct (`map fromEnum "hé"` → `[104,233]`)
