//! Exact artifact-backed declaration recovery and its status report.

use super::SessionError;
use std::path::Path;

#[path = "newrecovery_v2.rs"]
mod newrecovery_v2;
pub(crate) use newrecovery_v2::*;
pub use newrecovery_v2::{
    RecoveryArtifactClosure, RecoveryExport, RecoveryExportKind, RecoveryInstanceEvidence,
    RecoveryInstanceInventory, RecoveryNodeKind, RecoveryPublicOwner,
    RecoveryRefusal as RecoveryFormatRefusal, RecoverySymbolIdentity,
};

/// Authenticated domain header and exact immutable fact access. Observation
/// grants no compiler, source-instance, publication, or execution authority.
/// Only explicitly selected leaves are checked; this is not recovery validation.
#[derive(Debug)]
pub struct DeclarationDomainObservation {
    pub domain: super::RecoveryDomainId,
    pub source_session: u64,
    pub high_water: tidepool_repr::Generation,
    reader: RecoveryDomainReader,
}

impl DeclarationDomainObservation {
    pub fn declaration(
        &self,
        generation: tidepool_repr::Generation,
    ) -> Result<Option<DeclarationObservation>, SessionError> {
        self.reader
            .node(generation)
            .map_err(|error| graph_error(self.reader.store.path(), error))
            .map(|node| {
                node.map(|node| DeclarationObservation {
                    generation: node.id,
                    parent: node.parent,
                    kind: node.kind,
                    implementation_refs: node.implementation_refs,
                    artifact_refs: node.artifact_refs,
                    exports: node.exports,
                    lexical_roots: node.lexical_roots,
                    instances: node.instances,
                    context_root: node.context_root,
                })
            })
    }

    pub fn artifact(
        &self,
        id: tidepool_toolchain::artifact_inventory::ArtifactId,
    ) -> Result<Option<RecoveryArtifactClosure>, SessionError> {
        self.reader
            .artifact(id)
            .map_err(|error| graph_error(self.reader.store.path(), error))
    }

    /// Hydrate only this declaration's exact selected compiler facts.
    pub fn declaration_context(
        &self,
        generation: tidepool_repr::Generation,
    ) -> Result<tidepool_toolchain::exact_context::MaterializedContext, SessionError> {
        let path = self.reader.store.path();
        let node = self
            .reader
            .node(generation)
            .map_err(|error| graph_error(path, error))?
            .ok_or_else(|| SessionError::RecoveryManifest {
                path: path.to_path_buf(),
                detail: "observed declaration is absent from this exact domain snapshot".into(),
            })?;
        tidepool_toolchain::exact_context::hydrate_context(
            self.reader.store.as_ref(),
            &node.context_root,
        )
        .map_err(|error| SessionError::RecoverySnapshot(Box::new(error)))
    }
}

#[derive(Debug)]
pub struct DeclarationObservation {
    pub generation: tidepool_repr::Generation,
    pub parent: Option<tidepool_repr::Generation>,
    pub kind: RecoveryNodeKind,
    pub implementation_refs: Vec<tidepool_repr::Generation>,
    pub artifact_refs: Vec<tidepool_toolchain::artifact_inventory::ArtifactId>,
    pub exports: Vec<RecoveryExport>,
    pub lexical_roots: Vec<tidepool_toolchain::declaration_join::ExactModuleIdentity>,
    pub instances: RecoveryInstanceInventory,
    pub context_root: tidepool_toolchain::exact_context::ContextRoot,
}

/// Read scalar status through the production store owner and retain the exact
/// immutable domain root for subsequent selected reads. A missing database is
/// an error; an authenticated absent domain is `None`. Concurrent publication
/// during header acquisition refuses instead of mixing revisions.
pub fn observe_declaration_domain(
    path: &Path,
    domain: super::RecoveryDomainId,
) -> Result<Option<DeclarationDomainObservation>, SessionError> {
    let store = std::sync::Arc::new(
        super::RunRecoveryStore::open_read_only(path)
            .map_err(|error| SessionError::RecoveryStore(Box::new(error)))?,
    );
    let Some(state) = store
        .read_domain(domain)
        .map_err(|error| SessionError::RecoveryStore(Box::new(error)))?
    else {
        return Ok(None);
    };
    let reader = RecoveryDomainReader::open(store.clone(), &state)
        .map_err(|error| graph_error(path, error))?;
    if store
        .read_domain(domain)
        .map_err(|error| SessionError::RecoveryStore(Box::new(error)))?
        != Some(state.clone())
    {
        return Err(SessionError::StaleStagedDeclaration);
    }
    Ok(Some(DeclarationDomainObservation {
        domain,
        source_session: reader.source_session(),
        high_water: state.allocation_ceiling,
        reader,
    }))
}

pub(crate) fn graph_error(path: &Path, error: RecoveryError) -> SessionError {
    match error.kind {
        RecoveryErrorKind::Snapshot(error) => SessionError::RecoverySnapshot(Box::new(error)),
        RecoveryErrorKind::Store(error) => SessionError::RecoveryStore(Box::new(error)),
        RecoveryErrorKind::Manifest => SessionError::RecoveryManifest {
            path: path.to_path_buf(),
            detail: error.detail,
        },
    }
}

/// Declaration evidence restored under its original module identity.
/// Native execution still requires each demanded original binding lease.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RestoredDeclaration {
    pub generation: u64,
    pub module: String,
}

/// A durable binding winner whose native value belonged to the prior machine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnavailableBinding {
    pub name: String,
    pub session: u64,
    pub variable: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeclarationRecoveryReport {
    pub source_session: u64,
    pub successor_session: u64,
    pub restored: Vec<RestoredDeclaration>,
    pub unavailable_bindings: Vec<UnavailableBinding>,
    pub durability_unconfirmed: bool,
}

#[cfg(test)]
mod observation_tests {
    use super::*;

    #[test]
    fn readonly_observation_distinguishes_absence_from_failed_storage() {
        let root = tempfile::tempdir().unwrap();
        let missing = root.path().join("missing.sqlite");
        assert!(
            matches!(observe_declaration_domain(&missing, super::super::RecoveryDomainId::Root),
            Err(SessionError::RecoveryStore(error))
                if error.kind == super::super::StoreErrorKind::MissingDatabase)
        );
        assert!(!missing.exists(), "observation must not create a database");

        let path = root.path().join("recovery.sqlite");
        drop(super::super::RunRecoveryStore::create(&path).unwrap());
        let before = std::fs::read(&path).unwrap();
        assert!(
            observe_declaration_domain(&path, super::super::RecoveryDomainId::Root)
                .unwrap()
                .is_none()
        );
        assert!(observe_declaration_domain(
            &path,
            super::super::RecoveryDomainId::Session(tidepool_repr::SessionId(42))
        )
        .unwrap()
        .is_none());
        assert_eq!(std::fs::read(&path).unwrap(), before);

        let inaccessible = root.path().join("not-a-database");
        std::fs::create_dir(&inaccessible).unwrap();
        assert!(
            matches!(observe_declaration_domain(&inaccessible, super::super::RecoveryDomainId::Root),
            Err(SessionError::RecoveryStore(error))
                if error.kind == super::super::StoreErrorKind::Integrity)
        );
        let corrupt = root.path().join("corrupt.sqlite");
        std::fs::write(&corrupt, b"invalid sqlite storage").unwrap();
        assert!(
            observe_declaration_domain(&corrupt, super::super::RecoveryDomainId::Root).is_err()
        );
        assert_eq!(std::fs::read(&corrupt).unwrap(), b"invalid sqlite storage");
    }
}
