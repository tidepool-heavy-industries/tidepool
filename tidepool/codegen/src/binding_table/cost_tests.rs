//! Bookkeeping costs use actual table operations and installed source leases.
//! Run this exact test in its own process; allocation counters cover this thread,
//! not JIT/compiler work, other threads, peak memory, or allocator metadata.

use super::*;
use crate::prepared_program::{
    BatchLeaseRequest, BatchProgram, DemandedImage, GroupInventory, ImageRegistry,
    PreparedCallOptions, PreparedMachine, PreparedMachineOptions, PreparedResult,
};
use crate::suspension::{RealmId, ValueHandle};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::hint::black_box;
use std::sync::Arc;
use std::time::Instant;
use tidepool_repr::execution_schema::{
    testing, CachedHomeOwner, CertifiedGroup, CheckedLayout, ConstructorDecl, ConstructorId,
    ExprFrame, Group, HeapRhs, ModuleVersion, ResultContract, RuntimeRep, SignatureId,
    SymbolIdentity, UpdatePolicy, ValueId,
};
use tidepool_repr::Generation;

#[derive(Clone, Copy, Debug, Default, serde::Serialize)]
pub(crate) struct AllocationCounts {
    allocations: u64,
    allocated_bytes: u64,
    reallocations: u64,
    reallocated_bytes: u64,
    deallocations: u64,
    deallocated_bytes: u64,
}

thread_local! {
    static ALLOCATION_COUNTS: Cell<Option<AllocationCounts>> = const { Cell::new(None) };
}

fn count_allocation(update: impl FnOnce(&mut AllocationCounts)) {
    let _ = ALLOCATION_COUNTS.try_with(|cell| {
        if let Some(mut counts) = cell.get() {
            update(&mut counts);
            cell.set(Some(counts));
        }
    });
}

struct CountingAllocator;

