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

pub(crate) struct PublishedOriginalEndorsement {
    owner: ExactModuleIdentity,
    role: CompilerInputRole,
}

impl PublishedOriginalEndorsement {
    pub(crate) fn owner(&self) -> &ExactModuleIdentity {
        &self.owner
    }
    pub(crate) fn role(&self) -> &CompilerInputRole {
        &self.role
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
        let issued = roles
            .iter()
            .find_map(|role| match role {
                CompilerInputRole::PublishedSourceOriginal {
                    original: selected,
                    selection,
                    ..
                } if *selected == original => Some(selection.clone()),
                _ => None,
            })
            .unwrap_or_else(|| {
                crate::artifact_inventory::ExactArtifactSelection::capture(
                    &self.owner.facts.compiler_support.artifacts(),
                )
            });
        let issuer = self
            .owner
            .facts
            .compiler_support
            .artifacts()
            .select_issued(vec![original], &issued)?;
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
            if let Some(dependency) = role
                .original()
                .filter(|id| issuer.contains_namespace_artifact(*id))
            {
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
        role: &CompilerInputRole,
    ) -> Result<[u8; 32], CompileError> {
        let CompilerInputRole::PublishedSourceOriginal {
            interface,
            original,
            source_revision,
            original_input_identity,
            selection,
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
        if product.module_interface().is_none_or(|canonical| {
            !matches!(
                canonical.origin(),
                crate::certified_products::CanonicalOrigin::SourceOriginal { .. }
            )
        }) {
            return Err(failure("published original lacks canonical source origin"));
        }
        let compiler = self.compiler_metadata_snapshot()?;
        // Publication selects native roots through compiler roles. Exact child
        // carriers retain executable bytes, including historical versions,
        // without selecting their modules in the compiler namespace.
        for entry in closure
            .root_entries()
            .iter()
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
            "published-source-original3",
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

    pub(crate) fn published_source_original_selections_for_target(
        &self,
        groups: &BTreeSet<crate::artifact_inventory::NativeGroupBinding>,
        imports: &[crate::certified_products::PendingImportOwner],
    ) -> Result<Vec<Arc<PublishedSourceOriginalSelection>>, CompileError> {
        let publications = self.published_source_original_selections()?;
        if publications.is_empty() {
            return Ok(publications);
        }
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
                            && !product.module_interface().is_some_and(|interface| matches!(
                                interface.origin(),
                                crate::certified_products::CanonicalOrigin::NativeAuthoredDeclaration { .. }
                            )))
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
        Ok(publications
            .into_iter()
            .filter(|selection| {
                let issuer = selection.context.artifact_view();
                let roles = selection.context.compiler_input_roles();
                let originals = roles
                    .iter()
                    .filter_map(CompilerInputRole::original)
                    .collect::<BTreeSet<_>>();
                let root_original = roles
                    .iter()
                    .find(|role| role.is_published_source_original())
                    .and_then(CompilerInputRole::original);
                let native_root = root_original.is_some_and(|original| {
                    groups
                        .iter()
                        .any(|group| group.binding.artifact() == original)
                });
                (lexical.contains(selection.public_root()) || native_root)
                    && groups
                        .iter()
                        .filter(|group| originals.contains(&group.binding.artifact()))
                        .all(|group| issuer.contains_issued_native_group(*group))
            })
            .collect())
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
        Self::from_facts(DeclarationContextFacts {
            producer: self.owner.facts.producer,
            compiler_support,
            lexical: self
                .owner
                .facts
                .lexical
                .replace(self.published_lexical_closure(root)?)?,
            original_instance_environment: OriginalInstanceEnvironment::Unknown,
            // Checked values retain their own template custody in the caller's
            // context. A source publication carries its selected source graph.
            template_imports: None,
        })
        .finish()
    }

    pub(crate) fn issue_published_source_original(
        &self,
        revision: &str,
        input_identity: &str,
        root: &ExactModuleIdentity,
    ) -> Result<Arc<PublishedSourceOriginalSelection>, CompileError> {
        let target = self.original_instance_target()?;
        if !self
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
            .owner
            .facts
            .compiler_support
            .projection()
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
            selection: crate::artifact_inventory::ExactArtifactSelection::capture(
                &context.owner.facts.compiler_support.artifacts(),
            ),
            selection_sha256: [0; 32],
        };
        let digest = context.published_selection_sha256(root, &role)?;
        if let CompilerInputRole::PublishedSourceOriginal {
            selection_sha256, ..
        } = &mut role
        {
            *selection_sha256 = digest;
        }
        let endorsement = PublishedOriginalEndorsement {
            owner: root.clone(),
            role,
        };
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

    /// Compose the exact issuer-owned public graph and native closure. Identical
    /// selections are idempotent; a conflicting original or policy refuses.
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
        self = self.rebuild(|facts| facts.compiler_support = updated);
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
mod tests {
    use super::*;
    use crate::artifact_inventory::{NativeArtifactDemand, NativeGroupKey};
    use crate::certified_products::PendingImportOwner;
    use tidepool_repr::execution_schema::SymbolIdentity;

    #[test]
    fn published_source_composition_preserves_selected_issuer_over_historical_custody() {
        published_composition_histories(false, false);
    }

    #[test]
    fn sparse_partial_publication_cannot_promote_unissued_later_group() {
        published_composition_histories(true, false);
    }

    #[test]
    fn interface_adapter_preserves_issued_canonical_scope_and_refuses_unselected_attachment() {
        published_composition_histories(false, true);
    }

    fn check_interface_adapter(
        issued: &ExactDeclarationContext,
        selected_issuer: &ArtifactView,
        original: ArtifactId,
        child: ArtifactId,
    ) {
        use crate::artifact_inventory::{
            ArtifactBindingNode, ArtifactDependency, ExactArtifactSelection,
        };
        let root_owner = identity("fixture", "Root");
        let graph = issued.artifact_view().capture_graph_selection();
        let original_binding = *graph
            .namespace
            .iter()
            .find(|binding| binding.artifact == original)
            .unwrap();
        let canonical =
            issued.compiler_input_projection().snapshot_roles()[&root_owner].interface();
        let canonical_binding = issued
            .artifact_view()
            .binding_dependencies()
            .into_iter()
            .find_map(|(from, to, dependency)| match (from, to, dependency) {
                (
                    ArtifactBindingNode::Artifact(from),
                    ArtifactBindingNode::Artifact(to),
                    ArtifactDependency::Interface,
                ) if from == original_binding && to.artifact == canonical => Some(to),
                _ => None,
            })
            .expect("the selected original owns its exact canonical attachment");
        let types = issued
            .compiler_source_support()
            .selected_interfaces()
            .unwrap()
            .support_for_owners(vec![root_owner.clone()])
            .unwrap();
        let interfaces = types.artifacts();
        assert!(interfaces
            .capture_graph_selection()
            .namespace
            .contains(&canonical_binding));
        assert!(interfaces.selected_native_groups().is_empty());
        let expected_edges = interfaces.binding_dependencies();
        let empty = ExactDeclarationContext::new(&[], &[], vec![]).unwrap();
        let extended = empty
            .clone()
            .extend_issued_compiler_support(&types)
            .expect("the checked-cell interface projection preserves its issuing scope");
        let roles = extended.compiler_input_projection().snapshot_roles();
        assert_eq!(roles[&root_owner].interface(), canonical);
        assert_eq!(roles[&identity("fixture", "Child")].interface(), child);
        assert_eq!(roles.len(), 2);
        assert!(roles.values().all(|role| role.original().is_none()));
        assert!(extended
            .artifact_view()
            .capture_graph_selection()
            .namespace
            .contains(&canonical_binding));
        assert_eq!(
            extended.artifact_view().binding_dependencies(),
            expected_edges
        );
        assert!(extended.artifact_view().selected_native_groups().is_empty());
        assert!(extended.lexical_graph().is_empty());

        // Recovery may transport a narrower namespace without changing any
        // authenticated binding, edge, root or custody fact. An attachment in
        // custody does not make it a selected interface input.
        assert_eq!(
            selected_issuer
                .capture_graph_selection()
                .bindings
                .iter()
                .filter(|binding| binding.artifact == canonical)
                .count(),
            1,
            "the clean issuer negative cannot refuse from canonical binding ambiguity"
        );
        for source in [selected_issuer, issued.artifact_view()] {
            let graph = source.capture_graph_selection();
            let mut original_only = graph.clone();
            original_only
                .namespace
                .retain(|binding| *binding == original_binding);
            let original_only = source
                .select_issued(
                    vec![original],
                    &ExactArtifactSelection::from_recovered_graph_selection(original_only),
                )
                .expect("the owner authenticates the unchanged issued graph");
            let narrowed = original_only.capture_graph_selection();
            assert_eq!(narrowed.namespace, vec![original_binding]);
            assert_eq!(narrowed.bindings, graph.bindings);
            assert_eq!(narrowed.roots, graph.roots);
            assert_eq!(narrowed.custody_roots, graph.custody_roots);
            assert_eq!(narrowed.native_groups, graph.native_groups);
            assert_eq!(
                original_only.binding_dependencies(),
                source.binding_dependencies()
            );
            assert!(narrowed.bindings.contains(&canonical_binding));
            assert!(
                matches!(crate::declaration_context::recover_fixture_compiler_support(
                    types.projection().clone(), original_only, [],
                ), Err(CompileError::ExtractFailed(detail))
                    if detail == "exact declaration context: bound recovery namespace differs from recorded compiler roles"),
                "recorded type roles must refuse a namespace that omits their selected canonical attachment"
            );
        }
    }

    fn assert_sparse_partial_transport(context: &ExactDeclarationContext, original: ArtifactId) {
        let offers = context.compiler_source_support().native_offers();
        let partials = offers
            .iter()
            .filter(|offer| offer.original() == original)
            .filter_map(|offer| match offer.snapshot_facts().body {
                crate::exact_context::NativeOfferSnapshotBody::Partial(selection) => {
                    Some(selection)
                }
                crate::exact_context::NativeOfferSnapshotBody::Full(_) => {
                    panic!("sparse issuance became Full")
                }
                crate::exact_context::NativeOfferSnapshotBody::Empty(_) => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(partials.len(), 1);
        assert_eq!(
            partials[0]
                .graph_selection()
                .native_groups
                .iter()
                .filter(|group| group.binding.artifact() == original)
                .map(|group| group.original_ordinal)
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([7])
        );
        assert!(context
            .artifact_view()
            .capture_graph_selection()
            .native_groups
            .iter()
            .all(|group| group.binding.artifact() != original || group.original_ordinal == 7));
        let transport = NativeCompilerTransport::issue(
            context.compiler_input_projection(),
            &context.compiler_metadata_snapshot().unwrap(),
            context.artifact_view(),
            offers,
        )
        .unwrap();
        let availability = &transport.availability[&original];
        assert!(availability.permits(7));
        assert!(!availability.permits(11));
        assert!(
            matches!(&availability.mode, NativeTransportMode::IssuedGroups(groups)
            if groups == &BTreeSet::from([7]))
        );
    }

    fn published_composition_histories(sparse: bool, interface_adapter_only: bool) {
        use crate::artifact_inventory::{
            ArtifactBindingNode, ArtifactInventoryFailure, IssuedNativeOffer,
        };
        let producer = [2; 32];
        let child_interface = crate::certified_products::fixture_module_interface(
            producer,
            "fixture",
            "Child",
            BTreeMap::new(),
        );
        let child_owner = identity("fixture", "Child");
        let root_owner = identity("fixture", "Root");
        let target = Arc::new(ArtifactEntry::canonical(
            crate::certified_products::fixture_module_interface(
                producer,
                "fixture",
                "Target",
                BTreeMap::new(),
            ),
        ));
        let ordinals = if sparse { [7, 11] } else { [3, 29] };
        let root = |version| {
            let product = crate::certified_products::fixture_finalized_product_with_requirements(
                crate::certified_products::tests::original_groups_fixture_with_interface(
                    "Root",
                    ordinals
                        .into_iter()
                        .map(|ordinal| (ordinal, vec![]))
                        .collect(),
                    version,
                    &BTreeMap::new(),
                    b"published root interface".to_vec(),
                ),
                producer,
                Some(BTreeMap::from([(
                    ("fixture".into(), "Child".into()),
                    child_interface.interface_sha256(),
                )])),
            );
            let product = crate::certified_products::tests::recovered_witness_fixtures(&[product])
                .remove(0)
                .product;
            Arc::new(ArtifactEntry::original(producer, product).unwrap())
        };
        let native = root(1);
        let lexical = vec![
            ExactLexicalNode {
                owner: target.descriptor.owner.clone(),
                imports: vec![root_owner.clone()],
            },
            ExactLexicalNode {
                owner: root_owner.clone(),
                imports: vec![child_owner.clone()],
            },
            ExactLexicalNode {
                owner: child_owner.clone(),
                imports: vec![],
            },
        ];
        let mut failures = Vec::new();
        let mut completed = 0;
        for changed_child in [false, true] {
            let children = [
                Arc::new(ArtifactEntry::canonical(child_interface.clone())),
                Arc::new(ArtifactEntry::canonical(if changed_child {
                    crate::certified_products::fixture_module_core(
                        &child_interface,
                        b"next Child Core".to_vec(),
                    )
                } else {
                    child_interface.clone()
                })),
            ];
            assert_eq!(
                children[0].descriptor.interface_sha256,
                children[1].descriptor.interface_sha256
            );
            assert_eq!(
                children[0].descriptor.id != children[1].descriptor.id,
                changed_child
            );
            for reverse in [false, true] {
                let views = children
                    .iter()
                    .enumerate()
                    .map(|(index, child)| {
                        let inventory = ArtifactInventory::default();
                        let sibling = Arc::new(ArtifactEntry::canonical(
                            crate::certified_products::fixture_module_interface(
                                producer,
                                "fixture",
                                ["Left", "Right"][index],
                                BTreeMap::new(),
                            ),
                        ));
                        let mut entries =
                            vec![native.clone(), child.clone(), target.clone(), sibling];
                        if reverse {
                            entries.reverse();
                        }
                        if sparse {
                            inventory
                                .admit_recovery_selection(
                                    &inventory.empty_view(),
                                    entries,
                                    &BTreeSet::from([NativeGroupKey {
                                        artifact: native.descriptor.id,
                                        original_ordinal: 7,
                                    }]),
                                )
                                .unwrap()
                        } else {
                            inventory
                                .admit_shared(&inventory.empty_view(), entries)
                                .unwrap()
                        }
                    })
                    .collect::<Vec<_>>();
                let binding = |index: usize| {
                    *views[index]
                        .capture_graph_selection()
                        .namespace
                        .iter()
                        .find(|binding| binding.artifact == native.descriptor.id)
                        .unwrap()
                };
                assert_ne!(
                    binding(0),
                    binding(1),
                    "equal native payloads retain distinct issuers"
                );
                let edges = views
                    .iter()
                    .flat_map(|view| view.binding_dependencies())
                    .filter(|(from, to, _)| {
                        matches!(
                            (from, to),
                            (
                                ArtifactBindingNode::Artifact(_),
                                ArtifactBindingNode::Artifact(_)
                            )
                        )
                    })
                    .collect::<BTreeSet<_>>();
                for selected in 0..2 {
                    let projection = CompilerInputProjection::from_issued_entries(&[
                        native.clone(),
                        children[selected].clone(),
                        target.clone(),
                    ])
                    .unwrap();
                    let old =
                        IssuedNativeOffer::issue(native.descriptor.id, &views[1 - selected], None)
                            .unwrap();
                    let support = crate::declaration_context::recover_fixture_compiler_support(
                        projection.clone(),
                        views[selected].clone(),
                        [old.clone(), old],
                    )
                    .unwrap();
                    let issued = ExactDeclarationContext::from_authenticated_execution(
                        producer,
                        &views[selected],
                        projection,
                        lexical.clone(),
                        target.descriptor.owner.clone(),
                        &[
                            target.descriptor.owner.clone(),
                            root_owner.clone(),
                            child_owner.clone(),
                        ],
                    )
                    .unwrap()
                    .extend_issued_compiler_support(&support)
                    .unwrap();
                    if interface_adapter_only {
                        check_interface_adapter(
                            &issued,
                            &views[selected],
                            native.descriptor.id,
                            children[selected].descriptor.id,
                        );
                        completed += 1;
                        continue;
                    }
                    let publication = issued
                        .issue_published_source_original("revision", "input", &root_owner)
                        .expect("the completed target directly imported the native root");
                    if sparse {
                        assert_sparse_partial_transport(
                            publication.context(),
                            native.descriptor.id,
                        );
                    }
                    let publication_sha = publication.context().semantic_sha256();
                    let publication_graph = publication
                        .context()
                        .artifact_view()
                        .capture_graph_selection();
                    let published_role = publication
                        .context()
                        .compiler_input_projection()
                        .snapshot_roles()[&root_owner]
                        .clone();
                    assert!(published_role.is_published_source_original());
                    assert_eq!(
                        publication
                            .context()
                            .compiler_input_projection()
                            .snapshot_roles()[&child_owner]
                            .interface(),
                        children[selected].descriptor.id
                    );
                    assert!(
                        edges.iter().all(|edge| publication
                            .context()
                            .artifact_view()
                            .binding_dependencies()
                            .contains(edge)),
                        "the publication retains both exact historical child graphs"
                    );
                    let expected_groups = views[selected].selected_native_groups();
                    for start_empty in [false, true] {
                        let mut receiver = if start_empty {
                            ExactDeclarationContext::new(&[], &[], vec![]).unwrap()
                        } else {
                            issued.clone()
                        };
                        let initial = receiver.semantic_sha256();
                        let mut accepted = true;
                        for repetition in 0..3 {
                            match receiver
                                .clone()
                                .with_published_source_originals(&publication)
                            {
                                Ok(next) => {
                                    if sparse {
                                        assert_sparse_partial_transport(
                                            &next,
                                            native.descriptor.id,
                                        );
                                    }
                                    assert_eq!(
                                        next.compiler_input_projection().snapshot_roles()
                                            [&root_owner],
                                        published_role
                                    );
                                    assert_eq!(
                                        next.compiler_input_projection().snapshot_roles()
                                            [&child_owner]
                                            .interface(),
                                        children[selected].descriptor.id
                                    );
                                    assert!(next
                                        .compiler_input_projection()
                                        .snapshot_roles()
                                        .keys()
                                        .all(|owner| owner == &root_owner
                                            || owner == &child_owner
                                            || owner == &target.descriptor.owner));
                                    assert_eq!(
                                        next.artifact_view().selected_native_groups(),
                                        expected_groups
                                    );
                                    assert!(edges.iter().all(|edge| next
                                        .artifact_view()
                                        .binding_dependencies()
                                        .contains(edge)));
                                    if repetition > 0 {
                                        assert_eq!(
                                            next.semantic_sha256(),
                                            receiver.semantic_sha256(),
                                            "reimport is idempotent"
                                        );
                                    }
                                    receiver = next;
                                }
                                Err(error) => {
                                    failures.push(format!("changed_child={changed_child} reverse={reverse} selected={selected} start_empty={start_empty} repetition={repetition}: published consumer: {error}"));
                                    if repetition == 0 {
                                        assert_eq!(receiver.semantic_sha256(), initial);
                                    }
                                    accepted = false;
                                    break;
                                }
                            }
                        }
                        completed += usize::from(accepted);
                    }
                    assert_eq!(publication.context().semantic_sha256(), publication_sha);
                    assert_eq!(
                        publication
                            .context()
                            .artifact_view()
                            .capture_graph_selection(),
                        publication_graph
                    );
                    if changed_child {
                        let competing_projection = CompilerInputProjection::from_issued_entries(&[
                            native.clone(),
                            children[1 - selected].clone(),
                            target.clone(),
                        ])
                        .unwrap();
                        let competing = ExactDeclarationContext::from_authenticated_execution(
                            producer,
                            &views[1 - selected],
                            competing_projection,
                            lexical.clone(),
                            target.descriptor.owner.clone(),
                            &[],
                        )
                        .unwrap();
                        assert!(
                            matches!(competing.with_published_source_originals(&publication),
                            Err(CompileError::ArtifactInventory(error))
                            if matches!(error.failure, ArtifactInventoryFailure::OwnerConflict { .. })),
                            "different selected canonical roles must refuse"
                        );
                    }
                    let alternative = issued
                        .issue_published_source_original("other revision", "input", &root_owner)
                        .unwrap();
                    assert!(
                        publication
                            .context()
                            .as_ref()
                            .clone()
                            .with_published_source_originals(&alternative)
                            .is_err(),
                        "different publication policies must refuse"
                    );
                    let conflicting_root = root(2);
                    assert_ne!(conflicting_root.descriptor.id, native.descriptor.id);
                    let conflict_inventory = ArtifactInventory::default();
                    let conflict_view = conflict_inventory
                        .admit_shared(
                            &conflict_inventory.empty_view(),
                            vec![
                                conflicting_root.clone(),
                                children[selected].clone(),
                                target.clone(),
                            ],
                        )
                        .unwrap();
                    let conflict_projection = CompilerInputProjection::from_issued_entries(&[
                        conflicting_root,
                        children[selected].clone(),
                        target.clone(),
                    ])
                    .unwrap();
                    let conflicting = ExactDeclarationContext::from_authenticated_execution(
                        producer,
                        &conflict_view,
                        conflict_projection,
                        lexical.clone(),
                        target.descriptor.owner.clone(),
                        &[],
                    )
                    .unwrap();
                    assert!(
                        matches!(conflicting.with_published_source_originals(&publication),
                        Err(CompileError::ArtifactInventory(error))
                        if matches!(error.failure, ArtifactInventoryFailure::CompilerOriginalOfferConflict { .. })),
                        "different selected native originals must refuse"
                    );
                }
            }
        }
        eprintln!(
            "published composition histories: completed={completed} failed={}\n{}",
            failures.len(),
            failures.join("\n")
        );
        assert!(
            failures.is_empty(),
            "published composition rejected an issuer-preserving history"
        );
        assert_eq!(completed, if interface_adapter_only { 8 } else { 16 });
    }

    #[test]
    fn authored_certifier_keeps_published_subset_policy_separate_from_complete_demand() {
        let producer = [2; 32];
        let product = crate::certified_products::tests::recovered_witness_fixtures(&[
            crate::certified_products::fixture_finalized_product(
                crate::certified_products::tests::original_groups_fixture_with_interface(
                    "PublishedSubset",
                    vec![(3, vec![]), (29, vec![])],
                    1,
                    &BTreeMap::new(),
                    b"published interface".to_vec(),
                ),
                producer,
            ),
        ])
        .remove(0)
        .product;
        let root = Arc::new(ArtifactEntry::original(producer, product).unwrap());
        let target = Arc::new(ArtifactEntry::canonical(
            crate::certified_products::fixture_module_interface(
                producer,
                "fixture",
                "Target",
                BTreeMap::new(),
            ),
        ));
        let inventory = ArtifactInventory::default();
        let early = NativeGroupKey {
            artifact: root.descriptor.id,
            original_ordinal: 3,
        };
        let view = inventory
            .admit_recovery_selection(
                &inventory.empty_view(),
                vec![root.clone(), target.clone()],
                &BTreeSet::from([early]),
            )
            .unwrap();
        let projection =
            CompilerInputProjection::from_issued_entries(&[root.clone(), target.clone()]).unwrap();
        let issued = ExactDeclarationContext::from_authenticated_execution(
            producer,
            &view,
            projection.clone(),
            vec![
                ExactLexicalNode {
                    owner: target.descriptor.owner.clone(),
                    imports: vec![root.descriptor.owner.clone()],
                },
                ExactLexicalNode {
                    owner: root.descriptor.owner.clone(),
                    imports: vec![],
                },
            ],
            target.descriptor.owner.clone(),
            &[
                target.descriptor.owner.clone(),
                root.descriptor.owner.clone(),
            ],
        )
        .unwrap();
        let publication = issued
            .issue_published_source_original("revision", "input", &root.descriptor.owner)
            .unwrap();
        let published = publication.context();
        let before = published.artifact_view().capture_graph_selection();
        let before_sha = published.semantic_sha256();
        let complete_inventory = ArtifactInventory::default();
        let complete = complete_inventory
            .admit_shared(&complete_inventory.empty_view(), vec![root.clone(), target])
            .unwrap();
        let selection =
            crate::certified_products::CertifiedSourceSelection::from_compiler_projection(
                &projection,
                &complete.metadata_snapshot(),
                &tidepool_repr::execution_schema::InventoryOperation::new(Default::default()),
            )
            .unwrap();
        let originals = selection.selected_original_closure(&complete).unwrap();
        let original_graph = originals.native_closure().capture_graph_selection();
        let role =
            published.compiler_input_projection().snapshot_roles()[&root.descriptor.owner].clone();
        let policy =
            crate::artifact_inventory::NativeOriginalRequirement::from_role(role.clone()).unwrap();
        assert_eq!(
            policy
                .selection
                .graph_selection()
                .roots
                .iter()
                .filter(|node| matches!(
                    node,
                    crate::artifact_inventory::ArtifactBindingNode::Group(_)
                ))
                .count(),
            1
        );
        assert_eq!(originals.native_closure().selected_native_groups().len(), 2);
        let legacy_incoming = published
            .compiler_input_projection()
            .within_view(originals.native_closure());
        let (policy_only, _) = originals
            .native_closure()
            .merge_fixture_compiler_inputs(
                &legacy_incoming,
                published.artifact_view(),
                published.compiler_input_projection(),
            )
            .unwrap();
        assert_eq!(
            policy_only.selected_native_groups(),
            BTreeSet::from([early]),
            "role-policy merge alone loses the complete closure's second group"
        );
        let support = certified_artifact_view(&originals, published).unwrap();
        let demanded = (**published)
            .clone()
            .rebuild(|facts| facts.compiler_support = support.clone());
        let extended = demanded
            .clone()
            .extend_interface_context(&demanded)
            .unwrap();
        assert_eq!(extended.artifact_view().capture_graph_selection().roots,
            demanded.artifact_view().capture_graph_selection().roots,
            "type evidence must preserve the existing complete demand independently of published subset policy");

        assert_eq!(
            support.projection().snapshot_roles()[&root.descriptor.owner],
            role
        );
        for group in original_graph.roots.iter().filter(|node| {
            matches!(
                node,
                crate::artifact_inventory::ArtifactBindingNode::Group(_)
            )
        }) {
            assert!(
                support
                    .artifacts()
                    .capture_graph_selection()
                    .roots
                    .contains(group),
                "published policy replaced the complete issued demand"
            );
        }
        assert!(originals
            .native_closure()
            .binding_dependencies()
            .iter()
            .all(|edge| support.artifacts().binding_dependencies().contains(edge)));
        assert_eq!(published.artifact_view().capture_graph_selection(), before);
        assert_eq!(
            published.semantic_sha256(),
            before_sha,
            "joining custody must not mutate the immutable publication"
        );
        for shared_type in [false, true] {
            for fresh_group_count in [1, 2, 3] {
                let fresh = crate::certified_products::tests::recovered_witness_fixtures(&[
                    crate::certified_products::fixture_finalized_product_with_requirements(
                        crate::certified_products::tests::original_groups_fixture_with_interface(
                            "UnrelatedAuthored",
                            (0..fresh_group_count)
                                .map(|index| (7 + index * 10, vec![]))
                                .collect(),
                            2 + fresh_group_count as u8,
                            &BTreeMap::new(),
                            b"fresh authored interface".to_vec(),
                        ),
                        producer,
                        Some(if shared_type {
                            BTreeMap::from([(
                                (
                                    root.descriptor.owner.unit.clone(),
                                    root.descriptor.owner.module.clone(),
                                ),
                                root.descriptor.interface_sha256,
                            )])
                        } else {
                            BTreeMap::new()
                        }),
                    ),
                ])
                .remove(0)
                .product;
                let fresh_entry = Arc::new(ArtifactEntry::original(producer, fresh).unwrap());
                let ArtifactPayload::Original(root_product) = &root.payload else {
                    unreachable!()
                };
                let root_interface = Arc::new(ArtifactEntry::canonical(
                    root_product.module_interface().unwrap().clone(),
                ));
                let fresh_inventory = ArtifactInventory::default();
                let fresh_view = fresh_inventory
                    .admit_shared(
                        &fresh_inventory.empty_view(),
                        vec![fresh_entry.clone(), root_interface.clone()],
                    )
                    .unwrap();
                let fresh_projection =
                    CompilerInputProjection::from_issued_entries(&[fresh_entry, root_interface])
                        .unwrap();
                let fresh_selection =
                    crate::certified_products::CertifiedSourceSelection::from_compiler_projection(
                        &fresh_projection,
                        &fresh_view.metadata_snapshot(),
                        &tidepool_repr::execution_schema::InventoryOperation::new(
                            Default::default(),
                        ),
                    )
                    .unwrap();
                let fresh_originals = fresh_selection
                    .selected_original_closure(&fresh_view)
                    .unwrap();
                let baseline = published
                    .as_ref()
                    .clone()
                    .extend_issued_compiler_support(
                        &crate::declaration_context::issue_fixture_compiler_support(
                            fresh_projection,
                            fresh_view,
                        )
                        .unwrap(),
                    )
                    .unwrap();
                assert!(
                    baseline
                        .compiler_input_projection()
                        .within_view(fresh_originals.native_closure())
                        .validate(fresh_originals.native_closure())
                        .is_err(),
                    "within_view must not silently erase missing published policy"
                );
                let certified = certified_artifact_view(&fresh_originals, &baseline).unwrap();
                assert_eq!(
                    certified.artifacts().selected_native_groups().len(),
                    fresh_group_count as usize + 1
                );
                for selected in fresh_originals
                    .native_closure()
                    .capture_graph_selection()
                    .roots
                    .iter()
                    .filter(|node| {
                        matches!(
                            node,
                            crate::artifact_inventory::ArtifactBindingNode::Group(_)
                        )
                    })
                {
                    assert!(certified
                        .artifacts()
                        .capture_graph_selection()
                        .roots
                        .contains(selected));
                }
                assert_eq!(
                    certified.projection().snapshot_roles()[&root.descriptor.owner],
                    role
                );
                assert!(certified
                    .artifacts()
                    .capture_graph_selection()
                    .roots
                    .contains(&crate::artifact_inventory::ArtifactBindingNode::Group(
                        *policy
                            .selection
                            .graph_selection()
                            .native_groups
                            .first()
                            .unwrap()
                    )));
            }
        }
    }

    #[test]
    fn published_native_custody_does_not_require_dependency_namespace_originals() {
        let producer = [2; 32];
        let native = |module: &str, groups, version, requirements| {
            Arc::new(
                ArtifactEntry::original(
                    producer,
                    crate::certified_products::tests::recovered_witness_fixtures(&[
                        crate::certified_products::fixture_finalized_product_with_requirements(
                            crate::certified_products::tests::original_groups_fixture_with_interface(
                                module,
                                groups,
                                version,
                                &BTreeMap::new(),
                                format!("interface-{module}").into_bytes(),
                            ),
                            producer,
                            Some(requirements),
                        ),
                    ])
                    .remove(0)
                    .product,
                )
                .unwrap(),
            )
        };
        let source = |entry: &ArtifactEntry, ordinal, occurrence: &str| {
            let ArtifactPayload::Original(product) = &entry.payload else {
                unreachable!()
            };
            PendingImportOwner::Source {
                owner: product.owner().clone(),
                original_ordinal: ordinal,
                binder: SymbolIdentity {
                    unit: product.owner().unit.clone(),
                    module: product.owner().module.clone(),
                    namespace: "value".into(),
                    occurrence: occurrence.into(),
                    record_parent: None,
                },
            }
        };
        let leaf = native("Leaf", vec![(7, vec![])], 1, BTreeMap::new());
        let helper_requirements = BTreeMap::from([(
            ("fixture".into(), "Leaf".into()),
            leaf.descriptor.interface_sha256,
        )]);
        let old = native(
            "Helper",
            vec![(7, vec![source(&leaf, 7, "entry")])],
            1,
            helper_requirements.clone(),
        );
        let current = native(
            "Helper",
            vec![(7, vec![source(&leaf, 7, "entry")])],
            2,
            helper_requirements.clone(),
        );
        let future = native(
            "Helper",
            vec![(7, vec![source(&leaf, 7, "entry")])],
            3,
            helper_requirements,
        );
        let root = native(
            "Root",
            vec![(3, vec![]), (29, vec![source(&old, 7, "entry")])],
            1,
            BTreeMap::from([(
                ("fixture".into(), "Helper".into()),
                old.descriptor.interface_sha256,
            )]),
        );
        let target = Arc::new(ArtifactEntry::canonical(
            crate::certified_products::fixture_module_interface(
                producer,
                "fixture",
                "Target",
                BTreeMap::new(),
            ),
        ));
        let ArtifactPayload::Original(old_product) = &old.payload else {
            unreachable!()
        };
        let helper_interface = Arc::new(ArtifactEntry::canonical(
            old_product.module_interface().unwrap().clone(),
        ));
        let ArtifactPayload::Original(leaf_product) = &leaf.payload else {
            unreachable!()
        };
        let leaf_interface = Arc::new(ArtifactEntry::canonical(
            leaf_product.module_interface().unwrap().clone(),
        ));
        let early = NativeGroupKey {
            artifact: root.descriptor.id,
            original_ordinal: 3,
        };
        for current_original_role in [false, true] {
            let inventory = ArtifactInventory::default();
            let view = inventory
                .admit_recovery_selection(
                    &inventory.empty_view(),
                    vec![
                        root.clone(),
                        old.clone(),
                        current.clone(),
                        leaf.clone(),
                        target.clone(),
                    ],
                    &BTreeSet::from([early]),
                )
                .unwrap();
            let projection = CompilerInputProjection::from_issued_entries(&[
                root.clone(),
                if current_original_role {
                    current.clone()
                } else {
                    helper_interface.clone()
                },
                leaf_interface.clone(),
                target.clone(),
            ])
            .unwrap();
            let issued = ExactDeclarationContext::from_authenticated_execution(
                producer,
                &view,
                projection,
                vec![
                    ExactLexicalNode {
                        owner: target.descriptor.owner.clone(),
                        imports: vec![root.descriptor.owner.clone()],
                    },
                    ExactLexicalNode {
                        owner: root.descriptor.owner.clone(),
                        imports: vec![old.descriptor.owner.clone()],
                    },
                    ExactLexicalNode {
                        owner: old.descriptor.owner.clone(),
                        imports: vec![leaf.descriptor.owner.clone()],
                    },
                    ExactLexicalNode {
                        owner: leaf.descriptor.owner.clone(),
                        imports: vec![],
                    },
                ],
                target.descriptor.owner.clone(),
                &[
                    target.descriptor.owner.clone(),
                    root.descriptor.owner.clone(),
                    old.descriptor.owner.clone(),
                    leaf.descriptor.owner.clone(),
                ],
            )
            .unwrap();
            let publication = issued
                .issue_published_source_original("revision", "input", &root.descriptor.owner)
                .unwrap();
            let published = publication.context();
            assert!(published
                .artifact_view()
                .artifact_ids()
                .contains(&old.descriptor.id));
            assert!(published
                .artifact_view()
                .artifact_ids()
                .contains(&leaf.descriptor.id));
            assert_eq!(
                published
                    .artifact_view()
                    .artifact_ids()
                    .contains(&current.descriptor.id),
                current_original_role
            );
            assert_eq!(
                published.artifact_view().selected_native_groups(),
                BTreeSet::from([early])
            );
            let helper_selected =
                &published.compiler_metadata_snapshot().unwrap().entries[&old.descriptor.owner];
            if current_original_role {
                assert_eq!(helper_selected.descriptor.id, current.descriptor.id);
            } else {
                assert!(matches!(
                    helper_selected.payload,
                    ArtifactPayload::Canonical(_)
                ));
            }
            assert!(matches!(
                published.compiler_metadata_snapshot().unwrap().entries[&leaf.descriptor.owner]
                    .payload,
                ArtifactPayload::Canonical(_)
            ));
            let composed = ExactDeclarationContext::new(&[], &[], vec![])
                .unwrap()
                .with_published_source_originals(&publication)
                .unwrap();
            assert_eq!(composed.semantic_sha256(), published.semantic_sha256());
            let reopened = published.published_source_original_selections().unwrap();
            assert_eq!(
                reopened[0].context().semantic_sha256(),
                published.semantic_sha256()
            );
            let admitted = published
                .artifact_view()
                .inventory()
                .admit_shared_with_demand(
                    published.artifact_view(),
                    vec![root.clone()],
                    NativeArtifactDemand::CertifiedTargetImports(&[source(&root, 29, "entry_29")]),
                )
                .unwrap();
            assert_eq!(
                admitted.selected_native_groups(),
                BTreeSet::from([
                    early,
                    NativeGroupKey {
                        artifact: root.descriptor.id,
                        original_ordinal: 29
                    },
                    NativeGroupKey {
                        artifact: old.descriptor.id,
                        original_ordinal: 7
                    },
                    NativeGroupKey {
                        artifact: leaf.descriptor.id,
                        original_ordinal: 7
                    },
                ])
            );
            // A later exact output retains the original compiler roles while
            // selecting a previously unused body of the published module.
            let continued = ExactDeclarationContext::from_authenticated_execution(
                producer,
                &admitted,
                published.owner.facts.compiler_support.projection().clone(),
                published.owner.facts.lexical.to_vec(),
                root.descriptor.owner.clone(),
                &[root.descriptor.owner.clone()],
            )
            .expect("later native demand preserves the issued published selection");
            let reopened_after_demand = continued.published_source_original_selections().unwrap();
            assert_eq!(
                reopened_after_demand[0].context().semantic_sha256(),
                published.semantic_sha256(),
                "publication keeps its original selection independently of later demand"
            );
            let ambient = inventory
                .admit_shared_with_demand(
                    &admitted,
                    vec![future.clone()],
                    NativeArtifactDemand::ScopeInterfaces,
                )
                .unwrap();
            let with_ambient = ExactDeclarationContext::from_authenticated_execution(
                producer,
                &ambient,
                published.owner.facts.compiler_support.projection().clone(),
                published.owner.facts.lexical.to_vec(),
                root.descriptor.owner.clone(),
                &[root.descriptor.owner.clone()],
            )
            .unwrap();
            let frozen = with_ambient.published_source_original_selections().unwrap();
            assert_eq!(
                frozen[0].context().semantic_sha256(),
                published.semantic_sha256()
            );
            assert!(!frozen[0]
                .context()
                .artifact_view()
                .artifact_ids()
                .contains(&future.descriptor.id));
            let mut replaced = published.compiler_input_roles();
            let future_role = CompilerInputProjection::from_issued_entries(&[future.clone()])
                .unwrap()
                .roles()
                .remove(0);
            let position = replaced
                .iter()
                .position(|role| role.interface() == helper_interface.descriptor.id)
                .unwrap();
            replaced[position] = future_role;
            let replacement =
                CompilerInputProjection::restore(&ambient, &replaced).and_then(|projection| {
                    ExactDeclarationContext::from_authenticated_execution(
                        producer,
                        &ambient,
                        projection,
                        published.owner.facts.lexical.to_vec(),
                        root.descriptor.owner.clone(),
                        &[root.descriptor.owner.clone()],
                    )
                });
            if current_original_role {
                assert!(
                    replacement.is_err(),
                    "later compiler namespace replacement cannot rewrite the publication issuer"
                );
            } else {
                let reopened = replacement
                    .unwrap()
                    .published_source_original_selections()
                    .unwrap();
                assert_eq!(
                    reopened[0].context().semantic_sha256(),
                    published.semantic_sha256()
                );
                assert!(!reopened[0]
                    .context()
                    .artifact_view()
                    .artifact_ids()
                    .contains(&future.descriptor.id));
            }
            let recovery_scratch = tempfile::tempdir().unwrap();
            let (published_products, published_interfaces) = published
                .materialize_recovery_products_and_interfaces(recovery_scratch.path())
                .unwrap();
            let selection: crate::artifact_inventory::ArtifactGraphSelection =
                serde_json::from_slice(
                    &serde_json::to_vec(&published.artifact_view().capture_graph_selection())
                        .unwrap(),
                )
                .unwrap();
            let edges: Vec<_> = serde_json::from_slice(
                &serde_json::to_vec(&published.artifact_view().binding_dependencies()).unwrap(),
            )
            .unwrap();
            let recovery_inventory = RecoveredArtifactInventory::capture_bound(
                recovery_scratch.path(),
                &published_products,
                &published_interfaces,
                &[],
                &[],
                &published.artifact_view().descriptors(),
                &selection.bindings,
                &edges,
            )
            .unwrap();
            assert!(
                recovery_inventory
                    .context_with_roles(
                        &published.artifact_view().artifact_ids(),
                        &published
                            .artifact_view()
                            .selected_native_groups()
                            .into_iter()
                            .collect::<Vec<_>>(),
                        &published.compiler_input_roles(),
                        published.owner.facts.lexical.to_vec(),
                    )
                    .is_err(),
                "flat recovery cannot recreate an issuer's exact bound selection"
            );
            let recovered = recovery_inventory
                .context_with_graph_selection(
                    &selection,
                    &published.compiler_input_roles(),
                    published.owner.facts.lexical.to_vec(),
                )
                .unwrap();
            assert_eq!(recovered.semantic_sha256(), published.semantic_sha256());
            for snapshot in [published.as_ref(), &continued] {
                let scratch = tempfile::tempdir().unwrap();
                let products = recovery_artifacts::materialize_certified_products(
                    scratch.path(),
                    producer,
                    &snapshot.recovery_products(),
                )
                .unwrap();
                let interfaces = snapshot
                    .materialize_module_interfaces(scratch.path())
                    .unwrap();
                let reused_root = tempfile::tempdir().unwrap();
                let (reused_products, reused_interfaces) = snapshot
                    .materialize_recovery_products_and_interfaces(reused_root.path())
                    .unwrap();
                assert_eq!(
                    serde_json::to_value(&reused_products).unwrap(),
                    serde_json::to_value(&products).unwrap()
                );
                assert_eq!(
                    serde_json::to_value(&reused_interfaces).unwrap(),
                    serde_json::to_value(&interfaces).unwrap()
                );
                for interface in &reused_interfaces {
                    assert!(reused_root
                        .path()
                        .join(&interface.certificate_path)
                        .is_file());
                    assert!(reused_root
                        .path()
                        .join(&interface.interface.interface_path)
                        .is_file());
                }
                let roles: Vec<CompilerInputRole> = serde_json::from_slice(
                    &serde_json::to_vec(&snapshot.compiler_input_roles()).unwrap(),
                )
                .unwrap();
                let snapshot_selection: crate::artifact_inventory::ArtifactGraphSelection =
                    serde_json::from_slice(
                        &serde_json::to_vec(&snapshot.artifact_view().capture_graph_selection())
                            .unwrap(),
                    )
                    .unwrap();
                let snapshot_edges: Vec<_> = serde_json::from_slice(
                    &serde_json::to_vec(&snapshot.artifact_view().binding_dependencies()).unwrap(),
                )
                .unwrap();
                let durable = RecoveredArtifactInventory::capture_bound(
                    scratch.path(),
                    &products,
                    &interfaces,
                    &[],
                    &[],
                    &snapshot.artifact_view().descriptors(),
                    &snapshot_selection.bindings,
                    &snapshot_edges,
                )
                .unwrap()
                .context_with_graph_selection(
                    &snapshot_selection,
                    &roles,
                    snapshot.owner.facts.lexical.to_vec(),
                )
                .unwrap();
                assert_eq!(durable.artifact_view(), snapshot.artifact_view());
                assert_eq!(
                    durable.compiler_input_roles(),
                    snapshot.compiler_input_roles()
                );
                assert_eq!(durable.lexical_graph(), snapshot.lexical_graph());
                assert_eq!(
                    durable.original_instance_environment(),
                    &OriginalInstanceEnvironment::Unknown,
                    "native recovery does not reconstruct a completed source-compilation proof"
                );
                assert_eq!(
                    durable.published_source_original_selections().unwrap()[0]
                        .context()
                        .semantic_sha256(),
                    published.semantic_sha256(),
                    "durable continuation retains the original publication selection",
                );
            }
            let mut missing = recovery_inventory;
            missing.entries.remove(&old.descriptor.id);
            assert!(
                missing
                    .context_with_graph_selection(
                        &selection,
                        &published.compiler_input_roles(),
                        published.owner.facts.lexical.to_vec(),
                    )
                    .is_err(),
                "recovery cannot replace missing exact child custody with its canonical interface"
            );
        }
    }
}

#[cfg(test)]
#[path = "published_source/history_properties.rs"]
mod history_properties;
