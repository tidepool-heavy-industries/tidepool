//! Direct native entry compilation for validated prepared-STG programs.
//!
//! This is the bounded production seam from `LinkedProgram` to Cranelift. It
//! accepts no `CoreExpr`, never invokes the reference evaluator, and rejects
//! prepared forms that this first emitter slice cannot execute safely.

use std::sync::Arc;

use cranelift_codegen::ir::{self, types, AbiParam, InstBuilder, MemFlags, Value};
use cranelift_codegen::isa::CallConv;
use cranelift_codegen::Context;
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_module::{FuncId, Linkage, Module};
use tidepool_heap::execution_descriptor::{ObjectDescriptor, ObjectKind};
use tidepool_heap::gc::raw::{cheney_copy_descriptors, DescriptorSpace};
use tidepool_repr::execution_schema::{
    Architecture, Atom, ConstructorId, Endianness, Expr, Group, HeapRhs, LinkedProgram, RuntimeRep,
    ScalarLiteral, Signature, StorageLayout, ValueId, ValueRef,
};

use crate::alloc::emit_prepared_alloc_fast_path;
use crate::context::VMContext;
use crate::entry_abi::{EntryAbi, EnvironmentMode, NativeAbiProfile};
use crate::machine_state::{MachineFailure, MachineState};
use crate::pipeline::{CodegenPipeline, PipelineError};
use crate::prepared_calls::{plan_application, ApplicationKind, FlatPap};
use crate::prepared_control::{CallStatus, ControlError};
use crate::prepared_thunks::{PreparedThunk, ThunkEnter};

