//! Pins the value boundary Wave 6 effect suspension/resumption will be built
//! on: when generated code returns the `E` case of `Control.Monad.Freer`'s
//! representation (an effect request, as opposed to `Val`, the pure result),
//! the heap value Rust receives back from `run_entry`/the force adapter is
//! already at constructor WHNF, and any memoizing thunk-update obligation
//! that was pending on the path to that value has already settled. This is a
//! plain call/return value boundary — reaching it does not require native-stack
//! suspension (a fiber/coroutine mechanism); `run_entry` and the raw force
//! adapter both return synchronously to their caller. Wave 6's dispatcher may
//! rely on both of these without re-deriving them from the generated code.
//!
//! `E`'s shape here is a 2-field constructor mirroring `E (Union effs x) (Arr
//! effs x a)`, with plain constructor fields standing in for the union
//! payload and the FTCQueue-shaped continuation (`docs/GLOSSARY.md`'s
//! `Leaf`/`Node` sequence, `tidepool_repr::freer_names`) — a real function
//! field would hit the unrelated, already-covered "function results are
//! unobservable" contract (`entry_tests::w5_a4_function_result_is_typed_unobservable_failure`),
//! which is not what this boundary is about.
//!
//! Claim NOT pinned here, and deliberately not asserted anywhere in this
//! file: that every thunk reachable at the boundary is non-`Evaluating`. A
//! thunk consumed under `UpdatePolicy::SingleEntry` intentionally retains
//! `DescriptorState::Evaluating` after a successful, ordinary consume — that
//! is the state the engine intends, not a leftover in-progress obligation. A
//! future assertion that demands "no thunk is Evaluating" at this boundary
//! would be wrong and would break this legitimate behavior; the retention
//! test below pins the intended state positively instead.

use super::{
    entry_tests::caf_program, safepoint::NativeStackBounds, CompiledProgram, DescriptorMeaning,
    RunOptions, TopSlotBase,
};
use crate::{
    context::VMContext,
    machine_state::{MachineDisposition, MachineState},
    prepared_control::CallStatus,
};
use std::sync::{atomic::AtomicBool, Arc};
use tidepool_heap::execution_descriptor::{DescriptorState, ObjectDescriptor};
use tidepool_repr::execution_schema::{
    link_program, testing, Atom, CheckedLayout, ConstructorDecl, ConstructorId, ExprFrame,
    FieldLayout, Group, HeapBinding, HeapRhs, MachineImports, ResultContract, RuntimeRep,
    SignatureId, StorageLayout, TopBinding, UpdatePolicy, ValueId, ValueRef,
};

fn con(host_id: u64, name: &str, fields: Vec<RuntimeRep>) -> ConstructorDecl {
    let layout = StorageLayout::for_reps(&testing::target(), &fields).unwrap();
    ConstructorDecl {
        identity: testing::identity("Freer", name),
        family: testing::identity("Freer", name),
        host_id: tidepool_repr::DataConId(host_id),
        result_rep: RuntimeRep::LiftedRef,
        tag: 1,
        family_size: 1,
        strict_fields: vec![false; fields.len()],
        field_reps: fields,
        layout: CheckedLayout {
            fields: layout
                .fields()
                .iter()
                .map(|field| FieldLayout {
                    rep: field.rep(),
                    offset: field.offset(),
                })
                .collect(),
            alignment: layout.alignment(),
            payload_size: layout.payload_size(),
            root_mask: layout
                .fields()
                .iter()
                .map(|field| matches!(field.rep(), RuntimeRep::LiftedRef | RuntimeRep::UnliftedRef))
                .collect(),
        },
    }
}

