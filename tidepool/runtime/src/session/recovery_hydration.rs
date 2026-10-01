//! Restore public declaration authority through the existing log and scope owners.

use super::*;
use std::collections::BTreeSet;
use std::sync::Arc;
use tidepool_toolchain::declaration_join::{
    certify_recovered_declaration_tip_with_inventory, ClassInstanceEvidence, DeclarationExport,
    DeclarationKind, ExportIdentity, ExportNamespace, InstanceInventory,
    RecoveryDeclarationSelection,
};

/// Configured native composition backed by the lifetime run lock. This trait
/// is never a model-facing capability or a serialized recovery descriptor.
pub trait RecoveryRunAuthority: Send + Sync {
    fn owns_run(&self, run_root: &Path) -> std::io::Result<bool>;
}

/// Configured composition of the run lease and an opaque, after-admission
/// actor-journal receipt validated against the live actor placement.
pub trait RecoverySuccessorAuthority: Send + Sync {
    fn validate_successor(
        &self,
        run_root: &Path,
        predecessor: &RecoveryPublicOwner,
        successor: &RecoveryPublicOwner,
        session: SessionId,
        target: ScopeId,
    ) -> std::io::Result<bool>;
}

/// Issued only by the owning manifest reader, from the actual canonical file.
/// Keeping this share keeps the configured run owner alive through hydration
/// and publication staging; callers cannot supply a graph or lexical selector.
pub(super) struct OwnedRecoveryManifest {
    authority: Arc<dyn RecoveryRunAuthority>,
    root: PathBuf,
    path: PathBuf,
    bytes_digest: Option<blake3::Hash>,
}

pub(super) fn same_manifest_owner(
    left: &Option<Arc<OwnedRecoveryManifest>>,
    right: &Option<Arc<OwnedRecoveryManifest>>,
) -> bool {
    match (left, right) {
        (None, None) => true,
        (Some(left), Some(right)) => Arc::ptr_eq(left, right),
        _ => false,
    }
}

impl OwnedRecoveryManifest {
    pub(super) fn validate_owner(&self) -> Result<(), SessionError> {
        let valid = self.authority.owns_run(&self.root).map_err(|error| {
            SessionError::RecoveryManifest {
                path: self.path.clone(),
                detail: error.to_string(),
            }
        })?;
        let escaped =
            self.path.exists() && self.path.canonicalize().ok().as_ref() != Some(&self.path);
        if !valid
            || escaped
            || self
                .path
                .parent()
                .and_then(|root| root.canonicalize().ok())
                .as_ref()
                != Some(&self.root)
        {
            return Err(SessionError::RecoveryManifest {
                path: self.path.clone(),
                detail: "configured recovery owner no longer owns this canonical run".into(),
            });
        }
        Ok(())
    }

    fn validate_read(&self) -> Result<(), SessionError> {
        self.validate_owner()?;
        let current = match std::fs::read(&self.path) {
            Ok(bytes) => Some(blake3::hash(&bytes)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => {
                return Err(SessionError::RecoveryManifest {
                    path: self.path.clone(),
                    detail: error.to_string(),
                })
            }
        };
        if current != self.bytes_digest {
            return Err(SessionError::RecoveryManifest {
                path: self.path.clone(),
                detail: "recovery manifest changed after its owned read".into(),
            });
        }
        Ok(())
    }
}

impl SessionLib {
    /// Check an already attached run-owned manifest before reconciling an
    /// external conversation binding. This observes the exact persisted owner
    /// without minting a scope or transferring its incarnation.
    pub fn validate_recovered_public_owner(
        &self,
        expected: &RecoveryPublicOwner,
    ) -> Result<bool, SessionError> {
        let state = self
            .durable_graph
            .as_ref()
            .ok_or(SessionError::WrongPublicManifestTicket)?;
        let retained = state
            .owner
            .as_ref()
            .ok_or(SessionError::WrongPublicManifestTicket)?;
        retained.validate_owner()?;
        if state.unconfirmed.is_some() {
            return Err(SessionError::WrongPublicManifestTicket);
        }
        let invalid = |detail: String| SessionError::RecoveryManifest {
            path: state.path.clone(),
            detail,
        };
        let bytes = std::fs::read(&state.path).map_err(|error| invalid(error.to_string()))?;
        let read = recovery::read_v2_bytes(
            &state.path,
            state
                .path
                .parent()
                .expect("attached canonical manifest parent"),
            &bytes,
        )
        .map_err(|error| recovery::graph_error(&state.path, error))?
        .ok_or(SessionError::WrongPublicManifestTicket)?;
        if !read.artifact_losses.is_empty()
            || read.graph.checksum != state.graph.checksum
            || read.graph.high_water != state.graph.high_water
        {
            return Err(SessionError::WrongPublicManifestTicket);
        }
        retained.validate_owner()?;
        Ok(read
            .graph
            .public_surfaces
            .iter()
            .any(|surface| &surface.owner == expected))
    }

