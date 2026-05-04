# Haskell Verified Proptests

This directory contains a property-testing framework that cross-validates Tidepool's JIT compilation and execution against Rust's native evaluation. It generates random, valid Haskell source snippets paired with their expected `serde_json::Value` (computed natively in Rust), then compiles and runs the Haskell source via the `compile_and_run_pure` engine to ensure the outputs match identically.

## Architecture

The framework relies on the `proptest` crate for generating inputs. Each template uses the following structure:
1.  **Generator:** A function returning `impl Strategy<Value = (String, serde_json::Value)>`. The generator produces tuples of `(haskell_source_code, expected_json_value)`.
2.  **Runner:** A `#[test]` function that invokes `run_template(cases, generator)`. This runner spins up the `compile_and_run_pure` environment, feeds it the generated source, and asserts structural equality between the JIT's JSON-serialized output and the expected JSON value.

Example:
```rust
fn gen_fmap_maybe() -> impl Strategy<Value = (String, serde_json::Value)> {
    (arb_int(), proptest::option::of(arb_int())).prop_map(|(n, maybe_m)| {
        let src = match maybe_m {
            Some(m) => format!("(fmap (+({})) (Just ({}) :: Maybe Int))", n, m),
            None => format!("(fmap (+({})) (Nothing :: Maybe Int))", n),
        };
        let expected = match maybe_m {
            Some(m) => json!(m + n),
            None => json!(null),
        };
        (src, expected)
    })
}
```

## Adding a New Template

1.  **Create a generator:** Write an `impl Strategy` function in the appropriate category module (`fmap.rs`, `text.rs`, etc.).
2.  **Compute the expected value natively:** The expected value MUST be computed by standard Rust operations (e.g., `iterator.sum()`, `string.to_ascii_uppercase()`), not via `tidepool_eval` or tree-walking. This ensures an independent oracle.
3.  **Avoid problematic bounds:** Do not use `f32`/`f64` because precision and `Show` differences lead to false positives. Restrict text characters to ASCII to bypass Unicode case-folding divergence between `std::char` and GHC's `Data.Char`.
4.  **Register the test:** Provide a standard `#[test]` function wrapping the generator with `run_template(50, ...)` inside your module.

## Cache Strategy

The underlying test harness relies heavily on Tidepool's CBOR caching layer. Because GHC compilation is expensive, we rely on bounded parameters to improve cache hit rates. By constraining integers to `[-100, 100]`, limiting lists to small lengths, and picking from specific separator chars, the proptests achieve overlapping cache keys. Consequently, the first execution of the suite warms the cache (taking minutes), but subsequent runs execute rapidly.
