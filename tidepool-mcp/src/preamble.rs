//! Preamble, tool-description, and library-vocab assembly for the MCP server.
//!
//! [`build_preamble`] emits the eval expr module's header (pragmas + imports,
//! including `import Tidepool.Orchestrate`) plus the mode-selected
//! `paginateResult` alias. The pagination + orchestration helper BODIES live in
//! the generated [`orchestrate_module_source`] module (a pure function of the
//! effect set), imported rather than spliced into the expr module's scope — the
//! fix for the namespace-poison bug class where a session bind (`let glob = 5`)
//! captured a helper body's bare verb. [`build_eval_tool_description`] renders
//! the `eval` tool's human-facing description from the live effect set, and
//! [`library_vocab`] digests a user-library directory into a per-module
//! signature index.
//!
//! `build_preamble` is decomposed into named section builders
//! (`pragmas_and_imports`, `paginate_alias`) so the emitted-source seams are
//! legible.

use crate::EffectDecl;
use tidepool_runtime::session::ModuleEnv;

pub use tidepool_runtime::session::{declaration_pragmas as decl_pragmas, EVAL_PRAGMAS};

/// The `default` declaration emitted after all imports in the preamble.
/// Exported as the canonical injection-point marker: `insert_imports` in
/// `tidepool-repl` uses it to locate where session/user imports must go,
/// avoiding a magic-substring dependency on the literal text (AUDIT-3).
pub const PREAMBLE_DEFAULT_DECL: &str = "default (Int, Double, Text)\n";

/// The canonical ordered import lines shared by the eval `Expr` module and the
/// session decl modules — the SINGLE source of truth for the eval vocabulary
/// (`M`, the effect verbs from `Tidepool.Effects`, the `Tidepool.Prelude`
/// shadows, and the `T.`/`Map.`/`L.`/`Set.`/… qualified namespaces). Excludes
/// the `module Expr where` header and the `default` decl (those are eval-module
/// scaffolding, not imports). `user_library` inserts `import Library` — eval
/// only; the session has no user `Library` module, it accumulates its own
/// `Tidepool.Session.Lib.G<g>` chain instead.
///
/// `hide_note`: also hide Prelude's `note` (`Control.Error.Util`'s `a ->
/// Maybe b -> Either a b`) from the unqualified surface — set exactly when
/// `AskUser` is in the compiling row, since `Tidepool.Form.note :: Text -> M
/// ()` (the display-channel helper, auto-imported alongside `askUser`/
/// `choose` whenever `AskUser` is present — see `extra_imports_for!`)
/// otherwise collides with it (`Ambiguous occurrence`). `false` everywhere
/// else keeps `note` reachable on the general eval/Agent surface, which never
/// imports `Tidepool.Form` at all.
#[derive(Clone, Copy, Default)]
struct PreludeHides {
    note: bool,
    event_alt: bool,
}

impl PreludeHides {
    fn for_effects(effects: &[EffectDecl]) -> Self {
        Self {
            note: effects.iter().any(|e| e.type_name == "AskUser"),
            event_alt: effects.iter().any(|e| e.type_name == "RepoEvent"),
        }
    }

    fn prelude_import(self) -> &'static str {
        match (self.note, self.event_alt) {
            (false, false) => "import Tidepool.Prelude hiding (error)",
            (true, false) => "import Tidepool.Prelude hiding (error, note)",
            (false, true) => "import Tidepool.Prelude hiding (error, (<|>))",
            (true, true) => "import Tidepool.Prelude hiding (error, note, (<|>))",
        }
    }
}

fn eval_import_lines(user_library: bool, hides: PreludeHides) -> Vec<&'static str> {
    let mut v = vec![
        hides.prelude_import(),
        // Effect GADTs, `M`, the `error` shadow, and the send-wrapper helpers
        // live in the generated Tidepool.Effects module so library AND session
        // decl modules can import the SAME types and define effectful verbs.
        "import Tidepool.Effects",
        "import qualified Tidepool.Data.Text as T",
        "import qualified Data.Map.Strict as Map",
        // Merge/reconcile API (merge, zipWithMatched, …) — strict, matches Map.
        "import qualified Data.Map.Merge.Strict as MM",
        "import qualified Data.Set as Set",
        // `Aeson` qualifier: the `input` payload-lane injection emits
        // `input :: Aeson.Value` / `Aeson.String …` (json_to_haskell), so the
        // qualifier must be in scope for both eval AND session_eval.
        "import qualified Tidepool.Aeson as Aeson",
        "import qualified Tidepool.Aeson.KeyMap as KM",
        "import qualified Data.List as L",
        "import qualified Tidepool.TextFormat as TF",
        "import qualified Tidepool.Table as Tab",
        "import qualified Tidepool.Patch as Patch",
        // NOTE: Tidepool.Shell/Git/Cargo are NOT here — Shell/Cargo depend on
        // the Exec (`runArgv`) effect helper and Git on the Git verbs, so
        // they're imported conditionally in `pragmas_and_imports` only when
        // those effects are in the stack (else they fail to load on a
        // minimal/Console-only stack).
        "import Control.Monad.Freer hiding (run)",
    ];
    if user_library {
        v.push("import Library");
    }
    v.push("import qualified Prelude as P");
    v
}

/// The [`ModuleEnv`] for session Lane-A declaration modules under the full
/// effect stack — the eval pragmas + import surface (via [`EVAL_PRAGMAS`] /
/// [`eval_import_lines`]) so `session_def` helpers share the eval vocabulary
/// (e.g. a `Member Exec effs => Text -> Eff effs Text` helper using `run`,
/// `L.sortBy`, `Set.`, …). The
/// lens-free [`ModuleEnv::standalone_default`] remains for the plain-toolchain
/// standalone REPL/tests; this requires the `with-packages` GHC (it imports
/// `Tidepool.Prelude`, which pulls `Control.Lens`).
///
/// `effects`: the session's actual effect stack. Each effect's own
/// [`EffectDecl::extra_imports`] (Exec → `Tidepool.Shell`/`Tidepool.Cargo`,
/// Git → `Tidepool.Git`, …) is folded in, in list order — the SAME fold
/// [`pragmas_and_imports`] runs, so the decl and stmt/eval planes structurally
/// cannot diverge on this surface (previously two hand-mirrored `type_name ==
/// "..."` gates that had to be kept in sync by hand; friction #23 is the bug
/// that produced when they drifted). Passing the wrong (e.g.
/// empty/minimal) effect set here used to be masked by callers reaching for
/// the lens-free `ModuleEnv::standalone_default` instead — but that surface
/// also drops Prelude/Aeson, so a decl-plane pure bind (`v = object [...]`,
/// promoted to a decl for GHCi-parity generalization — see
/// `tidepool-runtime` `try_pure_bind_as_decl`) failed to resolve `object`/
/// `toJSON` under a minimal stack even though production always has them.
/// This fn is now safe to call for ANY stack: it always carries
/// Prelude/Aeson/qualified-namespaces (via `eval_import_lines`), and adds
/// each effect's companion imports only when that effect is actually present.
///
/// `user_library`: whether a project/global `Library` facade is on the
/// include path (mirrors the `stmt`-path flag in `has_user_library`, passed
/// by the caller since this module has no filesystem access to check
/// `base_include` itself). When `true`, decl items get `import Library` —
/// previously ALWAYS omitted here regardless of the flag (a decl referencing
/// a Library-re-exported type like `EditOutcome` needed its own explicit
/// import even though `:vocab` listed it as available). The caller
/// (`render_module`) is responsible for adding a `hiding (...)` clause
/// against a session's own decl heads, the same guard the `stmt` path already
/// applies via `hide_module_names` — importing `Library` unqualified here
/// without it would reintroduce the ambiguous-occurrence class BUG-7 fixed
/// for statements (a decl defining e.g. `data Hit`, which `Library`
/// re-exports, would collide).
#[must_use]
pub fn session_decl_module_env(effects: &[EffectDecl], user_library: bool) -> ModuleEnv {
    session_decl_module_env_with_companions(effects, user_library, CompanionImports::Include)
}

