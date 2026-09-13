//! Exact deferred stack capabilities under the pinned GHC profile. Membership
//! permits compilation, never execution or a fabricated result. Unknown names
//! and signatures remain unsupported at admission.

use cranelift_codegen::ir::{self, types, AbiParam, InstBuilder, Value};
use cranelift_frontend::FunctionBuilder;
use cranelift_module::{Linkage, Module};
use tidepool_repr::execution_schema::{ResultContract, RuntimeRep, Signature};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(super) enum Capability {
    CloneMyStack,
    DecodeStack,
    CurrentCostCentreStack,
    LookupIpe,
    StackFrames,
    CollectStackTrace,
    CostCentreStrings,
    DecodeStackEntries,
}

impl Capability {
    pub(super) fn name(self) -> &'static str {
        match self {
            Self::CloneMyStack => "ghc:cloneMyStack",
            Self::DecodeStack => "ghc:decodeStack",
            Self::CurrentCostCentreStack => "ghc:getCurrentCCS",
            Self::LookupIpe => "ghc:lookupIPE",
            Self::StackFrames => "ghc:stackFrames",
            Self::CollectStackTrace => "ghc:collectStackTrace",
            Self::CostCentreStrings => "ghc:ccsToStrings",
            Self::DecodeStackEntries => "ghc:decodeStackEntries",
        }
    }
}

pub(super) fn recognize(name: &str, signature: &Signature) -> Option<Capability> {
    use RuntimeRep::*;
    let (capability, arguments, results): (_, &[RuntimeRep], &[RuntimeRep]) = match name {
        "ghc:cloneMyStack" => (Capability::CloneMyStack, &[Void], &[UnliftedRef]),
        "ghc:decodeStack" => (
            Capability::DecodeStack,
            &[UnliftedRef, Void],
            &[UnliftedRef],
        ),
        "ghc:getCurrentCCS" => (
            Capability::CurrentCostCentreStack,
            &[LiftedRef, Void],
            &[Address],
        ),
        "ghc:lookupIPE" => (Capability::LookupIpe, &[Address, Address, Void], &[Word(8)]),
        "ghc:stackFrames" => (Capability::StackFrames, &[LiftedRef], &[LiftedRef]),
        "ghc:collectStackTrace" => (Capability::CollectStackTrace, &[Void], &[LiftedRef]),
        "ghc:ccsToStrings" => (
            Capability::CostCentreStrings,
            &[Address, LiftedRef, Void],
            &[LiftedRef],
        ),
        "ghc:decodeStackEntries" => (
            Capability::DecodeStackEntries,
            &[UnliftedRef, Int(64), Void],
            &[LiftedRef],
        ),
        _ => return None,
    };
    (signature.arguments == arguments
        && matches!(&signature.results, ResultContract::Returns(reps) if reps == results))
    .then_some(capability)
}

/// No heap access or result publication: a known capability is unavailable in
/// this engine, not evidence of corruption. First cause remains machine-owned.
pub(super) unsafe extern "C" fn unsupported(
    vmctx: *mut crate::context::VMContext,
    tag: u64,
) -> i32 {
    use crate::host_fns::RuntimeError;
    let machine = unsafe { crate::machine_state::machine_state(vmctx) };
    if machine.prepared_call_status() != crate::prepared_control::CallStatus::Success {
        return machine.prepared_call_status() as i32;
    }
    let capability = match tag {
        0 => Capability::CloneMyStack,
        1 => Capability::DecodeStack,
        2 => Capability::CurrentCostCentreStack,
        3 => Capability::LookupIpe,
        4 => Capability::StackFrames,
        5 => Capability::CollectStackTrace,
        6 => Capability::CostCentreStrings,
        7 => Capability::DecodeStackEntries,
        _ => {
            machine.set_first_cause(RuntimeError::BadPointer);
            return machine.prepared_call_status() as i32;
        }
    };
    machine.set_first_cause(RuntimeError::UnsupportedCapability(
        capability.name().into(),
    ));
    machine.prepared_call_status() as i32
}

