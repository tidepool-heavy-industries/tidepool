//! WS-D seam: bootstrap-load for the authored harness source — `render`,
//! `loop`, and the concrete `State` declaration (`examples/harness/Harness.hs`
//! is the reference contract this loads). Loaded ONCE at driver bootstrap,
//! NEVER per-turn (02-runtime.md: "Runtime-invoked at loop boundaries
//! only... The harness author cannot call `render` mid-loop" — the
//! anti-pattern this seam exists to make unrepresentable is a per-turn
//! reload that would blow the cache prefix).
//!
//! # A plain importable module, not decl-plane content
//!
//! [`HarnessSource`] does NOT splice the file's text anywhere, and this
//! crate does not parse or classify a single Haskell declaration in it —
//! [`load_harness_source`] only derives a module name and an include-path
//! root from the FILE PATH (GHC's own filename convention: `import Harness`
//! resolves `Harness.hs` on the search path), then verifies the file
//! exists. `tidepool-extract`/GHC does 100% of the real parsing +
//! typechecking, the same way it would for `import Harness (...)` written
//! by a model in any ordinary turn.
//!
//! This is deliberately NOT the session decl plane
//! (`tidepool_runtime::session::SessionLib`/`ResidentSession::
//! define_scoped`) — that mechanism is for genuinely dynamic,
//! session-accumulated declarations. A harness file is static and on disk;
//! treating it as a decl-plane splice would give its types a
//! generation-versioned "home module" (`Tidepool.Session.Lib.G<n>`)
//! DIFFERENT from what a separately-compiled nested Agent turn resolves via
//! `import Harness (...)` from the same file — and since a `Value`'s
//! constructor id is a stable hash of (defining module, name, arity), those
//! two compiles would then produce INCOMPATIBLE values for the "same"
//! author-defined type (confirmed empirically: a decl-plane-spliced attempt
//! case-trapped on `resume` for exactly this reason). Importing the SAME
//! static module from both sides keeps every compile resolving the same
//! defining module, so constructor ids agree. `driver::SelfHarnessDriver`
//! imports it QUALIFIED (see `state_cross`'s module doc) purely to dodge
//! unqualified-name collisions with the ambient `Tidepool.Prelude` — that's
//! unrelated to this module-identity concern.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

/// The authored harness's location: everything the driver needs to `import`
/// it (qualified) into a compile — no source text, no declarations parsed
/// or split out (see this module's doc for why).
#[derive(Debug, Clone)]
pub struct HarnessSource {
    /// The harness source file's path.
    pub path: PathBuf,
    /// The file's containing directory — an include-path root so
    /// `import <module_name>` resolves. The driver adds this to the outer
    /// session's compile `include`; a caller wiring a nested Agent to
    /// answer this harness's `runLLMTurn` holes adds it as that Agent's
    /// `project_lib` for the same reason (so BOTH compiles resolve author-
    /// defined types, e.g. a harness's own `Decision`, from the identical
    /// module — see this file's module doc for why that identity match is
    /// load-bearing).
    pub source_dir: PathBuf,
    /// The module name GHC will look for `path` under (its filename minus
    /// `.hs`, per GHC's own convention — not inspected from the file's
    /// content).
    pub module_name: String,
    /// The author modules an ANSWERER turn imports, so the hole's answer type
    /// is in scope when it builds one — derived structurally as the SIBLING
    /// modules `path` itself imports (see [`answerer_imports`]).
    ///
    /// Never [`Self::module_name`] itself: the harness module defines `loop`,
    /// whose `runLLMTurn` does not exist on the answerer's `[AskUser, Fork,
    /// Finalize]` row, and GHC compiles an imported module whole — importing it
    /// would fail the answerer's turn outright. The harness/agent split exists
    /// precisely so the author's TYPES carry no `runLLMTurn` dependency and can
    /// be imported from both sides.
    ///
    /// Empty when the harness imports no local sibling — correct for a harness
    /// whose holes are all Prelude-typed (`Text`, `Int`), and the loud-failure
    /// case for one that inlines author types alongside `loop`: the answerer
    /// cannot import them, so the pinned `finalize` reports the type as out of
    /// scope and the driver tells the author to split them out
    /// (`SelfHarnessDriver::types_in_scope_hint`).
    pub answerer_imports: Vec<String>,
    /// A content fingerprint of the loaded source text — not a manifest, just
    /// a hash, so a checkpoint restored against a since-edited harness file
    /// can be told apart from one restored against the same file (a harness
    /// file is expected to change across a self-iteration run; this is what
    /// lets a restart detect that rather than silently assume nothing moved).
    pub fingerprint: String,
}

#[derive(Debug, thiserror::Error)]
pub enum HarnessSourceError {
    #[error("harness source not found at {path}")]
    NotFound { path: String },
    #[error("harness source path {path} has no usable file stem for a module name")]
    BadFileName { path: String },
    #[error("harness source {path} could not be read for fingerprinting: {source}")]
    Io {
        path: String,
        source: std::io::Error,
    },
}

