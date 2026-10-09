//! Published source imports carry one completed original selection. The marker
//! changes source selection policy; immutable custody remains inventory-owned.

use super::*;
use sha2::{Digest, Sha256};

#[derive(Clone, Debug)]
pub struct PublishedSourceOriginalSelection {
    context: Arc<ExactDeclarationContext>,
    revision: String,
    public_root: ExactModuleIdentity,
}

impl PublishedSourceOriginalSelection {
    pub fn context(&self) -> &Arc<ExactDeclarationContext> {
        &self.context
    }
    pub fn revision(&self) -> &str {
        &self.revision
    }
    pub fn public_root(&self) -> &ExactModuleIdentity {
        &self.public_root
    }
}

impl ExactDeclarationContext {
    fn published_artifact_roots(
        &self,
        root: &ExactModuleIdentity,
        original: ArtifactId,
    ) -> Result<Vec<ArtifactId>, CompileError> {
        let metadata = self.inventory.metadata_snapshot();
        let roles = self.compiler_projection.roles();
        let mut roots = BTreeSet::from([original]);
        for node in self.published_lexical_closure(root)? {
            let role = roles
                .iter()
                .find(|role| {
                    metadata
                        .artifacts
                        .get(&role.interface())
                        .is_some_and(|entry| entry.descriptor.owner == node.owner)
                })
                .ok_or_else(|| {
                    failure("published lexical import loses its selected canonical interface")
                })?;
            roots.insert(role.interface());
        }
        Ok(roots.into_iter().collect())
    }
    fn published_lexical_closure(
        &self,
        root: &ExactModuleIdentity,
    ) -> Result<Vec<ExactLexicalNode>, CompileError> {
        let graph = self
            .lexical
            .iter()
            .map(|node| (&node.owner, node))
            .collect::<BTreeMap<_, _>>();
        let mut pending = vec![root.clone()];
        let mut selected = BTreeSet::new();
        while let Some(owner) = pending.pop() {
            if selected.insert(owner.clone()) {
                let node = graph
                    .get(&owner)
                    .ok_or_else(|| failure("published original loses its exact lexical closure"))?;
                pending.extend(node.imports.iter().cloned());
            }
        }
        Ok(selected
            .into_iter()
            .map(|owner| (*graph[&owner]).clone())
            .collect())
    }

    fn published_selection_sha256(
        &self,
        root: &ExactModuleIdentity,
        role: &CompilerInputRole,
    ) -> Result<[u8; 32], CompileError> {
        let CompilerInputRole::PublishedSourceOriginal {
            interface,
            original,
            source_revision,
            original_input_identity,
            ..
        } = role
        else {
            return Err(failure("published selection lacks its issued policy"));
        };
        if source_revision.is_empty() || original_input_identity.is_empty() {
            return Err(failure(
                "published selection lacks its source revision identity",
            ));
        }
        let closure = self
            .inventory
            .select_roots(self.published_artifact_roots(root, *original)?)?;
        let metadata = closure.metadata_snapshot();
        let entry = metadata
            .artifacts
            .get(original)
            .ok_or_else(|| failure("published original is outside selected custody"))?;
        let ArtifactPayload::Original(product) = &entry.payload else {
            return Err(failure("published selection has no native original"));
        };
        if product.module_interface().is_none_or(|canonical| {
            !matches!(
                canonical.origin(),
                crate::certified_products::CanonicalOrigin::SourceOriginal { .. }
            )
        }) {
            return Err(failure("published original lacks canonical source origin"));
        }
        let compiler = self.compiler_metadata_snapshot()?;
        for entry in metadata
            .artifacts
            .values()
            .filter(|entry| matches!(entry.payload, ArtifactPayload::Original(_)))
        {
            if compiler
                .entries
                .get(&entry.descriptor.owner)
                .map(|selected| selected.descriptor.id)
                != Some(entry.descriptor.id)
            {
                return Err(failure(
                    "published selection changes a selected original dependency",
                ));
            }
        }
        let bytes = serde_json::to_vec(&(
            "published-source-original1",
            self.producer,
            root,
            interface,
            original,
            source_revision,
            original_input_identity,
            metadata.artifacts.keys().copied().collect::<Vec<_>>(),
            closure.selected_native_groups(),
            self.published_lexical_closure(root)?,
        ))
        .map_err(failure)?;
        Ok(Sha256::digest(bytes).into())
    }

