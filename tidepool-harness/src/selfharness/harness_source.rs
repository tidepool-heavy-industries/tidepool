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
}

#[derive(Debug, thiserror::Error)]
pub enum HarnessSourceError {
    #[error("harness source not found at {path}")]
    NotFound { path: String },
    #[error("harness source path {path} has no usable file stem for a module name")]
    BadFileName { path: String },
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
    Ok(HarnessSource {
        path: path.to_path_buf(),
        source_dir,
        module_name,
    })
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
    }

    #[test]
    fn missing_file_is_an_error() {
        let path = PathBuf::from("/nonexistent/NoSuchHarness.hs");
        assert!(load_harness_source(&path).is_err());
    }
}
