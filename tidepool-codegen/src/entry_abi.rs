use cranelift_codegen::ir::{self, types, AbiParam, Type};
use cranelift_codegen::isa::CallConv;
use tidepool_repr::execution_schema::{
    Architecture, Endianness, LayoutError, ResultContract, RuntimeRep,
    Signature as SemanticSignature, StorageLayout, TargetDescriptor,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EnvironmentMode {
    Absent,
    Captured,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NativeAbiProfile {
    target: TargetDescriptor,
    result_register_components: usize,
}

impl NativeAbiProfile {
    pub fn new(
        target: TargetDescriptor,
        result_register_components: usize,
    ) -> Result<Self, AbiError> {
        if !matches!(
            target.architecture,
            Architecture::X86_64 | Architecture::Aarch64
        ) || target.endianness != Endianness::Little
            || target.pointer_width != 64
            || target.word_width != 64
        {
            return Err(AbiError::UnsupportedTarget(target));
        }
        Ok(Self {
            target,
            result_register_components,
        })
    }

    pub fn target(&self) -> &TargetDescriptor {
        &self.target
    }

    pub fn pointer_type(&self) -> Type {
        types::I64
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ResultTransport {
    Registers,
    CallerArea(StorageLayout),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EntryAbi {
    semantic_arguments: Vec<RuntimeRep>,
    physical_arguments: Vec<RuntimeRep>,
    semantic_results: ResultContract,
    physical_results: Vec<RuntimeRep>,
    environment: EnvironmentMode,
    result_layout: StorageLayout,
    result_transport: ResultTransport,
}

impl EntryAbi {
    /// Connected entries let Cranelift assign register and implicit-sret
    /// components. There is no explicit payload-area argument between entries.
    pub fn lower_internal(
        profile: &NativeAbiProfile,
        signature: &SemanticSignature,
        environment: EnvironmentMode,
    ) -> Result<Self, AbiError> {
        let mut abi = Self::lower(profile, signature, environment)?;
        abi.result_transport = ResultTransport::Registers;
        Ok(abi)
    }

    pub fn lower(
        profile: &NativeAbiProfile,
        signature: &SemanticSignature,
        environment: EnvironmentMode,
    ) -> Result<Self, AbiError> {
        if signature.results.is_caller_result() {
            return Err(AbiError::UninstantiatedResult);
        }
        let physical_arguments = signature
            .arguments
            .iter()
            .copied()
            .filter(|rep| *rep != RuntimeRep::Void)
            .collect();
        // NoSuccess has a status-only native ABI. Keep its semantic contract
        // separately: an empty physical payload does not mean successful Void.
        let returned_reps = signature.results.returned_reps().unwrap_or(&[]);
        let physical_results: Vec<_> = returned_reps
            .iter()
            .copied()
            .filter(|rep| *rep != RuntimeRep::Void)
            .collect();
        let result_layout = StorageLayout::for_reps(profile.target(), returned_reps)?;
        let result_components = cranelift_components(&physical_results, profile.pointer_type())?;
        // The status discriminant consumes one component of the verified result
        // budget. A caller area transports the entire result vector when it does
        // not fit; callers never mix register and area payloads.
        let result_transport = if result_components
            .len()
            .checked_add(1)
            .ok_or(AbiError::ComponentOverflow)?
            <= profile.result_register_components
        {
            ResultTransport::Registers
        } else {
            ResultTransport::CallerArea(result_layout.clone())
        };
        Ok(Self {
            semantic_arguments: signature.arguments.clone(),
            physical_arguments,
            semantic_results: signature.results.clone(),
            physical_results,
            environment,
            result_layout,
            result_transport,
        })
    }

    pub fn semantic_arguments(&self) -> &[RuntimeRep] {
        &self.semantic_arguments
    }

    pub fn physical_arguments(&self) -> &[RuntimeRep] {
        &self.physical_arguments
    }

    pub fn semantic_results(&self) -> &ResultContract {
        &self.semantic_results
    }

    pub fn physical_results(&self) -> &[RuntimeRep] {
        &self.physical_results
    }

    pub fn environment(&self) -> EnvironmentMode {
        self.environment
    }

    pub fn result_transport(&self) -> &ResultTransport {
        &self.result_transport
    }

    pub fn result_layout(&self) -> &StorageLayout {
        &self.result_layout
    }

    /// Lower the checked semantic ABI to the one Cranelift signature used for
    /// definitions, calls and Rust adapters. The status discriminant is always
    /// the first result; payload results are present only for register transport.
    pub fn cranelift_signature(
        &self,
        profile: &NativeAbiProfile,
        call_conv: CallConv,
    ) -> Result<ir::Signature, AbiError> {
        let pointer = profile.pointer_type();
        let mut signature = ir::Signature::new(call_conv);
        signature.params.push(AbiParam::new(pointer)); // vmctx
        if self.environment == EnvironmentMode::Captured {
            signature.params.push(AbiParam::new(pointer));
        }
        if matches!(self.result_transport, ResultTransport::CallerArea(_)) {
            signature.params.push(AbiParam::new(pointer));
        }
        signature.params.extend(
            cranelift_components(&self.physical_arguments, pointer)?
                .into_iter()
                .map(AbiParam::new),
        );
        signature.returns.push(AbiParam::new(types::I32));
        if matches!(self.result_transport, ResultTransport::Registers) {
            signature.returns.extend(
                cranelift_components(&self.physical_results, pointer)?
                    .into_iter()
                    .map(AbiParam::new),
            );
        }
        Ok(signature)
    }

    /// Signature of the platform-C boundary which owns the call to a Tail
    /// entry. Adapters always receive a result area, even when the Tail entry
    /// returns its payload in registers: the adapter stores successful payload
    /// components before returning the status to Rust. Rust therefore never
    /// calls a Tail address or depends on a platform's multi-return extension.
    pub fn platform_adapter_signature(
        &self,
        profile: &NativeAbiProfile,
        call_conv: CallConv,
    ) -> Result<ir::Signature, AbiError> {
        let pointer = profile.pointer_type();
        let mut signature = ir::Signature::new(call_conv);
        signature.params.push(AbiParam::new(pointer)); // vmctx
        if self.environment == EnvironmentMode::Captured {
            signature.params.push(AbiParam::new(pointer));
        }
        signature.params.push(AbiParam::new(pointer)); // result area
        signature.params.extend(
            cranelift_components(&self.physical_arguments, pointer)?
                .into_iter()
                .map(AbiParam::new),
        );
        signature.returns.push(AbiParam::new(types::I32));
        Ok(signature)
    }
}

fn cranelift_components(reps: &[RuntimeRep], pointer: Type) -> Result<Vec<Type>, AbiError> {
    let mut components = Vec::new();
    for rep in reps {
        match rep {
            RuntimeRep::Void => {}
            RuntimeRep::LiftedRef | RuntimeRep::UnliftedRef | RuntimeRep::Address => {
                components.push(pointer);
            }
            RuntimeRep::Int(8) | RuntimeRep::Word(8) => components.push(types::I8),
            RuntimeRep::Int(16) | RuntimeRep::Word(16) => components.push(types::I16),
            RuntimeRep::Int(32) | RuntimeRep::Word(32) => components.push(types::I32),
            RuntimeRep::Int(64) | RuntimeRep::Word(64) => components.push(types::I64),
            RuntimeRep::Float(32) => components.push(types::F32),
            RuntimeRep::Float(64) => components.push(types::F64),
            other => return Err(AbiError::UnsupportedRepresentation(*other)),
        }
    }
    Ok(components)
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum AbiError {
    #[error("caller-chosen result representation has not been instantiated")]
    UninstantiatedResult,
    #[error("unsupported native ABI target {0:?}")]
    UnsupportedTarget(TargetDescriptor),
    #[error("native ABI component count overflow")]
    ComponentOverflow,
    #[error("unsupported native ABI representation {0:?}")]
    UnsupportedRepresentation(RuntimeRep),
    #[error(transparent)]
    Layout(#[from] LayoutError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::CodegenPipeline;
    use cranelift_codegen::ir::{InstBuilder, MemFlags};
    use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
    use cranelift_module::{Linkage, Module};
    use tidepool_heap::execution_descriptor::{EntryMetadata, ObjectDescriptor, ObjectKind};
    use tidepool_repr::execution_schema::Endianness;

    fn x86_64() -> TargetDescriptor {
        TargetDescriptor {
            architecture: Architecture::X86_64,
            endianness: Endianness::Little,
            pointer_width: 64,
            word_width: 64,
            abi: "system-v".into(),
            features: Vec::new(),
        }
    }

    fn aarch64() -> TargetDescriptor {
        TargetDescriptor {
            architecture: Architecture::Aarch64,
            endianness: Endianness::Little,
            pointer_width: 64,
            word_width: 64,
            abi: "aapcs64".into(),
            features: Vec::new(),
        }
    }

    #[test]
    fn shared_abi_and_descriptor_preserve_void_and_root_classes() {
        let reps = vec![
            RuntimeRep::Void,
            RuntimeRep::Word(8),
            RuntimeRep::LiftedRef,
            RuntimeRep::Float(64),
            RuntimeRep::UnliftedRef,
            RuntimeRep::Address,
        ];
        let layout = StorageLayout::for_reps(&x86_64(), &reps).unwrap();
        assert_eq!(
            layout
                .fields()
                .iter()
                .map(|field| field.offset())
                .collect::<Vec<_>>(),
            vec![0, 8, 16, 24, 32]
        );
        assert_eq!(layout.managed_root_offsets(), &[8, 24]);
        assert_eq!((layout.payload_size(), layout.alignment()), (40, 8));

        let descriptor = ObjectDescriptor::new(
            ObjectKind::Pap,
            layout,
            Some(EntryMetadata::new(
                SemanticSignature {
                    arguments: vec![RuntimeRep::LiftedRef],
                    results: ResultContract::Returns(vec![RuntimeRep::LiftedRef]),
                },
                7,
            )),
        )
        .unwrap();
        assert_eq!(descriptor.payload_base(), 8);
        assert_eq!(descriptor.allocation_extent(), 48);
        assert_eq!(descriptor.trace_offsets(), &[16, 32]);

        let signature = SemanticSignature {
            arguments: reps,
            results: ResultContract::Returns(vec![RuntimeRep::LiftedRef, RuntimeRep::Int(64)]),
        };
        let profile = NativeAbiProfile::new(x86_64(), 2).unwrap();
        let abi = EntryAbi::lower(&profile, &signature, EnvironmentMode::Captured).unwrap();
        assert_eq!(abi.semantic_arguments().len(), 6);
        assert_eq!(abi.physical_arguments().len(), 5);
        assert!(matches!(
            abi.result_transport(),
            ResultTransport::CallerArea(_)
        ));
        let clif = abi.cranelift_signature(&profile, CallConv::Tail).unwrap();
        assert_eq!(clif.params.len(), 8); // vmctx + env + area + five stored args
        assert_eq!(clif.returns.len(), 1); // status only for caller-area transport
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn platform_adapter_executes_tail_entry_and_materializes_mixed_results() {
        let signature = SemanticSignature {
            arguments: vec![RuntimeRep::Int(64), RuntimeRep::Float(64)],
            results: ResultContract::Returns(vec![RuntimeRep::Int(64), RuntimeRep::Float(64)]),
        };
        let profile = NativeAbiProfile::new(x86_64(), 5).unwrap();
        let abi = EntryAbi::lower(&profile, &signature, EnvironmentMode::Absent).unwrap();
        assert!(matches!(abi.result_transport(), ResultTransport::Registers));

        let mut pipeline = CodegenPipeline::new(&[]).unwrap();
        let tail_signature = abi.cranelift_signature(&profile, CallConv::Tail).unwrap();
        let tail = pipeline
            .declare_function_with_signature("prepared_tail", Linkage::Local, &tail_signature)
            .unwrap();
        let mut tail_context = pipeline.module.make_context();
        tail_context.func.signature = tail_signature;
        let mut tail_frontend = FunctionBuilderContext::new();
        {
            let mut builder = FunctionBuilder::new(&mut tail_context.func, &mut tail_frontend);
            let block = builder.create_block();
            builder.append_block_params_for_function_params(block);
            builder.switch_to_block(block);
            builder.seal_block(block);
            let params = builder.block_params(block).to_vec();
            let status = builder.ins().iconst(types::I32, 0);
            builder.ins().return_(&[status, params[1], params[2]]);
            builder.finalize();
        }
        pipeline.define_function(tail, &mut tail_context).unwrap();

        let adapter_signature = abi
            .platform_adapter_signature(&profile, pipeline.isa.default_call_conv())
            .unwrap();
        let adapter = pipeline
            .declare_function_with_signature(
                "prepared_adapter",
                Linkage::Export,
                &adapter_signature,
            )
            .unwrap();
        let mut adapter_context = pipeline.module.make_context();
        adapter_context.func.signature = adapter_signature;
        let tail_reference = pipeline
            .module
            .declare_func_in_func(tail, &mut adapter_context.func);
        let mut adapter_frontend = FunctionBuilderContext::new();
        {
            let mut builder =
                FunctionBuilder::new(&mut adapter_context.func, &mut adapter_frontend);
            let block = builder.create_block();
            builder.append_block_params_for_function_params(block);
            builder.switch_to_block(block);
            builder.seal_block(block);
            let params = builder.block_params(block).to_vec();
            let call = builder
                .ins()
                .call(tail_reference, &[params[0], params[2], params[3]]);
            let results = builder.inst_results(call).to_vec();
            builder
                .ins()
                .store(MemFlags::trusted(), results[1], params[1], 0);
            builder
                .ins()
                .store(MemFlags::trusted(), results[2], params[1], 8);
            builder.ins().return_(&[results[0]]);
            builder.finalize();
        }
        pipeline
            .define_function(adapter, &mut adapter_context)
            .unwrap();
        pipeline.finalize().unwrap();

        type Adapter = unsafe extern "C" fn(u64, *mut u8, i64, f64) -> i32;
        // SAFETY: `platform_adapter_signature` is the matching platform-C
        // signature, and the module owner remains alive for this invocation.
        let invoke: Adapter = unsafe { std::mem::transmute(pipeline.get_function_ptr(adapter)) };
        let mut area = [0_u64; 2];
        let status = unsafe { invoke(0, area.as_mut_ptr().cast(), -17, 3.25) };
        assert_eq!(status, 0);
        assert_eq!(area[0] as i64, -17);
        assert_eq!(f64::from_bits(area[1]), 3.25);
    }

    #[test]
    fn register_transport_omits_void_and_prefixes_status() {
        let profile = NativeAbiProfile::new(x86_64(), 4).unwrap();
        let signature = SemanticSignature {
            arguments: vec![RuntimeRep::Void, RuntimeRep::Word(64)],
            results: ResultContract::Returns(vec![RuntimeRep::Int(64)]),
        };
        let abi = EntryAbi::lower(&profile, &signature, EnvironmentMode::Absent).unwrap();
        assert!(matches!(abi.result_transport(), ResultTransport::Registers));
        let clif = abi.cranelift_signature(&profile, CallConv::Tail).unwrap();
        assert_eq!(clif.params.len(), 2); // vmctx + one i64 component
        assert_eq!(clif.returns.len(), 2); // status + one i64 component
    }

    #[test]
    fn no_success_is_status_only_without_becoming_empty_returns() {
        let profile = NativeAbiProfile::new(x86_64(), 4).unwrap();
        let returns_nothing = SemanticSignature {
            arguments: Vec::new(),
            results: ResultContract::Returns(Vec::new()),
        };
        let never_returns = SemanticSignature {
            arguments: Vec::new(),
            results: ResultContract::NoSuccess,
        };
        let ordinary =
            EntryAbi::lower(&profile, &returns_nothing, EnvironmentMode::Absent).unwrap();
        let terminal = EntryAbi::lower(&profile, &never_returns, EnvironmentMode::Absent).unwrap();
        assert_eq!(
            ordinary.semantic_results(),
            &ResultContract::Returns(Vec::new())
        );
        assert_eq!(terminal.semantic_results(), &ResultContract::NoSuccess);
        assert!(ordinary.result_layout().fields().is_empty());
        assert!(terminal.result_layout().fields().is_empty());
        assert_eq!(
            ordinary
                .cranelift_signature(&profile, CallConv::Tail)
                .unwrap()
                .returns
                .len(),
            1
        );
        assert_eq!(
            terminal
                .cranelift_signature(&profile, CallConv::Tail)
                .unwrap()
                .returns
                .len(),
            1
        );
    }

    #[test]
    fn scalar_128_has_no_native_abi_components() {
        for rep in [RuntimeRep::Int(128), RuntimeRep::Word(128)] {
            assert!(matches!(
                cranelift_components(&[rep], types::I64),
                Err(AbiError::UnsupportedRepresentation(rejected)) if rejected == rep
            ));
        }
    }

    #[test]
    fn aarch64_profile_lowers_without_claiming_native_execution() {
        let signature = SemanticSignature {
            arguments: vec![RuntimeRep::Void, RuntimeRep::LiftedRef, RuntimeRep::Int(64)],
            results: ResultContract::Returns(vec![
                RuntimeRep::LiftedRef,
                RuntimeRep::Word(64),
                RuntimeRep::Float(64),
            ]),
        };
        let profile = NativeAbiProfile::new(aarch64(), 2).unwrap();
        let abi = EntryAbi::lower(&profile, &signature, EnvironmentMode::Captured).unwrap();
        assert_eq!(abi.physical_arguments().len(), 2);
        assert!(matches!(
            abi.result_transport(),
            ResultTransport::CallerArea(_)
        ));
    }
}
