//! Published source imports carry one completed original selection. The marker
//! changes source selection policy; immutable custody remains inventory-owned.

use super::*;
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublishedSourceOriginalSelection {
    context: Arc<ExactDeclarationContext>,
    revision: String,
    public_root: ExactModuleIdentity,
}

impl PublishedSourceOriginalSelection {
    /// Current compiler roles and demand. The published role independently
    /// retains the immutable native support acquired at first publication.
    pub fn context(&self) -> &Arc<ExactDeclarationContext> {
        &self.context
    }
    pub fn revision(&self) -> &str {
        &self.revision
    }
    pub fn public_root(&self) -> &ExactModuleIdentity {
        &self.public_root
    }

    fn selects_target(
        &self,
        lexical: &BTreeSet<ExactModuleIdentity>,
        groups: &BTreeSet<crate::artifact_inventory::NativeGroupBinding>,
    ) -> Result<bool, CompileError> {
        let birth = self
            .context
            .compiler_source_support()
            .publication_birth_support(&self.public_root)?;
        let root_original = birth
            .projection()
            .snapshot_roles()
            .get(&self.public_root)
            .and_then(CompilerInputRole::original)
            .ok_or_else(|| failure("publication birth lacks its selected original root"))?;
        if !lexical.contains(&self.public_root)
            && !groups
                .iter()
                .any(|group| group.binding.artifact() == root_original)
        {
            return Ok(false);
        }
        let CompilerInputRole::PublishedSourceOriginal {
            native_authority, ..
        } = &self.context.compiler_input_projection().snapshot_roles()[&self.public_root]
        else {
            return Err(failure("publication lacks its issued native selection"));
        };
        native_authority.permits_target_native_groups(groups)
    }
}

pub(crate) struct PublishedOriginalEndorsement {
    owner: ExactModuleIdentity,
    role: CompilerInputRole,
}

impl PublishedOriginalEndorsement {
    /// Endorse the role this acquired support already selected for `owner`.
    /// Callers supply the owner, never a separately assembled role fact.
    pub(crate) fn from_acquired_support(
        support: &crate::artifact_inventory::CompilerSourceSupport,
        owner: &ExactModuleIdentity,
    ) -> Result<Self, CompileError> {
        let role = support
            .projection()
            .snapshot_roles()
            .get(owner)
            .ok_or_else(|| failure("publication lacks its issued compiler role"))?
            .clone();
        Ok(Self {
            owner: owner.clone(),
            role,
        })
    }
    pub(crate) fn owner(&self) -> &ExactModuleIdentity {
        &self.owner
    }
    pub(crate) fn role(&self) -> &CompilerInputRole {
        &self.role
    }
}

struct PublishedSelectionFacts<'a> {
    interface: ArtifactId,
    original: ArtifactId,
    source_revision: &'a str,
    original_input_identity: &'a str,
    selection: &'a crate::artifact_inventory::ExactArtifactSelection,
    native_authority: &'a crate::artifact_inventory::PublicationNativeAuthority,
}

impl<'a> PublishedSelectionFacts<'a> {
    fn from_role(role: &'a CompilerInputRole) -> Result<Self, CompileError> {
        let CompilerInputRole::PublishedSourceOriginal {
            interface,
            original,
            source_revision,
            original_input_identity,
            selection,
            native_authority,
            ..
        } = role
        else {
            return Err(failure("published selection lacks its issued policy"));
        };
        Ok(Self {
            interface: *interface,
            original: *original,
            source_revision,
            original_input_identity,
            selection,
            native_authority,
        })
    }
}