// The allocator delegates ownership and layout handling to System. Its thread
// local counter uses a const Cell and never allocates or formats during a call.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            count_allocation(|counts| {
                counts.allocations += 1;
                counts.allocated_bytes += layout.size() as u64;
            });
        }
        pointer
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc_zeroed(layout) };
        if !pointer.is_null() {
            count_allocation(|counts| {
                counts.allocations += 1;
                counts.allocated_bytes += layout.size() as u64;
            });
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        count_allocation(|counts| {
            counts.deallocations += 1;
            counts.deallocated_bytes += layout.size() as u64;
        });
        unsafe { System.dealloc(pointer, layout) };
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        let pointer = unsafe { System.realloc(pointer, layout, size) };
        if !pointer.is_null() {
            count_allocation(|counts| {
                counts.reallocations += 1;
                counts.reallocated_bytes += size as u64;
            });
        }
        pointer
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

struct AllocationWindow;

impl AllocationWindow {
    fn start() -> Self {
        ALLOCATION_COUNTS.with(|cell| {
            assert!(cell.get().is_none(), "measurement windows cannot nest");
            cell.set(Some(AllocationCounts::default()));
        });
        Self
    }

    fn finish(self) -> AllocationCounts {
        ALLOCATION_COUNTS.with(|cell| cell.replace(None).expect("active allocation window"))
    }
}

impl Drop for AllocationWindow {
    fn drop(&mut self) {
        // A failed fixture must not contaminate subsequent test diagnostics.
        ALLOCATION_COUNTS.with(|cell| cell.set(None));
    }
}

pub(crate) fn measure(operation: impl FnOnce()) -> (u128, AllocationCounts) {
    let window = AllocationWindow::start();
    let started = Instant::now();
    operation();
    let elapsed_ns = started.elapsed().as_nanos();
    (elapsed_ns, window.finish())
}

fn emit(
    n: usize,
    baseline: usize,
    operation: &str,
    repetitions: usize,
    measured: (u128, AllocationCounts),
) {
    eprintln!(
        "binding_table_cost {}",
        serde_json::json!({
            "schema": 1,
            "bindings": n,
            "unrelated_baseline": baseline,
            "native_source_instances": n,
            "unrelated_native_instances": baseline,
            "operation": operation,
            "repetitions": repetitions,
            "elapsed_ns": measured.0,
            "current_thread_allocations": measured.1,
        })
    );
}

fn source_image() -> (DemandedImage, SourceBinder) {
    let mut wire = testing::wire_program();
    wire.signatures[0].results = ResultContract::Returns(vec![RuntimeRep::LiftedRef]);
    wire.constructors.push(ConstructorDecl {
        identity: testing::identity("CostCaf", "Value"),
        family: testing::identity("CostCaf", "Value"),
        host_id: tidepool_repr::DataConId(90_091),
        result_rep: RuntimeRep::LiftedRef,
        field_reps: vec![],
        strict_fields: vec![],
        layout: CheckedLayout {
            fields: vec![],
            alignment: 1,
            payload_size: 0,
            root_mask: vec![],
        },
        tag: 1,
        family_size: 1,
    });
    wire.expressions.nodes[0] = ExprFrame::Construct {
        constructor: ConstructorId(0),
        fields: vec![],
    };
    let Group::NonRecursive(top) = &mut wire.bindings[0] else {
        panic!("one fixed native source top");
    };
    top.identity = testing::identity("CostFixture", "caf");
    top.binding.rhs = HeapRhs::Thunk {
        signature: SignatureId(0),
        update: UpdatePolicy::Memoize,
        captures: vec![],
        body: 0,
    };
    let binder = SourceBinder {
        version: ModuleVersion([91; 32]),
        binder: top.identity.clone(),
    };
    let group = CertifiedGroup::admit(
        CachedHomeOwner {
            unit: "fixture".into(),
            module: "CostFixture".into(),
            module_version: binder.version.clone(),
            skinny_iface_sha256: [92; 32],
            product_sha256: [93; 32],
        },
        testing::projected_group(wire, 0).unwrap(),
        vec![],
    )
    .unwrap();
    let groups = [group];
    let demand = GroupInventory::new(&groups)
        .unwrap()
        .seal([binder.clone()])
        .unwrap();
    let mut images = demand.compile(&ImageRegistry::new()).unwrap();
    assert_eq!(images.len(), 1);
    (images.pop().unwrap(), binder)
}

struct Scenario {
    table: BindingTable,
    tree: ScopeTree,
    machine: PreparedMachine<'static>,
    parent: ScopeId,
    unrelated: ScopeId,
    child: ScopeId,
    old: SessionVarId,
    shadow: SessionVarId,
    local_ids: Vec<SessionVarId>,
    next_id: u64,
}

impl Scenario {
    fn entry(&mut self, name: String) -> BindingEntry {
        let generation = self.next_id;
        self.next_id += 1;
        BindingEntry {
            name: BindingName(name.clone()),
            id: SessionVarId::from_extract(generation),
            module: SessionModule::val(Generation(generation)),
            value: BoundValue {
                handle: crate::prepared_program::PreparedHandle::new(
                    ValueHandle(generation),
                    RuntimeRep::LiftedRef,
                ),
                identity: SymbolIdentity {
                    unit: "fixture".into(),
                    module: format!("Tidepool.Session.Val.G{generation}"),
                    namespace: "value".into(),
                    occurrence: name,
                    record_parent: None,
                },
            },
            type_display: None,
            defining_expr: None,
            scope: ScopeId::ROOT,
        }
    }

    fn bind(&mut self, scope: ScopeId, name: String) -> SessionVarId {
        let entry = self.entry(name);
        let id = entry.id;
        // Compatible with both the baseline return value and checked insertion.
        let _ = self.table.bind_in(scope, entry);
        assert!(self.table.get(id).is_some());
        id
    }

    fn new(n: usize, baseline: usize, image: &DemandedImage, binder: &SourceBinder) -> Self {
        let mut tree = ScopeTree::new();
        let parent = tree.mint_isolated();
        let unrelated = tree.mint_isolated();
        let child = tree.mint_isolated();
        let mut scenario = Self {
            table: BindingTable::new(),
            tree,
            machine: PreparedMachine::empty(PreparedMachineOptions {
                nursery_bytes: 1 << 20,
            })
            .unwrap(),
            parent,
            unrelated,
            child,
            old: SessionVarId::from_extract(0),
            shadow: SessionVarId::from_extract(0),
            local_ids: Vec::new(),
            next_id: 1,
        };
        for index in 0..n {
            let id = scenario.bind(parent, format!("value_{index:03}"));
            if index == 0 {
                scenario.old = id;
            }
        }
        let mut alias = scenario.entry("local_alias".into());
        alias.value = scenario.table.get(scenario.old).unwrap().value.clone();
        let (_, expired) = scenario
            .table
            .bind_alias_in(parent, alias, scenario.old)
            .unwrap();
        assert!(expired.is_empty());
        scenario.shadow = scenario.bind(parent, "value_000".into());
        for index in 0..baseline {
            scenario.bind(unrelated, format!("other_{index:03}"));
        }
        let count = n + baseline;
        let receipt = scenario
            .machine
            .install_shared_batch_with_leases(
                (0..count)
                    .map(|_| BatchProgram {
                        image: Arc::clone(image.image()),
                        imports: vec![],
                    })
                    .collect(),
                (0..count)
                    .map(|index| BatchLeaseRequest::for_demanded(index, image, binder).unwrap())
                    .collect(),
            )
            .unwrap();
        let mut instances = HashSet::new();
        for (index, lease) in receipt.leases.into_iter().enumerate() {
            assert!(
                instances.insert(lease.instance()),
                "native installations are independent"
            );
            let result = scenario
                .machine
                .run_entry_retained(
                    lease.instance().program(),
                    ValueId(0),
                    &[],
                    PreparedCallOptions {
                        observation_budget: 0,
                        collect_before_observation: false,
                    },
                    RealmId::ROOT,
                )
                .unwrap();
            let [PreparedResult::Managed(value)] = result.values.as_slice() else {
                panic!("fixed native CAF returns a managed value");
            };
            assert!(scenario.machine.release(*value));
            let scope = if index < n { parent } else { unrelated };
            scenario
                .table
                .register_source_instance_in(&scenario.tree, scope, lease)
                .unwrap();
        }
        assert_eq!(instances.len(), count);
        assert_eq!(scenario.machine.handle_count(), count);
        scenario
            .table
            .seed_detached_scope(&scenario.tree, parent, child);
        assert!(scenario
            .table
            .resolve_in(&scenario.tree, child, "local_alias")
            .is_none());
        assert_eq!(
            scenario
                .table
                .resolve_in(&scenario.tree, child, "value_000")
                .unwrap()
                .id,
            scenario.shadow
        );
        for index in 0..n {
            let id = scenario.bind(child, format!("value_{index:03}"));
            scenario.local_ids.push(id);
        }
        scenario
    }

    fn release_sources(&mut self, sources: Vec<SourceInstanceLease>) {
        for token in sources {
            assert!(self.machine.release(token.handle()));
        }
    }
}

#[test]
fn binding_scope_cost_matrix_preserves_shadow_alias_and_native_lifetimes() {
    const REPETITIONS: usize = 128;
    // Compile once; scenario construction, native installation and first CAF
    // forcing are excluded from every timing/allocation measurement below.
    let (image, binder) = source_image();
    for n in [1, 10, 100] {
        for baseline in [0, 100] {
            let mut scenario = Scenario::new(n, baseline, &image, &binder);
            for (scope, phase) in [(scenario.parent, "parent"), (scenario.child, "captured")] {
                let expected_names = if scope == scenario.parent { n + 1 } else { n };
                assert_eq!(
                    scenario.table.iter_current_in(&scenario.tree, scope).len(),
                    expected_names
                );
                let modules = scenario
                    .table
                    .scope_reachable_modules(&scenario.tree, scope)
                    .collect::<Vec<_>>();
                assert!(!modules.is_empty());
                assert_eq!(
                    scenario
                        .table
                        .source_instance_keys_in(&scenario.tree, scope)
                        .len(),
                    n
                );
                emit(
                    n,
                    baseline,
                    &format!("{phase}_iter_current_in"),
                    REPETITIONS,
                    measure(|| {
                        for _ in 0..REPETITIONS {
                            black_box(scenario.table.iter_current_in(&scenario.tree, scope));
                        }
                    }),
                );
                emit(
                    n,
                    baseline,
                    &format!("{phase}_scope_reachable_modules"),
                    REPETITIONS,
                    measure(|| {
                        for _ in 0..REPETITIONS {
                            black_box(
                                scenario
                                    .table
                                    .scope_reachable_modules(&scenario.tree, scope)
                                    .collect::<Vec<_>>(),
                            );
                        }
                    }),
                );
                emit(
                    n,
                    baseline,
                    &format!("{phase}_source_instance_keys_in"),
                    REPETITIONS,
                    measure(|| {
                        for _ in 0..REPETITIONS {
                            black_box(
                                scenario
                                    .table
                                    .source_instance_keys_in(&scenario.tree, scope),
                            );
                        }
                    }),
                );
            }
            let grandchild = scenario.tree.mint_isolated();
            emit(
                n,
                baseline,
                "seed_detached_scope",
                1,
                measure(|| {
                    black_box(scenario.table.seed_detached_scope(
                        &scenario.tree,
                        scenario.child,
                        grandchild,
                    ));
                }),
            );
            assert!(scenario.table.lease_count(scenario.old) >= 1);
            assert_eq!(
                scenario
                    .table
                    .source_instance_keys_in(&scenario.tree, grandchild)
                    .len(),
                n
            );
            assert_eq!(
                scenario.machine.handle_count(),
                n + baseline,
                "tip shares create no native handles"
            );
            assert_eq!(
                scenario.tree.retire(scenario.unrelated),
                vec![scenario.unrelated]
            );
            let unrelated = scenario.table.drain_scope_with_sources(scenario.unrelated);
            assert_eq!(unrelated.bindings.len(), baseline);
            assert_eq!(unrelated.source_instances.len(), baseline);
            scenario.release_sources(unrelated.source_instances);
            assert_eq!(scenario.tree.retire(scenario.parent), vec![scenario.parent]);
            let parent = scenario.table.drain_scope_with_sources(scenario.parent);
            assert!(
                parent.bindings.is_empty(),
                "captured aliases retain their source generation"
            );
            assert!(parent.source_instances.is_empty());
            assert!(scenario.table.get(scenario.old).is_some());
            assert_eq!(scenario.machine.handle_count(), n);
            assert_eq!(scenario.tree.retire(scenario.child), vec![scenario.child]);
            let child = scenario.table.drain_scope_with_sources(scenario.child);
            assert!(child.bindings.is_empty());
            assert!(child.source_instances.is_empty());
            assert!(scenario.table.lease_count(scenario.old) >= 1);
            assert_eq!(
                scenario
                    .table
                    .iter_current_in(&scenario.tree, grandchild)
                    .len(),
                n
            );
            let mut last = None;
            assert_eq!(scenario.tree.retire(grandchild), vec![grandchild]);
            emit(
                n,
                baseline,
                "drain_last_detached_scope",
                1,
                measure(|| {
                    last = Some(scenario.table.drain_scope_with_sources(grandchild));
                }),
            );
            let last = last.unwrap();
            assert!(last.bindings.iter().any(|entry| entry.id == scenario.old));
            assert_eq!(last.source_instances.len(), n);
            scenario.release_sources(last.source_instances);
            assert!(scenario.table.is_empty());
            assert_eq!(scenario.machine.handle_count(), 0);
            let quiescent = scenario.machine.quiesce().unwrap();
            scenario.machine.collect_major(quiescent).unwrap();
            assert_eq!(
                scenario.machine.residency().programs,
                0,
                "final source owner releases native installations"
            );
        }
    }
}

#[test]
fn growing_prefix_captures_measure_physical_lease_work_and_independent_readers() {
    let (image, binder) = source_image();
    for n in [1, 10, 100] {
        for unrelated in [0, 100] {
            let mut scenario = Scenario::new(n, unrelated, &image, &binder);
            let mut latest = None;
            let mut reader = None;
            let mut physical_bindings = 0;
            let mut physical_sources = 0;
            let mut elapsed_ns = 0;
            let mut allocations = AllocationCounts::default();
            for index in 0..n {
                scenario.bind(scenario.child, format!("step_{index:03}"));
                let binding_before = scenario.table.leases.values().sum::<usize>();
                let source_before = scenario
                    .table
                    .source_instances
                    .values()
                    .map(|lease| lease.shares)
                    .sum::<usize>();
                let snapshot = scenario.tree.mint_isolated();
                let measured = measure(|| {
                    scenario
                        .table
                        .seed_detached_scope(&scenario.tree, scenario.child, snapshot);
                });
                // Read actual owner counters before releasing the preceding
                // capture; these differences count production acquisitions.
                physical_bindings += scenario.table.leases.values().sum::<usize>() - binding_before;
                physical_sources += scenario
                    .table
                    .source_instances
                    .values()
                    .map(|lease| lease.shares)
                    .sum::<usize>()
                    - source_before;
                elapsed_ns += measured.0;
                let count = measured.1;
                allocations.allocations += count.allocations;
                allocations.allocated_bytes += count.allocated_bytes;
                allocations.reallocations += count.reallocations;
                allocations.reallocated_bytes += count.reallocated_bytes;
                allocations.deallocations += count.deallocations;
                allocations.deallocated_bytes += count.deallocated_bytes;
                if reader.is_none() {
                    reader = Some(snapshot);
                }
                if let Some(old) = latest.replace(snapshot) {
                    if Some(old) != reader {
                        scenario.tree.retire(old);
                        let drained = scenario.table.drain_scope_with_sources(old);
                        assert!(drained.bindings.is_empty());
                        assert!(drained.source_instances.is_empty());
                    }
                }
            }
            eprintln!(
                "binding_table_cost {}",
                serde_json::json!({
                    "schema": 2,
                    "bindings": n,
                    "unrelated_baseline": unrelated,
                    "native_source_instances": n,
                    "unrelated_native_instances": unrelated,
                    "operation": "growing_prefix_capture",
                    "repetitions": n,
                    "elapsed_ns": elapsed_ns,
                    "current_thread_allocations": allocations,
                    "physical_binding_acquisitions": physical_bindings,
                    "physical_native_share_acquisitions": physical_sources,
                })
            );
            let latest = latest.unwrap();
            let reader = reader.unwrap();
            assert!(scenario
                .table
                .resolve_in(&scenario.tree, reader, "step_000")
                .is_some());
            if n > 1 {
                assert!(scenario
                    .table
                    .resolve_in(&scenario.tree, reader, "step_001")
                    .is_none());
                assert!(scenario
                    .table
                    .resolve_in(&scenario.tree, latest, "step_001")
                    .is_some());
            }
            for scope in [scenario.unrelated, scenario.parent, scenario.child] {
                scenario.tree.retire(scope);
                let drained = scenario.table.drain_scope_with_sources(scope);
                scenario.release_sources(drained.source_instances);
            }
            if latest != reader {
                scenario.tree.retire(latest);
                let drained = scenario.table.drain_scope_with_sources(latest);
                assert!(
                    drained.source_instances.is_empty(),
                    "earlier reader retains native baseline"
                );
            }
            assert_eq!(scenario.machine.handle_count(), n);
            scenario.tree.retire(reader);
            let drained = scenario.table.drain_scope_with_sources(reader);
            assert_eq!(drained.source_instances.len(), n);
            scenario.release_sources(drained.source_instances);
            assert!(scenario.table.is_empty());
            assert_eq!(scenario.machine.handle_count(), 0);
            let quiescent = scenario.machine.quiesce().unwrap();
            scenario.machine.collect_major(quiescent).unwrap();
            assert_eq!(scenario.machine.residency().programs, 0);
        }
    }
}

#[test]
fn native_retraction_does_not_extend_last_owner_through_new_captures() {
    let (image, binder) = source_image();
    let mut scenario = Scenario::new(1, 0, &image, &binder);
    let receipt = scenario
        .machine
        .install_shared_batch_with_leases(
            vec![BatchProgram {
                image: Arc::clone(image.image()),
                imports: vec![],
            }],
            vec![BatchLeaseRequest::for_demanded(0, &image, &binder).unwrap()],
        )
        .unwrap();
    let token = receipt.leases.into_iter().next().unwrap();
    let key = scenario
        .table
        .register_source_instance_in(&scenario.tree, scenario.child, token)
        .unwrap();
    let older = scenario.tree.mint_isolated();
    scenario
        .table
        .seed_detached_scope(&scenario.tree, scenario.child, older);
    assert!(scenario
        .table
        .source_instance_keys_in(&scenario.tree, older)
        .contains(&key));
    assert!(scenario
        .table
        .retire_source_instances_in(scenario.child, &[key.clone()])
        .unwrap()
        .is_empty());
    let newer = scenario.tree.mint_isolated();
    scenario
        .table
        .seed_detached_scope(&scenario.tree, scenario.child, newer);
    assert!(!scenario
        .table
        .source_instance_keys_in(&scenario.tree, newer)
        .contains(&key));
    assert_eq!(scenario.machine.handle_count(), 2);
    scenario.tree.retire(older);
    let drained = scenario.table.drain_scope_with_sources(older);
    assert_eq!(
        drained.source_instances.len(),
        1,
        "new snapshot cannot prolong retracted instance"
    );
    scenario.release_sources(drained.source_instances);
    assert_eq!(scenario.machine.handle_count(), 1);
    for scope in [scenario.parent, scenario.child, scenario.unrelated, newer] {
        scenario.tree.retire(scope);
        let drained = scenario.table.drain_scope_with_sources(scope);
        scenario.release_sources(drained.source_instances);
    }
    assert!(scenario.table.is_empty());
    assert_eq!(scenario.machine.handle_count(), 0);
    let quiescent = scenario.machine.quiesce().unwrap();
    scenario.machine.collect_major(quiescent).unwrap();
    assert_eq!(scenario.machine.residency().programs, 0);
}
