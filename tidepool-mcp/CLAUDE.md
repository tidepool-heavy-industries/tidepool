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

Every effect has ONE definition. For a MIGRATED effect (Exec, Journal,
Worktree) that definition is a schema entry in `tidepool-protocol/src/effects/`
and every artifact below is GENERATED from it into `src/generated/` — see
`plans/self-iterating-harness/22-p1-protocol-scaffold.md` §9 for how to add the
next one. For the rest it is still a `<eff>_effect_def!` macro in
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

**New effect defs are row-polymorphic from birth.** Write every helper's real
Haskell signature as `Member <Eff> effs => ... -> Eff effs T` (`forall ...
effs.` when the helper itself is `@`-applied or otherwise needs the tyvar
named), never the closed `<verb> :: ... -> M T` shape — `M` is a
per-compile-module alias, so a helper defined against it cannot typecheck
under a narrower or differently-shaped row than the one the module happened
to generate for (e.g. a local `type M` shadow, PRD 21 C5's `delegate_wrap`).
Set `helpers_row_polymorphic true` on the definition — every effect in this
codebase does. This is no longer merely a vocab-widening opt-in (extract-wave
item 0b's original purpose): since stable-effects-core (below), it is what
lets the effect's GADT + `type_defs` + helpers live in the STABLE
`Tidepool.Effects.Core` module at all — `effects_core_module_source`
(`eval_prep.rs`) asserts it and PANICS naming the effect if unset, because
Core declares no `M` alias for a row-closed helper to typecheck against. The
model-visible prose (`description`/`prompt_card`) may keep the `M` shorthand
for readability — only the compiled helper text itself needs the real
signature. A helper BODY that truly cannot be made row-polymorphic (calls
another helper whose own type is still fixed to a concrete `M`) stays
concrete, with a comment at the definition site naming exactly which
dependency forces it — `Green`'s `asyncSpawn` is the one case where the
GADT's own CONSTRUCTOR field type is fixed to `Int -> M ()` by the wire
shape (not a body dependency); see `ROW_DEPENDENT_EFFECTS` (`eval_prep.rs`)
for how that one effect is handled (declared in the per-window shim instead
of Core — the whole effect, not just the one helper, since one bad
constructor forces the WHOLE GADT out).

## Stable-effects-core: the generated effects module is TWO modules

The generated Haskell effects surface an eval/session/turn imports is split
into a STABLE half and a PER-WINDOW half, so that effectful helper
DECLARATIONS (not just verb calls) can persist across turns and windows:

- **`Tidepool.Effects.Core`** (`effects_core_module_source`,
  `ensure_effects_core_module`) — every vocabulary effect's GADT, `type_defs`,
  and `Member`-polymorphic helpers. A PURE function of the effect VOCABULARY
  alone (which effects exist for this compile family — the general Agent
  stack, the self-iterating harness's answerer stack, …), never of the ROW or
  any `RowArgs` type application. Two compiles that share a vocabulary get
  BYTE-IDENTICAL Core text, hence the same content-addressed dir and no tycon
  churn between them — compiled once per vocabulary, not once per turn.
- **`Tidepool.Effects`** (`effects_shim_module_source`,
  `ensure_effects_shim_module`) — the tiny per-window SHIM: re-exports Core's
  whole surface (`module Tidepool.Effects.Core`) plus `type M = Eff
  <row_effects>`, the one thing that genuinely varies per compile (a pinned
  `Finalize <T>` hole's answer type, in particular). Declares no `data`/GADT of
  its own for an ordinary effect — `type M` is a synonym, invisible to the
  guard below — EXCEPT a [`ROW_DEPENDENT_EFFECTS`] effect (`Green`), whose
  whole GADT+helpers are spliced in here instead of Core, per-window, exactly
  as the pre-split single module always worked for it.

Model-visible spelling is UNCHANGED: `import Tidepool.Effects` and `M` resolve
exactly as before. Only WHERE each name is nominally declared moved — which is
exactly what makes it safe for a value or a DECLARATION mentioning those names
to survive a session bind or a shared decl plane
(`haskell/src/Tidepool/Translate.hs`'s `typeMentionsEffectMonad`, narrowed to
reject only the `Eff` tycon itself — the row still varies per compile, that
part is unchanged and locked — not "any tycon the generated module declares",
since Core's tycons no longer regenerate per turn). `EngineConfig` tracks
BOTH dirs (`core_dir`, stable across a config's whole life; `effects_dir`, the
shim, swapped per pinned turn by `turn_target`) and pushes both onto the
include path; `validation_include()` — the shared decl plane's validation
surface — drops the shim but KEEPS Core, which is the whole mechanism behind
"an effectful helper DECLARATION persists now": see
`tidepool-harness/tests/stable_effects_core_decl_plane.rs`.

**Former trap, now resolved by the above:** a bridged data record or an
`errors` ADT inlined directly in a `type_defs` literal used to be UNSAFE
(the per-session generated module was fragment-nominal, so an inline record
could not survive a session bind — `FileRead`/`fs_stable.rs`'s whole story).
`Tidepool.Records.Bridged`/`Tidepool.Records.Stable` (`stable_errors true`)
exist as the carve-out that predates this split and generalizes into it: ANY
`type_defs`/`errors` text now lands in the STABLE Core module regardless of
whether it's carved out or left inline, so the carve-out is no longer
load-bearing for fragment-nominal safety — it is harmless, historical
residue, not a trap. Do not treat "not carved into Records.Bridged/Stable" as
a defect in new code; a NEW effect can inline its records/errors in
`type_defs` same as any other effect's, and a session bind of the whole
`Either <Err> T` a verb returns validates and persists without destructuring
(see `tidepool-repl/tests/repro_t_multiline_sig.rs`'s
`either_returning_verb_bind_now_persists_across_turns`).

the `tidepool` binary only wires the handler stack (`build_base_stack`, called
from `tidepool/src/stack.rs`); the
`tidepool-bridge` marshals `Value` ↔ `serde_json::Value`.

If an effect's helpers need a companion Haskell import beyond the fixed eval
surface (`preamble::eval_import_lines` — Prelude, the qualified `T.`/`Map.`/…
namespaces, `Tidepool.Effects`), add ONE arm to `extra_imports_for!` in
`effect_defs.rs`, matched on the effect's own identifier (see the `Git`/
`AskUser`/`Subagent` arms). A MIGRATED effect declares them as schema data in
`tidepool-protocol` instead — `Exec`'s `Tidepool.Shell`/`Tidepool.Cargo` and
`Worktree`'s `Tidepool.Worktree` are emitted straight into the generated decl.
Either way, that ONE edit is picked up by both the stmt/eval plane (`preamble::pragmas_and_imports`) and the decl plane
(`preamble::session_decl_module_env`) automatically — both fold over
`EffectDecl::extra_imports` the same way, so there is no second gate to keep
in sync by hand. What it does NOT reach is the generated `Tidepool.Effects`
module itself — a helper spliced in there may not reference a name that lives
in the authored library layer, because that module cannot import it (see the
scaffold doc §11.12, and `prd19_emit.rs`, which pins both facts).

## On-disk paths & config (`tidepool_runtime::paths`)

The installed server is self-sufficient from any directory. All locations resolve
through one module, `tidepool-runtime/src/paths.rs`:

- **Cache (regenerable)** — `$XDG_CACHE_HOME/tidepool` → `~/.cache/tidepool`. Holds
  the materialized **bundled stdlib** (`stdlib/<content-hash>/`, complete tree,
  embedded at build time by `tidepool/build.rs`, re-materialized when the binary
  changes — no version-stamp staleness), the generated effects modules
  (`effects/`, self-healed per eval — TWO content-addressed dirs per compile
  family, `tidepool-effects-core-<hash>` for the stable `Tidepool.Effects.Core`
  and `tidepool-effects-<hash>` for the per-window `Tidepool.Effects` shim +
  `Tidepool.Orchestrate` — see the stable-effects-core section above), and the
  compiled-artifact memos.
- **User-global config** — `$TIDEPOOL_CONFIG_DIR` → `$XDG_CONFIG_HOME/tidepool` →
  `~/.config/tidepool`. Holds the global verb `lib/`, `secrets/`, and
  `config.toml`. Legacy `~/.tidepool/{lib,secrets}` is honored if present.
- **Project-local** — the nearest ancestor of CWD containing a `.tidepool/`
  (git-style walk-up): `lib/`, `secrets/`, `kv.json`, `config.toml`, `PATTERNS.md`.
- **CWD** — the Fs sandbox root, and Exec's initial working directory only;
  Exec itself is not filesystem-sandboxed (see `tidepool-handlers/CLAUDE.md`).

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
