//! `dataToTagSmall#` admits exactly LiftedRef -> Int64. Generated Tail entry
//! forces the argument, and its status dominates all uses of the managed
//! result. A noncollecting host returns the descriptor's zero-based family tag
//! through a caller-owned scalar slot; pointer low bits are only evidence.

use crate::prepared_control::CallStatus;
use cranelift_codegen::ir::{types, InstBuilder, MemFlags, Value};
use cranelift_frontend::FunctionBuilder;
use cranelift_module::{FuncId, Module};

/// Inspect only a value already forced by the generated Tail entry. The caller
/// owns a writable scalar slot, which remains untouched on failure.
///
/// # Safety
/// `vmctx` belongs to this prepared invocation, `reference` is its live
/// generated managed result, and `output` points to writable `i64` storage.
pub(super) unsafe extern "C" fn prepared_data_to_tag_small(
    vmctx: *mut crate::context::VMContext,
    reference: usize,
    output: *mut i64,
) -> i32 {
    let machine = unsafe { crate::machine_state::machine_state(vmctx) };
    if machine.prepared_call_status() != CallStatus::Success {
        return machine.prepared_call_status() as i32;
    }
    let result = if output.is_null() {
        Err(crate::host_fns::bad_pointer())
    } else {
        unsafe { machine.prepared_constructor_tag(reference) }
    };
    match result {
        Ok(tag) => {
            unsafe { output.write(tag) };
            CallStatus::Success as i32
        }
        Err(cause) => {
            machine.set_first_cause(cause);
            machine.prepared_call_status() as i32
        }
    }
}

pub(super) fn emit(
    builder: &mut FunctionBuilder<'_>,
    pipeline: &mut crate::pipeline::CodegenPipeline,
    vmctx: Value,
    prepared_enter: FuncId,
    argument: Value,
) -> Result<Vec<Value>, super::CompileError> {
    builder.declare_value_needs_stack_map(argument);
    let entry = pipeline
        .module
        .declare_func_in_func(prepared_enter, builder.func);
    let call = builder.ins().call(entry, &[vmctx, argument]);
    let returned = builder.inst_results(call).to_vec();
    super::arrays::finish_checked_call(builder, returned[0]);
    let evaluated = returned[1];
    builder.declare_value_needs_stack_map(evaluated);

    let host = super::arrays::declare_host(builder, pipeline, "prepared_data_to_tag_small", 3)?;
    let output = super::arrays::output_slot(builder);
    let call = builder.ins().call(host, &[vmctx, evaluated, output]);
    let status = builder.inst_results(call)[0];
    super::arrays::finish_checked_call(builder, status);
    Ok(vec![builder.ins().load(
        types::I64,
        MemFlags::trusted(),
        output,
        0,
    )])
}

#[cfg(test)]
mod tests {
    use super::super::{CompiledProgram, ExecutionError, RunOptions, Unsupported};
    use crate::{host_fns::RuntimeError, machine_state::MachineDisposition};
    use std::sync::{atomic::AtomicBool, Arc};
    use tidepool_repr::{execution_schema::*, Literal};

    fn constructor(tag: u8) -> ConstructorDecl {
        ConstructorDecl {
            identity: testing::identity("DataTag", &format!("C{tag}")),
            family: testing::identity("DataTag", "Seven"),
            host_id: tidepool_repr::DataConId(990 + u64::from(tag)),
            result_rep: RuntimeRep::LiftedRef,
            tag: u32::from(tag),
            family_size: 7,
            field_reps: vec![],
            strict_fields: vec![],
            layout: CheckedLayout {
                fields: vec![],
                alignment: 1,
                payload_size: 0,
                root_mask: vec![],
            },
        }
    }

    fn wire(lazy: bool, garbage: usize) -> WireProgram {
        let mut wire = testing::wire_program();
        wire.signatures.push(Signature {
            arguments: vec![RuntimeRep::LiftedRef],
            results: ResultContract::Returns(vec![RuntimeRep::Int(64)]),
        });
        wire.signatures.push(Signature {
            arguments: vec![],
            results: ResultContract::Returns(vec![RuntimeRep::LiftedRef]),
        });
        wire.operations.push(OperationDecl {
            identity: OperationIdentity::PrimOp("dataToTagSmall#".into()),
            signature: SignatureId(1),
        });
        wire.constructors.extend((1..=7).map(constructor));
        wire.expressions.nodes = vec![ExprFrame::Operation {
            operation: OperationId(0),
            arguments: vec![Atom::Ref(ValueRef::Local(ValueId(1)))],
        }];
        let rhs = if lazy {
            wire.expressions.nodes.push(ExprFrame::Construct {
                constructor: ConstructorId(6),
                fields: vec![],
            });
            let mut body = 1;
            for index in 0..garbage {
                wire.expressions.nodes.push(ExprFrame::Let {
                    bindings: Group::NonRecursive(HeapBinding {
                        id: ValueId(100 + index as u32),
                        rhs: HeapRhs::Constructor {
                            constructor: ConstructorId(0),
                            fields: vec![],
                        },
                    }),
                    body,
                });
                body = wire.expressions.nodes.len() - 1;
            }
            HeapRhs::Thunk {
                signature: SignatureId(2),
                update: UpdatePolicy::Memoize,
                captures: vec![],
                body,
            }
        } else {
            HeapRhs::Constructor {
                constructor: ConstructorId(6),
                fields: vec![],
            }
        };
        wire.bindings.push(Group::NonRecursive(TopBinding {
            identity: testing::identity("DataTag", "argument"),
            binding: HeapBinding {
                id: ValueId(1),
                rhs,
            },
        }));
        wire
    }

