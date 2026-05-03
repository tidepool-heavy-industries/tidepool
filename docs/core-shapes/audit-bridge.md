# Audit: tidepool-bridge primitive impls

## `is_boxing_con` helper

- **Location:** `tidepool-bridge/src/impls.rs:10` (`fn is_boxing_con`)
- **Constructor names looked up:** variable `name` (caller passes `"I#"`, `"W#"`, `"D#"`, `"C#"`)
- **Lookup strategy:** `get_by_name`
- **Assumed arity:** N/A (predicate only)
- **Failure mode on shape mismatch:** Returns `false`.
- **Cross-module collision risk:** `medium`
- **Mode:** `always-on`
- **Test coverage:** `tidepool-bridge/tests/roundtrip.rs`
- **Notes:** Used by numeric and Char `FromCore` impls to unwrap GHC boxing constructors. Diverges from derive by using unqualified `get_by_name`.

## `PhantomData<T>`

- **Location:** `tidepool-bridge/src/impls.rs:37` (`impl FromCore for std::marker::PhantomData<T>`)
- **Constructor names looked up:** `"()"`
- **Lookup strategy:** `get_by_name` (ToCore), `iter().find(|dc| dc.rep_arity == 0)` fallback
- **Assumed arity:** `0`
- **Failure mode on shape mismatch:** `ArityMismatch`, `TypeMismatch`, `UnknownDataConName`
- **Cross-module collision risk:** `low`
- **Mode:** `always-on`
- **Test coverage:** `tidepool-bridge-derive/tests/phantom_data.rs`
- **Notes:** `ToCore` uses `"()"` but falls back to the first arity-0 constructor in the table if missing.

## `()` (Unit)

- **Location:** `tidepool-bridge/src/impls.rs:91` (`impl ToCore for ()` / `impl FromCore for ()`)
- **Constructor names looked up:** `"()"`
- **Lookup strategy:** `get_by_name`
- **Assumed arity:** `0`
- **Failure mode on shape mismatch:** `UnknownDataConName`, `TypeMismatch`
- **Cross-module collision risk:** `low`
- **Mode:** `always-on`
- **Test coverage:** `tidepool-bridge/src/impls.rs:tests::test_unit_roundtrip`
- **Notes:** Candidate for `get_by_name_arity("()", 0)`.

## `i64` (Int)

- **Location:** `tidepool-bridge/src/impls.rs:110` (`impl FromCore for i64` / `impl ToCore for i64`)
- **Constructor names looked up:** `"I#"`
- **Lookup strategy:** `get_by_name`
- **Assumed arity:** `1`
- **Failure mode on shape mismatch:** `TypeMismatch`, `UnknownDataConName`
- **Cross-module collision risk:** `medium`
- **Mode:** `always-on`
- **Test coverage:** `tidepool-bridge/tests/roundtrip.rs`
- **Notes:** Transparently unwraps `I#` box or accepts `LitInt`. Related to `i32` and `char` impls.

## `u64` (Word)

- **Location:** `tidepool-bridge/src/impls.rs:135` (`impl FromCore for u64` / `impl ToCore for u64`)
- **Constructor names looked up:** `"W#"`
- **Lookup strategy:** `get_by_name`
- **Assumed arity:** `1`
- **Failure mode on shape mismatch:** `TypeMismatch`, `UnknownDataConName`
- **Cross-module collision risk:** `medium`
- **Mode:** `always-on`
- **Test coverage:** `tidepool-bridge/tests/roundtrip.rs`
- **Notes:** Transparently unwraps `W#` box or accepts `LitWord`.

## `f64` (Double)

- **Location:** `tidepool-bridge/src/impls.rs:160` (`impl FromCore for f64` / `impl ToCore for f64`)
- **Constructor names looked up:** `"D#"`
- **Lookup strategy:** `get_by_name`
- **Assumed arity:** `1`
- **Failure mode on shape mismatch:** `TypeMismatch`, `UnknownDataConName`
- **Cross-module collision risk:** `medium`
- **Mode:** `always-on`
- **Test coverage:** `tidepool-bridge/tests/roundtrip.rs`
- **Notes:** Transparently unwraps `D#` box or accepts `LitDouble`.

