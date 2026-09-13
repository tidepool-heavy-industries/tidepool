//! Explicit-worklist native emission over the checked flat arena.

use super::{plan::ProgramPlan, CompileError, Unsupported};
use crate::entry_abi::{EntryAbi, EnvironmentMode, NativeAbiProfile};
use crate::pipeline::CodegenPipeline;
use cranelift_codegen::isa::CallConv;
use cranelift_codegen::{
    ir::{self, types, Block, BlockArg, InstBuilder, MemFlags, Value},
    Context,
};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_module::{FuncId, Module};
use std::collections::BTreeMap;
use std::sync::Arc;
use tidepool_heap::execution_descriptor::ObjectDescriptor;
use tidepool_repr::execution_schema::{
    AlternativePattern, Atom, CaseKind, ExprFrame, Group, HeapRhs, JoinId, RuntimeRep, Signature,
    SignatureId, ValueId, ValueRef,
};

type Values = BTreeMap<ValueId, Value>;

#[derive(Clone)]
struct Destination {
    block: Block,
    reps: Vec<RuntimeRep>,
}

#[derive(Clone)]
struct JoinTarget {
    block: Block,
    signature: Signature,
}

enum Work {
    Emit {
        node: usize,
        block: Block,
        block_reps: Vec<RuntimeRep>,
        values: Values,
        joins: BTreeMap<JoinId, JoinTarget>,
        destination: Destination,
    },
    Case {
        node: usize,
        block: Block,
        reps: Vec<RuntimeRep>,
        values: Values,
        joins: BTreeMap<JoinId, JoinTarget>,
        destination: Destination,
    },
}

/// All generated entry points share (vmctx, tagged_environment, physical args).
/// Case/let continuations are native blocks, not recursively emitted Rust calls.
pub(super) fn emit_function(
    plan: &ProgramPlan<'_>,
    id: ValueId,
    functions: &BTreeMap<ValueId, FuncId>,
    dispatchers: &super::apply::Dispatchers,
    prepared_gc: FuncId,
    prepared_poll: FuncId,
    prepared_stack_overflow: FuncId,
    prepared_enter: FuncId,
    case_trap: FuncId,
    pipeline: &mut CodegenPipeline,
) -> Result<(), CompileError> {
    let output = *functions.get(&id).ok_or_else(|| unsupported(id, 0))?;
    emit_function_at(
        plan,
        id,
        output,
        functions,
        dispatchers,
        prepared_gc,
        prepared_poll,
        prepared_stack_overflow,
        prepared_enter,
        case_trap,
        pipeline,
    )
}

pub(super) fn emit_thunk_body(
    plan: &ProgramPlan<'_>,
    id: ValueId,
    output: FuncId,
    functions: &BTreeMap<ValueId, FuncId>,
    dispatchers: &super::apply::Dispatchers,
    prepared_gc: FuncId,
    prepared_poll: FuncId,
    prepared_stack_overflow: FuncId,
    prepared_enter: FuncId,
    case_trap: FuncId,
    pipeline: &mut CodegenPipeline,
) -> Result<(), CompileError> {
    emit_function_at(
        plan,
        id,
        output,
        functions,
        dispatchers,
        prepared_gc,
        prepared_poll,
        prepared_stack_overflow,
        prepared_enter,
        case_trap,
        pipeline,
    )
}

