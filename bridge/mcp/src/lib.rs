//! Shared Haskell effect declarations, generated preambles, and output capture.

mod eval_prep;
pub use eval_prep::*;
// The single failure taxonomy lives in tidepool-runtime; re-export it from the
// server facade so callers keep reaching it as `tidepool_mcp::FailureClass`.
pub use tidepool_runtime::{classify, FailureClass, FailureEnvelope, Phase};

mod effect_decls;
pub use effect_decls::*;

mod effect_defs;

mod fs_stable;
pub use fs_stable::*;

// Effect declarations generated from the `tidepool-protocol` schema. An
// effect appears here once its whole vertical has migrated; the rest are
// still expanded from `effect_defs`'s macros (migrated one effect at a time).
mod generated;
pub use generated::*;

mod preamble;
pub use preamble::*;

mod describe;
pub use describe::*;

mod lib_isolate;
pub use lib_isolate::*;

use parking_lot::Mutex;
use std::path::{Path, PathBuf};
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Templating
// ---------------------------------------------------------------------------

/// Stable effect vocabulary and installed orchestration source roots.
/// Executable rows are explicit checked invocation inputs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectsModuleDirs {
    /// Universal Core and both stable authored-facing facades.
    pub core: PathBuf,
    /// Helpers selected by the installed handler cohort, independent of the
    /// invoking actor's selected row.
    pub orchestration: PathBuf,
}

impl EffectsModuleDirs {
    #[must_use]
    pub fn include_paths(&self) -> [PathBuf; 2] {
        [self.core.clone(), self.orchestration.clone()]
    }
}

/// Materialize stable vocabulary and the installed cohort's orchestration
/// helpers. Every root is content-addressed and can be recreated after reap.
pub fn ensure_effects_module(effects: &[EffectDecl]) -> std::io::Result<EffectsModuleDirs> {
    let vocabulary = all_decls();
    for effect in effects {
        assert!(
            vocabulary
                .iter()
                .any(|known| known.type_name == effect.type_name),
            "effect row contains `{}`, which is absent from universal Tidepool.Effects.Core",
            effect.type_name
        );
    }
    Ok(EffectsModuleDirs {
        core: ensure_effects_core_module()?,
        orchestration: ensure_orchestrate_module(effects)?,
    })
}

/// Select the deployed stable vocabulary or materialize it for development.
pub fn ensure_effects_core_module() -> std::io::Result<PathBuf> {
    write_core_module(&effects_core_module_source())
}

/// Retain deployed vocabulary paths after comparing the genuine production sources.
pub fn write_core_module(core_src: &str) -> std::io::Result<PathBuf> {
    let authored = effects_authored_module_source();
    let facade = effects_facade_module_source();
    let files = [
        ("Effects/Core.hs", core_src),
        ("Effects/Authored.hs", authored.as_str()),
        ("Effects.hs", facade.as_str()),
    ];
    if let Some(selection) = tidepool_toolchain::toolchain::configured_module_source_selection()
        .map_err(std::io::Error::other)?
    {
        if core_src != effects_core_module_source() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "configured catalog requires this build's genuine stable Core source",
            ));
        }
        return select_deployed_core_module(&selection, &files);
    }
    write_module_dir("tidepool-effects-core", &files)
}

fn select_deployed_core_module(
    selection: &tidepool_toolchain::toolchain::NativeCatalogSourceSelection,
    files: &[(&str, &str); 3],
) -> std::io::Result<PathBuf> {
    let root = selection.root(tidepool_toolchain::toolchain::NativeSourceRole::StableEffects);
    for (relative, expected) in files {
        let path = root.join("Tidepool").join(relative);
        if std::fs::read(&path)? != expected.as_bytes() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "deployed stable effect source differs from this build: {}",
                    path.display()
                ),
            ));
        }
    }
    Ok(root)
}

/// Installed orchestration helpers keep their existing cohort-dependent bodies.
pub fn ensure_orchestrate_module(effects: &[EffectDecl]) -> std::io::Result<PathBuf> {
    write_orchestrate_module(&orchestrate_module_source(effects))
}

pub fn write_orchestrate_module(source: &str) -> std::io::Result<PathBuf> {
    write_module_dir("tidepool-orchestrate", &[("Orchestrate.hs", source)])
}

