# Model-facing exemplar sweep — census

Date: 2026-08-11. Branch `root.exemplar-sweep`.

> **EXECUTED 2026-08-11.** Every DRIFTED entry below is fixed; every
> REPORT-ONLY entry is reported and deliberately untouched. Verify legs:
> `cargo check --workspace --all-targets` (green — this is also the
> `examples/{guess,tide}` exercise path, since their `haskell_eval!` macros run
> `tidepool-extract` at BUILD time), `cargo fmt --all -- --check` (clean),
> `cargo clippy --workspace --all-targets` (clean, zero warnings),
> `scripts/battery.sh -p tidepool-mcp -E 'all()'` (162 passed, 7 skipped), and
> the new `tidepool-harness::dogfood_harness_typecheck` (2 passed).
>
> **Two failures only the compile surfaced,** both pre-existing in
> `dev-tree/HarnessTypes.hs` and neither visible to `cargo check` (these are
> Haskell sources loaded at runtime, not cargo targets):
> - `Blocked Text` — a positional payload in a SUM has no generic-JSON key, and
>   `State` is checkpointed through `ToJSON`/`FromJSON`. The derive was rejected
>   outright: *"a payload constructor in a sum must use record syntax"*. Now
>   `Blocked { blockedReason :: Text }`.
> - `[fmt|{phase st}|]` and `[fmt|Last run: {summary}|]` — `Tidepool.Render` has
>   instances for `Text`/`String`/`Int`/`Double`/`Bool`/`Char` and nothing else,
>   so an author-defined type has no rendering the quoter could guess. Both now
>   get an explicit rendering, which is where that decision belongs anyway.
>
> These are the reason the sweep added
> `tidepool-harness/tests/dogfood_harness_typecheck.rs`: `examples/harness` has
> ~8 driver tests covering it, the authored dogfood harnesses had none, and
> `dev-tree` had rotted past the point where reading it was enough to tell.

**Why.** The API is the prompt. Every model-facing exemplar — an example crate,
an authored harness, a code snippet inside a tool description or a served
resource — is imitated verbatim. A snippet that no longer typechecks against
today's surface is a fluency tax charged to every model that reads it.
`tidepool-mcp/CLAUDE.md`'s review rule states the bar: *the examples in the
tool descriptions are the de facto style guide*, and *the descriptions attest
to the idealized surface — a gap a caller hits is a bug to fix, not a caution
to add.*

**Acceptance bar used for every verdict.** Today's surface is:

- Forms are `askUser @T` with `deriving (Generic, FromJSON)`; the submission is
  plain JSON. `DerivedForm a = (FormRoot a, FromJSON a)` — `FromJSON` is
  REQUIRED, not optional. The `Tidepool.Ui` eDSL and the `[form|]` quasiquoter
  are DELETED; `Tidepool.QQ` exports `fmt` / `j` / `patch` / `uri` only.
- There is no `ModelCodec`. The model boundary is plain vendored
  `ToJSON`/`FromJSON` plus `JsonSchema`.