fn emit_function_at(
    plan: &ProgramPlan<'_>,
    id: ValueId,
    output: FuncId,
    functions: &BTreeMap<ValueId, FuncId>,
    dispatchers: &super::apply::Dispatchers,
    prepared_gc: FuncId,
    prepared_poll: FuncId,
    prepared_stack_overflow: FuncId,
    prepared_enter: FuncId,
    case_trap: FuncId,
    pipeline: &mut CodegenPipeline,
) -> Result<(), CompileError> {
    let (signature, body) = match plan.functions.get(&id) {
        Some(function) => (function.signature.clone(), Some(function.body)),
        None if plan.thunks.contains_key(&id) => {
            let thunk = &plan.thunks[&id];
            (thunk.signature.clone(), Some(thunk.body))
        }
        None => {
            let binding = plan
                .top_bindings
                .get(&id)
                .ok_or_else(|| unsupported(id, 0))?;
            (top_signature(plan, binding), None)
        }
    };
    let profile = NativeAbiProfile::new(plan.program.envelope().target.clone(), 0)?;
    let abi = EntryAbi::lower_internal(&profile, &signature, EnvironmentMode::Captured)?;
    let mut context = Context::new();
    context.func.signature = abi.cranelift_signature(&profile, CallConv::Tail)?;
    let mut frontend = FunctionBuilderContext::new();
    let mut builder = FunctionBuilder::new(&mut context.func, &mut frontend);
    let start = builder.create_block();
    builder.append_block_params_for_function_params(start);
    builder.switch_to_block(start);
    builder.seal_block(start);
    let parameters = builder.block_params(start).to_vec();
    let vmctx = parameters[0];
    let tagged_environment = parameters[1];
    let physical_arguments = &parameters[2..];
    // Keep every managed value live across the entry safepoint, including
    // arguments that have not yet been copied into the local-value map.
    builder.declare_value_needs_stack_map(tagged_environment);
    for (&value, rep) in physical_arguments.iter().zip(abi.physical_arguments()) {
        if matches!(rep, RuntimeRep::LiftedRef | RuntimeRep::UnliftedRef) {
            builder.declare_value_needs_stack_map(value);
        }
    }

    // Check native headroom before any generated entry host call. The helper
    // compares the machine SP with the invocation threshold and records a
    // typed StackOverflow through VMContext on failure.
    let preflight_status = emit_preflight(&mut builder, vmctx, prepared_stack_overflow, pipeline);
    emit_status_guard(&mut builder, preflight_status);
    let poll = pipeline
        .module
        .declare_func_in_func(prepared_poll, builder.func);
    let point = builder.ins().iconst(
        types::I32,
        crate::prepared_control::PreparedSafepoint::FunctionEntry as i64,
    );
    let entry_status = builder.ins().call(poll, &[vmctx, point]);
    let entry_status = builder.inst_results(entry_status)[0];
    let entry = emit_status_guard(&mut builder, entry_status);
    let mut values = BTreeMap::new();
    if let Some(function) = plan.functions.get(&id) {
        bind_parameters(
            &mut builder,
            &mut values,
            &function.parameters,
            &function.signature.arguments,
            physical_arguments,
            id,
            function.body,
        )?;
        let environment = builder.ins().band_imm(tagged_environment, !7_i64);
        bind_captures(
            &mut builder,
            &mut values,
            function.captures,
            &function.descriptor,
            environment,
            id,
            function.body,
        )?;
    } else if let Some(thunk) = plan.thunks.get(&id) {
        let environment = builder.ins().band_imm(tagged_environment, !7_i64);
        bind_captures(
            &mut builder,
            &mut values,
            thunk.captures,
            &thunk.descriptor,
            environment,
            id,
            thunk.body,
        )?;
    } else {
        // A top constructor or byte literal is already materialized by the
        // invocation-owned top table; its environment is the returned value.
        return_top(&mut builder, tagged_environment, &signature.results);
        builder.finalize();
        pipeline.define_function(output, &mut context)?;
        return Ok(());
    }
    let root = body.ok_or_else(|| unsupported(id, 0))?;
    let exit = builder.create_block();
    append_params(&mut builder, exit, &signature.results)?;
    let destination = Destination {
        block: exit,
        reps: signature.results.clone(),
    };
    let mut worklist = vec![Work::Emit {
        node: root,
        block: entry,
        block_reps: Vec::new(),
        values,
        joins: BTreeMap::new(),
        destination,
    }];
    while let Some(work) = worklist.pop() {
        match work {
            Work::Emit {
                node,
                block,
                block_reps,
                values,
                joins,
                destination,
            } => {
                if builder.current_block() != Some(block) {
                    builder.switch_to_block(block);
                }
                if block != entry && !block_reps.is_empty() {
                    mark_block_params(&mut builder, block, &block_reps)?;
                }
                let frame = plan
                    .program
                    .expressions()
                    .nodes
                    .get(node)
                    .ok_or_else(|| unsupported(id, node))?;
                match frame {
                    ExprFrame::Case {
                        scrutinee,
                        binder,
                        scrutinee_reps,
                        kind,
                        alternatives,
                    } => {
                        let scrutinee_block = builder.create_block();
                        append_params(&mut builder, scrutinee_block, scrutinee_reps)?;
                        worklist.push(Work::Case {
                            node,
                            block: scrutinee_block,
                            reps: scrutinee_reps.clone(),
                            values: values.clone(),
                            joins: joins.clone(),
                            destination,
                        });
                        worklist.push(Work::Emit {
                            node: *scrutinee,
                            block,
                            block_reps: Vec::new(),
                            values,
                            joins,
                            destination: Destination {
                                block: scrutinee_block,
                                reps: scrutinee_reps.clone(),
                            },
                        });
                        let _ = (binder, kind, alternatives);
                    }
                    ExprFrame::LetJoins { bindings, body } => {
                        let mut joins = joins;
                        let declared: Vec<_> = group_items(bindings)
                            .iter()
                            .map(|binding| {
                                let signature =
                                    plan.program.signatures()[binding.signature.0 as usize].clone();
                                let join_block = builder.create_block();
                                (
                                    binding,
                                    JoinTarget {
                                        block: join_block,
                                        signature,
                                    },
                                )
                            })
                            .collect();
                        for (_, target) in &declared {
                            append_params(&mut builder, target.block, &target.signature.arguments)?;
                        }
                        for (binding, target) in &declared {
                            joins.insert(binding.id, target.clone());
                        }
                        for (binding, target) in declared {
                            let mut body_values = values.clone();
                            bind_block_values(
                                &mut body_values,
                                &builder,
                                target.block,
                                &binding.parameters,
                                &target.signature.arguments,
                            )?;
                            worklist.push(Work::Emit {
                                node: binding.body,
                                block: target.block,
                                block_reps: target.signature.arguments.clone(),
                                values: body_values,
                                joins: joins.clone(),
                                destination: destination.clone(),
                            });
                        }
                        worklist.push(Work::Emit {
                            node: *body,
                            block,
                            block_reps: Vec::new(),
                            values,
                            joins,
                            destination,
                        });
                    }
                    ExprFrame::Jump { join, arguments } => {
                        let target = joins.get(join).ok_or_else(|| unsupported(id, node))?;
                        let point = builder.ins().iconst(
                            types::I32,
                            crate::prepared_control::PreparedSafepoint::Backedge as i64,
                        );
                        let status = builder.ins().call(poll, &[vmctx, point]);
                        let status = builder.inst_results(status)[0];
                        emit_status_guard(&mut builder, status);
                        let arguments = emit_atoms(
                            &mut builder,
                            &values,
                            arguments,
                            &target.signature.arguments,
                            vmctx,
                            plan,
                            id,
                            node,
                        )?;
                        jump_to(&mut builder, &target.block, arguments);
                    }
                    ExprFrame::Let { bindings, body } => {
                        // Validation already enforces nonrecursive visibility;
                        // recursive groups bind every sibling before stores.
                        let values = emit_let_group(
                            &mut builder,
                            vmctx,
                            prepared_gc,
                            pipeline,
                            plan,
                            values,
                            bindings,
                            id,
                            node,
                        )?;
                        let body_block = builder
                            .current_block()
                            .ok_or_else(|| unsupported(id, node))?;
                        worklist.push(Work::Emit {
                            node: *body,
                            block: body_block,
                            block_reps: Vec::new(),
                            values,
                            joins,
                            destination,
                        });
                    }
                    ExprFrame::Operation {
                        operation,
                        arguments,
                    } => {
                        let declaration = plan
                            .program
                            .operations()
                            .get(operation.0 as usize)
                            .ok_or_else(|| unsupported(id, node))?;
                        let signature = plan
                            .program
                            .signatures()
                            .get(declaration.signature.0 as usize)
                            .ok_or_else(|| unsupported(id, node))?;
                        let operation =
                            super::primitives::recognize_operation(declaration, signature)
                                .ok_or_else(|| unsupported(id, node))?;
                        let physical_arguments = emit_atoms(
                            &mut builder,
                            &values,
                            arguments,
                            &signature.arguments,
                            vmctx,
                            plan,
                            id,
                            node,
                        )?;
                        let output = super::primitives::emit_operation(
                            operation,
                            &mut builder,
                            &physical_arguments,
                            vmctx,
                            pipeline,
                        )?;
                        if output.len()
                            != signature
                                .results
                                .iter()
                                .filter(|rep| **rep != RuntimeRep::Void)
                                .count()
                        {
                            return Err(unsupported(id, node));
                        }
                        jump_to(&mut builder, &destination.block, output);
                    }
                    ExprFrame::Return(atoms) => {
                        let output = emit_atoms(
                            &mut builder,
                            &values,
                            atoms,
                            &destination.reps,
                            vmctx,
                            plan,
                            id,
                            node,
                        )?;
                        jump_to(&mut builder, &destination.block, output);
                    }
                    ExprFrame::Call {
                        callee,
                        signature: call_signature,
                        arguments,
                    } => {
                        let output = emit_exact_call(
                            &mut builder,
                            vmctx,
                            pipeline,
                            functions,
                            dispatchers,
                            &values,
                            callee,
                            *call_signature,
                            arguments,
                            plan,
                            id,
                            node,
                        )?;
                        jump_to(&mut builder, &destination.block, output);
                    }
                    ExprFrame::Enter {
                        callee,
                        signature: call_signature,
                    } => {
                        let output = emit_enter(
                            &mut builder,
                            &values,
                            callee,
                            *call_signature,
                            vmctx,
                            prepared_enter,
                            pipeline,
                            plan,
                            id,
                            node,
                        )?;
                        jump_to(&mut builder, &destination.block, output);
                    }
                    ExprFrame::Construct {
                        constructor,
                        fields,
                    } => {
                        let output = emit_construct(
                            &mut builder,
                            vmctx,
                            prepared_gc,
                            pipeline,
                            &values,
                            *constructor,
                            fields,
                            plan,
                            id,
                            node,
                        )?;
                        jump_to(&mut builder, &destination.block, output);
                    }
                }
            }
            Work::Case {
                node,
                block,
                reps,
                values,
                joins,
                destination,
            } => {
                if builder.current_block() != Some(block) {
                    builder.switch_to_block(block);
                }
                mark_block_params(&mut builder, block, &reps)?;
                emit_case_dispatch(
                    &mut builder,
                    id,
                    node,
                    &reps,
                    values,
                    joins,
                    destination,
                    case_trap,
                    pipeline,
                    vmctx,
                    plan,
                    &mut worklist,
                )?;
            }
        }
    }
    builder.seal_all_blocks();
    builder.switch_to_block(exit);
    let result_values = block_values(&builder, exit, &signature.results)?;
    for (&value, rep) in result_values.iter().zip(
        signature
            .results
            .iter()
            .filter(|rep| **rep != RuntimeRep::Void),
    ) {
        if matches!(rep, RuntimeRep::LiftedRef | RuntimeRep::UnliftedRef) {
            builder.declare_value_needs_stack_map(value);
        }
    }
    return_values(&mut builder, result_values, &signature.results);
    builder.finalize();
    pipeline.define_function(output, &mut context)?;
    Ok(())
}