/// Build a declaration environment whose effect companion modules are either
/// implicit or left behind an authored facade.
#[must_use]
pub fn session_decl_module_env_with_companions(
    effects: &[EffectDecl],
    user_library: bool,
    companion_imports: CompanionImports,
) -> ModuleEnv {
    let mut imports: Vec<String> =
        eval_import_lines(user_library, PreludeHides::for_effects(effects))
            .into_iter()
            .map(String::from)
            .collect();
    if companion_imports == CompanionImports::Include {
        for decl in effects {
            imports.extend(decl.extra_imports.iter().map(|s| (*s).to_string()));
        }
    }
    // Orchestration helpers (readGlob/searchFiles/memo/renderJson/…): the
    // stmt plane gets these via the expr module's imports; without this the
    // decl plane's import surface diverges — a decl using `readGlob` failed
    // "not in scope" with no hint (friction #23, found live 2026-07-01).
    imports.push("import Tidepool.Orchestrate".into());
    ModuleEnv {
        pragmas: decl_pragmas(),
        imports,
    }
}

/// The [`ModuleEnv`] for persistent authored declarations.
///
/// This excludes the per-window `Tidepool.Effects` shim, whose `M` alias
/// varies with the active interpreter, and imports the stable
/// `Tidepool.Effects.Authored` facade instead. Persistent effectful helpers
/// must therefore state their real row-polymorphic contract with `Member`
/// constraints. Their source is compiled exactly as authored; the declaration
/// plane never rewrites or discards type signatures.
///
/// Effect companion imports (`Tidepool.Form` etc.) are still excluded, since
/// they depend on the shim's row being genuinely present. Everything else —
/// `Tidepool.Prelude` (unqualified `Text`, `object`, the pure vocabulary every
/// turn has ambient), the qualified namespaces, Aeson — stays, so a decl a
/// model authors in turn-module scope validates under the SAME names a real
/// turn has. The lens-free [`ModuleEnv::standalone_default`] is NOT a
/// substitute: it has no Prelude, so `data X = X Text` failed to validate
/// even though `Text` is ambient in every turn (companion dogfood,
/// 2026-08-14 — a fatal boot-class bug before the retry fix that landed with
/// this env).
#[must_use]
pub fn pure_decl_module_env() -> ModuleEnv {
    let mut imports: Vec<String> = eval_import_lines(false, PreludeHides::default())
        .into_iter()
        .filter(|l| *l != "import Tidepool.Effects")
        .map(String::from)
        .collect();
    imports.push("import Tidepool.Effects.Authored".to_string());
    ModuleEnv {
        pragmas: decl_pragmas(),
        imports,
    }
}

/// Emit the LANGUAGE pragma block, the `module Expr` header, and the fixed
/// import set (plus the conditional `import Library`).
///
/// QuasiQuotes + ViewPatterns are ALWAYS-ON by root decision: one eval
/// dialect everywhere beats conditional grammar (the Tidepool.QQ IMPORT
/// is still token-gated in eval() — scope, not syntax).
//
// KNOWN COST / FIXME(root): GHC's enableCodeGenForTH keys on extension
// PRESENCE (needsTemplateHaskellOrQQ checks xopt, not splice usage), so
// every eval bytecode-provisions its home-module graph — +3.0s per
// uncached eval (measured: 10.35s vs 7.30s full-preamble extract,
// 3-run avg), paid even by evals that never splice. REQUIRES an
// unpoison-fixed tidepool-extract-bin (this branch: GhcPipeline.hs
// unsets Opt_IgnoreInterfacePragmas on the downgraded summaries between
// depanal and load'); under a pre-fix binary every eval dies with the
// clz# deopt class (the spliton repro tests pin exactly this
// deoptimization bug). Fix later = token-gating
// (see 71d77fb, reverted) or upstream lazy provisioning.
// Dialect note: with QuasiQuotes on, `[x|x<-xs]` (comprehension with no
// space before `|`) parses as a quasi-quote — write `[x | x <- xs]`.
fn pragmas_and_imports(
    out: &mut String,
    effects: &[EffectDecl],
    user_library: bool,
    companion_imports: CompanionImports,
) {
    out.push_str(EVAL_PRAGMAS);
    out.push('\n');
    out.push_str("module Expr where\n");
    for imp in eval_import_lines(user_library, PreludeHides::for_effects(effects)) {
        out.push_str(imp);
        out.push('\n');
    }
    // Each effect's own companion imports (Exec → Tidepool.Shell/Cargo, Git →
    // Tidepool.Git, AskUser → Tidepool.Form, …) — emitted only when that
    // effect is present, else e.g. Shell/Cargo fail to load (they depend on
    // Exec's `runArgv`, absent from a generated Tidepool.Effects built from a
    // smaller stack). One fold over `EffectDecl::extra_imports`, in list
    // order, shared with `session_decl_module_env` — see that fn's doc
    // comment for why this used to be two hand-mirrored gates (friction #23).
    if companion_imports == CompanionImports::Include {
        for decl in effects {
            for imp in decl.extra_imports {
                out.push_str(imp);
                out.push('\n');
            }
        }
    }
    // The pagination / orchestration helper DEFINITIONS live in the generated
    // Tidepool.Orchestrate module (always written by `ensure_effects_module`,
    // co-located + hashed with Tidepool.Effects), NOT spliced into this expr
    // module's own scope. Splicing them here let a session's unqualified bind
    // (`let glob = 5`) capture a helper body's bare `glob`, type-erroring every
    // turn (no `hiding` clause can protect a same-module definition). As an
    // imported module, Orchestrate's internal `glob` resolves in ITS scope.
    out.push_str("import Tidepool.Orchestrate\n");
    out.push_str(PREAMBLE_DEFAULT_DECL);
    out.push('\n');
}

