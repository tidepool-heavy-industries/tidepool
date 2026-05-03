# Audit: tidepool-bridge primitive impls

## `get_resilient` helper

- **Location:** `tidepool-bridge/src/impls.rs:14` (`fn get_resilient`)
- **Constructor names looked up:** variable `name` (caller-provided)
- **Lookup strategy:** `get_by_name_arity` first, fallback to `matches.first()` if the table contains the name at any other arity
- **Assumed arity:** caller-supplied
- **Failure mode on shape mismatch:** Returns `Some(first_match)` even when ambiguous; emits `eprintln!` diagnostic in `cfg(debug_assertions)` builds when multiple same-name+same-arity entries exist.
- **Cross-module collision risk:** `medium` (silent fallback when arity mismatches)
- **Mode:** `always-on`
- **Test coverage:** `tidepool-bridge/tests/ambiguity_assert.rs`
- **Notes:** Added in PR #293 to replace direct `get_by_name` calls in primitive `FromCore`/`ToCore` impls. Provides arity-aware lookup with diagnostic recovery rather than panicking on ambiguity. All hand-written impls below now route through this helper.

## `is_boxing_con` helper

- **Location:** `tidepool-bridge/src/impls.rs:43` (`fn is_boxing_con`)
- **Constructor names looked up:** variable `name` (caller passes `"I#"`, `"W#"`, `"D#"`, `"C#"`)
- **Lookup strategy:** `get_resilient(table, name, 1)`
- **Assumed arity:** `1`
- **Failure mode on shape mismatch:** Returns `false`.
- **Cross-module collision risk:** `low` (post-#293 hardening; was `medium` pre-hardening)
- **Mode:** `always-on`
- **Test coverage:** `tidepool-bridge/tests/roundtrip.rs`, `tidepool-bridge/tests/ambiguity_assert.rs`
- **Notes:** Used by numeric and Char `FromCore` impls to unwrap GHC boxing constructors. Now arity-aware via `get_resilient`; collision risk reduced from `medium` to `low` since multi-arity ghosts of `I#`/`W#`/etc. are filtered.

## `PhantomData<T>`

- **Location:** `tidepool-bridge/src/impls.rs:70` (`impl FromCore for std::marker::PhantomData<T>`); `:84` (`impl ToCore`)
- **Constructor names looked up:** `"()"`
- **Lookup strategy:** `get_by_name` (ToCore), `iter().find(|dc| dc.rep_arity == 0)` fallback
- **Assumed arity:** `0`
- **Failure mode on shape mismatch:** `ArityMismatch`, `TypeMismatch`, `UnknownDataConName`
- **Cross-module collision risk:** `low`
- **Mode:** `always-on`
- **Test coverage:** `tidepool-bridge-derive/tests/phantom_data.rs`
- **Notes:** `ToCore` uses `"()"` but falls back to the first arity-0 constructor in the table if missing.

## `()` (Unit)

- **Location:** `tidepool-bridge/src/impls.rs:133` (`impl ToCore for ()` / `impl FromCore for ()`)
- **Constructor names looked up:** `"()"`
- **Lookup strategy:** `get_by_name`
- **Assumed arity:** `0`
- **Failure mode on shape mismatch:** `UnknownDataConName`, `TypeMismatch`
- **Cross-module collision risk:** `low`
- **Mode:** `always-on`
- **Test coverage:** `tidepool-bridge/src/impls.rs:tests::test_unit_roundtrip`
- **Notes:** Candidate for `get_by_name_arity("()", 0)`.

## `i64` (Int)

- **Location:** `tidepool-bridge/src/impls.rs:157` (`impl FromCore for i64` / `impl ToCore for i64`)
- **Constructor names looked up:** `"I#"`
- **Lookup strategy:** `get_by_name`
- **Assumed arity:** `1`
- **Failure mode on shape mismatch:** `TypeMismatch`, `UnknownDataConName`
- **Cross-module collision risk:** `medium`
- **Mode:** `always-on`
- **Test coverage:** `tidepool-bridge/tests/roundtrip.rs`
- **Notes:** Transparently unwraps `I#` box or accepts `LitInt`. Related to `i32` and `char` impls.

## `u64` (Word)

- **Location:** `tidepool-bridge/src/impls.rs:186` (`impl FromCore for u64` / `impl ToCore for u64`)
- **Constructor names looked up:** `"W#"`
- **Lookup strategy:** `get_by_name`
- **Assumed arity:** `1`
- **Failure mode on shape mismatch:** `TypeMismatch`, `UnknownDataConName`
- **Cross-module collision risk:** `medium`
- **Mode:** `always-on`
- **Test coverage:** `tidepool-bridge/tests/roundtrip.rs`
- **Notes:** Transparently unwraps `W#` box or accepts `LitWord`.

## `f64` (Double)

- **Location:** `tidepool-bridge/src/impls.rs:215` (`impl FromCore for f64` / `impl ToCore for f64`)
- **Constructor names looked up:** `"D#"`
- **Lookup strategy:** `get_by_name`
- **Assumed arity:** `1`
- **Failure mode on shape mismatch:** `TypeMismatch`, `UnknownDataConName`
- **Cross-module collision risk:** `medium`
- **Mode:** `always-on`
- **Test coverage:** `tidepool-bridge/tests/roundtrip.rs`
- **Notes:** Transparently unwraps `D#` box or accepts `LitDouble`.

## `bool`

- **Location:** `tidepool-bridge/src/impls.rs:270` (`impl FromCore for bool` / `impl ToCore for bool`)
- **Constructor names looked up:** `"True"`, `"False"`
- **Lookup strategy:** `get_by_name`
- **Assumed arity:** `0`
- **Failure mode on shape mismatch:** `UnknownDataConName`, `ArityMismatch`, `UnknownDataCon` (if `DataConId` doesn't match `True` or `False` lookup), `TypeMismatch`
- **Cross-module collision risk:** `low`
- **Mode:** `always-on`
- **Test coverage:** `tidepool-bridge/tests/roundtrip.rs`
- **Notes:** Candidate for `get_by_name_arity`. Collision risk is low as True/False are standard.

## `char`

- **Location:** `tidepool-bridge/src/impls.rs:321` (`impl FromCore for char` / `impl ToCore for char`)
- **Constructor names looked up:** `"C#"`
- **Lookup strategy:** `get_by_name`
- **Assumed arity:** `1`
- **Failure mode on shape mismatch:** `TypeMismatch`, `UnknownDataConName`
- **Cross-module collision risk:** `medium`
- **Mode:** `always-on`
- **Test coverage:** `tidepool-bridge/tests/roundtrip.rs`
- **Notes:** Transparently unwraps `C#` box or accepts `LitChar`.

## `String` (Text)

- **Location:** `tidepool-bridge/src/impls.rs:348` (`impl FromCore for String`); `:436` (`impl ToCore for String`)
- **Constructor names looked up:** `"Text"`, `"ByteArray"`, `"[]"`, `":"`, `"C#"`
- **Lookup strategy:** `get_resilient` (post-#293; was `get_by_name`)
- **Assumed arity:** `3 for Text`, `1 for ByteArray`, `0 for []`, `2 for :`, `1 for C#`
- **Failure mode on shape mismatch:** `TypeMismatch`, `UnknownDataConName`, `InternalError` (mutex poisoned)
- **Cross-module collision risk:** `medium` (post-#293; was `high` pre-hardening)
- **Mode:** `always-on`
- **Test coverage:** `tidepool-bridge/tests/roundtrip.rs`, `tidepool-bridge/tests/proptest_text.rs`
- **Notes:** Complex impl handling both unboxed `Text` worker format (`Con("Text", [ByteArray#, Int#, Int#])`, GHC -O2 representation) and cons-cell `[Char]` list format. `FromCore` (line 348) handles lifted `ByteArray` wrappers and accepts both raw `LitChar` and boxed `C#` in cons-cells (line ~411). `ToCore` (line 436) emits the GHC worker representation for `Text` (unboxed `ByteArray#` and `Int#` fields). All lookups now route through `get_resilient` for arity-aware disambiguation. Remaining migration target: `get_by_qualified_name` for `Text` would close the residual collision risk entirely.

## `Option<T>`

- **Location:** `tidepool-bridge/src/impls.rs:461` (`impl FromCore for Option<T>` / `impl ToCore for Option<T>`)
- **Constructor names looked up:** `"Nothing"`, `"Just"`
- **Lookup strategy:** `get_by_name`
- **Assumed arity:** `0 for Nothing`, `1 for Just`
- **Failure mode on shape mismatch:** `UnknownDataConName`, `ArityMismatch`, `UnknownDataCon`, `TypeMismatch`
- **Cross-module collision risk:** `low`
- **Mode:** `always-on`
- **Test coverage:** `tidepool-bridge/tests/roundtrip.rs`
- **Notes:** Candidate for `get_by_name_arity`.

## `Vec<T>` (List)

- **Location:** `tidepool-bridge/src/impls.rs:517` (`impl FromCore for Vec<T>` / `impl ToCore for Vec<T>`)
- **Constructor names looked up:** `"[]"`, `":"`
- **Lookup strategy:** `get_by_name`
- **Assumed arity:** `0 for []`, `2 for :`
- **Failure mode on shape mismatch:** `UnknownDataConName`, `ArityMismatch`, `UnknownDataCon`, `TypeMismatch`
- **Cross-module collision risk:** `low`
- **Mode:** `always-on`
- **Test coverage:** `tidepool-bridge/tests/roundtrip.rs`
- **Notes:** Handles standard cons-list. Candidate for `get_by_name_arity`.

## `Result<T, E>` (Either)

- **Location:** `tidepool-bridge/src/impls.rs:581` (`impl FromCore for Result<T, E>` / `impl ToCore for Result<T, E>`)
- **Constructor names looked up:** `"Right"`, `"Ok"`, `"Left"`, `"Err"`
- **Lookup strategy:** `get_by_name` (with `or_else` fallback between Haskell/Rust naming)
- **Assumed arity:** `1`
- **Failure mode on shape mismatch:** `UnknownDataConName` (Right/Ok or Left/Err), `ArityMismatch`, `UnknownDataCon`, `TypeMismatch`
- **Cross-module collision risk:** `medium`
- **Mode:** `always-on`
- **Test coverage:** `tidepool-bridge/tests/roundtrip.rs`
- **Notes:** Maps both `Either` (Right/Left) and `Result` (Ok/Err) to Rust `Result`. Candidate for `get_by_name_arity`.

## `(A, B)` (Pair)

- **Location:** `tidepool-bridge/src/impls.rs:641` (`impl FromCore for (A, B)` / `impl ToCore for (A, B)`)
- **Constructor names looked up:** `"(,)"`
- **Lookup strategy:** `get_by_name`
- **Assumed arity:** `2`
- **Failure mode on shape mismatch:** `UnknownDataConName`, `ArityMismatch`, `UnknownDataCon`, `TypeMismatch`
- **Cross-module collision risk:** `low`
- **Mode:** `always-on`
- **Test coverage:** `tidepool-bridge/tests/roundtrip.rs`
- **Notes:** Candidate for `get_by_name_arity("(,)", 2)`.

## `(A, B, C)` (Triple)

- **Location:** `tidepool-bridge/src/impls.rs:678` (`impl FromCore for (A, B, C)` / `impl ToCore for (A, B, C)`)
- **Constructor names looked up:** `"(,,)"`
- **Lookup strategy:** `get_by_name`
- **Assumed arity:** `3`
- **Failure mode on shape mismatch:** `UnknownDataConName`, `ArityMismatch`, `UnknownDataCon`, `TypeMismatch`
- **Cross-module collision risk:** `low`
- **Mode:** `always-on`
- **Test coverage:** `tidepool-bridge/tests/roundtrip.rs`
- **Notes:** Candidate for `get_by_name_arity("(,,)", 3)`.

## `serde_json::Value`

- **Location:** `tidepool-bridge/src/json.rs:22` (`impl ToCore for serde_json::Value`)
- **Constructor names looked up:** `"Null"`, `"Bool"`, `"Number"`, `"String"`, `"Array"`, `"Object"`
- **Lookup strategy:** `get_by_name_arity`
- **Assumed arity:** `0 for Null`, `1 for Bool/Number/String/Array/Object`
- **Failure mode on shape mismatch:** `UnknownDataConName`
- **Cross-module collision risk:** `low`
- **Mode:** `always-on`
- **Test coverage:** `tidepool-bridge/src/json.rs:tests`
- **Notes:** Already uses `get_by_name_arity` to avoid collisions with GHC-internal types like `Array`.

## `keymap_to_value` helper

- **Location:** `tidepool-bridge/src/json.rs:93` (`fn keymap_to_value`)
- **Constructor names looked up:** `"Data.Map.Bin"`, `"Data.Map.Tip"`, `"Bin"`, `"Tip"`, `"I#"`
- **Lookup strategy:** `get_by_qualified_name` | `get_by_name_arity` | `get_companion`
- **Assumed arity:** `5 for Bin`, `0 for Tip`, `1 for I#`
- **Failure mode on shape mismatch:** `UnknownDataConName`
- **Cross-module collision risk:** `low`
- **Mode:** `always-on`
- **Test coverage:** `tidepool-bridge/src/json.rs:tests`
- **Notes:** High-quality lookup strategy: tries qualified names first, then falls back to arity-based or companion lookup. Matches `Data.Map.Strict` heap representation.