/// A CAF shaped like an effect-request return: forcing the entry allocates an
/// `E`-like constructor whose first field is itself a distinct memoizing
/// thunk (the lazily built union payload) and whose second field is an
/// eagerly allocated stand-in for the continuation. `policy` governs only the
/// entry thunk, so its settlement can be inspected independently of the
/// nested union thunk (always `Memoize`). Driven only through `run_entry`,
/// which builds the top-level address table every other top binding this
/// wire references depends on.
fn effect_request_caf(policy: UpdatePolicy) -> CompiledProgram {
    let mut wire = testing::wire_program();
    wire.signatures[0].results = ResultContract::Returns(vec![RuntimeRep::LiftedRef]);
    wire.constructors.push(con(
        9700,
        "E",
        vec![RuntimeRep::LiftedRef, RuntimeRep::LiftedRef],
    ));
    wire.constructors.push(con(9701, "UnionPayload", vec![]));
    wire.constructors.push(con(9702, "ArrLeaf", vec![]));
    wire.expressions.nodes = vec![
        ExprFrame::Construct {
            constructor: ConstructorId(1),
            fields: vec![],
        },
        ExprFrame::Construct {
            constructor: ConstructorId(0),
            fields: vec![
                Atom::Ref(ValueRef::Local(ValueId(1))),
                Atom::Ref(ValueRef::Local(ValueId(2))),
            ],
        },
    ];
    wire.bindings = vec![Group::Recursive(vec![
        TopBinding {
            identity: testing::identity("Freer", "e-caf"),
            binding: HeapBinding {
                id: ValueId(0),
                rhs: HeapRhs::Thunk {
                    signature: SignatureId(0),
                    update: policy,
                    captures: vec![],
                    body: 1,
                },
            },
        },
        TopBinding {
            identity: testing::identity("Freer", "union-thunk"),
            binding: HeapBinding {
                id: ValueId(1),
                rhs: HeapRhs::Thunk {
                    signature: SignatureId(0),
                    update: UpdatePolicy::Memoize,
                    captures: vec![],
                    body: 0,
                },
            },
        },
        TopBinding {
            identity: testing::identity("Freer", "arr-leaf"),
            binding: HeapBinding {
                id: ValueId(2),
                rhs: HeapRhs::Constructor {
                    constructor: ConstructorId(2),
                    fields: vec![],
                },
            },
        },
    ])];
    let linked = link_program(testing::prepare(wire).unwrap(), &MachineImports::default()).unwrap();
    CompiledProgram::compile(&linked, TopSlotBase::ZERO).unwrap()
}

/// The low-level `RawForce` harness below bypasses `run_entry`'s top-level
/// address table entirely (see its doc comment) — it pre-allocates only the
/// entry descriptor, so any other top binding the entry's body reaches
/// through a bare `Local` reference resolves through an address table that
/// was never built, reading uninitialized memory. `captured_caf_program` in
/// `settlement_tests` sidesteps this by making every such value a declared
/// `capture` on the thunk itself — a field stored directly in the thunk's own
/// payload, which the harness can poke by hand before forcing. This wire
/// mirrors that pattern for an `E`-shaped, two-field entry: both fields are
/// captures, and `ValueId(1)`/`ValueId(2)` exist only so the schema's
/// free-variable/identity checks resolve — `RawForce` never runs their own
/// construction, it allocates matching objects directly and writes them into
/// the entry thunk's capture slots.
fn captured_effect_request_caf(policy: UpdatePolicy) -> CompiledProgram {
    let mut wire = testing::wire_program();
    wire.signatures[0].results = ResultContract::Returns(vec![RuntimeRep::LiftedRef]);
    wire.constructors.push(con(
        9700,
        "E",
        vec![RuntimeRep::LiftedRef, RuntimeRep::LiftedRef],
    ));
    wire.constructors.push(con(9701, "UnionPayload", vec![]));
    wire.constructors.push(con(9702, "ArrLeaf", vec![]));
    wire.expressions.nodes = vec![ExprFrame::Construct {
        constructor: ConstructorId(0),
        fields: vec![
            Atom::Ref(ValueRef::Local(ValueId(1))),
            Atom::Ref(ValueRef::Local(ValueId(2))),
        ],
    }];
    wire.bindings = vec![Group::Recursive(vec![
        TopBinding {
            identity: testing::identity("Freer", "e-caf"),
            binding: HeapBinding {
                id: ValueId(0),
                rhs: HeapRhs::Thunk {
                    signature: SignatureId(0),
                    update: policy,
                    captures: vec![ValueRef::Local(ValueId(1)), ValueRef::Local(ValueId(2))],
                    body: 0,
                },
            },
        },
        TopBinding {
            identity: testing::identity("Freer", "union-payload"),
            binding: HeapBinding {
                id: ValueId(1),
                rhs: HeapRhs::Constructor {
                    constructor: ConstructorId(1),
                    fields: vec![],
                },
            },
        },
        TopBinding {
            identity: testing::identity("Freer", "arr-leaf"),
            binding: HeapBinding {
                id: ValueId(2),
                rhs: HeapRhs::Constructor {
                    constructor: ConstructorId(2),
                    fields: vec![],
                },
            },
        },
    ])];
    let linked = link_program(testing::prepare(wire).unwrap(), &MachineImports::default()).unwrap();
    CompiledProgram::compile(&linked, TopSlotBase::ZERO).unwrap()
}

