# Bug #3: empty-Text consumption in the tree-walker (eval oracle)

**Status:** characterized 2026-06-22. **Downgraded** from the memory note's framing.

## Key finding — JIT (production) is FINE
Confirmed live via the MCP server (JIT):
```
T.null  (T.pack "")  -> true        T.uncons (T.pack "") -> Nothing
T.unpack(T.pack "")  -> ""          (T.pack "" == T.pack "") -> true
T.length(T.pack "")  -> 0
```
All correct. So this is **not a production bug** — it only affects the
**tree-walking interpreter** (`tidepool-eval`), which is test infrastructure
(the differential oracle + `haskell_suite` fixtures), not the MCP eval path.

## Impact (reduced)
Empty-Text programs that the JIT runs correctly may **eval-Err** (TypeMismatch
or a 2 MB overflow) in the tree-walker. Under the JIT-vs-eval differential
that surfaces as an `EvalErr` finding / divergence rather than a clean match —
i.e. it dents *oracle fidelity*, it does not affect users. The standing
workaround (memory `empty-text-interp-string-space`) — author JIT parsers in
String space, `T.pack` only at leaves — remains valid but is now known to be a
*defensive* habit, not a correctness necessity on the JIT.

## Mechanism (to pin before any fix)
The tree-walker models an empty `Text` (`T.pack ""`) as `Value::Lit(LitString([]))`,
whereas non-empty Text / the vendored `Data.Text` consumers (`null`/`uncons`/
`unpack`/`Eq`) case-match the real `Text arr# off# len#` constructor shape. The
bare `LitString` doesn't match → TypeMismatch (or a degenerate loop → overflow).
NOT yet reproduced in an isolated eval-side test — do that first.

## Proposed direction (if we decide oracle fidelity is worth it)
Make the tree-walker model Text **consistently**: either (a) always realize
`T.pack`/string-literal Text as the same `Text arr off len` Con the consumers
expect (so empty is `Text <empty-arr> 0 0`, not `LitString []`), or (b) teach
the Text-consuming builtins to accept a `LitString` uniformly (empty included).
(a) is more principled (single representation); (b) is more localized.

## Recommendation
**Lower priority than #1.** It's oracle-only and the JIT is correct. Fix when
hardening the differential oracle (cf. `plans/` real-core corpus work), not as
a user-facing bug. Gate any fix on `haskell_suite` + the JIT-vs-eval proptests.
