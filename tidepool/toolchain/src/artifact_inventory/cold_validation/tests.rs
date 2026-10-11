use super::*;
use proptest::prelude::*;
use proptest::test_runner::FileFailurePersistence;

#[derive(Default)]
struct Fixture {
    entries: BTreeMap<ArtifactId, Arc<ArtifactEntry>>,
    rows: BindingRows,
}

impl Fixture {
    fn add(&mut self, entry: ArtifactEntry, scope: u8) -> ArtifactBindingNode {
        let id = entry.descriptor.id;
        self.entries.entry(id).or_insert_with(|| Arc::new(entry));
        let node = ArtifactBindingNode::Artifact(ArtifactBinding {
            artifact: id,
            selection: ArtifactSelectionId([scope; 32]),
        });
        self.rows.entry(node).or_default();
        node
    }

    fn add_original(&mut self, entry: ArtifactEntry, scope: u8) -> ArtifactBindingNode {
        let ArtifactPayload::Original(product) = &entry.payload else {
            panic!("original fixture");
        };
        let canonical = ArtifactEntry::canonical(product.module_interface().unwrap().clone());
        let carrier = self.add(canonical, scope);
        let original = self.add(entry, scope);
        self.edge(original, carrier, ArtifactDependency::Interface);
        original
    }

    fn group(
        &mut self,
        artifact: ArtifactBindingNode,
        scope: u8,
        ordinal: u32,
    ) -> ArtifactBindingNode {
        let node = ArtifactBindingNode::Group(NativeGroupBinding {
            binding: ArtifactBinding {
                artifact: artifact.artifact(),
                selection: ArtifactSelectionId([scope; 32]),
            },
            original_ordinal: ordinal,
        });
        self.rows.entry(node).or_default();
        self.edge(node, artifact, ArtifactDependency::Interface);
        node
    }

    fn edge(
        &mut self,
        from: ArtifactBindingNode,
        to: ArtifactBindingNode,
        edge: ArtifactDependency,
    ) {
        self.rows.get_mut(&from).unwrap().insert((to, edge));
    }

    fn result(&self) -> Result<(), CompileError> {
        validate_selected_rows(&InventoryState::default(), &self.entries, &self.rows)
    }

    fn agrees(&self) {
        assert_eq!(
            self.result().is_ok(),
            transitive_oracle(&self.entries, &self.rows).is_ok(),
            "rows: {:#?}",
            self.rows,
        );
    }
}

// This is the old per-source validator: independently collect each forward
// closure, construct its selected owner maps and recompute the certified row.
// It deliberately uses none of the direct-row or reverse-search helpers.
fn transitive_oracle(
    entries: &BTreeMap<ArtifactId, Arc<ArtifactEntry>>,
    rows: &BindingRows,
) -> Result<u64, CompileError> {
    let state = InventoryState::default();
    let mut visited = 0;
    for source in rows.keys() {
        let mut pending = vec![*source];
        let mut reachable = BTreeSet::new();
        while let Some(node) = pending.pop() {
            if reachable.insert(node) {
                visited += 1;
                pending.extend(
                    rows.get(&node)
                        .ok_or_else(|| failure("oracle target absent"))?
                        .iter()
                        .map(|(target, _)| *target),
                );
            }
        }
        let selected = artifact_ids(&reachable.iter().copied().collect())
            .iter()
            .map(|id| {
                entries
                    .get(id)
                    .map(|entry| (*id, Arc::clone(entry)))
                    .ok_or_else(|| failure("oracle payload absent"))
            })
            .collect::<Result<BTreeMap<_, _>, _>>()?;
        let owners = SelectedOwners::new(&state, &selected, &BTreeSet::new())?;
        let expected = match source {
            ArtifactBindingNode::Artifact(binding) => owners
                .dependencies(&state, &entries[&binding.artifact], &selected)?
                .into_iter()
                .map(|(id, label)| (LogicalNodeKey::Artifact(id), label))
                .collect::<BTreeSet<_>>(),
            ArtifactBindingNode::Group(group) => {
                let key = NativeGroupKey {
                    artifact: group.binding.artifact,
                    original_ordinal: group.original_ordinal,
                };
                owners
                    .planned_edges(&state, &selected, &BTreeSet::from([key]))?
                    .0[&LogicalNodeKey::Group(key)]
                    .clone()
            }
        };
        let actual = rows[source]
            .iter()
            .map(|(target, label)| (target.logical(), label.clone()))
            .collect::<BTreeSet<_>>();
        if expected != actual || actual.len() != rows[source].len() {
            return Err(failure("oracle row differs"));
        }
    }
    Ok(visited)
}