## `bool`

- **Location:** `tidepool-bridge/src/impls.rs:210` (`impl FromCore for bool` / `impl ToCore for bool`)
- **Constructor names looked up:** `"True"`, `"False"`
- **Lookup strategy:** `get_by_name`
- **Assumed arity:** `0`
- **Failure mode on shape mismatch:** `UnknownDataConName`, `ArityMismatch`, `UnknownDataCon` (if tag found but name doesn't match True/False), `TypeMismatch`
- **Cross-module collision risk:** `low`
- **Mode:** `always-on`
- **Test coverage:** `tidepool-bridge/tests/roundtrip.rs`
- **Notes:** Candidate for `get_by_name_arity`. Collision risk is low as True/False are standard.

## `char`

- **Location:** `tidepool-bridge/src/impls.rs:263` (`impl FromCore for char` / `impl ToCore for char`)
- **Constructor names looked up:** `"C#"`
- **Lookup strategy:** `get_by_name`
- **Assumed arity:** `1`
- **Failure mode on shape mismatch:** `TypeMismatch`, `UnknownDataConName`
- **Cross-module collision risk:** `medium`
- **Mode:** `always-on`
- **Test coverage:** `tidepool-bridge/tests/roundtrip.rs`
- **Notes:** Transparently unwraps `C#` box or accepts `LitChar`.

## `String` (Text)

- **Location:** `tidepool-bridge/src/impls.rs:289` (`impl FromCore for String` / `impl ToCore for String`)
- **Constructor names looked up:** `"Text"`, `"ByteArray"`, `"[]"`, `":"`, `"C#"`
- **Lookup strategy:** `get_by_name`
- **Assumed arity:** `3 for Text`, `1 for ByteArray`, `0 for []`, `2 for :`, `1 for C#`
- **Failure mode on shape mismatch:** `TypeMismatch`, `UnknownDataConName`, `InternalError` (mutex poisoned)
- **Cross-module collision risk:** `high`
- **Mode:** `always-on`
- **Test coverage:** `tidepool-bridge/tests/roundtrip.rs`, `tidepool-bridge/tests/proptest_text.rs`
- **Notes:** Complex impl handling both unboxed `Text` worker format and cons-cell `[Char]` list format. Uses multiple `get_by_name` lookups. Should migrate to `get_by_name_arity` for all lookups.

## `Option<T>`

- **Location:** `tidepool-bridge/src/impls.rs:384` (`impl FromCore for Option<T>` / `impl ToCore for Option<T>`)
- **Constructor names looked up:** `"Nothing"`, `"Just"`
- **Lookup strategy:** `get_by_name`
- **Assumed arity:** `0 for Nothing`, `1 for Just`
- **Failure mode on shape mismatch:** `UnknownDataConName`, `ArityMismatch`, `UnknownDataCon`, `TypeMismatch`
- **Cross-module collision risk:** `low`
- **Mode:** `always-on`
- **Test coverage:** `tidepool-bridge/tests/roundtrip.rs`
- **Notes:** Candidate for `get_by_name_arity`.

## `Vec<T>` (List)

- **Location:** `tidepool-bridge/src/impls.rs:446` (`impl FromCore for Vec<T>` / `impl ToCore for Vec<T>`)
- **Constructor names looked up:** `"[]"`, `":"`
- **Lookup strategy:** `get_by_name`
- **Assumed arity:** `0 for []`, `2 for :`
- **Failure mode on shape mismatch:** `UnknownDataConName`, `ArityMismatch`, `UnknownDataCon`, `TypeMismatch`
- **Cross-module collision risk:** `low`
- **Mode:** `always-on`
- **Test coverage:** `tidepool-bridge/tests/roundtrip.rs`
- **Notes:** Handles standard cons-list. Candidate for `get_by_name_arity`.

## `Result<T, E>` (Either)

- **Location:** `tidepool-bridge/src/impls.rs:513` (`impl FromCore for Result<T, E>` / `impl ToCore for Result<T, E>`)
- **Constructor names looked up:** `"Right"`, `"Ok"`, `"Left"`, `"Err"`
- **Lookup strategy:** `get_by_name` (with `or_else` fallback between Haskell/Rust naming)
- **Assumed arity:** `1`
- **Failure mode on shape mismatch:** `UnknownDataConName` (Right/Ok or Left/Err), `ArityMismatch`, `UnknownDataCon`, `TypeMismatch`
- **Cross-module collision risk:** `medium`
- **Mode:** `always-on`
- **Test coverage:** `tidepool-bridge/tests/roundtrip.rs`
- **Notes:** Maps both `Either` (Right/Left) and `Result` (Ok/Err) to Rust `Result`. Candidate for `get_by_name_arity`.

## `(A, B)` (Pair)

- **Location:** `tidepool-bridge/src/impls.rs:583` (`impl FromCore for (A, B)` / `impl ToCore for (A, B)`)
- **Constructor names looked up:** `"(,)"`
- **Lookup strategy:** `get_by_name`
- **Assumed arity:** `2`
- **Failure mode on shape mismatch:** `UnknownDataConName`, `ArityMismatch`, `UnknownDataCon`, `TypeMismatch`
- **Cross-module collision risk:** `low`
- **Mode:** `always-on`
- **Test coverage:** `tidepool-bridge/tests/roundtrip.rs`
- **Notes:** Candidate for `get_by_name_arity("(,)", 2)`.

## `(A, B, C)` (Triple)

- **Location:** `tidepool-bridge/src/impls.rs:627` (`impl FromCore for (A, B, C)` / `impl ToCore for (A, B, C)`)
- **Constructor names looked up:** `"(,,)"`
- **Lookup strategy:** `get_by_name`
- **Assumed arity:** `3`
- **Failure mode on shape mismatch:** `UnknownDataConName`, `ArityMismatch`, `UnknownDataCon`, `TypeMismatch`
- **Cross-module collision risk:** `low`
- **Mode:** `always-on`
- **Test coverage:** `tidepool-bridge/tests/roundtrip.rs`
- **Notes:** Candidate for `get_by_name_arity("(,,)", 3)`.

## `serde_json::Value`

- **Location:** `tidepool-bridge/src/json.rs:17` (`impl ToCore for serde_json::Value`)
- **Constructor names looked up:** `"Null"`, `"Bool"`, `"Number"`, `"String"`, `"Array"`, `"Object"`
- **Lookup strategy:** `get_by_name_arity`
- **Assumed arity:** `0 for Null`, `1 for Bool/Number/String/Array/Object`
- **Failure mode on shape mismatch:** `UnknownDataConName`
- **Cross-module collision risk:** `low`
- **Mode:** `always-on`
- **Test coverage:** `tidepool-bridge/src/json.rs:tests`
- **Notes:** Already uses `get_by_name_arity` to avoid collisions with GHC-internal types like `Array`.

## `keymap_to_value` helper

- **Location:** `tidepool-bridge/src/json.rs:75` (`fn keymap_to_value`)
- **Constructor names looked up:** `"Data.Map.Bin"`, `"Data.Map.Tip"`, `"Bin"`, `"Tip"`, `"I#"`
- **Lookup strategy:** `get_by_qualified_name` | `get_by_name_arity` | `get_companion`
- **Assumed arity:** `5 for Bin`, `0 for Tip`, `1 for I#`
- **Failure mode on shape mismatch:** `UnknownDataConName`
- **Cross-module collision risk:** `low`
- **Mode:** `always-on`
- **Test coverage:** `tidepool-bridge/src/json.rs:tests`
- **Notes:** High-quality lookup strategy: tries qualified names first, then falls back to arity-based or companion lookup. Matches `Data.Map.Strict` heap representation.
