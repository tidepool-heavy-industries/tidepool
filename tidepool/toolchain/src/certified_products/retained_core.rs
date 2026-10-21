//! Stable native identities from the validated reachable promotion graph.

use super::*;

type OwnerKey = (String, String);

/// Authenticated nominal metadata for stable identity, never execution demand.
#[derive(Clone, Debug)]
pub(super) enum NominalImportOwner {
    Source {
        owner: CachedHomeOwner,
        original_ordinal: u32,
        binder: SymbolIdentity,
    },
    OriginalBinding {
        unit: String,
        module: String,
        interface: crate::artifact_inventory::ArtifactBinding,
        original: crate::artifact_inventory::ArtifactBinding,
        binder: SymbolIdentity,
    },
    Retained {
        identity: SymbolIdentity,
        generation: u64,
    },
    Package {
        unit: String,
        module: String,
        binder: SymbolIdentity,
        interface_digest: [u8; 32],
    },
}

impl NominalImportOwner {
    pub(super) fn authenticate(
        receipt: ReceiptImportOwner,
        sources: &SourceGroupMap,
        selection: &CertifiedSourceSelection,
        packages: &BTreeMap<OwnerKey, PackageInterfaceWitness>,
        validation: &mut PackageInterfaceValidation,
    ) -> CertResult<Self> {
        Ok(
            match normalize_receipt_metadata(receipt, sources, selection, packages, validation)? {
                ReceiptImportOwner::Source {
                    unit,
                    module,
                    module_version,
                    original_ordinal,
                    binder,
                } => Self::Source {
                    owner: selection
                        .receipt_source_member(&unit, &module, module_version.as_ref())?
                        .clone(),
                    original_ordinal,
                    binder,
                },
                ReceiptImportOwner::OriginalBinding {
                    unit,
                    module,
                    interface,
                    original,
                    binder,
                } => Self::OriginalBinding {
                    unit,
                    module,
                    interface,
                    original,
                    binder,
                },
                ReceiptImportOwner::Retained {
                    identity,
                    generation,
                } => Self::Retained {
                    identity,
                    generation,
                },
                ReceiptImportOwner::Package {
                    unit,
                    module,
                    binder,
                    interface_digest,
                } => Self::Package {
                    unit,
                    module,
                    binder,
                    interface_digest,
                },
            },
        )
    }

    pub(super) fn receipt(&self) -> ReceiptImportOwner {
        match self {
            Self::Source {
                owner,
                original_ordinal,
                binder,
            } => ReceiptImportOwner::Source {
                unit: owner.unit.clone(),
                module: owner.module.clone(),
                module_version: Some(owner.module_version.clone()),
                original_ordinal: *original_ordinal,
                binder: binder.clone(),
            },
            Self::OriginalBinding {
                unit,
                module,
                interface,
                original,
                binder,
            } => ReceiptImportOwner::OriginalBinding {
                unit: unit.clone(),
                module: module.clone(),
                interface: *interface,
                original: *original,
                binder: binder.clone(),
            },
            Self::Retained {
                identity,
                generation,
            } => ReceiptImportOwner::Retained {
                identity: identity.clone(),
                generation: *generation,
            },
            Self::Package {
                unit,
                module,
                binder,
                interface_digest,
            } => ReceiptImportOwner::Package {
                unit: unit.clone(),
                module: module.clone(),
                binder: binder.clone(),
                interface_digest: *interface_digest,
            },
        }
    }
}

/// Staged after canonical payloads, native groups and nominal import membership
/// passed their owning validators. Local source versions are not yet issued.
pub(super) struct PromotedModule {
    pub(super) canonical_sha256: [u8; 32],
    pub(super) product_sha256: [u8; 32],
    pub(super) package_sha256: [u8; 32],
    pub(super) groups: BTreeMap<u32, Vec<NominalImportOwner>>,
}

