//! Restore public declaration authority through the existing log and scope owners.

use super::*;
use std::collections::BTreeSet;
use std::sync::Arc;
use tidepool_toolchain::declaration_join::{
    certify_recovered_declaration_tip_in_context, ClassInstanceEvidence, DeclarationExport,
    DeclarationKind, ExportIdentity, ExportNamespace, InstanceInventory,
    RecoveredArtifactInventory, RecoveryDeclarationOrigin, RecoveryDeclarationSelection,
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

/// The observed machine domain before attachment created any initial roots.
/// An existing run database may contain only independent child domains.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecoveredDomainPresence {
    Absent,
    Empty,
    Published,
}

/// Retains the configured host lease and immutable demanded compiler receipts.
/// The database path and the compiler artifact directory are separate facts.
pub(super) struct OwnedRecoveryManifest {
    authority: Arc<dyn RecoveryRunAuthority>,
    root: PathBuf,
    path: PathBuf,
    artifact_root: PathBuf,
    contexts: parking_lot::Mutex<
        BTreeMap<
            tidepool_toolchain::exact_context::ContextRoot,
            Arc<tidepool_toolchain::declaration_join::ExactDeclarationContext>,
        >,
    >,
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
    /// This check has no SQL work and is also used before consuming a held
    /// generation lease. The retained host authority can revoke admission.
    pub(super) fn validate_owner(&self) -> Result<(), SessionError> {
        let invalid = |detail: String| SessionError::RecoveryManifest {
            path: self.path.clone(),
            detail,
        };
        if !self
            .authority
            .owns_run(&self.root)
            .map_err(|error| invalid(error.to_string()))?
            || self
                .root
                .canonicalize()
                .map_err(|error| invalid(error.to_string()))?
                != self.root
            || self
                .path
                .canonicalize()
                .map_err(|error| invalid(error.to_string()))?
                != self.path
            || self.path.parent() != Some(self.root.as_path())
            || self
                .artifact_root
                .canonicalize()
                .map_err(|error| invalid(error.to_string()))?
                != self.artifact_root
            || !self.artifact_root.starts_with(&self.root)
        {
            return Err(invalid(
                "configured recovery owner no longer owns these canonical run paths".into(),
            ));
        }
        Ok(())
    }
}

/// Authenticate the payload union once for this acquisition, then reissue each
/// demanded context through the compiler inventory owner. Historical roots
/// outside this set do not cause artifact reads or compiler reconstruction.
fn hydrate_context_roots(
    graph: &recovery::RecoveryGraph,
    owner: &OwnedRecoveryManifest,
    roots: &BTreeSet<tidepool_toolchain::exact_context::ContextRoot>,
) -> Result<(), SessionError> {
    owner.validate_owner()?;
    graph
        .store()
        .ensure_ready()
        .map_err(|error| SessionError::RecoveryStore(Box::new(error)))?;
    let cached = owner.contexts.lock();
    let demanded = roots
        .iter()
        .filter(|root| !cached.contains_key(*root))
        .cloned()
        .collect::<Vec<_>>();
    drop(cached);
    if demanded.is_empty() {
        return Ok(());
    }
    let mut receipts = Vec::new();
    let mut bindings = BTreeSet::new();
    let mut edges = BTreeSet::new();
    for root in demanded {
        let receipt = graph
            .context(&root)
            .map_err(|error| recovery::graph_error(&owner.path, error))?;
        let snapshot = receipt.snapshot();
        let selection = snapshot
            .graph_selection()
            .map_err(|error| SessionError::RecoverySnapshot(Box::new(error)))?;
        bindings.extend(selection.bindings.iter().copied());
        edges.extend(
            snapshot
                .binding_dependencies()
                .map_err(|error| SessionError::RecoverySnapshot(Box::new(error)))?,
        );
        receipts.push((receipt, selection));
    }
    let artifacts = bindings
        .iter()
        .map(|binding| binding.artifact)
        .collect::<BTreeSet<_>>();
    let mut products = Vec::new();
    let mut interfaces = Vec::new();
    let mut joins = Vec::new();
    let mut values = Vec::new();
    let mut descriptors = Vec::new();
    for id in artifacts {
        let artifact = graph
            .artifact(id)
            .map_err(|error| recovery::graph_error(&owner.path, error))?
            .ok_or(SessionError::WrongPublicManifestTicket)?;
        descriptors.push(artifact.descriptor());
        match artifact.as_ref() {
            recovery::RecoveryArtifactClosure::Home(reference) => products.push(reference.clone()),
            recovery::RecoveryArtifactClosure::ModuleInterface(reference) => {
                interfaces.push(reference.clone())
            }
            recovery::RecoveryArtifactClosure::Join(reference) => joins.push(reference.clone()),
            recovery::RecoveryArtifactClosure::ValueInterface(reference) => {
                values.push(reference.clone())
            }
        }
    }
    let inventory = RecoveredArtifactInventory::capture_bound(
        &owner.artifact_root,
        &products,
        &interfaces,
        &joins,
        &values,
        &descriptors,
        &bindings.into_iter().collect::<Vec<_>>(),
        &edges.into_iter().collect::<Vec<_>>(),
    )
    .map_err(|error| SessionError::RecoveryArtifacts(Box::new(error)))?;
    let mut restored = Vec::new();
    for (receipt, selection) in receipts {
        let snapshot = receipt.snapshot();
        let mut context = inventory.context_with_graph_selection(
            &selection,
            &snapshot.compiler_roles(),
            snapshot.lexical(),
        )?;
        if context.toolchain_identity_sha256() != snapshot.producer_sha256() {
            return Err(SessionError::WrongPublicManifestTicket);
        }
        context.bind_snapshot_baseline(&receipt)?;
        restored.push((receipt.root().clone(), Arc::new(context)));
    }
    owner.validate_owner()?;
    owner.contexts.lock().extend(restored);
    Ok(())
}