impl ExactDeclarationContext {
    fn published_artifact_roots(
        &self,
        root: &ExactModuleIdentity,
        original: ArtifactId,
    ) -> Result<Vec<ArtifactId>, CompileError> {
        let metadata = self
            .owner
            .facts
            .compiler_support
            .artifacts()
            .metadata_snapshot();
        let roles = self.owner.facts.compiler_support.projection().roles();
        let issued = roles.iter().find_map(|role| match role {
            CompilerInputRole::PublishedSourceOriginal {
                original: selected,
                selection,
                ..
            } if *selected == original => Some(selection.clone()),
            _ => None,
        });
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
            // Completed source selection follows the issuer's compiler namespace,
            // independently of the executable groups demanded so far.
            if let Some(dependency) = role.original().filter(|id| match &issued {
                Some(selection) => selection
                    .graph_selection()
                    .namespace
                    .iter()
                    .any(|binding| binding.artifact == *id),
                None => self
                    .owner
                    .facts
                    .compiler_support
                    .artifacts()
                    .contains_namespace_artifact(*id),
            }) {
                roots.insert(dependency);
            }
        }
        Ok(roots.into_iter().collect())
    }
    fn published_lexical_closure(
        &self,
        root: &ExactModuleIdentity,
    ) -> Result<Vec<ExactLexicalNode>, CompileError> {
        let graph = self
            .owner
            .facts
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
        facts: &PublishedSelectionFacts<'_>,
    ) -> Result<[u8; 32], CompileError> {
        let interface = facts.interface;
        let original = &facts.original;
        let source_revision = facts.source_revision;
        let original_input_identity = facts.original_input_identity;
        let selection = facts.selection;
        let native_authority = facts.native_authority;
        if source_revision.is_empty() || original_input_identity.is_empty() {
            return Err(failure(
                "published selection lacks its source revision identity",
            ));
        }
        let closure = self
            .owner
            .facts
            .compiler_support
            .artifacts()
            .select_issued(self.published_artifact_roots(root, *original)?, selection)?;
        let metadata = closure.metadata_snapshot();
        let entry = metadata
            .artifacts
            .get(original)
            .ok_or_else(|| failure("published original is outside selected custody"))?;
        let ArtifactPayload::Original(product) = &entry.payload else {
            return Err(failure("published selection has no native original"));
        };
        if !matches!(
            product.module_interface().origin(),
            crate::certified_products::CanonicalOrigin::SourceOriginal { .. }
        ) {
            return Err(failure("published original lacks canonical source origin"));
        }
        let support = &self.owner.facts.compiler_support;
        let roles = support.projection().snapshot_roles();
        // A narrowed consumer may retain an issued dependency as its exact
        // canonical interface. That does not replace the publication's native
        // closure or endorse a competing compiler original.
        for entry in closure
            .root_entries()
            .iter()
            .filter(|entry| matches!(entry.payload, ArtifactPayload::Original(_)))
        {
            let ArtifactPayload::Original(product) = &entry.payload else {
                unreachable!()
            };
            let canonical = ArtifactEntry::canonical_artifact_id(product.module_interface());
            let issued_binding = closure.selection_snapshot().namespace.get(&canonical);
            let current_binding = support
                .artifacts()
                .selection_snapshot()
                .namespace
                .get(&canonical);
            let current_role = roles.get(&entry.descriptor.owner);
            let compatible = match current_role {
                Some(role) if role.original().is_some() => {
                    role.original() == Some(entry.descriptor.id) && role.interface() == canonical
                }
                Some(CompilerInputRole::InterfaceOnly { interface })
                    if &entry.descriptor.owner != root =>
                {
                    *interface == canonical
                        && issued_binding.is_some()
                        && current_binding == issued_binding
                }
                _ => false,
            };
            if !compatible {
                let current_kind = current_role.map(|role| match role {
                    CompilerInputRole::InterfaceOnly { .. } => "interface-only",
                    CompilerInputRole::ReusableOriginal { .. } => "reusable-original",
                    CompilerInputRole::PublishedSourceOriginal { .. } => "published-original",
                });
                let current_original = current_role.and_then(CompilerInputRole::original);
                let current_interface = current_role.map(CompilerInputRole::interface);
                return Err(failure(format!(
                    "published selection changes a selected original dependency: owner={:?} issued_original={:?} issued_interface={canonical:?} issued_canonical_binding={issued_binding:?} current_role={current_kind:?} current_original={current_original:?} current_interface={current_interface:?} current_canonical_binding={current_binding:?}",
                    entry.descriptor.owner, entry.descriptor.id,
                )));
            }
        }
        // This role commits compiler/native selection. Source permission is
        // carried separately by the context's admitted scope and Context9 root;
        // borrowing this role after source restriction cannot restore a grant.
        let bytes = serde_json::to_vec(&(
            "published-source-original4",
            native_authority.facts().commitment(),
            self.owner.facts.producer,
            root,
            interface,
            original,
            source_revision,
            original_input_identity,
            metadata.artifacts.keys().copied().collect::<Vec<_>>(),
            closure.selected_native_groups(),
            selection.observation_digest()?,
            self.published_lexical_closure(root)?,
        ))
        .map_err(failure)?;
        Ok(Sha256::digest(bytes).into())
    }

    /// Build the complete published role from this context's own selected
    /// original, native owner and lexical closure. The digest is computed
    /// before construction so no placeholder role is ever admitted.
    fn endorse_published_original(
        &self,
        root: &ExactModuleIdentity,
        revision: &str,
        input_identity: &str,
    ) -> Result<PublishedOriginalEndorsement, CompileError> {
        let metadata = self.compiler_metadata_snapshot()?;
        let entry = metadata
            .entries
            .get(root)
            .ok_or_else(|| failure("published root lacks its issued compiler original"))?;
        if !matches!(entry.payload, ArtifactPayload::Original(_)) {
            return Err(failure("published root has only interface custody"));
        }
        let original = entry.descriptor.id;
        let interface = self
            .owner
            .facts
            .compiler_support
            .projection()
            .roles()
            .into_iter()
            .find(|role| role.original() == Some(original))
            .ok_or_else(|| failure("published root lacks its exact interface role"))?
            .interface();
        let selection = crate::artifact_inventory::ExactArtifactSelection::capture(
            self.owner.facts.compiler_support.artifacts(),
        );
        let native_authority = crate::artifact_inventory::PublicationNativeAuthority::capture(
            &self.owner.facts.compiler_support,
        )?;
        let selection_sha256 = self.published_selection_sha256(
            root,
            &PublishedSelectionFacts {
                interface,
                original,
                source_revision: revision,
                original_input_identity: input_identity,
                selection: &selection,
                native_authority: &native_authority,
            },
        )?;
        Ok(PublishedOriginalEndorsement {
            owner: root.clone(),
            role: CompilerInputRole::PublishedSourceOriginal {
                interface,
                original,
                source_revision: revision.into(),
                original_input_identity: input_identity.into(),
                selection,
                selection_sha256,
                native_authority,
            },
        })
    }

    pub(super) fn validate_published_source_originals(&self) -> Result<(), CompileError> {
        let roles = self
            .owner
            .facts
            .compiler_support
            .projection()
            .roles()
            .into_iter()
            .filter(CompilerInputRole::is_published_source_original)
            .collect::<Vec<_>>();
        if roles.is_empty() {
            return Ok(());
        }
        let metadata = self
            .owner
            .facts
            .compiler_support
            .artifacts()
            .metadata_snapshot();
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
                let facts = PublishedSelectionFacts::from_role(&role)?;
                if self.published_selection_sha256(root, &facts)? != *selection_sha256 {
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
            .owner
            .facts
            .compiler_support
            .projection()
            .roles()
            .iter()
            .any(CompilerInputRole::is_published_source_original)
        {
            return Ok(Vec::new());
        }
        self.normalize()?;
        let metadata = self
            .owner
            .facts
            .compiler_support
            .artifacts()
            .metadata_snapshot();
        self.owner
            .facts
            .compiler_support
            .projection()
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
                Ok(Arc::new(PublishedSourceOriginalSelection {
                    context: Arc::new(context),
                    revision,
                    public_root: root,
                }))
            })
            .collect()
    }

    #[tracing::instrument(
        target = "exomonad_harness::timing", name = "exact.source_publications.for_target",
        level = "debug", skip_all,
        fields(inclusive = true, target_groups = groups.len(), target_imports = imports.len())
    )]
    pub(crate) fn published_source_original_selections_for_target(
        &self,
        groups: &BTreeSet<crate::artifact_inventory::NativeGroupBinding>,
        imports: &[crate::certified_products::PendingImportOwner],
    ) -> Result<Vec<Arc<PublishedSourceOriginalSelection>>, CompileError> {
        let mut timing = crate::timing::InvocationTiming::start_scoped(
            "exact.source_publications.for_target",
            "construct_publications",
        );
        let publications = self.published_source_original_selections()?;
        if publications.is_empty() {
            timing.finish(true);
            return Ok(publications);
        }
        let candidate_count = publications.len();
        timing.advance("target_lexical_selection");
        let compiler = self.compiler_metadata_snapshot()?;
        let mut lexical = BTreeSet::new();
        for import in imports {
            if let crate::certified_products::PendingImportOwner::Source { owner, .. } = import {
                let root = ExactModuleIdentity {
                    unit: owner.unit.clone(),
                    module: owner.module.clone(),
                };
                // Historical native imports may retain a different completed
                // source than the compiler namespace selects for this module.
                // Only the selected full source identity admits a lexical walk.
                if !compiler.entries.get(&root).is_some_and(|entry| {
                    matches!(&entry.payload, ArtifactPayload::Original(product)
                        if product.owner() == owner
                            && !matches!(
                                product.module_interface().origin(),
                                crate::certified_products::CanonicalOrigin::NativeAuthoredDeclaration { .. }
                            ))
                }) {
                    continue;
                }
                // Notebook declaration implementations are native dependencies,
                // not public source lexical carriers. Their issued native group
                // closure still participates in publication selection below.
                lexical.extend(
                    self.published_lexical_closure(&root)?
                        .into_iter()
                        .map(|node| node.owner),
                );
            }
        }
        timing.advance("filter_exact_issuers");
        let mut selected = Vec::with_capacity(publications.len());
        for publication in publications {
            if publication.selects_target(&lexical, groups)? {
                selected.push(publication);
            }
        }
        timing.advance("assemble_selection");
        tracing::debug!(
            target: "exomonad_harness::timing",
            candidate_count, selected_count = selected.len(),
            "source publication target selection"
        );
        timing.finish(true);
        Ok(selected)
    }

    fn published_context_for_root(
        &self,
        root: &ExactModuleIdentity,
        original: ArtifactId,
    ) -> Result<Self, CompileError> {
        let roots = self.published_artifact_roots(root, original)?;
        let lexical = self.published_lexical_closure(root)?;
        let owners = lexical.iter().map(|node| node.owner.clone()).collect();
        let compiler_support = self
            .owner
            .facts
            .compiler_support
            .select_publication_graph(root, roots, &owners)?;
        let source_scope = if self.source_scope().admits(root) {
            self.source_scope()
                .project_roots(std::slice::from_ref(root))?
        } else {
            SourceLexicalScope::default()
        };
        source_scope.preserve_support(self.compiler_source_support(), &compiler_support)?;
        Self::from_facts(DeclarationContextFacts {
            producer: self.owner.facts.producer,
            compiler_support,
            source_scope,
            lexical: self.owner.facts.lexical.replace(lexical)?,
            // Checked values retain their own template custody in the caller's
            // context. A source publication carries its selected source graph.
            template_imports: None,
        })
        .finish()
    }
}