fn joined(module: &str, requirements: &[&str], producer: u8) -> ArtifactEntry {
    let interface = crate::certified_products::fixture_module_interface(
        [producer; 32],
        "fixture",
        module,
        BTreeMap::new(),
    );
    ArtifactEntry::interface(
        CertifiedJoinedInterface::from_certification(
            [producer; 32],
            "fixture".into(),
            module.into(),
            interface.interface_bytes().to_vec(),
            interface.package_imports_bytes().to_vec(),
        )
        .unwrap(),
        JoinedInterfaceRole::LexicalJoin,
        requirements
            .iter()
            .map(|module| ExactModuleIdentity {
                unit: "fixture".into(),
                module: (*module).into(),
            })
            .collect(),
    )
}

fn canonical(module: &str, producer: u8) -> ArtifactEntry {
    ArtifactEntry::canonical(crate::certified_products::fixture_module_interface(
        [producer; 32],
        "fixture",
        module,
        BTreeMap::new(),
    ))
}

fn native(
    module: &str,
    groups: Vec<(u32, Vec<crate::certified_products::PendingImportOwner>)>,
    version: u8,
) -> ArtifactEntry {
    ArtifactEntry::original(
        [2; 32],
        crate::certified_products::fixture_finalized_product(
            crate::certified_products::tests::original_groups_fixture(
                module,
                groups,
                version,
                &BTreeMap::new(),
            ),
            [2; 32],
        ),
    )
    .unwrap()
}

fn source_import(
    entry: &ArtifactEntry,
    ordinal: u32,
) -> crate::certified_products::PendingImportOwner {
    let ArtifactPayload::Original(product) = &entry.payload else {
        panic!("native fixture");
    };
    crate::certified_products::PendingImportOwner::Source {
        owner: product.owner().clone(),
        original_ordinal: ordinal,
        binder: tidepool_repr::execution_schema::SymbolIdentity {
            unit: "fixture".into(),
            module: product.owner().module.clone(),
            namespace: "value".into(),
            occurrence: if ordinal == 7 {
                "entry".into()
            } else {
                format!("entry_{ordinal}")
            },
            record_parent: None,
        },
    }
}

fn retained_import(module: &str) -> crate::certified_products::PendingImportOwner {
    crate::certified_products::PendingImportOwner::Retained {
        identity: tidepool_repr::execution_schema::SymbolIdentity {
            unit: "fixture".into(),
            module: module.into(),
            namespace: "value".into(),
            occurrence: "bound".into(),
            record_parent: Some("Parent".into()),
        },
        generation: u64::MAX,
    }
}

#[test]
fn disconnected_versions_and_equal_payload_with_different_children_are_valid() {
    let mut fixture = Fixture::default();
    let first = fixture.add(canonical("Leaf", 2), 1);
    let second = fixture.add(canonical("Leaf", 3), 2);
    let left = fixture.add(joined("Join", &["Leaf"], 2), 1);
    let right = fixture.add(joined("Join", &["Leaf"], 2), 2);
    assert_eq!(left.artifact(), right.artifact());
    fixture.edge(left, first, ArtifactDependency::Interface);
    fixture.edge(right, second, ArtifactDependency::Interface);
    fixture.agrees();
    fixture.result().unwrap();
    let work = LAST_WORK.with(std::cell::Cell::get);
    assert_eq!(work.competing_keys, 1);
    assert_eq!(work.reverse_searches, 2);

    let root = fixture.add(joined("Root", &["Join"], 2), 3);
    // Two typed occurrences of one logical child are an invalid outgoing row.
    fixture.edge(root, left, ArtifactDependency::Interface);
    fixture.edge(root, right, ArtifactDependency::Interface);
    fixture.agrees();
    assert!(fixture.result().is_err());
}

