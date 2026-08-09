# Receipt — dev-structural-codec (PRD 18 gate 1(b))

## Verdict: GO

Both a list-carrying record-and-sum and a genuinely recursive ADT whose
recursion goes through a list round-trip through a hand-rolled
`GHC.Generics` structural codec, on the real `tidepool-extract` +
Cranelift JIT pipeline — not merely typechecked, but executed: the
round-tripped value is reconstructed by `decodeS` running on the JIT and
compared for equality by JIT-executed code (`roundTrips`), and separately
the raw `encodeS` shape is asserted against an expected `serde_json::Value`
built from a value that also ran on the JIT.

The self-referential dictionary the gate exists to test —
`Structural Plan` needing `Structural [Plan]` needing `Structural Plan`,
arising from `data Plan = Step Text | Seq [Plan]` — elaborates and runs
correctly. No named limit; no case-trap; no extract diagnostic.

## What was built

- `haskell/lib/Tidepool/Agent/CodecSpike.hs` — a standalone `GHC.Generics`
  traversal (`GStructural`/`GStructuralSum`/`GStructuralProd`), deliberately
  NOT built on `Tidepool.Aeson.Value.ToJSON`/`Tidepool.Aeson.FromJSON`'s own
  generic defaults. Those reject every sum type with a non-nullary
  constructor at compile time ("deriving ... via GHC.Generics supports
  single-constructor records only") — confirmed by reading
  `haskell/lib/Tidepool/Aeson/Value.hs`/`FromJSON.hs` before writing this
  module — so neither `WorkerResult` (a sum of two records) nor `Plan` (a
  sum of positional constructors) could derive through the existing
  vendored machinery. This module uses base `GHC.Generics` directly, per
  the spec's boundary (no dependency on the generic-surface lane's
  Symbol-metadata substrate, which is still mid-fold and off-limits).
- `tidepool-runtime/tests/agent_structural_codec.rs` — 10 tests driving the
  codec through `EvalHarness::run_pure` (real extract + JIT).

## Structural value chosen: aeson-style `Value`

Used `Tidepool.Aeson.Value.Value` (the existing vendored six-constructor
JSON `Value` type), per the spec's stated pragmatic default: it is what
backends speak, `tidepool_agent::seam::DynamicToolDeclaration` already
carries `serde_json::Value`, and `EvalResult::to_json()`
(`tidepool-runtime/src/render.rs:228-234`) already special-cases this exact
`Value` type for transparent passthrough to `serde_json::Value` — meaning a
Haskell `Value` returned as an eval target renders as the REAL encoded JSON
shape in the Rust test, not through the generic (and, per item 8 of the
mistake ledger, inconsistent) `value_to_json` constructor-name renderer
used for arbitrary Haskell values. That let the Rust tests assert on exact
wire shapes (`worker_result_encodes_to_uniform_tagged_shape`,
`plan_nested_depth_two_encodes_correctly`), not just round-trip booleans —
stronger evidence that reconstruction, not typechecking, is what passed.
A dedicated `StructuralValue` was considered and rejected: it would only
duplicate this six-constructor type for no expressive gain here.

## Encoding: one shape for every constructor form

Every constructor — nullary, positional, or record, of any arity — encodes
to the SAME shape:

```json
{"tag": "<constructor name>", "fields": [<field values, in declaration order>]}
```

A nullary constructor is simply the `fields: []` case of the same
traversal; it is never special-cased into a bare string. Record field names
are not carried on the wire (that would require a second, differently
shaped encoding for the positional/nullary cases this codec also has to
support); field ORDER — read back by the exact same `GHC.Generics` walk
that wrote it — is what makes `decodeS` the inverse of `encodeS` by
construction, not a hand-maintained pairing. This was a deliberate response
to `plans/post-restart/codex-review-2026-08-08.md` item 8's ledger (nullary
constructors becoming bare strings while records become `_con` and
positional constructors a third shape — three shapes for three constructor
forms in that codec).