/// Generate the complete `Tidepool.Orchestrate` module source: the pagination /
/// auto-truncation helpers (`putStrLn`, `valSize`, the `trunc*` family,
/// `renderJson`, `paginateInteractive`/`paginateTrunc`) plus the KV/file/process
/// orchestration helpers (`memo`, `readGlob`, `searchFiles`, …).
///
/// A PURE function of `effects` only (no `interactive_pagination` /
/// `user_library` dependence) so it can be co-located with — and content-hashed
/// alongside — the generated `Tidepool.Effects` module in the same staging dir
/// (see [`crate::ensure_effects_module`]). Every helper that references an effect
/// verb is gated on that effect's presence so the module compiles on any stack.
///
/// These helpers used to be spliced as raw text into every eval's own expr
/// module; living in their own importable module is the fix for the
/// namespace-poison bug class — a session bind like `let glob = 5` can no longer
/// capture a helper body's bare `glob` (Orchestrate's internal `glob` resolves
/// in ITS own scope), and the expr module hides colliding names on the import.
pub fn orchestrate_module_source(effects: &[EffectDecl]) -> String {
    // Built once; every presence question below (`has_exec`, `has_ask`, …) is
    // a `.contains()` lookup instead of a fresh `effects.iter().any(...)`
    // rescan of the same slice — the genuinely conditional helper BODIES
    // (e.g. the `has_console && has_kv` `putStrLn` variant below) still stay
    // conditional, only the presence CHECK is shared.
    let names: std::collections::HashSet<&str> = effects.iter().map(|e| e.type_name).collect();
    let mut out = String::new();
    out.push_str(EVAL_PRAGMAS);
    out.push('\n');
    out.push_str("-- GENERATED by the tidepool MCP server (pure fn of the effect set).\n");
    out.push_str("-- Pagination + KV/file/process orchestration helpers, imported by\n");
    out.push_str("-- the eval expr module instead of spliced into its scope.\n");
    out.push_str("module Tidepool.Orchestrate where\n");
    out.push_str("import Tidepool.Prelude hiding (error)\n");
    out.push_str("import Tidepool.Effects\n");
    // `send` (used by `putStrLn`) comes from freer — Tidepool.Effects does not
    // re-export it (no export list ⇒ only its own decls), so import it here too.
    out.push_str("import Control.Monad.Freer hiding (run)\n");
    out.push_str("import qualified Tidepool.Data.Text as T\n");
    out.push_str("import qualified Data.Map.Strict as Map\n");
    out.push_str("import qualified Tidepool.Aeson.KeyMap as KM\n");
    out.push_str("import qualified Data.List as L\n");
    let has_exec = names.contains("Exec");
    let has_http = names.contains("Http");
    if has_exec && has_http {
        out.push_str("import qualified Tidepool.Shell as Shell\n");
        out.push_str("import Tidepool.Shell (sh)\n");
        out.push_str("import qualified Tidepool.Git as Git\n");
        out.push_str("import qualified Tidepool.Cargo as Cargo\n");
    }
    // `paginateInteractive` (gated on Ask below) calls `ask SStr …`, whose
    // composed verb and Schema constructors live in the authored stdlib
    // since the #24 move out of the Ask decl. Orchestrate — unlike the
    // generated Tidepool.Effects/Core modules — MAY import authored library
    // modules (Tidepool.Prelude above is one already), so it takes the
    // import directly rather than through `extra_imports` (which reaches
    // only the eval/decl planes).
    if names.contains("Ask") {
        out.push_str("import Tidepool.Form.Schema\n");
    }
    out.push('\n');

    // ToWire: result-rendering class for show-default mode (tidepool-repl).
    // Always emitted (no effect dep) so template_haskell_show_default's
    // `toWire _r` resolves. Text renders bare; Value passes through as JSON;
    // any Show type via show.
    //
    // INVESTIGATED (2026-07-01): tried adding `instance {-# OVERLAPPING #-}
    // ToJSON a => ToWire a where toWire = toJSON` to prefer ToJSON over Show
    // when both are available. Does NOT work — GHC rejects it as "duplicate
    // instance declarations", because both instances have the IDENTICAL head
    // `ToWire a` (fully polymorphic) and differ only in their CONSTRAINT
    // (`Show a` vs `ToJSON a`), not in type structure. GHC's
    // OVERLAPPING/OVERLAPPABLE resolution only orders instances whose heads
    // differ structurally (e.g. `C [a]` vs `C a`) — it cannot pick between
    // two equally-general heads based on which constraint happens to be
    // satisfiable. That would need constraint-based backtracking Haskell's
    // instance resolution doesn't do (the closest real mechanisms —
    // `IncoherentInstances`, `Data.Reflection`-style dictionary reflection —
    // don't give the desired "prefer X if satisfiable, else Y" semantics
    // safely/deterministically). Do not re-attempt this exact approach
    // without a fundamentally different mechanism (e.g. a closed type family
    // computing a type-level `Bool` for "has ToJSON", dispatched via a
    // separate class hierarchy) — treat as a design question, not a quick fix.
    // Container instances (added after the floor/Text/Value trio above): a
    // list/Maybe/tuple/Either result renders as structural JSON (Array/Null/
    // Object) with its LEAVES still Show-strings — only containers gain
    // structure, the Show floor is untouched. `Array`/`Object` here are the
    // vendored `Tidepool.Aeson.Value` constructors, whose payload is a plain
    // `[Value]`/`Map.Map Key Value` (NOT `Data.Vector.Vector` — this module
    // deliberately avoids Vector/HashMap primops, see
    // `Tidepool.Aeson.Value`'s module haddock), so no `Data.Vector` import is
    // needed: `Array [toWire a, toWire b]` / `Array . map toWire` build the
    // constructor directly from a list literal, mirroring the `ToJSON [a]`/
    // `ToJSON (a,b)` instances in that same module. The `[Char]`/`[a]` overlap
    // is the same one aeson's own `ToJSON` instances carry (`{-# OVERLAPPING
    // #-}` on `[Char]` is enough — GHC only needs ONE side of an overlapping
    // pair flagged, and the `Show a => ToWire a` floor above is already
    // OVERLAPPABLE, so `[a]`/`[Char]`/`Maybe a`/tuples/`Either` all resolve
    // over it with no extra pragma).
    out.push_str(concat!(
        "class ToWire a where toWire :: a -> Value\n",
        "instance {-# OVERLAPPABLE #-} Show a => ToWire a where toWire = String . T.pack . show\n",
        "instance ToWire Text where toWire = String\n",
        "instance ToWire Value where toWire = id\n",
        // Bare scalars: delegate to the vendored ToJSON so numbers/bools render
        // as native JSON scalars (not Show-strings) — `[Int]` -> `[10,20]`,
        // `Just True` -> `true`. toJSON avoids the vendored-Scientific
        // no-Fractional trap (it builds Number via `scientific`, not realToFrac).
        "instance ToWire Int where toWire = toJSON\n",
        "instance ToWire Integer where toWire = toJSON\n",
        "instance ToWire Double where toWire = toJSON\n",
        "instance ToWire Float where toWire = toJSON\n",
        "instance ToWire Bool where toWire = toJSON\n",
        "instance {-# OVERLAPPING #-} ToWire [Char] where toWire = String . T.pack\n",
        "instance ToWire a => ToWire [a] where toWire = Array . map toWire\n",
        "instance ToWire a => ToWire (Maybe a) where toWire = maybe Null toWire\n",
        "instance (ToWire a, ToWire b) => ToWire (a, b) where toWire (a, b) = Array [toWire a, toWire b]\n",
        "instance (ToWire a, ToWire b, ToWire c) => ToWire (a, b, c) where toWire (a, b, c) = Array [toWire a, toWire b, toWire c]\n",
        "instance (ToWire a, ToWire b) => ToWire (Either a b) where\n",
        "  toWire (Left a) = Object (Map.singleton \"Left\" (toWire a))\n",
        "  toWire (Right b) = Object (Map.singleton \"Right\" (toWire b))\n",
        "\n",
    ));

    let has_ask = names.contains("Ask");
    let has_console = names.contains("Console");
    let has_kv = names.contains("KV");
    let has_fs = names.contains("FsRead") && names.contains("FsWrite");

    out.push_str("-- Pagination\n");
    out.push_str(concat!(
        "showI :: Int -> Text\n",
        "showI n = T.pack (show n)\n",
    ));
    // putStrLn: Print effect + char counter in KV (when available)
    if has_console && has_kv {
        out.push_str(concat!(
            "putStrLn :: Text -> M ()\n",
            "putStrLn t = do\n",
            "  send (Print t)\n",
            "  v <- kvGet \"__sayChars\"\n",
            "  let cur = case v of { Just b -> case b ^? _Int of { Just n -> n; _ -> 0 }; Nothing -> 0 }\n",
            "  kvSet \"__sayChars\" (toJSON (cur + T.length t))\n",
        ));
    } else if has_console {
        out.push_str(concat!(
            "putStrLn :: Text -> M ()\n",
            "putStrLn = send . Print\n",
        ));
    }

    // valSize..renderJson are PURE (reference no effect verbs, only Value/KM/T)
    // — always emitted; the paginate variants below depend on them.
    out.push_str(concat!(
        "valSize :: Value -> Int\n",
        "valSize v = case v of\n",
        "  String t -> T.length t + 2\n",
        "  Number _ -> 8\n",
        "  Bool b -> if b then 4 else 5\n",
        "  Null -> 4\n",
        "  Array xs -> arrSz xs 2\n",
        "  Object m -> objSz (KM.toList m) 2\n",
    ));
    out.push_str(concat!(
        "arrSz :: [Value] -> Int -> Int\n",
        "arrSz [] acc = acc\n",
        "arrSz [x] acc = acc + valSize x\n",
        "arrSz (x:xs) acc = arrSz xs (acc + valSize x + 2)\n",
    ));
    out.push_str(concat!(
        "objSz :: [(Key, Value)] -> Int -> Int\n",
        "objSz [] acc = acc\n",
        "objSz [(k,v)] acc = acc + T.length (KM.toText k) + 4 + valSize v\n",
        "objSz ((k,v):rest) acc = objSz rest (acc + T.length (KM.toText k) + 4 + valSize v + 2)\n",
    ));
    out.push_str(concat!(
        "truncArr :: Int -> Int -> [Value] -> ([Value], Int, [(Int, Value)])\n",
        "truncArr _ nid [] = ([], nid, [])\n",
        "truncArr bud nid (x:xs)\n",
        "  | bud <= 30 = ([marker], nid + 1, [(nid, Array (x:xs))])\n",
        "  | sz <= bud = let (r, nid', s) = truncArr (bud - sz - 2) nid xs in (x : r, nid', s)\n",
        "  | otherwise = let m = String (\"[~\" <> showI sz <> \" chars -> stub_\" <> showI nid <> \"]\")\n",
        "                    (r, nid', s) = truncArr (bud - 50) (nid + 1) xs\n",
        "                in (m : r, nid', (nid, x) : s)\n",
        "  where sz = valSize x\n",
        "        n = 1 + length xs\n",
        "        tsz = sz + arrSz xs 0\n",
        "        marker = String (\"[\" <> showI n <> \" more, ~\" <> showI tsz <> \" chars -> stub_\" <> showI nid <> \"]\")\n",
    ));
    out.push_str(concat!(
        "truncKvs :: Int -> Int -> [(Key, Value)] -> ([(Key, Value)], Int, [(Int, Value)])\n",
        "truncKvs _ nid [] = ([], nid, [])\n",
        "truncKvs bud nid ((k,v):rest)\n",
        "  | bud <= 30 = ([(KM.fromText \"...\", String marker)], nid + 1, [(nid, object (map (\\(k',v') -> KM.toText k' .= v') ((k,v):rest)))])\n",
        "  | sz <= bud = let (r, nid', s) = truncKvs (bud - sz - 2) nid rest in ((k,v) : r, nid', s)\n",
        "  | otherwise = let m = String (\"[~\" <> showI (valSize v) <> \" chars -> stub_\" <> showI nid <> \"]\")\n",
        "                    (r, nid', s) = truncKvs (bud - 50) (nid + 1) rest\n",
        "                in ((k, m) : r, nid', (nid, v) : s)\n",
        "  where sz = T.length (KM.toText k) + 4 + valSize v\n",
        "        n = 1 + length rest\n",
        "        tsz = sz + objSz rest 0\n",
        "        marker = \"[\" <> showI n <> \" more fields, ~\" <> showI tsz <> \" chars -> stub_\" <> showI nid <> \"]\"\n",
    ));
    out.push_str(concat!(
        "truncGo :: Int -> Int -> Value -> (Value, Int, [(Int, Value)])\n",
        "truncGo bud nid v\n",
        "  | valSize v <= bud = (v, nid, [])\n",
        "  | otherwise = case v of\n",
        "      Array xs -> let (items, nid', stubs) = truncArr bud nid xs in (Array items, nid', stubs)\n",
        "      Object m -> let (pairs, nid', stubs) = truncKvs bud nid (KM.toList m)\n",
        "                  in (object (map (\\(k',v') -> KM.toText k' .= v') pairs), nid', stubs)\n",
        "      String t -> let keep = max 10 (bud - 30)\n",
        "                  in (String (T.take keep t <> \"...[\" <> showI (T.length t) <> \" chars]\"), nid, [])\n",
        "      _ -> (v, nid, [])\n",
    ));
    out.push_str(concat!(
        "truncVal :: Int -> Value -> (Value, [(Int, Value)])\n",
        "truncVal budget val = let (v, _, stubs) = truncGo budget 0 val in (v, stubs)\n",
    ));
    out.push_str(concat!(
        "lookupStub :: Int -> [(Int, Value)] -> Maybe Value\n",
        "lookupStub _ [] = Nothing\n",
        "lookupStub sid ((k,v):rest) = if sid == k then Just v else lookupStub sid rest\n",
    ));

    out.push_str(concat!(
        "renderJson :: Value -> Text\n",
        "renderJson v = case v of\n",
        "  Object m -> \"{\" <> T.intercalate \",\" (map (\\(k,v') -> \"\\\"\" <> KM.toText k <> \"\\\":\" <> renderJson v') (KM.toList m)) <> \"}\"\n",
        "  Array xs -> \"[\" <> T.intercalate \",\" (map renderJson xs) <> \"]\"\n",
        "  String t -> \"\\\"\" <> T.concatMap (\\c -> case c of { '\\\\' -> \"\\\\\\\\\"; '\"' -> \"\\\\\\\"\"; '\\n' -> \"\\\\n\"; '\\t' -> \"\\\\t\"; '\\r' -> \"\\\\r\"; _ -> T.singleton c }) t <> \"\\\"\"\n",
        "  Number n -> T.pack (show n)\n",
        "  Bool b -> if b then \"true\" else \"false\"\n",
        "  Null -> \"null\"\n",
    ));

    // paginate* reference `M`, which only exists on a non-empty stack
    // (`type M = Eff '[…]` is emitted only then). Gate accordingly.
    if !effects.is_empty() {
        // Interactive: suspend to the caller via `ask` to fetch stub chunks.
        if has_ask {
            out.push_str(concat!(
                "paginateInteractive :: Int -> Value -> M Value\n",
                "paginateInteractive budget val\n",
                "  | valSize val <= budget = pure val\n",
                "  | otherwise = do\n",
                "      let (truncated, stubs) = truncVal budget val\n",
                "      case stubs of\n",
                "        [] -> pure truncated\n",
                "        _ -> do\n",
                "          let stubInfo = Array (map (\\(sid, sv) -> object [\"id\" .= (\"stub_\" <> showI sid), \"size\" .= toJSON (valSize sv)]) stubs)\n",
                "          resp <- ask SStr (\"[Pagination] truncated: \" <> renderJson truncated <> \" stubs: \" <> renderJson stubInfo <> \" | Reply with a stub id (e.g. stub_0) to fetch that chunk; any other reply ends pagination and returns the current chunk.\")\n",
                "          case resp ^? _String of\n",
                "            Just s -> case parseIntM (T.drop 5 s) of\n",
                "              Just sid -> case lookupStub sid stubs of\n",
                "                Just subtree -> paginateInteractive budget subtree\n",
                "                Nothing -> pure truncated\n",
                "              Nothing -> pure truncated\n",
                "            _ -> pure truncated\n",
            ));
        }
        // Truncate-only: always emitted so every non-empty stack has a
        // paginateTrunc. Console variant emits a note (needs putStrLn); the
        // pure variant truncates silently when there is no Console.
        if has_console {
            out.push_str(concat!(
                "paginateTrunc :: Int -> Value -> M Value\n",
                "paginateTrunc budget val\n",
                "  | valSize val <= budget = pure val\n",
                "  | otherwise = do\n",
                "      let (truncated, _) = truncVal budget val\n",
                "      putStrLn \"[truncated \u{2014} bind the result and re-query]\"\n",
                "      pure truncated\n",
            ));
        } else {
            out.push_str(concat!(
                "paginateTrunc :: Int -> Value -> M Value\n",
                "paginateTrunc budget val\n",
                "  | valSize val <= budget = pure val\n",
                "  | otherwise = let (truncated, _) = truncVal budget val in pure truncated\n",
            ));
        }
    }

    // KV + file orchestration helpers — each gated on its effect so the module
    // compiles on any stack. (Previously gated behind `user_library`; now a pure
    // fn of the effect set, so they're available in every eval.)
    if has_kv {
        out.push_str("-- KV orchestration helpers\n");
        out.push_str(concat!(
            "memo :: Text -> M Value -> M Value\n",
            "memo k compute = do\n",
            "  cached <- kvGet k\n",
            "  case cached of\n",
            "    Just v  -> pure v\n",
            "    Nothing -> do { v <- compute; kvSet k v; pure v }\n",
        ));
        // kvModify/kvIncr/kvAppend are lost-update-free: each is an optimistic
        // compare-and-swap RETRY loop over `kvCas`, not a racy kvGet-then-kvSet.
        // Two agent processes incrementing the same key both converge instead of
        // one silently clobbering the other (the FsWriteCas #330 guarantee, for
        // KV). On a conflict `kvCas` refreshes the store, so the retry's kvGet
        // reads the fresh value.
        out.push_str(concat!(
            "kvModify :: Text -> (Maybe Value -> Value) -> M Value\n",
            "kvModify k f = do\n",
            "  old <- kvGet k\n",
            "  let new = f old\n",
            "  r <- kvCas k old new\n",
            "  case r of { Right () -> pure new; Left _ -> kvModify k f }\n",
        ));
        out.push_str(concat!(
            "kvIncr :: Text -> M Int\n",
            "kvIncr k = do\n",
            "  old <- kvGet k\n",
            "  let n = case old >>= (^? _Int) of { Just i -> i; _ -> 0 }\n",
            "  let n' = n + 1\n",
            "  r <- kvCas k old (toJSON n')\n",
            "  case r of { Right () -> pure n'; Left _ -> kvIncr k }\n",
        ));
        out.push_str(concat!(
            "kvAppend :: Text -> Value -> M [Value]\n",
            "kvAppend k v = do\n",
            "  old <- kvGet k\n",
            "  let xs = case old >>= (^? _Array) of { Just arr -> arr; _ -> [] }\n",
            "  let xs' = xs ++ [v]\n",
            "  r <- kvCas k old (toJSON xs')\n",
            "  case r of { Right () -> pure xs'; Left _ -> kvAppend k v }\n",
        ));
        out.push_str(concat!(
            "kvAll :: M [(Text, Value)]\n",
            "kvAll = do\n",
            "  ks <- kvKeysP \"\"\n",
            "  vs <- mapM kvGet ks\n",
            "  pure (zipWith (\\k mv -> (k, maybe Null id mv)) ks vs)\n",
        ));
    }
    if has_fs {
        // The primitive filesystem verbs return typed failure (#335); these orchestration
        // helpers abort-on-failure via `liftEither` (in scope from Tidepool.Effects),
        // preserving their pre-#335 throw-on-read-error behaviour. The per-file
        // isolating read lives on `FsRead`; mutation requires `FsWrite` too.
        out.push_str("-- File orchestration helpers\n");
        out.push_str(concat!(
            "mapFiles :: [Text] -> (Text -> Text -> M Text) -> M [Text]\n",
            "mapFiles paths transform = mapM (\\p -> do\n",
            "  content <- readFile p >>= liftEither\n",
            "  result <- transform p content\n",
            "  writeFile p result >>= liftEither\n",
            "  pure p) paths\n",
        ));
        out.push_str(concat!(
            "mapFile :: Text -> (Text -> Text) -> M ()\n",
            "mapFile path f = do { c <- readFile path >>= liftEither; writeFile path (f c) >>= liftEither }\n",
        ));
        out.push_str(concat!(
            "mapFileM :: Text -> (Text -> M Text) -> M ()\n",
            "mapFileM path f = readFile path >>= liftEither >>= f >>= writeFile path >>= liftEither\n",
        ));
        out.push_str(concat!(
            "searchFiles :: Text -> Text -> M [Hit]\n",
            "searchFiles pat needle = do\n",
            "  files <- glob pat >>= liftEither\n",
            "  fmap concat $ forM files $ \\p -> do\n",
            "    content <- readFile p >>= liftEither\n",
            "    let ls = zip [(1::Int)..] (T.lines content)\n",
            "    pure [Hit p n l | (n, l) <- ls, T.isInfixOf needle l]\n",
        ));
        out.push_str(concat!(
            "lineCount :: Text -> M Int\n",
            "lineCount path = length . T.lines <$> (readFile path >>= liftEither)\n",
        ));
        out.push_str(concat!(
            "fileContains :: Text -> Text -> M Bool\n",
            "fileContains path needle = T.isInfixOf needle <$> (readFile path >>= liftEither)\n",
        ));
    }
    if has_exec {
        // `run` is typed (#335); abort-on-failure via `liftEither`, preserving
        // `runChecked`/`runAll`'s pre-#335 throw-on-failure behaviour.
        out.push_str(concat!(
            "runChecked :: Text -> M Text\n",
            "runChecked cmd = do\n",
            "  p <- run cmd >>= liftEither\n",
            "  if ok p then pure p.stdout else error (\"command failed (\" <> T.pack (show p.exitCode) <> \"): \" <> p.stderr)\n",
        ));
        out.push_str(concat!(
            "runAll :: [Text] -> M [Proc]\n",
            "runAll = mapM (\\c -> run c >>= liftEither)\n",
        ));
    }

    out
}