/// Call the shared noncollecting capability failure host and terminate the
/// current path without touching the operation's declared result area.
pub(super) fn emit_unsupported(
    builder: &mut FunctionBuilder<'_>,
    pipeline: &mut crate::pipeline::CodegenPipeline,
    vmctx: Value,
    capability: Capability,
) -> Result<(), super::CompileError> {
    let mut signature = ir::Signature::new(pipeline.isa.default_call_conv());
    signature
        .params
        .extend([AbiParam::new(types::I64), AbiParam::new(types::I64)]);
    signature.returns.push(AbiParam::new(types::I32));
    let host = pipeline
        .module
        .declare_function(
            "prepared_unsupported_capability",
            Linkage::Import,
            &signature,
        )
        .map_err(|error| crate::pipeline::PipelineError::Declaration(error.to_string()))?;
    let host = pipeline.module.declare_func_in_func(host, builder.func);
    let tag = builder.ins().iconst(types::I64, capability as u8 as i64);
    let call = builder.ins().call(host, &[vmctx, tag]);
    let status = builder.inst_results(call)[0];
    super::no_success::emit_status(builder, status);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host_fns::RuntimeError;
    use crate::machine_state::{MachineDisposition, MachineFailure};
    use std::sync::{atomic::AtomicBool, Arc};
    use tidepool_repr::execution_schema::{testing, *};

    fn capability_wire(name: &str, caf: bool) -> WireProgram {
        let result = if name == "ghc:collectStackTrace" {
            RuntimeRep::LiftedRef
        } else {
            RuntimeRep::UnliftedRef
        };
        let mut wire = testing::wire_program();
        wire.signatures[0].results = ResultContract::Returns(vec![result]);
        wire.signatures.push(Signature {
            arguments: vec![RuntimeRep::Void],
            results: ResultContract::Returns(vec![result]),
        });
        wire.operations.push(OperationDecl {
            identity: OperationIdentity::Capability { name: name.into() },
            signature: SignatureId(1),
        });
        wire.expressions.nodes = vec![ExprFrame::Operation {
            operation: OperationId(0),
            arguments: vec![Atom::Void],
        }];
        let Group::NonRecursive(entry) = &mut wire.bindings[0] else {
            unreachable!()
        };
        entry.binding.rhs = if caf {
            HeapRhs::Thunk {
                signature: SignatureId(0),
                update: UpdatePolicy::Memoize,
                captures: vec![],
                body: 0,
            }
        } else {
            HeapRhs::Function {
                signature: SignatureId(0),
                parameters: vec![],
                captures: vec![],
                body: 0,
            }
        };
        wire
    }

    fn compile(
        wire: WireProgram,
    ) -> Result<super::super::CompiledProgram, super::super::CompileError> {
        let prepared = testing::prepare(wire).expect("capability fixture validates");
        let linked = link_program(prepared, &MachineImports::default()).expect("fixture links");
        super::super::CompiledProgram::compile(&linked)
    }

    #[test]
    fn catalog_requires_exact_name_and_signature() {
        use RuntimeRep::*;
        for (name, arguments, results, expected) in [
            (
                "ghc:cloneMyStack",
                vec![Void],
                vec![UnliftedRef],
                Capability::CloneMyStack,
            ),
            (
                "ghc:decodeStack",
                vec![UnliftedRef, Void],
                vec![UnliftedRef],
                Capability::DecodeStack,
            ),
            (
                "ghc:getCurrentCCS",
                vec![LiftedRef, Void],
                vec![Address],
                Capability::CurrentCostCentreStack,
            ),
            (
                "ghc:lookupIPE",
                vec![Address, Address, Void],
                vec![Word(8)],
                Capability::LookupIpe,
            ),
            (
                "ghc:stackFrames",
                vec![LiftedRef],
                vec![LiftedRef],
                Capability::StackFrames,
            ),
            (
                "ghc:collectStackTrace",
                vec![Void],
                vec![LiftedRef],
                Capability::CollectStackTrace,
            ),
            (
                "ghc:ccsToStrings",
                vec![Address, LiftedRef, Void],
                vec![LiftedRef],
                Capability::CostCentreStrings,
            ),
            (
                "ghc:decodeStackEntries",
                vec![UnliftedRef, Int(64), Void],
                vec![LiftedRef],
                Capability::DecodeStackEntries,
            ),
        ] {
            assert!(matches!(
                recognize(
                    name,
                    &Signature {
                        arguments,
                        results: ResultContract::Returns(results),
                    }
                ),
                Some(actual) if actual == expected
            ));
        }
        let exact = Signature {
            arguments: vec![Void],
            results: ResultContract::Returns(vec![UnliftedRef]),
        };
        assert!(recognize("ghc:unknown", &exact).is_none());
        assert!(recognize(
            "ghc:cloneMyStack",
            &Signature {
                arguments: vec![Void],
                results: ResultContract::Returns(vec![LiftedRef]),
            }
        )
        .is_none());
        for rejected in [
            Signature {
                arguments: vec![LiftedRef, Int(64), Void],
                results: ResultContract::Returns(vec![LiftedRef]),
            },
            Signature {
                arguments: vec![UnliftedRef, Int(32), Void],
                results: ResultContract::Returns(vec![LiftedRef]),
            },
            Signature {
                arguments: vec![UnliftedRef, Int(64), Void],
                results: ResultContract::Returns(vec![UnliftedRef]),
            },
            Signature {
                arguments: vec![UnliftedRef, Int(64), Void],
                results: ResultContract::NoSuccess,
            },
        ] {
            assert!(recognize("ghc:decodeStackEntries", &rejected).is_none());
        }
        assert!(recognize(
            "ghc:decodeStackEntries-lookalike",
            &Signature {
                arguments: vec![UnliftedRef, Int(64), Void],
                results: ResultContract::Returns(vec![LiftedRef]),
            },
        )
        .is_none());
    }

    #[test]
    fn unknown_capability_fails_native_admission() {
        assert!(compile(capability_wire("ghc:unknown", false)).is_err());
    }

    #[test]
    fn reached_capability_returns_reusable_failure_without_output() {
        let mut wire = testing::wire_program();
        wire.signatures[0].results = ResultContract::Returns(vec![RuntimeRep::Word(8)]);
        wire.signatures.push(Signature {
            arguments: vec![RuntimeRep::Address, RuntimeRep::Address, RuntimeRep::Void],
            results: ResultContract::Returns(vec![RuntimeRep::Word(8)]),
        });
        wire.operations.push(OperationDecl {
            identity: OperationIdentity::Capability {
                name: "ghc:lookupIPE".into(),
            },
            signature: SignatureId(1),
        });
        wire.expressions.nodes = vec![
            ExprFrame::Operation {
                operation: OperationId(0),
                arguments: vec![
                    Atom::Scalar(ScalarLiteral::Bytes(b"first".to_vec())),
                    Atom::Scalar(ScalarLiteral::Bytes(b"second".to_vec())),
                    Atom::Void,
                ],
            },
            ExprFrame::Return(vec![Atom::Scalar(ScalarLiteral::Word {
                bits: 8,
                bytes: vec![1],
            })]),
            ExprFrame::Case {
                scrutinee: 0,
                binder: ValueId(10),
                scrutinee_results: ResultContract::Returns(vec![RuntimeRep::Word(8)]),
                kind: CaseKind::Primitive(RuntimeRep::Word(8)),
                alternatives: vec![Alternative {
                    pattern: AlternativePattern::Default,
                    binders: vec![],
                    body: 1,
                }],
            },
        ];
        let Group::NonRecursive(entry) = &mut wire.bindings[0] else {
            unreachable!()
        };
        let HeapRhs::Function { body, .. } = &mut entry.binding.rhs else {
            unreachable!()
        };
        *body = 2;
        let program = compile(wire).unwrap();
        assert!(matches!(
            program.run_entry(
                ValueId(0),
                &[],
                &super::super::RunOptions::default(),
                Arc::new(AtomicBool::new(false)),
            ),
            Err(super::super::ExecutionError::Runtime(MachineFailure {
                cause: RuntimeError::UnsupportedCapability(name),
                disposition: MachineDisposition::Reusable,
            })) if name == "ghc:lookupIPE"
        ));
    }

    #[test]
    fn reached_decode_stack_entries_is_reusable_and_publishes_no_result() {
        let mut wire = testing::wire_program();
        wire.signatures[0].results = ResultContract::Returns(vec![RuntimeRep::LiftedRef]);
        wire.signatures.push(Signature {
            arguments: vec![RuntimeRep::Int(64), RuntimeRep::Void],
            results: ResultContract::Returns(vec![RuntimeRep::UnliftedRef]),
        });
        wire.signatures.push(Signature {
            arguments: vec![
                RuntimeRep::UnliftedRef,
                RuntimeRep::Int(64),
                RuntimeRep::Void,
            ],
            results: ResultContract::Returns(vec![RuntimeRep::LiftedRef]),
        });
        wire.operations = vec![
            OperationDecl {
                identity: OperationIdentity::PrimOp("newByteArray#".into()),
                signature: SignatureId(1),
            },
            OperationDecl {
                identity: OperationIdentity::Capability {
                    name: "ghc:decodeStackEntries".into(),
                },
                signature: SignatureId(2),
            },
        ];
        let zero = Atom::Scalar(ScalarLiteral::Int {
            bits: 64,
            bytes: 0_i64.to_be_bytes().to_vec(),
        });
        wire.expressions.nodes = vec![
            ExprFrame::Operation {
                operation: OperationId(0),
                arguments: vec![zero.clone(), Atom::Void],
            },
            ExprFrame::Operation {
                operation: OperationId(1),
                arguments: vec![Atom::Ref(ValueRef::Local(ValueId(10))), zero, Atom::Void],
            },
            ExprFrame::Case {
                scrutinee: 0,
                binder: ValueId(11),
                scrutinee_results: ResultContract::Returns(vec![RuntimeRep::UnliftedRef]),
                kind: CaseKind::MultiValue,
                alternatives: vec![Alternative {
                    pattern: AlternativePattern::Default,
                    binders: vec![ValueId(10)],
                    body: 1,
                }],
            },
        ];
        let Group::NonRecursive(entry) = &mut wire.bindings[0] else {
            unreachable!()
        };
        let HeapRhs::Function { body, .. } = &mut entry.binding.rhs else {
            unreachable!()
        };
        *body = 2;

        let program = compile(wire).unwrap();
        assert!(matches!(
            program.run_entry(
                ValueId(0),
                &[],
                &super::super::RunOptions::default(),
                Arc::new(AtomicBool::new(false)),
            ),
            Err(super::super::ExecutionError::Runtime(MachineFailure {
                cause: RuntimeError::UnsupportedCapability(name),
                disposition: MachineDisposition::Reusable,
            })) if name == "ghc:decodeStackEntries"
        ));

        let machine = crate::machine_state::MachineState::new();
        machine.set_first_cause(RuntimeError::Cancelled);
        let mut vmctx = crate::context::VMContext::new(
            std::ptr::null_mut(),
            std::ptr::null(),
            crate::host_fns::gc_trigger,
        );
        vmctx.machine_state = &machine as *const _ as *mut _;
        assert_eq!(
            unsafe { unsupported(&mut vmctx, Capability::DecodeStackEntries as u8 as u64) },
            crate::prepared_control::CallStatus::Cancelled as i32,
        );
        assert_eq!(machine.take_runtime_error(), Some(RuntimeError::Cancelled));
    }

    #[test]
    fn untaken_capability_alternative_allows_selected_success() {
        let mut wire = testing::wire_program();
        wire.signatures[0].results = ResultContract::Returns(vec![RuntimeRep::Word(8)]);
        wire.signatures.push(Signature {
            arguments: vec![RuntimeRep::Address, RuntimeRep::Address, RuntimeRep::Void],
            results: ResultContract::Returns(vec![RuntimeRep::Word(8)]),
        });
        wire.operations.push(OperationDecl {
            identity: OperationIdentity::Capability {
                name: "ghc:lookupIPE".into(),
            },
            signature: SignatureId(1),
        });
        let byte = |value| {
            Atom::Scalar(ScalarLiteral::Word {
                bits: 8,
                bytes: vec![value],
            })
        };
        wire.expressions.nodes = vec![
            ExprFrame::Operation {
                operation: OperationId(0),
                arguments: vec![
                    Atom::Scalar(ScalarLiteral::Bytes(b"first".to_vec())),
                    Atom::Scalar(ScalarLiteral::Bytes(b"second".to_vec())),
                    Atom::Void,
                ],
            },
            ExprFrame::Return(vec![byte(1)]),
            ExprFrame::Return(vec![byte(0)]),
            ExprFrame::Case {
                scrutinee: 2,
                binder: ValueId(10),
                scrutinee_results: ResultContract::Returns(vec![RuntimeRep::Word(8)]),
                kind: CaseKind::Primitive(RuntimeRep::Word(8)),
                alternatives: vec![
                    Alternative {
                        pattern: AlternativePattern::Literal(ScalarLiteral::Word {
                            bits: 8,
                            bytes: vec![0],
                        }),
                        binders: vec![],
                        body: 1,
                    },
                    Alternative {
                        pattern: AlternativePattern::Default,
                        binders: vec![],
                        body: 0,
                    },
                ],
            },
        ];
        let Group::NonRecursive(entry) = &mut wire.bindings[0] else {
            unreachable!()
        };
        let HeapRhs::Function { body, .. } = &mut entry.binding.rhs else {
            unreachable!()
        };
        *body = 3;
        let result = compile(wire).unwrap().run_entry(
            ValueId(0),
            &[],
            &super::super::RunOptions::default(),
            Arc::new(AtomicBool::new(false)),
        );
        assert!(matches!(
            result,
            Ok(super::super::RunResult { values, .. })
                if matches!(values.as_slice(), [tidepool_bridge::Value::Lit(
                    tidepool_repr::Literal::LitWord(1)
                )])
        ));
    }

    #[test]
    fn capability_caf_retries_twice_and_preserves_first_cause() {
        let mut wire = capability_wire("ghc:collectStackTrace", true);
        let reference_signature = SignatureId(wire.signatures.len() as u32);
        wire.signatures.push(Signature {
            arguments: vec![],
            results: ResultContract::Returns(vec![RuntimeRep::LiftedRef]),
        });
        wire.expressions
            .nodes
            .push(ExprFrame::Return(vec![Atom::Ref(ValueRef::Local(
                ValueId(0),
            ))]));
        wire.bindings.push(Group::NonRecursive(TopBinding {
            identity: testing::identity("Capability", "referenceEntry"),
            binding: HeapBinding {
                id: ValueId(1),
                rhs: HeapRhs::Function {
                    signature: reference_signature,
                    parameters: vec![],
                    captures: vec![],
                    body: 1,
                },
            },
        }));
        let program = compile(wire).unwrap();
        let mut invocation = super::super::invocation::PreparedInvocation::enter(
            &program,
            ValueId(1),
            &[],
            &super::super::RunOptions::default(),
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
        for _ in 0..2 {
            assert!(matches!(
                invocation.observe(10_000),
                Err(super::super::ExecutionError::Runtime(MachineFailure {
                    cause: RuntimeError::UnsupportedCapability(ref name),
                    disposition: MachineDisposition::Reusable,
                })) if name == "ghc:collectStackTrace"
            ));
            invocation.machine.set_first_cause(RuntimeError::UserError);
            assert_eq!(
                invocation.machine.take_runtime_error(),
                Some(RuntimeError::UnsupportedCapability(
                    "ghc:collectStackTrace".into()
                ))
            );
        }
    }

    #[test]
    fn host_rejects_invalid_tag_and_preserves_existing_cause() {
        for existing in [None, Some(RuntimeError::Cancelled)] {
            let machine = crate::machine_state::MachineState::new();
            if let Some(error) = existing.clone() {
                machine.set_first_cause(error);
            }
            let mut vmctx = crate::context::VMContext::new(
                std::ptr::null_mut(),
                std::ptr::null(),
                crate::host_fns::gc_trigger,
            );
            vmctx.machine_state = &machine as *const _ as *mut _;
            unsafe { unsupported(&mut vmctx, u64::MAX) };
            assert_eq!(
                machine.take_runtime_error(),
                Some(existing.unwrap_or(RuntimeError::BadPointer))
            );
        }
    }
}