Decode is loud, not silently coercive: an unrecognized tag, a wrong
top-level JSON kind, a missing `"tag"`/`"fields"` key, or a field-count
mismatch against a constructor's actual arity all return a descriptive
`Left`, never a sentinel value, a truncated/padded reconstruction, or a
default. Two tests exercise this directly (`unknown_tag_is_a_loud_decode_error`,
`wrong_field_count_is_a_loud_decode_error`).

## Shapes proven, on the real JIT

1. **List-carrying record-and-sum** — PRD 18's own
   `WorkerResult = Completed { summary :: Text, caveats :: [Text] } | Blocked { blocker :: Text, evidence :: [Text] }`.
   Both constructors are records; both round-trip
   (`worker_result_completed_round_trips`,
   `worker_result_blocked_round_trips`), and their wire shapes are asserted
   identical in form (`worker_result_encodes_to_uniform_tagged_shape`).
2. **Genuinely recursive ADT, recursion through a list** —
   `data Plan = Step Text | Seq [Plan]`. A leaf round-trips
   (`plan_leaf_round_trips`); the self-referential
   `Structural Plan`/`Structural [Plan]` dictionary elaborates and executes
   correctly.
3. **Edges**:
   - Empty list on a record field (`worker_result_empty_list_round_trips`,
     `Completed _ []`).
   - Empty list directly on the recursive knot
     (`plan_empty_seq_round_trips`, `Seq []`) — the sharper version of the
     empty-container edge, since it sits on the same list that carries the
     recursion rather than a leaf field.
   - Value nested two levels deep
     (`plan_nested_depth_two_round_trips`,
     `Seq [Step "a", Seq [Step "b", Step "c"]]`), with the exact recursive
     wire shape asserted (`plan_nested_depth_two_encodes_correctly`) to rule
     out a truncated/flattened encode silently agreeing with itself on the
     round-trip check alone.

## Named limits

None found. No case-trap, no extract diagnostic, no JIT trap. The codec
does not currently handle `Maybe`, `Either`, numeric leaves, or `Value`
passthrough as field types — this was a deliberate minimality choice (the
spec: "keep it minimal and honest ... not shipping a finished codec") since
none of the three required shapes need them, not a discovered limitation.
A consumer needing those leaf types adds `Structural` instances for them
the same way `Structural Text` and `Structural [a]` are defined here; no
change to the `GStructural`/`GStructuralSum`/`GStructuralProd` traversal
itself is implied.

## Test counts (real extract/JIT, tier 2 battery)

```
scripts/battery.sh -p tidepool-runtime -E 'binary(agent_structural_codec)'
```

10 tests run: **10 passed, 0 skipped, 0 failed** (`tidepool-runtime::agent_structural_codec`, nextest run ID `b03ae741-5172-4d76-a1ec-310bd5423fa5`).

| Test | Result |
|---|---|
| `plan_leaf_round_trips` | PASS |
| `plan_empty_seq_round_trips` | PASS |
| `plan_nested_depth_two_encodes_correctly` | PASS |
| `unknown_tag_is_a_loud_decode_error` | PASS |
| `worker_result_blocked_round_trips` | PASS |
| `plan_nested_depth_two_round_trips` | PASS |
| `worker_result_empty_list_round_trips` | PASS |
| `worker_result_encodes_to_uniform_tagged_shape` | PASS |
| `worker_result_completed_round_trips` | PASS |
| `wrong_field_count_is_a_loud_decode_error` | PASS |

`cargo check --workspace`: clean. `cargo clippy --workspace --all-targets`:
clean on the new files (pre-existing warnings elsewhere in the workspace
are unrelated to this change). `cargo fmt --all -- --check`: clean.

## Coordination note for dev-mode-encoding

No codec overlap materialized: this lane's `WorkerResult`/`Plan` are
private proof types local to `CodecSpike.hs`, not exported for
`compileTools` to consume directly. dev-mode-encoding's gate only needs a
shallow schema for its `compileTools` leaves per the lane boundary; nothing
here needs to be shared or reconciled with `Contract.hs`.
