//! Pure linkage records are checked independently of the SCC implementation.
//! These fixtures issue no acquired grants or runtime installation authority.

use super::*;
use proptest::prelude::*;
use proptest::test_runner::FileFailurePersistence;

const NAMES: [&str; 4] = ["NodeA", "NodeB", "NodeC", "NodeD"];

fn reachable(edges: &[Vec<usize>], root: usize) -> BTreeSet<usize> {
    let mut reached = BTreeSet::from([root]);
    loop {
        let next = reached
            .iter()
            .flat_map(|node| edges[*node].iter().copied())
            .chain(reached.iter().copied())
            .collect::<BTreeSet<_>>();
        if next == reached {
            return reached;
        }
        reached = next;
    }
}

fn plans(edges: &[Vec<usize>], values: &[i64; 4], order: &[usize]) -> Vec<NativeCodePlan> {
    let positions = order
        .iter()
        .enumerate()
        .map(|(position, logical)| (*logical, position))
        .collect::<BTreeMap<_, _>>();
    order
        .iter()
        .map(|node| {
            let children = edges[*node]
                .iter()
                .map(|child| (NAMES[*child], positions[child]))
                .collect::<Vec<_>>();
            plan(NAMES[*node], &children, values[*node])
        })
        .collect()
}

fn check_model(edges: &[Vec<usize>], values: [i64; 4], changed_node: usize) {
    let roots = [0, 1, 2, 3];
    let source = plans(edges, &values, &roots);
    let all = NativeCodeComponent::link(&source, &roots, &operation()).unwrap();
    // Check correspondence is injective before using IDs to observe edges.
    // Distinct nominal definitions cannot overwrite the oracle's model nodes.
    let observed = all
        .iter()
        .enumerate()
        .map(|(logical, code)| (code.id(), logical))
        .collect::<BTreeMap<_, _>>();
    assert_eq!(observed.len(), 4);
    let closures = roots.map(|root| reachable(edges, root));
    for root in roots {
        let one = NativeCodeComponent::link(&source, &[root], &operation()).unwrap();
        assert_eq!(
            one[0].id(),
            all[root].id(),
            "other selected roots do not alter this code"
        );
        let imports = all[root]
            .imports()
            .map(|import| match import {
                NativeCodeImport::Code { code, value } => {
                    assert_eq!(value, ValueId(0));
                    observed[&code.id()]
                }
                NativeCodeImport::Retained(_) => panic!("this model has code edges only"),
            })
            .collect::<Vec<_>>();
        assert_eq!(imports, edges[root], "all ordered slots are preserved");
        for other in roots {
            let same_scc = closures[root].contains(&other) && closures[other].contains(&root);
            assert_eq!(
                Arc::ptr_eq(all[root].component(), all[other].component()),
                same_scc
            );
        }
    }

    let reverse_order = [3, 2, 1, 0];
    let reversed =
        NativeCodeComponent::link(&plans(edges, &values, &reverse_order), &roots, &operation())
            .unwrap();
    for (position, logical) in reverse_order.iter().enumerate() {
        assert_eq!(reversed[position].id(), all[*logical].id());
    }

    let mut extended = source.clone();
    extended.push(plan("Unrelated", &[], 99));
    let with_extra =
        NativeCodeComponent::link(&extended, &[4, 2, 0, 0, 3, 1], &operation()).unwrap();
    for (slot, logical) in [(1, 2), (2, 0), (3, 0), (4, 3), (5, 1)] {
        assert_eq!(with_extra[slot].id(), all[logical].id());
    }

    let mut changed = values;
    changed[changed_node] += 1;
    let mutated =
        NativeCodeComponent::link(&plans(edges, &changed, &roots), &roots, &operation()).unwrap();
    for root in roots {
        assert_eq!(
            all[root].id() != mutated[root].id(),
            closures[root].contains(&changed_node),
            "only ancestors of a changed body, including its SCC, change code identity"
        );
    }

    if let Some(node) = roots
        .into_iter()
        .find(|node| edges[*node].iter().rev().copied().collect::<Vec<_>>() != edges[*node])
    {
        let mut reordered = edges.to_vec();
        reordered[node].reverse();
        let next =
            NativeCodeComponent::link(&plans(&reordered, &values, &roots), &roots, &operation())
                .unwrap();
        for root in roots {
            assert_eq!(
                all[root].id() != next[root].id(),
                closures[root].contains(&node),
                "ordered dependency changes propagate through actual ancestors"
            );
        }
    }
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

proptest! {
    #![proptest_config(property_config())]

    #[test]
    fn linked_graphs_match_independent_reachability_and_scc_model(
        edges in proptest::collection::vec(
            proptest::collection::vec(0usize..4, 0..5).prop_map(|edges| {
                let mut seen = BTreeSet::new();
                edges.into_iter().filter(|edge| seen.insert(*edge)).collect::<Vec<_>>()
            }), 4),
        values in any::<[i8; 4]>(),
        changed_node in 0usize..4,
    ) {
        check_model(&edges, values.map(i64::from), changed_node);
    }
}

#[test]
fn exhaustive_small_graphs_cover_cycles_and_shared_descendants() {
    for mask in 0..16 {
        let mut edges = vec![Vec::new(); 4];
        for from in 0..2 {
            for to in 0..2 {
                if mask & (1 << (from * 2 + to)) != 0 {
                    edges[from].push(to);
                }
            }
        }
        for changed in 0..4 {
            check_model(&edges, [11, 22, 33, 44], changed);
        }
    }
    check_model(&[vec![1, 2], vec![3], vec![3], vec![]], [1, 2, 3, 4], 3);
}

#[test]
fn external_component_cuts_preserve_code_without_merging_nominal_definitions() {
    let source = [
        plan("A", &[("B", 1)], 1),
        plan("B", &[("C", 2)], 2),
        plan("C", &[], 3),
    ];
    let all = NativeCodeComponent::link(&source, &[0, 1, 2], &operation()).unwrap();
    let mut parent = source[0].clone();
    parent.imports[0] = NativeCodePlanImport::External {
        code: all[1].clone(),
        value: ValueId(0),
    };
    let cut = NativeCodeComponent::link(&[parent], &[0], &operation()).unwrap();
    assert_eq!(cut[0].id(), all[0].id());

    // Equal immutable occurrences may have one CodeId. This makes no claim
    // that their acquired grants or installed CAF instances are interchangeable.
    let globals = ["entry", "alias"]
        .into_iter()
        .map(|name| GlobalDecl {
            identity: testing::identity("Child", name),
            rep: RuntimeRep::LiftedRef,
            entry_signature: None,
            required_evaluated: false,
            required_generation: None,
        })
        .collect();
    let child = NativeCodePlan {
        body: fixture_body("Child", vec![], true),
        imports: vec![],
    };
    let parent = NativeCodePlan {
        body: fixture_body("Parent", globals, false),
        imports: vec![
            NativeCodePlanImport::Local {
                occurrence: 1,
                value: ValueId(0),
            },
            NativeCodePlanImport::Local {
                occurrence: 1,
                value: ValueId(1),
            },
        ],
    };
    let shared = [parent.clone(), child.clone()];
    let mut duplicate_parent = parent;
    duplicate_parent.imports[1] = NativeCodePlanImport::Local {
        occurrence: 2,
        value: ValueId(1),
    };
    let duplicates = [duplicate_parent, child.clone(), child];
    let duplicate_code = NativeCodeComponent::link(&duplicates, &[0], &operation()).unwrap();
    let shared_code = NativeCodeComponent::link(&shared, &[0], &operation()).unwrap();
    assert_eq!(duplicate_code[0].id(), shared_code[0].id());
    let children = duplicate_code[0]
        .imports()
        .map(|edge| match edge {
            NativeCodeImport::Code { code, value } => (code, value),
            _ => unreachable!(),
        })
        .collect::<Vec<_>>();
    assert_eq!(children[0].0.id(), children[1].0.id());
    assert_eq!((children[0].1, children[1].1), (ValueId(0), ValueId(1)));

    let same_result = [
        plan("Parent", &[("Left", 1), ("Right", 2)], 1),
        plan("Left", &[], 2),
        plan("Right", &[], 2),
    ];
    let baseline = NativeCodeComponent::link(&same_result, &[1, 2], &operation()).unwrap();
    assert_ne!(
        baseline[0].id(),
        baseline[1].id(),
        "different nominal children remain distinct despite equal raw results"
    );
    let original = plan("Nominal", &[], 2);
    let NativeCodeBody::Package(body) = &original.body else {
        unreachable!()
    };
    let mut owner = body.owner().clone();
    owner.defining_version_sha256[0] ^= 1;
    let changed_nominal = NativeCodePlan {
        body: NativeCodeBody::Package(
            PackageGroupCode::admit(owner, body.projected_group().clone()).unwrap(),
        ),
        imports: vec![],
    };
    let old = NativeCodeComponent::link(&[original], &[0], &operation()).unwrap();
    let new = NativeCodeComponent::link(&[changed_nominal], &[0], &operation()).unwrap();
    assert_ne!(
        old[0].id(),
        new[0].id(),
        "an exact nominal version changes identity with identical definitions"
    );

    let mut invalid = source.to_vec();
    invalid[0].imports[0] = NativeCodePlanImport::Local {
        occurrence: 1,
        value: ValueId(1),
    };
    assert!(matches!(
        NativeCodeComponent::link(&invalid, &[0], &operation()),
        Err(NativeCodeError::ImportContract)
    ));
    assert_eq!(
        NativeCodeComponent::link(&source, &[0], &operation()).unwrap()[0].id(),
        all[0].id()
    );
}

fn fixture_body(module: &str, globals: Vec<GlobalDecl>, two_exports: bool) -> NativeCodeBody {
    let NativeCodeBody::Package(template) = plan(module, &[], 42).body else {
        unreachable!()
    };
    let mut wire = testing::wire_program();
    let Group::NonRecursive(mut top) = wire.bindings.remove(0) else {
        unreachable!()
    };
    top.identity = testing::identity(module, "entry");
    if two_exports {
        let mut alias = top.clone();
        alias.identity = testing::identity(module, "alias");
        alias.binding.id = ValueId(1);
        wire.bindings = vec![Group::Recursive(vec![top, alias])];
    } else {
        wire.bindings = vec![Group::NonRecursive(top)];
    }
    wire.globals = globals;
    NativeCodeBody::Package(
        PackageGroupCode::admit(
            template.owner().clone(),
            testing::projected_group(wire, 7).unwrap(),
        )
        .unwrap(),
    )
}

#[test]
fn exact_retained_requirements_change_code_and_keep_symbol_generation_and_type() {
    let held = |generation: u64, type_artifact| {
        let identity = testing::identity("Captured", "value");
        NativeCodePlan {
            body: fixture_body(
                "Held",
                vec![GlobalDecl {
                    identity: identity.clone(),
                    rep: RuntimeRep::LiftedRef,
                    entry_signature: None,
                    required_evaluated: false,
                    required_generation: Some(generation),
                }],
                false,
            ),
            imports: vec![NativeCodePlanImport::Retained(
                NativeRetainedRequirement::new(identity, generation, type_artifact),
            )],
        }
    };
    let initial = NativeCodeComponent::link(&[held(7, Some([1; 32]))], &[0], &operation()).unwrap();
    for (generation, type_artifact) in [(8, Some([1; 32])), (7, Some([2; 32])), (7, None)] {
        let changed =
            NativeCodeComponent::link(&[held(generation, type_artifact)], &[0], &operation())
                .unwrap();
        assert_ne!(initial[0].id(), changed[0].id());
        let imports = changed[0].imports().collect::<Vec<_>>();
        let [NativeCodeImport::Retained(requirement)] = imports.as_slice() else {
            panic!("one exact retained leaf")
        };
        assert_eq!(
            requirement.identity(),
            &testing::identity("Captured", "value")
        );
        assert_eq!(requirement.generation(), generation);
        assert_eq!(requirement.type_artifact().copied(), type_artifact);
    }
}
