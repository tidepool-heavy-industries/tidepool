//! Content-addressed Haskell facades for exact session exports.
//!
//! A fresh actor must see selected declarations without inheriting the
//! defining actor's lexical scope. The facade generated here imports an exact
//! gen-versioned [`SessionModule`] and re-exports only the selected GHC-derived
//! [`ExportItem`]s. It is a regenerable source artifact in the existing
//! session include tree, not a declaration replay, program-image registry, or
//! live-root owner.

use std::path::{Path, PathBuf};

use tidepool_repr::{SessionId, SessionModule};

use super::{ExportItem, SessionCompileView};

/// An exact declaration surface minted by [`super::SessionLib`]. Fields are
/// private so callers cannot claim that an arbitrary name is exported by a
/// module; selection is checked against the authoritative declaration log.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExactExportSurface {
    session: SessionId,
    source: Option<SessionModule>,
    items: Vec<ExportItem>,
}

impl ExactExportSurface {
    pub(crate) fn new(
        session: SessionId,
        source: Option<SessionModule>,
        mut items: Vec<ExportItem>,
    ) -> Self {
        items.sort_by_key(ExportItem::render_entry);
        items.dedup_by(|left, right| left.head_name() == right.head_name());
        Self {
            session,
            source,
            items,
        }
    }

    #[must_use]
    pub fn session(&self) -> SessionId {
        self.session
    }

    #[must_use]
    pub fn source_module(&self) -> Option<SessionModule> {
        self.source
    }

    #[must_use]
    pub fn items(&self) -> &[ExportItem] {
        &self.items
    }

    /// Materialize this surface under the exact session root named by `view`.
    /// Existing identical content is reused; writes are atomic best-effort
    /// because the file is a fully regenerable compile artifact.
    pub fn materialize(
        &self,
        view: &SessionCompileView,
    ) -> Result<MaterializedFacade, ExactFacadeError> {
        if view.session() != self.session {
            return Err(ExactFacadeError::WrongSession {
                surface: self.session,
                view: view.session(),
            });
        }
        let identity = FacadeIdentity::for_surface(self);
        let source = render_facade(&identity, self);
        let path = view.session_root().join(identity.relative_hs_path());
        if std::fs::read(&path).is_ok_and(|existing| existing == source.as_bytes()) {
            return Ok(MaterializedFacade {
                identity,
                path,
                source,
            });
        }
        let parent = path
            .parent()
            .ok_or_else(|| ExactFacadeError::InvalidPath(path.clone()))?;
        std::fs::create_dir_all(parent).map_err(|source| ExactFacadeError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
        tidepool_atomic_write::write_best_effort(&path, source.as_bytes()).map_err(|error| {
            ExactFacadeError::Io {
                path: error.path,
                source: error.source,
            }
        })?;
        Ok(MaterializedFacade {
            identity,
            path,
            source,
        })
    }
}

/// Stable identity of one generated facade. Its digest covers the originating
/// session, exact source module, and selected exports.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct FacadeIdentity {
    digest: String,
}

impl FacadeIdentity {
    fn for_surface(surface: &ExactExportSurface) -> Self {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"tidepool-exact-export-facade-v1\0");
        hasher.update(&surface.session.0.to_le_bytes());
        if let Some(module) = surface.source {
            hasher.update(module.module_name().as_bytes());
        }
        hasher.update(b"\0");
        for item in &surface.items {
            hasher.update(item.render_entry().as_bytes());
            hasher.update(b"\0");
        }
        Self {
            digest: hasher.finalize().to_hex().to_string(),
        }
    }

    #[must_use]
    pub fn digest(&self) -> &str {
        &self.digest
    }

    #[must_use]
    pub fn module_name(&self) -> String {
        format!("Tidepool.Actor.Surface.H{}", self.digest)
    }

    #[must_use]
    pub fn relative_hs_path(&self) -> PathBuf {
        PathBuf::from(format!("Tidepool/Actor/Surface/H{}.hs", self.digest))
    }
}

/// A materialized facade importable from the originating session root.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MaterializedFacade {
    identity: FacadeIdentity,
    path: PathBuf,
    source: String,
}

impl MaterializedFacade {
    #[must_use]
    pub fn identity(&self) -> &FacadeIdentity {
        &self.identity
    }

    #[must_use]
    pub fn module_name(&self) -> String {
        self.identity.module_name()
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    #[must_use]
    pub fn source(&self) -> &str {
        &self.source
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ExactFacadeError {
    #[error("exact export surface belongs to session {surface}, not compile view {view}")]
    WrongSession { surface: SessionId, view: SessionId },
    #[error("facade target has no parent directory: {}", .0.display())]
    InvalidPath(PathBuf),
    #[error("facade I/O at {}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ExactExportError {
    #[error("session has no declaration plane")]
    NoDeclarationPlane,
    #[error("scope {0:?} is not live")]
    DeadScope(tidepool_codegen::scope::ScopeId),
    #[error("`{name}` is not an exported declaration in scope {scope:?}")]
    UnknownExport {
        scope: tidepool_codegen::scope::ScopeId,
        name: String,
    },
}

fn render_facade(identity: &FacadeIdentity, surface: &ExactExportSurface) -> String {
    let module = identity.module_name();
    let entries: Vec<String> = surface.items.iter().map(ExportItem::render_entry).collect();
    let mut source = String::from("-- GENERATED — exact actor export membrane. Do not edit.\n");
    source.push_str(&format!("module {module}"));
    if entries.is_empty() {
        source.push_str(" () where\n");
        return source;
    }
    source.push_str("\n  ( ");
    source.push_str(&entries.join("\n  , "));
    source.push_str("\n  ) where\n");
    if let Some(source_module) = surface.source {
        source.push_str(&format!(
            "import {} ({})\n",
            source_module.module_name(),
            entries.join(", ")
        ));
    }
    source
}

#[cfg(test)]
mod tests {
    use super::*;
    use tidepool_codegen::scope::ScopeId;
    use tidepool_repr::Generation;

    #[test]
    fn materialized_facade_reexports_only_selected_exact_items() {
        let dir = tempfile::tempdir().expect("tempdir");
        let view = SessionCompileView::new(
            SessionId(8),
            ScopeId::ROOT,
            dir.path().to_path_buf(),
            Some(SessionModule::lib(Generation(4))),
            Vec::new(),
            Vec::new(),
            Generation(1),
        );
        let surface = ExactExportSurface::new(
            SessionId(8),
            Some(SessionModule::lib(Generation(4))),
            vec![
                ExportItem::Value {
                    name: "review".into(),
                },
                ExportItem::Type {
                    name: "Finding".into(),
                    cons: vec!["Finding".into()],
                },
            ],
        );

        let facade = surface.materialize(&view).expect("materialize facade");
        assert!(facade.source().contains("Finding(..)"));
        assert!(facade.source().contains("review"));
        assert!(facade
            .source()
            .contains("import Tidepool.Session.Lib.G4 (Finding(..), review)"));
        assert_eq!(
            std::fs::read_to_string(facade.path()).unwrap(),
            facade.source()
        );
    }
}