fn cached_context(
    owner: &OwnedRecoveryManifest,
    root: &tidepool_toolchain::exact_context::ContextRoot,
) -> Result<Arc<tidepool_toolchain::declaration_join::ExactDeclarationContext>, SessionError> {
    owner
        .contexts
        .lock()
        .get(root)
        .cloned()
        .ok_or(SessionError::WrongPublicManifestTicket)
}

impl SessionLib {
    pub fn recovered_domain_presence(&self) -> Result<RecoveredDomainPresence, SessionError> {
        let state = self
            .durable_graph
            .as_ref()
            .ok_or(SessionError::WrongPublicManifestTicket)?;
        state
            .store
            .ensure_ready()
            .map_err(|error| SessionError::RecoveryStore(Box::new(error)))?;
        state
            .owner
            .as_ref()
            .ok_or(SessionError::WrongPublicManifestTicket)?
            .validate_owner()?;
        Ok(if !state.attached_existing_domain {
            RecoveredDomainPresence::Absent
        } else if state
            .store
            .has_public_owners(state.domain_id)
            .map_err(|error| SessionError::RecoveryStore(Box::new(error)))?
        {
            RecoveredDomainPresence::Published
        } else {
            RecoveredDomainPresence::Empty
        })
    }

    pub fn has_recovered_domain(&self) -> Result<bool, SessionError> {
        Ok(self.recovered_domain_presence()? != RecoveredDomainPresence::Absent)
    }

