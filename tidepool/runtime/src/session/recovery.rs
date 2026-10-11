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

/// Authenticated immutable storage facts. These observations grant no compiler,
/// source-instance, publication, or native execution authority.
pub struct DeclarationDomainObservation {
    pub domain: super::RecoveryDomainId,
    pub source_session: u64,
    pub high_water: tidepool_repr::Generation,
    pub declarations: Vec<DeclarationObservation>,
    pub artifacts: Vec<std::sync::Arc<RecoveryArtifactClosure>>,
    graph: RecoveryGraph,
}

impl std::fmt::Debug for DeclarationDomainObservation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeclarationDomainObservation")
            .field("domain", &self.domain)
            .field("source_session", &self.source_session)
            .field("high_water", &self.high_water)
            .field("declarations", &self.declarations)
            .field("artifact_count", &self.artifacts.len())
            .finish()
    }
}

impl DeclarationDomainObservation {
    /// Demand only this declaration's exact selected dependency facts.
    pub fn declaration_dependencies(
        &self,
        generation: tidepool_repr::Generation,
    ) -> Result<
        Vec<(
            tidepool_toolchain::artifact_inventory::ArtifactBindingNode,
            tidepool_toolchain::artifact_inventory::ArtifactBindingNode,
            tidepool_toolchain::artifact_inventory::ArtifactDependency,
        )>,
        SessionError,
    > {
        let path = self.graph.store().path();
        let node = self
            .graph
            .node(generation)
            .map_err(|error| graph_error(path, error))?
            .ok_or_else(|| SessionError::RecoveryManifest {
                path: path.to_path_buf(),
                detail: "observed declaration is absent from this exact domain snapshot".into(),
            })?;
        self.graph
            .context(&node.context_root)
            .map_err(|error| graph_error(path, error))?
            .snapshot()
            .binding_dependencies()
            .map_err(|error| SessionError::RecoverySnapshot(Box::new(error)))
    }
}

#[derive(Debug)]
pub struct DeclarationObservation {
    pub generation: tidepool_repr::Generation,
    pub parent: Option<tidepool_repr::Generation>,
    pub kind: RecoveryNodeKind,
    pub exports: Vec<RecoveryExport>,
    pub lexical_roots: Vec<tidepool_toolchain::declaration_join::ExactModuleIdentity>,
    pub instances: RecoveryInstanceInventory,
    pub context_root: tidepool_toolchain::exact_context::ContextRoot,
}

/// Read a domain through the production block and context acquisition owners.
/// A missing database is an error; an authenticated absent domain is `None`.
/// Concurrent publication refuses the observation instead of mixing revisions.
pub fn observe_declaration_domain(
    path: &Path,
    domain: super::RecoveryDomainId,
) -> Result<Option<DeclarationDomainObservation>, SessionError> {
    let store = std::sync::Arc::new(
        super::RunRecoveryStore::open_read_only(path)
            .map_err(|error| SessionError::RecoveryStore(Box::new(error)))?,
    );
    let state = store
        .read_domain(domain)
        .map_err(|error| SessionError::RecoveryStore(Box::new(error)))?;
    let Some(state) = state else { return Ok(None) };
    let graph = RecoveryGraph::hydrate_domain(store.clone(), domain, &state)
        .map_err(|error| graph_error(path, error))?;
    let mut declarations = Vec::new();
    for generation in graph.node_ids() {
        let node = graph
            .node(generation)
            .map_err(|error| graph_error(path, error))?
            .expect("the immutable declaration map contains its iterated key");
        declarations.push(DeclarationObservation {
            generation: node.id,
            parent: node.parent,
            kind: node.kind,
            exports: node.exports.clone(),
            lexical_roots: node.lexical_roots.clone(),
            instances: node.instances.clone(),
            context_root: node.context_root.clone(),
        });
    }
    let artifacts = graph
        .artifact_ids()
        .map(|id| {
            graph
                .artifact(id)
                .map_err(|error| graph_error(path, error))
                .map(|artifact| {
                    artifact.expect("the immutable artifact map contains its iterated key")
                })
        })
        .collect::<Result<Vec<_>, _>>()?;
    if store
        .read_domain(domain)
        .map_err(|error| SessionError::RecoveryStore(Box::new(error)))?
        != Some(state)
    {
        return Err(SessionError::StaleStagedDeclaration);
    }
    Ok(Some(DeclarationDomainObservation {
        domain,
        source_session: graph.source_session(),
        high_water: graph.high_water(),
        declarations,
        artifacts,
        graph,
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