- `()` on the wire is `null` (`FormRoot ()` → `UnitShape`, distinct from an
  empty record's `{}`).
- Effect results are named records read with record-dot (`p.stdout`, `h.path`).
- Effect verbs return `Either <Err>` (#335). `run cmd` is `Right p <- run cmd`;
  so are `glob`, `grepGlob`, `readFile`, `llm`, `httpGet`. `liftEither`
  unwraps-or-aborts.
- Typed subagents exist: `spawnAgent @r :: SpawnSpec -> M (Either SpawnError
  (SpawnOutcome, r))` in `Tidepool.Agent.Spawn`, spec built with `spawnSpec
  wspec label task`. The result type must be a single-constructor RECORD
  deriving `(Generic, FromJSON, JsonSchema)`. The async
  handle / `waitAgent` / `pokeAgent` surface from PRD 18 does NOT exist.

Verdicts: **CURRENT** (matches today's surface), **DRIFTED** (references a
surface that changed or never landed), **REPORT-ONLY** (out of this sweep's
edit boundary).

---

## A. `examples/guess` — the compiler demo

Exercised at BUILD time: it is a workspace member whose
`haskell_eval!`/`haskell_inline!` macros run `tidepool-extract` during macro
expansion, so `cargo check --workspace --all-targets` compiles its Haskell.

| Exemplar | Verdict | Note |
|---|---|---|
| `haskell/Effects.hs` | CURRENT | Hand-written `Console`/`Rng` GADTs. Teaches the freer-simple → Cranelift path, deliberately NOT the eval stdlib (its own comment says why). No eval-idiom claim to drift. |
| `haskell/Game.hs` | CURRENT | Same class. |
| `src/main.rs` | CURRENT | Rust-side handler + driver for the custom row. |

## B. `examples/tide` — the REPL/interpreter demo

Same build-time exercise path as `examples/guess`.

| Exemplar | Verdict | Note |
|---|---|---|
| `haskell/{Types,Effects,Eval}.hs` | CURRENT | Five custom effects (`Repl`/`Console`/`Env`/`Net`/`Fs`), a hand-rolled `showInt` avoiding Prelude. Compiler-surface exemplar, not eval-surface. |
| `src/{main,handlers,ast,parser,lib}.rs` | CURRENT | — |

## C. `examples/harness` — the frozen reference contract

Exercised by ~8 integration tests (`tidepool-harness/tests/selfharness_spine`,
`finalize_type_pinning`, `acceptance_askuser`, `acceptance_lazy_boot`,
`selfharness_framing`, `selfharness_lifecycle`, `dogfood_observability`,
`tidepool-web/tests/crash_recovery`), all of which put `examples/harness` on the
include path and load `Harness.hs` through the real driver.

| Exemplar | Verdict | Note |
|---|---|---|
| `Harness.hs` | CURRENT | `loop :: State -> Harness State`, `render :: State -> Text`, `runLLMTurn @Decision`. Matches the locked contract. |
| `HarnessTypes.hs` | CURRENT | `deriving (Generic, ToJSON, FromJSON, Show)`; nullary sums encode as bare constructor-name strings. |

## D. `harness-dogfooding` — the authored harnesses

| Exemplar | Verdict | Note |
|---|---|---|
| `README.md` | DRIFTED | The `dev-tree/` bullet says it "will compile as those surfaces land" and tags PRD 18 / 19. PRD 19 (worktrees/events) and the typed spawn both landed — but spawn landed with a DIFFERENT shape than the sketch assumed (synchronous one-cycle, no handle/poke). The bullet reads as "just waiting", which is no longer true. |
| `run.sh` | CURRENT | — |
| `wizard/Harness.hs` | CURRENT | `runLLMTurn @Contribution`, prompt models `finalize @Contribution (…)`, defers operator input to `askUser` forms. |
| `wizard/HarnessTypes.hs` | CURRENT | `render :: State -> Text` (locked signature), flat fields, `[fmt|…|]`. |
| `dev-tree/Harness.hs` | **DRIFTED (hard)** | See below. |
| `dev-tree/HarnessTypes.hs` | **DRIFTED** | See below. |

### `dev-tree/Harness.hs` — what it references vs. what exists

| Sketch spelling | Reality |
|---|---|
| `spawnAgent spec prompt` (2 args) → `AgentHandle DevMessage WorkerResult` | `spawnAgent @r spec` (1 arg + type app) → `M (Either SpawnError (SpawnOutcome, r))`, synchronous |
| `waitForResult h` | does not exist (spawn is run-to-completion) |
| `pokeAgent h msg`, `whenSafe`, `interrupting` | do not exist (PRD 18 async surface, deliberately not landed) |
| `agent { instructions, tools, model, workspace, retention }` record-update default | does not exist; the spec is `spawnSpec :: WorktreeSpec -> Text -> Text -> SpawnSpec` |
| `noTools`, `Capable`, `workspaceOf`, `Durable`, `Ephemeral` | do not exist; the no-tools path is `spawnAgent` itself (`NoTools` at `ToolRounds 0`) |
| `import Tidepool.Agent` for the spawn verbs | `Tidepool.Agent` exports only `type Agent`; the spawn surface is `Tidepool.Agent.Spawn` (not auto-imported — `Subagent` has no `extra_imports` arm) |
| `payload observed` | the field is `value`: `data Observed a = Observed { eventId :: EventId, value :: a }` |
| `withHandler (headChanged tree) handler body` | CORRECT — `withHandler :: Event a -> (a -> M ()) -> M b -> M b` |
| `createWorktree` / `fromCurrentRepository` / `fromWorktree` / `allowDirtySnapshot` / `worktreeBranch` / `renderWorktreeError` / `SourceDirty` | CORRECT — all live in `Tidepool.Worktree` |

### `dev-tree/HarnessTypes.hs`

- `data Phase = … | Blocked Text` — a positional payload in a sum has no
  generic-JSON key, so the `ToJSON`/`FromJSON` derive is rejected. Found by
  compiling, not by reading.
- `[fmt|{phase st}|]` / `[fmt|Last run: {summary}|]` — `Render` covers the
  scalars only; an author-defined type needs an explicit rendering. Same.
- `render :: State -> Maybe Text -> Text` — **violates the LOCKED signature**
  `render :: State -> Text`. The driver invokes `render` with one argument;
  compaction context is the runtime's job to compose, per
  `examples/harness/HarnessTypes.hs`'s own haddock.
- `WorkerResult` derives `(Generic, ToJSON, FromJSON, Show, Eq)` — a typed
  `spawnAgent @WorkerResult` additionally requires `JsonSchema` (that class is
  what derives the backend's `outputSchema`).
- `DevMessage` exists only to feed `pokeAgent`, which does not exist.

## E. `tidepool-mcp/src/preamble.rs` — eval tool description

| Snippet | Verdict | Note |
|---|---|---|
| `data Cfg = Cfg { … } deriving (Generic, FromJSON)` | CURRENT | The primary typed-decode example; pinned by `eval_description_models_the_idealized_idiom`. |
| `… ; grepGlob target "**/*.rs" <&> stake limit }` | **DRIFTED** | `grepGlob :: Text -> FilePath -> M (Either FsError [Hit])` since #335. `<&> stake limit` applies `stake` to the `Either`, not to the `[Hit]` — does not typecheck. |
| `Right p <- run "git status --short"` | CURRENT | — |
| `readFile … >>= \case { Right body …; Left (FsNotFound _) … }` | CURRENT | — |
| `Right v <- httpGet "https://api.github.com/repos/o/r"` | CURRENT | — |
| `llm (SObj [("k", SEnum ["a","b"])]) p <&> (^? key "k" . _String)` | **DRIFTED** | `llm :: Schema -> Text -> M (Either LlmError Value)` since #335. The optic is applied to the `Either`, which has no `AsValue` instance — does not typecheck. |
| `ask schema prompt` line | CURRENT | `ask :: Schema -> Text -> M Value` — no `Either`, correct as written. |
| `writeFile ".tidepool/lib/Mod.hs" (input ^. _String)` | CURRENT | Returns `M (Either FsError ())`; legal as a final eval expression. |
| `orchestrate_module_source` generated helper bodies | CURRENT | `runChecked`/`mapFiles`/`searchFiles` all `>>= liftEither`; `runChecked` uses `p.stdout`/`p.exitCode` record-dot. |

## F. `tidepool-mcp/src/resources.rs` — served resource texts

| Snippet | Verdict | Note |
|---|---|---|
| `guide_md`: `glob "**/*.rs" >>= mapM (\p -> (,) p <$> getFileSize p)` | **DRIFTED** | `glob :: FilePath -> M (Either FsError [FilePath])` since #335 — `mapM` is applied to the `Either`. |
| `guide_md`: `do { Right src <- readFile "CLAUDE.md"; pure (stake 5 (lines src)) }` | CURRENT | — |
| `guide_md`: the input-lane typed decode | **DRIFTED** | Same `grepGlob`/`<&> stake` text as E. |
| `guide_md`: effect-result records section | CURRENT | `Proc`/`Hit`/`FileRead`/`FileMeta` fields all match `effect_defs.rs`. |
| `guide_md`: "Effect failures are values" | CURRENT | — |
| `schema_md` | CURRENT | `ask`/`llm` signatures correct, extraction example binds `Right v <- llm …`. |
| `edits_md` | CURRENT | Pinned by `edits_md_documents_typed_update_outcomes`; signatures match the `fs_decl` helpers. |
| `capabilities_md` | CURRENT | No code snippets; the reach-path table is generated. |

## G. `tidepool-mcp/src/effect_defs.rs` — per-effect descriptions

These render into `tidepool://effect/{name}` verbatim and contribute their first
sentence to the eval tool description's effect index.

| Description | Verdict | Note |
|---|---|---|
| `askuser_effect_def!` | **DRIFTED** | Says "derive `Generic` for it and any nested custom types". `DerivedForm a = (FormRoot a, FromJSON a)` — `FromJSON` is required and is what reads the submitted JSON back. Following the description as written does not compile. It also carries no worked example, unlike `Tidepool.Form`'s own haddock. |
| `subagent_effect_def!` | **DRIFTED (by omission)** | Documents only the raw verbs (`spawnAgentRaw`/`agentBeginRaw`/`agentResumeRaw`) and says "prefer the typed `spawnAgentWithTools`" without ever showing the typed call, its module, or the `deriving (Generic, FromJSON, JsonSchema)` a result type needs. The examples are the style guide; the recommended form is the one that must be shown. |
| `finalize_effect_def!` | CURRENT | `finalize x` / type-indexed row. |
| `event`/`worktree` effect descriptions | CURRENT | `withHandler`, `Observed { eventId, value }`, receipts. |

## H. `tidepool-mcp/CLAUDE.md` — the eval-authoring pattern snippets

| Snippet | Verdict | Note |
|---|---|---|
| **Aperture** | **DRIFTED (broken)** | Binds `data <- expensiveScan`. `data` is a Haskell reserved word — the snippet cannot parse, and it is the FIRST snippet in the file that the review rule calls the style guide. |
| Census / editing / diff-on-input-lane / `Edit` DSL / `checkDiff` / structural search | CURRENT | Signatures match `effect_defs.rs`. |

## I. `tidepool-web/README.md`

CURRENT. The driving examples are `curl` verbs; the submission body is a flat
`{ key: scalar }` JSON object (`enum → tag string`, `int → number`), which is
exactly what the derived-form collector accepts. No Haskell snippets, no
deleted-surface references.

## J. `tidepool-repl/src/server.rs` — READ-ONLY (reported, not edited)

Excluded from edits: owned by the in-flight `batch-turns` sibling.

**Findings: no drift.** `build_tool_description` and the three `make_tool`
descriptions model today's surface correctly:

- `Right p <- run cmd` with `p.stdout` / `p.exitCode` / `p.stderr`, and an
  explicit "bare selectors like `stdout p` are ambiguous — always use dot
  syntax".
- `grepGlob`/`searchFiles → [Hit]` (`h.path`, `h.line`, `h.text`);
  `readGlob → [FileRead]` (`r.path`, `r.contents :: Either FsError Text`).
- No `[form|]`, no `Ui`, no `ModelCodec`, no untyped-`ask` claim.
- The effect index is DERIVED (`describe_effects_index`), so it cannot drift
  from the decls.

Two non-blocking notes for that owner:

1. `run cmd` is written `Either <EffectError> Proc` (a metavariable) where the
   concrete type is `ExecError`. The same metavariable appears in
   `tidepool-mcp`'s `guide_md`, so this is a consistent house spelling rather
   than repl-specific drift — worth a decision, not a fix in isolation.
2. The repl description never models the typed-`input` decode or `askUser @T`.
   That is CORRECT scoping (the repl has no `input` payload lane), noted only
   so it is not mistaken for an omission.

## K. REPORT-ONLY — dead `[form|]` quoter token (`tidepool-mcp/src/eval_prep.rs`)

Out of this sweep's edit boundary (that file is surface, not exemplar), but it
is a live reference to a deleted surface:

- `uses_qq` (`eval_prep.rs:326`) still lists `"[form|"` as a quasi-quoter
  open-token. `Tidepool.QQ` exports `fmt`/`j`/`patch`/`uri` only — there is no
  `form` quoter. An eval whose source merely contains that string pays the
  quoter-module import (~+385ms per the function's own comment) for a quoter
  that cannot resolve.
- `test_uses_qq_detection` asserts
  `uses_qq("askUserRaw (toJSON [form|choice ok?: yes no|])")` — a test that
  spells out the deleted idiom next to `askUserRaw`, which is exactly the form
  `askUser @T` replaced.
- Separately, `prop_uses_qq_detects_every_token` draws `tok in 0usize..4` over
  a 5-element `QQ_TOKENS` table, so the last token is never generated. The
  proptest under-covers by one index regardless of what happens to `[form|`.

Removing the token changes `uses_qq`'s behaviour, so it belongs to whoever owns
that gate, together with the two tests.

## L. Adjacent-scope stdlib haddock (served as `tidepool://stdlib/{module}`)

Stdlib module sources are served verbatim as MCP resources, so their haddock
examples are model-facing exemplars in the same sense as a tool description.

| Exemplar | Verdict | Note |
|---|---|---|
| `Tidepool/Form.hs` module haddock | CURRENT | `deriving (Generic, FromJSON)`, `request <- askUser @DeployRequest`. This is the reference spelling the `askuser_effect_def!` description drifted from. |
| `Tidepool/Agent/Spawn.hs` haddock (`spawnAgent`) | **DRIFTED** | `result <- spawnAgent (spawnSpec wspec "porter" "port the handler")` omits the `@WorkerResult` type application. `r` is determined only by the type application (the module's own prose writes `spawnAgent @WorkerResult spec`); as written the example is ambiguous. |
| `Tidepool/Agent/Spawn.hs` haddock (`spawnAgentWithTools`) | CURRENT | `spawnAgentWithTools @WorkerTools @WorkerResult (ToolRounds 4) …`. |
| `Tidepool/Event.hs` module haddock | **DRIFTED** | The worked example calls `spawnAgent parentSpec parentTask` (2 args), `waitAgent`, `pokeAgent child (whenSafe (Rebase …))` — the same never-landed PRD 18 handle surface `dev-tree` sketches. Its `value change` accessor IS correct. |

---

## Summary

| Bucket | Current | Drifted |
|---|---|---|
| `examples/guess`, `examples/tide` | 8 | 0 |
| `examples/harness` | 2 | 0 |
| `harness-dogfooding` | 3 | 3 (5 defects) |
| `preamble.rs` description | 7 | 2 |
| `resources.rs` resources | 6 | 2 |
| `effect_defs.rs` descriptions | 2 | 2 |
| `tidepool-mcp/CLAUDE.md` | 6 | 1 |
| `tidepool-web/README.md` | 1 | 0 |
| `tidepool-repl` descriptions (read-only) | 4 | 0 |
| stdlib haddock (adjacent) | 2 | 2 |

Every drifted entry above is a snippet that would not compile if pasted. The
common root cause is #335 (effect verbs gained typed `Either` failure) reaching
snippets written before it, plus two examples written against a PRD 18 handle
surface that was never built.