    /// Observe the exact domain revision and keyed public owner without
    /// minting a scope, transferring ownership, or expanding context roots.
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
        let snapshot = state
            .store
            .read_snapshot_identity(state.domain_id, Some(expected))
            .map_err(|error| SessionError::RecoveryStore(Box::new(error)))?
            .ok_or(SessionError::WrongPublicManifestTicket)?;
        if snapshot.domain.revision != state.store_revision
            || snapshot.domain.content_revision != state.graph.content_revision()
            || snapshot.domain.allocation_ceiling != state.allocation_ceiling
            || snapshot.domain.lineage != state.graph.lineage()
            || Some(&snapshot.domain.snapshot_root) != state.graph.domain_root()
        {
            return Err(SessionError::WrongPublicManifestTicket);
        }
        let Some(current) = snapshot.public_owner else {
            return Ok(false);
        };
        let matches = state.graph.public_owner_state(expected) == Some(&current);
        retained.validate_owner()?;
        Ok(matches)
    }

    pub(super) fn recovered_public_compiler_context(
        &self,
        public_owner: &RecoveryPublicOwner,
    ) -> Result<
        Option<Arc<tidepool_toolchain::declaration_join::ExactDeclarationContext>>,
        SessionError,
    > {
        if !self.validate_recovered_public_owner(public_owner)? {
            return Err(SessionError::WrongPublicManifestTicket);
        }
        let state = self
            .durable_graph
            .as_ref()
            .ok_or(SessionError::WrongPublicManifestTicket)?;
        let surface = state
            .graph
            .surface(public_owner)
            .map_err(|error| recovery::graph_error(&state.path, error))?
            .ok_or(SessionError::WrongPublicManifestTicket)?;
        let Some(root) = &surface.context_root else {
            return Ok(None);
        };
        let owner = state
            .owner
            .as_ref()
            .ok_or(SessionError::WrongPublicManifestTicket)?;
        hydrate_context_roots(&state.graph, owner, &BTreeSet::from([root.clone()]))?;
        Ok(Some(cached_context(owner, root)?))
    }

    /// Attach one exact machine-session domain under the configured run lease.
    /// Root survives host reincarnation; independent Session domains are issued
    /// once and never reopen another machine's identity.
    pub fn attach_owned_recovery_domain(
        &mut self,
        store: Arc<RunRecoveryStore>,
        domain: RecoveryDomainId,
        artifact_root: PathBuf,
        authority: Arc<dyn RecoveryRunAuthority>,
        settlement: &mut dyn FnMut(crate::CompilerTransactionClose),
    ) -> Result<(), SessionError> {
        let path = store.path().to_path_buf();
        let invalid = |detail: String| SessionError::RecoveryManifest {
            path: path.clone(),
            detail,
        };
        if self.log.generation() != Generation(0) || self.durable_graph.is_some() {
            return Err(invalid(
                "recovery must attach before declaration identities are admitted".into(),
            ));
        }
        if let RecoveryDomainId::Session(id) = domain {
            if id != self.id {
                return Err(SessionError::WrongPublicManifestTicket);
            }
        }
        let root = path
            .parent()
            .ok_or_else(|| invalid("recovery store has no host run directory".into()))?
            .canonicalize()
            .map_err(|error| invalid(error.to_string()))?;
        if path
            .canonicalize()
            .map_err(|error| invalid(error.to_string()))?
            != path
            || artifact_root
                .canonicalize()
                .map_err(|error| invalid(error.to_string()))?
                != artifact_root
            || self
                .root
                .canonicalize()
                .map_err(|error| invalid(error.to_string()))?
                != artifact_root
        {
            return Err(invalid(
                "recovery attachment requires its actual canonical store and compiler directory"
                    .into(),
            ));
        }
        let owner = Arc::new(OwnedRecoveryManifest {
            authority,
            root,
            path: path.clone(),
            artifact_root: artifact_root.clone(),
            contexts: Default::default(),
        });
        owner.validate_owner()?;
        store
            .ensure_ready()
            .map_err(|error| SessionError::RecoveryStore(Box::new(error)))?;
        let (graph, domain_state, attached_existing_domain) =
            recovery::load_or_create_domain_graph(
                Arc::clone(&store),
                domain,
                self.id.0,
                self.log.generation(),
            )?;
        let recovered_log = self.hydrate_recovery_graph(
            &graph,
            &store,
            &owner,
            domain_state.allocation_ceiling,
            settlement,
        )?;
        owner.validate_owner()?;
        let current = store
            .read_domain(domain)
            .map_err(|error| SessionError::RecoveryStore(Box::new(error)))?
            .ok_or(SessionError::WrongPublicManifestTicket)?;
        if current != domain_state {
            return Err(SessionError::WrongPublicManifestTicket);
        }
        self.log = recovered_log;
        self.generation_lease = None;
        self.local_generation_issued = false;
        self.durable_graph = Some(DurableDeclarationGraph {
            owner: Some(owner),
            readiness_lifetime: Arc::new(()),
            path,
            store,
            domain_id: domain,
            store_revision: domain_state.revision,
            allocation_ceiling: domain_state.allocation_ceiling,
            artifact_root,
            attached_existing_domain,
            graph,
        });
        Ok(())
    }
    pub(super) fn hydrate_recovery_graph(
        &self,
        graph: &recovery::RecoveryGraph,
        store: &RunRecoveryStore,
        owner: &OwnedRecoveryManifest,
        allocation_ceiling: Generation,
        settlement: &mut dyn FnMut(crate::CompilerTransactionClose),
    ) -> Result<DeclLog, SessionError> {
        let invalid = |detail: String| SessionError::RecoveryManifest {
            path: owner.path.clone(),
            detail,
        };
        store
            .ensure_ready()
            .map_err(|error| SessionError::RecoveryStore(Box::new(error)))?;
        graph
            .load_public_surfaces()
            .map_err(|error| recovery::graph_error(&owner.path, error))?;
        let mut roots = BTreeSet::new();
        for surface in graph.public_surfaces() {
            if let Some(root) = &surface.context_root {
                roots.insert(root.clone());
            }
            if let Some(generation) = surface.declaration_root {
                let node = graph
                    .node(generation)
                    .map_err(|error| recovery::graph_error(&owner.path, error))?
                    .ok_or_else(|| invalid("recovered public root is absent".into()))?;
                roots.insert(node.context_root.clone());
            }
        }
        hydrate_context_roots(graph, owner, &roots)?;
        let mut log = DeclLog::new();
        if !log.restore_high_water(allocation_ceiling) {
            return Err(invalid(
                "could not restore burned declaration identities".into(),
            ));
        }
        for surface in graph.public_surfaces() {
            let Some(generation) = surface.declaration_root else {
                continue;
            };
            if log.recovered_at(generation).is_some() {
                continue;
            }
            let node = graph
                .node(generation)
                .map_err(|error| recovery::graph_error(&owner.path, error))?
                .ok_or_else(|| invalid("recovered public root is absent".into()))?;
            let context = cached_context(owner, &node.context_root)?;
            let lexical = context.lexical_graph().to_vec();
            validate_recovery_native_markers(&node, &context).map_err(invalid)?;
            if node.lexical_roots.len() != 1 {
                return Err(invalid(
                    "recovered declaration tip has no unique lexical root".into(),
                ));
            }
            let root = node.lexical_roots[0].clone();
            let projection = graph
                .interface_projection(&surface.owner, &BTreeMap::new())
                .map_err(|error| invalid(error.to_string()))?;
            let mut exports = Vec::new();
            for head in projection.values() {
                let recovery::RecoveryHead::Available { export, .. } = head else {
                    return Err(invalid(
                        "public declaration root lacks exact interface evidence".into(),
                    ));
                };
                exports.push(recovered_export(export).ok_or_else(|| {
                    invalid("unsupported durable declaration export identity".into())
                })?);
            }
            let instances = recovered_instances(&node.instances)
                .ok_or_else(|| invalid("unsupported durable instance identity".into()))?;
            let family_closure = node
                .instances
                .family_consistency_closure
                .iter()
                .map(recovered_identity)
                .collect::<Option<Vec<_>>>()
                .ok_or_else(|| invalid("unsupported durable family closure identity".into()))?;
            let evidence = Arc::new(certify_recovered_declaration_tip_in_context(
                context,
                RecoveryDeclarationSelection {
                    origin: match node.kind {
                        recovery::RecoveryNodeKind::Join => RecoveryDeclarationOrigin::Join,
                        recovery::RecoveryNodeKind::Authored => {
                            RecoveryDeclarationOrigin::Authored {
                                generation: generation.0,
                                introduced_exports: node
                                    .exports
                                    .iter()
                                    .map(recovered_export)
                                    .collect::<Option<Vec<_>>>()
                                    .ok_or_else(|| {
                                        invalid(
                                            "unsupported durable authored export identity".into(),
                                        )
                                    })?,
                            }
                        }
                    },
                    root: root.clone(),
                    lexical: lexical.clone(),
                    exports,
                    instances,
                    family_closure,
                },
                &self.extra_include,
                settlement,
            )?);
            let source_roots = lexical
                .iter()
                .find(|lexical| lexical.owner == root)
                .ok_or_else(|| invalid("recovered root lacks lexical edges".into()))?
                .imports
                .clone();
            let mut reachable = BTreeSet::new();
            let mut pending = source_roots.clone();
            while let Some(owner) = pending.pop() {
                if reachable.insert(owner.clone()) {
                    let source = lexical
                        .iter()
                        .find(|lexical| lexical.owner == owner)
                        .ok_or_else(|| invalid("recovered lexical surface is not closed".into()))?;
                    pending.extend(source.imports.clone());
                }
            }
            let source_surface = render::AdmittedDeclarationSurface {
                roots: source_roots,
                lexical: lexical
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
                node.kind,
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

pub(super) fn validate_recovery_native_markers(
    node: &recovery::RecoveryNode,
    context: &tidepool_toolchain::declaration_join::ExactDeclarationContext,
) -> Result<(), String> {
    let expected = match node.kind {
        recovery::RecoveryNodeKind::Authored => {
            let artifacts = BTreeSet::from([context
                .authored_native_root(node.id.0)
                .map_err(|error| error.to_string())?]);
            let roots = selected_native_roots(context.artifact_view(), &artifacts);
            certified_native_dependencies(context, &roots).map_err(|error| error.to_string())?
        }
        recovery::RecoveryNodeKind::Join => {
            let owners = node
                .exports
                .iter()
                .flat_map(|export| std::iter::once(&export.identity).chain(export.children.iter()))
                .map(
                    |identity| tidepool_toolchain::declaration_join::ExactModuleIdentity {
                        unit: identity.unit.clone(),
                        module: identity.module.clone(),
                    },
                )
                .collect();
            native_binding_dependencies(
                context
                    .authored_native_binding_custody_requirements(&owners)
                    .map_err(|error| error.to_string())?,
            )
        }
    };
    let expected = expected.into_iter().collect::<BTreeSet<_>>();
    let actual = node
        .live_dependencies
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    if actual.len() != node.live_dependencies.len() || actual != expected {
        return Err(
            "recovery live dependencies differ from verified original native requirements".into(),
        );
    }
    Ok(())
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

/// Stage the exact scoped context and its selected artifact custody. The
/// caller supplies the actual prior owner root for incremental materialization.
pub(super) fn materialize_public_compiler_context(
    graph: &recovery::RecoveryGraph,
    root: &Path,
    context: Option<&Arc<tidepool_toolchain::declaration_join::ExactDeclarationContext>>,
    previous: Option<&tidepool_toolchain::exact_context::ContextRoot>,
) -> Result<
    (
        recovery::RecoveryGraph,
        Option<tidepool_toolchain::exact_context::ContextRoot>,
    ),
    SessionError,
> {
    let Some(context) = context else {
        return Ok((graph.clone(), None));
    };
    let invalid = |detail: String| SessionError::RecoveryManifest {
        path: root.to_path_buf(),
        detail,
    };
    let mut candidate = graph.candidate();
    let artifacts =
        recovery::materialize_missing_context_artifacts(graph, root, context, previous)?;
    for artifact in artifacts {
        candidate
            .insert_artifact(artifact)
            .map_err(|error| invalid(error.to_string()))?;
    }
    let context_root = candidate
        .insert_context(context, previous)
        .map_err(|error| recovery::graph_error(root, error))?;
    Ok((
        candidate
            .seal()
            .map_err(|error| recovery::graph_error(root, error))?,
        Some(context_root),
    ))
}

#[cfg(test)]
mod lazy_tests {
    use super::*;
    use crate::session::recovery_test_fixture::RecoveryFixture;
    use tidepool_testing::effect_surface::TestEffectSurface;

    #[derive(Clone)]
    struct EmptyOutput;
    impl OutputSink for EmptyOutput {
        fn drain(&self) -> Vec<String> {
            Vec::new()
        }
        fn snapshot(&self) -> Vec<String> {
            Vec::new()
        }
    }

    fn library(root: &Path, effects: &TestEffectSurface) -> SessionLib {
        SessionLib::open(
            fresh_session_id(),
            root.join("haskell-session"),
            ModuleEnv::standalone_default(),
        )
        .unwrap()
        .with_validation_include(effects.include_paths().to_vec())
    }

    fn owner() -> RecoveryPublicOwner {
        RecoveryPublicOwner::new(&tidepool_repr::ActorPath::parse("root/lazy").unwrap(), 1).unwrap()
    }

    fn probe(
        context: Arc<tidepool_toolchain::declaration_join::ExactDeclarationContext>,
        lexical_root: &tidepool_toolchain::declaration_join::ExactModuleIdentity,
        effects: &TestEffectSurface,
        root: &Path,
        generation: u64,
    ) -> serde_json::Value {
        let imports = &lexical_root.module;
        let templates = [TurnTemplate {
            kind: TemplateSelector::Expr,
            source: assemble_expression_module(
                &insert_preamble_imports(effects.preamble(), imports),
                "__result",
                effects.row(),
                "{{TURN}}",
                ExpressionLift::Pure,
            ),
        }];
        let include = effects
            .include_paths()
            .iter()
            .map(PathBuf::as_path)
            .collect::<Vec<_>>();
        let TurnResult::Expr { compiled, .. } = tidepool_testing::with_settlement(|settlement| {
            run_turn(
                TurnRequest {
                    exact_context: Some(Arc::new(
                        tidepool_toolchain::declaration_join::ExactCompileContext::new(context),
                    )),
                    session_id: None,
                    turn_text: "answer (41 :: Int)",
                    templates: &templates,
                    include: &include,
                    session_root: root,
                    inject_modules: &[],
                    gen: generation,
                    verdict: None,
                    target: None,
                    retained_imports: &[],
                },
                settlement,
            )
        })
        .unwrap() else {
            panic!("the original probe must compile as an expression");
        };
        let mut resident = ResidentSession::from_persistent_for_test(
            frunk::HNil,
            EmptyOutput,
            PersistentSession::new(None, 1024 * 1024),
        );
        let ResidentOutcome::Completed { result, .. } =
            tidepool_testing::with_settlement(|settlement| {
                resident.run_with_sites("immutable_context_probe", compiled.code(), settlement)
            })
            .unwrap()
        else {
            panic!("the original probe must finish");
        };
        result.to_json()
    }

    #[test]
    fn cold_hydration_demands_only_selected_contexts_and_preserves_older_shadowed_native_context() {
        tidepool_testing::eval_harness::require_extract();
        let effects = TestEffectSurface::minimal(&[]).unwrap();
        let run = tempfile::tempdir().unwrap();
        let fixture = RecoveryFixture::create(run.path());
        let mut lib = library(run.path(), &effects);
        tidepool_testing::with_settlement(|settlement| fixture.attach_root(&mut lib, settlement))
            .unwrap();
        let mut producer = PersistentSession::new(Some(lib), 1024 * 1024);
        let public = producer.mint_isolated_scope();
        producer
            .initialize_durable_public_scope(owner(), public)
            .unwrap();
        let execution = Arc::new(producer.begin_private_execution(public).unwrap());
        let private = execution.private_scope();
        tidepool_testing::with_settlement(|settlement| {
            producer.define_scoped_in(
                private,
                &[include_str!("fixtures/recovery-original.hs")],
                settlement,
            )
        })
        .unwrap();
        let old_tip = producer.lib().scope_tip(private);
        let old_node = producer
            .lib()
            .durable_graph
            .as_ref()
            .unwrap()
            .graph
            .node(old_tip)
            .unwrap()
            .unwrap();
        let old_root = old_node.context_root.clone();
        let old_lexical_root = old_node.lexical_roots[0].clone();
        let old_selection = producer
            .compile_view_in(private)
            .unwrap()
            .exact_declaration_context()
            .unwrap()
            .artifact_view()
            .capture_graph_selection();
        tidepool_testing::with_settlement(|settlement| {
            producer.define_scoped_in(
                private,
                &["answer :: Int -> Int\nanswer value = value + 100"],
                settlement,
            )
        })
        .unwrap();
        let intent = producer
            .freeze_execution_intent(&execution, vec![], vec![])
            .unwrap();
        let CertifiedDeclarationPublication::Accepted(accepted) =
            tidepool_testing::with_settlement(|settlement| {
                producer
                    .restage_declaration_publication(owner(), intent)
                    .unwrap()
                    .certify(settlement)
            })
            .unwrap()
        else {
            panic!("shadowed public declaration must be accepted");
        };
        assert!(matches!(
            producer
                .publish_staged_public_manifest(
                    accepted.stage().unwrap(),
                    &PublicationDecision::new()
                )
                .unwrap(),
            PublicManifestCommit::Durable
        ));
        let current_tip = producer.lib().scope_tip(public);
        let current_node = producer
            .lib()
            .durable_graph
            .as_ref()
            .unwrap()
            .graph
            .node(current_tip)
            .unwrap()
            .unwrap();
        let current_lexical_root = current_node.lexical_roots[0].clone();
        let current_root = producer
            .lib()
            .durable_graph
            .as_ref()
            .unwrap()
            .graph
            .surface(&owner())
            .unwrap()
            .unwrap()
            .context_root
            .clone()
            .unwrap();
        assert_ne!(old_root, current_root);
        let authority = Arc::clone(&fixture.owner);
        drop(producer);
        drop(fixture);
        let authored_tree = run.path().join("haskell-session/Tidepool/Session");
        assert!(
            authored_tree.is_dir(),
            "the producer must have authored its session modules"
        );
        std::fs::remove_dir_all(authored_tree).unwrap();

        let fixture = RecoveryFixture::reopen(Arc::clone(&authority));
        let mut cold = library(run.path(), &effects);
        tidepool_testing::with_settlement(|settlement| fixture.attach_root(&mut cold, settlement))
            .unwrap();
        let state = cold.durable_graph.as_ref().unwrap();
        let retained = state.owner.as_ref().unwrap();
        hydrate_context_roots(&state.graph, retained, &BTreeSet::from([old_root.clone()])).unwrap();
        let older = cached_context(retained, &old_root).unwrap();
        assert_eq!(
            older.artifact_view().capture_graph_selection(),
            old_selection
        );
        let newer = cold
            .recovered_public_compiler_context(&owner())
            .unwrap()
            .unwrap();
        assert_eq!(
            probe(older, &old_lexical_root, &effects, cold.include_dir(), 8001),
            serde_json::json!(42)
        );
        assert_eq!(
            probe(
                newer,
                &current_lexical_root,
                &effects,
                cold.include_dir(),
                8002
            ),
            serde_json::json!(141)
        );
        drop(cold);
        drop(fixture);

        let database = run.path().join("recovery.sqlite");
        let connection = rusqlite::Connection::open(&database).unwrap();
        assert_eq!(
            connection
                .execute(
                    "DELETE FROM blocks WHERE cid=?1",
                    [old_root.cid().to_bytes()]
                )
                .unwrap(),
            1
        );
        drop(connection);
        let fixture = RecoveryFixture::reopen(Arc::clone(&authority));
        let mut cold = library(run.path(), &effects);
        tidepool_testing::with_settlement(|settlement| fixture.attach_root(&mut cold, settlement))
            .unwrap();
        assert!(cold.validate_recovered_public_owner(&owner()).unwrap());
        let state = cold.durable_graph.as_ref().unwrap();
        let error = hydrate_context_roots(
            &state.graph,
            state.owner.as_ref().unwrap(),
            &BTreeSet::from([old_root.clone()]),
        )
        .unwrap_err();
        assert!(
            matches!(error, SessionError::RecoverySnapshot(error) if matches!(*error, tidepool_toolchain::exact_context::SnapshotError::MissingBlock(cid) if cid == *old_root.cid()))
        );
        drop(cold);
        drop(fixture);

        let connection = rusqlite::Connection::open(&database).unwrap();
        assert_eq!(
            connection
                .execute(
                    "DELETE FROM blocks WHERE cid=?1",
                    [current_root.cid().to_bytes()]
                )
                .unwrap(),
            1
        );
        drop(connection);
        let fixture = RecoveryFixture::reopen(authority);
        let mut refused = library(run.path(), &effects);
        assert!(tidepool_testing::with_settlement(
            |settlement| fixture.attach_root(&mut refused, settlement)
        )
        .is_err());
        assert!(refused.durable_graph.is_none());
        assert_eq!(refused.generation(), Generation(0));
    }
}
