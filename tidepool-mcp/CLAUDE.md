# tidepool-mcp — MCP server library (eval surface)

Serves the `eval`/`resume`/`abort` tools over an effect stack. The live API
reference for eval authors is the **`eval` tool description** (emitted by the
server, assembled from the `*_decl()` functions here). The eval stdlib lives in
`haskell/lib/Tidepool/`. See the repo-root `CLAUDE.md` for the project map.

Adding an effect = a `*_decl()` here (Haskell-facing constructors + helpers) + a
`*Req` handler arm in `tidepool-handlers/src/lib.rs` (using `cx.respond`/
`respond_caught`/`respond_stream`); `tidepool/src/main.rs` only wires the handler
stack (`build_base_stack`). The `tidepool-bridge` marshals `Value` ↔ `serde_json::Value`.
The cheap path is a new constructor on an existing effect (e.g. `ParseJson` on
`Http`); a wholly new effect type needs a positional union-tag slot.

## On-disk paths & config (`tidepool_runtime::paths`)

The installed server is self-sufficient from any directory. All locations resolve
through one module, `tidepool-runtime/src/paths.rs`:

- **Cache (regenerable)** — `$XDG_CACHE_HOME/tidepool` → `~/.cache/tidepool`. Holds
  the materialized **bundled stdlib** (`stdlib/<content-hash>/`, complete tree,
  embedded at build time by `tidepool/build.rs`, re-materialized when the binary
  changes — no version-stamp staleness), the generated `Tidepool.Effects` module
  (`effects/`, self-healed per eval), and the compiled-artifact memos.
- **User-global config** — `$TIDEPOOL_CONFIG_DIR` → `$XDG_CONFIG_HOME/tidepool` →
  `~/.config/tidepool`. Holds the global verb `lib/`, `secrets/`, and
  `config.toml`. Legacy `~/.tidepool/{lib,secrets}` is honored if present.
- **Project-local** — the nearest ancestor of CWD containing a `.tidepool/`
  (git-style walk-up): `lib/`, `secrets/`, `kv.json`, `config.toml`, `PATTERNS.md`.
- **CWD** — the Fs/Exec sandbox (unchanged).

**Verb library resolution** is layered: GHC include order
`[effects, stdlib, project-lib, global-lib]` (first match wins), so `Tidepool.*`
resolves from the bundle and a project `Library`/module shadows the global one.
The tool-description vocab digest merges both dirs (project overrides global).
**Config** (`config.toml`) layers default < global < project < env; keys:
`llm_model`, `eval_timeout_secs`. Env knobs: `TIDEPOOL_PRELUDE_DIR`,
`TIDEPOOL_CONFIG_DIR`, `TIDEPOOL_LLM_MODEL`, `TIDEPOOL_EVAL_TIMEOUT_SECS`,
`TIDEPOOL_EXTRACT`. In-repo, `ensure_prelude` short-circuits to `haskell/lib`.

## Eval-authoring patterns (know-how, not in the tool description)

**Aperture** (`ask schema prompt` as a decision gate): place the suspend after data
gathering, before expensive ops. The computation does the grunt work (scan, parse,
format a menu) then suspends; during the suspend→resume gap the caller scouts
independently (bash, grep, other evals) and resumes with an informed choice that
steers the rest. The suspended eval is a coroutine checkpoint; the gap is a
free-form intelligence window. `ask` is structured — the reply is validated
against the schema, extract it with optics.

```haskell
data   <- expensiveScan
go     <- ask (SObj [("proceed", SBool)]) (formatMenu data) <&> (^? key "proceed" . _Bool)
if go == Just True then expensiveAnalysis data else pure "skipped"
```

**Census**: one eval replaces N tool calls — `fsGlob` + `mapM fsMetadata` +
filtering gives a codebase overview in a single round-trip.

