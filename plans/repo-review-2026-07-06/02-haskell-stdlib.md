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

### H1. Non-ASCII `String` literals silently corrupted (REPRODUCED LIVE) — [FIXED]

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

**Done:** added a shared `utf8CodepointsOf :: [Word8] -> [Int]` decode helper
(`Translate.hs`, next to `extractAddrLitBytes`) and applied it at every
literal-unpack cons-cell site (`unpackCString#`, `unpackAppendCString#`
static+partial, `unpackFoldrCString#` static+partial — not just the one arm
named above, since all of them shared the identical byte-per-`LEChar` bug).
Also gave the runtime (non-literal) `Addr#` loops a real UTF-8 decoding
variant (`emitUtf8DecodeStep`/`emitUtf8LeadCascade`/`emitUtf8NByteBranch`,
hand-built primop IR: mask+`IntEq`-cascade on the lead byte's high bits,
shift+OR the continuation bytes' data bits, `Chr`/`Ord` to convert), shared
by all three `emitRuntimeUnpackCString`/`AppendCString`/`FoldrCString` loops
— not reachable by either live repro (both go through the static-literal
arms) but asked for explicitly since ASCII-only coverage there was equally a
landmine for any future non-literal non-ASCII Addr#. Malformed/unexpected
lead bytes fall back to treating the byte as-is (best effort, never traps).
Regression tests: `stdlib_regressions_02.rs` `works_nonascii_*` (2-byte é,
3-byte — and €, both `\NNNN`-escaped and raw UTF-8 source bytes).

### H2. Non-ASCII literals in fusion contexts abort extraction with a misleading error (REPRODUCED LIVE) — [FIXED, discrepancy noted]

**Where:** `Translate.hs:1090-1146` — `unpackFoldrCStringUtf8#` has no
non-static/zero-arg fallback arms.

Reproduced: `pure (T.length "héllo")` fails with *"Dangling NVar:
unpackFoldrCStringUtf8#… This is an extract-pipeline bug… not a user error"*
while the ASCII version works (falls through to bare `NVar`;
`Resolve.hs:304-314` refuses to resolve magic unpack vars).

**Fix:** add the fallback arms `unpackAppendCString#` already has
(Translate.hs:1003-1052), folding in the decode from H1.

**Discrepancy:** the root cause is NOT missing non-static/zero-arg fallback
arms (that part of the finding doesn't hold against the code — the "Just
bytes" static arm's 3-element list pattern DOES match for the héllo literal;
`extractAddrLitBytes` is content-agnostic). Confirmed via
`TIDEPOOL_DUMP_CLOSED=y`: `T.length "héllo"` lowers to
`unpackFoldrCStringUtf8# lit lengthFB (id @Int) (I# 0#)` — the length-fusion
`foldr`/`build` instantiates the folded type `a` as `Int -> Int` (the
strictness-accumulator trick), so the fully-fused call carries a FOURTH value
arg beyond the syntactic `(lit, f, z)` triple. An exact-length
`[litArg, fArg, zArg]` pattern silently misses this over-saturated
application and falls through to a dangling `NVar` — under-saturation was
never the issue; the ASCII control case (`T.length "hello"`) doesn't even
reach `unpackFoldrCString#` at all (GHC worker/wraps it into a self-contained
specialized loop over the raw Addr#), so it was never a fair comparison.
**Actual fix:** generalized the static arm's guard to
`(litArg:fArg:zArg:extraArgs) <- args`, re-applying `extraArgs` to the
expanded result. The (still-worth-having) non-static/zero-arg fallback arms
were added too, mirroring `unpackAppendCString#`'s shape, reusing
`emitRuntimeUnpackFoldrCString` (now UTF-8-decoding, see H1) — genuinely
unreachable by any live repro but no longer missing.
Regression test: `stdlib_regressions_02.rs::works_nonascii_fusion_context_length`.

### H3. `replicate` with negative n never terminates — [FIXED]

**Where:** `haskell/lib/Tidepool/Prelude.hs:524-529`.
`go 0 = []; go !m = x : go (m-1)` skips the base case for negative `m` →
infinite list; the file's own comment (:930-933) says infinite lists SIGSEGV
the JIT. Trivially reachable: `replicate (n - length xs) pad` when `xs` is
longer. Base returns `[]`. **Fix:** `go m | m <= 0 = []`.

**Done:** applied exactly as prescribed. Regression test:
`stdlib_regressions_02.rs::works_replicate_negative_n_terminates` (run with a
generous `recv_timeout` — a cold per-eval GHC compile alone regularly takes
20-70s in this suite, so the timeout window has to comfortably exceed that or
it reads as "still hanging" regardless of the fix).

### H4. `splitAt` with negative n returns components exactly swapped — [FIXED]

**Where:** `Prelude.hs:442-449`. Returns `(xs, [])`; base returns `([], xs)`.
Silent. **Fix:** `go m ys | m <= 0 = ([], ys)`.

**Done:** applied exactly as prescribed. Regression test:
`stdlib_regressions_02.rs::works_splitat_negative_n_matches_base`.

## MEDIUM

### M1. Non-finite Doubles JSON-encode as garbage finite numbers

**Where:** `haskell/lib/Tidepool/Aeson/Scientific.hs:150-171`.
`fromDouble = readDecimalToScientific . show` folds the LETTERS of
`"inf"`/`"NaN"` as digits: `toJSON (1/0)` → `Number 6374` on the JIT. A
mean-of-empty-list NaN reaches the caller as a plausible number (upstream
aeson encodes `Null`). **Fix:** reuse `fmtFrac`'s NaN/Inf guards; map to `Null`.

**STATUS: FIXED.** `fromDouble` itself is unchanged (it structurally cannot
return `Null` — it returns `Scientific`, and this module must not import
`Tidepool.Aeson.Value`). Added `isFiniteDouble :: Double -> Bool` next to it,
reusing `fmtFrac`'s dictionary-free NaN/Inf test, and applied the guard at the
actual `ToJSON Double`/`ToJSON Float` instances (`Aeson/Value.hs`): non-finite
now maps to `Null`, finite is unchanged. Pinning tests:
`stdlib_regressions_02_medium.rs::works_tojson_double_infinity_is_null`,
`::works_tojson_double_nan_is_null`,
`::works_tojson_double_finite_unaffected` (regression guard).

### M2. Out-of-range integer JSON decode silently wraps

**Where:** `Aeson/FromJSON.hs:171-173` + `Aeson/Lens.hs:93-96`.
`eitherDecode "18446744073709551615" :: … Int` → `Right (-1)`; upstream aeson
bounds-checks. **Fix:** route through the currently-DEAD `toBoundedInteger`
(`Scientific.hs:189`); the `_Int` prism returns `Nothing` on out-of-range.

**STATUS: FIXED** (+ Opportunity 6 done: `toBoundedInteger` is no longer
dead). **Discrepancy:** `toBoundedInteger` was not fully dead before this fix
— `Prelude.hs`'s `asInt` already called it; the actual gap was the
`FromJSON Int` instance and the `_Int`/`_Integer` prisms, which is what this
fix targets. `FromJSON Int` now bounds-checks via `toBoundedInteger` ONLY for
exact-integer `Scientific` values (an out-of-range integer is now `Error`,
not a silent wraparound) — a fractional number still truncates toward zero
exactly as before (unaffected; that wasn't part of this finding). `_Int`/
`_Integer` (`Aeson/Lens.hs`) now go through new `floorScientific`/
`floorBoundedInteger` helpers (`Scientific.hs`) instead of
`truncateScientific`, which ALSO fixes the LOW below (floor, not truncate).
Pinning tests: `stdlib_regressions_02_medium.rs`
`::works_eitherdecode_int_out_of_range_is_left`,
`::works_eitherdecode_int_in_range_unaffected` (regression guard),
`::works_int_prism_floors_not_truncates`, `::works_int_prism_out_of_range_is_nothing`.

### M3. Patch.hs: standard `diff -u` timestamp headers break parsing

**Where:** `haskell/lib/Tidepool/Patch.hs:189-203`. `--- path\t2026-01-01 …`:
tab+timestamp become part of the path → spurious "renames unsupported"
rejection for plain in-place edits; `/dev/null` create-detection misses.
**Fix:** truncate paths at first `'\t'`.

**STATUS: FIXED.** Added `truncateAtTabS :: String -> String` (`fst . break
(== '\t')`), applied to both the `---`/`+++` raw path strings before the
`/dev/null` check and the rename-mismatch comparison. Pinning tests:
`stdlib_regressions_02_medium.rs::works_patch_tab_timestamp_not_treated_as_rename`,
`::works_patch_devnull_create_with_tab_timestamp`.

### M4. Patch.hs: `\ No newline at end of file` parsed then DISCARDED

**Where:** `Patch.hs:394, 422`. Applying such a patch silently produces the
wrong trailing newline. **Fix:** track the marker per side, or reject loudly.

**STATUS: FIXED** (tracked and applied — no loud-reject fallback was needed).
`Hunk` gained two fields, `hOldNoNewline`/`hNewNoNewline`; `parseBodyN` now
threads them, attributing a marker to whichever side(s) the immediately
preceding body line belongs to (context lines mark both). `invertPatch`/
`creationPatch`/`sliceHunk` (genPatch) and the `[patch|...|]` QQ codegen
(`QQ/Patch.hs`) were updated for the new arity. `applyFilePatch`'s `goHunks`
now corrects the reconstructed file's trailing newline using the LAST hunk's
flags: if the new side lacks a trailing newline, drop a trailing empty
element from the untouched tail; if the old side lacked one but the new side
should have one, append one. (The no-marker case — the overwhelming majority
of real patches — is untouched, so this can't regress any existing
patch/genPatch test.) Pinning tests:
`stdlib_regressions_02_medium.rs::works_patch_apply_marker_new_side_loses_trailing_newline`,
`::works_patch_apply_marker_new_side_gains_trailing_newline` (both
directions).

### M5. 2-result unboxed-tuple fallback binds BOTH result binders to the same primop node — [FIXED]

**Where:** `Translate.hs:1550-1574`. E.g. `casSmallArray#`'s `flag` binder
would receive a heap pointer (JIT returns only the old value); stateful
primops risk double execution. No reachable stdlib path today — a landmine.
**Fix:** hard `error` naming the op on the 2-result arms (fail loud at extract
time instead of silently miscompiling).

**Done:** both the pure and the stateful 2-result arms now `error` naming the
offending primop/FFI call (via `showPprUnsafe v`), instead of aliasing both
result binders to the same primop node. No correct 2-result split was
attempted — fail-loud is the locked scope. Since this has no reachable
stdlib path (confirmed: every multi-result primop the stdlib actually calls —
`quotRem`, `addC`/`subC`, `decodeDouble_Int64#` — already goes through the
dedicated `splitMultiReturnPrimOp`/`splitUnaryMultiReturnPrimOp` split, never
this generic fallback), it can't be pinned as a `code: &str` JIT probe (the
MCP preamble's fixed pragma set doesn't include `MagicHash`/`UnboxedTuples`).
Verified directly against the extract binary instead, with hand-written
`MagicHash`/`UnboxedTuples` source hitting both now-error'd arms:
- pure: `case decodeFloat_Int# f of (# m, e #) -> ...` →
  `"Unsupported 2-result pure unboxed-tuple primop: decodeFloat_Int#"`
- stateful: `case casArray# arr 0# old new s of (# s', flag, oldVal #) -> ...`
  → `"Unsupported 2-result stateful unboxed-tuple primop/FFI call: casArray#"`

Both abort extraction with the named marker (`SKIPPED`, not a crash or a
silent miscompile) — see the note in `stdlib_regressions_02.rs` for why this
isn't duplicated as a Rust test.

### M6. `Slice [a]` negative-n semantics diverge from base AND from `Slice Text`

**Where:** `Prelude.hs:614-619`. `stake (-1) [1,2,3]` = `[1,2,3]` vs `[]` —
polymorphic code changes meaning by instance. **Fix:** clamp `n <= 0`.

**STATUS: FIXED.** `stake`/`sdrop` for `[a]` now guard `n <= 0` before the
literal-`0`/empty-list clauses, matching `Slice Text` and base `take`/`drop`.
Pinning tests: `stdlib_regressions_02_medium.rs::works_slice_list_stake_negative_clamps_to_empty`,
`::works_slice_list_sdrop_negative_clamps_to_whole`.

### M7. `camelToSnake` emits a leading underscore on PascalCase

**Where:** `haskell/lib/Tidepool/TextFormat.hs:54-60`. `"HelloWorld"` →
`"_hello_world"`, contradicting its own doctest. **Fix:** suppress `_` at
position 0.

**STATUS: FIXED.** `go` now threads an `isFirst` flag; the underscore is
suppressed only at position 0 (every other uppercase transition still gets
one) — `"HTTPServer"` still gives `"h_t_t_p_server"` per its own doctest.
Pinning test: `stdlib_regressions_02_medium.rs::works_cameltosnake_no_leading_underscore`.

### M8. ASCII-only `isAlpha`/`isSpace`/etc. shadow Data.Char with divergent Unicode semantics

**Where:** `Prelude.hs:830-857`. Also internally inconsistent: the vendored
`T.words` (`Data/Text.hs:52`) uses real Unicode `isSpace`. **Fix:** match
Data.Char, or rename to `isAsciiAlpha` etc. (API-is-the-prompt: prefer match).

**STATUS: FIXED** (matched, per the locked choice — did not rename).
`isAlpha`/`isAlphaNum`/`isSpace`/`isUpper`/`isLower` now delegate to
`Data.Char`'s real Unicode-table implementations (`isDigit` untouched — it
was already ASCII-only in upstream `Data.Char` too, no divergence). Safe on
the JIT by the same precedent already proven live: the vendored `T.words`
(`Data/Text.hs:52`) already runs `Data.Char.isSpace` on the JIT. Pinning
tests: `stdlib_regressions_02_medium.rs::works_isalpha_unicode_letter` (é,
U+00E9), `::works_isspace_unicode_nbsp` (U+00A0) — both outside the old
ASCII-only ranges.

### M9. `center` pads the odd char on the RIGHT

**Where:** `Prelude.hs:1100-1108`. Contradicts both its haddock and
`Data.Text.center`. **Fix:** swap `lpad`/`rpad`; make `TextFormat.hs:116`
`centerWith` agree.

**STATUS: FIXED.** Swapped which half is computed by `div` in both
`Prelude.hs`'s `center` and `TextFormat.hs`'s `centerWith` — `rpad = total
\`div\` 2; lpad = total - rpad`, so the odd leftover character now goes to
`lpad`. Pinning tests: `stdlib_regressions_02_medium.rs::works_center_pads_odd_char_left`,
`::works_textformat_centerwith_agrees`.

### M10. `Len [a]` non-guarded recursion — JIT stack death on long lists — [FIXED]

**Where:** `Prelude.hs:583-586`. `1 + len xs`; the repo's own evidence
(`Data/Text.hs:273-282`) is ~20k depth kills the JIT stack; the adjacent
`length` was deliberately accumulator-strict. **Fix:** same accumulator shape.
(Note: plan 01 finding 5 may raise the real ceiling — this fix is still right.)

**Done:** `Len [a]`'s instance now shares `length`'s accumulator-strict `go`
shape (`go !acc [] = acc; go !acc (_:xs) = go (acc+1) xs`). Regression test:
`stdlib_regressions_02.rs::works_len_class_long_list_no_stack_death` (50k
elements, comfortably past the ~20k non-tail-call ceiling).

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

**STATUS — all fixed except the noted skip:**

- `parseDoubleM` — **FIXED.** Now parses an optional `e`/`E` exponent
  (`parseMantissa`/`parseExponent`/`scaleByPow10`) and accumulates digits
  directly as a `Double` via `digitsToDouble` (never through `Int`), so a
  digit run past ~19 characters loses precision like any `Double` parse
  instead of silently overflowing. `scaleByPow10` combines the mantissa and
  exponent with ONE final multiply/divide against a precomputed power of ten
  (not N chained single-digit steps) — chaining was tried first and measurably
  failed to round-trip `showDouble` for negative exponents (compounding
  rounding error). Pinning tests:
  `stdlib_regressions_02_medium.rs::works_parsedoublem_accepts_exponent_notation`,
  `::works_parsedoublem_roundtrips_showdouble_extreme_magnitudes`.
- `nubBy` argument order — **FIXED.** `elemBy` now calls `eq y x` (seen
  element first, candidate second), matching base's `elem_by eq y (x:xs) = x
  \`eq\` y || …`. Pinning test:
  `stdlib_regressions_02_medium.rs::works_nubby_argument_order_matches_base`
  (a directional, non-equivalence predicate distinguishes the two orders).
- `_Int`/`_Integer` floor — **FIXED**, folded into the M2 fix above (new
  `floorScientific`/`floorBoundedInteger` in `Scientific.hs`).
- `QQ/Fmt/Runtime.hs` digit grouping / `fmtFrac` overflow / `fmtInt minBound`
  — **ALL FIXED.** Grouping size is now `3` for decimal, `4` for hex/oct/bin
  (`groupSize`); `,` grouping is now rejected for `b`/`o`/`x`/`X` at spec-parse
  time (`QQ/PyF/Spec.hs`'s new `failIfCommaGrouping`, matching Python's
  `Cannot specify ',' with '<type>'.`). `fmtFrac`'s `scaled`/`powI` are now
  `Integer` (not `Int`), so a huge Double scaled by `10^prec` no longer
  saturates. `fmtInt`'s magnitude and `digitsInBase` are now `Integer` (not
  `Int`), so `negate minBound` no longer silently stays negative and crashes
  the digit-table index. Pinning tests:
  `stdlib_regressions_02_medium.rs::works_fmt_hex_grouping_is_four_not_three`,
  `::fails_fmt_hex_comma_grouping_rejected`,
  `::works_fmtfrac_beyond_2_63_does_not_saturate`,
  `::works_fmtint_minbound_does_not_crash`.
