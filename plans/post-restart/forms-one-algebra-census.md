# FormShape is the one operator-presentation algebra — census + verdicts

Approved direction (Inanna, 2026-08-10). Three mechanisms answered "show a
human a typed question"; after this lane there is one.

This file is the census the cuts were made from. It is a receipt, not a
standing design document: once the CLAUDE.md files describe the one-algebra
world, this exists to explain *why each thing died*.

## The survivor

**`FormShape`** (`haskell/lib/Tidepool/Form/Shape.hs` ↔
`tidepool-harness/src/selfharness/operator.rs`) — derived from a type's own
`Generic` metadata by `Tidepool.Form.GForm`, shipped over `AskUserWith` by
`Tidepool.Form.Wire`, rendered recursively by `tidepool-web`'s
`render::generic_shape`, answered as plain JSON decoded by the answer type's
own `FromJSON`. Live, load-bearing, sole algebra.

## Verdicts

| Thing | Producers | Consumers | Verdict |
|---|---|---|---|
| `FormSpec` (Rust, single field `shape`) | `engine::decode_askuser_spec` only | every consumer projects `.shape` immediately; `StdinGate` ignores it | **DELETE** — `present_form(&FormShape)` |
| `Tidepool.Ui` (Haskell eDSL) | nothing live | `Tidepool.FormQQ`, `Tidepool.QQ` re-export, one `jit_surface` probe | **DELETE** |
| `tidepool-harness/src/ui.rs` (Rust wire mirror) | `uiof::ui_of` only | `uiof` only | **DELETE** |
| `Tidepool.FormQQ` + `.Parse` (`[form\|]`) | nothing live | expands to `[Ui]`, which nothing renders | **DELETE** (coverage ported — see below) |
| `uiof::ui_of` / `resume_expr_from_submission` / `defining_module` | `Harness::pending_derived_ui`, `Harness::answer_mechanical` | those two are `pub` but called ONLY from tests | **DELETE** |
| `uiof::type_synopsis` | `engine::type_shape_line` (hole-card text) | LIVE on every hole card | **KEEP** — module renamed `synopsis.rs` |
| `HoleRouting::Dialog { ui: Json }` | never produced (`classify_hole` has no arm; `dialogAsk` is deleted and `driver.rs` asserts it never reappears) | two defensive match arms | **DELETE** |

## Why `uiof` is deleted rather than re-derived onto `FormShape`

The offered alternative was "one algebra, two derivation sources": re-derive a
`FormShape` from the compiled `DataConTable` instead of a `Ui`. Rejected on
two grounds, both census facts:

1. **No live consumer.** `pending_derived_ui` and `answer_mechanical` are
   reachable only from `tests/uiof_mechanical.rs` and one case in
   `tests/acceptance_run_llm_turn.rs`. No driver, no web handler, no MCP
   surface, and no plan document calls them. The hole card that *is* live
   (`engine::hole_card`) uses `type_synopsis`, a different function — the
   "hole-card consumer" of `ui_of` never existed.
2. **The table cannot express the algebra.** `DataConTable` captures field
   LABELS but not field TYPES (`uiof.rs`'s own module doc says so, and
   `plans/self-iterating-harness/15-generic-surface-wave.md` records it as a
   known shallowness). A `FormShape` derived from it could only ever emit
   `StringShape` for every field. That is a strictly degraded second producer
   of the shared algebra — it would make "one algebra" mean "one algebra plus
   a lossy impostor", for zero live callers.

Deleting is the honest move; `type_synopsis` already carries the one thing the
table can truthfully say (constructor and selector names) and stays.

## Stdlib-QuasiQuoter survival coverage

`haskell/CLAUDE.md` documented `works_form_qq`
(`tidepool-runtime/tests/jit_surface.rs`) as the proof that a QuasiQuoter
DEFINED IN THE STDLIB — not shipped with GHC — survives the extract pipeline
end to end, splice evaluation and all. That proof does not die with `[form|]`.
It is carried by `works_stdlib_quoter_survives_extract` and
`stdlib_quoter_bad_input_fails_loudly_at_compile_time`, built on
`Tidepool.QQ`'s `[uri|]` — also stdlib-defined, also compile-time-validating,
so both halves of the original proof (a successful splice through the real
pipeline, and a malformed one failing loudly at COMPILE time) are preserved.

## Wire shapes that changed (both were explicitly unfrozen)

- `GET /api/form` — `{"form": {"shape": …}}` flattens to `{"form": …}`.
- `Event::FormPresented` in `transcript.jsonl` — the `spec` field is now the
  bare shape, not `{"shape": …}`.