impl CompletedOriginalExecution<'_> {
    pub(crate) fn issue_published_source_original(
        &self,
        revision: &str,
        input: &crate::compile_input::SealedOriginalCompileInput,
        root: &ExactModuleIdentity,
    ) -> Result<Arc<PublishedSourceOriginalSelection>, CompileError> {
        if !Arc::ptr_eq(self.evidence(), &input.issued_original_execution()) {
            return Err(failure(
                "published source input belongs to another original execution",
            ));
        }
        let input_identity = input.original_input_identity();
        let original = self.context().as_ref();
        if !original.source_scope().admits(root) {
            return Err(failure("published root lacks an admitted source scope"));
        }
        let target = self.target();
        if !original
            .owner
            .facts
            .lexical
            .iter()
            .any(|node| &node.owner == target && node.imports.contains(root))
        {
            return Err(failure(
                "published root is not a direct import of the completed source target",
            ));
        }
        let metadata = original.compiler_metadata_snapshot()?;
        let entry = metadata
            .entries
            .get(root)
            .ok_or_else(|| failure("published root lacks an issued compiler original"))?;
        if !matches!(entry.payload, ArtifactPayload::Original(_)) {
            return Err(failure("published root has only interface custody"));
        }
        let mut context = original.published_context_for_root(root, entry.descriptor.id)?;
        if let Some(CompilerInputRole::PublishedSourceOriginal {
            source_revision,
            original_input_identity,
            ..
        }) = context
            .owner
            .facts
            .compiler_support
            .projection()
            .snapshot_roles()
            .get(root)
        {
            if source_revision == revision && original_input_identity == input_identity {
                return Ok(Arc::new(PublishedSourceOriginalSelection {
                    context: Arc::new(context.finish()?),
                    revision: revision.into(),
                    public_root: root.clone(),
                }));
            }
            return Err(failure(
                "publication reissue conflicts with its issued source identity",
            ));
        }
        let endorsement = context.endorse_published_original(root, revision, input_identity)?;
        let updated = context
            .owner
            .facts
            .compiler_support
            .publish_original(&endorsement)?;
        context = context.rebuild(|facts| facts.compiler_support = updated);

        Ok(Arc::new(PublishedSourceOriginalSelection {
            context: Arc::new(context.finish()?),
            revision: revision.into(),
            public_root: root.clone(),
        }))
    }
}