- `Data/Time.hs` `addUTCTime` rounding — **FIXED** (`truncate` → `round`).
  Negative-year garbage digits — **FIXED**: `pad4` now `error`s loudly
  (`"formatISO8601: negative year …"`) instead of `digitChar`'s
  catch-all `_ -> '9'` firing on a negative `Int`. Pinning tests:
  `stdlib_regressions_02_medium.rs::works_addutctime_rounds_ms_conversion`,
  `::fails_formatiso8601_negative_year_errors_loudly`.
- `Table.hs` `parseCsv` — **FIXED** (quote-aware, not renamed): a new
  `parseCsvRowS` recursive-descent String-space splitter handles quoted
  fields (commas inside quotes, `""` escaping a literal quote); `parseTsv`/
  `parseDelimited` are untouched (quoting was never claimed there). A literal
  newline inside a quoted field is still not supported (rows are still split
  on `'\n'` before quote parsing, like every other row-oriented verb here) —
  documented in the haddock rather than fixed, since RFC-4180 multi-line
  fields would require restructuring the row-splitting step shared by every
  verb in this module, a materially bigger change than this LOW warranted.
  Pinning test: `stdlib_regressions_02_medium.rs::works_parsecsv_quoted_field_with_embedded_comma`.
- `CborEncode.hs` stale arity-5/6/7 comment — **FIXED** (comment only, states
  exactly 7 now). Not eval-reachable (compile-time comment); verified by
  direct inspection against `tidepool-repr/src/serial/read.rs:144-147`.
