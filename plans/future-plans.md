# Future Plans / Idea Backlog

Steered 2026-06-11. Status legend: **GO** (approved direction), **BACKLOG**
(wait for a compelling trigger), **TODO** (small, do when convenient).

## A. Effect-log suspension (resume = replay) — BACKLOG (until a use case)

Effects are the only nondeterminism in the system (locked architecture), so a
suspended computation serializes as `(source hash, [effect responses so far])`
— no heap/closure serialization. Resume = recompile from cache + re-run,
feeding logged responses instead of re-dispatching until the log runs dry;
execution is bit-identical back to the suspend point.

- ~200 lines: response log in the machine loop + `resume_from_log`.
- Unlocks: durable suspended `ask`s (resume days later / another machine),
  forkable apertures (same log, different final answer → decision search
  trees), migration.
- Design note: replay feeds recorded responses (side effects do NOT re-fire),
  but a crash mid-effect resumes at-least-once — effects need an idempotence
  flag (`Exec` is the dangerous one).
- Trigger to revisit: any user story needing long-lived asks, workflow
  durability, or speculative double-resume.

## B. Content-addressed Core, stage 1 — GO (kills the #313 bug class)

Hash the canonical form of each top-level RHS (de Bruijn-ized binders for
alpha-equivalence; hash SCC groups for rec bindings, the Unison move) and use
it as the binding's VarId in Translate.hs. A collision then MEANS identical
code — harmless dedup instead of wrong-continuation resumption.

- Subsumes S6's load-time detector (keep it as belt-and-braces).
- Scope: one pass in Translate.hs (canonical print + fingerprint), days-scale.
- Stage 2 (separate decision): per-binding compile-cache keyed by content hash
  → incremental recompilation; also dissolves the S5 F3-class staleness for
  the Haskell-source half. Week-scale cache restructure.

## C. `compile_to_callable` — GO (sketched below)

Compile a named PURE function binding and hand Rust an `impl Fn(A) -> B`
(ToCore in → JIT call → FromCore out). The missing sibling of
`compile_and_run`; bridge traits already exist in both directions.

Use cases (sketches):

1. **Streaming transducers / lazy-IO killer.** `rust_iter.filter(jit_pred)`,
   `map(jit_fn)` — Haskell owns per-element logic, Rust owns the loop. MCP
   `sift`/`triage` over unbounded logs without materializing; kills the need
   for any lazy-IO story on the Haskell side.
2. **Hot-loop application inside the MCP itself.** Today every refinement of
   a large result set is another eval round-trip. A compiled predicate gets
   applied Rust-side over the full ast-grep/glob result set (pagination
   filters, census shaping) at JIT speed, one round-trip total.
3. **Typed rules engines for host apps.** A Rust service hot-loads business
   logic (routing rules, alert thresholds, validators) as pure Haskell. The
   capability story is maximal: a PURE function cannot touch the world at
   all — safer than Lua/WASM plugins by construction, and typed.
4. **Data-pipeline kernels.** parseCsv/arrow rows on the Rust side, a
   compiled per-row Haskell function mapped over millions of rows; derived
   columns, sort keys (`sortByColumn` with a compiled key extractor),
   group-by discriminators.
5. **Test oracles.** Property tests call Haskell reference implementations
   directly as `Fn` — W6's pure-case harness would have been ~half its size
   (no subprocess/JSON plumbing for pure comparisons).

Implementation sketch: `compile_to_callable::<A, B>(src, fn_name, includes)`
→ compiles via the normal pipeline, resolves the binding to a JIT entry that
takes one boxed arg, returns a handle owning (machine, entry ptr); call =
`A::to_value` → value_to_heap → enter → heap_to_value_forcing → `B::from_value`.
Pure bindings only (reject `Eff` types at compile, like IOTypeDetected).
GC note: each call's allocations live in the handle's nursery; decide
reset-per-call vs persistent (reset-per-call is simplest and safe for pure).

## D. Heap verifier (fail-loud invariant mode) — GO, next hands-on item

`TIDEPOOL_HEAP_VERIFY=1`, hooked after gc-compact: walk every object —
valid tag; size ↔ arity consistency for Cons; field pointers in-heap and
aligned; known lit tags (catches S3-C3 drift); valid thunk states; BLACKHOLE
captures visible (catches S3-C6); byte-array capacity-word sanity. Tests run
with it ON; production pays nothing. Turns the S3/W3 silent-corruption class
into loud failures at the first GC after the corruption.

## E. Divergence debugger (minimal) — BACKLOG

Trace flag → both machines (eval + JIT) keep a ring buffer of
`(step, value-hash)`; on differential failure print the first differing
index. Localizes future #313-style hunts to one rerun. A tool, not a
subsystem; build when the next gnarly divergence shows up.

## G. JSON decode for evals (vendored-aeson gap) — TODO (small)

The vendored aeson is construction-only (ToJSON; parser stripped, PR #144). An
eval can BUILD JSON freely and CONSUME the `input` lane via lens
(`^?`/`^..`/`_String`/`as*`), but there is no way to turn an arbitrary JSON
**string** (from `readFile`/`run` output) into a `Value` — the only parse path
is `httpGet`/`httpPost`, which already `serde_json::from_str` the body Rust-side
(`tidepool/src/main.rs` `parse_response`). Note: `httpGet` already returns a
parsed `Value`, so an `httpGetJSON` variant would be a no-op duplicate.

- **Minimal bandaid:** expose that same Rust-side parse as a standalone verb —
  `parseJson :: Text -> M Value` (+ `tryParseJson :: Text -> M (Either Text Value)`).
  One new effect constructor mirroring `HttpGet`'s structure (decl in
  `tidepool-mcp/src/lib.rs` + handler in `tidepool/src/main.rs`); reuses the
  existing `serde_json::from_str`. Closes the gap; lens consumes the result.
- **Bigger (separate):** typed decode — re-add `FromJSON` + `fromJSON ::
  Value -> Result a` + `Result(..)` (pure Haskell, operates on an existing
  `Value`, no text-parser risk) so records decode without hand-lensing every
  field. Generic deriving is the nice-but-heavier part. Do only if the lens
  path proves too verbose in practice.
- Trigger: any eval that needs to parse local/command-produced JSON.

## F. Retire the eager effect-results path — TODO (after soak)

The lazy path is default-on and W4 exonerated it across a 33-cell
consumption matrix. The eager kill-switch is the only reason the 100k node
cap, the 2000-node probe/dismantle spine logic, and the one documented
mode-divergence exist (it already cost W6 a harness flake). After a few more
weeks of soak: delete eager → delete the cap → delete the probe → delete the
divergence. Negative-line-count change; cite `plans/proptest-findings-lazy.md`
as the safety evidence.
