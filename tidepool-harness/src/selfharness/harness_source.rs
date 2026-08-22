//! Bootstrap-load for the authored harness source — `render`,
//! `loop`, and the concrete `State` declaration (`examples/harness/Harness.hs`
//! is the reference contract this loads). Loaded ONCE at driver bootstrap,
//! NEVER per-turn: `render`/`loop` are runtime-invoked only at loop
//! boundaries, never mid-loop by the harness author, so a per-turn reload
//! (which would blow the compile cache prefix) is never needed.
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
//! author-defined type: a value built under one home module case-traps on
//! `resume` when the other side expects its own. Importing the SAME
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
    /// Whether the harness declares the OPT-IN resume entry point
    /// `resumeLoop :: ResumeFold -> State -> Harness State` (PRD 20 S1-L5).
    ///
    /// The universal entry (`loop :: State -> Harness State`) is unchanged and
    /// every harness keeps it; this flag only tells the driver whether the
    /// WIDER entry exists to inject a boot-time journal fold through. A
    /// non-empty fold against a harness where this is `false` is refused at
    /// boot (`DriverError::ResumeEntryMissing`) rather than quietly redoing
    /// finished work — that refusal is the "resume cannot forget to look"
    /// property, and this flag is what makes it decidable before a cycle runs.
    ///
    /// Derived by a structural source scan ([`declares_resume_entry`]): the
    /// scan SELECTS the entry, it does not validate it. GHC remains the real
    /// check — a `resumeLoop` of the wrong type fails its compile the
    /// ordinary way, and nothing here parses a GHC error.
    pub declares_resume_entry: bool,
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
/// exists.
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
    let declares_resume_entry = declares_resume_entry(path);
    let fingerprint = fingerprint_source(path)?;
    Ok(HarnessSource {
        path: path.to_path_buf(),
        source_dir,
        module_name,
        declares_resume_entry,
        fingerprint,
    })
}

/// Whether `path` declares a TOP-LEVEL `resumeLoop` binding — a line beginning
/// at column 0 with `resumeLoop` followed by a non-identifier character (its
/// type signature or its first equation; either is enough, and a nested
/// `where`-bound one is indented and so correctly not seen).
///
/// Structural, subject to the same rule this module's doc sets out: nothing
/// here is typechecked, resolved, or classified. The scan only SELECTS which
/// entry the driver compiles. GHC is the real check — a `resumeLoop` with
/// the wrong type, or one this scan mis-reads, fails its compile as an
/// ordinary GHC error, and no driver code parses that error.
fn declares_resume_entry(path: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(path) else {
        return false;
    };
    text.lines().any(|line| {
        line.strip_prefix("resumeLoop").is_some_and(|rest| {
            !rest.starts_with(|c: char| c.is_alphanumeric() || c == '_' || c == '\'')
        })
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

    /// The scan SELECTS the entry: a harness declaring `resumeLoop` opts into
    /// the boot fold, one that doesn't stays on the universal `loop` (and a
    /// non-empty fold against it is refused at boot rather than silently
    /// redone).
    #[test]
    fn resume_entry_is_detected_only_where_it_is_declared() {
        let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        let with_resume =
            load_harness_source(&fixtures.join("ResumeHarness.hs")).expect("fixture resolves");
        assert!(
            with_resume.declares_resume_entry,
            "ResumeHarness.hs declares a top-level resumeLoop"
        );
        for name in ["OuterEffectsHarness.hs", "TwoHoleHarness.hs"] {
            let source = load_harness_source(&fixtures.join(name)).expect("fixture resolves");
            assert!(
                !source.declares_resume_entry,
                "{name} declares no resumeLoop and must not be offered as a resume entry"
            );
        }
    }

    /// Only a TOP-LEVEL binding counts: an indented (`where`-bound) definition
    /// is not the entry the driver can call, and a longer name that merely
    /// starts with `resumeLoop` is a different binding entirely.
    #[test]
    fn resume_entry_scan_requires_column_zero_and_a_whole_name() {
        let dir =
            std::env::temp_dir().join(format!("harness-source-resume-scan-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");

        let cases = [
            ("resumeLoop :: ResumeFold -> State -> Harness State\n", true),
            ("resumeLoop fold st = loop st\n", true),
            ("  resumeLoop fold st = loop st\n", false),
            ("resumeLoopHelper :: Int\n", false),
            ("resumeLoop' :: Int\n", false),
            ("loop :: State -> Harness State\n", false),
        ];
        for (i, (body, expected)) in cases.iter().enumerate() {
            let path = dir.join(format!("Case{i}.hs"));
            std::fs::write(&path, format!("module Case{i} where\n{body}")).expect("write case");
            assert_eq!(
                declares_resume_entry(&path),
                *expected,
                "case {i} ({body:?})"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_file_is_an_error() {
        let path = PathBuf::from("/nonexistent/NoSuchHarness.hs");
        assert!(load_harness_source(&path).is_err());
    }
}