- `app/Main.hs` DataCon meta keying — **FIXED**: the per-target `Map.fromList`
  inside the `--all-closed` fold is now keyed `(dcid, qname)` instead of
  `dcid` alone, so `Map.union` across targets can no longer silently drop one
  of a colliding pair before it reaches `mergeMetaPreserving`'s loud
  collision-preserving merge. Not eval-reachable (only exercised by the
  extractor's own multi-target fixture-generation path, not any single JIT
  eval); verified by inspection that the new keying matches
  `tsUsedDCs`/`mergeMetaPreserving`'s existing `(varId, qname)` convention
  (`src/Tidepool/Translate.hs`) — following M5's precedent for
  extract-level-only verification.
- `.tidepool/lib/RustSections.hs` — **SKIPPED**, per this branch's explicit
  task boundary: the file is untracked WIP not present in this worktree
  (root owns it).

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
   export). **DONE** — see M2's STATUS block above.

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

- [x] H1–H4 fixed; non-ASCII lane added and green — landed in a NEW file,
      `tidepool-runtime/tests/stdlib_regressions_02.rs` (not `jit_surface.rs`/
      `TextSuite.hs`, to avoid touching files another worker owns in this
      wave; see that file's module doc for the full probe set)
- [x] M1–M10 fixed (each with a pinning test where the harness reaches it) —
      M5 and M10 were the previous wave (both fixed, see their entries above);
      **this wave (stdlib-medium branch): M1-M4 and M6-M9 fixed**, each with a
      pinning test in `tidepool-runtime/tests/stdlib_regressions_02_medium.rs`.
      All 10 M-findings are now fixed.
- [x] Lows fixed or filed; RustSections.hs items fixed in-place (it's WIP) —
      **this wave:** all LOW items fixed except `.tidepool/lib/RustSections.hs`,
      which is SKIPPED per this branch's explicit task boundary (untracked WIP,
      not present in this worktree — root owns it, see the LOW STATUS block
      above). `app/Main.hs` and `CborEncode.hs` LOWs are fixed but not
      Rust-pinned (not JIT-eval-reachable — verified by inspection, following
      M5's precedent).
- [x] Fixtures regenerated per haskell/CLAUDE.md; `scripts/battery.sh` green —
      `scripts/battery.sh` itself is root's job post-merge (explicitly out of
      scope for this branch, per its task boundary — a concurrent worker owns
      14 known pre-existing failures there). Fixture regeneration WAS done
      transiently THIS WAVE to differentially verify the M1-M4/M6-M9 fixes
      against the full `test/Suite.hs` corpus (218/218 `haskell_suite`/
      `haskell_suite_differential` tests green, plus the earlier H1/H2/M5
      wave's 218/218) — the regenerated fixtures were then reverted (`git
      checkout`) since two consecutive regenerations from the SAME unchanged
      binary already differ in GHC's non-deterministic synthetic-name
      suffixes, so committing them would be pure noise unrelated to this fix.
- [ ] Redeployed via `scripts/redeploy.sh`; live-server spot-check of H1/H2
      repros now correct (`map fromEnum "hé"` → `[104,233]`) — root's job
      after merge, per this branch's task boundary
