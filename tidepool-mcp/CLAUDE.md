# tidepool-mcp — MCP server library (eval surface)

Serves the `eval`/`resume`/`abort` tools over an effect stack. The live API
reference for eval authors is the **`eval` tool description** (emitted by the
server, assembled from the `*_decl()` functions here). The eval stdlib lives in
`haskell/lib/Tidepool/`. See the repo-root `CLAUDE.md` for the project map.

> **Review rule — examples are the de facto style guide.** The code snippets in
> the `eval`/`session_run` tool descriptions (`preamble.rs`, `resources.rs`,
> `tidepool-repl/src/server.rs`) are what callers imitate verbatim, so a change
> in the idiom is a change to the examples: when the recommended spelling moves
> (typed `input` decode, `Right p <- run cmd`, record-dot, …), update every
> example that models the old form in the same pass. The descriptions attest to
> the idealized surface — a gap a caller hits is a bug to fix, not a caution to
> add.

Every effect has ONE definition: a `<eff>_effect_def!` macro in
`src/effect_defs.rs` carrying the GADT constructors (Haskell type strings AND
Rust bridge types), helper-verb text, and the handler/method wiring. Two
projections consume it: `effect_decl_projection!` (defined in `effect_defs.rs`,
invoked per-effect from `effect_decls.rs`) generates the `*_decl()` builder,
and `effect_rust_projection!` (`tidepool-handlers/src/effect_glue.rs`)
generates the `*Req` enum + `DescribeEffect` + dispatch. Adding a constructor =
one `verbs` row in the definition + one hand-written inherent method on the
handler struct (using `cx.respond`/`respond_list`, or an errors-tagged method
returning `Result<T, ErrEnum>` for typed failure). A wholly new effect type
needs a new definition + handler module + a positional union-tag slot.

the `tidepool` binary only wires the handler stack (`build_base_stack`, called
from `tidepool/src/stack.rs`); the
`tidepool-bridge` marshals `Value` ↔ `serde_json::Value`.

If an effect's helpers need a companion Haskell import beyond the fixed eval
surface (`preamble::eval_import_lines` — Prelude, the qualified `T.`/`Map.`/…
namespaces, `Tidepool.Effects`), add ONE arm to `extra_imports_for!` in
`effect_defs.rs`, matched on the effect's own identifier (see the `Exec`/
`Git`/`AskUser` arms — e.g. `Exec`'s helpers build on `runArgv`, so it pulls
in `Tidepool.Shell`/`Tidepool.Cargo`). That one edit is picked up by both the
stmt/eval plane (`preamble::pragmas_and_imports`) and the decl plane
(`preamble::session_decl_module_env`) automatically — both fold over
`EffectDecl::extra_imports` the same way, so there is no second gate to keep
in sync by hand.

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
scan <- expensiveScan
go   <- ask (SObj [("proceed", SBool)]) (formatMenu scan) <&> (^? key "proceed" . _Bool)
if go == Just True then expensiveAnalysis scan else pure "skipped"
```

**Census**: one eval replaces N tool calls — `fsGlob` + `mapM fsMetadata` +
filtering gives a codebase overview in a single round-trip.

**Editing — `update` is the common-case core verb** (always available in any
repo; in `fs_decl` helpers, not project-lib): exact str-replace, exactly-once,
the MCP Edit-tool shape. `tidepool://edits` is the live reference for all four
tiers (`update` family, the `Edit` DSL, `[patch|]`/Diff, ast-grep) — signatures
are not restated here. **The invariant behind them all: no editing verb ever
throws.** An empty `old`, a missing file, an absent pattern, an ambiguous
anchor, a conflicting hunk — every one comes back as a typed DATA outcome, so a
batch over many files cannot half-apply mid-loop, and `plan*` dry-runs
(`planUpdate`/`planEdits`) return their diff as data too. The tiers below are
power tools for batch / diff-shaped / syntax-aware work.

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

