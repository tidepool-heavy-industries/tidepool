//! Lane A — session declaration accumulation (plan §5.0).
//!
//! A [`SessionLib`] accumulates user declarations as **source text** across
//! turns. Each `define` turn:
//!   1. extracts the declaration's binder names **from GHC** (never a Rust-side
//!      Haskell parser) — see [`binders::extract_binders`];
//!   2. appends a [`render::DeclTurn`] to the ordered log and bumps the
//!      [`Generation`];
//!   3. regenerates the whole `Tidepool.Session.Lib.G<g>` module as a pure
//!      function of the log (selective re-export, plan §3) and writes it
//!      atomically into the session include tree.
//!
//! Later turns see prior declarations by importing `Tidepool.Session.Lib.G<g>`
//! through the **existing** batch-compile pipeline ([`crate::compile_haskell`])
//! with the session dir on the include path at highest precedence. The value
//! and type planes (Waves 1/3) are out of scope here — this lane ships standalone
//! as a usable declaration REPL.

pub mod binders;
pub mod render;
pub mod turn;

pub use turn::{
    classify_turn, compile_session_turn, BoundBinder, SessionBind, SessionTurnResult,
    TurnClassification, ValueTier,
};

use std::path::{Path, PathBuf};

use tidepool_repr::{Generation, SessionId, SessionModule};

pub use render::{DeclLog, DeclTurn, ExportItem, ModuleEnv, RenderedModule};

/// Derive the `lib/` directory that holds Tidepool stdlib source files
/// (e.g. `Tidepool.Data.Text`) by walking the `TIDEPOOL_EXTRACT` path up to the
/// `dist-newstyle` directory and returning its sibling `lib/`. Returns an empty
/// vec when the extract is not set or not inside a `dist-newstyle` tree.
fn derive_stdlib_include() -> Vec<std::path::PathBuf> {
    let extract = std::env::var("TIDEPOOL_EXTRACT").unwrap_or_default();
    if extract.is_empty() {
        return vec![];
    }
    let mut path = std::path::PathBuf::from(extract);
    loop {
        if path.file_name().and_then(|n| n.to_str()) == Some("dist-newstyle") {
            if let Some(parent) = path.parent() {
                let lib = parent.join("lib");
                if lib.is_dir() {
                    return vec![lib];
                }
            }
            break;
        }
        if !path.pop() {
            break;
        }
    }
    vec![]
}

/// Errors from the declaration-accumulation path.
#[derive(thiserror::Error, Debug)]
pub enum SessionError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    /// GHC binder extraction failed (parse error in the declaration, or the
    /// extractor was unavailable / produced unreadable output).
    #[error("binder extraction failed: {0}")]
    BinderExtraction(String),
    /// The candidate gen module failed to type-check via GHC. The declaration
    /// log has been rolled back; the session remains usable.
    #[error("declaration type-check failed: {0}")]
    ValidationFailed(String),
}

/// A resident session's declaration library. Owns the ordered decl log, the
/// monotonic generation, and the on-disk include tree.
pub struct SessionLib {
    id: SessionId,
    /// Root of the session include tree: gen modules live at
    /// `<root>/Tidepool/Session/Lib/G<g>.hs`. Placed on the GHC include path at
    /// highest precedence so they shadow any same-named module.
    root: PathBuf,
    log: DeclLog,
    env: ModuleEnv,
}

impl SessionLib {
    /// Open a session rooted at `root` (created if absent). `env` controls the
    /// generated modules' pragma/import surface; pass
    /// [`ModuleEnv::standalone_default`] for the pure Lane-A surface.
    pub fn open(id: SessionId, root: impl Into<PathBuf>, env: ModuleEnv) -> Result<SessionLib, SessionError> {
        let root = root.into();
        std::fs::create_dir_all(&root)?;
        Ok(SessionLib {
            id,
            root,
            log: DeclLog::new(),
            env,
        })
    }

    /// The include directory to place on the GHC search path (highest precedence).
    #[must_use]
    pub fn include_dir(&self) -> &Path {
        &self.root
    }

    /// The current generation (`Generation(0)` until the first `define`).
    #[must_use]
    pub fn generation(&self) -> Generation {
        self.log.generation()
    }

    /// The current session-library module, or `None` before any declaration.
    #[must_use]
    pub fn current_module(&self) -> Option<SessionModule> {
        let g = self.log.generation();
        (g.0 > 0).then(|| SessionModule::lib(g))
    }

    /// The `import Tidepool.Session.Lib.G<g>` line a turn should prepend to see
    /// the accumulated declarations, or `None` if the session is empty.
    #[must_use]
    pub fn import_line(&self) -> Option<String> {
        self.current_module().map(|m| format!("import {}", m.module_name()))
    }

    /// Names of every value/function binder introduced across all declaration
    /// turns (all generations, not just the current one). Used by the eval
    /// assembler to hide session-defined names from the Prelude import so a
    /// user function named `over`/`view`/etc. resolves unambiguously to the
    /// session decl rather than the Prelude re-export (BUG-7).
    #[must_use]
    pub fn decl_value_names(&self) -> Vec<&str> {
        self.log
            .turns
            .iter()
            .flat_map(|t| t.items.iter())
            .filter_map(|item| {
                if let ExportItem::Value { name } = item {
                    Some(name.as_str())
                } else {
                    None
                }
            })
            .collect()
    }

    /// A cache salt unique to `(session, generation)`. Threaded into
    /// [`crate::compile_haskell_salted`] so two sessions' identical-text modules
    /// don't collide and a generation bump invalidates correctly (plan §3 R6).
    #[must_use]
    pub fn cache_salt(&self) -> String {
        format!("session:{}:gen:{}", self.id, self.log.generation())
    }