#[test]
fn diamonds_cycles_and_all_dependency_kinds_preserve_conflicts() {
    let mut fixture = Fixture::default();
    let first = fixture.add(canonical("Leaf", 2), 1);
    let second = fixture.add(canonical("Leaf", 3), 2);
    let left = fixture.add(joined("Left", &["Leaf", "Cycle"], 2), 1);
    let right = fixture.add(joined("Right", &["Leaf", "Cycle"], 2), 2);
    let cycle = fixture.add(joined("Cycle", &["Left"], 2), 3);
    let root = fixture.add(joined("Root", &["Left", "Right"], 2), 3);
    for (from, to) in [
        (left, first),
        (right, first),
        (left, cycle),
        (right, cycle),
        (cycle, left),
        (root, left),
        (root, right),
    ] {
        fixture.edge(from, to, ArtifactDependency::Interface);
    }
    fixture.agrees();
    fixture.result().unwrap();
    fixture
        .rows
        .get_mut(&right)
        .unwrap()
        .remove(&(first, ArtifactDependency::Interface));
    fixture.edge(right, second, ArtifactDependency::Interface);
    fixture.agrees();
    assert!(fixture.result().is_err());

    // A group-only root in a new scope has a carrier in an older scope. Its
    // retained binding edge still reaches both interface versions through Root.
    let original = fixture.add_original(
        native("Native", vec![(7, vec![retained_import("Root")])], 1),
        4,
    );
    let group = fixture.group(original, 5, 7);
    let label = fixture.entries[&original.artifact()].native_requirements_for(7)[0]
        .1
        .clone();
    assert!(matches!(
        label,
        ArtifactDependency::NativeBinding {
            generation: u64::MAX,
            record_parent: Some(_),
            ..
        }
    ));
    fixture.edge(group, root, label);
    fixture.agrees();
    assert!(fixture.result().is_err());
}

#[test]
fn crossed_group_carrier_and_native_labels_are_authenticated() {
    let child = native("Child", vec![(7, Vec::new())], 1);
    let parent = native(
        "Parent",
        vec![(7, vec![source_import(&child, 7), retained_import("Value")])],
        1,
    );
    let mut fixture = Fixture::default();
    let value = fixture.add(canonical("Value", 2), 1);
    let child = fixture.add_original(child, 1);
    let parent = fixture.add_original(parent, 1);
    let child_group = fixture.group(child, 2, 7);
    let parent_group = fixture.group(parent, 3, 7);
    for (owner, label) in fixture.entries[&parent.artifact()]
        .native_requirements_for(7)
        .to_vec()
    {
        fixture.edge(
            parent_group,
            if owner.module == "Child" {
                child_group
            } else {
                value
            },
            label,
        );
    }
    fixture.agrees();
    fixture.result().unwrap();
    assert!(!fixture
        .rows
        .contains_key(&ArtifactBindingNode::Artifact(ArtifactBinding {
            artifact: parent.artifact(),
            selection: parent_group.selection(),
        })));
    let valid = fixture.rows[&parent_group].clone();
    for mutation in 0..7 {
        fixture.rows.insert(parent_group, valid.clone());
        let (target, mut label) = fixture.rows[&parent_group]
            .iter()
            .find(|(_, label)| matches!(label, ArtifactDependency::NativeBinding { .. }))
            .cloned()
            .unwrap();
        fixture
            .rows
            .get_mut(&parent_group)
            .unwrap()
            .remove(&(target, label.clone()));
        if let ArtifactDependency::NativeBinding {
            dependent_ordinal,
            generation,
            namespace,
            occurrence,
            record_parent,
        } = &mut label
        {
            match mutation {
                0 => *dependent_ordinal += 1,
                1 => *generation -= 1,
                2 => namespace.push('x'),
                3 => occurrence.push('x'),
                4 => *record_parent = None,
                5 => label = ArtifactDependency::Interface,
                _ => {
                    label = ArtifactDependency::NativeGroup {
                        dependent_ordinal: 7,
                        required_ordinal: 7,
                    }
                }
            }
        }
        fixture.edge(parent_group, target, label);
        fixture.agrees();
        assert!(fixture.result().is_err(), "mutation {mutation}");
    }
    fixture.rows.insert(parent_group, valid.clone());
    let (target, label) = fixture.rows[&parent_group]
        .iter()
        .find(|(_, label)| matches!(label, ArtifactDependency::NativeGroup { .. }))
        .cloned()
        .unwrap();
    fixture
        .rows
        .get_mut(&parent_group)
        .unwrap()
        .remove(&(target, label));
    fixture.edge(
        parent_group,
        child_group,
        ArtifactDependency::NativeGroup {
            dependent_ordinal: 7,
            required_ordinal: 11,
        },
    );
    fixture.agrees();
    assert!(fixture.result().is_err());
    fixture.rows.insert(parent_group, valid);
    let wrong = fixture.add_original(native("Child", vec![(7, Vec::new())], 2), 4);
    let wrong_group = fixture.group(wrong, 5, 7);
    let label = ArtifactDependency::NativeGroup {
        dependent_ordinal: 7,
        required_ordinal: 7,
    };
    fixture
        .rows
        .get_mut(&parent_group)
        .unwrap()
        .remove(&(child_group, label.clone()));
    fixture.edge(parent_group, wrong_group, label);
    fixture.agrees();
    assert!(fixture.result().is_err());
}