/// Resolve a harness source file (e.g. `examples/harness/Harness.hs`) at
/// bootstrap into its module name + include-path root, verifying it
/// exists. WS-D.
pub fn load_harness_source(path: &Path) -> Result<HarnessSource, HarnessSourceError> {
    if !path.is_file() {
        return Err(HarnessSourceError::NotFound {
            path: path.display().to_string(),
        });
    }
    let source_dir = path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let module_name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .map(str::to_string)
        .ok_or_else(|| HarnessSourceError::BadFileName {
            path: path.display().to_string(),
        })?;
    let answerer_imports = answerer_imports(path, &source_dir);
    let fingerprint = fingerprint_source(path)?;
    Ok(HarnessSource {
        path: path.to_path_buf(),
        source_dir,
        module_name,
        answerer_imports,
        fingerprint,
    })
}

/// A hash of `path`'s source text, formatted as hex — cheap and sufficient
/// for change detection (see [`HarnessSource::fingerprint`]'s doc); not a
/// cryptographic digest.
fn fingerprint_source(path: &Path) -> Result<String, HarnessSourceError> {
    let text = std::fs::read_to_string(path).map_err(|source| HarnessSourceError::Io {
        path: path.display().to_string(),
        source,
    })?;
    let mut hasher = DefaultHasher::new();
    text.hash(&mut hasher);
    Ok(format!("{:016x}", hasher.finish()))
}

/// The sibling modules `path` imports — the author's own vocabulary modules,
/// which an answerer turn must import to name the hole's answer type.
///
/// Derived from the harness file's own IMPORT LIST, intersected with the `.hs`
/// files sitting beside it. Structural, not a naming convention: the edge is
/// the one the author already declared by importing their types, so it holds
/// for any module layout (`HarnessTypes`, `Domain` + `Vocab`, whatever) with no
/// rule for the author to learn. Two properties come free — a module the
/// harness does NOT import is not pulled in (so several unrelated harnesses can
/// share a directory, as the test fixtures do), and `path` itself is never in
/// the list (a module does not import itself).
///
/// Scanning import lines is not the declaration parsing this module's doc rules
/// out: nothing here is typechecked, resolved, or classified, and a mis-read
/// line can only add or drop a candidate import that GHC then judges normally.
fn answerer_imports(path: &Path, source_dir: &Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let mut found: Vec<String> = Vec::new();
    for line in text.lines() {
        let Some(rest) = line.strip_prefix("import ") else {
            continue;
        };
        let module = rest
            .trim_start()
            .strip_prefix("qualified ")
            .unwrap_or(rest)
            .split(|c: char| !(c.is_alphanumeric() || c == '.' || c == '_' || c == '\''))
            .find(|t| !t.is_empty())
            .unwrap_or_default();
        let is_sibling = !module.is_empty()
            && source_dir
                .join(format!("{}.hs", module.replace('.', "/")))
                .is_file();
        if is_sibling && !found.iter().any(|m| m == module) {
            found.push(module.to_string());
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_the_reference_harness_module() {
        let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let path = manifest
            .parent()
            .unwrap()
            .join("examples/harness/Harness.hs");
        let source = load_harness_source(&path).expect("reference harness resolves");
        assert_eq!(source.path, path);
        assert_eq!(source.module_name, "Harness");
        assert_eq!(source.source_dir, path.parent().unwrap());
        assert_eq!(source.answerer_imports, vec!["HarnessTypes".to_string()]);
        assert!(!source.fingerprint.is_empty());
    }

    #[test]
    fn fingerprint_is_stable_and_distinguishes_different_sources() {
        let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        let a = load_harness_source(&fixtures.join("TwoHoleHarness.hs")).expect("fixture resolves");
        let b = load_harness_source(&fixtures.join("TwoHoleHarness.hs")).expect("fixture resolves");
        let c =
            load_harness_source(&fixtures.join("CompactionHarness.hs")).expect("fixture resolves");
        assert_eq!(
            a.fingerprint, b.fingerprint,
            "same file must fingerprint identically"
        );
        assert_ne!(
            a.fingerprint, c.fingerprint,
            "different source text must fingerprint differently"
        );
    }

    /// The sibling set is what the harness IMPORTS, not what shares its
    /// directory: the two fixture harnesses sit side by side, and neither may
    /// drag the other into an answerer compile (each defines a `loop` whose
    /// `runLLMTurn` the answerer's row lacks). Both import only stdlib modules,
    /// so both resolve to no author imports.
    #[test]
    fn a_harness_does_not_import_an_unrelated_neighbour() {
        let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        for name in ["TwoHoleHarness.hs", "CompactionHarness.hs"] {
            let source = load_harness_source(&fixtures.join(name)).expect("fixture resolves");
            assert!(
                source.answerer_imports.is_empty(),
                "{name} must not offer a neighbouring harness as an answerer import, got {:?}",
                source.answerer_imports
            );
        }
    }

    #[test]
    fn missing_file_is_an_error() {
        let path = PathBuf::from("/nonexistent/NoSuchHarness.hs");
        assert!(load_harness_source(&path).is_err());
    }
}