/// Whether effect-specific helper modules are part of the implicit authored
/// vocabulary. Curated facades can omit them while retaining the same effect
/// declarations and handlers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompanionImports {
    Include,
    Omit,
}

/// Result-pagination mode selecting which `paginateResult` alias body a
/// preamble emits — see [`build_preamble_non_interactive_mode`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaginateMode {
    /// Ask-suspend on an oversized result (`paginateInteractive`) when the
    /// stack carries `Ask`; falls back to the `Truncate` target otherwise.
    /// Used by [`build_preamble`] (interactive eval).
    Interactive,
    /// Haskell-side truncation (`paginateTrunc`) — the non-interactive
    /// default. Used by [`build_preamble_non_interactive`].
    Truncate,
    /// No Haskell-side truncation (`paginateResult _ v = pure v`): the full
    /// value reaches Rust untouched. Used by `tidepool-repl` via
    /// [`build_preamble_non_interactive_mode`] — its Rust-side truncation
    /// (`truncate::truncate_result`) can stash the elided subtrees for
    /// `:stub <n>`, which Haskell-side truncation discards.
    Passthrough,
}

/// Emit the mode-selected `paginateResult` alias into the eval expr module.
/// `Tidepool.Orchestrate` exports `paginateInteractive` and/or `paginateTrunc`;
/// the result wrapper in `eval_prep.rs` calls `paginateResult`, so the expr
/// module aliases one of them by mode (or, for [`PaginateMode::Passthrough`],
/// binds a local pass-through instead of aliasing an `Orchestrate` export).
/// Emitted only when the stack has effects — `M`/`paginateResult` are
/// otherwise unused.
fn paginate_alias(out: &mut String, effects: &[EffectDecl], mode: PaginateMode) {
    if effects.is_empty() {
        return;
    }
    out.push_str("paginateResult :: Int -> Value -> M Value\n");
    match mode {
        PaginateMode::Passthrough => {
            out.push_str("paginateResult _ v = pure v\n");
        }
        PaginateMode::Interactive | PaginateMode::Truncate => {
            let has_ask = effects.iter().any(|e| e.type_name == "Ask");
            let target = if mode == PaginateMode::Interactive && has_ask {
                "paginateInteractive"
            } else {
                "paginateTrunc"
            };
            out.push_str(&format!("paginateResult = {target}\n"));
        }
    }
    out.push('\n');
}