    /// A path alone can initialize an empty graph, but cannot admit a retained
    /// public surface. Production recovery uses the configured run owner.
    pub fn attach_recovery_graph_v2(
        &mut self,
        path: impl Into<PathBuf>,
    ) -> Result<(), SessionError> {
        self.attach_recovery_graph(path.into(), None)
    }

    pub fn attach_owned_recovery_graph_v3(
        &mut self,
        path: impl Into<PathBuf>,
        authority: Arc<dyn RecoveryRunAuthority>,
    ) -> Result<(), SessionError> {
        self.attach_recovery_graph(path.into(), Some(authority))
    }

    fn attach_recovery_graph(
        &mut self,
        path: PathBuf,
        authority: Option<Arc<dyn RecoveryRunAuthority>>,
    ) -> Result<(), SessionError> {
        let invalid = |detail: String| SessionError::RecoveryManifest {
            path: path.clone(),
            detail,
        };
        if path.exists()
            && path
                .canonicalize()
                .map_err(|error| invalid(error.to_string()))?
                != path
        {
            return Err(invalid(
                "recovery manifest must be the actual canonical run-owned file".into(),
            ));
        }
        if self.log.generation() != Generation(0)
            || self.recovery_manifest_path.is_some()
            || self.durable_graph.is_some()
        {
            return Err(invalid(
                "recovery must attach before declarations are admitted".into(),
            ));
        }
        let parent = path
            .parent()
            .ok_or_else(|| invalid("recovery manifest has no run-owned parent directory".into()))?;
        let root = parent
            .canonicalize()
            .map_err(|error| invalid(error.to_string()))?;
        let path = root.join(
            path.file_name()
                .ok_or_else(|| invalid("recovery manifest has no file name".into()))?,
        );
        let invalid = |detail: String| SessionError::RecoveryManifest {
            path: path.clone(),
            detail,
        };
        if let Some(authority) = &authority {
            if !authority
                .owns_run(&root)
                .map_err(|error| invalid(error.to_string()))?
            {
                return Err(invalid(
                    "configured recovery owner does not own this canonical run".into(),
                ));
            }
        }
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => Some(bytes),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(invalid(error.to_string())),
        };
        let graph = match bytes
            .as_ref()
            .map(|bytes| recovery::read_v2_bytes(&path, &root, bytes))
            .transpose()
            .map_err(|error| recovery::graph_error(&path, error))?
            .flatten()
        {
            Some(read) if read.artifact_losses.is_empty() => read.graph,
            Some(_) => {
                return Err(invalid(
                    "recovery graph has unavailable or corrupt artifacts".into(),
                ))
            }
            None if bytes.is_some() => {
                return Err(invalid(
                    "existing recovery manifest did not yield an exact graph".into(),
                ))
            }
            None => recovery::RecoveryGraph::empty(self.id.0, self.id.0)
                .map_err(|error| invalid(error.to_string()))?,
        };
        if authority.is_none() && !graph.public_surfaces.is_empty() {
            return Err(invalid(
                "retained public recovery requires its configured run owner".into(),
            ));
        }
        let owner = authority.map(|authority| {
            Arc::new(OwnedRecoveryManifest {
                authority,
                root: root.clone(),
                path: path.clone(),
                bytes_digest: bytes.as_ref().map(|bytes| blake3::hash(bytes)),
            })
        });
        if let Some(owner) = &owner {
            owner.validate_read()?;
        }
        let recovered_log = self.hydrate_recovery_graph(&graph, &root)?;
        if let Some(owner) = &owner {
            owner.validate_read()?;
        }
        self.log = recovered_log;
        self.durable_graph = Some(DurableDeclarationGraph {
            owner,
            path,
            graph,
            unconfirmed: None,
        });
        Ok(())
    }
    pub(super) fn hydrate_recovery_graph(
        &self,
        graph: &recovery::RecoveryGraph,
        recovery_root: &Path,
    ) -> Result<DeclLog, SessionError> {
        let invalid = |detail: String| SessionError::RecoveryManifest {
            path: recovery_root.to_path_buf(),
            detail,
        };
        let mut log = DeclLog::new();
        if !log.restore_high_water(graph.high_water) {
            return Err(invalid(
                "could not restore burned declaration identities".into(),
            ));
        }
        for surface in &graph.public_surfaces {
            let Some(generation) = surface.declaration_root else {
                continue;
            };
            if log.recovered_at(generation).is_some() {
                continue;
            }
            let node = graph
                .nodes
                .iter()
                .find(|node| node.id == generation)
                .ok_or_else(|| invalid("recovered public root is absent".into()))?;
            if node.lexical_roots.len() != 1 {
                return Err(invalid(
                    "recovered declaration tip has no unique lexical root".into(),
                ));
            }
            let root = node.lexical_roots[0].clone();
            let projection = graph
                .projection(&surface.owner, &BTreeMap::new())
                .map_err(|error| invalid(error.to_string()))?;
            let mut exports = Vec::new();
            for head in projection.values() {
                let recovery::RecoveryHead::Available { export, .. } = head else {
                    return Err(invalid(
                        "public declaration root retains unavailable live state".into(),
                    ));
                };
                exports.push(recovered_export(export).ok_or_else(|| {
                    invalid("unsupported durable declaration export identity".into())
                })?);
            }
            if !matches!(
                node.state,
                recovery::RecoveryNodeState::ExactArtifactClosure
            ) || !node.live_dependencies.is_empty()
            {
                return Err(invalid(
                    "public declaration root retains unavailable live state".into(),
                ));
            }
            let selected = graph
                .artifacts
                .iter()
                .filter(|artifact| node.artifact_refs.contains(&artifact.artifact_id()))
                .collect::<Vec<_>>();
            let mut products = Vec::new();
            let mut joins = Vec::new();
            let mut values = Vec::new();
            for artifact in selected {
                match artifact {
                    recovery::RecoveryArtifactClosure::Home(reference) => {
                        products.push(reference.clone())
                    }
                    recovery::RecoveryArtifactClosure::Join(reference) => {
                        joins.push(reference.clone())
                    }
                    recovery::RecoveryArtifactClosure::ValueInterface(reference) => {
                        values.push(reference.clone())
                    }
                }
            }
            let artifact_ids = node.artifact_refs.iter().copied().collect::<BTreeSet<_>>();
            let descriptors = graph
                .artifacts
                .iter()
                .filter(|artifact| artifact_ids.contains(&artifact.artifact_id()))
                .map(|artifact| artifact.descriptor())
                .collect::<Vec<_>>();
            let dependencies = graph
                .artifact_dependencies
                .iter()
                .filter(|edge| {
                    artifact_ids.contains(&edge.source) && artifact_ids.contains(&edge.target)
                })
                .map(|edge| (edge.source, edge.target, edge.dependency.clone()))
                .collect::<Vec<_>>();
            let instances = recovered_instances(&node.instances)
                .ok_or_else(|| invalid("unsupported durable instance identity".into()))?;
            let family_closure = node
                .instances
                .family_consistency_closure
                .iter()
                .map(recovered_identity)
                .collect::<Option<Vec<_>>>()
                .ok_or_else(|| invalid("unsupported durable family closure identity".into()))?;
            let evidence = Arc::new(certify_recovered_declaration_tip_with_inventory(
                recovery_root,
                &products,
                &joins,
                &values,
                &descriptors,
                &dependencies,
                RecoveryDeclarationSelection {
                    root: root.clone(),
                    lexical: node.lexical.clone(),
                    exports,
                    instances,
                    family_closure,
                },
                &self.extra_include,
            )?);
            let source_roots = node
                .lexical
                .iter()
                .find(|lexical| lexical.owner == root)
                .ok_or_else(|| invalid("recovered root lacks lexical edges".into()))?
                .imports
                .clone();
            let mut reachable = BTreeSet::new();
            let mut pending = source_roots.clone();
            while let Some(owner) = pending.pop() {
                if reachable.insert(owner.clone()) {
                    let lexical = node
                        .lexical
                        .iter()
                        .find(|lexical| lexical.owner == owner)
                        .ok_or_else(|| invalid("recovered lexical surface is not closed".into()))?;
                    pending.extend(lexical.imports.clone());
                }
            }
            let source_surface = render::AdmittedDeclarationSurface {
                roots: source_roots,
                lexical: node
                    .lexical
                    .iter()
                    .filter(|lexical| reachable.contains(&lexical.owner))
                    .cloned()
                    .collect(),
            };
            let imports = graph
                .workbench_imports(generation)
                .map_err(|error| invalid(error.to_string()))?;
            let turn = render::DeclTurn {
                normalized: DeclarationSource {
                    prologue: Default::default(),
                    body: String::new(),
                },
                external_imports: SourceImports::new(),
                sources: Vec::new(),
                workbench_imports: SourceImports::from_specs(imports),
                items: evidence.exports().iter().map(recovered_item).collect(),
                value_types: BTreeMap::new(),
                retracts: Vec::new(),
                parent: None,
            };
            if !log.restore_recovered(
                generation,
                render::RecoveredDeclaration {
                    turn,
                    evidence,
                    surface: source_surface,
                },
            ) {
                return Err(invalid(
                    "recovered tip conflicts with its burned module identity".into(),
                ));
            }
        }
        Ok(log)
    }
}