pub(super) fn module_versions(
    promoted: &BTreeMap<OwnerKey, PromotedModule>,
    packages: &BTreeMap<OwnerKey, PackageInterfaceWitness>,
) -> CertResult<BTreeMap<OwnerKey, ModuleVersion>> {
    promoted
        .keys()
        .map(|root| {
            let graph = graph_value(root, promoted, packages)?;
            let mut bytes = Vec::new();
            ciborium::ser::into_writer(&graph, &mut bytes)
                .map_err(|_| CertificationError::Mismatch("retained identity graph encoding"))?;
            let mut digest = Sha256::new();
            for field in [
                b"retained-core-home-v1".as_slice(),
                root.0.as_bytes(),
                root.1.as_bytes(),
                bytes.as_slice(),
            ] {
                digest.update((field.len() as u64).to_be_bytes());
                digest.update(field);
            }
            Ok((root.clone(), ModuleVersion(digest.finalize().into())))
        })
        .collect()
}

fn graph_value(
    root: &OwnerKey,
    promoted: &BTreeMap<OwnerKey, PromotedModule>,
    packages: &BTreeMap<OwnerKey, PackageInterfaceWitness>,
) -> CertResult<Value> {
    let mut pending = vec![root.clone()];
    let mut reached = BTreeSet::new();
    while let Some(key) = pending.pop() {
        if !reached.insert(key.clone()) {
            continue;
        }
        let node = promoted
            .get(&key)
            .ok_or(CertificationError::Mismatch("retained graph owner"))?;
        for import in node.groups.values().flatten() {
            if let NominalImportOwner::Source { owner, .. } = import {
                let selected = (owner.unit.clone(), owner.module.clone());
                if promoted.contains_key(&selected) {
                    pending.push(selected);
                }
            }
        }
    }
    Ok(value_array(
        reached
            .into_iter()
            .map(|key| {
                let node = &promoted[&key];
                Ok(value_array([
                    value_text(key.0),
                    value_text(key.1),
                    value_text(hex(&node.canonical_sha256)),
                    value_text(hex(&node.product_sha256)),
                    value_text(hex(&node.package_sha256)),
                    value_array(
                        node.groups
                            .iter()
                            .map(|(ordinal, imports)| {
                                Ok(value_array([
                                    Value::Integer((*ordinal).into()),
                                    value_array(
                                        imports
                                            .iter()
                                            .map(|import| edge_value(import, promoted, packages))
                                            .collect::<CertResult<Vec<_>>>()?,
                                    ),
                                ]))
                            })
                            .collect::<CertResult<Vec<_>>>()?,
                    ),
                ]))
            })
            .collect::<CertResult<Vec<_>>>()?,
    ))
}