#[test]
fn native_competing_certifications_include_group_occurrences() {
    let first_child = native("Child", vec![(7, Vec::new())], 1);
    let second_child = native("Child", vec![(7, Vec::new())], 2);
    let first_parent = native("Parent", vec![(7, vec![source_import(&first_child, 7)])], 1);
    let second_parent = native(
        "Parent",
        vec![(7, vec![source_import(&second_child, 7)])],
        1,
    );
    let key = |entry: &ArtifactEntry| {
        let ArtifactPayload::Original(product) = &entry.payload else {
            panic!()
        };
        NativeOwnerKey::from_owner(product.owner())
    };
    assert_eq!(key(&first_parent), key(&second_parent));
    assert_ne!(first_parent.descriptor.id, second_parent.descriptor.id);
    let mut fixture = Fixture::default();
    let first_child = fixture.add_original(first_child, 1);
    let second_child = fixture.add_original(second_child, 2);
    let first_parent = fixture.add_original(first_parent, 1);
    let second_parent = fixture.add_original(second_parent, 2);
    let first_group = fixture.group(first_parent, 3, 7);
    let second_group = fixture.group(second_parent, 4, 7);
    let first_child_group = fixture.group(first_child, 3, 7);
    let second_child_group = fixture.group(second_child, 4, 7);
    let label = ArtifactDependency::NativeGroup {
        dependent_ordinal: 7,
        required_ordinal: 7,
    };
    fixture.edge(first_group, first_child_group, label.clone());
    fixture.edge(second_group, second_child_group, label);
    fixture.agrees();
    fixture.result().unwrap();

    // Test the conflict search independently of row certification: group nodes
    // themselves seed their payload, even when their carriers are disconnected.
    let arbitrary_root = fixture.add(joined("SearchRoot", &[], 2), 5);
    fixture.edge(arbitrary_root, first_group, ArtifactDependency::Interface);
    fixture.edge(
        arbitrary_root,
        second_group,
        ArtifactDependency::NativeBinding {
            dependent_ordinal: 7,
            generation: 0,
            namespace: "value".into(),
            occurrence: "anything".into(),
            record_parent: None,
        },
    );
    assert!(validate_transitive_conflicts(
        &fixture.entries,
        &fixture.rows,
        &mut ValidationWork::default()
    )
    .is_err());
}

#[test]
fn canonical_seals_producers_and_complete_zero_rows_are_authenticated() {
    let child_entry = canonical("Child", 2);
    let parent_entry =
        ArtifactEntry::canonical(crate::certified_products::fixture_module_interface(
            [2; 32],
            "fixture",
            "Parent",
            BTreeMap::from([(
                ("fixture".into(), "Child".into()),
                child_entry.descriptor.interface_sha256,
            )]),
        ));
    let mut fixture = Fixture::default();
    let child = fixture.add(child_entry, 1);
    let parent = fixture.add(parent_entry, 1);
    fixture.edge(parent, child, ArtifactDependency::Interface);
    fixture.agrees();
    fixture.result().unwrap();
    let other = fixture.add(canonical("Child", 3), 2);
    fixture.rows.get_mut(&parent).unwrap().clear();
    fixture.edge(parent, other, ArtifactDependency::Interface);
    fixture.agrees();
    assert!(fixture.result().is_err());
    fixture.rows.get_mut(&parent).unwrap().clear();
    fixture.agrees();
    assert!(fixture.result().is_err());
    fixture.edge(parent, child, ArtifactDependency::Interface);
    fixture.edge(child, parent, ArtifactDependency::Interface);
    fixture.agrees();
    assert!(fixture.result().is_err());
}

