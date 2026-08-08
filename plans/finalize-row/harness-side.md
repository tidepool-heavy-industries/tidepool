# Spec — harness side of the row-indexed `Finalize`

Working spec for the dev implementing the harness half. Deleted when the work
lands; the durable record is `tidepool-harness/CLAUDE.md` plus the commit
messages.

## What is already done (on your base commit)

`Finalize` is type-indexed. In the generated `Tidepool.Effects`:

```haskell
data NoAnswer
data Finalize v a where
  FinalizeWith :: Int -> v -> Finalize v a
finalize      :: forall v a effs. Member (Finalize v) effs => v -> Eff effs a
finalizeSited :: forall v a effs. Member (Finalize v) effs => Int -> v -> Eff effs a
type M = Eff '[AskUser, Fork, Finalize Decision]   -- when a compile names Decision
type M = Eff '[AskUser, Fork, Finalize NoAnswer]   -- when it names nothing
```

`Member (Finalize T)` — the row — is now the whole pin. The mechanism is
PROVEN through the real extract; do not re-litigate it, implement it:

- `finalize @Decision (Decision {...}) :: M ()` compiles, spelling unchanged.
- `finalize @Text ("oops" :: Text)` against a `Decision` row fails with
  `'Finalize Text' is not a member of the type-level list
  '[AskUser, Fork, Finalize Decision]' In the constraint (Member (Finalize Text) ...)`.
- Inferred (no type application) fails identically.
- `asks.json` still records `{0: "Decision"}` — extract needs no change.
- A function-typed answer (`finalize @(Int -> Int) ...`) still compiles.

### The API you build on (all `pub` in `tidepool-mcp`)

```rust
tidepool_mcp::RowArgs::at("Finalize", ["Decision"]).importing(["HarnessTypes"])
tidepool_mcp::ensure_effects_module_at(&decls, &row) -> io::Result<PathBuf>
tidepool_mcp::build_effect_stack_type_at(&decls, &row) -> String
tidepool_mcp::effects_module_source_at(&decls, &row) -> String
```

The no-argument forms still exist and render at the defaults
(`Finalize NoAnswer`). Read `tidepool-mcp/src/effect_decls.rs` (`RowArgs`,
`row_entry`) and the two unit tests in `tidepool-mcp/src/eval_prep.rs`
(`finalize_row_entry_is_applied_to_its_answer_type`,
`compound_answer_type_is_parenthesized_in_the_row`) — they show the exact
rendering.

**The generated module must import the module defining the answer type.** It
names `Decision` in `type M`, so `Decision` has to be in scope THERE, not only
in the turn module. That is what `.importing([...])` is for; feed it the
answer contract's imports.

## The work

### 1. Per-hole instantiation

Today `EngineConfig` materializes ONE effects module at construction
(`EngineConfig::from_decls` → `ensure_effects_module`) and renders the row from
`effect_names` (`EngineConfig::effect_stack_type`). The answerer node is reused
across holes whose types differ, so the row must vary per hole.

The answer type already reaches the turn: `SelfHarnessDriver` builds an
`AnswerContract` per hole (`driver.rs`, `answer_contract(ty)`) and sets it via
`Harness::set_answer_contract`; `Harness::run_block` reads it back and passes
`contract.ty` to `engine::template_turn` as the `finalize_ty` argument. Keep
that path — retarget what it DOES:

- `finalize_ty: Option<&str>` becomes the row instantiation rather than shim
  input. A turn with a contract compiles against an effects module + stack
  string built with `RowArgs::at("Finalize", [ty]).importing(contract imports)`;
  a turn without one compiles at the default (`Finalize NoAnswer`).
- Because the row lives in the generated `type M`, a pinned turn needs its OWN
  effects include dir. Resolve it per turn via `ensure_effects_module_at` and
  swap that entry in the include list. The dir is content-addressed on the
  generated source, so repeats of the same answer type are free and two
  answer types can never be served each other's module.
- The turn module's own `result :: Eff <stack> a` must name the SAME row —
  render it with `build_effect_stack_type_at`, not from `effect_names`.

Shape this however reads best (a small helper on `EngineConfig` that returns
`(include, stack)` for an optional contract is one obvious option). What
matters: ONE place computes the row, and both the include dir and the stack
string come from it — they cannot disagree.

The contract's imports are already carried: `AnswerContract` has the author
modules (`HarnessSource::answerer_imports`, which the driver puts on the
contract). Reuse them; do NOT redesign that half.

### 2. Delete the shim machinery

- `tidepool_harness::engine::finalize_shim` — delete the function.
- `tidepool_mcp::build_preamble_shadowing_effects` — delete it, and
  `EFFECTS_QUALIFIER` and the `shadowed` parameter threaded through
  `pragmas_and_imports` if nothing else uses them (check first: `grep -rn
  EFFECTS_QUALIFIER shadowed`). A pinned turn now uses the ORDINARY
  `build_preamble` — no `hiding`, no qualified alias, no injected binding.
- Drop the now-dead `merge_helpers` shim branch in `template_turn_for` if the
  helper is left with a single caller doing nothing.

### 3. The hole card sentence

The driver tells the answerer what it is answering (grep `types_in_scope_hint`
and the hole-card / prompt assembly in `driver.rs`). Update the sentence that
describes the finalize contract so it states the truth: the type is in scope
AND the row is what enforces it — a wrong-typed `finalize` is a compile error
naming the row. Do NOT touch `HarnessSource::answerer_imports` or
`types_in_scope_hint`'s import-scope logic itself; only the wording that
describes finalize.

### 4. Tests

`tidepool-harness/tests/finalize_type_pinning.rs` is the acceptance. Four of
its five tests must pass UNCHANGED. Exactly one must change, and root has
already approved the change:

- `wrong_typed_finalize_compiles_when_unpinned` — under the row, "unpinned" is
  not expressible: `Member (Finalize v)` is satisfiable by exactly the one type
  in the row. Rewrite it as the same CONTROL with the row instantiated AT
  `Text`: the same wrong-typed block compiles when the row names `Text`, so the
  rejection above comes from the row parameter selecting, not from unrelated
  breakage. Update the test's doc comment to state that intent shift explicitly
  (it currently describes the unpinned hole).
- Do not weaken or delete any other test in that file. If something else there
  seems to need changing, STOP and report instead.

Also update the module-level doc comment of that file: the PIN half is now the
row, not a shim.

`tidepool-harness/tests/acceptance_finalize.rs` has two `template_turn(...,
None)` turns that call `finalize`. They must name their answer type now
(`Some("Int -> Int")`). Keep everything else about those tests intact.

Add ONE new compile-level test to `finalize_type_pinning.rs`: an author module
edited between two compiles is picked up by the second (write a temp author
module defining a type, compile a `finalize` at it, rewrite the module with a
different constructor set, compile again, assert the second compile sees the
NEW definition). This pins root's cache-key concern — the effects staging dir
is content-addressed on generated SOURCE, and it must not be able to serve a
stale module against an edited author type.

## Boundaries

- Do NOT touch `HarnessSource::answerer_imports` or the import-scope half of
  the landed fix. Orthogonal, stays.
- Do NOT touch `compile.rs` timing/bench or lifecycle/persistence — sibling
  agents own those files.
- Do NOT change anything under `haskell/`. The extract needs no change; that is
  established, not an open question.
- Do NOT add a second mechanism alongside the row. No shims, no `hiding`, no
  qualified-alias dances, no "default to the old path when X".
- Never `git add -A`; stage the paths you changed. Never force-push.