/// Assemble the full Haskell preamble for an eval: module header + imports
/// (incl. `import Tidepool.Orchestrate`) and the mode-selected `paginateResult`
/// alias. The helper bodies live in the imported `Tidepool.Orchestrate` module,
/// not spliced here (the namespace-poison fix).
pub fn build_preamble(effects: &[EffectDecl], user_library: bool) -> String {
    build_preamble_with_companions(effects, user_library, CompanionImports::Include)
}

/// Build an eval preamble while controlling whether effect helper modules are
/// imported implicitly. Installed effects and interpreter authority are
/// unchanged.
#[must_use]
pub fn build_preamble_with_companions(
    effects: &[EffectDecl],
    user_library: bool,
    companion_imports: CompanionImports,
) -> String {
    let mut out = String::new();
    pragmas_and_imports(&mut out, effects, user_library, companion_imports);
    paginate_alias(&mut out, effects, PaginateMode::Interactive);
    out
}

/// Like [`build_preamble`] but with non-interactive (truncate-only) pagination:
/// oversized results are truncated in-place with a marker instead of suspending
/// via `ask`. Used where bindings persist across turns and the caller can
/// re-query a truncated result by its bound name. Always [`PaginateMode::Truncate`]
/// — see [`build_preamble_non_interactive_mode`] for other modes.
pub fn build_preamble_non_interactive(effects: &[EffectDecl], user_library: bool) -> String {
    build_preamble_non_interactive_mode(effects, user_library, PaginateMode::Truncate)
}