impl ExactDeclarationContext {
    /// Compose the exact issuer-owned public graph and native closure. Identical
    /// selections are idempotent; a conflicting original or policy refuses.
    /// Carry this context into compiler World construction to preserve the
    /// publication's lexical proof alongside its selected compiler roles.
    pub fn with_published_source_originals(
        mut self,
        selection: &PublishedSourceOriginalSelection,
    ) -> Result<Self, CompileError> {
        let incoming = selection.context();
        self = self.admit_producer(incoming.owner.facts.producer)?;
        let lexical = compose_lexical_nodes(
            self.owner
                .facts
                .lexical
                .iter()
                .chain(incoming.owner.facts.lexical.iter()),
        )?;
        let updated = self
            .owner
            .facts
            .compiler_support
            .merge(&incoming.owner.facts.compiler_support)?;
        let source_scope = self
            .source_scope()
            .append_admitted(incoming.source_scope().rows().iter().cloned())?;
        self.source_scope()
            .preserve_support(self.compiler_source_support(), &updated)?;
        incoming
            .source_scope()
            .preserve_support(incoming.compiler_source_support(), &updated)?;
        self = self.rebuild(|facts| {
            facts.compiler_support = updated;
            facts.source_scope = source_scope;
        });
        {
            let updated = self.owner.facts.lexical.replace(lexical)?;
            self = self.rebuild(|facts| facts.lexical = updated);
        }
        self.finish()
    }

