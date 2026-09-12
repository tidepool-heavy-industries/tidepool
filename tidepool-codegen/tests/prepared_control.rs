#[path = "../src/prepared_control.rs"]
mod prepared_control;

use cranelift_codegen::ir::{types, AbiParam, InstBuilder, MemFlags, Signature};
use cranelift_codegen::isa::CallConv;
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{Linkage, Module};
use prepared_control::{CallStatus, ControlError, JoinContract, NativeValue, ResultArea};
use tidepool_repr::execution_schema::{
    Architecture, Endianness, RuntimeRep, Signature as SemanticSignature, TargetDescriptor,
};

fn target() -> TargetDescriptor {
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
fn result_area_publishes_atomically_and_preserves_raw_ref_classes() {
    let reps = [
        RuntimeRep::Void,
        RuntimeRep::LiftedRef,
        RuntimeRep::Int(64),
        RuntimeRep::Address,
        RuntimeRep::Word(8),
        RuntimeRep::Float(64),
    ];
    let mut area = ResultArea::new(&target(), &reps).unwrap();
    assert_eq!(area.layout().managed_root_offsets(), &[0]);
    area.write(1, NativeValue::ManagedRef(0x1000)).unwrap();
    area.write(
        2,
        NativeValue::Int {
            bits: 64,
            value: -7,
        },
    )
    .unwrap();
    area.write(3, NativeValue::Address(0x1000)).unwrap();
    area.write(4, NativeValue::Word { bits: 8, value: 3 })
        .unwrap();
    area.write(
        5,
        NativeValue::Float {
            bits: 64,
            bytes: 1.5f64.to_bits().to_le_bytes().to_vec(),
        },
    )
    .unwrap();
    let values = area.finish(CallStatus::Success).unwrap();
    assert!(matches!(values[1], Some(NativeValue::ManagedRef(0x1000))));
    assert!(matches!(values[3], Some(NativeValue::Address(0x1000))));

    let mut failed = ResultArea::new(&target(), &reps).unwrap();
    failed.write(1, NativeValue::ManagedRef(0x2000)).unwrap();
    assert_eq!(
        failed.finish(CallStatus::IntegrityFailure),
        Err(ControlError::CallFailed(CallStatus::IntegrityFailure))
    );
}

#[test]
fn join_contract_rejects_arity_and_representation_drift() {
    let contract = JoinContract::from_signature(&SemanticSignature {
        arguments: vec![RuntimeRep::Void, RuntimeRep::Word(8), RuntimeRep::LiftedRef],
        results: vec![RuntimeRep::Int(64), RuntimeRep::Float(64)],
    });
    contract
        .check_arguments(&[
            RuntimeRep::Void,
            RuntimeRep::Word(8),
            RuntimeRep::UnliftedRef,
        ])
        .unwrap();
    assert!(matches!(
        contract.check_arguments(&[RuntimeRep::Word(8)]),
        Err(ControlError::Arity { .. })
    ));
    assert!(matches!(
        contract.check_results(&[RuntimeRep::Word(64), RuntimeRep::Float(64)]),
        Err(ControlError::Representation { .. })
    ));
}

#[cfg(target_arch = "x86_64")]
#[test]
fn system_v_adapter_calls_tail_entry_with_whole_result_area() {
    let mut module =
        JITModule::new(JITBuilder::new(cranelift_module::default_libcall_names()).unwrap());
    let pointer = module.target_config().pointer_type();
    let mut tail_signature = Signature::new(CallConv::Tail);
    tail_signature.params.push(AbiParam::new(pointer));
    tail_signature.returns.push(AbiParam::new(types::I64));
    let tail_id = module
        .declare_function("prepared_tail", Linkage::Local, &tail_signature)
        .unwrap();

    let mut context = module.make_context();
    context.func.signature = tail_signature.clone();
    let mut builder_context = FunctionBuilderContext::new();
    {
        let mut builder = FunctionBuilder::new(&mut context.func, &mut builder_context);
        let block = builder.create_block();
        builder.append_block_params_for_function_params(block);
        builder.switch_to_block(block);
        builder.seal_block(block);
        let out = builder.block_params(block)[0];
        let raw = builder.ins().iconst(types::I64, -7);
        builder.ins().store(MemFlags::trusted(), raw, out, 0);
        let reference = builder.ins().iconst(types::I64, 0x1000);
        builder.ins().store(MemFlags::trusted(), reference, out, 8);
        let status = builder.ins().iconst(types::I64, CallStatus::Success as i64);
        builder.ins().return_(&[status]);
        builder.finalize();
    }
    module.define_function(tail_id, &mut context).unwrap();
    module.clear_context(&mut context);

    let mut adapter_signature = Signature::new(module.target_config().default_call_conv);
    adapter_signature.params.push(AbiParam::new(pointer));
    adapter_signature.returns.push(AbiParam::new(types::I64));
    let adapter_id = module
        .declare_function("prepared_adapter", Linkage::Export, &adapter_signature)
        .unwrap();
    context.func.signature = adapter_signature;
    {
        let mut builder = FunctionBuilder::new(&mut context.func, &mut builder_context);
        let block = builder.create_block();
        builder.append_block_params_for_function_params(block);
        builder.switch_to_block(block);
        builder.seal_block(block);
        let callee = module.declare_func_in_func(tail_id, builder.func);
        let output = builder.block_params(block)[0];
        let call = builder.ins().call(callee, &[output]);
        let status = builder.inst_results(call)[0];
        builder.ins().return_(&[status]);
        builder.finalize();
    }
    module.define_function(adapter_id, &mut context).unwrap();
    module.finalize_definitions().unwrap();

    let address = module.get_finalized_function(adapter_id);
    let adapter: unsafe extern "C" fn(*mut u64) -> i64 = unsafe { std::mem::transmute(address) };
    let mut area = [0u64; 2];
    let status = unsafe { adapter(area.as_mut_ptr()) };
    assert_eq!(CallStatus::from_raw(status).unwrap(), CallStatus::Success);
    assert_eq!(area, [(-7i64) as u64, 0x1000]);
}