fn edge_value(
    import: &NominalImportOwner,
    promoted: &BTreeMap<OwnerKey, PromotedModule>,
    packages: &BTreeMap<OwnerKey, PackageInterfaceWitness>,
) -> CertResult<Value> {
    let package_path = |unit: &str, module: &str, seal: &[u8; 32]| -> CertResult<Value> {
        let package = packages
            .get(&(unit.to_owned(), module.to_owned()))
            .filter(|package| package.sha256 == *seal)
            .ok_or(CertificationError::Mismatch(
                "retained identity package witness",
            ))?;
        Ok(value_text(package.selected_path.to_str().ok_or(
            CertificationError::Mismatch("package witness path"),
        )?))
    };
    Ok(match import {
        NominalImportOwner::Source {
            owner,
            original_ordinal,
            binder,
        } => {
            if promoted.contains_key(&(owner.unit.clone(), owner.module.clone())) {
                value_array([
                    value_text("local-source"),
                    value_text(&owner.unit),
                    value_text(&owner.module),
                    Value::Integer((*original_ordinal).into()),
                    value_identity(binder),
                ])
            } else {
                value_array([
                    value_text("source"),
                    value_home(owner),
                    Value::Integer((*original_ordinal).into()),
                    value_identity(binder),
                ])
            }
        }
        NominalImportOwner::OriginalBinding { binder, .. } => value_array([
            value_text("original-binding"),
            value_import(&import.receipt()),
            value_identity(binder),
        ]),
        NominalImportOwner::Retained {
            identity,
            generation,
        } => value_array([
            value_text("retained"),
            value_identity(identity),
            Value::Integer((*generation).into()),
        ]),
        NominalImportOwner::Package {
            unit,
            module,
            binder,
            interface_digest,
        } => value_array([
            value_text("package"),
            value_text(unit),
            value_text(module),
            value_text(hex(interface_digest)),
            value_identity(binder),
            package_path(unit, module, interface_digest)?,
        ]),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // These are semantic hash inputs, not accepted compiler products.
    fn key(module: &str) -> OwnerKey {
        ("main".into(), module.into())
    }

    fn binder(unit: &str, module: &str, occurrence: &str) -> SymbolIdentity {
        SymbolIdentity {
            unit: unit.into(),
            module: module.into(),
            namespace: "value".into(),
            occurrence: occurrence.into(),
            record_parent: None,
        }
    }

    fn source(module: &str) -> NominalImportOwner {
        NominalImportOwner::Source {
            owner: CachedHomeOwner {
                unit: "main".into(),
                module: module.into(),
                module_version: ModuleVersion([0; 32]),
                skinny_iface_sha256: [4; 32],
                product_sha256: [5; 32],
            },
            original_ordinal: 2,
            binder: binder("main", module, "value"),
        }
    }

    fn node(imports: Vec<NominalImportOwner>) -> PromotedModule {
        PromotedModule {
            canonical_sha256: [1; 32],
            product_sha256: [2; 32],
            package_sha256: [3; 32],
            groups: BTreeMap::from([(2, imports)]),
        }
    }

    fn version(nodes: &BTreeMap<OwnerKey, PromotedModule>) -> ModuleVersion {
        module_versions(nodes, &BTreeMap::new()).unwrap()[&key("A")].clone()
    }

    #[test]
    fn promotion_identity_excludes_unrelated_nodes_and_includes_transitive_bodies() {
        let mut nodes = BTreeMap::from([
            (key("A"), node(vec![source("B")])),
            (key("B"), node(vec![])),
        ]);
        let original = version(&nodes);
        nodes.insert(key("Unrelated"), node(vec![source("External")]));
        assert_eq!(version(&nodes), original);
        nodes.get_mut(&key("Unrelated")).unwrap().canonical_sha256 = [9; 32];
        assert_eq!(version(&nodes), original);
        nodes.get_mut(&key("B")).unwrap().product_sha256 = [9; 32];
        assert_ne!(version(&nodes), original);
        nodes.get_mut(&key("B")).unwrap().product_sha256 = [2; 32];
        nodes
            .get_mut(&key("B"))
            .unwrap()
            .groups
            .insert(3, vec![source("External")]);
        assert_ne!(version(&nodes), original);
    }

    #[test]
    fn promotion_identity_handles_cycles_without_local_versions_or_input_order() {
        let mut nodes = BTreeMap::from([
            (key("A"), node(vec![source("B")])),
            (key("B"), node(vec![source("A")])),
        ]);
        let original = version(&nodes);
        assert_eq!(
            hex(&original.0),
            "3ac76245a5831ad99106a7304dd99c695e988df96fb34703e0a09c4fa33c45ea"
        );
        assert_eq!(
            hex(&module_versions(&nodes, &BTreeMap::new()).unwrap()[&key("B")].0),
            "fcf24cceb8e82b87bb709cf8d6fe6242e716f4ea80e2fdb005af6d60045421be"
        );
        for node in nodes.values_mut() {
            for import in node.groups.values_mut().flatten() {
                let NominalImportOwner::Source { owner, .. } = import else {
                    unreachable!()
                };
                owner.module_version = ModuleVersion([99; 32]);
            }
        }
        let reversed = nodes.into_iter().rev().collect::<BTreeMap<_, _>>();
        assert_eq!(version(&reversed), original);
    }

    #[test]
    fn promotion_identity_binds_external_native_owners_and_retained_generations() {
        let mut nodes = BTreeMap::from([(key("A"), node(vec![source("External")]))]);
        let original = version(&nodes);
        let NominalImportOwner::Source { owner, .. } = &mut nodes
            .get_mut(&key("A"))
            .unwrap()
            .groups
            .get_mut(&2)
            .unwrap()[0]
        else {
            unreachable!()
        };
        owner.module_version = ModuleVersion([7; 32]);
        assert_ne!(version(&nodes), original);
        let NominalImportOwner::Source { owner, .. } = &mut nodes
            .get_mut(&key("A"))
            .unwrap()
            .groups
            .get_mut(&2)
            .unwrap()[0]
        else {
            unreachable!()
        };
        owner.module_version = ModuleVersion([0; 32]);
        owner.product_sha256 = [7; 32];
        assert_ne!(version(&nodes), original);
        nodes.get_mut(&key("A")).unwrap().groups.insert(
            2,
            vec![NominalImportOwner::Retained {
                identity: binder("main", "Lib.G1", "retained"),
                generation: 1,
            }],
        );
        let generation_one = version(&nodes);
        let NominalImportOwner::Retained { generation, .. } = &mut nodes
            .get_mut(&key("A"))
            .unwrap()
            .groups
            .get_mut(&2)
            .unwrap()[0]
        else {
            unreachable!()
        };
        *generation = 2;
        assert_ne!(version(&nodes), generation_one);
    }

    #[test]
    fn promotion_identity_binds_transitive_inherited_native_version() {
        let mut inherited = source("H");
        let NominalImportOwner::Source { owner, .. } = &mut inherited else {
            unreachable!()
        };
        owner.module_version = ModuleVersion([7; 32]);
        let mut nodes = BTreeMap::from([
            (key("A"), node(vec![source("B")])),
            (key("B"), node(vec![inherited])),
            (key("Unrelated"), node(vec![])),
        ]);
        let original = module_versions(&nodes, &BTreeMap::new()).unwrap();
        let NominalImportOwner::Source { owner, .. } = &mut nodes
            .get_mut(&key("B"))
            .unwrap()
            .groups
            .get_mut(&2)
            .unwrap()[0]
        else {
            unreachable!()
        };
        owner.module_version = ModuleVersion([8; 32]);
        let changed = module_versions(&nodes, &BTreeMap::new()).unwrap();
        assert_ne!(changed[&key("A")], original[&key("A")]);
        assert_ne!(changed[&key("B")], original[&key("B")]);
        assert_eq!(changed[&key("Unrelated")], original[&key("Unrelated")]);
        for module in ["A", "B"] {
            let node = &nodes[&key(module)];
            assert_eq!(node.canonical_sha256, [1; 32]);
            assert_eq!(node.product_sha256, [2; 32]);
            assert_eq!(node.package_sha256, [3; 32]);
        }
    }

    #[test]
    fn promotion_original_reference_matches_haskell_edge_and_binds_every_nominal_field() {
        use crate::artifact_inventory::{ArtifactBinding, ArtifactId, ArtifactSelectionId};
        let reference = NominalImportOwner::OriginalBinding {
            unit: "main".into(),
            module: "Original".into(),
            interface: ArtifactBinding {
                artifact: ArtifactId([11; 32]),
                selection: ArtifactSelectionId([12; 32]),
            },
            original: ArtifactBinding {
                artifact: ArtifactId([13; 32]),
                selection: ArtifactSelectionId([14; 32]),
            },
            binder: SymbolIdentity {
                record_parent: Some("Parent".into()),
                ..binder("main", "Original", "field")
            },
        };
        // The Haskell retainedNativeVersions writer nests encodeOriginalBinding
        // and the complete outer SymbolIdentity, including namespace and parent.
        let expected = value_array([
            value_text("original-binding"),
            value_array([
                value_text("original-binding"),
                value_text("main"),
                value_text("Original"),
                value_array([Value::Bytes(vec![11; 32]), Value::Bytes(vec![12; 32])]),
                value_array([Value::Bytes(vec![13; 32]), Value::Bytes(vec![14; 32])]),
            ]),
            value_array([
                value_text("main"),
                value_text("Original"),
                value_text("value"),
                value_text("field"),
                value_text("Parent"),
            ]),
        ]);
        assert_eq!(
            edge_value(&reference, &BTreeMap::new(), &BTreeMap::new()).unwrap(),
            expected
        );
        let original = version(&BTreeMap::from([(key("A"), node(vec![reference.clone()]))]));
        assert_eq!(
            hex(&original.0),
            "a9677ae303d03c7ec2ae92566cd383107ccf97ac63eedd1a6743ff469793e85c"
        );
        for changed in 0..11 {
            let mut variant = reference.clone();
            let NominalImportOwner::OriginalBinding {
                unit,
                module,
                interface,
                original: original_binding,
                binder,
            } = &mut variant
            else {
                unreachable!()
            };
            match changed {
                0 => unit.push_str("-other"),
                1 => module.push_str("Other"),
                2 => interface.artifact.0[0] ^= 1,
                3 => interface.selection.0[0] ^= 1,
                4 => original_binding.artifact.0[0] ^= 1,
                5 => original_binding.selection.0[0] ^= 1,
                6 => binder.unit.push_str("-other"),
                7 => binder.module.push_str("Other"),
                8 => binder.namespace = "type".into(),
                9 => binder.occurrence.push_str("Other"),
                10 => binder.record_parent = None,
                _ => unreachable!(),
            }
            assert_ne!(
                version(&BTreeMap::from([(key("A"), node(vec![variant]))])),
                original,
                "field {changed}"
            );
        }
    }

    #[test]
    fn promotion_identity_preserves_zero_group_nodes() {
        let mut empty = node(vec![]);
        empty.groups.clear();
        let mut nodes = BTreeMap::from([(key("A"), empty)]);
        let original = version(&nodes);
        assert_eq!(module_versions(&nodes, &BTreeMap::new()).unwrap().len(), 1);
        nodes.get_mut(&key("A")).unwrap().canonical_sha256 = [9; 32];
        assert_ne!(version(&nodes), original);
    }

    #[test]
    fn promotion_identity_binds_package_seal_and_path() {
        let identity = binder("pkg", "Package", "value");
        let package = NominalImportOwner::Package {
            unit: "pkg".into(),
            module: "Package".into(),
            binder: identity.clone(),
            interface_digest: [8; 32],
        };
        let mut nodes = BTreeMap::from([(key("A"), node(vec![package]))]);
        let package_key = ("pkg".into(), "Package".into());
        let mut packages = BTreeMap::from([(
            package_key.clone(),
            PackageInterfaceWitness {
                selected_path: "/package/original.hi".into(),
                sha256: [8; 32],
            },
        )]);
        let original = module_versions(&nodes, &packages).unwrap()[&key("A")].clone();
        packages.get_mut(&package_key).unwrap().selected_path = "/package/changed.hi".into();
        assert_ne!(
            module_versions(&nodes, &packages).unwrap()[&key("A")],
            original
        );
        packages.get_mut(&package_key).unwrap().selected_path = "/package/original.hi".into();
        packages.get_mut(&package_key).unwrap().sha256 = [9; 32];
        assert!(module_versions(&nodes, &packages).is_err());
        let NominalImportOwner::Package {
            interface_digest, ..
        } = &mut nodes
            .get_mut(&key("A"))
            .unwrap()
            .groups
            .get_mut(&2)
            .unwrap()[0]
        else {
            unreachable!()
        };
        *interface_digest = [9; 32];
        assert_ne!(
            module_versions(&nodes, &packages).unwrap()[&key("A")],
            original
        );
    }
}

#[cfg(test)]
mod properties;