/// Emit a generated native stack preflight. The comparison happens in the
/// generated frame before any potentially collecting or otherwise external
/// host call. A null threshold fails closed as a stack-overflow status;
/// prepared run entries always install a non-null threshold.
pub(super) fn emit_preflight(
    builder: &mut FunctionBuilder<'_>,
    vmctx: Value,
    stack_overflow: FuncId,
    pipeline: &mut CodegenPipeline,
) -> Value {
    let flags = MemFlags::trusted();
    let limit = builder.ins().load(
        types::I64,
        flags,
        vmctx,
        crate::layout::VMCTX_PREPARED_STACK_LIMIT_OFFSET,
    );
    let stack_pointer = builder.ins().get_stack_pointer(types::I64);
    let configured = builder
        .ins()
        .icmp_imm(ir::condcodes::IntCC::NotEqual, limit, 0);
    let enough = builder.ins().icmp(
        ir::condcodes::IntCC::UnsignedGreaterThanOrEqual,
        stack_pointer,
        limit,
    );
    let permitted = builder.ins().band(configured, enough);
    let success = builder.create_block();
    let overflow = builder.create_block();
    let done = builder.create_block();
    builder.append_block_param(done, types::I32);
    builder.ins().brif(permitted, success, &[], overflow, &[]);

    builder.switch_to_block(success);
    builder.seal_block(success);
    let status = builder.ins().iconst(
        types::I32,
        crate::prepared_control::CallStatus::Success as i64,
    );
    builder.ins().jump(done, &[status.into()]);

    builder.switch_to_block(overflow);
    builder.seal_block(overflow);
    let overflow = pipeline
        .module
        .declare_func_in_func(stack_overflow, builder.func);
    let status = builder.ins().call(overflow, &[vmctx]);
    let status = builder.inst_results(status)[0];
    builder.ins().jump(done, &[status.into()]);

    builder.switch_to_block(done);
    builder.seal_block(done);
    builder.block_params(done)[0]
}

/// Branch on a prepared status and terminate the current function on any
/// failure. The success block becomes the caller's current insertion block.
pub(super) fn emit_status_guard(builder: &mut FunctionBuilder<'_>, status: Value) -> Block {
    let success = builder.create_block();
    let failure = builder.create_block();
    let ok = builder.ins().icmp_imm(
        ir::condcodes::IntCC::Equal,
        status,
        crate::prepared_control::CallStatus::Success as i64,
    );
    builder.ins().brif(ok, success, &[], failure, &[]);

    builder.switch_to_block(failure);
    builder.seal_block(failure);
    crate::alloc::emit_prepared_failure_return(builder, status);

    builder.switch_to_block(success);
    builder.seal_block(success);
    success
}

fn unsupported(binding: ValueId, node: usize) -> CompileError {
    CompileError::Unsupported(Unsupported::Expression { binding, node })
}

fn group_items<T>(group: &Group<T>) -> &[T] {
    match group {
        Group::NonRecursive(item) => std::slice::from_ref(item),
        Group::Recursive(items) => items,
    }
}

