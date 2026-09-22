//! Exercise generated settlement while both semispaces and the root survive.

use super::{
    entry_tests::caf_program, safepoint::NativeStackBounds, CompiledProgram, DescriptorMeaning,
};
use crate::prepared_control::{CallStatus, PreparedSafepoint};
use crate::{
    context::VMContext,
    host_fns::RuntimeError,
    machine_state::{MachineDisposition, MachineState},
};
use std::sync::Arc;
use tidepool_heap::execution_descriptor::{
    DescriptorState, ObjectDescriptor, FORWARDING_POINTER_OFFSET,
};
use tidepool_heap::managed_reference::untag;
use tidepool_repr::execution_schema::{
    link_program, testing, Atom, CheckedLayout, ConstructorDecl, ConstructorId, ExprFrame, Group,
    HeapBinding, HeapRhs, MachineImports, OperationDecl, OperationId, OperationIdentity,
    ResultContract, RuntimeRep, Signature, SignatureId, TopBinding, UpdatePolicy, ValueId,
    ValueRef,
};

struct Invocation<'a> {
    program: &'a CompiledProgram,
    machine: Box<MachineState>,
    vmctx: VMContext,
    root: Box<usize>,
    descriptor: Arc<ObjectDescriptor>,
    original: *const u8,
}