/// The `E`-shaped result of `run_entry`/observation reaches Rust as a real
/// `Con` — never a suspended-computation marker, because `tidepool_bridge::Value`
/// has no such variant to return in the first place — and its nested union
/// thunk is likewise fully realized. `run_entry` is one ordinary synchronous
/// call: no thread, fiber, or other native-stack switch is involved in
/// reaching this boundary.
#[test]
fn effect_constructor_reaches_whnf_through_a_plain_call_return_boundary() {
    let program = effect_request_caf(UpdatePolicy::Memoize);
    let result = program
        .run_entry(
            ValueId(0),
            &[],
            &RunOptions::default(),
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
    assert_eq!(result.values.len(), 1);
    let tidepool_bridge::Value::Con(tag, fields) = &result.values[0] else {
        panic!("effect-request boundary must observe a constructor");
    };
    assert_eq!(*tag, tidepool_repr::DataConId(9700));
    assert!(matches!(
        fields.as_slice(),
        [tidepool_bridge::Value::Con(union, union_fields), tidepool_bridge::Value::Con(arr, arr_fields)]
            if *union == tidepool_repr::DataConId(9701) && union_fields.is_empty()
                && *arr == tidepool_repr::DataConId(9702) && arr_fields.is_empty()
    ));
}

/// Test-only raw invocation that calls the generated force adapter directly
/// (as `settlement_tests` does) so a thunk's `DescriptorState` can be read
/// right after the call it settled in returns, with nothing else observed or
/// forced in between.
struct RawForce<'a> {
    program: &'a CompiledProgram,
    machine: Box<MachineState>,
    vmctx: VMContext,
    root: Box<usize>,
    descriptor: Arc<ObjectDescriptor>,
}

impl<'a> RawForce<'a> {
    fn new(program: &'a CompiledProgram) -> Self {
        let descriptor = program
            .descriptor_registry
            .values()
            .find_map(|metadata| match metadata.meaning {
                DescriptorMeaning::Callable {
                    binding: ValueId(0),
                    ..
                } => Some(Arc::clone(&metadata.descriptor)),
                _ => None,
            })
            .unwrap();
        let machine = Box::new(MachineState::new());
        machine.set_stack_map_registry(&program.pipeline.stack_maps);
        machine
            .install_prepared_buffer(vec![0_u64; 32], program.descriptors.clone())
            .unwrap();
        let (start, size) = machine.gc_active_range().unwrap();
        unsafe { descriptor.initialize_header(start) };
        let mut root = Box::new(start as usize);
        machine.register_rust_root((&mut *root as *mut usize).cast());
        let mut vmctx =
            unsafe { VMContext::new(start, start.add(size), crate::host_fns::gc_trigger) };
        vmctx.alloc_ptr = unsafe { start.add(descriptor.allocation_extent() as usize) };
        vmctx.machine_state = (&*machine as *const MachineState).cast_mut();
        vmctx.prepared_stack_limit = NativeStackBounds::current()
            .unwrap()
            .limit_with_frame_reserve(program.pipeline.native_frame_maximum())
            .unwrap();
        Self {
            program,
            machine,
            vmctx,
            root,
            descriptor,
        }
    }

    /// Allocate a value matching `host_id`'s descriptor and write it into the
    /// entry thunk's `logical`-th capture slot, exactly as
    /// `settlement_tests::Invocation::install_captured_constructor` does for
    /// its single capture — generalized to an arbitrary capture index so an
    /// `E`-shaped, two-capture entry can be prepared without ever running the
    /// top-level address table `RawForce` deliberately skips.
    fn install_capture(&mut self, logical: usize, host_id: u64) -> usize {
        let capture = self
            .program
            .descriptor_registry
            .values()
            .find_map(|metadata| match &metadata.meaning {
                DescriptorMeaning::Constructor(observation)
                    if observation.identity == tidepool_repr::DataConId(host_id) =>
                {
                    Some(Arc::clone(&metadata.descriptor))
                }
                _ => None,
            })
            .expect("capture target descriptor");
        let object = self.vmctx.alloc_ptr;
        unsafe { capture.initialize_header(object) };
        let stored = self.descriptor.payload().logical_to_stored()[logical]
            .expect("thunk capture has a managed slot");
        let field = &self.descriptor.payload().fields()[stored as usize];
        let tagged = object as usize | usize::from(capture.tag());
        unsafe {
            std::ptr::write_unaligned(
                (*self.root as *mut u8)
                    .add((self.descriptor.payload_base() + field.offset()) as usize)
                    .cast::<usize>(),
                tagged,
            );
            self.vmctx.alloc_ptr = object.add(capture.allocation_extent() as usize);
        }
        tagged
    }