fn append_params(
    builder: &mut FunctionBuilder<'_>,
    block: Block,
    reps: &[RuntimeRep],
) -> Result<(), CompileError> {
    for rep in reps.iter().copied().filter(|rep| *rep != RuntimeRep::Void) {
        builder.append_block_param(block, physical_type(rep)?);
    }
    Ok(())
}

fn block_values(
    builder: &FunctionBuilder<'_>,
    block: Block,
    reps: &[RuntimeRep],
) -> Result<Vec<Value>, CompileError> {
    let values = builder.block_params(block);
    if values.len() != reps.iter().filter(|rep| **rep != RuntimeRep::Void).count() {
        return Err(unsupported(ValueId(0), 0));
    }
    Ok(values.to_vec())
}

fn mark_block_params(
    builder: &mut FunctionBuilder<'_>,
    block: Block,
    reps: &[RuntimeRep],
) -> Result<(), CompileError> {
    for (&value, rep) in block_values(builder, block, reps)?
        .iter()
        .zip(reps.iter().filter(|rep| **rep != RuntimeRep::Void))
    {
        if matches!(rep, RuntimeRep::LiftedRef | RuntimeRep::UnliftedRef) {
            builder.declare_value_needs_stack_map(value);
        }
    }
    Ok(())
}

fn bind_block_values(
    values: &mut Values,
    builder: &FunctionBuilder<'_>,
    block: Block,
    ids: &[ValueId],
    reps: &[RuntimeRep],
) -> Result<(), CompileError> {
    if ids.len() != reps.len() {
        return Err(unsupported(ValueId(0), 0));
    }
    let mut physical = block_values(builder, block, reps)?.into_iter();
    for (&id, &rep) in ids.iter().zip(reps) {
        if rep != RuntimeRep::Void {
            values.insert(id, physical.next().ok_or_else(|| unsupported(id, 0))?);
        }
    }
    if physical.next().is_some() {
        return Err(unsupported(ValueId(0), 0));
    }
    Ok(())
}

fn jump_to(builder: &mut FunctionBuilder<'_>, block: &Block, values: Vec<Value>) {
    let args: Vec<_> = values.into_iter().map(BlockArg::Value).collect();
    builder.ins().jump(*block, &args);
}

fn emit_let_group(
    builder: &mut FunctionBuilder<'_>,
    vmctx: Value,
    prepared_gc: FuncId,
    pipeline: &mut CodegenPipeline,
    plan: &ProgramPlan<'_>,
    mut values: Values,
    bindings: &Group<tidepool_repr::execution_schema::HeapBinding>,
    owner: ValueId,
    node: usize,
) -> Result<Values, CompileError> {
    let bindings = group_items(bindings);
    if bindings.is_empty() {
        return Ok(values);
    }
    let mut descriptors = Vec::with_capacity(bindings.len());
    let mut total = 0_u64;
    for binding in bindings {
        let descriptor = match &binding.rhs {
            HeapRhs::Constructor { constructor, .. } => {
                plan.constructors.get(constructor.0 as usize)
            }
            HeapRhs::Function { .. } => plan
                .functions
                .get(&binding.id)
                .map(|function| &function.descriptor),
            HeapRhs::Thunk { .. } => plan.thunks.get(&binding.id).map(|thunk| &thunk.descriptor),
            HeapRhs::Bytes(_) => return Err(unsupported(owner, node)),
        }
        .ok_or_else(|| unsupported(owner, node))?;
        total = total
            .checked_add(u64::from(descriptor.allocation_extent()))
            .ok_or_else(|| unsupported(owner, node))?;
        descriptors.push(descriptor);
    }
    if total < 16 || total % 8 != 0 {
        return Err(unsupported(owner, node));
    }
    let gc = pipeline
        .module
        .declare_func_in_func(prepared_gc, builder.func);
    let base = crate::alloc::emit_prepared_reserve_fast_path(builder, vmctx, gc, total);
    let flags = MemFlags::trusted();
    let mut offset = 0_i64;
    let mut objects = Vec::with_capacity(bindings.len());
    for (binding, descriptor) in bindings.iter().zip(&descriptors) {
        let object = if offset == 0 {
            base
        } else {
            builder.ins().iadd_imm(base, offset)
        };
        offset += i64::from(descriptor.allocation_extent());
        let tag = builder.ins().iconst(types::I64, descriptor.tag() as i64);
        let tagged = builder.ins().bor(object, tag);
        builder.declare_value_needs_stack_map(tagged);
        values.insert(binding.id, tagged);
        objects.push(object);
    }
    // Every sibling address is now rooted and derived after the sole possible
    // safepoint. Header and payload initialization intentionally contains no
    // calls, so recursive references cannot observe a partially published group.
    for ((binding, descriptor), object) in bindings.iter().zip(&descriptors).zip(objects) {
        let header = builder
            .ins()
            .iconst(types::I64, descriptor.initial_header_word() as i64);
        builder.ins().store(flags, header, object, 0);
        match &binding.rhs {
            HeapRhs::Constructor {
                constructor,
                fields,
            } => {
                let declaration = &plan.program.constructors()[constructor.0 as usize];
                for (logical, (atom, rep)) in fields.iter().zip(&declaration.field_reps).enumerate()
                {
                    let Some(stored) = descriptor.payload().logical_to_stored()[logical] else {
                        continue;
                    };
                    if *rep == RuntimeRep::Void {
                        continue;
                    }
                    let field = &descriptor.payload().fields()[stored as usize];
                    let value = atom_value(builder, vmctx, &values, plan, atom, *rep, owner, node)?;
                    builder.ins().store(
                        flags,
                        value,
                        object,
                        (descriptor.payload_base() + field.offset()) as i32,
                    );
                }
            }
            HeapRhs::Function { captures, .. } => {
                for (logical, capture) in captures.iter().enumerate() {
                    let Some(stored) = descriptor.payload().logical_to_stored()[logical] else {
                        continue;
                    };
                    let field = &descriptor.payload().fields()[stored as usize];
                    let atom = Atom::Ref(capture.clone());
                    let value = atom_value(
                        builder,
                        vmctx,
                        &values,
                        plan,
                        &atom,
                        field.rep(),
                        owner,
                        node,
                    )?;
                    builder.ins().store(
                        flags,
                        value,
                        object,
                        (descriptor.payload_base() + field.offset()) as i32,
                    );
                }
            }
            HeapRhs::Thunk { captures, .. } => {
                for (logical, capture) in captures.iter().enumerate() {
                    let Some(stored) = descriptor.payload().logical_to_stored()[logical] else {
                        continue;
                    };
                    let field = &descriptor.payload().fields()[stored as usize];
                    let atom = Atom::Ref(capture.clone());
                    let value = atom_value(
                        builder,
                        vmctx,
                        &values,
                        plan,
                        &atom,
                        field.rep(),
                        owner,
                        node,
                    )?;
                    builder.ins().store(
                        flags,
                        value,
                        object,
                        (descriptor.payload_base() + field.offset()) as i32,
                    );
                }
            }
            HeapRhs::Bytes(_) => unreachable!(),
        }
    }
    Ok(values)
}