    /// Append a declaration turn. Extracts binder names from GHC, regenerates the
    /// gen-versioned module, writes it atomically, validates it type-checks via GHC
    /// (for turns that introduce value binders), and returns the new generation.
    ///
    /// `decl_text` may contain several top-level declarations; their binders are
    /// classified together as this turn's introduced names.
    ///
    /// Empty / whitespace-only `decl_text` is a **no-op**: returns the current
    /// generation without bumping it (RE-1 fix).
    ///
    /// Syntactically-invalid declarations are rejected here (GHC's parser fails →
    /// `SessionError::BinderExtraction`) and the log is left untouched.
    ///
    /// Declarations that parse but fail to type-check (e.g. a body referencing a
    /// not-yet-defined name) are also rejected: when the turn introduces value
    /// binders, the candidate gen module is compiled via a thin wrapper; on failure
    /// the log is rolled back and `SessionError::ValidationFailed` is returned
    /// (BUG-A fix). Turns that introduce only type/class/instance binders (no
    /// `ExportItem::Value` entries) skip this validation step so the existing
    /// class-method-export behaviour (GHC-54721) is left to surface at eval time
    /// rather than at define time.
    pub fn define(&mut self, decl_text: &str) -> Result<Generation, SessionError> {
        // RE-1: empty / whitespace declaration is a no-op — don't bump the gen.
        if decl_text.trim().is_empty() {
            return Ok(self.log.generation());
        }

        let items = binders::extract_binders(decl_text, &[self.root.as_path()])?;

        // Only validate turns that introduce value binders (functions / values).
        // Type / class / instance turns are skipped — see doc above.
        let has_value_binder = items.iter().any(|i| matches!(i, ExportItem::Value { .. }));

        self.log.push(DeclTurn {
            sources: vec![decl_text.to_string()],
            items,
        });
        let gen = self.log.generation();
        let rendered = render::render_module(&self.log, gen, &self.env);
        self.write_module(&rendered)?;

        // BUG-A fix: validate the candidate module type-checks via GHC.
        // On failure, roll back the log to the prior generation so the session
        // remains usable for subsequent turns.
        if has_value_binder {
            if let Err(e) = self.validate_candidate(&rendered) {
                self.log.turns.pop();
                return Err(e);
            }
        }

        Ok(gen)
    }

    /// Validate that the candidate gen module compiles and type-checks by running
    /// the extract in full-compile mode on a thin wrapper that imports it. The
    /// candidate is already written on disk at this point; this just drives GHC on
    /// it and surfaces any scope / type errors as a clean `SessionError`.
    fn validate_candidate(&self, rendered: &RenderedModule) -> Result<(), SessionError> {
        let temp = tempfile::TempDir::new()?;

        // Thin wrapper: importing the candidate forces GHC to compile it and
        // report any scope/type errors. `result = ()` is a trivial target.
        let module_name = rendered.module.module_name();
        let wrapper_src = format!(
            "module TidepoolValidate where\nimport {module_name} ()\nresult :: ()\nresult = ()\n"
        );
        let wrapper_path = temp.path().join("TidepoolValidate.hs");
        std::fs::write(&wrapper_path, &wrapper_src)?;

        let extract_bin =
            std::env::var("TIDEPOOL_EXTRACT").unwrap_or_else(|_| "tidepool-extract".to_string());

        let mut cmd = std::process::Command::new(&extract_bin);
        cmd.arg(&wrapper_path)
            .arg("--output-dir")
            .arg(temp.path())
            .arg("--target")
            .arg("result")
            .arg("--include")
            .arg(&self.root);

        // The candidate module imports stdlib sources (e.g. Tidepool.Data.Text)
        // that live next to `dist-newstyle` in the project tree. Auto-discover
        // that sibling `lib/` from TIDEPOOL_EXTRACT's path so validation finds
        // them without any extra configuration.
        for dir in derive_stdlib_include() {
            cmd.arg("--include").arg(dir);
        }

        let output = cmd.output().map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                SessionError::BinderExtraction(
                    "tidepool-extract not found on PATH (set TIDEPOOL_EXTRACT)".to_string(),
                )
            } else {
                SessionError::Io(e)
            }
        })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
            return Err(SessionError::ValidationFailed(stderr));
        }

        Ok(())
    }

    /// Atomically write a rendered module to its place in the include tree.
    fn write_module(&self, rendered: &RenderedModule) -> Result<(), SessionError> {
        let rel = rendered.module.relative_hs_path();
        let path = self.root.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // Atomic replace: write a sibling temp then rename.
        let dir = path.parent().unwrap_or(&self.root);
        let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
        use std::io::Write;
        tmp.write_all(rendered.source.as_bytes())?;
        tmp.persist(&path)
            .map_err(|e| SessionError::Io(e.error))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_session_has_no_module() {
        let dir = tempfile::tempdir().unwrap();
        let lib = SessionLib::open(SessionId(1), dir.path(), ModuleEnv::standalone_default()).unwrap();
        assert_eq!(lib.generation(), Generation(0));
        assert!(lib.current_module().is_none());
        assert!(lib.import_line().is_none());
    }

    #[test]
    fn cache_salt_changes_with_generation_and_session() {
        let dir = tempfile::tempdir().unwrap();
        let lib1 = SessionLib::open(SessionId(1), dir.path(), ModuleEnv::standalone_default()).unwrap();
        let lib2 = SessionLib::open(SessionId(2), dir.path(), ModuleEnv::standalone_default()).unwrap();
        assert_ne!(lib1.cache_salt(), lib2.cache_salt());
    }
}