    fn force(&mut self) -> CallStatus {
        NativeStackBounds::current()
            .unwrap()
            .ensure_current_frame_reserve(self.program.pipeline.native_frame_maximum() * 2)
            .unwrap();
        let adapter = self
            .program
            .pipeline
            .get_function_ptr(self.program.prepared_force_adapter());
        let adapter: unsafe extern "C" fn(*mut VMContext, *mut u64, usize) -> i32 =
            unsafe { std::mem::transmute(adapter) };
        let mut output = 0xdead_beef_u64;
        let status = unsafe { adapter(&mut self.vmctx, &mut output, *self.root) };
        CallStatus::from_raw(i64::from(status)).unwrap()
    }

    fn state(&self) -> DescriptorState {
        assert_eq!(self.machine.disposition(), MachineDisposition::Reusable);
        unsafe {
            self.descriptor.state(
                *self.root as *const u8,
                self.descriptor.allocation_extent() as usize,
            )
        }
        .unwrap()
    }
}

impl Drop for RawForce<'_> {
    fn drop(&mut self) {
        self.machine.clear_gc_state();
        self.machine.clear_stack_map_registry();
    }
}

/// Claim 1's settlement half: a `Memoize` thunk consumed on the path to the
/// effect-request boundary is `Updated`, not merely `Live` or still
/// `Evaluating`, by the time the force adapter that settled it returns.
#[test]
fn memoize_update_settles_before_the_settling_call_returns() {
    let program = captured_effect_request_caf(UpdatePolicy::Memoize);
    let mut invocation = RawForce::new(&program);
    invocation.install_capture(0, 9701);
    invocation.install_capture(1, 9702);
    assert_eq!(invocation.force(), CallStatus::Success);
    assert_eq!(
        invocation.state(),
        DescriptorState::Updated,
        "a Memoize thunk observed at the effect-request boundary must have \
         its pending update settled, not left Evaluating"
    );
}

/// Claim 2, asserted positively: a `SingleEntry` thunk successfully consumed
/// at the same boundary retains `Evaluating` rather than moving to `Updated`
/// or `Live`. This is the engine's intended terminal state for a consumed
/// `SingleEntry` thunk, and a future change that makes this `Updated` (or
/// anything else) must fail this test, not pass it silently.
#[test]
fn single_entry_consumed_thunk_retains_evaluating_after_success() {
    let program = captured_effect_request_caf(UpdatePolicy::SingleEntry);
    let mut invocation = RawForce::new(&program);
    invocation.install_capture(0, 9701);
    invocation.install_capture(1, 9702);
    assert_eq!(invocation.force(), CallStatus::Success);
    assert_eq!(
        invocation.state(),
        DescriptorState::Evaluating,
        "SingleEntry must retain Evaluating on a successfully consumed thunk"
    );
}

/// `caf_program` (from `entry_tests`) already exercises this shape without
/// the `E` framing; confirm both update policies still resolve to the exact
/// states above through the ordinary generic single-field CAF, so this
/// module's claims are not an artifact of the specific `E` layout chosen
/// above.
#[test]
fn retention_and_settlement_hold_for_the_generic_caf_shape_too() {
    let memoize = caf_program(0, false, UpdatePolicy::Memoize);
    let mut memoize_invocation = RawForce::new(&memoize);
    assert_eq!(memoize_invocation.force(), CallStatus::Success);
    assert_eq!(memoize_invocation.state(), DescriptorState::Updated);

    let single_entry = caf_program(0, false, UpdatePolicy::SingleEntry);
    let mut single_entry_invocation = RawForce::new(&single_entry);
    assert_eq!(single_entry_invocation.force(), CallStatus::Success);
    assert_eq!(single_entry_invocation.state(), DescriptorState::Evaluating);
}