#[allow(clippy::too_many_arguments)]
fn emit_case_dispatch(
    builder: &mut FunctionBuilder<'_>,
    owner: ValueId,
    node: usize,
    reps: &[RuntimeRep],
    values: Values,
    joins: BTreeMap<JoinId, JoinTarget>,
    destination: Destination,
    case_trap: FuncId,
    pipeline: &mut CodegenPipeline,
    vmctx: Value,
    plan: &ProgramPlan<'_>,
    worklist: &mut Vec<Work>,
) -> Result<(), CompileError> {
    let ExprFrame::Case {
        binder,
        kind,
        alternatives,
        ..
    } = &plan.program.expressions().nodes[node]
    else {
        return Err(unsupported(owner, node));
    };
    let scrutinee = block_values(
        builder,
        builder
            .current_block()
            .ok_or_else(|| unsupported(owner, node))?,
        reps,
    )?;
    let mut alternative_blocks = Vec::with_capacity(alternatives.len());
    for alternative in alternatives {
        let block = builder.create_block();
        let field_reps: Vec<_> = match (&alternative.pattern, kind) {
            (_, CaseKind::MultiValue) => reps.to_vec(),
            (AlternativePattern::Constructor(id), _) => plan.program.constructors()[id.0 as usize]
                .field_reps
                .clone(),
            _ => Vec::new(),
        };
        append_params(builder, block, &field_reps)?;
        let mut body_values = values.clone();
        if !matches!(kind, CaseKind::MultiValue) {
            if let Some(&value) = scrutinee.first() {
                body_values.insert(*binder, value);
            }
        }
        bind_block_values(
            &mut body_values,
            builder,
            block,
            &alternative.binders,
            &field_reps,
        )?;
        alternative_blocks.push((block, field_reps, body_values));
    }
    for (alternative, (block, field_reps, body_values)) in
        alternatives.iter().zip(alternative_blocks.iter())
    {
        worklist.push(Work::Emit {
            node: alternative.body,
            block: *block,
            block_reps: field_reps.clone(),
            values: body_values.clone(),
            joins: joins.clone(),
            destination: destination.clone(),
        });
    }
    let invalid = builder.create_block();
    match kind {
        CaseKind::MultiValue => jump_to(builder, &alternative_blocks[0].0, scrutinee),
        CaseKind::Polymorphic => jump_to(builder, &alternative_blocks[0].0, Vec::new()),
        CaseKind::Primitive(rep) => {
            let mut next = None;
            let mut default = None;
            for (index, alternative) in alternatives.iter().enumerate() {
                if matches!(alternative.pattern, AlternativePattern::Default) {
                    default = Some(alternative_blocks[index].0);
                    continue;
                }
                let here = next.take();
                if let Some(here) = here {
                    builder.switch_to_block(here);
                }
                match &alternative.pattern {
                    AlternativePattern::Default => unreachable!("handled before literal dispatch"),
                    AlternativePattern::Literal(literal) => {
                        let expected = scalar_value(builder, literal, *rep, plan, owner, node)?;
                        let equal =
                            match rep {
                                RuntimeRep::Float(32) | RuntimeRep::Float(64) => builder
                                    .ins()
                                    .fcmp(ir::condcodes::FloatCC::Equal, scrutinee[0], expected),
                                _ => builder.ins().icmp(
                                    ir::condcodes::IntCC::Equal,
                                    scrutinee[0],
                                    expected,
                                ),
                            };
                        let otherwise = builder.create_block();
                        builder
                            .ins()
                            .brif(equal, alternative_blocks[index].0, &[], otherwise, &[]);
                        next = Some(otherwise);
                    }
                    AlternativePattern::Constructor(_) => return Err(unsupported(owner, node)),
                }
            }
            if let Some(next) = next {
                builder.switch_to_block(next);
                builder.ins().jump(default.unwrap_or(invalid), &[]);
            } else {
                builder.ins().jump(default.unwrap_or(invalid), &[]);
            }
        }
        CaseKind::Algebraic(family) => {
            let mut routes = Vec::new();
            let mut alternatives_by_descriptor = Vec::new();
            let mut default = None;
            for (index, alternative) in alternatives.iter().enumerate() {
                match alternative.pattern {
                    AlternativePattern::Constructor(constructor) => {
                        let route = builder.create_block();
                        routes.push((route, constructor, index));
                        alternatives_by_descriptor
                            .push((plan.constructors[constructor.0 as usize].clone(), route));
                    }
                    AlternativePattern::Default => default = Some(alternative_blocks[index].0),
                    AlternativePattern::Literal(_) => return Err(unsupported(owner, node)),
                }
            }
            let descriptors: Vec<_> = plan
                .program
                .constructors()
                .iter()
                .enumerate()
                .filter(|(_, declaration)| declaration.family == *family)
                .map(|(index, _)| plan.constructors[index].clone())
                .collect();
            emit_algebraic_dispatch(
                builder,
                scrutinee[0],
                &descriptors,
                &alternatives_by_descriptor,
                default,
                invalid,
            );
            for (route, constructor, index) in routes {
                builder.switch_to_block(route);
                let object = builder.ins().band_imm(scrutinee[0], !7_i64);
                let descriptor = &plan.constructors[constructor.0 as usize];
                let fields = descriptor
                    .payload()
                    .logical_to_stored()
                    .iter()
                    .enumerate()
                    .filter_map(|(logical, stored)| stored.map(|stored| (logical, stored)))
                    .map(|(_, stored)| {
                        let field = &descriptor.payload().fields()[stored as usize];
                        builder.ins().load(
                            physical_type(field.rep()).expect("validated representation"),
                            MemFlags::trusted(),
                            object,
                            (descriptor.payload_base() + field.offset()) as i32,
                        )
                    })
                    .collect();
                jump_to(builder, &alternative_blocks[index].0, fields);
            }
        }
    }
    builder.switch_to_block(invalid);
    let trap = pipeline
        .module
        .declare_func_in_func(case_trap, builder.func);
    builder.ins().call(trap, &[vmctx]);
    let status = builder.ins().iconst(
        types::I32,
        crate::prepared_control::CallStatus::IntegrityFailure as i64,
    );
    crate::alloc::emit_prepared_failure_return(builder, status);
    Ok(())
}