    fn compile(wire: WireProgram) -> CompiledProgram {
        let linked =
            link_program(testing::prepare(wire).unwrap(), &MachineImports::default()).unwrap();
        CompiledProgram::compile(&linked).unwrap()
    }

    fn run(
        program: &CompiledProgram,
        nursery_bytes: usize,
        cancelled: bool,
    ) -> Result<super::super::RunResult, ExecutionError> {
        program.run_entry(
            ValueId(0),
            &[],
            &RunOptions {
                nursery_bytes,
                ..RunOptions::default()
            },
            Arc::new(AtomicBool::new(cancelled)),
        )
    }

    #[test]
    fn direct_and_collecting_lazy_constructor_use_family_tag_not_pointer_bits() {
        for (lazy, garbage, nursery) in [(false, 0, 4096), (true, 48, 64)] {
            let result = run(&compile(wire(lazy, garbage)), nursery, false).unwrap();
            if lazy {
                assert!(
                    result.collections > 0,
                    "lazy body must collect before tag inspection"
                );
            }
            assert!(matches!(
                result.values.as_slice(),
                [tidepool_bridge::HaskellValue::Lit(Literal::LitInt(6))]
            ));
        }
    }

    #[test]
    fn exact_signature_is_required() {
        let mut wire = wire(false, 0);
        wire.signatures[0].results = ResultContract::Returns(vec![RuntimeRep::Word(64)]);
        wire.signatures[1].results = ResultContract::Returns(vec![RuntimeRep::Word(64)]);
        let linked =
            link_program(testing::prepare(wire).unwrap(), &MachineImports::default()).unwrap();
        assert!(matches!(
            CompiledProgram::compile(&linked),
            Err(super::super::CompileError::Unsupported(
                Unsupported::Operation { .. }
            ))
        ));
    }

    #[test]
    fn function_argument_is_a_typed_constructor_failure() {
        let mut wire = wire(false, 0);
        wire.expressions.nodes.push(ExprFrame::Construct {
            constructor: ConstructorId(0),
            fields: vec![],
        });
        let Group::NonRecursive(argument) = &mut wire.bindings[1] else {
            unreachable!()
        };
        argument.binding.rhs = HeapRhs::Function {
            signature: SignatureId(2),
            parameters: vec![],
            captures: vec![],
            body: 1,
        };
        let error = run(&compile(wire), 4096, false).unwrap_err();
        assert!(
            matches!(&error, ExecutionError::Runtime(failure) if failure.cause == RuntimeError::ExpectedConstructor && failure.disposition == MachineDisposition::Unavailable),
            "{error:?}"
        );
    }

    #[test]
    fn cancelled_argument_publishes_no_scalar() {
        let error = run(&compile(wire(true, 8)), 64, true).unwrap_err();
        assert!(
            matches!(error, ExecutionError::Runtime(failure) if failure.cause == RuntimeError::Cancelled && failure.disposition == MachineDisposition::Reusable)
        );
    }

    #[test]
    fn raised_argument_publishes_no_scalar() {
        let mut wire = wire(true, 0);
        let failure_signature = SignatureId(wire.signatures.len() as u32);
        wire.signatures.push(Signature {
            arguments: vec![RuntimeRep::Void],
            results: ResultContract::NoSuccess,
        });
        wire.operations.push(OperationDecl {
            identity: OperationIdentity::PrimOp("raiseDivZero#".into()),
            signature: failure_signature,
        });
        wire.expressions.nodes[1] = ExprFrame::Operation {
            operation: OperationId(1),
            arguments: vec![Atom::Void],
        };
        let Group::NonRecursive(argument) = &mut wire.bindings[1] else {
            unreachable!()
        };
        let HeapRhs::Thunk { body, .. } = &mut argument.binding.rhs else {
            unreachable!()
        };
        *body = 1;
        let error = run(&compile(wire), 4096, false).unwrap_err();
        assert!(
            matches!(error, ExecutionError::Runtime(failure) if failure.cause == RuntimeError::DivisionByZero && failure.disposition == MachineDisposition::Reusable)
        );
    }
}
