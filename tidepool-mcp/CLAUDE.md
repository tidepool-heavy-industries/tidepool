# tidepool-mcp — MCP server library (eval surface)

Serves the `eval`/`resume`/`abort` tools over an effect stack. The live API
reference for eval authors is the **`eval` tool description** (emitted by the
server, assembled from the `*_decl()` functions here). The eval stdlib lives in
`haskell/lib/Tidepool/`. See the repo-root `CLAUDE.md` for the project map.

Every effect has ONE definition: a `<eff>_effect_def!` macro in
`src/effect_defs.rs` carrying the GADT constructors (Haskell type strings AND
Rust bridge types), helper-verb text, and the handler/method wiring. Two
projections consume it: `effect_decl_projection!` (in `effect_decls.rs`)
generates the `*_decl()` builder, and `effect_rust_projection!`
(`tidepool-handlers/src/effect_glue.rs`) generates the `*Req` enum +
`DescribeEffect` + dispatch. Adding a constructor = one `verbs` row in the
definition + one hand-written inherent method on the handler struct (using
`cx.respond`/`respond_caught`/`respond_stream`). A wholly new effect type
needs a new definition + handler module + a positional union-tag slot.
`tidepool/src/main.rs` only wires the handler stack (`build_base_stack`); the
`tidepool-bridge` marshals `Value` ↔ `serde_json::Value`.

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
`TIDEPOOL_EXTRACT`. In-repo, `ServerBuilder::with_prelude`'s fallback points the
include path at `haskell/lib` directly.

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
path); `updateJ` rides the input lane; `insertAfter`/`writeChecked` also live here.
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
in-eval read+edit is safe; numbers captured in a PRIOR eval go stale (use
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
- `grepGlob :: Text -> FilePath -> M [Hit]` — structured text-level search with
  regex + filename globbing. Returns `[Hit]` {path, line, text} (the shared
  record; `matchLocs` over `hsDef`/`rsFn` `[Match]` yields the same `[Hit]` shape).

---

## MCP Server Internals

Notes for anyone working on the server process itself (`tidepool/src/main.rs`, `tidepool-mcp/src/lib.rs`).

**Eval thread signal handling**: A best-effort SIGILL/SIGSEGV handler is installed via `sigaltstack`+`sigaction`. `panic!` from a signal handler is UB and does not reliably unwind. The real safety net is returning `Ok(None)` → `CallToolResult::error` (not `McpError`) from the JIT boundary — this surfaces the failure to the MCP client without killing the server process or the connection. Do not try to make the signal handler do more than set a flag.

**Preamble imports**: Every eval sees:
```haskell
import Tidepool.Prelude hiding (error)
import Control.Monad.Freer hiding (run)
import qualified Prelude as P
```
Our `error :: Text -> a` shadows Prelude's `String` version. Our `run :: Text -> M Proc` shadows Freer's `run :: Eff '[] a -> a`. These hiding clauses are load-bearing — removing them breaks eval code that uses `error` with Text or `run` for shell commands.

**Eval timeout**: The default is 30 seconds (configurable via `eval_timeout_secs` in `config.toml` or `TIDEPOOL_EVAL_TIMEOUT_SECS`). Shell commands blocked on `.output()` (e.g. `cargo test --workspace`) consume the full timeout. The timeout returns a clean `CallToolResult::error`, not a crash.