fn top_signature(
    plan: &ProgramPlan<'_>,
    binding: &tidepool_repr::execution_schema::HeapBinding,
) -> Signature {
    let result = match &binding.rhs {
        HeapRhs::Bytes(_) => RuntimeRep::Address,
        HeapRhs::Constructor { constructor, .. } => {
            plan.program.constructors()[constructor.0 as usize].result_rep
        }
        HeapRhs::Function { signature, .. } | HeapRhs::Thunk { signature, .. } => {
            plan.program.signatures()[signature.0 as usize]
                .results
                .first()
                .copied()
                .unwrap_or(RuntimeRep::Void)
        }
    };
    Signature {
        arguments: Vec::new(),
        results: vec![result],
    }
}

fn bind_parameters(
    builder: &mut FunctionBuilder<'_>,
    values: &mut BTreeMap<ValueId, Value>,
    logical: &[ValueId],
    reps: &[RuntimeRep],
    physical: &[Value],
    binding: ValueId,
    node: usize,
) -> Result<(), CompileError> {
    if logical.len() != reps.len() {
        return Err(unsupported(binding, node));
    }
    let mut next = 0;
    for (&id, &rep) in logical.iter().zip(reps) {
        if rep != RuntimeRep::Void {
            let Some(&value) = physical.get(next) else {
                return Err(unsupported(binding, node));
            };
            if matches!(rep, RuntimeRep::LiftedRef | RuntimeRep::UnliftedRef) {
                builder.declare_value_needs_stack_map(value);
            }
            values.insert(id, value);
            next += 1;
        }
    }
    if next != physical.len() {
        return Err(unsupported(binding, node));
    }
    Ok(())
}

fn bind_captures(
    builder: &mut FunctionBuilder<'_>,
    values: &mut BTreeMap<ValueId, Value>,
    captures: &[ValueRef],
    descriptor: &ObjectDescriptor,
    environment: Value,
    owner: ValueId,
    node: usize,
) -> Result<(), CompileError> {
    for (logical, capture) in captures.iter().enumerate() {
        let ValueRef::Local(id) = capture else {
            return Err(unsupported(owner, node));
        };
        let Some(stored) = descriptor
            .payload()
            .logical_to_stored()
            .get(logical)
            .and_then(|slot| *slot)
        else {
            continue;
        };
        let field = &descriptor.payload().fields()[stored as usize];
        let value = builder.ins().load(
            physical_type(field.rep())?,
            MemFlags::trusted(),
            environment,
            (descriptor.payload_base() + field.offset()) as i32,
        );
        if matches!(field.rep(), RuntimeRep::LiftedRef | RuntimeRep::UnliftedRef) {
            builder.declare_value_needs_stack_map(value);
        }
        values.insert(*id, value);
    }
    Ok(())
}

fn emit_enter(
    builder: &mut FunctionBuilder<'_>,
    values: &BTreeMap<ValueId, Value>,
    callee: &Atom,
    signature_id: SignatureId,
    vmctx: Value,
    prepared_enter: FuncId,
    pipeline: &mut CodegenPipeline,
    plan: &ProgramPlan<'_>,
    owner: ValueId,
    node: usize,
) -> Result<Vec<Value>, CompileError> {
    let signature = plan
        .program
        .signatures()
        .get(signature_id.0 as usize)
        .ok_or_else(|| unsupported(owner, node))?;
    if !signature.arguments.is_empty()
        || signature.results.len() != 1
        || !matches!(
            signature.results[0],
            RuntimeRep::LiftedRef | RuntimeRep::UnliftedRef
        )
    {
        return Err(unsupported(owner, node));
    }
    // The program-level state machine owns evaluatedness, descriptor
    // inspection, blackholes, and update settlement.  In particular, a local
    // thunk must not be returned as though it were already the result.
    let callee = atom_value(
        builder,
        vmctx,
        values,
        plan,
        callee,
        RuntimeRep::LiftedRef,
        owner,
        node,
    )?;
    let complete = builder.create_block();
    builder.append_block_param(complete, physical_type(signature.results[0])?);
    let enter = pipeline
        .module
        .declare_func_in_func(prepared_enter, builder.func);
    let call = builder.ins().call(enter, &[vmctx, callee]);
    let returned = builder.inst_results(call).to_vec();
    let valid = builder.create_block();
    let invalid = builder.create_block();
    let status = returned[0];
    let success = builder.ins().icmp_imm(
        ir::condcodes::IntCC::Equal,
        status,
        crate::prepared_control::CallStatus::Success as i64,
    );
    builder.ins().brif(success, valid, &[], invalid, &[]);

    builder.switch_to_block(valid);
    builder.seal_block(valid);
    builder.ins().jump(complete, &[returned[1].into()]);

    builder.switch_to_block(invalid);
    builder.seal_block(invalid);
    crate::alloc::emit_prepared_failure_return(builder, status);

    builder.switch_to_block(complete);
    builder.seal_block(complete);
    let result = builder.block_params(complete)[0];
    if matches!(
        signature.results[0],
        RuntimeRep::LiftedRef | RuntimeRep::UnliftedRef
    ) {
        builder.declare_value_needs_stack_map(result);
    }
    Ok(vec![result])
}