    pub(super) fn published_scope_value(&self) -> Result<Value, CompileError> {
        if !self
            .owner
            .facts
            .compiler_support
            .projection()
            .roles()
            .iter()
            .any(CompilerInputRole::is_published_source_original)
        {
            return Ok(Value::Array(vec![]));
        }
        self.validate_published_source_originals()?;
        let metadata = self.compiler_metadata_snapshot()?;
        Ok(Value::Array(
            self.owner
                .facts
                .compiler_support
                .projection()
                .roles()
                .into_iter()
                .filter_map(|role| {
                    let CompilerInputRole::PublishedSourceOriginal {
                        interface,
                        original,
                        source_revision,
                        original_input_identity,
                        selection_sha256,
                        ..
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
                    let owner = &metadata.retained.artifacts[&interface].descriptor.owner;
                    let ArtifactPayload::Original(product) =
                        &metadata.retained.artifacts[&original].payload
                    else {
                        return Err(failure("published scope loses native original"));
                    };
                    let context = self.published_context_for_root(owner, original)?;
                    let selected = context.compiler_metadata_snapshot()?;
                    let dependencies = context
                        .owner
                        .facts
                        .compiler_support
                        .projection()
                        .roles()
                        .into_iter()
                        .filter_map(|role| {
                            let original = role.original()?;
                            let entry = &selected.retained.artifacts[&original];
                            if &entry.descriptor.owner == owner {
                                return None;
                            }
                            let ArtifactPayload::Original(product) = &entry.payload else {
                                return None;
                            };
                            Some(Value::Array(vec![
                                text(&entry.descriptor.owner.unit),
                                text(&entry.descriptor.owner.module),
                                text(hex(&entry.descriptor.interface_sha256)),
                                text(hex(&product.owner().product_sha256)),
                            ]))
                        })
                        .collect();
                    Ok(Value::Array(vec![
                        text(&owner.unit),
                        text(&owner.module),
                        text(hex(&metadata.retained.artifacts[&interface]
                            .descriptor
                            .interface_sha256)),
                        text(hex(&product.owner().product_sha256)),
                        text(revision),
                        text(input),
                        text(hex(&selection)),
                        Value::Array(dependencies),
                    ]))
                })
                .collect::<Result<Vec<_>, CompileError>>()?,
        ))
    }
}

#[cfg(test)]
#[path = "published_source/tests.rs"]
mod tests;

#[cfg(test)]
#[path = "published_source/history_properties.rs"]
mod history_properties;
