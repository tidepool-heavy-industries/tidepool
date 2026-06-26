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

use std::path::{Path, PathBuf};

use tidepool_repr::{Generation, SessionId, SessionModule};

pub use render::{DeclLog, DeclTurn, ExportItem, ModuleEnv, RenderedModule};

/// Errors from the declaration-accumulation path.
#[derive(thiserror::Error, Debug)]
pub enum SessionError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    /// GHC binder extraction failed (parse error in the declaration, or the
    /// extractor was unavailable / produced unreadable output).
    #[error("binder extraction failed: {0}")]
    BinderExtraction(String),
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

    /// A cache salt unique to `(session, generation)`. Threaded into
    /// [`crate::compile_haskell_salted`] so two sessions' identical-text modules
    /// don't collide and a generation bump invalidates correctly (plan §3 R6).
    #[must_use]
    pub fn cache_salt(&self) -> String {
        format!("session:{}:gen:{}", self.id, self.log.generation())
    }

    /// Append a declaration turn. Extracts binder names from GHC, regenerates the
    /// gen-versioned module, writes it atomically, and returns the new generation.
    ///
    /// `decl_text` may contain several top-level declarations; their binders are
    /// classified together as this turn's introduced names.
    pub fn define(&mut self, decl_text: &str) -> Result<Generation, SessionError> {
        let items = binders::extract_binders(decl_text, &[self.root.as_path()])?;
        self.log.push(DeclTurn {
            sources: vec![decl_text.to_string()],
            items,
        });
        let gen = self.log.generation();
        let rendered = render::render_module(&self.log, gen, &self.env);
        self.write_module(&rendered)?;
        Ok(gen)
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