    pub(super) fn validate_published_source_originals(&self) -> Result<(), CompileError> {
        let roles = self
            .compiler_projection
            .roles()
            .into_iter()
            .filter(CompilerInputRole::is_published_source_original)
            .collect::<Vec<_>>();
        if roles.is_empty() {
            return Ok(());
        }
        let metadata = self.inventory.metadata_snapshot();
        for role in roles {
            if let CompilerInputRole::PublishedSourceOriginal {
                interface,
                selection_sha256,
                ..
            } = &role
            {
                let root = &metadata
                    .artifacts
                    .get(interface)
                    .ok_or_else(|| failure("published interface is outside retained custody"))?
                    .descriptor
                    .owner;
                if self.published_selection_sha256(root, &role)? != *selection_sha256 {
                    return Err(failure(
                        "published original selection differs from its issued closure",
                    ));
                }
            }
        }
        Ok(())
    }

    pub fn published_source_original_selections(
        &self,
    ) -> Result<Vec<Arc<PublishedSourceOriginalSelection>>, CompileError> {
        if !self
            .compiler_projection
            .roles()
            .iter()
            .any(CompilerInputRole::is_published_source_original)
        {
            return Ok(Vec::new());
        }
        self.normalize()?;
        let metadata = self.inventory.metadata_snapshot();
        self.compiler_projection
            .roles()
            .into_iter()
            .filter_map(|role| {
                let CompilerInputRole::PublishedSourceOriginal {
                    interface,
                    source_revision,
                    original,
                    ..
                } = role
                else {
                    return None;
                };
                Some((
                    metadata.artifacts[&interface].descriptor.owner.clone(),
                    original,
                    source_revision,
                ))
            })
            .map(|(root, original, revision)| {
                let context = self.published_context_for_root(&root, original)?;
                context.normalize()?;
                Ok(Arc::new(PublishedSourceOriginalSelection {
                    context: Arc::new(context),
                    revision,
                    public_root: root,
                }))
            })
            .collect()
    }

    fn published_context_for_root(
        &self,
        root: &ExactModuleIdentity,
        original: ArtifactId,
    ) -> Result<Self, CompileError> {
        let mut context = self.clone();
        context.inventory = self
            .inventory
            .select_roots(self.published_artifact_roots(root, original)?)?;
        let ids = context
            .inventory
            .descriptors()
            .into_iter()
            .map(|row| row.id)
            .collect::<BTreeSet<_>>();
        let metadata = self.inventory.metadata_snapshot();
        let roles = self
            .compiler_projection
            .roles()
            .into_iter()
            .filter(|role| ids.contains(&role.interface()))
            .map(|role| {
                let owner = &metadata.artifacts[&role.interface()].descriptor.owner;
                if owner == root && role.is_published_source_original() {
                    role
                } else if let Some(original) =
                    role.original().filter(|original| ids.contains(original))
                {
                    CompilerInputRole::ReusableOriginal {
                        interface: role.interface(),
                        original,
                    }
                } else {
                    CompilerInputRole::InterfaceOnly {
                        interface: role.interface(),
                    }
                }
            })
            .collect::<Vec<_>>();
        context.compiler_projection = CompilerInputProjection::restore(&context.inventory, &roles)?;
        context.lexical = self.published_lexical_closure(root)?;
        context.original_instance_environment = OriginalInstanceEnvironment::Unknown;
        Ok(context)
    }