/// Process-level write-through cache for the content-addressed generated-module
/// directories, keyed on `(dir prefix, content hash)` — the prefix keeps the
/// core dir's cache entries from colliding with the orchestration dir's (or any future
/// caller's) even on a coincidental hash match across independent content.
///
/// Serializes concurrent writes within one process: the first call for a given
/// key acquires the lock and writes every file; concurrent callers block until
/// ALL of them are on disk, then get the cached path. This closes the TOCTOU
/// window where a caller could see one generated file but not a sibling and
/// compute a fingerprint / call GHC against an incomplete staging dir.
/// Inter-process safety (multiple `cargo test` binaries) is handled by the
/// atomic-rename primitive inside [`write_module_file`].
fn generated_module_write_cache(
) -> &'static Mutex<std::collections::HashMap<(&'static str, String), PathBuf>> {
    use std::sync::OnceLock;
    static CACHE: OnceLock<Mutex<std::collections::HashMap<(&'static str, String), PathBuf>>> =
        OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
}

/// Materialize a set of `(relative path under Tidepool/, source)` pairs into a
/// SINGLE content-addressed staging dir if absent, and return the dir (an
/// include root whose `Tidepool/` subtree holds every file). The dir hash
/// covers every source TOGETHER, so a change to any one busts the whole dir —
/// co-location means a caller that needs several files generated from the same
/// inputs (e.g. Core and its stable authored facades)
/// gets them for free, no extra include path to thread through.
///
/// `dir_prefix` names the staging-dir family (`"tidepool-effects-core"`,
/// `"tidepool-effects"`) — distinct callers get distinct dirs even if their
/// content hashes happened to collide, and it is what a human sees first when
/// listing the cache dir.
///
/// Concurrent calls within the same process are serialized: only one caller
/// writes the files at a time; others wait and reuse the cached result. This
/// prevents parallel tests from racing on a partially-written staging dir.
/// Inter-process races (parallel test binaries) are handled by the
/// atomic-rename primitive inside [`write_module_file`].
///
/// Self-heals if the staging dir is externally removed
/// (`rm -rf ~/.cache/tidepool`): the cache entry is evicted and the files are
/// re-materialized on the next call.
pub(crate) fn write_module_dir(
    dir_prefix: &'static str,
    files: &[(&str, &str)],
) -> std::io::Result<PathBuf> {
    // blake3, content-addressed and deterministic across processes (no
    // per-process SipHash seed like DefaultHasher, which would hash identical
    // source to different paths in each process → "Could not find module
    // Tidepool.Effects" when a second process picks a different cache dir
    // than the one that wrote it). Each source is its own length-framed
    // field (via `content_hash_hex`), so hashing them separately can never
    // collide with hashing their concatenation.
    let field_bytes: Vec<&[u8]> = files.iter().map(|(_, src)| src.as_bytes()).collect();
    let hash = content_hash_hex(&field_bytes);
    let root = tidepool_toolchain::paths::effects_dir().join(format!("{dir_prefix}-{hash}"));
    let module_dir = root.join("Tidepool");

    // Acquire the process-level serialization lock. Concurrent callers
    // (parallel test threads, concurrent eval requests) block here; the first
    // to proceed writes every file and stores the result; the rest take the
    // fast path below.
    let mut cache = generated_module_write_cache().lock();
    let key = (dir_prefix, hash.clone());

    // Fast path: previously written this key AND files still present.
    if cache.contains_key(&key) {
        if files.iter().all(|(rel, _)| module_dir.join(rel).exists()) {
            return Ok(root);
        }
        // Files were externally removed. Evict the stale entry and fall
        // through to re-materialize while still holding the lock.
        cache.remove(&key);
    }

    // Slow path: write every file (still under the lock so concurrent callers
    // wait rather than racing into the same write sequence).
    for (rel, src) in files {
        write_module_file(&module_dir, rel, src)?;
    }
    cache.insert(key, root.clone());
    Ok(root)
}

/// Atomically write `<module_dir>/<rel>` with `src` if it does not already
/// exist. `rel` may itself contain a directory separator (e.g.
/// `"Effects/Core.hs"`), whose parent is created alongside `module_dir`.
/// Concurrent writers use distinct temporary files and publish by rename.
pub(crate) fn write_module_file(module_dir: &Path, rel: &str, src: &str) -> std::io::Result<()> {
    let module_path = module_dir.join(rel);
    if !module_path.exists() {
        let Some(parent) = module_path.parent() else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("generated module path `{rel}` has no parent directory"),
            ));
        };
        std::fs::create_dir_all(parent)?;
        tidepool_atomic_write::write_best_effort(&module_path, src.as_bytes())?;
    }
    Ok(())
}

/// Blake3 content-address hash of `fields`, each framed with its own byte
/// length before hashing so two different field splits can never collide
/// (e.g. `["ab", "c"]` hashing the same as `["a", "bc"]` would under bare
/// concatenation). Truncated to 32 hex chars (128 bits) — collision-safe for
/// a content-addressed staging dir name.
pub(crate) fn content_hash_hex(fields: &[&[u8]]) -> String {
    let mut h = blake3::Hasher::new();
    for f in fields {
        h.update(&(f.len() as u64).to_le_bytes());
        h.update(f);
    }
    h.finalize().to_hex()[..32].to_string()
}

// ---------------------------------------------------------------------------
// Output capture
// ---------------------------------------------------------------------------

/// Captured output from effect handlers (e.g., Console Print).
///
/// Clone is cheap (Arc-backed). Thread-safe for use across spawn_blocking.
/// `parking_lot::Mutex` (the file-wide choice) — no poisoning, so `.lock()`
/// hands back the guard directly.
#[derive(Clone, Default)]
pub struct CapturedOutput {
    lines: Arc<Mutex<Vec<String>>>,
}

impl CapturedOutput {
    pub fn new() -> Self {
        Self::default()
    }

    /// Push a line of output.
    pub fn push(&self, line: String) {
        self.lines.lock().push(line);
    }

    /// Drain all captured lines, returning them and clearing the buffer.
    pub fn drain(&self) -> Vec<String> {
        std::mem::take(&mut *self.lines.lock())
    }

    /// Snapshot current captured lines without clearing the buffer.
    pub fn snapshot(&self) -> Vec<String> {
        self.lines.lock().clone()
    }
}

impl tidepool_runtime::session::OutputSink for CapturedOutput {
    fn drain(&self) -> Vec<String> {
        CapturedOutput::drain(self)
    }