impl<'a> Invocation<'a> {
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
        machine.register_prepared_entries(
            std::iter::empty(),
            program
                .thunk_entries
                .iter()
                .map(|&(header, function)| (header, program.pipeline.get_function_ptr(function))),
        );
        machine.set_stack_map_registry(&program.pipeline.stack_maps);
        machine
            .install_prepared_buffer(vec![0_u64; 8], program.descriptors.clone())
            .unwrap();
        let (start, size) = machine.gc_active_range().unwrap();
        unsafe { descriptor.initialize_header(start) };
        let mut root = Box::new(start as usize);
        machine.register_rust_root((&mut *root as *mut usize).cast());
        let mut vmctx = unsafe { VMContext::new(start, start.add(size)) };
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
            original: start,
        }
    }

    fn force(&mut self) -> (CallStatus, usize) {
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
        (
            CallStatus::from_raw(i64::from(status)).unwrap(),
            output as usize,
        )
    }

    fn install_captured_constructor(&mut self) -> usize {
        let capture = self
            .program
            .descriptor_registry
            .values()
            .find_map(|metadata| match &metadata.meaning {
                DescriptorMeaning::Constructor(observation)
                    if observation.identity == tidepool_repr::DataConId(900) =>
                {
                    Some(Arc::clone(&metadata.descriptor))
                }
                _ => None,
            })
            .expect("captured constructor descriptor");
        let object = self.vmctx.alloc_ptr;
        unsafe { capture.initialize_header(object) };
        let stored = self.descriptor.payload().logical_to_stored()[0]
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

    fn captured_slot(&self) -> usize {
        let stored = self.descriptor.payload().logical_to_stored()[0]
            .expect("thunk capture has a managed slot");
        let field = &self.descriptor.payload().fields()[stored as usize];
        unsafe {
            std::ptr::read_unaligned(
                (*self.root as *const u8)
                    .add((self.descriptor.payload_base() + field.offset()) as usize)
                    .cast::<usize>(),
            )
        }
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

fn captured_caf_program(policy: UpdatePolicy) -> CompiledProgram {
    let mut wire = testing::wire_program();
    wire.signatures[0].results = ResultContract::Returns(vec![RuntimeRep::LiftedRef]);
    wire.constructors.push(ConstructorDecl {
        identity: testing::identity("W5", "CapturedUnit"),
        family: testing::identity("W5", "CapturedUnit"),
        host_id: tidepool_repr::DataConId(900),
        result_rep: RuntimeRep::LiftedRef,
        tag: 1,
        family_size: 1,
        strict_fields: vec![],
        field_reps: vec![],
        layout: CheckedLayout {
            fields: vec![],
            alignment: 1,
            payload_size: 0,
            root_mask: vec![],
        },
    });
    wire.expressions.nodes = vec![ExprFrame::Return(vec![Atom::Ref(ValueRef::Local(
        ValueId(1),
    ))])];
    wire.bindings = vec![Group::Recursive(vec![
        TopBinding {
            identity: testing::identity("W5", "captured-thunk"),
            binding: HeapBinding {
                id: ValueId(0),
                rhs: HeapRhs::Thunk {
                    signature: SignatureId(0),
                    update: policy,
                    captures: vec![ValueRef::Local(ValueId(1))],
                    body: 0,
                },
            },
        },
        TopBinding {
            identity: testing::identity("W5", "captured-constructor"),
            binding: HeapBinding {
                id: ValueId(1),
                rhs: HeapRhs::Constructor {
                    constructor: ConstructorId(0),
                    fields: vec![],
                },
            },
        },
    ])];
    let linked = link_program(testing::prepare(wire).unwrap(), &MachineImports::default()).unwrap();
    CompiledProgram::compile(&linked).unwrap()
}

fn captured_raise_io_caf_program(policy: UpdatePolicy) -> CompiledProgram {
    let mut wire = testing::wire_program();
    wire.signatures[0].results = ResultContract::Returns(vec![RuntimeRep::LiftedRef]);
    wire.signatures.push(Signature {
        arguments: vec![RuntimeRep::LiftedRef, RuntimeRep::Void],
        results: ResultContract::Returns(vec![RuntimeRep::LiftedRef]),
    });
    wire.operations.push(OperationDecl {
        identity: OperationIdentity::PrimOp("raiseIO#".into()),
        signature: SignatureId(1),
    });
    wire.constructors.push(ConstructorDecl {
        identity: testing::identity("W5", "RaisedException"),
        family: testing::identity("W5", "RaisedException"),
        host_id: tidepool_repr::DataConId(900),
        result_rep: RuntimeRep::LiftedRef,
        tag: 1,
        family_size: 1,
        strict_fields: vec![],
        field_reps: vec![],
        layout: CheckedLayout {
            fields: vec![],
            alignment: 1,
            payload_size: 0,
            root_mask: vec![],
        },
    });
    wire.expressions.nodes = vec![ExprFrame::Operation {
        operation: OperationId(0),
        arguments: vec![Atom::Ref(ValueRef::Local(ValueId(1))), Atom::Void],
    }];
    wire.bindings = vec![Group::Recursive(vec![
        TopBinding {
            identity: testing::identity("W5", "raised-thunk"),
            binding: HeapBinding {
                id: ValueId(0),
                rhs: HeapRhs::Thunk {
                    signature: SignatureId(0),
                    update: policy,
                    captures: vec![ValueRef::Local(ValueId(1))],
                    body: 0,
                },
            },
        },
        TopBinding {
            identity: testing::identity("W5", "raised-exception"),
            binding: HeapBinding {
                id: ValueId(1),
                rhs: HeapRhs::Constructor {
                    constructor: ConstructorId(0),
                    fields: vec![],
                },
            },
        },
    ])];
    let linked = link_program(testing::prepare(wire).unwrap(), &MachineImports::default()).unwrap();
    CompiledProgram::compile(&linked).unwrap()
}

impl Drop for Invocation<'_> {
    fn drop(&mut self) {
        self.machine.clear_gc_state();
        self.machine.clear_stack_map_registry();
    }
}

#[test]
fn w5_a1_update_lands_on_relocated_thunk_and_source_is_forwarded() {
    let program = caf_program(3, false, UpdatePolicy::Memoize);
    let mut invocation = Invocation::new(&program);
    let (status, result) = invocation.force();
    assert_eq!(status, CallStatus::Success);
    assert_eq!(
        invocation.machine.gc_generation(),
        1,
        "keep the source semispace owned and unreused"
    );
    assert_ne!(*invocation.root, invocation.original as usize);
    assert_eq!(invocation.state(), DescriptorState::Updated);
    let source_state = unsafe {
        invocation.descriptor.state(
            invocation.original,
            invocation.descriptor.allocation_extent() as usize,
        )
    }
    .unwrap();
    assert_eq!(source_state, DescriptorState::Forwarded);
    let target = unsafe { ((*invocation.root + FORWARDING_POINTER_OFFSET) as *const usize).read() };
    assert_eq!(target, result);
    assert_ne!(untag(result), 0);
}

#[test]
fn w5_a1_cancellation_settles_live_and_does_not_publish_output() {
    for policy in [UpdatePolicy::Memoize, UpdatePolicy::SingleEntry] {
        for (point, occurrence) in [
            (PreparedSafepoint::FunctionEntry, 1),
            (PreparedSafepoint::ThunkEntry, 2),
            (PreparedSafepoint::Allocation, 1),
            (PreparedSafepoint::ThunkCommit, 1),
        ] {
            let program = caf_program(3, false, policy);
            let mut invocation = Invocation::new(&program);
            invocation
                .machine
                .fail_prepared_at(point, occurrence, RuntimeError::Cancelled);
            let (status, output) = invocation.force();
            assert_eq!(status, CallStatus::Cancelled, "{point:?}");
            assert_eq!(
                output, 0xdead_beef,
                "failed force must not publish a payload"
            );
            assert_eq!(invocation.state(), DescriptorState::Live, "{point:?}");
            assert_eq!(
                invocation.machine.take_runtime_error(),
                Some(RuntimeError::Cancelled)
            );
            assert_eq!(
                invocation.force().0,
                CallStatus::Success,
                "a settled reusable thunk must permit retry: {point:?} {policy:?}"
            );
        }
    }
}

#[test]
fn w5_a1_cancellation_preserves_captured_managed_value_for_retry() {
    for policy in [UpdatePolicy::Memoize, UpdatePolicy::SingleEntry] {
        for (point, occurrence) in [
            (PreparedSafepoint::FunctionEntry, 1),
            (PreparedSafepoint::ThunkEntry, 2),
            (PreparedSafepoint::ThunkCommit, 1),
        ] {
            let program = captured_caf_program(policy);
            let mut invocation = Invocation::new(&program);
            let captured = invocation.install_captured_constructor();
            invocation
                .machine
                .fail_prepared_at(point, occurrence, RuntimeError::Cancelled);

            let (status, output) = invocation.force();
            assert_eq!(status, CallStatus::Cancelled, "{policy:?} {point:?}");
            assert_eq!(output, 0xdead_beef);
            assert_eq!(invocation.state(), DescriptorState::Live);
            assert_eq!(invocation.captured_slot(), captured);
            assert_eq!(
                invocation.machine.take_runtime_error(),
                Some(RuntimeError::Cancelled)
            );

            let (status, result) = invocation.force();
            assert_eq!(status, CallStatus::Success, "{policy:?} {point:?}");
            assert_eq!(result, captured);
            assert_eq!(
                invocation.state(),
                match policy {
                    UpdatePolicy::Memoize => DescriptorState::Updated,
                    UpdatePolicy::SingleEntry => DescriptorState::Evaluating,
                },
                "{policy:?} {point:?}",
            );
        }
    }
}

#[test]
fn raise_io_restores_caf_capture_and_preserves_first_cause_on_retry() {
    for policy in [UpdatePolicy::Memoize, UpdatePolicy::SingleEntry] {
        let program = captured_raise_io_caf_program(policy);
        let mut invocation = Invocation::new(&program);
        let captured = invocation.install_captured_constructor();

        for _ in 0..2 {
            let (status, output) = invocation.force();
            assert_eq!(status, CallStatus::LanguageFailure, "{policy:?}");
            assert_eq!(output, 0xdead_beef, "raised call published an output");
            assert_eq!(invocation.state(), DescriptorState::Live, "{policy:?}");
            assert_eq!(invocation.captured_slot(), captured, "{policy:?}");
            assert_eq!(
                invocation.machine.take_runtime_error(),
                Some(RuntimeError::RaisedException),
                "settlement replaced the first cause for {policy:?}",
            );
        }
    }
}

#[test]
fn w5_a1_language_and_stack_failure_restore_thunks_for_retry() {
    for cause in [RuntimeError::UserError, RuntimeError::StackOverflow] {
        for policy in [UpdatePolicy::Memoize, UpdatePolicy::SingleEntry] {
            let program = caf_program(0, false, policy);
            let mut invocation = Invocation::new(&program);
            invocation
                .machine
                .fail_prepared_at(PreparedSafepoint::ThunkCommit, 1, cause.clone());
            let (status, output) = invocation.force();
            assert_eq!(status, CallStatus::LanguageFailure);
            assert_eq!(output, 0xdead_beef);
            assert_eq!(invocation.state(), DescriptorState::Live);
            assert_eq!(invocation.machine.take_runtime_error(), Some(cause.clone()));
            assert_eq!(invocation.force().0, CallStatus::Success);
        }
    }
}

#[test]
fn w5_a1_native_recursive_entry_reaches_typed_stack_bound() {
    // Compilation and generated execution both happen in the child. In
    // particular, no !Send compiled owner is moved between native threads.
    std::thread::Builder::new()
        .stack_size(512 * 1024)
        .spawn(|| {
            use tidepool_repr::execution_schema::*;
            let mut wire = testing::wire_program();
            wire.expressions.nodes[0] = ExprFrame::Call {
                callee: Atom::Ref(ValueRef::Local(ValueId(0))),
                signature: SignatureId(0),
                arguments: vec![],
            };
            let Group::NonRecursive(top) = wire.bindings.remove(0) else {
                unreachable!()
            };
            wire.bindings.push(Group::Recursive(vec![top]));
            let linked =
                link_program(testing::prepare(wire).unwrap(), &MachineImports::default()).unwrap();
            let program = CompiledProgram::compile(&linked).unwrap();
            let result = program.run_entry(
                ValueId(0),
                &[],
                &super::RunOptions::default(),
                Arc::new(std::sync::atomic::AtomicBool::new(false)),
            );
            assert!(
                matches!(result, Err(super::ExecutionError::Runtime(failure))
            if failure.cause == RuntimeError::StackOverflow
                && failure.disposition == MachineDisposition::Reusable)
            );
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn w5_a1_join_backedge_cancellation_settles_thunk() {
    use tidepool_repr::execution_schema::*;
    let mut wire = testing::wire_program();
    wire.signatures[0].results = ResultContract::Returns(vec![RuntimeRep::LiftedRef]);
    wire.expressions.nodes = vec![
        ExprFrame::Jump {
            join: JoinId(0),
            arguments: vec![],
        },
        ExprFrame::Jump {
            join: JoinId(0),
            arguments: vec![],
        },
        ExprFrame::LetJoins {
            bindings: Group::Recursive(vec![JoinBinding {
                id: JoinId(0),
                signature: SignatureId(0),
                parameters: vec![],
                body: 0,
            }]),
            body: 1,
        },
    ];
    let Group::NonRecursive(top) = &mut wire.bindings[0] else {
        unreachable!()
    };
    top.binding.rhs = HeapRhs::Thunk {
        signature: SignatureId(0),
        update: UpdatePolicy::Memoize,
        captures: vec![],
        body: 2,
    };
    let linked = link_program(testing::prepare(wire).unwrap(), &MachineImports::default()).unwrap();
    let program = CompiledProgram::compile(&linked).unwrap();
    let mut invocation = Invocation::new(&program);
    for _ in 0..2 {
        invocation.machine.fail_prepared_at(
            PreparedSafepoint::Backedge,
            3,
            RuntimeError::Cancelled,
        );
        let (status, output) = invocation.force();
        assert_eq!(status, CallStatus::Cancelled);
        assert_eq!(output, 0xdead_beef);
        assert_eq!(invocation.state(), DescriptorState::Live);
        assert_eq!(
            invocation.machine.take_runtime_error(),
            Some(RuntimeError::Cancelled)
        );
    }
}

#[test]
fn nonreturning_join_crosses_empty_case_scrutinee_and_cancels() {
    use tidepool_repr::execution_schema::*;
    let mut wire = testing::wire_program();
    wire.signatures[0].results = ResultContract::NoSuccess;
    // GHC's `let x = x in x` puts the recursive bottom jump under an
    // empty case whose nominal scrutinee representation remains lifted.
    wire.expressions.nodes = vec![
        ExprFrame::Jump {
            join: JoinId(0),
            arguments: vec![],
        },
        ExprFrame::Case {
            scrutinee: 0,
            binder: ValueId(1),
            scrutinee_results: ResultContract::Returns(vec![RuntimeRep::LiftedRef]),
            kind: CaseKind::Polymorphic,
            alternatives: vec![],
        },
        ExprFrame::Jump {
            join: JoinId(0),
            arguments: vec![],
        },
        ExprFrame::LetJoins {
            bindings: Group::Recursive(vec![JoinBinding {
                id: JoinId(0),
                signature: SignatureId(0),
                parameters: vec![],
                body: 1,
            }]),
            body: 2,
        },
    ];
    let Group::NonRecursive(top) = &mut wire.bindings[0] else {
        unreachable!()
    };
    top.binding.rhs = HeapRhs::Thunk {
        signature: SignatureId(0),
        update: UpdatePolicy::Memoize,
        captures: vec![],
        body: 3,
    };
    let linked = link_program(testing::prepare(wire).unwrap(), &MachineImports::default()).unwrap();
    let program = CompiledProgram::compile(&linked).unwrap();
    let mut invocation = Invocation::new(&program);
    for _ in 0..2 {
        invocation.machine.fail_prepared_at(
            PreparedSafepoint::Backedge,
            3,
            RuntimeError::Cancelled,
        );
        let (status, output) = invocation.force();
        assert_eq!(status, CallStatus::Cancelled);
        assert_eq!(output, 0xdead_beef);
        assert_eq!(invocation.state(), DescriptorState::Live);
        assert_eq!(
            invocation.machine.take_runtime_error(),
            Some(RuntimeError::Cancelled)
        );
    }
}