fn emit_construct(
    builder: &mut FunctionBuilder<'_>,
    vmctx: Value,
    prepared_gc: FuncId,
    pipeline: &mut CodegenPipeline,
    values: &BTreeMap<ValueId, Value>,
    constructor: tidepool_repr::execution_schema::ConstructorId,
    fields: &[Atom],
    plan: &ProgramPlan<'_>,
    owner: ValueId,
    node: usize,
) -> Result<Vec<Value>, CompileError> {
    let declaration = plan
        .program
        .constructors()
        .get(constructor.0 as usize)
        .ok_or_else(|| unsupported(owner, node))?;
    if fields.len() != declaration.field_reps.len() {
        return Err(unsupported(owner, node));
    }
    let descriptor = plan
        .constructors
        .get(constructor.0 as usize)
        .ok_or_else(|| unsupported(owner, node))?;
    let gc = pipeline
        .module
        .declare_func_in_func(prepared_gc, builder.func);
    let object = crate::alloc::emit_prepared_alloc_fast_path(builder, vmctx, descriptor, gc);
    let flags = MemFlags::trusted();
    let header = builder
        .ins()
        .iconst(types::I64, descriptor.initial_header_word() as i64);
    builder.ins().store(flags, header, object, 0);
    for (logical, (atom, rep)) in fields.iter().zip(&declaration.field_reps).enumerate() {
        let Some(stored) = descriptor
            .payload()
            .logical_to_stored()
            .get(logical)
            .and_then(|slot| *slot)
        else {
            continue;
        };
        if *rep == RuntimeRep::Void {
            continue;
        }
        let field = &descriptor.payload().fields()[stored as usize];
        let value = atom_value(builder, vmctx, values, plan, atom, *rep, owner, node)?;
        builder.ins().store(
            flags,
            value,
            object,
            (descriptor.payload_base() + field.offset()) as i32,
        );
    }
    let tag = builder.ins().iconst(types::I64, descriptor.tag() as i64);
    let tagged = builder.ins().bor(object, tag);
    builder.declare_value_needs_stack_map(tagged);
    Ok(vec![tagged])
}

fn emit_exact_call(
    builder: &mut FunctionBuilder<'_>,
    vmctx: Value,
    pipeline: &mut CodegenPipeline,
    functions: &BTreeMap<ValueId, FuncId>,
    dispatchers: &super::apply::Dispatchers,
    values: &BTreeMap<ValueId, Value>,
    callee: &Atom,
    signature: SignatureId,
    arguments: &[Atom],
    plan: &ProgramPlan<'_>,
    owner: ValueId,
    node: usize,
) -> Result<Vec<Value>, CompileError> {
    let ValueRef::Local(_) = atom_ref(callee, owner, node)? else {
        return Err(unsupported(owner, node));
    };
    let environment = atom_value(
        builder,
        vmctx,
        values,
        plan,
        callee,
        RuntimeRep::LiftedRef,
        owner,
        node,
    )?;
    let _ = functions;
    let callee_ref = *dispatchers
        .get(&signature)
        .ok_or_else(|| unsupported(owner, node))?;
    let callee_ref = pipeline
        .module
        .declare_func_in_func(callee_ref, builder.func);
    let signature = plan
        .program
        .signatures()
        .get(signature.0 as usize)
        .ok_or_else(|| unsupported(owner, node))?;
    if arguments.len() != signature.arguments.len() {
        return Err(unsupported(owner, node));
    }
    let mut call_arguments = vec![vmctx, environment];
    call_arguments.extend(emit_atoms(
        builder,
        values,
        arguments,
        &signature.arguments,
        vmctx,
        plan,
        owner,
        node,
    )?);
    Ok(super::emit_direct_call(
        builder,
        callee_ref,
        &call_arguments,
        &signature.results,
    ))
}

fn atom_ref(atom: &Atom, owner: ValueId, node: usize) -> Result<&ValueRef, CompileError> {
    match atom {
        Atom::Ref(reference) => Ok(reference),
        _ => Err(unsupported(owner, node)),
    }
}

fn atom_value(
    builder: &mut FunctionBuilder<'_>,
    vmctx: Value,
    values: &BTreeMap<ValueId, Value>,
    plan: &ProgramPlan<'_>,
    atom: &Atom,
    expected: RuntimeRep,
    owner: ValueId,
    node: usize,
) -> Result<Value, CompileError> {
    match atom {
        Atom::Ref(ValueRef::Local(id)) => {
            if let Some(value) = values.get(id).copied() {
                return Ok(value);
            }
            let Some(slot) = plan.top_slots.get(id).copied() else {
                return Err(unsupported(owner, node));
            };
            let tops = builder.ins().load(
                types::I64,
                MemFlags::trusted(),
                vmctx,
                crate::layout::VMCTX_PREPARED_TOPS_OFFSET,
            );
            let value = builder.ins().load(
                types::I64,
                MemFlags::trusted(),
                tops,
                (slot * std::mem::size_of::<usize>()) as i32,
            );
            if matches!(expected, RuntimeRep::LiftedRef | RuntimeRep::UnliftedRef) {
                builder.declare_value_needs_stack_map(value);
            }
            Ok(value)
        }
        Atom::Ref(ValueRef::Global(_)) => Err(unsupported(owner, node)),
        Atom::Scalar(scalar) => scalar_value(builder, scalar, expected, plan, owner, node),
        Atom::Void | Atom::Rubbish(_) => Err(unsupported(owner, node)),
    }
}

fn emit_atoms(
    builder: &mut FunctionBuilder<'_>,
    values: &BTreeMap<ValueId, Value>,
    atoms: &[Atom],
    reps: &[RuntimeRep],
    vmctx: Value,
    plan: &ProgramPlan<'_>,
    owner: ValueId,
    node: usize,
) -> Result<Vec<Value>, CompileError> {
    if atoms.len() != reps.len() {
        return Err(unsupported(owner, node));
    }
    atoms
        .iter()
        .zip(reps)
        .filter_map(|(atom, rep)| (*rep != RuntimeRep::Void).then_some((atom, *rep)))
        .map(|(atom, rep)| atom_value(builder, vmctx, values, plan, atom, rep, owner, node))
        .collect()
}

fn return_top(builder: &mut FunctionBuilder<'_>, environment: Value, results: &[RuntimeRep]) {
    return_values(builder, vec![environment], results);
}