fn recovered_identity(identity: &recovery::RecoverySymbolIdentity) -> Option<ExportIdentity> {
    let namespace = match identity.namespace.as_str() {
        "value" => ExportNamespace::Value,
        "type" => ExportNamespace::Type,
        "constructor" => ExportNamespace::Constructor,
        "field" => ExportNamespace::Field,
        _ => return None,
    };
    let record_parent = match &identity.record_parent {
        None => None,
        Some(parent)
            if parent.unit == identity.unit
                && parent.module == identity.module
                && parent.namespace == "type"
                && parent.record_parent.is_none() =>
        {
            Some(parent.occurrence.clone())
        }
        Some(_) => return None,
    };
    Some(ExportIdentity {
        unit: identity.unit.clone(),
        module: identity.module.clone(),
        namespace,
        occurrence: identity.occurrence.clone(),
        record_parent,
    })
}

fn recovered_export(export: &recovery::RecoveryExport) -> Option<DeclarationExport> {
    Some(DeclarationExport {
        kind: match export.kind {
            recovery::RecoveryExportKind::Value => DeclarationKind::Value,
            recovery::RecoveryExportKind::Type => DeclarationKind::Type,
            recovery::RecoveryExportKind::Class => DeclarationKind::Class,
        },
        head: recovered_identity(&export.identity)?,
        children: export
            .children
            .iter()
            .map(recovered_identity)
            .collect::<Option<_>>()?,
    })
}