#[derive(Debug, thiserror::Error)]
pub enum PreparedNativeError {
    #[error("binding {0:?} is missing")]
    MissingBinding(ValueId),
    #[error("native prepared form is not implemented: {0}")]
    Unsupported(&'static str),
    #[error("constructor id {0} is outside the checked table")]
    Constructor(u32),
    #[error("argument count mismatch: expected {expected}, got {actual}")]
    Arguments { expected: usize, actual: usize },
    #[error("native result area or generated object is invalid")]
    ResultArea,
    #[error(transparent)]
    Pipeline(#[from] PipelineError),
    #[error("descriptor/collection failure: {0}")]
    Descriptor(String),
    #[error("{cause}", cause = .0.cause)]
    Runtime(MachineFailure),
    #[error(transparent)]
    Control(#[from] ControlError),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NativeConstructor {
    pub constructor: ConstructorId,
    pub fields: Vec<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CollectionEvidence {
    pub result: NativeConstructor,
    pub bytes_copied: usize,
    pub root_moved: bool,
}

#[derive(Clone, Copy)]
enum FieldSource {
    Immediate(u64),
    Parameter(usize),
}

/// Owns the adapter, Tail entry, descriptor, and their executable allocation.
/// The pinned profile uses caller-area transport for the single managed result.
pub struct PreparedNativeProgram {
    pipeline: CodegenPipeline,
    adapter: FuncId,
    binding: ValueId,
    argument_reps: Vec<RuntimeRep>,
    constructor: ConstructorId,
    descriptor: Arc<ObjectDescriptor>,
}

impl PreparedNativeProgram {
    pub fn compile(linked: &LinkedProgram) -> Result<Self, PreparedNativeError> {
        Self::compile_binding(linked, linked.prepared().entry())
    }

    pub fn compile_binding(
        linked: &LinkedProgram,
        binding: ValueId,
    ) -> Result<Self, PreparedNativeError> {
        let prepared = linked.prepared();
        let target = &prepared.envelope().target;
        let host_matches = cfg!(all(target_os = "linux", target_arch = "x86_64"))
            && target.architecture == Architecture::X86_64;
        if !host_matches
            || target.endianness != Endianness::Little
            || target.pointer_width != 64
            || !matches!(target.abi.as_str(), "sysv64" | "system-v")
        {
            return Err(PreparedNativeError::Unsupported(
                "artifact target does not match the pinned native host profile",
            ));
        }
        // One component is reserved for status, forcing the managed result
        // through the one caller-area contract on both supported profiles.
        let profile = NativeAbiProfile::new(target.clone(), 1)
            .map_err(|error| PreparedNativeError::Descriptor(error.to_string()))?;
        let top = prepared
            .bindings()
            .iter()
            .flat_map(group_items)
            .find(|top| top.binding.id == binding)
            .ok_or(PreparedNativeError::MissingBinding(binding))?;

        let (signature, parameters, constructor, fields) = match &top.binding.rhs {
            HeapRhs::Constructor {
                constructor,
                fields,
            } => (
                Signature {
                    arguments: Vec::new(),
                    results: vec![RuntimeRep::LiftedRef],
                },
                &[][..],
                *constructor,
                fields.as_slice(),
            ),
            HeapRhs::Function {
                signature,
                parameters,
                body,
                ..
            } => {
                let Expr::Construct {
                    constructor,
                    fields,
                } = body.as_ref()
                else {
                    return Err(PreparedNativeError::Unsupported(
                        "function body other than direct constructor",
                    ));
                };
                let signature = prepared
                    .signatures()
                    .get(signature.0 as usize)
                    .ok_or(PreparedNativeError::Unsupported(
                        "missing function signature",
                    ))?
                    .clone();
                (
                    signature,
                    parameters.as_slice(),
                    *constructor,
                    fields.as_slice(),
                )
            }
            _ => return Err(PreparedNativeError::Unsupported("non-function binding")),
        };
        let abi = EntryAbi::lower(&profile, &signature, EnvironmentMode::Absent)
            .map_err(|error| PreparedNativeError::Descriptor(error.to_string()))?;
        let plan = plan_application(&abi, &signature.arguments)
            .map_err(|_| PreparedNativeError::Unsupported("invalid checked application"))?;
        if plan.kind() != &ApplicationKind::Exact {
            return Err(PreparedNativeError::Unsupported(
                "non-exact entry application",
            ));
        }
        if signature.results != [RuntimeRep::LiftedRef] {
            return Err(PreparedNativeError::Unsupported(
                "non-reference function result",
            ));
        }
        if signature.arguments.iter().any(|rep| {
            !matches!(
                rep,
                RuntimeRep::Void
                    | RuntimeRep::LiftedRef
                    | RuntimeRep::UnliftedRef
                    | RuntimeRep::Address
                    | RuntimeRep::Int(64)
                    | RuntimeRep::Word(64)
            )
        }) {
            return Err(PreparedNativeError::Unsupported(
                "native invocation component other than one 64-bit register",
            ));
        }

        let declaration = prepared
            .constructors()
            .get(constructor.0 as usize)
            .ok_or(PreparedNativeError::Constructor(constructor.0))?;
        if declaration.field_reps.len() != fields.len() {
            return Err(PreparedNativeError::ResultArea);
        }
        let layout = StorageLayout::for_reps(target, &declaration.field_reps)
            .map_err(|error| PreparedNativeError::Descriptor(error.to_string()))?;
        let descriptor = Arc::new(
            ObjectDescriptor::new(ObjectKind::Constructor, layout, None)
                .map_err(|error| PreparedNativeError::Descriptor(error.to_string()))?,
        );
        if descriptor.allocation_alignment() != 8
            || descriptor.allocation_extent() < 16
            || descriptor.allocation_extent() % 8 != 0
        {
            return Err(PreparedNativeError::Unsupported(
                "generated object requires an eight-byte-aligned descriptor layout",
            ));
        }
        if descriptor
            .payload()
            .fields()
            .iter()
            .any(|field| field.size() > 8)
        {
            return Err(PreparedNativeError::Unsupported(
                "generated object needs a wider field component",
            ));
        }
        let sources = fields
            .iter()
            .zip(&declaration.field_reps)
            .map(|(atom, rep)| field_source(atom, *rep, parameters))
            .collect::<Result<Vec<_>, _>>()?;

        let mut pipeline = CodegenPipeline::new(&[(
            "prepared_gc_trigger",
            crate::host_fns::prepared_gc_trigger as *const u8,
        )])?;
        let mut prepared_gc_signature = ir::Signature::new(pipeline.isa.default_call_conv());
        prepared_gc_signature.params.push(AbiParam::new(types::I64));
        prepared_gc_signature.params.push(AbiParam::new(types::I64));
        prepared_gc_signature
            .returns
            .push(AbiParam::new(types::I32));
        let prepared_gc = pipeline
            .module
            .declare_function(
                "prepared_gc_trigger",
                Linkage::Import,
                &prepared_gc_signature,
            )
            .map_err(|error| PipelineError::Declaration(error.to_string()))?;
        let tail_signature = abi
            .cranelift_signature(&profile, CallConv::Tail)
            .map_err(|error| PreparedNativeError::Descriptor(error.to_string()))?;
        let tail = pipeline
            .module
            .declare_function("tidepool_prepared_tail", Linkage::Local, &tail_signature)
            .map_err(|error| PipelineError::Declaration(error.to_string()))?;
        let mut tail_context = Context::new();
        tail_context.func.signature = tail_signature;
        emit_tail(
            &mut tail_context,
            &mut pipeline,
            prepared_gc,
            &descriptor,
            &declaration.field_reps,
            &sources,
            &signature.arguments,
        );
        pipeline.define_function(tail, &mut tail_context)?;

        let adapter_signature = abi
            .platform_adapter_signature(&profile, pipeline.isa.default_call_conv())
            .map_err(|error| PreparedNativeError::Descriptor(error.to_string()))?;
        let adapter = pipeline
            .module
            .declare_function(
                "tidepool_prepared_adapter",
                Linkage::Export,
                &adapter_signature,
            )
            .map_err(|error| PipelineError::Declaration(error.to_string()))?;
        let mut adapter_context = Context::new();
        adapter_context.func.signature = adapter_signature;
        emit_adapter(&mut adapter_context, &mut pipeline, tail);
        pipeline.define_function(adapter, &mut adapter_context)?;
        pipeline.finalize()?;
        Ok(Self {
            pipeline,
            adapter,
            binding,
            argument_reps: signature.arguments,
            constructor,
            descriptor,
        })
    }

    pub fn execute(&self) -> Result<NativeConstructor, PreparedNativeError> {
        self.execute_with_arguments(&[])
    }

    pub fn execute_with_arguments(
        &self,
        arguments: &[u64],
    ) -> Result<NativeConstructor, PreparedNativeError> {
        let (object, _, _) = self.execute_object(arguments)?;
        decode_object(self.constructor, &self.descriptor, &object)
    }

    pub fn execute_flat_pap(
        &self,
        pap: &FlatPap,
        suffix: &[u64],
    ) -> Result<NativeConstructor, PreparedNativeError> {
        if pap.target() != self.binding || pap.remaining_semantic() != suffix.len() {
            return Err(PreparedNativeError::Arguments {
                expected: pap.remaining_semantic(),
                actual: suffix.len(),
            });
        }
        let mut arguments = pap
            .prefix()
            .iter()
            .map(|argument| {
                u64::try_from(argument.bits).map_err(|_| {
                    PreparedNativeError::Unsupported("PAP component wider than 64 bits")
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        arguments.extend_from_slice(suffix);
        self.execute_with_arguments(&arguments)
    }

    pub fn execute_memoized(
        &self,
        thunk: &mut PreparedThunk<NativeConstructor, String>,
        arguments: &[u64],
    ) -> Result<NativeConstructor, PreparedNativeError> {
        match thunk.enter() {
            ThunkEnter::Cached(value) => Ok(value),
            ThunkEnter::Evaluate(token) => match self.execute_with_arguments(arguments) {
                Ok(value) => {
                    thunk.complete(token, value.clone()).map_err(|_| {
                        PreparedNativeError::Unsupported("stale thunk completion token")
                    })?;
                    Ok(value)
                }
                Err(error) => {
                    let detail = error.to_string();
                    thunk.fail(token, detail).map_err(|_| {
                        PreparedNativeError::Unsupported("stale thunk failure token")
                    })?;
                    Err(error)
                }
            },
            ThunkEnter::Failed(_) => Err(PreparedNativeError::Unsupported("memoized failure")),
            ThunkEnter::Blackhole => Err(PreparedNativeError::Unsupported("thunk blackhole")),
            ThunkEnter::SingleEntryReentered => Err(PreparedNativeError::Unsupported(
                "single-entry thunk reentered",
            )),
            ThunkEnter::TokenExhausted => {
                Err(PreparedNativeError::Unsupported("thunk token exhausted"))
            }
        }
    }

    pub fn execute_after_collection(
        &self,
        arguments: &[u64],
    ) -> Result<CollectionEvidence, PreparedNativeError> {
        let (mut from, object_offset, from_used) = self.execute_object(arguments)?;
        let from_ptr = from.as_mut_ptr().cast::<u8>();
        let from_len = from.len() * size_of::<u64>();
        let object = unsafe { from_ptr.add(object_offset) };
        let mut space = DescriptorSpace::new([Arc::clone(&self.descriptor)])
            .map_err(|error| PreparedNativeError::Descriptor(error.to_string()))?;
        let mut root = object;
        let roots = [&mut root as *mut *mut u8];
        let mut to = vec![0_u64; from.len()];
        // The collector requires word alignment; every byte of the u64 buffer
        // is initialized and the byte view stays within its allocation.
        let tospace =
            unsafe { std::slice::from_raw_parts_mut(to.as_mut_ptr().cast::<u8>(), from_len) };
        let copied = unsafe {
            cheney_copy_descriptors(&roots, from_ptr, from_used, tospace, &mut space)
                .map_err(|error| PreparedNativeError::Descriptor(error.to_string()))?
        };
        let result = decode_object_from_ptr(self.constructor, &self.descriptor, root)?;
        Ok(CollectionEvidence {
            result,
            bytes_copied: copied.bytes_copied,
            root_moved: root != object,
        })
    }

    /// Finalized safepoints owned by this compiled program. The raw-only
    /// adapter-to-Tail call still has a precise empty map in the inventory.
    pub fn stack_map_count(&self) -> usize {
        self.pipeline.stack_maps.len()
    }

    fn execute_object(
        &self,
        arguments: &[u64],
    ) -> Result<(Vec<u64>, usize, usize), PreparedNativeError> {
        if arguments.len() != self.argument_reps.len() {
            return Err(PreparedNativeError::Arguments {
                expected: self.argument_reps.len(),
                actual: arguments.len(),
            });
        }
        let physical_arguments = arguments
            .iter()
            .zip(&self.argument_reps)
            .filter_map(|(value, rep)| (*rep != RuntimeRep::Void).then_some(*value))
            .collect::<Vec<_>>();
        let extent = self.descriptor.allocation_extent() as usize;
        let machine_state = MachineState::new();
        machine_state.set_stack_map_registry(&self.pipeline.stack_maps);
        if let Err(error) = machine_state.install_prepared_buffer(
            vec![0_u64; extent.div_ceil(8)],
            vec![Arc::clone(&self.descriptor)],
        ) {
            machine_state.clear_stack_map_registry();
            let disposition = error.machine_disposition();
            return Err(PreparedNativeError::Runtime(MachineFailure {
                cause: error,
                disposition,
            }));
        }
        let Some((start, size)) = machine_state.gc_active_range() else {
            machine_state.clear_gc_state();
            machine_state.clear_stack_map_registry();
            return Err(PreparedNativeError::ResultArea);
        };
        let mut vmctx =
            unsafe { VMContext::new(start, start.add(size), crate::host_fns::gc_trigger) };
        vmctx.machine_state = (&machine_state as *const MachineState).cast_mut();
        let mut result_area = [0_u64; 2];
        let pointer = self.pipeline.get_function_ptr(self.adapter);
        let result = (|| {
            let status = unsafe {
                match physical_arguments.as_slice() {
                    [] => {
                        let entry: extern "C" fn(*mut VMContext, *mut u64) -> i32 =
                            std::mem::transmute(pointer);
                        entry(&mut vmctx, result_area.as_mut_ptr())
                    }
                    [argument] => {
                        let entry: extern "C" fn(*mut VMContext, *mut u64, u64) -> i32 =
                            std::mem::transmute(pointer);
                        entry(&mut vmctx, result_area.as_mut_ptr(), *argument)
                    }
                    _ => {
                        return Err(PreparedNativeError::Unsupported(
                            "more than one physical argument",
                        ))
                    }
                }
            };
            let status = CallStatus::from_raw(i64::from(status))?;
            if status == CallStatus::IntegrityFailure {
                machine_state.set_first_cause(crate::host_fns::RuntimeError::BadPointer);
            }
            let machine_status = machine_state.prepared_call_status();
            if status != CallStatus::Success || machine_status != CallStatus::Success {
                if let Some(failure) = machine_state.last_failure() {
                    let _ = machine_state.take_runtime_error();
                    return Err(PreparedNativeError::Runtime(failure));
                }
                return Err(PreparedNativeError::Control(ControlError::CallFailed(
                    if status != CallStatus::Success {
                        status
                    } else {
                        machine_status
                    },
                )));
            }
            let (active_start, active_size) = machine_state
                .gc_active_range()
                .ok_or(PreparedNativeError::ResultArea)?;
            let object = result_area[0] as *mut u8;
            let offset = (object as usize)
                .checked_sub(active_start as usize)
                .filter(|offset| {
                    offset
                        .checked_add(extent)
                        .is_some_and(|end| end <= active_size)
                })
                .ok_or(PreparedNativeError::ResultArea)?;
            Ok(offset)
        })();
        let (buffer, used) = machine_state.reclaim_session_heap(vmctx.alloc_ptr);
        machine_state.clear_gc_state();
        machine_state.clear_stack_map_registry();
        let object_offset = result?;
        let buffer = buffer.ok_or(PreparedNativeError::ResultArea)?;
        if used > buffer.len() * size_of::<u64>() || object_offset + extent > used {
            return Err(PreparedNativeError::ResultArea);
        }
        Ok((buffer, object_offset, used))
    }
}

fn emit_tail(
    context: &mut Context,
    pipeline: &mut CodegenPipeline,
    prepared_gc: FuncId,
    descriptor: &ObjectDescriptor,
    field_reps: &[RuntimeRep],
    sources: &[FieldSource],
    argument_reps: &[RuntimeRep],
) {
    let mut builder_context = FunctionBuilderContext::new();
    let mut builder = FunctionBuilder::new(&mut context.func, &mut builder_context);
    let block = builder.create_block();
    builder.append_block_params_for_function_params(block);
    builder.switch_to_block(block);
    builder.seal_block(block);
    let prepared_gc = pipeline
        .module
        .declare_func_in_func(prepared_gc, builder.func);
    let params = builder.block_params(block).to_vec();
    let vmctx = params[0];
    let result_area = params[1];
    let argument_values = &params[2..];
    for (value, rep) in argument_values.iter().zip(
        argument_reps
            .iter()
            .copied()
            .filter(|rep| *rep != RuntimeRep::Void),
    ) {
        if matches!(rep, RuntimeRep::LiftedRef | RuntimeRep::UnliftedRef) {
            builder.declare_value_needs_stack_map(*value);
        }
    }
    let object = emit_prepared_alloc_fast_path(&mut builder, vmctx, descriptor, prepared_gc);
    let flags = MemFlags::trusted();
    let header = builder
        .ins()
        .iconst(types::I64, descriptor.initial_header_word() as i64);
    builder.ins().store(flags, header, object, 0);
    for ((source, rep), logical) in sources.iter().zip(field_reps).zip(0..) {
        let Some(stored) = descriptor.payload().logical_to_stored()[logical] else {
            continue;
        };
        let field = &descriptor.payload().fields()[stored as usize];
        let value = match source {
            FieldSource::Immediate(word) => immediate(&mut builder, *rep, *word),
            FieldSource::Parameter(index) => {
                let physical = argument_reps[..*index]
                    .iter()
                    .filter(|rep| **rep != RuntimeRep::Void)
                    .count();
                argument_values[physical]
            }
        };
        builder.ins().store(
            flags,
            value,
            object,
            (descriptor.payload_base() + field.offset()) as i32,
        );
    }
    let success = builder.ins().iconst(types::I32, CallStatus::Success as i64);
    builder.ins().store(flags, object, result_area, 0);
    builder.ins().return_(&[success]);
    builder.finalize();
}

fn emit_adapter(context: &mut Context, pipeline: &mut CodegenPipeline, tail: FuncId) {
    let mut builder_context = FunctionBuilderContext::new();
    let mut builder = FunctionBuilder::new(&mut context.func, &mut builder_context);
    let block = builder.create_block();
    builder.append_block_params_for_function_params(block);
    builder.switch_to_block(block);
    builder.seal_block(block);
    let callee = pipeline.module.declare_func_in_func(tail, builder.func);
    let arguments = builder.block_params(block).to_vec();
    let call = builder.ins().call(callee, &arguments);
    let status = builder.inst_results(call)[0];
    builder.ins().return_(&[status]);
    builder.finalize();
}

fn immediate(builder: &mut FunctionBuilder<'_>, rep: RuntimeRep, word: u64) -> Value {
    let ty = match rep {
        RuntimeRep::Int(8) | RuntimeRep::Word(8) => types::I8,
        RuntimeRep::Int(16) | RuntimeRep::Word(16) => types::I16,
        RuntimeRep::Int(32) | RuntimeRep::Word(32) => types::I32,
        _ => types::I64,
    };
    builder.ins().iconst(ty, word as i64)
}

fn field_source(
    atom: &Atom,
    expected: RuntimeRep,
    parameters: &[ValueId],
) -> Result<FieldSource, PreparedNativeError> {
    match atom {
        Atom::Ref(ValueRef::Local(value)) => parameters
            .iter()
            .position(|parameter| parameter == value)
            .map(FieldSource::Parameter)
            .ok_or(PreparedNativeError::Unsupported(
                "non-parameter local constructor field",
            )),
        _ => scalar_word(atom, expected).map(FieldSource::Immediate),
    }
}

fn decode_object(
    constructor: ConstructorId,
    descriptor: &ObjectDescriptor,
    object: &[u64],
) -> Result<NativeConstructor, PreparedNativeError> {
    decode_object_from_ptr(constructor, descriptor, object.as_ptr().cast::<u8>())
}

fn decode_object_from_ptr(
    constructor: ConstructorId,
    descriptor: &ObjectDescriptor,
    object: *const u8,
) -> Result<NativeConstructor, PreparedNativeError> {
    let fields = descriptor
        .payload()
        .fields()
        .iter()
        .map(|field| unsafe {
            let source = object.add((descriptor.payload_base() + field.offset()) as usize);
            match field.size() {
                1 => u64::from(*source),
                2 => u64::from(u16::from_ne_bytes([*source, *source.add(1)])),
                4 => u64::from(std::ptr::read_unaligned(source.cast::<u32>())),
                8 => std::ptr::read_unaligned(source.cast::<u64>()),
                _ => 0,
            }
        })
        .collect();
    Ok(NativeConstructor {
        constructor,
        fields,
    })
}

fn group_items<T>(group: &Group<T>) -> &[T] {
    match group {
        Group::NonRecursive(item) => std::slice::from_ref(item),
        Group::Recursive(items) => items,
    }
}

fn scalar_word(atom: &Atom, expected: RuntimeRep) -> Result<u64, PreparedNativeError> {
    match (atom, expected) {
        (Atom::Scalar(ScalarLiteral::Char(value)), RuntimeRep::Word(32)) => Ok(u64::from(*value)),
        (Atom::Scalar(ScalarLiteral::Int { bytes, .. }), RuntimeRep::Int(_))
        | (Atom::Scalar(ScalarLiteral::Word { bytes, .. }), RuntimeRep::Word(_))
            if bytes.len() <= 8 =>
        {
            let mut word = [0_u8; 8];
            word[8 - bytes.len()..].copy_from_slice(bytes);
            Ok(u64::from_be_bytes(word))
        }
        _ => Err(PreparedNativeError::Unsupported(
            "non-immediate constructor field",
        )),
    }
}

#[cfg(all(test, target_os = "linux", target_arch = "x86_64"))]
mod tests {
    use super::*;
    use tidepool_repr::execution_schema::TargetDescriptor;

    fn target() -> TargetDescriptor {
        TargetDescriptor {
            architecture: Architecture::X86_64,
            endianness: Endianness::Little,
            pointer_width: 64,
            word_width: 64,
            abi: "sysv64".into(),
            features: Vec::new(),
        }
    }

    #[test]
    fn managed_argument_survives_generated_allocation_collection() {
        let target = target();
        let parent = Arc::new(
            ObjectDescriptor::new(
                ObjectKind::Constructor,
                StorageLayout::for_reps(&target, &[RuntimeRep::LiftedRef, RuntimeRep::Address])
                    .unwrap(),
                None,
            )
            .unwrap(),
        );
        let child = Arc::new(
            ObjectDescriptor::new(
                ObjectKind::Constructor,
                StorageLayout::for_reps(&target, &[]).unwrap(),
                None,
            )
            .unwrap(),
        );
        let mut pipeline = CodegenPipeline::new(&[(
            "prepared_gc_trigger",
            crate::host_fns::prepared_gc_trigger as *const u8,
        )])
        .unwrap();
        let mut gc_signature = ir::Signature::new(pipeline.isa.default_call_conv());
        gc_signature.params.push(AbiParam::new(types::I64));
        gc_signature.params.push(AbiParam::new(types::I64));
        gc_signature.returns.push(AbiParam::new(types::I32));
        let gc = pipeline
            .module
            .declare_function("prepared_gc_trigger", Linkage::Import, &gc_signature)
            .unwrap();
        let mut signature = ir::Signature::new(pipeline.isa.default_call_conv());
        signature.params.push(AbiParam::new(types::I64));
        signature.params.push(AbiParam::new(types::I64));
        signature.params.push(AbiParam::new(types::I64));
        signature.params.push(AbiParam::new(types::I64));
        signature.returns.push(AbiParam::new(types::I32));
        let mut tail_signature = signature.clone();
        tail_signature.call_conv = CallConv::Tail;
        let tail = pipeline
            .module
            .declare_function(
                "prepared_managed_argument_gc_tail",
                Linkage::Local,
                &tail_signature,
            )
            .unwrap();
        let mut context = Context::new();
        context.func.signature = tail_signature;
        emit_tail(
            &mut context,
            &mut pipeline,
            gc,
            &parent,
            &[RuntimeRep::LiftedRef, RuntimeRep::Address],
            &[FieldSource::Parameter(0), FieldSource::Parameter(1)],
            &[RuntimeRep::LiftedRef, RuntimeRep::Address],
        );
        let entry = context.func.layout.entry_block().unwrap();
        let branch = context.func.layout.last_inst(entry).unwrap();
        let ir::InstructionData::Brif { blocks, .. } = &context.func.dfg.insts[branch] else {
            panic!("prepared allocation entry must branch to slow and fast paths");
        };
        let slow = blocks[0].block(&context.func.dfg.value_lists);
        let func = &context.func;
        let calls = func
            .layout
            .blocks()
            .flat_map(|block| {
                func.layout
                    .block_insts(block)
                    .filter(move |inst| func.dfg.insts[*inst].opcode().is_call())
                    .map(move |_| block)
            })
            .collect::<Vec<_>>();
        assert_eq!(
            calls,
            vec![slow],
            "only the allocation slow path may call a host function"
        );
        pipeline.define_function(tail, &mut context).unwrap();
        let adapter = pipeline
            .module
            .declare_function("prepared_managed_argument_gc", Linkage::Export, &signature)
            .unwrap();
        let mut adapter_context = Context::new();
        adapter_context.func.signature = signature;
        emit_adapter(&mut adapter_context, &mut pipeline, tail);
        pipeline
            .define_function(adapter, &mut adapter_context)
            .unwrap();
        pipeline.finalize().unwrap();
        assert!(!pipeline.stack_maps.is_empty());

        let machine_state = MachineState::new();
        machine_state.set_stack_map_registry(&pipeline.stack_maps);
        let child_extent = child.allocation_extent() as usize;
        machine_state
            .install_prepared_buffer(
                vec![0_u64; child_extent.div_ceil(size_of::<u64>())],
                vec![Arc::clone(&parent), Arc::clone(&child)],
            )
            .unwrap();
        let (start, size) = machine_state.gc_active_range().unwrap();
        let mut vmctx =
            unsafe { VMContext::new(start, start.add(size), crate::host_fns::gc_trigger) };
        vmctx.machine_state = (&machine_state as *const MachineState).cast_mut();
        unsafe {
            child.initialize_header(start);
            vmctx.alloc_ptr = start.add(child_extent);
        }

        let mut result_area = [0_u64; 1];
        let raw_address = start as u64;
        let raw_address_before_collection = raw_address;
        let code = pipeline.get_function_ptr(adapter);
        let status = unsafe {
            let run: extern "C" fn(*mut VMContext, *mut u64, u64, u64) -> i32 =
                std::mem::transmute(code);
            run(
                &mut vmctx,
                result_area.as_mut_ptr(),
                start as u64,
                raw_address,
            )
        };
        assert_eq!(status, CallStatus::Success as i32);
        assert_eq!(machine_state.prepared_call_status(), CallStatus::Success);
        let (active_start, active_size) = machine_state.gc_active_range().unwrap();
        let parent_ptr = result_area[0] as *mut u8;
        let managed_field = parent.payload().logical_to_stored()[0].unwrap() as usize;
        let address_field = parent.payload().logical_to_stored()[1].unwrap() as usize;
        let child_ptr = unsafe {
            std::ptr::read_unaligned(
                parent_ptr
                    .add(
                        parent.payload_base() as usize
                            + parent.payload().fields()[managed_field].offset() as usize,
                    )
                    .cast::<u64>(),
            ) as *mut u8
        };
        let retained_address = unsafe {
            std::ptr::read_unaligned(
                parent_ptr
                    .add(
                        parent.payload_base() as usize
                            + parent.payload().fields()[address_field].offset() as usize,
                    )
                    .cast::<u64>(),
            )
        };
        assert_ne!(child_ptr, start);
        assert!(child_ptr as usize >= active_start as usize);
        assert!((child_ptr as usize) < active_start as usize + active_size);
        assert_eq!(retained_address, raw_address_before_collection);

        let (buffer, _) = machine_state.reclaim_session_heap(vmctx.alloc_ptr);
        machine_state.clear_gc_state();
        machine_state.clear_stack_map_registry();
        drop(buffer);
    }
}
