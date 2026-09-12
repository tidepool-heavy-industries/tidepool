use cranelift_codegen::ir::{types, AbiParam, InstBuilder, Signature};
use cranelift_codegen::isa::CallConv;
use cranelift_codegen::settings;
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{Linkage, Module};
use tidepool_codegen::entry_abi::{EntryAbi, EnvironmentMode, NativeAbiProfile};
use tidepool_codegen::prepared_calls::{plan_application, ApplicationKind, FlatPap, PapArgument};
use tidepool_repr::execution_schema::{
    Architecture, Endianness, RuntimeRep, Signature as SemanticSignature, TargetDescriptor, ValueId,
};

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

#[test]
fn void_pap_prefix_is_semantic_but_not_physical() {
    let signature = SemanticSignature {
        arguments: vec![RuntimeRep::Void, RuntimeRep::Word(8), RuntimeRep::LiftedRef],
        results: vec![RuntimeRep::Word(8)],
    };
    let pap = FlatPap::new(ValueId(7), signature)
        .extend(&[PapArgument {
            rep: RuntimeRep::Void,
            bits: 0,
        }])
        .unwrap();
    assert_eq!(pap.target(), ValueId(7));
    assert_eq!(pap.remaining_semantic(), 2);
    assert_eq!(pap.prefix().len(), 1);
    assert_eq!(pap.physical_prefix().count(), 0);
    let extended = pap
        .extend(&[PapArgument {
            rep: RuntimeRep::Word(8),
            bits: 3,
        }])
        .unwrap();
    assert_eq!(pap.prefix().len(), 1, "extension mutated shared PAP");
    assert_eq!(extended.physical_prefix().count(), 1);
}

#[test]
fn oversaturated_managed_arguments_are_explicit_roots() {
    let signature = SemanticSignature {
        arguments: vec![RuntimeRep::Int(64)],
        results: vec![RuntimeRep::Int(64)],
    };
    let profile = NativeAbiProfile::new(x86_64(), 3).unwrap();
    let abi = EntryAbi::lower(&profile, &signature, EnvironmentMode::Absent).unwrap();
    let plan = plan_application(
        &abi,
        &[
            RuntimeRep::Int(64),
            RuntimeRep::LiftedRef,
            RuntimeRep::Address,
        ],
    )
    .unwrap();
    assert_eq!(
        plan.kind(),
        &ApplicationKind::Oversaturated {
            pending_semantic: 2
        }
    );
    assert_eq!(plan.pending_root_arguments(), &[1]);
}

#[test]
fn x86_64_c_adapter_calls_tail_entry_from_planned_physical_signature() {
    let semantic = SemanticSignature {
        arguments: vec![RuntimeRep::Void, RuntimeRep::Int(64), RuntimeRep::Int(64)],
        results: vec![RuntimeRep::Int(64)],
    };
    let profile = NativeAbiProfile::new(x86_64(), 3).unwrap();
    let abi = EntryAbi::lower(&profile, &semantic, EnvironmentMode::Absent).unwrap();
    let plan = plan_application(
        &abi,
        &[RuntimeRep::Void, RuntimeRep::Int(64), RuntimeRep::Int(64)],
    )
    .unwrap();
    assert_eq!(plan.kind(), &ApplicationKind::Exact);
    assert_eq!(plan.consumed_semantic(), 3);
    assert_eq!(plan.consumed_physical(), &[1, 2]);

    let flags = settings::Flags::new(settings::builder());
    let isa = cranelift_native::builder().unwrap().finish(flags).unwrap();
    assert_eq!(isa.triple().architecture.to_string(), "x86_64");
    let builder = JITBuilder::with_isa(isa, cranelift_module::default_libcall_names());
    let mut module = JITModule::new(builder);

    let mut tail_sig = Signature::new(CallConv::Tail);
    tail_sig
        .params
        .extend([AbiParam::new(types::I64), AbiParam::new(types::I64)]);
    tail_sig.returns.push(AbiParam::new(types::I64));
    let tail_id = module
        .declare_function("planned_add", Linkage::Local, &tail_sig)
        .unwrap();
    let mut tail_ctx = module.make_context();
    tail_ctx.func.signature = tail_sig.clone();
    {
        let mut fb_ctx = FunctionBuilderContext::new();
        let mut function = FunctionBuilder::new(&mut tail_ctx.func, &mut fb_ctx);
        let block = function.create_block();
        function.append_block_params_for_function_params(block);
        function.switch_to_block(block);
        function.seal_block(block);
        let parameters = function.block_params(block).to_vec();
        let sum = function.ins().iadd(parameters[0], parameters[1]);
        function.ins().return_(&[sum]);
        function.finalize();
    }
    module.define_function(tail_id, &mut tail_ctx).unwrap();

    let mut adapter_sig = module.make_signature();
    adapter_sig
        .params
        .extend([AbiParam::new(types::I64), AbiParam::new(types::I64)]);
    adapter_sig.returns.push(AbiParam::new(types::I64));
    let adapter_id = module
        .declare_function("planned_add_adapter", Linkage::Export, &adapter_sig)
        .unwrap();
    let mut adapter_ctx = module.make_context();
    adapter_ctx.func.signature = adapter_sig;
    {
        let mut fb_ctx = FunctionBuilderContext::new();
        let mut function = FunctionBuilder::new(&mut adapter_ctx.func, &mut fb_ctx);
        let block = function.create_block();
        function.append_block_params_for_function_params(block);
        function.switch_to_block(block);
        function.seal_block(block);
        let callee = module.declare_func_in_func(tail_id, function.func);
        let parameters = function.block_params(block).to_vec();
        let call = function.ins().call(callee, &parameters);
        let result = function.inst_results(call)[0];
        function.ins().return_(&[result]);
        function.finalize();
    }
    module
        .define_function(adapter_id, &mut adapter_ctx)
        .unwrap();
    module.finalize_definitions().unwrap();
    let pointer = module.get_finalized_function(adapter_id);
    let adapter: extern "C" fn(i64, i64) -> i64 = unsafe { std::mem::transmute(pointer) };
    assert_eq!(adapter(19, 23), 42);
}
