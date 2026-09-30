//! Same fixture measures validated graph clones and actual native registry
//! lookups. Validation and immutable native compilation stay outside windows.

use super::*;
use crate::binding_table::cost_tests::measure;
use std::hint::black_box;
use tidepool_repr::execution_schema::{
    link_program, testing, CachedHomeOwner, GlobalDecl, Group, HeapRhs, ImportOwner,
    MachineImports, ModuleVersion, RuntimeRep, ValueId,
};
use tidepool_repr::SessionVarId;

#[test]
fn prepared_clone_and_image_lookup_cost_matrix() {
    const REPEATS: usize = 128;
    for n in [1, 10, 100] {
        let mut wire = testing::wire_program();
        let Group::NonRecursive(template) = wire.bindings[0].clone() else {
            unreachable!()
        };
        for id in 1..n {
            let mut top = template.clone();
            top.identity.occurrence = format!("function_{id:03}");
            top.binding.id = ValueId(id as u32);
            let HeapRhs::Function { body, .. } = &mut top.binding.rhs else {
                unreachable!()
            };
            *body = id;
            wire.expressions
                .nodes
                .push(wire.expressions.nodes[0].clone());
            wire.bindings.push(Group::NonRecursive(top));
        }
        let prepared = testing::prepare(wire).unwrap();
        let linked = link_program(prepared.clone(), &MachineImports::default()).unwrap();
        let registry = ImageRegistry::new();
        let image = Arc::new(CompiledProgram::compile(&linked).unwrap());
        let installed = registry.insert(linked.clone(), Arc::clone(&image));
        assert!(Arc::ptr_eq(&installed, &image));
        for operation in ["prepared_clone", "image_registry_hot_lookup"] {
            let measured = measure(|| {
                for _ in 0..REPEATS {
                    if operation == "prepared_clone" {
                        black_box(prepared.clone());
                    } else {
                        let found = registry.lookup(&linked).unwrap();
                        assert!(Arc::ptr_eq(&found, &image));
                        black_box(found);
                    }
                }
            });
            eprintln!(
                "prepared_image_cost {}",
                serde_json::json!({
                    "schema": 1, "definitions": n, "operation": operation,
                    "repetitions": REPEATS, "elapsed_ns": measured.0,
                    "current_thread_allocations": measured.1,
                })
            );
        }
        assert_eq!(registry.misses(), 0);
        assert_eq!(registry.hits(), REPEATS as u64);
    }
}

#[test]
fn certified_group_machine_local_owner_fragmentation_cost_matrix() {
    for n in [1, 10, 100] {
        let mut wire = testing::wire_program();
        wire.globals.push(GlobalDecl {
            identity: testing::identity("Imports", "retained"),
            rep: RuntimeRep::LiftedRef,
            entry_signature: None,
            required_evaluated: false,
            required_generation: Some(1),
        });
        let group = testing::projected_group(wire, 0).unwrap();
        let owner = CachedHomeOwner {
            unit: "fixture".into(),
            module: "Fixture".into(),
            module_version: ModuleVersion([21; 32]),
            skinny_iface_sha256: [22; 32],
            product_sha256: [23; 32],
        };
        let groups = (0..n)
            .map(|id| {
                CertifiedGroup::admit(
                    owner.clone(),
                    group.clone(),
                    vec![ImportOwner::Retained {
                        id: SessionVarId::from_extract((id + 1) as u64),
                        generation: 1,
                    }],
                )
                .unwrap()
            })
            .collect::<Vec<_>>();
        if n > 1 {
            assert_ne!(groups[0], groups[1], "live owner proofs stay distinct");
        }
        let image = Arc::new(CompiledProgram::compile_certified_group(&groups[0]).unwrap());
        let registry = ImageRegistry::new();
        let measured = measure(|| {
            for group in &groups {
                // Every elected compiler returns the already compiled identical
                // neutral image; the counter measures actual registry elections.
                let selected = registry
                    .get_or_compile_group(group, || Ok::<_, ()>(Arc::clone(&image)))
                    .unwrap();
                assert!(Arc::ptr_eq(&selected, &image));
                black_box(selected);
            }
        });
        eprintln!(
            "prepared_image_cost {}",
            serde_json::json!({
                "schema": 1, "definitions": 1,
                "operation": "machine_local_group_owners", "repetitions": n,
                "elapsed_ns": measured.0, "current_thread_allocations": measured.1,
                "elected_compilers": registry.misses(), "live_image_hits": registry.hits(),
            })
        );
    }
}