**Declarative small edits — the `Edit` verbs.** When a change is awkward as a
diff (replace lines 10–15, insert after an anchor), name it with an `Edit`
(`applyEdits`/`editsJ`, `planEdits`/`planEditsJ` to dry-run) and let the engine
lower it to a CONTEXT-anchored patch on the same atomic apply. Constructors,
JSON ops, and the conflict strings are in `tidepool://edits`. **What is NOT
there, and is the hazard: line numbers are only valid within one eval.** They
resolve against the file read in the SAME eval and bake into a context-anchored
patch, so an in-eval read+edit is safe — but numbers captured in a PRIOR eval
go stale silently. Use the anchor ops cross-eval; they are content-addressed
and self-checking.

**checkDiff-first when a `[patch|]` pattern silently fails to match.** A no-match
is ambiguous (input doesn't parse vs. parses but shape differs). `checkDiff
diffText` (pure, returns `Value`) disambiguates: `{"parses":false,…}` = fix the
*diff text*; `{"parses":true,"files":[…]}` = fix the *pattern shape* against that
structure. Pattern holes: `$var` at a path; per-line `$x`/`-$x`/`+$x` (each binds
one line's `Text`); a bare `$var` in hunks position binds the file's whole
`[Hunk]`; trailing `...` allows extra files. `@@` line numbers are HINTS, not
matched. See the `qq_patch_pat_*` Suite fixtures for canonical shapes.

## Structural search

`grepGlob` is the structured-search verb:

- `grepGlob :: Text -> FilePath -> M (Either FsError [Hit])` — regex-search
  files matching a path glob (arg order: regex first, glob second). Returns
  `[Hit]` {path, line, text} — the shared record shape other search verbs
  (`readGlob`, etc.) also use.

---

## MCP Server Internals

Notes for anyone working on the server process itself (`tidepool/src/main.rs`, `tidepool-mcp/src/lib.rs`).

**Eval thread signal handling**: A best-effort SIGILL/SIGSEGV handler is installed via `sigaltstack`+`sigaction`. `panic!` from a signal handler is UB and does not reliably unwind. The real safety net is returning `Ok(None)` → `CallToolResult::error` (not `McpError`) from the JIT boundary — this surfaces the failure to the MCP client without killing the server process or the connection. Do not try to make the signal handler do more than set a flag.

**Preamble imports**: the canonical list (`preamble::eval_import_lines`) every eval sees:
```haskell
import Tidepool.Prelude hiding (error)
import Tidepool.Effects
import qualified Tidepool.Data.Text as T
import qualified Data.Map.Strict as Map
import qualified Data.Map.Merge.Strict as MM
import qualified Data.Set as Set
import qualified Tidepool.Aeson as Aeson
import qualified Tidepool.Aeson.KeyMap as KM
import qualified Data.List as L
import qualified Tidepool.TextFormat as TF
import qualified Tidepool.Table as Tab
import qualified Tidepool.Patch as Patch
import Control.Monad.Freer hiding (run)
import qualified Prelude as P
```
(`import Library` is inserted before the final `Prelude as P` line when a
project library is present; each present effect's own `extra_imports` — e.g.
`Exec` → `Tidepool.Shell`/`Tidepool.Cargo`, `Git` → `Tidepool.Git`, `AskUser` →
`Tidepool.Form` — folds in on top of this base list, see `extra_imports_for!`
above.) The two `hiding` clauses are the load-bearing ones: our
`error :: Text -> a` shadows Prelude's `String` version, and our
`run :: Text -> M (Either ExecError Proc)` shadows Freer's
`run :: Eff '[] a -> a`. Removing either breaks eval code that uses `error`
with Text or `run` for shell commands.

**Eval timeout**: The default is 600 seconds, per-request raisable to 1800 (configurable via `eval_timeout_secs` in `config.toml` or `TIDEPOOL_EVAL_TIMEOUT_SECS`). Long shell commands (builds, test suites) run comfortably inside it; at the window an eval at an effect boundary parks as a continuation, a pure runaway is detached. The timeout returns a clean `CallToolResult::error`, not a crash.
