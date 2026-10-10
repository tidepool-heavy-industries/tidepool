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
            .inventory
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
        let roots = self.published_artifact_roots(root, original)?;
        let inventory =
            match self.compiler_projection.roles().iter().find(|role| {
                role.original() == Some(original) && role.is_published_source_original()
            }) {
                Some(CompilerInputRole::PublishedSourceOriginal { selection, .. }) => {
                    self.inventory.select_issued(roots, selection)?
                }
                _ => self.inventory.select_roots(roots)?,
            };
        let ids = inventory
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
        Ok(Self {
            producer: self.producer,
            compiler_projection: CompilerInputProjection::restore(&inventory, &roles)?,
            inventory,
            lexical: self.published_lexical_closure(root)?,
            original_instance_environment: OriginalInstanceEnvironment::Unknown,
            // Checked values retain their own template custody in the caller's
            // context. A source publication carries its selected source graph.
            template_imports: None,
        })
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
            selection: crate::artifact_inventory::ExactArtifactSelection::capture(
                &context.inventory,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifact_inventory::{NativeArtifactDemand, NativeGroupKey};
    use crate::certified_products::PendingImportOwner;
    use tidepool_repr::execution_schema::SymbolIdentity;

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
            .unwrap()
            .with_compiler_input_projection(projection)
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
            assert!(!published
                .artifact_view()
                .artifact_ids()
                .contains(&current.descriptor.id));
            assert_eq!(
                published.artifact_view().selected_native_groups(),
                BTreeSet::from([early])
            );
            assert!(matches!(
                published.compiler_metadata_snapshot().unwrap().entries[&old.descriptor.owner]
                    .payload,
                ArtifactPayload::Canonical(_)
            ));
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
                published.lexical.clone(),
                root.descriptor.owner.clone(),
                &[root.descriptor.owner.clone()],
            )
            .unwrap()
            .with_compiler_input_projection(published.compiler_projection.clone())
            .expect("later native demand preserves the issued published selection");
            let reopened_after_demand = continued.published_source_original_selections().unwrap();
            assert_eq!(
                reopened_after_demand[0].context().semantic_sha256(),
                published.semantic_sha256(),
                "publication keeps its original selection independently of later demand"
            );
            let recovery_inventory = RecoveredArtifactInventory {
                producer,
                entries: published
                    .artifact_view()
                    .entries()
                    .into_iter()
                    .map(|entry| (entry.descriptor.id, entry))
                    .collect(),
                recorded_inventory: true,
                interfaces: published.artifact_view().interface_dependencies(),
            };
            let recovered = recovery_inventory
                .context_with_published_roles(
                    &published.artifact_view().artifact_ids(),
                    &published
                        .artifact_view()
                        .selected_native_groups()
                        .into_iter()
                        .collect::<Vec<_>>(),
                    &published.compiler_input_roles(),
                    published.lexical.clone(),
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
                let durable = ExactDeclarationContext::capture_recovery_with_inventory(
                    scratch.path(),
                    &products,
                    &interfaces,
                    &[],
                    &[],
                    &snapshot.artifact_view().descriptors(),
                    &snapshot.artifact_view().interface_dependencies(),
                    &snapshot
                        .artifact_view()
                        .selected_native_groups()
                        .into_iter()
                        .collect::<Vec<_>>(),
                    &roles,
                    snapshot.lexical.clone(),
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
                    .context_with_published_roles(
                        &published
                            .artifact_view()
                            .artifact_ids()
                            .into_iter()
                            .filter(|id| *id != old.descriptor.id)
                            .collect::<Vec<_>>(),
                        &[early],
                        &published.compiler_input_roles(),
                        published.lexical.clone(),
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