fn recovered_instances(
    inventory: &recovery::RecoveryInstanceInventory,
) -> Option<InstanceInventory> {
    Some(InstanceInventory {
        classes: inventory
            .classes
            .iter()
            .filter(|instance| instance.selected)
            .map(|instance| {
                Some(ClassInstanceEvidence {
                    dfun: recovered_identity(&instance.dfun)?,
                    class: recovered_identity(&instance.class)?,
                    selected_axioms: instance
                        .selected_axioms
                        .iter()
                        .map(recovered_identity)
                        .collect::<Option<_>>()?,
                })
            })
            .collect::<Option<_>>()?,
        families: inventory
            .selected_family_axioms
            .iter()
            .map(recovered_identity)
            .collect::<Option<_>>()?,
    })
}

fn recovered_item(export: &DeclarationExport) -> ExportItem {
    match export.kind {
        DeclarationKind::Value => ExportItem::Value {
            name: export.head.occurrence.clone(),
        },
        DeclarationKind::Type => ExportItem::Type {
            name: export.head.occurrence.clone(),
            cons: export
                .children
                .iter()
                .map(|child| child.occurrence.clone())
                .collect(),
        },
        DeclarationKind::Class => ExportItem::Class {
            name: export.head.occurrence.clone(),
            methods: export
                .children
                .iter()
                .map(|child| child.occurrence.clone())
                .collect(),
        },
    }
}