/// Like [`build_preamble_non_interactive`], with an explicit [`PaginateMode`].
/// `tidepool-repl` calls this directly with [`PaginateMode::Passthrough`] so the
/// full result reaches Rust, instead of string-patching the `Truncate`-mode
/// output after the fact.
pub fn build_preamble_non_interactive_mode(
    effects: &[EffectDecl],
    user_library: bool,
    mode: PaginateMode,
) -> String {
    let mut out = String::new();
    pragmas_and_imports(&mut out, effects, user_library, CompanionImports::Include);
    paginate_alias(&mut out, effects, mode);
    out
}

/// Qualified aeson imports for MCP eval. Unqualified symbols now come from Tidepool.Prelude.
/// These provide `Aeson.` prefix (used by json_to_haskell for input injection) and
/// qualified access to KeyMap/Vector for power users.
pub fn aeson_imports() -> String {
    concat!(
        "qualified Tidepool.Aeson as Aeson\n",
        "qualified Tidepool.Aeson.KeyMap as KM\n",
    )
    .into()
}

pub(crate) fn build_eval_tool_description(effects: &[EffectDecl]) -> String {
    let mut desc = String::from(concat!(
        "`code` is a single Haskell EXPRESSION of type `M a`; its value is the ",
        "eval's result. The server wraps it in a module with the effect stack, ",
        "pragmas, and imports. Compose with `>>=`, `<&>`, `>=>`, and point-free ",
        "pipelines; attach a trailing `where` for local bindings. For step-by-step ",
        "sequencing write an explicit `do` block. Invoke effects with the helper ",
        "verbs. First call compiles from scratch (several seconds, uncached); ",
        "subsequent calls reuse the compiled artifact and are fast.\n",
        "Batch CORRELATED reads (the files + status + log that answer one ",
        "question) into a single eval \u{2014} one correlated program has less ",
        "inter-call drift than separate round-trips, NOT a filesystem snapshot: ",
        "a concurrent writer can still interleave mid-eval. Reads split across ",
        "separate evals drift further apart, and a join over them can silently ",
        "drop keys.\n",
        "The unqualified `Tidepool.Prelude` is the recommended surface: a Text-first, ",
        "effect-aware standard library that resolves cleanly on the JIT. `FilePath` is ",
        "`Text` here \u{2014} path/extension work is `T.` functions (`T.dropWhile`, ",
        "`T.stripPrefix`), never `String` idioms like `dropWhile (== '.')`. Qualified ",
        "namespaces reach the wider ecosystem \u{2014} among them T. (Data.Text), ",
        "L. (Data.List), Map. (Data.Map.Strict), MM. (Data.Map.Merge.Strict), ",
        "Set. (Data.Set), KM. (Tidepool.Aeson.KeyMap), TF. (Tidepool.TextFormat), ",
        "Tab. (Tidepool.Table), and P. (the full base Prelude). tidepool://capabilities ",
        "is the live index of the Prelude shadow surface and the names that live ",
        "under a qualifier.\n",
        "The final value of `code` renders to JSON for the caller \u{2014} Int → ",
        "number, [Char] → string, Bool → true/false, lists → arrays, and a ",
        "`Value` → that JSON directly. In the REPL (`session_run`), results render ",
        "via `Show` by default — `Text` is bare, custom ADTs work without `ToJSON`. ",
        "For structured output return a `Value` (via ",
        "`object`/`toJSON`/`eitherDecode`/`llm`/`httpGet`, …), e.g. ",
        "`Right v <- httpGet \"https://api.github.com/repos/o/r\"`; reserve `putStrLn`/`say` for ",
        "human-readable debug traces, and return `pure x` in place of ",
        "`send (Print (show x))`. Extract from a ",
        "`Value` with optics: `v ^? key \"f\" . _String` (also `_Int`, `_Double`, ",
        "`_Bool`, `_Array`); `renderJson :: Value -> Text` renders one to compact JSON.\n",
        "Pass large or quote-heavy content (file bodies, generated source, config) ",
        "as a real JSON value in the `input` param \u{2014} ",
        "the eval reads it via the `input` binding, so `code` stays a short verb. ",
        "Decode it into a typed record and the payload is available by field:\n",
        "  data Cfg = Cfg { target :: Text, limit :: Int } deriving (Generic, FromJSON)\n",
        "  do { Cfg{..} <- liftEither (resultToEither (fromJSON input)); Right hits <- grepGlob target \"**/*.rs\"; pure (take limit hits) }\n",
        "For a single field, optics read straight off the `Value`: ",
        "`input ^? key \"target\" . _String`; for a whole-file write, put the body on ",
        "`input`: `writeFile \".tidepool/lib/Mod.hs\" (input ^. _String)`.",
    ));

    if !effects.is_empty() {
        desc.push_str(concat!(
            "\nTyped effects cover the common operations directly: `glob`/`grepGlob` (FsRead) ",
            "for filesystem and structured text search; `run \"...\"` runs any shell command for the rest.\n",
            "For a JSON FILE, `readGlob` + `eitherDecode` + optics (`key`/`_String`/`values`/`cosmos`) is ",
            "the canonical query pattern \u{2014} decode each file to a `Value` and walk it with lenses; ",
            "`grepGlob`'s per-LINE `Hit` model is a poor fit for single-line/minified JSON, where the ",
            "whole file is one \"line\" (and now truncates as one oversized hit):\n",
            "  do { rs <- readGlob \"data/*.json\"; pure [ v ^? key \"status\" . _String ",
            "| r <- rs, Right t <- [r.contents], Right v <- [eitherDecode t] ] }\n",
            "Failure shape follows the verb's own signature, not one universal rule: most external ",
            "effects return `Either <EffectError> a` (bind the `Right`, match a specific `Left` to ",
            "recover); an absence query like `kvGet`/`fsMeta` returns `Maybe a` instead (no `Either` at ",
            "all); state/clock/form verbs (`kvSet`, `getCurrentTime`, `ask`) are total. Check ",
            "`tidepool://effect/{name}` when unsure:\n",
            "  do { Right p <- run \"git status --short\"; pure (T.lines p.stdout) }\n",
            "  readFile \"notes.md\" >>= \\case { Right body -> pure (T.length body); Left (FsNotFound _) -> pure 0 }\n",
            "`liftEither` unwraps a `Right` or aborts the eval on the `Left` (only meaningful for the ",
            "`Either` shape). In a fold ",
            "over many items, keep per-item failures as DATA and return both sides ",
            "(`partitionEithers`) \u{2014} an abort mid-batch discards the completed work.\n",
            "`sh \"cmd\"` is the happy-path form of `run`: stripped stdout on success, ",
            "throws with the exit code + stderr on nonzero \u{2014} reach for `run` directly when a ",
            "nonzero exit is itself data worth inspecting; qualified `Shell.`/`Git.`/`Cargo.` ",
            "cover the wider argv-typed shell/git/cargo surface when Exec is in the stack.\n",
            "Commit/log dates (e.g. from `gitShow`/`gitLog`) arrive strict ISO-8601 \u{2014} ",
            "`parseISO8601 c.date` then `formatDay` to bucket by day, `Map.fromListWith` to group.\n",
            "Effects (invoke via the helper verbs; read tidepool://effect/{name} for each one\u{2019}s constructors + helpers):\n",
        ));
        // DERIVED from the decls (crate::describe): one entry per effect —
        // name + first-sentence description + helper verb names. The
        // hand-written per-effect enumeration this replaced drifted from the
        // decls (the #25/#31/#41 class); now `:browse`, this description, and
        // the repl session_run description all render from the SAME derivation.
        desc.push_str(&crate::describe_effects_index(effects));

        let names: std::collections::HashSet<&str> = effects.iter().map(|e| e.type_name).collect();
        let has_llm = names.contains("Llm");
        let has_ask = names.contains("Ask");
        if has_llm && has_ask {
            desc.push_str(concat!(
                "\nStructured LLM / Ask (one Schema vocabulary; full detail in tidepool://schema):\n",
                "  Schema = SObj [(Text,Schema)] | SArr Schema | SStr | SNum | SBool | SEnum [Text] | SOpt Schema\n",
                "  ask schema prompt  -- SUSPEND to the caller; reply validated vs schema, no token burn\n",
                "  llm schema prompt  -- AUTONOMOUS server-side call (costs tokens); structured Value, no fences\n",
                "  bind the Right, then extract with optics:\n",
                "    do { Right v <- llm (SObj [(\"k\", SEnum [\"a\",\"b\"])]) p; pure (v ^? key \"k\" . _String) }\n",
            ));
        }

        desc.push_str(concat!(
            "\nEdit files: `update path old new` replaces the one exact occurrence of `old` ",
            "(include enough surrounding context to name it uniquely); `planUpdate` returns the ",
            "diff as data. The full editing surface (Edit DSL, diffs) is in tidepool://edits.\n",
        ));

        desc.push_str(concat!(
            "\nResources — this description is a FLOOR; pull the depth on demand via resources/read:\n",
            "  tidepool://guide           full guide: returning JSON, the `input` parameter, pagination, examples, failure isolation\n",
            "  tidepool://effect/{name}   per-effect constructors, types, and helper signatures\n",
            "  tidepool://schema          the Schema grammar + ask/llm in full\n",
            "  tidepool://edits           the declarative Edit verb JSON schema\n",
            "  tidepool://vocab           live project-library verb signatures (.tidepool/lib)\n",
            "  tidepool://capabilities    the Prelude shadow surface, names under a qualifier, and partial-function safe forms\n",
            "  tidepool://patterns        worked examples\n",
            "  tidepool://stdlib/{module} vendored stdlib module source (e.g. Tidepool.Prelude)\n",
        ));
    }

    desc
}