    fn snapshot(&self) -> Vec<String> {
        CapturedOutput::snapshot(self)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deployed_core_retains_original_sources_and_refuses_changed_or_missing_bytes() {
        use tidepool_toolchain::toolchain::{NativeCatalogSourceSelection, NativeSourceRole};
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().join("effects");
        let core = effects_core_module_source();
        let authored = effects_authored_module_source();
        let facade = effects_facade_module_source();
        let files = [
            ("Effects/Core.hs", core.as_str()),
            ("Effects/Authored.hs", authored.as_str()),
            ("Effects.hs", facade.as_str()),
        ];
        for (relative, source) in files {
            let path = root.join("Tidepool").join(relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, source).unwrap();
        }
        let selection = NativeCatalogSourceSelection {
            snapshot_root: fixture.path().to_path_buf(),
            roles: NativeSourceRole::ORDERED,
            source_files: NativeCatalogSourceSelection::source_manifest(fixture.path()).unwrap(),
        };
        let manifest = selection.source_files.clone();
        assert_eq!(
            select_deployed_core_module(&selection, &files).unwrap(),
            root
        );
        assert_eq!(
            NativeCatalogSourceSelection::source_manifest(fixture.path()).unwrap(),
            manifest
        );
        for (relative, source) in files {
            let path = root.join("Tidepool").join(relative);
            std::fs::write(&path, "changed").unwrap();
            assert_eq!(
                select_deployed_core_module(&selection, &files)
                    .unwrap_err()
                    .kind(),
                std::io::ErrorKind::InvalidData
            );
            std::fs::remove_file(&path).unwrap();
            assert_eq!(
                select_deployed_core_module(&selection, &files)
                    .unwrap_err()
                    .kind(),
                std::io::ErrorKind::NotFound
            );
            std::fs::write(path, source).unwrap();
        }
        let mut changed = files;
        changed[0].1 = "caller changed Core";
        assert!(select_deployed_core_module(&selection, &changed).is_err());
    }

    /// Core module + facade module + orchestrate module + preamble concatenated:
    /// content assertions that predate the importable-module split (and the
    /// stable vocabulary split) check against the union of all generated
    /// sources the eval sees. NOT compilable as one file (two `module`
    /// headers) — `.contains()` assertions only.
    fn generated_sources(effects: &[EffectDecl], user_library: bool) -> String {
        let mut s = effects_core_module_source();
        s.push_str(&effects_facade_module_source());
        s.push_str(&orchestrate_module_source(effects));
        s.push_str(&build_preamble(effects, user_library));
        s
    }

    #[test]
    fn test_preamble_grep_glob_present() {
        let effects = vec![fs_read_decl()];
        let preamble = generated_sources(&effects, false);
        // grepGlob is the FsRead structured text-search verb (the SG structural
        // combinators it used to sit beside were cut with the SG effect).
        assert!(preamble.contains("grepGlob :: forall effs. Member FsRead effs => Text -> FilePath -> Eff effs (Either FsError [Hit])"));
    }

    #[test]
    fn test_preamble_qq_pragmas_always_on() {
        // Root decision: one eval dialect everywhere. See the FIXME at the
        // pragma line in build_preamble for the latency cost this carries
        // (extension-keyed TH provisioning) and the unpoison-fixed-binary
        // requirement it implies.
        for (src, name) in [
            (build_preamble(&[], false), "preamble"),
            (build_preamble(&[fs_read_decl()], true), "preamble+lib"),
        ] {
            let pragma_line = src.lines().next().unwrap();
            assert!(
                pragma_line.contains("QuasiQuotes"),
                "{name}: QuasiQuotes missing from pragma line"
            );
            assert!(
                pragma_line.contains("ViewPatterns"),
                "{name}: ViewPatterns missing from pragma line"
            );
        }
    }

    #[test]
    fn test_template_haskell_qq_import_placement() {
        let pre = build_preamble(&[], false);
        // mirror eval()'s assembly for a QQ-using request
        let code = "pure [fmt|hello {name}|]";
        let mut imports = aeson_imports();
        if uses_qq(code) {
            imports.push_str("Tidepool.QQ (fmt, j, patch, uri)\n");
        }
        let src = template_haskell(&pre, "'[]", code, &imports, "", None);
        let qq = src
            .find("import Tidepool.QQ (fmt, j, patch, uri)\n")
            .expect("QQ import missing from rendered module");
        let default_decl = src.find("default (Int").unwrap();
        assert!(qq < default_decl, "QQ import must precede default decl");
    }

    #[test]
    fn test_no_qq_import_without_token() {
        let pre = build_preamble(&[], false);
        let code = "pure [x | x <- xs]";
        let mut imports = aeson_imports();
        if uses_qq(code) {
            imports.push_str("Tidepool.QQ (fmt, j, patch, uri)\n");
        }
        let src = template_haskell(&pre, "'[]", code, &imports, "", None);
        assert!(
            !src.contains("Tidepool.QQ"),
            "no-splice eval must not import Tidepool.QQ"
        );
    }

    #[test]
    fn test_build_preamble() {
        let effects = vec![
            EffectDecl {
                type_name: "Console",
                description: "Print output",
                constructors: &["Print :: Text -> Console ()"],
                type_defs: &[],
                extra_imports: &[],
                helpers: &[],
                type_params: &[],
                default_row_args: &[],
                prompt_card: None,
            },
            EffectDecl {
                type_name: "KV",
                description: "Key-value store",
                constructors: &[
                    "KvGet :: Text -> KV (Maybe Text)",
                    "KvSet :: Text -> Text -> KV ()",
                ],
                type_defs: &[],
                extra_imports: &[],
                helpers: &[],
                type_params: &[],
                default_row_args: &[],
                prompt_card: None,
            },
        ];
        let preamble = generated_sources(&effects, false);
        assert!(preamble.contains("data Console a where"));
        assert!(preamble.contains("  Print :: Text -> Console ()"));
        assert!(preamble.contains("data KV a where"));
    }

    /// Drift guard: the session decl `ModuleEnv` MUST stay a subset of the eval
    /// preamble's pragmas+imports, so a `session_def` helper sees the same
    /// vocabulary an `eval`/`session_eval` expression does. If someone adds an
    /// import to the eval preamble but not `eval_import_lines`, this catches it.
    #[test]
    fn session_decl_env_matches_eval_preamble() {
        // Exec+Http present so both the decl env and the eval preamble emit
        // the qualified Tidepool.Shell/Git/Cargo imports (gated identically on
        // the same effect pair in both).
        let env = session_decl_module_env(&[exec_decl(), http_decl()], false);
        let preamble = build_preamble(&[exec_decl(), http_decl()], false);
        // Every decl import line appears verbatim in the eval preamble.
        for imp in &env.imports {
            assert!(
                preamble.contains(&format!("{imp}\n")),
                "session decl import `{imp}` missing from eval preamble — drift"
            );
        }
        // Same pragma block, modulo the persistent declaration environment's intentional
        // NoMonomorphismRestriction (decl_pragmas adds it so nullary
        // constrained binds generalize; the eval expr module must NOT carry it
        // — see decl_pragmas). Strip that one addition, then the blocks match.
        let decl_pragmas_sans_nmr = env.pragmas.replace("NoMonomorphismRestriction, ", "");
        assert!(
            preamble.contains(&decl_pragmas_sans_nmr),
            "session decl pragmas (minus decl-only NMR) diverged from eval preamble"
        );
        // The decl env is qualified-imports-only: no unqualified `import Library`
        // (it would clash with decl-defined names — see hide_library_names; the
        // shell modules are safe because they're imported qualified).
        assert!(!env.imports.iter().any(|i| i == "import Library"));
    }

    #[test]
    fn test_template_haskell() {
        let effects = vec![EffectDecl {
            type_name: "Console",
            description: "",
            constructors: &["Print :: Text -> Console ()"],
            type_defs: &[],
            extra_imports: &[],
            helpers: &[],
            type_params: &[],
            default_row_args: &[],
            prompt_card: None,
        }];
        let preamble = build_preamble(&effects, false);
        let stack = build_effect_stack_type(&effects);
        let source = "do\n  let x = 42\n  pure x";

        let result = template_haskell(&preamble, &stack, source, "", "", None);

        assert!(result.contains("module Expr where"));
        assert!(result.contains("import Control.Monad.Freer hiding (run)"));
        // GADTs live in the generated Tidepool.Effects module now.
        assert!(result.contains("import Tidepool.Effects"));
        assert!(effects_core_module_source_for(&effects).contains("data Console a where"));
        // User code is a real top-level binding (expression-first contract).
        assert!(result.contains("__user = let {\n __b =\ndo\n  let x = 42\n  pure x\n } in __b"));
        assert!(result.contains("result :: Eff '[Console] Value"));
        assert!(result.contains("result = do"));
        assert!(result.contains("  _r <- __user"));
    }

    #[test]
    fn test_template_haskell_expression_forms() {
        let effects = vec![EffectDecl {
            type_name: "Console",
            description: "",
            constructors: &["Print :: Text -> Console ()"],
            type_defs: &[],
            extra_imports: &[],
            helpers: &[],
            type_params: &[],
            default_row_args: &[],
            prompt_card: None,
        }];
        let preamble = build_preamble(&effects, false);
        let stack = build_effect_stack_type(&effects);

        // Multi-line composition expression rides through VERBATIM (explicit
        // let-brackets suspend layout; no indent transform).
        let pipeline = "glob \"**/*.rs\"\n  >>= mapM getFileSize\n  <&> sizeRank 9";
        let r = template_haskell(&preamble, &stack, pipeline, "", "", None);
        assert!(r.contains(
            "__user = let {\n __b =\nglob \"**/*.rs\"\n  >>= mapM getFileSize\n  <&> sizeRank 9\n } in __b"
        ));

        // Trailing where-clause is legal: __user is a genuine declaration.
        let with_where = "sizeRank 9 <$> sized\n  where\n    sized = mapM go =<< glob \"**/*.rs\"";
        let r = template_haskell(&preamble, &stack, with_where, "", "", None);
        assert!(r.contains("__user = let {\n __b =\nsizeRank 9 <$> sized\n  where\n    sized ="));
    }

    #[test]
    fn test_extract_sigs() {
        let src = "\
{-# LANGUAGE NoImplicitPrelude #-}
-- | A comment with a fake sig :: not real
module Lib where

import Tidepool.Prelude

-- | Single-line.
oracle :: Text -> M Text
oracle q = do
  a <- ask q
  pure (vshow a)

-- | Multi-line: continuations join.
steerM :: Monad m
       => (Int -> Int -> a -> m r)
       -> b -> [a] -> m b
steerM suspend step = go 0
  where
    go _ acc [] = pure acc

type Vocab s = [(Text, Text -> s -> M s)]
data Rose a = Rose a [Rose a]
data Console a where
  Print :: Text -> Console ()

(<?>) :: Q a -> Text -> M a
(Q s p t) <?> prompt = undefined
";
        let sigs = extract_sigs(src);
        assert!(sigs.contains(&"oracle :: Text -> M Text".to_string()));
        assert!(sigs.contains(
            &"steerM :: Monad m => (Int -> Int -> a -> m r) -> b -> [a] -> m b".to_string()
        ));
        assert!(sigs.contains(&"type Vocab s = [(Text, Text -> s -> M s)]".to_string()));
        assert!(sigs.contains(&"data Rose a = Rose a [Rose a]".to_string()));
        assert!(sigs.contains(&"(<?>) :: Q a -> Text -> M a".to_string()));
        // GADT `where` heads and indented constructor sigs are excluded;
        // comment-embedded `::` never matches.
        assert!(!sigs.iter().any(|s| s.contains("Console")));
        assert!(!sigs.iter().any(|s| s.contains("fake sig")));
        // Function bodies never leak into signatures.
        assert!(!sigs.iter().any(|s| s.contains("go 0")));
    }

    #[test]
    fn test_preamble_includes_helpers() {
        let decls = standard_decls();
        let preamble = generated_sources(&decls, false);
        // Standard Haskell names as primary — assert the SIGNATURE lines,
        // not the `= send . …` bodies (body wording is volatile; the
        // signature is the stable contract eval authors depend on).
        assert!(preamble.contains("putStrLn :: Members '[Console, KV] effs => Text -> Eff effs ()"));
        // #335: the primitive filesystem verbs expose typed failure; the composite
        // helpers below (appendFile/doesFileExist/…) absorb it and keep their
        // shape.
        assert!(preamble.contains(
            "readFile :: forall effs. Member FsRead effs => FilePath -> Eff effs (Either FsError Text)"
        ));
        assert!(preamble.contains("writeFile :: forall effs. Member FsWrite effs => FilePath -> Text -> Eff effs (Either FsError ())"));
        assert!(preamble.contains("appendFile :: forall effs. Members '[FsRead, FsWrite] effs => FilePath -> Text -> Eff effs (Either FsError ())"));
        assert!(preamble.contains("listDirectory :: forall effs. Member FsRead effs => FilePath -> Eff effs (Either FsError [FilePath])"));
        assert!(preamble.contains(
            "doesFileExist :: forall effs. Member FsRead effs => FilePath -> Eff effs Bool"
        ));
        assert!(preamble.contains(
            "getFileSize :: forall effs. Member FsRead effs => FilePath -> Eff effs (Maybe Int)"
        ));
        assert!(preamble.contains(
            "fsMeta :: forall effs. Member FsRead effs => FilePath -> Eff effs (Maybe FileMeta)"
        ));
        assert!(preamble.contains("glob :: forall effs. Member FsRead effs => FilePath -> Eff effs (Either FsError [FilePath])"));
        // Core editing verbs (the str-replace common case + dry-run).
        assert!(preamble.contains("update :: forall effs. Members '[FsRead, FsWrite] effs => FilePath -> Text -> Text -> Eff effs UpdateOneOutcome"));
        assert!(preamble.contains("updateAll :: forall effs. Members '[FsRead, FsWrite] effs => FilePath -> Text -> Text -> Eff effs UpdateAllOutcome"));
        assert!(preamble.contains("planUpdate :: forall effs. Member FsRead effs => FilePath -> Text -> Text -> Eff effs UpdateOutcome"));
        assert!(
            preamble.contains("insertAfter :: forall effs. Members '[FsRead, FsWrite] effs => FilePath -> Text -> Text -> Eff effs InsertAfterOutcome")
        );
        assert!(preamble.contains(
            "run :: forall effs. Member Exec effs => Text -> Eff effs (Either ExecError Proc)"
        ));
        // No old aliases (verb-type sweep: records over tuples, no dup names)
        assert!(!preamble.contains("fsRead"));
        assert!(!preamble.contains("fsWrite"));
        assert!(!preamble.contains("callCommand"));
        assert!(!preamble.contains("readProcess"));
        assert!(!preamble.contains("fsGlob"));
        assert!(!preamble.contains("fsMetadata"));
        assert!(!preamble.contains("parseFileMeta"));
        // `say` is the Console wrapper (re-added 2026-06-22, friction #5).
        assert!(preamble.contains("say :: forall effs. Member Console effs => Text -> Eff effs ()"));
        // KV storage failures are now typed at the effect boundary.
        assert!(preamble
            .contains("kvGet :: forall effs. Member KV effs => Text -> Eff effs (Either KvError (Maybe Value))"));
        // #335: httpGet is errors-tagged.
        assert!(preamble.contains(
            "httpGet :: forall effs. Member Http effs => Text -> Eff effs (Either HttpError Value)"
        ));
        // `ask` itself now lives in the stdlib (`Tidepool.Form.Schema`,
        // auto-imported here); only the thin `askRaw` verb wrapper is
        // generated.
        assert!(preamble
            .contains("askRaw :: forall effs. Member Ask effs => Text -> Value -> Eff effs Value"));
        assert!(preamble.contains("import Tidepool.Form.Schema"));
    }

    #[test]
    fn test_ask_decl() {
        let decl = ask_decl();
        assert_eq!(decl.type_name, "Ask");
        // Bare `Ask` was reaped with the structured-Ask collapse; only AskWith
        // (schema-carrying) remains.
        assert_eq!(decl.constructors.len(), 1);
        assert!(decl.constructors[0].contains("AskWith :: Text -> Value -> Ask Value"));
        // The Schema vocabulary (`data Schema`/`schemaToValue`/`isOpt`/
        // `innerSchema`) AND `ask` itself now live in the stdlib
        // (`Tidepool.Form.Schema`), auto-imported via `extra_imports` since
        // Ask is always present in every stack, reaching .tidepool/lib
        // modules and Llm-less stacks exactly as the old inline `type_defs`
        // did. Only the thin `askRaw` verb wrapper stays in the decl itself
        // — the generated module cannot import authored library code.
        let type_defs = decl.type_defs.join("\n");
        assert!(!type_defs.contains("data Schema"));
        assert!(!type_defs.contains("data Q a"));
        assert!(decl.extra_imports.contains(&"import Tidepool.Form.Schema"));
        let helpers = decl.helpers.join("\n");
        assert!(helpers
            .contains("askRaw :: forall effs. Member Ask effs => Text -> Value -> Eff effs Value"));
        assert!(helpers.contains("askRaw prompt payload = send (AskWith prompt payload)"));
        assert!(!helpers.contains("schemaToValue"));
        assert!(!helpers
            .contains("ask :: forall effs. Member Ask effs => Schema -> Text -> Eff effs Value"));
        assert!(!helpers.contains("askQ"));
    }

    #[test]
    fn test_standard_decls_includes_ask() {
        let decls = standard_decls();
        assert_eq!(decls.len(), 11);
        assert_eq!(decls[2].type_name, "FsRead");
        assert_eq!(decls[3].type_name, "FsWrite");
        assert_eq!(decls[4].type_name, "Http");
        assert_eq!(decls[5].type_name, "Exec");
        assert_eq!(decls[6].type_name, "Llm");
        assert_eq!(decls[7].type_name, "Git");
        assert_eq!(decls[8].type_name, "Time");
        assert_eq!(decls[9].type_name, "Entropy");
        assert_eq!(decls[10].type_name, "Ask");
    }

    #[test]
    fn test_ask_in_preamble() {
        let decls = standard_decls();
        let preamble = generated_sources(&decls, false);
        assert!(preamble.contains("data Ask a where"));
        assert!(preamble.contains("  AskWith :: Text -> Value -> Ask Value"));
        assert_eq!(
            build_effect_stack_type(&decls),
            "'[Console, KV, FsRead, FsWrite, Http, Exec, Llm, Git, Time, Entropy, Ask]"
        );
    }

    #[test]
    fn test_ask_in_effect_stack_type() {
        let decls = standard_decls();
        let stack = build_effect_stack_type(&decls);
        assert_eq!(
            stack,
            "'[Console, KV, FsRead, FsWrite, Http, Exec, Llm, Git, Time, Entropy, Ask]"
        );
    }

    #[test]
    fn test_preamble_hides_run_from_freer() {
        let decls = standard_decls();
        let preamble = generated_sources(&decls, false);
        assert!(preamble.contains("import Control.Monad.Freer hiding (run)"));
        // Our run helper should still be present (#335: errors-tagged).
        assert!(preamble.contains(
            "run :: forall effs. Member Exec effs => Text -> Eff effs (Either ExecError Proc)\nrun = send . Run"
        ));
    }

    #[test]
    fn test_preamble_text_error_shadow() {
        let decls = standard_decls();
        let preamble = generated_sources(&decls, false);
        // Prelude error (String-based) is hidden
        assert!(preamble.contains("import Tidepool.Prelude hiding (error)"));
        // Text-taking error is defined via qualified Prelude
        assert!(preamble.contains("import qualified Prelude as P"));
        // Assert the Text-taking `error` SIGNATURE (the shadow contract);
        // the `= P.error . T.unpack` body is an implementation detail.
        assert!(preamble.contains("error :: Text -> a"));
    }

    #[test]
    fn test_exec_decl() {
        let decl = exec_decl();
        assert_eq!(decl.type_name, "Exec");
        // #335: Run/RunIn are errors-tagged.
        assert!(decl
            .constructors
            .iter()
            .any(|c| c.contains("Run :: Text -> Exec (Either ExecError Proc)")));
        assert!(decl
            .constructors
            .iter()
            .any(|c| c.contains("RunIn :: Text -> Text -> Exec (Either ExecError Proc)")));
    }

    #[test]
    fn test_preamble_orchestration_helpers() {
        let decls = standard_decls();
        // The orchestration helpers moved OUT of the expr-module preamble into
        // the generated Tidepool.Orchestrate module (the namespace-poison fix);
        // assert their signatures there instead.
        let orch = orchestrate_module_source(&decls);
        // runChecked runs a command and returns stdout, erroring on nonzero
        // exit (assert the signature; the body is volatile).
        assert!(orch.contains("runChecked :: Member Exec effs => Text -> Eff effs Text"));
        // File manipulation helpers
        assert!(orch.contains(
            "mapFile :: Members '[FsRead, FsWrite] effs => Text -> (Text -> Text) -> Eff effs ()"
        ));
        assert!(orch.contains("mapFileM :: Members '[FsRead, FsWrite] effs => Text -> (Text -> Eff effs Text) -> Eff effs ()"));
        assert!(
            orch.contains("searchFiles :: Member FsRead effs => Text -> Text -> Eff effs [Hit]")
        );
        assert!(orch.contains("lineCount :: Member FsRead effs => Text -> Eff effs Int"));
        assert!(
            orch.contains("fileContains :: Member FsRead effs => Text -> Text -> Eff effs Bool")
        );
        // The primitive kvClear owns deletion; orchestration adds no duplicate helper.
        assert!(
            orch.contains("kvAll :: Member KV effs => Eff effs (Either KvError [(Text, Value)])")
        );
        assert!(!orch.contains("kvClear ::"));
        assert!(orch.contains("runAll :: Member Exec effs => [Text] -> Eff effs [Proc]"));
        // The expr-module preamble no longer splices these bodies — it imports
        // the module and only emits the paginateResult alias.
        let preamble = build_preamble(&decls, true);
        assert!(preamble.contains("import Tidepool.Orchestrate"));
        assert!(!preamble.contains("runChecked :: Member Exec effs => Text -> Eff effs Text"));
        assert!(!preamble.contains("searchFiles ::"));
        // The structured Ask/Llm surface's thin verb wrappers live in the
        // generated Tidepool.Effects module — one Schema vocabulary, extract
        // with optics. The Q-builder DSL and the `??`/`?!`/triage/survey/sift
        // sugar are removed. `data Schema`/`schemaToValue`/`ask` itself now
        // live in the stdlib (`Tidepool.Form.Schema`) — the generated module
        // cannot import authored library code, so it keeps only the thin
        // `askRaw` verb wrapper (bare `send (AskWith …)`, no `Schema`
        // reference at all).
        let effects_mod = effects_core_module_source_for(&decls);
        assert!(!effects_mod.contains("data Schema = SObj"));
        assert!(!effects_mod.contains("import Tidepool.Form.Schema"));
        assert!(!effects_mod
            .contains("ask :: forall effs. Member Ask effs => Schema -> Text -> Eff effs Value"));
        assert!(effects_mod
            .contains("askRaw :: forall effs. Member Ask effs => Text -> Value -> Eff effs Value"));
        assert!(effects_mod.contains("askRaw prompt payload = send (AskWith prompt payload)"));
        // #335: llm is errors-tagged (fully total — budget exhaustion is DATA);
        // tryLlm is gone (llm supersedes it). The composed `llm` (which calls
        // `schemaToValue`) lives in its own `Tidepool.Llm` module, same split
        // as `ask`/`askRaw` — only the thin `llmRaw` verb wrapper is
        // generated. `Tidepool.Llm` is a SEPARATE module from
        // `Tidepool.Form.Schema` (not merely a different name for the same
        // one): `Ask` is universal but `Llm` is not, so folding `llm` into
        // the always-imported `Tidepool.Form.Schema` broke every Ask-only
        // roster (e.g. `build_minimal_stack`'s Console-only rosters) — see
        // `extra_imports_for!(Llm)`'s own arm in `effect_defs.rs`.
        assert!(!effects_mod.contains(
            "llm :: forall effs. Member Llm effs => Schema -> Text -> Eff effs (Either LlmError Value)"
        ));
        assert!(effects_mod.contains(
            "llmRaw :: forall effs. Member Llm effs => Text -> Value -> Eff effs (Either LlmError Value)"
        ));
        assert!(effects_mod.contains("llmRaw prompt payload = send (LlmStructured prompt payload)"));
        assert!(!effects_mod.contains("tryLlm"));
        // The removed Q layer + sugar are gone.
        assert!(!effects_mod.contains("data Q a"));
        assert!(!effects_mod.contains("askQ ::"));
        assert!(!effects_mod.contains("llmQ ::"));
        assert!(!effects_mod.contains("llmJson ::"));
        assert!(!effects_mod.contains("pick :: [Text] -> Q Text"));
        assert!(!effects_mod.contains("(??)"));
        assert!(!effects_mod.contains("(?!)"));
        assert!(!effects_mod.contains("triage ::"));
        assert!(!effects_mod.contains("survey ::"));
        assert!(!effects_mod.contains("sift ::"));
        // and NOT duplicated in the preamble (one definition site)
        assert!(!preamble.contains("data Schema = SObj"));
        // askRaw lives in ask_decl (always present), so it survives an Llm-less stack
        let no_llm: Vec<EffectDecl> = standard_decls()
            .into_iter()
            .filter(|d| d.type_name != "Llm")
            .collect();
        let no_llm_mod = effects_core_module_source_for(&no_llm);
        assert!(no_llm_mod
            .contains("askRaw :: forall effs. Member Ask effs => Text -> Value -> Eff effs Value"));
        // llm needs the Llm effect — absent from an Llm-less stack.
        assert!(!no_llm_mod.contains("llm :: forall effs. Member Llm effs => Schema -> Text -> Eff effs (Either LlmError Value)"));
    }

    #[test]
    fn test_orchestration_is_pure_fn_of_effects() {
        // Tidepool.Orchestrate depends on the installed handler cohort and has
        // its own content-addressed source root. The preamble imports its bodies
        // regardless of the user_library flag.
        let decls = standard_decls();
        assert!(!build_preamble(&decls, false).contains("runChecked"));
        assert!(!build_preamble(&decls, true).contains("runChecked"));
        // The module carries them based on effects alone (Exec present here).
        let orch = orchestrate_module_source(&decls);
        assert!(orch.contains("runChecked :: Member Exec effs => Text -> Eff effs Text"));
        assert!(orch.contains("import Tidepool.Effects.Authored\n"));
        for signature in orch.lines().filter(|line| line.contains(" :: ")) {
            assert!(
                !signature.split_whitespace().any(|token| token == "M"),
                "imported orchestration helper depends on the selected row: {signature}"
            );
        }
        let console_only = orchestrate_module_source(&[console_decl()]);
        assert!(console_only.contains("putStrLn :: Member Console effs => Text -> Eff effs ()"));
        assert!(console_only
            .contains("paginateTrunc :: Member Console effs => Int -> Value -> Eff effs Value"));
        let no_console: Vec<_> = decls
            .iter()
            .filter(|d| d.type_name != "Console")
            .cloned()
            .collect();
        let no_console = orchestrate_module_source(&no_console);
        assert!(!no_console.contains("putStrLn ::"));
        assert!(no_console.contains("paginateTrunc :: Int -> Value -> Eff effs Value"));
        // An Exec-less stack omits the Exec-gated helpers.
        let no_exec: Vec<EffectDecl> = standard_decls()
            .into_iter()
            .filter(|d| d.type_name != "Exec")
            .collect();
        assert!(!orchestrate_module_source(&no_exec).contains("runChecked"));
    }

    #[test]
    fn test_parse_constructor_no_args() {
        let p = parse_constructor("GitBranches :: Git [Value]").unwrap();
        assert_eq!(
            p,
            ParsedConstructor {
                name: "GitBranches".into(),
                arity: 0
            }
        );
    }

    #[test]
    fn test_parse_constructor_two_args() {
        let p = parse_constructor("GitLog :: Text -> Int -> Git [Value]").unwrap();
        assert_eq!(
            p,
            ParsedConstructor {
                name: "GitLog".into(),
                arity: 2
            }
        );
    }

    #[test]
    fn test_parse_constructor_nested_types() {
        let p = parse_constructor("FakeReq :: Text -> Text -> [(Text,Text)] -> Text -> Fake Value")
            .unwrap();
        assert_eq!(
            p,
            ParsedConstructor {
                name: "FakeReq".into(),
                arity: 4
            }
        );
    }

    #[test]
    fn test_preamble_required_imports() {
        let decls = standard_decls();
        let preamble = build_preamble(&decls, false);
        assert!(preamble.contains("import Tidepool.Prelude hiding (error)"));
        assert!(preamble.contains("import qualified Tidepool.Data.Text as T"));
        // The preamble explicitly names the type alongside its qualified API.
        assert!(preamble.contains("import Data.Text (Text)"));
        assert!(preamble.contains("import Control.Monad.Freer hiding (run)"));
        assert!(preamble.contains("import qualified Tidepool.Aeson.KeyMap as KM"));
    }

    #[test]
    fn test_template_haskell_truncation() {
        let effects = vec![EffectDecl {
            type_name: "Console",
            description: "",
            constructors: &["Print :: Text -> Console ()"],
            type_defs: &[],
            extra_imports: &[],
            helpers: &[],
            type_params: &[],
            default_row_args: &[],
            prompt_card: None,
        }];
        let preamble = build_preamble(&effects, false);
        let stack = build_effect_stack_type(&effects);
        let source = "pure 42";

        // With budget
        let result = template_haskell(&preamble, &stack, source, "", "", Some(1024));
        assert!(result.contains("kvSet \"__sayChars\" (toJSON (0 :: Int)) >>= liftEither"));
        assert!(result.contains("paginateResult (max 100 (1024 - _sayC)) (toJSON _r)"));

        // Without budget (defaults to 4096)
        let result = template_haskell(&preamble, &stack, source, "", "", None);
        assert!(result.contains("paginateResult 4096 (toJSON _r)"));
    }

    #[test]
    fn test_effect_decls_basic_validation() {
        let console = console_decl();
        assert_eq!(console.type_name, "Console");
        assert!(console.constructors[0].contains("Print"));

        let kv = kv_decl();
        assert_eq!(kv.type_name, "KV");
        assert!(kv.constructors.iter().any(|c| c.contains("KvGet")));

        let fs = fs_read_decl();
        assert_eq!(fs.type_name, "FsRead");
        assert!(fs.constructors.iter().any(|c| c.contains("FsRead")));

        let http = http_decl();
        assert_eq!(http.type_name, "Http");
        assert!(http.constructors.iter().any(|c| c.contains("HttpGet")));
    }

    #[test]
    fn test_effects_hash_is_deterministic_across_calls() {
        let source = "module Tidepool.Orchestrate where\n-- sentinel\n";
        let first = write_orchestrate_module(source).unwrap();
        let second = write_orchestrate_module(source).unwrap();
        assert_eq!(first, second);
        let expected_hash = content_hash_hex(&[source.as_bytes()]);
        assert_eq!(
            first.file_name().unwrap().to_str().unwrap(),
            format!("tidepool-orchestrate-{expected_hash}")
        );
        let changed =
            write_orchestrate_module("module Tidepool.Orchestrate where\n-- other\n").unwrap();
        assert_ne!(first, changed);
        std::fs::remove_dir_all(first).ok();
        std::fs::remove_dir_all(changed).ok();
    }

    #[test]
    fn test_effects_module_self_heals_after_reap() {
        let source = format!(
            "module Tidepool.Orchestrate where\n-- probe {}\n",
            std::process::id()
        );
        let directory = write_orchestrate_module(&source).unwrap();
        let module = directory.join("Tidepool/Orchestrate.hs");
        assert_eq!(std::fs::read_to_string(&module).unwrap(), source);
        std::fs::remove_dir_all(&directory).unwrap();
        let recreated = write_orchestrate_module(&source).unwrap();
        assert_eq!(directory, recreated);
        assert_eq!(std::fs::read_to_string(&module).unwrap(), source);
        std::fs::remove_dir_all(directory).ok();
    }

    #[test]
    fn test_captured_output_drain() {
        let output = CapturedOutput::new();
        output.push("line 1".to_string());
        output.push("line 2".to_string());

        let drained = output.drain();
        assert_eq!(drained, vec!["line 1", "line 2"]);

        let empty = output.drain();
        assert!(empty.is_empty());
    }
}

#[cfg(test)]
mod ergonomics_tests {
    use super::*;

    #[test]
    fn test_preamble_ergonomics() {
        let decls = standard_decls();
        let preamble = build_preamble(&decls, false);
        assert!(preamble.contains("ExtendedDefaultRules"));
        assert!(preamble.contains("default (Int, Double, Text)"));
        // renderJson + the interactive-pagination prompt moved into the generated
        // Tidepool.Orchestrate module (imported by the preamble, not spliced).
        let orch = orchestrate_module_source(&decls);
        assert!(orch.contains("renderJson :: Value -> Text"));
        assert!(orch.contains("| Reply with a stub id (e.g. stub_0) to fetch that chunk"));
    }
}
