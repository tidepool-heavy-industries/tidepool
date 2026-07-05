# Restore `Scientific`, cut `NumberI` — exact JSON numbers end-to-end

## Why (premise-reversal)

`b2fd5b9b` (2026-02-26) dropped `Data.Scientific` → `Double` because Integer/GMP
primops SIGILL'd on the JIT. `tidepool-bignum` (2026-06-20) fixed Integer-without-GMP
— **the premise is now false** (verified: `product [1..30] :: Integer` is exact on the
live JIT). `NumberI !Int` was only ever the Int64-capped workaround, and being a
non-aeson second `Value` constructor it makes idiomatic `case v of Number n -> …`
case-trap on integer fields (the friction that started this).

Goal: `Value` matches upstream aeson — one `Number !Scientific` constructor. Cut
`NumberI`. Exact numbers end-to-end.

## Viability — GREEN (spiked 2026-07-05)

- `serde_json` `arbitrary_precision` ENABLED (tidepool-eval/bridge/mcp Cargo.toml);
  needed so decode keeps the exact decimal token (`n.as_str()`) instead of i64/f64.
- Workspace `cargo check` clean with the flag on.
- No `serde(flatten)` / `serde(untagged)` anywhere (the classic arb-precision breakers).
- No `serde_json::from_value`-into-typed (the other breaker). All `from_value` are the
  custom `tidepool_bridge::FromCore` over our own `Value`.
- Number/json/serialization test slice: 26/26 pass with flag on, code unchanged.

## Heap contract (the shared interface — all sides must agree)

```
Number (Scientific coeff exp)
  = Con(Number,     [ Con(Scientific, [ <Integer>, Lit(LitInt exp) ]) ])
<Integer> = Con(IS, [Lit(LitInt n)])              -- |n| ≤ i64::MAX
          | Con(IP, [Lit(LitByteArray limbsLE)])  -- positive, magnitude
          | Con(IN, [Lit(LitByteArray limbsLE)])  -- negative, magnitude
limbsLE = little-endian u64 chunks, 8 bytes each, least-significant first
          (exact inverse of shapes.rs::bignat_bytes_to_decimal)
```
`Scientific` is a single-ctor arity-2 con (no record labels — keep it opaque).

## Workstreams

### Foundation (crux — do first, prove with round-trip test)
- `tidepool-eval/src/shapes.rs`: `pub fn integer_from_decimal(&str) -> Value`
  (IS for i64-range; else IP/IN limbs). Round-trip test vs `bignat_bytes_to_decimal`.
- Rewrite `json_number` → `scientific_from_number(n, ids)` building the contract above
  from `n.as_str()` (parse decimal → coefficient string + base10 exponent).

### Haskell
- NEW `haskell/lib/Tidepool/Aeson/Scientific.hs` — vendored minimal `Scientific`:
  `data Scientific = Scientific !Integer !Int` + `scientific`, `coefficient`,
  `base10Exponent`, `fromFloatDigits`, `toRealFloat`, `toBoundedInteger`,
  `floatingOrInteger`, Eq/Ord (normalized)/Show (aeson-style)/Num. Export via `Aeson.hs`.
- `Value.hs`: `Number !Scientific`; cut `NumberI`; fix `ToJSON Int/Double/Float/
  Integer/Word` (all → `Number` via `scientific`/`fromFloatDigits`, Integer exact).
- `FromJSON.hs`: `parseJSON (Number s)` → `toRealFloat`/`toBoundedInteger`; drop NumberI arms.
- `Lens.hs`: `_Number :: Prism' Value Scientific`; `_Int`/`_Integer`/`_Double` via
  Scientific projections; drop NumberI.
- `Prelude.hs`: `asInt`/`asDouble` via Scientific. `QQ/Json.hs`: literal path → `Number (scientific …)`.

### Rust (build/render/bridge)
- `tidepool-eval/src/json.rs`: `JsonConIds` — swap `number_i`/`number` for
  `number` + `scientific` + `is`/`ip`/`in` ids. `json_to_value` number arm → new builder.
- `tidepool-bridge/src/json.rs`: same number policy (unify on shapes.rs builder); cut NumberI table entries.
- `tidepool-runtime/src/render.rs`: `Number(Scientific coeff exp)` → JSON number token
  (render coeff Integer exactly × 10^exp, no f64). Drop the NumberI arm (188).
- `tidepool-codegen`: JsonDecode host fn follows json_to_value; ensure value_to_heap
  materializes Scientific/IS/IP/IN cons.

### Cut NumberI (mechanical sweep)
- `tidepool-mcp/src/eval_prep.rs`, `preamble.rs`, `lib.rs` tests; `tidepool-handlers/
  src/test_support.rs`; con-table fixtures. Grep `NumberI` → zero non-historical hits.

### Tests
- `tidepool-codegen/tests/json_decode_differential.rs`: NUMBER_I→SCIENTIFIC + IS/IP/IN
  cons; `render_value` Number arm reads Scientific; add a >i64 exact-int case
  (`12345678901234567890123456789`) asserting JIT≡eval≡exact string.
- `jit_surface.rs`: `works_*` probe for `_Number`/`_Int` + big-int exact decode.

## Verify
`cargo check --workspace` → `clippy` → `json_decode_differential` → GHC-heavy slice
(`--ignore-default-filter`) → full `jit_surface` → redeploy → live `eval` big-int round-trip.