/// Extract the narrow source vocabulary shown by MCP help. This is indexing
/// metadata, not semantic Haskell introspection; `:type` and `:info` are GHC
/// operations owned by `tidepool-runtime`.
pub(crate) fn extract_sigs(src: &str) -> Vec<String> {
    fn valid_head(head: &str) -> bool {
        (head.starts_with(|character: char| character.is_ascii_lowercase() || character == '_')
            && head.chars().all(|character| {
                character.is_ascii_alphanumeric() || character == '_' || character == '\''
            }))
            || (head.starts_with('(') && head.ends_with(')'))
    }
    fn inline_head(line: &str) -> Option<&str> {
        if line.starts_with(char::is_whitespace) {
            return None;
        }
        let (head, _) = line.split_once("::")?;
        let head = head.trim_end();
        (!head.is_empty() && !head.contains(' ') && valid_head(head)).then_some(head)
    }

    let mut signatures = Vec::new();
    let mut current: Option<String> = None;
    let mut pending_head: Option<String> = None;
    for line in src.lines() {
        if inline_head(line).is_some() {
            if let Some(signature) = current.take() {
                signatures.push(signature);
            }
            pending_head = None;
            current = Some(line.to_string());
        } else if let Some(head) = pending_head.take() {
            let trimmed = line.trim();
            if line.starts_with(char::is_whitespace) && trimmed.starts_with("::") {
                current = Some(format!("{head} {trimmed}"));
            } else {
                if (line.starts_with("data ") || line.starts_with("type "))
                    && !line.contains("where")
                {
                    signatures.push(line.to_string());
                }
                let candidate = line.trim();
                if !line.starts_with(char::is_whitespace)
                    && !candidate.contains(char::is_whitespace)
                    && valid_head(candidate)
                {
                    pending_head = Some(candidate.to_string());
                }
            }
        } else if let Some(signature) = current.as_mut() {
            let trimmed = line.trim();
            if line.starts_with(char::is_whitespace)
                && !trimmed.is_empty()
                && !trimmed.starts_with("--")
            {
                signature.push(' ');
                signature.push_str(trimmed);
            } else if let Some(signature) = current.take() {
                signatures.push(signature);
            }
        } else if (line.starts_with("data ") || line.starts_with("type "))
            && !line.contains("where")
        {
            signatures.push(line.to_string());
        } else {
            let candidate = line.trim();
            if !line.starts_with(char::is_whitespace)
                && !candidate.contains(char::is_whitespace)
                && valid_head(candidate)
            {
                pending_head = Some(candidate.to_string());
            }
        }
    }
    if let Some(signature) = current {
        signatures.push(signature);
    }
    signatures
}

/// Parse the `module Library ( module A, module B, … ) where` re-export list
/// to learn which verb modules are actually IN SCOPE bare (the auto-imported
/// `Library` facade re-exports a curated subset — sibling modules like
/// `RustAudit`/`MechDemo` are excluded, usually because their names would clash).
/// Returns the set of re-exported module stems, or `None` if no `Library.hs` is
/// found / its export list can't be parsed (callers fall back to listing all).
fn library_inscope_modules(
    dirs: &[std::path::PathBuf],
) -> Option<std::collections::HashSet<String>> {
    for dir in dirs {
        let path = dir.join("Library.hs");
        let Ok(src) = std::fs::read_to_string(&path) else {
            continue;
        };
        let mods = parse_library_exports(&src);
        if !mods.is_empty() {
            return Some(mods);
        }
    }
    None
}

/// Pure parse of a `module Library ( module A, module B, … ) where` header into
/// the set of re-exported module stems. Returns empty if the header is absent.
fn parse_library_exports(src: &str) -> std::collections::HashSet<String> {
    let mut mods = std::collections::HashSet::new();
    // The export list runs from `module Library` to the closing `)`.
    let Some(after) = src.split_once("module Library").map(|(_, r)| r) else {
        return mods;
    };
    let list = after.split_once(')').map(|(l, _)| l).unwrap_or(after);
    for raw in list.split(',') {
        // Each entry looks like `module Schemes` (the FIRST also carries the
        // opening `(`, e.g. `( module Schemes`); strip the paren, then take the
        // token after `module`.
        let entry = raw.trim().trim_start_matches('(').trim();
        if let Some(rest) = entry.strip_prefix("module ") {
            if let Some(name) = rest.split_whitespace().next() {
                mods.insert(name.to_string());
            }
        }
    }
    mods
}