**Editing — `update` is the common-case core verb** (always available in any repo;
in `fs_decl` helpers, not project-lib): `update path old new :: M ()` is exact
str-replace, exactly-once, and THROWS a precise error on not-found/ambiguous —
the MCP Edit-tool shape. `updateAll` returns a count; `planUpdate :: M Value` is the
dry-run that returns `{changed,diff}` as DATA (never throws — the branch-before-commit
path); `updateJ` rides the input lane; `insertAfter`/`writeChecked` migrated here too.
The tiers below (`Edit` DSL, `[patch|]`/Diff, ast-grep) are power tools for
batch / diff-shaped / syntax-aware work; `tidepool://edits` documents all four,
common-case first.

**Diff-on-the-input-lane** (`[patch|]`/Diff verbs): multi-line `[patch|...|]`
literals in `code` are corrupted by template indentation — ride the `input`
payload lane instead: `applyDiff d where d = case input of { String s -> s; _ ->
"" }` with the unified diff as the JSON string. `applyDiff` is all-or-nothing
(plan-first; zero writes on any conflict) and reports conflicts/already-applied
as DATA.

**Never hand-write hunk arithmetic** — `genPatchTo path newContent` reads the
current file and generates the unified diff (Myers O(ND), 3-line context, counts
correct by construction; absent file → creation patch, identical → `""`). Put the
new body on the `input` lane and generate-then-apply in one eval. `genPatch path
old new :: Either Text FilePatch` is the pure core; `diffFiles a b` diffs two
existing files.

**Declarative small edits — the `Edit` verbs.** When a change is awkward as a diff
(replace lines 10–15, insert after an anchor), name it with an `Edit` and let the
engine lower it to a CONTEXT-anchored patch on the same atomic apply: `applyEdits
:: Text -> [Edit] -> M Value` (in-eval) / `editsJ :: Value -> M Value` (input
lane). `Edit` = `ReplaceLines lo hi [Text]` / `InsertAt n [Text]` / `ReplaceAnchor
a [Text]` / `InsertAfterAnchor a [Text]` / `InsertBeforeAnchor a [Text]` (1-based;
anchors are substring tests that must hit exactly one line). `planEdits`/
`planEditsJ` is a dry run returning the rendered review `diff`; `applyEdits` is
all-or-nothing; problems come back as DATA (`anchor-missing`/`anchor-ambiguous`/
`range-out-of-bounds`/`edits-overlap`). **Line-number safety:** numbers resolve
against the file read in the SAME eval and bake into a context-anchored patch — an
in-eval read+edit is safe; numbers captured in a PRIOR eval are the footgun (use
the anchor ops cross-eval — they're content-addressed and self-checking).

**checkDiff-first when a `[patch|]` pattern silently fails to match.** A no-match
is ambiguous (input doesn't parse vs. parses but shape differs). `checkDiff
diffText` (pure, returns `Value`) disambiguates: `{"parses":false,…}` = fix the
*diff text*; `{"parses":true,"files":[…]}` = fix the *pattern shape* against that
structure. Pattern holes: `$var` at a path; per-line `$x`/`-$x`/`+$x` (each binds
one line's `Text`); a bare `$var` in hunks position binds the file's whole
`[Hunk]`; trailing `...` allows extra files. `@@` line numbers are HINTS, not
matched. See the `qq_patch_pat_*` Suite fixtures for canonical shapes.

## Structural search

- `hsDef`/`hsSig`/`rsFn` recipes find function/signature definitions by name.
  (`hsDef` matches clauses with argument patterns — it misses point-free/nullary
  bindings and bare type sigs.)
- `rHas`/`rInside` are deep by default (`stopBy: end`); use `rHasChild`/
  `rInsideParent` for direct children.
- `grepGlob :: Text -> FilePath -> M [(FilePath, Int, Text)]` — structured
  text-level search with regex + filename globbing. (Returns tuples, whereas
  `hsDef`/`rsFn` return `[Match]` records — shapes differ.)