    pub(crate) fn issue_published_source_original(
        &self,
        revision: &str,
        input_identity: &str,
        root: &ExactModuleIdentity,
    ) -> Result<Arc<PublishedSourceOriginalSelection>, CompileError> {
        let target = self.original_instance_target()?;
        if !self
            .lexical
            .iter()
            .any(|node| &node.owner == target && node.imports.contains(root))
        {
            return Err(failure(
                "published root is not a direct import of the completed source target",
            ));
        }
        let metadata = self.compiler_metadata_snapshot()?;
        let entry = metadata
            .entries
            .get(root)
            .ok_or_else(|| failure("published root lacks an issued compiler original"))?;
        if !matches!(entry.payload, ArtifactPayload::Original(_)) {
            return Err(failure("published root has only interface custody"));
        }
        let mut context = self.published_context_for_root(root, entry.descriptor.id)?;
        let interface = context
            .compiler_projection
            .roles()
            .into_iter()
            .find(|role| role.original() == Some(entry.descriptor.id))
            .ok_or_else(|| failure("published root lacks its exact interface role"))?
            .interface();
        let mut role = CompilerInputRole::PublishedSourceOriginal {
            interface,
            original: entry.descriptor.id,
            source_revision: revision.into(),
            original_input_identity: input_identity.into(),
            selection_sha256: [0; 32],
        };
        let digest = context.published_selection_sha256(root, &role)?;
        if let CompilerInputRole::PublishedSourceOriginal {
            selection_sha256, ..
        } = &mut role
        {
            *selection_sha256 = digest;
        }
        context.compiler_projection.admit_role(root.clone(), role)?;
        context.normalize()?;
        Ok(Arc::new(PublishedSourceOriginalSelection {
            context: Arc::new(context),
            revision: revision.into(),
            public_root: root.clone(),
        }))
    }

    /// Compose the exact issuer-owned public graph and native closure. Identical
    /// selections are idempotent; a conflicting original or policy refuses.
    pub fn with_published_source_originals(
        mut self,
        selection: &PublishedSourceOriginalSelection,
    ) -> Result<Self, CompileError> {
        let incoming = selection.context();
        self.admit_producer(incoming.producer)?;
        let lexical = compose_lexical_nodes(self.lexical.iter().chain(incoming.lexical.iter()))?;
        self.compiler_projection = self
            .compiler_projection
            .merge(&incoming.compiler_projection)?;
        self.inventory = self.inventory.merge(&incoming.inventory)?;
        self.lexical = lexical;
        self.normalize()?;
        Ok(self)
    }

    pub(super) fn published_scope_value(&self) -> Result<Value, CompileError> {
        if !self
            .compiler_projection
            .roles()
            .iter()
            .any(CompilerInputRole::is_published_source_original)
        {
            return Ok(Value::Array(vec![]));
        }
        self.validate_published_source_originals()?;
        let metadata = self.compiler_metadata_snapshot()?;
        Ok(Value::Array(
            self.compiler_projection
                .roles()
                .into_iter()
                .filter_map(|role| {
                    let CompilerInputRole::PublishedSourceOriginal {
                        interface,
                        original,
                        source_revision,
                        original_input_identity,
                        selection_sha256,
                    } = role
                    else {
                        return None;
                    };
                    Some((
                        interface,
                        original,
                        source_revision,
                        original_input_identity,
                        selection_sha256,
                    ))
                })
                .map(|(interface, original, revision, input, selection)| {
                    let owner = &metadata.artifacts[&interface].descriptor.owner;
                    let ArtifactPayload::Original(product) = &metadata.artifacts[&original].payload
                    else {
                        return Err(failure("published scope loses native original"));
                    };
                    Ok(Value::Array(vec![
                        text(&owner.unit),
                        text(&owner.module),
                        text(hex(&metadata.artifacts[&interface]
                            .descriptor
                            .interface_sha256)),
                        text(hex(&product.owner().product_sha256)),
                        text(revision),
                        text(input),
                        text(hex(&selection)),
                    ]))
                })
                .collect::<Result<Vec<_>, CompileError>>()?,
        ))
    }
}