/// Scan a user-library directory for top-level type signatures (plus
/// `data`/`type` heads) and render a per-module vocabulary digest for
/// the eval tool description. This is the affordance that keeps eval
/// code shape-first: the combinators a user would otherwise re-invent
/// are visible at every call site instead of requiring a read of the
/// lib sources. Snapshot at server start; restart to refresh.
///
/// Only modules actually re-exported by the `Library` facade (hence in scope
/// bare) are listed — otherwise the digest would advertise verbs that fail with
/// "not in scope" (e.g. `RustAudit.panicSites`). Falls back to listing every
/// module when no parseable `Library.hs` is present.
///
/// `only`, when `Some(module)`, scopes the digest to that one module — a
/// deliberate `:vocab <module>` ask bypasses the in-scope gate (the caller
/// explicitly named it, so "not auto-imported" isn't a reason to hide it) and
/// returns an explicit "no module" note on no match instead of an empty blob.
pub fn library_vocab(dirs: &[std::path::PathBuf], only: Option<&str>) -> String {
    // Diagnostic modules, not vocabulary.
    const SKIP: &[&str] = &["Probe", "SelfTest"];
    const SIG_MAX: usize = 120;
    const TOTAL_MAX: usize = 8000;

    // Modules in scope bare (re-exported by `Library`); `None` ⇒ list all.
    let inscope = library_inscope_modules(dirs);

    // Layered across dirs (project first, then global): a module name seen in an
    // earlier dir shadows the same name later, so a project module overrides a
    // global one — matching GHC's first-match-wins include search.
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut found = false;
    let mut out = String::new();
    for dir in dirs {
        let mut files: Vec<std::path::PathBuf> = std::fs::read_dir(dir)
            .map(|rd| {
                rd.filter_map(|e| e.ok().map(|e| e.path()))
                    .filter(|p| p.extension().is_some_and(|x| x == "hs"))
                    .collect()
            })
            .unwrap_or_default();
        files.sort();

        for path in files {
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            if SKIP.contains(&stem) || !seen.insert(stem.to_string()) {
                continue;
            }
            if let Some(m) = only {
                if stem != m {
                    continue;
                }
            } else if let Some(ref ins) = inscope {
                // Only list modules the `Library` facade re-exports (in scope
                // bare). `Library` itself has no sigs of its own, so it's
                // naturally absent. An explicit `only` bypasses this gate.
                if stem != "Library" && !ins.contains(stem) {
                    continue;
                }
            }
            let Ok(src) = std::fs::read_to_string(&path) else {
                continue;
            };
            let sigs = extract_sigs(&src);
            if sigs.is_empty() {
                continue;
            }
            found = true;
            // Scope truth: tag each module bare-in-scope (a `Library`
            // re-export, importable without ceremony) vs needs-import. The
            // digest previously listed module-qualified names with no hint that
            // some require an explicit `import`; the tag closes that drift.
            let scope_tag = match &inscope {
                Some(ins) if stem == "Library" || ins.contains(stem) => {
                    "  -- bare (Library re-export)".to_string()
                }
                Some(_) => format!("  -- needs: import {stem}"),
                // No parseable Library.hs: scope is unknown, so don't assert.
                None => String::new(),
            };
            out.push_str(&format!("  {stem}:{scope_tag}\n"));
            for s in sigs {
                let s: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
                let s: String = s.chars().take(SIG_MAX).collect();
                out.push_str(&format!("    {s}\n"));
                if out.len() > TOTAL_MAX {
                    out.push_str("  …(truncated)\n");
                    return out;
                }
            }
        }
    }
    if let Some(m) = only {
        if !found {
            return format!("  no module '{m}' found in the library search path\n");
        }
    }
    out
}

#[cfg(test)]
mod vocab_tests {
    use super::{
        build_preamble_with_companions, parse_library_exports,
        session_decl_module_env_with_companions, CompanionImports,
    };

    #[test]
    fn companion_effect_imports_can_be_left_to_a_curated_facade() {
        let actor = crate::actor_decl();
        let included = build_preamble_with_companions(&[actor], false, CompanionImports::Include);
        let omitted = build_preamble_with_companions(&[actor], false, CompanionImports::Omit);
        assert!(included.contains("import Tidepool.Actor\n"));
        assert!(!omitted.contains("import Tidepool.Actor\n"));

        let decls =
            session_decl_module_env_with_companions(&[actor], false, CompanionImports::Omit);
        assert!(!decls
            .imports
            .iter()
            .any(|import| import == "import Tidepool.Actor"));
    }

    #[test]
    fn parses_reexport_list_and_excludes_siblings() {
        // Mirrors the real `.tidepool/lib/Library.hs` header shape.
        let src = "\
-- | Re-export facade.
module Library
  ( module Schemes
  , module Explore
  , module Extra
  ) where

import Schemes
import Explore
import Extra
";
        let mods = parse_library_exports(src);
        assert!(mods.contains("Schemes"), "Schemes should be in scope");
        assert!(mods.contains("Explore"));
        assert!(mods.contains("Extra"));
        // A sibling module that exists on disk but is NOT re-exported (the
        // `RustAudit.panicSites`-not-in-scope friction) must be absent.
        assert!(!mods.contains("RustAudit"), "RustAudit is not re-exported");
        assert_eq!(mods.len(), 3);
    }

    #[test]
    fn absent_header_yields_empty_set() {
        // No `module Library` header ⇒ empty ⇒ library_vocab falls back to all.
        assert!(parse_library_exports("module Schemes where\nfoo :: Int\n").is_empty());
    }

    /// A self-contained lib dir (no repo dependency) for `library_vocab` scoping tests.
    struct FixtureDir(std::path::PathBuf);
    impl FixtureDir {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "tidepool-vocab-fixture-{name}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            std::fs::create_dir_all(&dir).expect("create fixture dir");
            std::fs::write(
                dir.join("Alpha.hs"),
                "module Alpha where\n\nfooAlpha :: Int -> Int\n",
            )
            .unwrap();
            std::fs::write(
                dir.join("Beta.hs"),
                "module Beta where\n\nfooBeta :: Text -> Text\n",
            )
            .unwrap();
            // No `Library.hs` here ⇒ `inscope` is `None` ⇒ unscoped `:vocab` lists all.
            FixtureDir(dir)
        }
    }
    impl Drop for FixtureDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn library_vocab_scoped_to_one_module() {
        let fixture = FixtureDir::new("scoped");
        let dirs = vec![fixture.0.clone()];

        let all = super::library_vocab(&dirs, None);
        assert!(all.contains("Alpha:"), "unscoped should list Alpha: {all}");
        assert!(all.contains("Beta:"), "unscoped should list Beta: {all}");

        let alpha_only = super::library_vocab(&dirs, Some("Alpha"));
        assert!(
            alpha_only.contains("fooAlpha"),
            "scoped to Alpha should show fooAlpha: {alpha_only}"
        );
        assert!(
            !alpha_only.contains("Beta") && !alpha_only.contains("fooBeta"),
            "scoped to Alpha must NOT show Beta: {alpha_only}"
        );
    }

    #[test]
    fn library_vocab_scoped_to_missing_module_reports_not_found() {
        let fixture = FixtureDir::new("missing");
        let dirs = vec![fixture.0.clone()];

        let out = super::library_vocab(&dirs, Some("NoSuchModule"));
        assert!(
            out.contains("no module 'NoSuchModule' found"),
            "missing module should report clearly, not silently return empty: {out}"
        );
    }

    #[test]
    fn library_vocab_tags_scope_bare_vs_needs_import() {
        // A lib dir WITH a Library facade re-exporting only Alpha. Both modules
        // are on disk, but only Alpha is bare-in-scope; the digest must say so.
        let dir = std::env::temp_dir().join(format!(
            "tidepool-vocab-scopetag-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("Alpha.hs"),
            "module Alpha where\n\nfooAlpha :: Int\n",
        )
        .unwrap();
        std::fs::write(dir.join("Beta.hs"), "module Beta where\n\nfooBeta :: Int\n").unwrap();
        std::fs::write(
            dir.join("Library.hs"),
            "module Library ( module Alpha ) where\nimport Alpha\n",
        )
        .unwrap();

        // Beta is not re-exported, so the in-scope gate hides it from the
        // unscoped digest; Alpha is tagged as a bare re-export.
        let all = super::library_vocab(std::slice::from_ref(&dir), None);
        assert!(
            all.contains("Alpha:  -- bare (Library re-export)"),
            "Alpha should be tagged bare: {all}"
        );
        assert!(!all.contains("Beta:"), "Beta is not re-exported: {all}");

        // An explicit `:vocab Beta` bypasses the gate and marks it needs-import.
        let beta = super::library_vocab(std::slice::from_ref(&dir), Some("Beta"));
        assert!(
            beta.contains("Beta:  -- needs: import Beta"),
            "explicit Beta should be tagged needs-import: {beta}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
