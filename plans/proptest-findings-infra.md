# Proptest Infrastructure Self-Test Findings (S4 oracle-the-oracle)

Oracle-the-oracle: property-based audit of tidepool-testing's own machinery — the
worklist comparators (`compare::values_equal`, `proptest::values_equal`,
`heap_to_value`), `TreeBuilder`, and the generator well-formedness/depth contracts.
Every differential verdict in the campaign routes through these helpers; a bug here
silently corrupts every suite downstream.

Suite: `tidepool-testing/tests/proptest_infra_selftest.rs`
(naive reference comparators + adversarial generators live inside the test file;
nothing in `src/` was modified).

## Confirmed bugs

### BUG-1 — `proptest::values_equal` equates heterogeneous pairs (FALSE-POSITIVE class)

`tidepool-testing/src/proptest.rs` `values_equal`: the worklist match handles
`(Lit,Lit)` and `(Con,Con)`, then falls through to `_ => {}` ("Closures, thunks,
join conts: skip"). The catch-all also matches every *heterogeneous comparable*
pair, so:

```rust
values_equal(&Value::Lit(LitInt(1)), &Value::Con(DataConId(0), vec![]))  // == true
values_equal(&Con(1,[Lit 1]), &Con(1,[Con(0,[])]))                       // == true (nested)
```

Any shape-level divergence — JIT returns a `Con` where eval returns a `Lit`, or
vice versa, at any position — is reported as EQUAL. This flows directly through
`check_jit_vs_eval` and `check_pass_preserves_eval`, i.e. the primary differential
oracles of the campaign.

Repro: `bug1_proptest_comparator_equates_lit_and_con` (`#[ignore]`).

### BUG-2 — `compare::values_equal` not reflexive on ByteArray (FALSE-NEGATIVE class)

`tidepool-testing/src/compare.rs` `values_equal`: `(ByteArray, ByteArray)` falls
into `_ => return false`, contradicting the doc comment ("ByteArrays are …
skipped/not comparable" — Closures and JoinConts with the same wording compare
equal). Consequence:

```rust
let v = Value::ByteArray(...);
values_equal(&v, &v.clone())   // == false — eq(a,a) violated
```

Amplifier: `heap_to_value` *manufactures* ByteArray sentinels on three paths —
depth > `MAX_HEAP_DEPTH` (1000), and the JIT's extended lit tags String/ByteArray
(`LitTag::from_byte` → `None`). So any heap-reconstructed value containing Text, a
string lit, or >1000 nesting compares unequal to anything — including a second
reconstruction of the *same* heap object. Suites comparing such values via
`compare::values_equal`/`assert_values_eq` report spurious B1s.

Repro: `bug2_compare_bytearray_not_reflexive` (`#[ignore]`).

### BUG-3 — `arb_core_expr_depth(d)` exceeds its depth cap by ~d (INVARIANT violation)

`gen/strategy.rs`: `gen_leaf`'s fallback for `Fun`/`Pair` types expands at
depth 0 into `Lam`/`Con` nodes *with children*. Crucially the overage is not a
constant: an `App` spine stacks function types `Fun(aₖ, Fun(aₖ₋₁, … ty))` —
one level per App — and when the fun position reaches the depth-0 frontier, the
whole accumulated stack collapses into a Lam chain at once. Worst case is
roughly **2d + type-nesting (~4)**, not d. Measured with an independent
edge-counting walker (300 samples each):

| d | max measured depth | overage |
|---|--------------------|---------|
| 3 | 8                  | +5      |
| 5 | 11                 | +6      |
| 7 | 14                 | +7      |

The maxima track ~2d exactly. The shrunk counterexamples confirm the mechanism
visually: `App { fun: <Lam→Lam→Lam chain>, … }` — collapsed Fun stacks.

W1's reach statistics ("depth 5", "depth 7") are therefore systematically
*under*-labeled — real trees run up to ~2× deeper than the parameter claims.

Repro: `bug3_generator_depth_violation` (`#[ignore]`, seed committed in
`tests/proptest_infra_selftest.proptest-regressions`). The live
`g4_generator_contracts` guards the characterized true bound (`≤ 2d + 8`).

### BUG-4 — `cbor_roundtrip_preserves_eval` misclassifies `(Err, Ok)` divergence

`gen/strategy.rs` tests, `cbor_roundtrip_preserves_eval`: the
`(Ok(_), Err(e))` arm panics ("roundtrip broke eval"), but the symmetric
`(Err(_), Ok(_))` arm is counted as `both_error` and *passes silently*. A CBOR
roundtrip that turns a failing program into a succeeding one is a roundtrip
non-identity (W1's own B5 class) and is currently invisible. Found by inspection;
in-src `#[cfg(test)]` code, so documented here rather than repro'd externally.

## Comparator inconsistencies (hazards, not single-sided bugs)

- **NaN semantics differ between the two src comparators**: `compare::lits_equal`
  treats any-NaN == any-NaN; `proptest::values_equal` uses derived `Literal` eq
  (bitwise). A JIT/eval NaN-payload difference passes `compare` but fails
  `proptest` suites. Characterized by a live (green) test.
- **ConFun**: `compare` compares tag/arity/args; `proptest` skips entirely
  (subsumed by BUG-1's catch-all).

## Verified negatives (infra that held up)

- `compare::values_equal` agrees with a naive recursive reference over equal /
  near-miss / shared-subtree adversarial classes (excluding the BUG-2 ByteArray
  class), and is symmetric: eq(a,b) == eq(b,a). The worklist pairing order is
  correct — wide+deep shared-subtree mixes with one deep mutation are caught.
- `proptest::values_equal` agrees with a strict-lit naive reference on
  homogeneous Lit/Con trees with shape-preserving mutations.
- `TreeBuilder`: push/push_tree preserve structure under random merge sequences;
  offsets exact; all indices in bounds.
- Generated exprs (`arb_core_expr_depth(5)`): child indices strictly backward,
  root == len-1, no orphan nodes (full reachability from root).
- `arb_ground_expr_depth(3)` (300 cases, ≥25 reaching comparison): where
  eval+deep_force succeed, results are never closure-valued (claim holds).
- CBOR roundtrip identity at depth 5 and weighted depth 7 (the old test capped
  at 3).

## Hygiene notes (proptest persistence footguns, found while seeding)

- `FileFailurePersistence::SourceParallel` (the default) **fails silently for
  integration tests** — it walks up looking for `lib.rs`/`main.rs`, which
  `tests/` files lack. Seeds were never being written. The selftest uses
  `WithSource("proptest-regressions")` (the convention the repo's committed
  `tidepool-codegen/tests/*.proptest-regressions` files already follow). Other
  suites constructing `TestRunner` directly may be silently non-persisting too.
- A stale seed in the shared per-file regressions file is replayed *first* by
  **every** runner in that file; if the replayed case fails, `run()` returns
  before reaching the new-failure persistence branch — so a stale entry can
  permanently mask seed persistence for later-failing properties.
- `*.proptest-regressions` is in `.gitignore` (line 27) — every committed seed
  in this repo required `git add -f`. Worth revisiting: the ignore rule
  contradicts both proptest's recommendation and the repo's own committed seed
  files.

## Trust verdict

The campaign's prior verdicts need one targeted re-examination and otherwise
stand. BUG-1 means every suite whose oracle is `proptest::check_jit_vs_eval` or
`check_pass_preserves_eval` could not have detected a *variant-shape* divergence
(Lit-vs-Con at any position); their green results prove agreement only up to
shape-class, not value. Literal-vs-literal and Con-vs-Con divergences (tags,
field counts, payloads) WERE reliably detected, which is the dominant divergence
mode actually observed (W1's D2 was Con-vs-Con and was caught), so prior B1
verdicts that *fired* are trustworthy; prior *green* runs are weaker than
advertised. BUG-2 biases in the safe direction (false alarms, not missed bugs) —
and W1 routed the deep differential through `run_pure` precisely to avoid
`heap_to_value`, so its depth-5/7 results are unaffected. BUG-3 mislabels W1's
reach statistics conservatively (real trees run up to ~2× deeper than claimed —
measured max 14 at d=7), so coverage claims survive and were in fact stronger
than advertised. Recommended fixes, in order: replace `proptest::values_equal`'s
catch-all with explicit non-comparable arms + `_ => return false`; make
`(ByteArray, ByteArray)` skip (equal) in `compare::values_equal` per its doc; fix
the `(Err, Ok)` arm in `cbor_roundtrip_preserves_eval`.