fn property_config() -> ProptestConfig {
    let mut config = ProptestConfig::default();
    if std::env::var_os("PROPTEST_CASES").is_none() {
        config.cases = 128;
    }
    if std::env::var_os("PROPTEST_MAX_SHRINK_ITERS").is_none() {
        config.max_shrink_iters = 4096;
    }
    if let Some(path) = option_env!("TIDEPOOL_PROPTEST_REGRESSIONS") {
        config.failure_persistence = Some(Box::new(FileFailurePersistence::Direct(path)));
    }
    config
}

// Two scopes share each joined payload but independently choose its exact
// children. Leaf payloads compete by interface owner. Edges cross scopes and
// can form cycles, diamonds and conflicting ancestors.
fn generated_fixture(masks: &[u8], choices: &[bool]) -> Fixture {
    const N: usize = 4;
    let mut fixture = Fixture::default();
    let leaves = [
        fixture.add(canonical("Leaf", 2), 1),
        fixture.add(canonical("Leaf", 3), 2),
    ];
    let nodes: Vec<_> = (0..N)
        .map(|i| {
            let names: Vec<_> = (0..N).map(|j| format!("Join{j}")).collect();
            let mut required: Vec<_> = (0..N)
                .filter(|j| masks[i] & (1 << j) != 0)
                .map(|j| names[j].as_str())
                .collect();
            if masks[i] & 16 != 0 {
                required.push("Leaf");
            }
            [
                fixture.add(joined(&format!("Join{i}"), &required, 2), 1),
                fixture.add(joined(&format!("Join{i}"), &required, 2), 2),
            ]
        })
        .collect();
    for i in 0..N {
        for scope in 0..2 {
            for j in 0..N {
                if masks[i] & (1 << j) != 0 {
                    let to_scope = usize::from(choices[(i * 2 + scope) * (N + 1) + j]);
                    fixture.edge(
                        nodes[i][scope],
                        nodes[j][to_scope],
                        ArtifactDependency::Interface,
                    );
                }
            }
            if masks[i] & 16 != 0 {
                let leaf = usize::from(choices[(i * 2 + scope) * (N + 1) + N]);
                fixture.edge(nodes[i][scope], leaves[leaf], ArtifactDependency::Interface);
            }
        }
    }
    fixture
}

fn generated_native_fixture(masks: &[u8], choices: &[bool]) -> Fixture {
    const N: usize = 3;
    let placeholders: Vec<_> = (0..N)
        .map(|i| native(&format!("Native{i}"), vec![(7, Vec::new())], 1))
        .collect();
    let products = |targets: &[ArtifactEntry]| {
        (0..N)
            .map(|i| {
                let mut imports = (0..N)
                    .filter(|j| *j != i && masks[i] & (1 << j) != 0)
                    .map(|j| source_import(&targets[j], 7))
                    .collect::<Vec<_>>();
                if masks[i] & 8 != 0 {
                    imports.push(retained_import("Leaf"));
                }
                native(&format!("Native{i}"), vec![(7, imports)], 1)
            })
            .collect::<Vec<_>>()
    };
    let first_pass = products(&placeholders);
    let complete = products(&first_pass);
    for (first, final_entry) in first_pass.iter().zip(&complete) {
        let (ArtifactPayload::Original(first), ArtifactPayload::Original(final_product)) =
            (&first.payload, &final_entry.payload)
        else {
            panic!("native fixture");
        };
        assert_eq!(first.owner(), final_product.owner());
    }
    let mut fixture = Fixture::default();
    let leaves = [
        fixture.add(canonical("Leaf", 2), 1),
        fixture.add(canonical("Leaf", 3), 2),
    ];
    let originals: Vec<_> = complete
        .into_iter()
        .map(|entry| fixture.add_original(entry, 1))
        .collect();
    // Neither group scope owns its carrier. Both typed occurrences must seed
    // conflict propagation even though they share one original payload.
    let groups: Vec<_> = originals
        .iter()
        .map(|original| {
            [
                fixture.group(*original, 2, 7),
                fixture.group(*original, 3, 7),
            ]
        })
        .collect();
    for i in 0..N {
        for scope in 0..2 {
            for (owner, label) in fixture.entries[&originals[i].artifact()]
                .native_requirements_for(7)
                .to_vec()
            {
                let target = if owner.module == "Leaf" {
                    leaves[usize::from(choices[(i * 2 + scope) * (N + 1) + N])]
                } else {
                    let j = owner
                        .module
                        .strip_prefix("Native")
                        .unwrap()
                        .parse::<usize>()
                        .unwrap();
                    groups[j][usize::from(choices[(i * 2 + scope) * (N + 1) + j])]
                };
                fixture.edge(groups[i][scope], target, label);
            }
        }
    }
    fixture
}