fn return_values(builder: &mut FunctionBuilder<'_>, values: Vec<Value>, reps: &[RuntimeRep]) {
    let status = builder.ins().iconst(
        types::I32,
        crate::prepared_control::CallStatus::Success as i64,
    );
    let mut returned = vec![status];
    returned.extend(
        values
            .into_iter()
            .zip(reps.iter().filter(|rep| **rep != RuntimeRep::Void))
            .map(|(value, _)| value),
    );
    builder.ins().return_(&returned);
}

fn physical_type(rep: RuntimeRep) -> Result<ir::Type, CompileError> {
    let result = match rep {
        RuntimeRep::Int(8) | RuntimeRep::Word(8) => types::I8,
        RuntimeRep::Int(16) | RuntimeRep::Word(16) => types::I16,
        RuntimeRep::Int(32) | RuntimeRep::Word(32) => types::I32,
        RuntimeRep::Float(32) => types::F32,
        RuntimeRep::Float(64) => types::F64,
        RuntimeRep::LiftedRef
        | RuntimeRep::UnliftedRef
        | RuntimeRep::Address
        | RuntimeRep::Int(64)
        | RuntimeRep::Word(64) => types::I64,
        RuntimeRep::Void => return Err(unsupported(ValueId(0), 0)),
        _ => return Err(unsupported(ValueId(0), 0)),
    };
    Ok(result)
}

fn scalar_value(
    builder: &mut FunctionBuilder<'_>,
    scalar: &tidepool_repr::execution_schema::ScalarLiteral,
    expected: RuntimeRep,
    plan: &ProgramPlan<'_>,
    owner: ValueId,
    node: usize,
) -> Result<Value, CompileError> {
    match scalar {
        tidepool_repr::execution_schema::ScalarLiteral::Int { bytes, .. }
        | tidepool_repr::execution_schema::ScalarLiteral::Word { bytes, .. } => {
            if !matches!(expected, RuntimeRep::Int(_) | RuntimeRep::Word(_)) || bytes.len() > 8 {
                return Err(unsupported(owner, node));
            }
            let mut word = [0_u8; 8];
            let start = word.len().saturating_sub(bytes.len());
            word[start..].copy_from_slice(&bytes[bytes.len().saturating_sub(8)..]);
            Ok(builder
                .ins()
                .iconst(physical_type(expected)?, i64::from_be_bytes(word)))
        }
        tidepool_repr::execution_schema::ScalarLiteral::Float { bits, bytes } => {
            if bytes.len() != usize::from(*bits / 8) {
                return Err(unsupported(owner, node));
            }
            let mut word = [0_u8; 8];
            word[8 - bytes.len()..].copy_from_slice(bytes);
            match expected {
                RuntimeRep::Float(32) => Ok(builder.ins().f32const(f32::from_bits(
                    u32::from_be_bytes(word[4..].try_into().unwrap()),
                ))),
                RuntimeRep::Float(64) => Ok(builder
                    .ins()
                    .f64const(f64::from_bits(u64::from_be_bytes(word)))),
                _ => Err(unsupported(owner, node)),
            }
        }
        tidepool_repr::execution_schema::ScalarLiteral::NullAddress => (expected
            == RuntimeRep::Address)
            .then(|| builder.ins().iconst(types::I64, 0))
            .ok_or_else(|| unsupported(owner, node)),
        tidepool_repr::execution_schema::ScalarLiteral::Bytes(bytes) => {
            if expected != RuntimeRep::Address {
                return Err(unsupported(owner, node));
            }
            let address = plan
                .bytes
                .get(bytes)
                .ok_or_else(|| unsupported(owner, node))?
                .as_ptr() as usize;
            Ok(builder.ins().iconst(types::I64, address as i64))
        }
    }
}

/// Checked algebraic dispatch uses full descriptor identity, not a family-
/// relative low-bit tag. A DEFAULT applies only to a member of this family.
/// `invalid` records typed integrity failure and returns through the entry ABI.
/// Managed pointer provenance is established before this helper is reached.
pub(super) fn emit_algebraic_dispatch(
    builder: &mut FunctionBuilder<'_>,
    scrutinee: Value,
    family: &[Arc<ObjectDescriptor>],
    alternatives: &[(Arc<ObjectDescriptor>, Block)],
    default: Option<Block>,
    invalid: Block,
) {
    let object = builder.ins().band_imm(scrutinee, !7_i64);
    let nonnull = builder.create_block();
    let null = builder
        .ins()
        .icmp_imm(ir::condcodes::IntCC::Equal, object, 0);
    builder.ins().brif(null, invalid, &[], nonnull, &[]);
    builder.switch_to_block(nonnull);
    builder.seal_block(nonnull);
    let header = builder
        .ins()
        .load(types::I64, MemFlags::trusted(), object, 0);
    // Initial header is a live pinned descriptor address. A stateful or foreign
    // family object cannot match any branch, even with convincing tag bits.
    for descriptor in family {
        let next = builder.create_block();
        let matched = builder.create_block();
        let same = builder.ins().icmp_imm(
            ir::condcodes::IntCC::Equal,
            header,
            descriptor.initial_header_word() as i64,
        );
        builder.ins().brif(same, matched, &[], next, &[]);
        builder.switch_to_block(matched);
        builder.seal_block(matched);
        let tag = builder.ins().band_imm(scrutinee, 7);
        let unknown = builder.ins().icmp_imm(ir::condcodes::IntCC::Equal, tag, 0);
        let generic = builder.ins().icmp_imm(ir::condcodes::IntCC::Equal, tag, 7);
        let canonical =
            builder
                .ins()
                .icmp_imm(ir::condcodes::IntCC::Equal, tag, descriptor.tag() as i64);
        let valid = builder.ins().bor(unknown, generic);
        let valid = builder.ins().bor(valid, canonical);
        let destination = alternatives
            .iter()
            .find(|(candidate, _)| Arc::ptr_eq(candidate, descriptor))
            .map(|(_, block)| *block)
            .or(default)
            .unwrap_or(invalid);
        builder.ins().brif(valid, destination, &[], invalid, &[]);
        builder.switch_to_block(next);
        builder.seal_block(next);
    }
    builder.ins().jump(invalid, &[]);
}