proptest! {
    #![proptest_config(property_config())]
    #[test]
    fn direct_rows_and_competing_reverse_closures_match_transitive_oracle(
        masks in proptest::collection::vec(0u8..32, 4),
        choices in proptest::collection::vec(any::<bool>(), 40),
        tamper in 0usize..5,
    ) {
        let mut fixture = generated_fixture(&masks, &choices);
        let expected = transitive_oracle(&fixture.entries, &fixture.rows).is_ok();
        prop_assert_eq!(fixture.result().is_ok(), expected, "masks={:?} choices={:?}", masks, choices);
        if tamper != 0 {
            let source = *fixture.rows.keys().find(|node| fixture.entries[&node.artifact()].descriptor.owner.module == "Join0").unwrap();
            match tamper {
                1 => { fixture.rows.get_mut(&source).unwrap().clear(); }
                2 => { fixture.edge(source, source, ArtifactDependency::NativeBinding { dependent_ordinal: 7, generation: 1, namespace: "value".into(), occurrence: "x".into(), record_parent: None }); }
                3 => { fixture.edge(source, source, ArtifactDependency::NativeGroup { dependent_ordinal: 7, required_ordinal: 7 }); }
                _ => { fixture.edge(source, source, ArtifactDependency::Interface); }
            }
            prop_assert_eq!(fixture.result().is_ok(), transitive_oracle(&fixture.entries, &fixture.rows).is_ok());
        }
    }

    #[test]
    fn native_cycles_crossed_carriers_and_bindings_match_transitive_oracle(
        masks in proptest::collection::vec(0u8..16, 3),
        choices in proptest::collection::vec(any::<bool>(), 24),
        tamper in 0usize..4,
    ) {
        let mut fixture = generated_native_fixture(&masks, &choices);
        prop_assert_eq!(fixture.result().is_ok(), transitive_oracle(&fixture.entries, &fixture.rows).is_ok(), "masks={:?} choices={:?}", masks, choices);
        if tamper != 0 {
            let group = *fixture.rows.keys().find(|node| matches!(node, ArtifactBindingNode::Group(_))).unwrap();
            match tamper {
                1 => { fixture.rows.get_mut(&group).unwrap().clear(); }
                2 => { fixture.edge(group, group, ArtifactDependency::Interface); }
                _ => { fixture.edge(group, group, ArtifactDependency::NativeGroup { dependent_ordinal: 7, required_ordinal: 11 }); }
            }
            prop_assert_eq!(fixture.result().is_ok(), transitive_oracle(&fixture.entries, &fixture.rows).is_ok());
        }
    }
}

#[test]
fn unique_owner_validation_visits_each_row_once_without_searches() {
    for n in [32usize, 128, 512] {
        let mut fixture = Fixture::default();
        let names: Vec<_> = (0..n).map(|i| format!("Node{i}")).collect();
        let nodes: Vec<_> = (0..n)
            .map(|i| {
                let requirements = names
                    .get(i + 1)
                    .map(|name| vec![name.as_str()])
                    .unwrap_or_default();
                fixture.add(joined(&names[i], &requirements, 2), 1)
            })
            .collect();
        for pair in nodes.windows(2) {
            fixture.edge(pair[0], pair[1], ArtifactDependency::Interface);
        }
        fixture.result().unwrap();
        let work = LAST_WORK.with(std::cell::Cell::get);
        assert_eq!(work.rows, n as u64);
        assert_eq!(work.edges, n as u64 - 1);
        assert_eq!(work.competing_keys, 0);
        assert_eq!(work.reverse_searches, 0);
        assert_eq!(work.reverse_nodes, 0);
        assert_eq!(work.reverse_edges, 0);
        let old_visits = transitive_oracle(&fixture.entries, &fixture.rows).unwrap();
        assert_eq!(old_visits, (n * (n + 1) / 2) as u64);
        eprintln!(
            "cold validation chain nodes={n}: {work:?}; old forward node visits={}",
            old_visits
        );
    }
}
