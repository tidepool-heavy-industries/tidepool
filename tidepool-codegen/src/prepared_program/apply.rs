//! Dynamic application keeps logical arity (including Void) separate from
//! payload storage. PAPs contain the original function reference followed by
//! its supplied prefix, never another PAP. Their pinned descriptor determines
//! both the prefix signature and exact tracing layout.
//!
//! Generate one Tail-ABI dispatcher per demanded call signature (and demanded
//! oversaturation suffix). Enter the callee first; match its descriptor against
//! the owner's function/PAP table. Load pending arguments into rooted SSA before
//! any call/allocation. Exact calls use the normal status/rooting ABI; partial
//! calls reserve once and initialize without a host call before publication;
//! excess arguments apply to the successful lifted result. No Rust function
//! pointer may call a Tail entry, and no unrooted host argument buffer is used.

use super::plan::ProgramPlan;
use super::resolve;
use crate::entry_abi::{EntryAbi, EnvironmentMode, NativeAbiProfile};
use crate::pipeline::CodegenPipeline;
use cranelift_codegen::ir::{self, types, InstBuilder, MemFlags};
use cranelift_codegen::isa::CallConv;
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_module::{FuncId, Linkage, Module};
use std::collections::BTreeMap;
use std::sync::Arc;
use tidepool_heap::execution_descriptor::{EntryMetadata, ObjectDescriptor, ObjectKind};
use tidepool_repr::execution_schema::{
    RuntimeRep, Signature, StorageLayout, TargetDescriptor, ValueId,
};

/// Source calls, owner offers and generated suffixes share one signature map.
pub(super) struct Dispatchers {
    // Boxed keys pin the metadata addresses embedded in generated code.
    entries: BTreeMap<Box<Signature>, FuncId>,
}

impl Dispatchers {
    pub(super) fn find(&self, signature: &Signature) -> Option<FuncId> {
        self.entries.get(signature).copied()
    }

    fn iter(&self) -> impl Iterator<Item = (&Signature, FuncId)> {
        self.entries
            .iter()
            .map(|(signature, function)| (signature.as_ref(), *function))
    }

    fn demand_address(&self, signature: &Signature) -> Result<i64, super::CompileError> {
        self.entries
            .get_key_value(signature)
            .map(|(key, _)| key.as_ref() as *const Signature as i64)
            .ok_or_else(|| super::CompileError::MissingDemand(signature.clone()))
    }
}

/// The zero-argument lifted demand `[] -> LiftedRef` applied to a function
/// or PAP with at least one remaining argument returns the entered callee
/// unchanged, which is exactly what `prepared_enter` returns for an evaluated
/// callee. When the two native signatures agree, owners offer that demand
/// through `prepared_enter` instead of generating a dispatcher whose header
/// chain spans every function and PAP layout. A program that itself demands
/// the shape (a zero-argument source call, or an exact offer of a
/// zero-argument function) still generates the dispatcher.
fn zero_argument_lift() -> Signature {
    Signature {
        arguments: Vec::new(),
        results: super::ResultContract::Returns(vec![RuntimeRep::LiftedRef]),
    }
}

fn enter_serves_zero_argument_lift(
    profile: &NativeAbiProfile,
) -> Result<bool, super::CompileError> {
    let abi = EntryAbi::lower_internal(profile, &zero_argument_lift(), EnvironmentMode::Captured)?;
    Ok(abi.cranelift_signature(profile, CallConv::Tail)? == super::entry::signature())
}

/// A flattened partial application of an original function. `pending` counts
/// logical arguments, so a Void argument advances arity without occupying a slot.
pub(super) struct PapLayout {
    pub function: ValueId,
    pub descriptor: Arc<ObjectDescriptor>,
}

/// The only arity split used by dispatch admission and code emission. A failed
/// match is not a fallback cast: that descriptor cannot satisfy the demanded ABI.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum Application {
    Partial {
        total_pending: usize,
    },
    Exact,
    /// Saturate the underlying nonreturning entry; never apply excess args.
    NoSuccess {
        consumed: usize,
    },
    Excess {
        consumed: usize,
        remainder: Signature,
    },
}

enum ResolvedContinuation {
    Return,
    Terminal,
    Apply {
        consumed: usize,
        remainder: Signature,
    },
}

pub(super) fn classify(
    entry: &Signature,
    pending: usize,
    demand: &Signature,
) -> Option<Application> {
    let remaining = entry.arguments.get(pending..)?;
    let consumed = remaining.len().min(demand.arguments.len());
    if remaining[..consumed] != demand.arguments[..consumed] {
        return None;
    }
    match demand.arguments.len().cmp(&remaining.len()) {
        std::cmp::Ordering::Less
            if demand.results
                == tidepool_repr::execution_schema::ResultContract::Returns(vec![
                    RuntimeRep::LiftedRef,
                ]) =>
        {
            Some(Application::Partial {
                total_pending: pending + demand.arguments.len(),
            })
        }
        std::cmp::Ordering::Equal | std::cmp::Ordering::Greater
            if entry.results == tidepool_repr::execution_schema::ResultContract::NoSuccess =>
        {
            Some(Application::NoSuccess { consumed })
        }
        std::cmp::Ordering::Equal
            if demand.results == entry.results
                || (entry.results.is_caller_result()
                    && matches!(demand.results, super::ResultContract::Returns(_))) =>
        {
            Some(Application::Exact)
        }
        std::cmp::Ordering::Greater
            if demand.results != tidepool_repr::execution_schema::ResultContract::NoSuccess
                && entry.results
                    == tidepool_repr::execution_schema::ResultContract::Returns(vec![
                        RuntimeRep::LiftedRef,
                    ]) =>
        {
            Some(Application::Excess {
                consumed,
                remainder: Signature {
                    arguments: demand.arguments[consumed..].to_vec(),
                    results: demand.results.clone(),
                },
            })
        }
        _ => None,
    }
}

pub(super) fn layouts<'a>(
    target: &TargetDescriptor,
    functions: impl IntoIterator<Item = (ValueId, &'a Signature)>,
) -> Result<BTreeMap<(ValueId, usize), PapLayout>, super::CompileError> {
    let mut layouts = BTreeMap::new();
    for (function, signature) in functions {
        for pending in 1..signature.arguments.len() {
            let mut reps = vec![RuntimeRep::LiftedRef];
            reps.extend_from_slice(&signature.arguments[..pending]);
            let descriptor = Arc::new(ObjectDescriptor::new(
                ObjectKind::Pap,
                StorageLayout::for_reps(target, &reps)?,
                Some(EntryMetadata::new(
                    Signature {
                        arguments: signature.arguments[pending..].to_vec(),
                        results: signature.results.clone(),
                    },
                    u64::from(function.0),
                )),
            )?);
            layouts.insert(
                (function, pending),
                PapLayout {
                    function,
                    descriptor,
                },
            );
        }
    }
    Ok(layouts)
}

/// Declare one native Tail-ABI dispatcher for each demanded call signature.
/// The callee occupies the same ABI position as a captured environment; this
/// keeps dynamic calls compatible with ordinary generated entries without a
/// Rust-side function-pointer cast.
pub(super) fn declare_dispatchers(
    plan: &ProgramPlan<'_>,
    profile: &NativeAbiProfile,
    pipeline: &mut CodegenPipeline,
) -> Result<Dispatchers, super::CompileError> {
    let result_instances = super::plan::result_instances(plan.program);
    let enter_lifts = enter_serves_zero_argument_lift(profile)?;
    let mut demanded = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for declaration in plan.program.operations() {
        let signature = &plan.program.signatures()[declaration.signature.0 as usize];
        if let Some(callback) = super::lifetime::callback_signature(declaration, signature) {
            if seen.insert(callback.clone()) {
                demanded.push(callback);
            }
        }
    }
    for frame in &plan.program.expressions().nodes {
        let tidepool_repr::execution_schema::ExprFrame::Call { signature, .. } = frame else {
            continue;
        };
        let semantic = plan
            .program
            .signatures()
            .get(signature.0 as usize)
            .ok_or(super::CompileError::MissingRepresentation(ValueId(
                signature.0,
            )))?
            .clone();
        let results = if semantic.results.is_caller_result() {
            result_instances.iter().cloned().collect::<Vec<_>>()
        } else {
            vec![semantic.results.clone()]
        };
        for results in results {
            let concrete = Signature {
                arguments: semantic.arguments.clone(),
                results,
            };
            if seen.insert(concrete.clone()) {
                demanded.push(concrete);
            }
        }
    }
    // Every owner serves exact PAP suffixes and all proper partial prefixes,
    // even when no call in its own source demands that shape.
    for function in plan.functions.values() {
        let entry = function.signature;
        for pending in 0..entry.arguments.len().max(1) {
            let remaining = &entry.arguments[pending..];
            let results = if entry.results.is_caller_result() {
                result_instances.iter().cloned().collect::<Vec<_>>()
            } else {
                vec![entry.results.clone()]
            };
            for results in results {
                let exact = Signature {
                    arguments: remaining.to_vec(),
                    results,
                };
                if seen.insert(exact.clone()) {
                    demanded.push(exact);
                }
            }
            // `supplied == 0` is the zero-argument lift `prepared_enter`
            // serves (see `zero_argument_lift`).
            let first = usize::from(enter_lifts);
            for supplied in first..remaining.len() {
                let partial = Signature {
                    arguments: remaining[..supplied].to_vec(),
                    results: super::ResultContract::Returns(vec![RuntimeRep::LiftedRef]),
                };
                if seen.insert(partial.clone()) {
                    demanded.push(partial);
                }
            }
        }
    }
    // Close over foreign excess suffixes and the statically typed prefix
    // probes. Every new signature is a slice of an existing argument vector.
    let mut cursor = 0;
    while let Some(demand) = demanded.get(cursor).cloned() {
        cursor += 1;
        for consumed in 0..=demand.arguments.len() {
            let terminal = Signature {
                arguments: demand.arguments[..consumed].to_vec(),
                results: super::ResultContract::NoSuccess,
            };
            if seen.insert(terminal.clone()) {
                demanded.push(terminal);
            }
            if consumed > 0 && consumed < demand.arguments.len() {
                let prefix = Signature {
                    arguments: demand.arguments[..consumed].to_vec(),
                    results: super::ResultContract::Returns(vec![RuntimeRep::LiftedRef]),
                };
                let suffix = Signature {
                    arguments: demand.arguments[consumed..].to_vec(),
                    results: demand.results.clone(),
                };
                for shape in [prefix, suffix] {
                    if seen.insert(shape.clone()) {
                        demanded.push(shape);
                    }
                }
            }
        }
    }
    let mut entries = BTreeMap::new();
    for (index, semantic) in demanded.into_iter().enumerate() {
        let abi = EntryAbi::lower_internal(profile, &semantic, EnvironmentMode::Captured)?;
        let native = abi.cranelift_signature(profile, CallConv::Tail)?;
        let function = pipeline.declare_function_with_signature(
            &format!("prepared_apply_{index}"),
            Linkage::Local,
            &native,
        )?;
        entries.insert(Box::new(semantic), function);
    }
    Ok(Dispatchers { entries })
}

/// Emit the exact-application portion of a dispatcher. PAP allocation and
/// excess application are deliberately separate paths; an exact mismatch
/// returns the prepared integrity status instead of falling through to a cast.
#[expect(
    clippy::too_many_arguments,
    reason = "dispatcher emission carries independent borrowed ABI, runtime-entry, root, and pipeline state"
)]
pub(super) fn emit_dispatchers(
    plan: &ProgramPlan<'_>,
    dispatchers: &Dispatchers,
    functions: &BTreeMap<ValueId, BTreeMap<super::ResultContract, FuncId>>,
    profile: &NativeAbiProfile,
    prepared_gc: FuncId,
    prepared_poll: FuncId,
    prepared_stack_overflow: FuncId,
    prepared_enter: FuncId,
    prepared_bad_state: FuncId,
    prepared_resolve_call: FuncId,
    prepared_unresolved_call: FuncId,
    pipeline: &mut CodegenPipeline,
) -> Result<Vec<resolve::CallableExport>, super::CompileError> {
    let started = std::time::Instant::now();
    let mut code_bytes = 0usize;
    // Every offer is recorded where its header's chain entry is emitted, so
    // an owner never offers a shape its dispatcher cannot serve.
    let mut exports = Vec::new();
    for (signature, output) in dispatchers.iter() {
        let abi = EntryAbi::lower_internal(profile, signature, EnvironmentMode::Captured)?;
        let mut context = cranelift_codegen::Context::new();
        context.func.signature = abi.cranelift_signature(profile, CallConv::Tail)?;
        let mut frontend = FunctionBuilderContext::new();
        let mut builder = FunctionBuilder::new(&mut context.func, &mut frontend);
        // Everything below sees only the ENTERED callee: the dispatcher's
        // raw parameters, including the original (possibly unevaluated)
        // callee, stay private to `emit_dispatch_entry`.
        let DispatchInput {
            vmctx,
            entered,
            callee,
            arguments: physical_arguments,
        } = emit_dispatch_entry(
            &mut builder,
            pipeline,
            &abi,
            signature,
            prepared_poll,
            prepared_stack_overflow,
            prepared_enter,
        );
        let mut next = entered;
        for (&id, function) in &plan.functions {
            let Some(application) = classify(function.signature, 0, signature) else {
                continue;
            };
            let Some(callee_function) = callee_instance(functions, id, function, signature) else {
                continue;
            };
            exports.push(resolve::CallableExport {
                header: function.descriptor.initial_header_word(),
                function: output,
                signature: signature.clone(),
            });
            let hit = builder.create_block();
            let following = builder.create_block();
            builder.switch_to_block(next);
            if next != entered {
                builder.seal_block(next);
            }
            let object = builder.ins().band_imm(callee, !7_i64);
            let header = builder
                .ins()
                .load(types::I64, MemFlags::trusted(), object, 0);
            let descriptor = builder
                .ins()
                .iconst(types::I64, function.descriptor.initial_header_word() as i64);
            let matches = builder
                .ins()
                .icmp(ir::condcodes::IntCC::Equal, header, descriptor);
            builder.ins().brif(matches, hit, &[], following, &[]);
            builder.switch_to_block(hit);
            builder.seal_block(hit);
            match application {
                Application::Exact => {
                    let target = pipeline
                        .module
                        .declare_func_in_func(callee_function, builder.func);
                    let call_arguments = call_arguments(vmctx, callee, &physical_arguments);
                    let call = builder.ins().call(target, &call_arguments);
                    let returned = builder.inst_results(call).to_vec();
                    builder.ins().return_(&returned);
                }
                Application::NoSuccess { consumed } => {
                    let target = pipeline
                        .module
                        .declare_func_in_func(callee_function, builder.func);
                    let call_arguments =
                        call_arguments(vmctx, callee, physical_arguments.iter().take(consumed));
                    let _ = super::emit_direct_call(
                        &mut builder,
                        pipeline,
                        vmctx,
                        target,
                        &call_arguments,
                        &tidepool_repr::execution_schema::ResultContract::NoSuccess,
                    )?;
                }
                Application::Partial { total_pending } => {
                    if total_pending == 0 {
                        let status = builder.ins().iconst(
                            types::I32,
                            crate::prepared_control::CallStatus::Success as i64,
                        );
                        builder.ins().return_(&[status, callee]);
                        next = following;
                        continue;
                    }
                    let Some(layout) = plan.pap_layouts.get(&(id, total_pending)) else {
                        return Err(super::CompileError::MissingRepresentation(id));
                    };
                    emit_partial(
                        &mut builder,
                        vmctx,
                        callee,
                        &physical_arguments,
                        layout,
                        prepared_gc,
                        pipeline,
                    )?;
                }
                Application::Excess {
                    consumed,
                    remainder,
                } => emit_excess(
                    &mut builder,
                    vmctx,
                    callee,
                    &physical_arguments,
                    0,
                    consumed,
                    &remainder,
                    callee_function,
                    dispatchers,
                    pipeline,
                    prepared_bad_state,
                )?,
            }
            next = following;
        }
        for (&(id, pending), layout) in &plan.pap_layouts {
            let Some(function) = plan.functions.get(&id) else {
                continue;
            };
            let Some(application) = classify(function.signature, pending, signature) else {
                continue;
            };
            let Some(callee_function) = callee_instance(functions, id, function, signature) else {
                continue;
            };
            exports.push(resolve::CallableExport {
                header: layout.descriptor.initial_header_word(),
                function: output,
                signature: signature.clone(),
            });
            let hit = builder.create_block();
            let following = builder.create_block();
            builder.switch_to_block(next);
            if next != entered {
                builder.seal_block(next);
            }
            let object = builder.ins().band_imm(callee, !7_i64);
            let header = builder
                .ins()
                .load(types::I64, MemFlags::trusted(), object, 0);
            let descriptor = builder
                .ins()
                .iconst(types::I64, layout.descriptor.initial_header_word() as i64);
            let matches = builder
                .ins()
                .icmp(ir::condcodes::IntCC::Equal, header, descriptor);
            builder.ins().brif(matches, hit, &[], following, &[]);
            builder.switch_to_block(hit);
            builder.seal_block(hit);
            // A PAP applies its underlying function, stored in field 0.
            let pap_function = load_pap_field(&mut builder, object, layout, 0)?;
            let mut flattened = Vec::with_capacity(pending + physical_arguments.len());
            for logical in 0..pending {
                if layout.descriptor.payload().logical_to_stored()[logical + 1].is_none() {
                    flattened.push(None);
                    continue;
                }
                flattened.push(Some(load_pap_field(
                    &mut builder,
                    object,
                    layout,
                    logical + 1,
                )?));
            }
            flattened.extend(physical_arguments.iter().copied());
            match application {
                Application::Exact => {
                    let call_arguments = call_arguments(vmctx, pap_function, &flattened);
                    let target = pipeline
                        .module
                        .declare_func_in_func(callee_function, builder.func);
                    let call = builder.ins().call(target, &call_arguments);
                    let returned = builder.inst_results(call).to_vec();
                    builder.ins().return_(&returned);
                }
                Application::NoSuccess { consumed } => {
                    let target = pipeline
                        .module
                        .declare_func_in_func(callee_function, builder.func);
                    let call_arguments = call_arguments(
                        vmctx,
                        pap_function,
                        flattened.iter().take(pending + consumed),
                    );
                    let _ = super::emit_direct_call(
                        &mut builder,
                        pipeline,
                        vmctx,
                        target,
                        &call_arguments,
                        &tidepool_repr::execution_schema::ResultContract::NoSuccess,
                    )?;
                }
                Application::Partial { total_pending } => {
                    let layout = plan
                        .pap_layouts
                        .get(&(id, total_pending))
                        .ok_or(super::CompileError::MissingRepresentation(id))?;
                    emit_partial(
                        &mut builder,
                        vmctx,
                        pap_function,
                        &flattened,
                        layout,
                        prepared_gc,
                        pipeline,
                    )?;
                }
                Application::Excess {
                    consumed,
                    remainder,
                } => emit_excess(
                    &mut builder,
                    vmctx,
                    pap_function,
                    &flattened,
                    pending,
                    consumed,
                    &remainder,
                    callee_function,
                    dispatchers,
                    pipeline,
                    prepared_bad_state,
                )?,
            }
            next = following;
        }
        builder.switch_to_block(next);
        // A dispatcher with NO local candidate at all (every callee of this
        // shape is foreign; legal since G0 admits dynamic and import callees
        // without a locally shaped function) never left `entered`, which is
        // already sealed.
        if next != entered {
            builder.seal_block(next);
        }
        // Full-demand lookup may find an owner's exact or partial adapter.
        // Prefix probes are non-mutating; only exhaustion records failure.
        let object = builder.ins().band_imm(callee, !7_i64);
        let header = builder
            .ins()
            .load(types::I64, MemFlags::trusted(), object, 0);
        let resolve_ref = pipeline
            .module
            .declare_func_in_func(prepared_resolve_call, builder.func);
        let mut probes = vec![(signature.clone(), ResolvedContinuation::Return)];
        for consumed in (0..=signature.arguments.len()).rev() {
            let terminal = Signature {
                arguments: signature.arguments[..consumed].to_vec(),
                results: super::ResultContract::NoSuccess,
            };
            if terminal != *signature {
                probes.push((terminal, ResolvedContinuation::Terminal));
            }
        }
        // A NoSuccess demand cannot acquire evidence of terminal behavior by
        // first running a merely lifted-returning function (classify agrees).
        if signature.results != super::ResultContract::NoSuccess {
            for consumed in (1..signature.arguments.len()).rev() {
                probes.push((
                    Signature {
                        arguments: signature.arguments[..consumed].to_vec(),
                        results: super::ResultContract::Returns(vec![RuntimeRep::LiftedRef]),
                    },
                    ResolvedContinuation::Apply {
                        consumed,
                        remainder: Signature {
                            arguments: signature.arguments[consumed..].to_vec(),
                            results: signature.results.clone(),
                        },
                    },
                ));
            }
        }
        for (demand, application) in probes {
            let metadata = builder
                .ins()
                .iconst(types::I64, dispatchers.demand_address(&demand)?);
            let lookup = builder.ins().call(resolve_ref, &[vmctx, header, metadata]);
            let code = builder.inst_results(lookup)[0];
            let found = builder
                .ins()
                .icmp_imm(ir::condcodes::IntCC::NotEqual, code, 0);
            let hit = builder.create_block();
            let miss = builder.create_block();
            builder.ins().brif(found, hit, &[], miss, &[]);
            builder.switch_to_block(hit);
            builder.seal_block(hit);
            let native = EntryAbi::lower_internal(profile, &demand, EnvironmentMode::Captured)?
                .cranelift_signature(profile, CallConv::Tail)?;
            let sig_ref = builder.import_signature(native);
            let args = call_arguments(
                vmctx,
                callee,
                physical_arguments.iter().take(demand.arguments.len()),
            );
            let call = builder.ins().call_indirect(sig_ref, code, &args);
            let returned = builder.inst_results(call).to_vec();
            match application {
                ResolvedContinuation::Return => {
                    builder.ins().return_(&returned);
                }
                ResolvedContinuation::Terminal => {
                    super::emit_call_results(
                        &mut builder,
                        pipeline,
                        vmctx,
                        &returned,
                        &demand.results,
                    )?;
                }
                ResolvedContinuation::Apply {
                    consumed,
                    remainder,
                } => {
                    if let Some(payload) = super::emit_call_results(
                        &mut builder,
                        pipeline,
                        vmctx,
                        &returned,
                        &demand.results,
                    )? {
                        let suffix = dispatchers
                            .find(&remainder)
                            .ok_or_else(|| super::CompileError::MissingDemand(remainder.clone()))?;
                        let suffix_ref = pipeline.module.declare_func_in_func(suffix, builder.func);
                        let args =
                            call_arguments(vmctx, payload[0], &physical_arguments[consumed..]);
                        let call = builder.ins().call(suffix_ref, &args);
                        let result = builder.inst_results(call).to_vec();
                        builder.ins().return_(&result);
                    }
                }
            }
            builder.switch_to_block(miss);
            builder.seal_block(miss);
        }
        let recorded_ref = pipeline
            .module
            .declare_func_in_func(prepared_unresolved_call, builder.func);
        let recorded = builder.ins().call(recorded_ref, &[vmctx, object]);
        let status = builder.inst_results(recorded)[0];
        crate::alloc::emit_prepared_failure_return(&mut builder, status);
        builder.seal_all_blocks();
        builder.finalize();
        pipeline.define_function(output, &mut context)?;
        if let Some(compiled) = context.compiled_code() {
            code_bytes += compiled.code_buffer().len();
        }
    }
    let lift = zero_argument_lift();
    if dispatchers.find(&lift).is_none() && enter_serves_zero_argument_lift(profile)? {
        let headers = plan
            .functions
            .values()
            .filter(|function| !function.signature.arguments.is_empty())
            .map(|function| function.descriptor.initial_header_word())
            .chain(
                plan.pap_layouts
                    .values()
                    .map(|layout| layout.descriptor.initial_header_word()),
            );
        for header in headers {
            exports.push(resolve::CallableExport {
                header,
                function: prepared_enter,
                signature: lift.clone(),
            });
        }
    }
    tracing::debug!(target: "tidepool::prepared_apply",
        dispatchers = dispatchers.entries.len(), code_bytes,
        offers = exports.len(),
        elapsed_ms = started.elapsed().as_millis() as u64,
        "compiled application dispatchers");
    Ok(exports)
}

/// The compiled instance of `id` a demand applies: its own result contract,
/// or the demand's for a caller-result function.
fn callee_instance(
    functions: &BTreeMap<ValueId, BTreeMap<super::ResultContract, FuncId>>,
    id: ValueId,
    function: &super::plan::FunctionPlan<'_>,
    demand: &Signature,
) -> Option<FuncId> {
    let results = if function.signature.results.is_caller_result() {
        &demand.results
    } else {
        &function.signature.results
    };
    functions.get(&id)?.get(results).copied()
}

/// What a dispatcher body may use after its entry sequence: the VM context,
/// the block reached once the callee is entered, the ENTERED callee, and the
/// demanded arguments in logical order (`None` for a `Void` position).
/// Deliberately absent: the dispatcher's raw parameters, and with them the
/// original callee, which may be a thunk or an updated indirection whose
/// payload is not the applied function's environment.
struct DispatchInput {
    vmctx: ir::Value,
    entered: ir::Block,
    callee: ir::Value,
    arguments: Vec<Option<ir::Value>>,
}

/// Emit a dispatcher's entry: parameters, their stack-map declarations,
/// preflight and function-entry poll, then `prepared_enter` on the callee
/// (returning the failure status if entering fails). The builder is left
/// in the sealed `entered` block.
fn emit_dispatch_entry(
    builder: &mut FunctionBuilder<'_>,
    pipeline: &mut CodegenPipeline,
    abi: &EntryAbi,
    signature: &Signature,
    prepared_poll: FuncId,
    prepared_stack_overflow: FuncId,
    prepared_enter: FuncId,
) -> DispatchInput {
    let start = builder.create_block();
    builder.append_block_params_for_function_params(start);
    builder.switch_to_block(start);
    builder.seal_block(start);
    let params = builder.block_params(start).to_vec();
    let vmctx = params[0];
    let original_callee = params[1];
    builder.declare_value_needs_stack_map(original_callee);
    for (&value, rep) in params[2..].iter().zip(abi.physical_arguments()) {
        if matches!(rep, RuntimeRep::LiftedRef | RuntimeRep::UnliftedRef) {
            builder.declare_value_needs_stack_map(value);
        }
    }
    let preflight = super::emit::emit_preflight(builder, vmctx, prepared_stack_overflow, pipeline);
    super::emit::emit_status_guard(builder, preflight);
    let poll = pipeline
        .module
        .declare_func_in_func(prepared_poll, builder.func);
    let point = builder.ins().iconst(
        types::I32,
        crate::prepared_control::PreparedSafepoint::FunctionEntry as i64,
    );
    let poll = builder.ins().call(poll, &[vmctx, point]);
    let poll_status = builder.inst_results(poll)[0];
    super::emit::emit_status_guard(builder, poll_status);
    let entered = builder.create_block();
    let entered_failure = builder.create_block();
    let enter = pipeline
        .module
        .declare_func_in_func(prepared_enter, builder.func);
    let forced = builder.ins().call(enter, &[vmctx, original_callee]);
    let forced_values = builder.inst_results(forced).to_vec();
    let forced_ok = builder.ins().icmp_imm(
        ir::condcodes::IntCC::Equal,
        forced_values[0],
        crate::prepared_control::CallStatus::Success as i64,
    );
    builder
        .ins()
        .brif(forced_ok, entered, &[], entered_failure, &[]);
    builder.switch_to_block(entered_failure);
    builder.seal_block(entered_failure);
    crate::alloc::emit_prepared_failure_return(builder, forced_values[0]);
    builder.switch_to_block(entered);
    builder.seal_block(entered);
    let callee = forced_values[1];
    builder.declare_value_needs_stack_map(callee);
    DispatchInput {
        vmctx,
        entered,
        callee,
        arguments: logical_arguments(signature, &params[2..]),
    }
}

/// The one way an application's native argument list is built: VM context,
/// the environment (the applied function or closure), then the physical
/// arguments (logical `Void` positions carry no value and are skipped).
fn call_arguments<'a>(
    vmctx: ir::Value,
    environment: ir::Value,
    arguments: impl IntoIterator<Item = &'a Option<ir::Value>>,
) -> Vec<ir::Value> {
    let mut native = vec![vmctx, environment];
    native.extend(arguments.into_iter().flatten().copied());
    native
}

fn logical_arguments(signature: &Signature, physical: &[ir::Value]) -> Vec<Option<ir::Value>> {
    let mut next = 0;
    signature
        .arguments
        .iter()
        .map(|rep| {
            if *rep == RuntimeRep::Void {
                None
            } else {
                let value = physical.get(next).copied();
                next += 1;
                value
            }
        })
        .collect()
}

fn emit_partial(
    builder: &mut FunctionBuilder<'_>,
    vmctx: ir::Value,
    callee: ir::Value,
    arguments: &[Option<ir::Value>],
    layout: &PapLayout,
    prepared_gc: FuncId,
    pipeline: &mut CodegenPipeline,
) -> Result<(), super::CompileError> {
    let gc = pipeline
        .module
        .declare_func_in_func(prepared_gc, builder.func);
    let object =
        crate::alloc::emit_prepared_alloc_fast_path(builder, vmctx, &layout.descriptor, gc);
    let flags = MemFlags::trusted();
    let header = builder
        .ins()
        .iconst(types::I64, layout.descriptor.initial_header_word() as i64);
    builder.ins().store(flags, header, object, 0);
    store_pap_field(builder, object, &layout.descriptor, 0, callee)?;
    for (logical, value) in arguments.iter().enumerate() {
        let Some(value) = value else { continue };
        store_pap_field(builder, object, &layout.descriptor, logical + 1, *value)?;
    }
    let tag = builder
        .ins()
        .iconst(types::I64, layout.descriptor.tag() as i64);
    let tagged = builder.ins().bor(object, tag);
    builder.declare_value_needs_stack_map(tagged);
    let status = builder.ins().iconst(
        types::I32,
        crate::prepared_control::CallStatus::Success as i64,
    );
    builder.ins().return_(&[status, tagged]);
    Ok(())
}

#[expect(
    clippy::too_many_arguments,
    reason = "excess-call emission carries independent borrowed ABI, dispatcher, callee, and pipeline state"
)]
fn emit_excess(
    builder: &mut FunctionBuilder<'_>,
    vmctx: ir::Value,
    function: ir::Value,
    arguments: &[Option<ir::Value>],
    pending: usize,
    consumed: usize,
    remainder: &Signature,
    target_id: FuncId,
    dispatchers: &Dispatchers,
    pipeline: &mut CodegenPipeline,
    prepared_bad_state: FuncId,
) -> Result<(), super::CompileError> {
    // The first call is saturated against the original function. Its lifted
    // result is then treated as a fresh callee for the suffix dispatcher.
    let first_args = call_arguments(vmctx, function, arguments.iter().take(pending + consumed));
    let target = pipeline
        .module
        .declare_func_in_func(target_id, builder.func);
    let call = builder.ins().call(target, &first_args);
    let returned = builder.inst_results(call).to_vec();
    let success = builder.create_block();
    let failure = builder.create_block();
    let ok = builder.ins().icmp_imm(
        ir::condcodes::IntCC::Equal,
        returned[0],
        crate::prepared_control::CallStatus::Success as i64,
    );
    builder.ins().brif(ok, success, &[], failure, &[]);
    builder.switch_to_block(failure);
    builder.seal_block(failure);
    crate::alloc::emit_prepared_failure_return(builder, returned[0]);
    builder.switch_to_block(success);
    builder.seal_block(success);
    let result = returned[1];
    builder.declare_value_needs_stack_map(result);
    let suffix = &arguments[pending + consumed..];
    if let Some(suffix_dispatcher) = dispatchers.find(remainder) {
        let target = pipeline
            .module
            .declare_func_in_func(suffix_dispatcher, builder.func);
        let call_args = call_arguments(vmctx, result, suffix);
        let call = builder.ins().call(target, &call_args);
        let returned = builder.inst_results(call).to_vec();
        builder.ins().return_(&returned);
        return Ok(());
    }
    // The declaration pass closes every excess suffix, including signatures
    // absent from the source arena. Reaching this branch therefore indicates
    // a compiler-integrity breach rather than a second, ad-hoc dispatcher.
    emit_bad_state(builder, vmctx, prepared_bad_state, pipeline);
    Ok(())
}

fn emit_bad_state(
    builder: &mut FunctionBuilder<'_>,
    vmctx: ir::Value,
    bad_state: FuncId,
    pipeline: &mut CodegenPipeline,
) {
    let bad_state = pipeline
        .module
        .declare_func_in_func(bad_state, builder.func);
    let status = builder.ins().call(bad_state, &[vmctx]);
    let status = builder.inst_results(status)[0];
    crate::alloc::emit_prepared_failure_return(builder, status);
}

fn load_pap_field(
    builder: &mut FunctionBuilder<'_>,
    object: ir::Value,
    layout: &PapLayout,
    logical: usize,
) -> Result<ir::Value, super::CompileError> {
    let Some(stored) = layout.descriptor.payload().logical_to_stored()[logical] else {
        return Err(super::CompileError::MissingRepresentation(layout.function));
    };
    let field = &layout.descriptor.payload().fields()[stored as usize];
    let value = builder.ins().load(
        native_type(field.rep())?,
        MemFlags::trusted(),
        object,
        (layout.descriptor.payload_base() + field.offset()) as i32,
    );
    if matches!(field.rep(), RuntimeRep::LiftedRef | RuntimeRep::UnliftedRef) {
        builder.declare_value_needs_stack_map(value);
    }
    Ok(value)
}

fn store_pap_field(
    builder: &mut FunctionBuilder<'_>,
    object: ir::Value,
    descriptor: &ObjectDescriptor,
    logical: usize,
    value: ir::Value,
) -> Result<(), super::CompileError> {
    let Some(stored) = descriptor.payload().logical_to_stored()[logical] else {
        return Ok(());
    };
    let field = &descriptor.payload().fields()[stored as usize];
    builder.ins().store(
        MemFlags::trusted(),
        value,
        object,
        (descriptor.payload_base() + field.offset()) as i32,
    );
    Ok(())
}

fn native_type(rep: RuntimeRep) -> Result<ir::Type, super::CompileError> {
    Ok(match rep {
        RuntimeRep::LiftedRef | RuntimeRep::UnliftedRef | RuntimeRep::Address => types::I64,
        RuntimeRep::Int(8) | RuntimeRep::Word(8) => types::I8,
        RuntimeRep::Int(16) | RuntimeRep::Word(16) => types::I16,
        RuntimeRep::Int(32) | RuntimeRep::Word(32) => types::I32,
        RuntimeRep::Int(64) | RuntimeRep::Word(64) => types::I64,
        RuntimeRep::Float(32) => types::F32,
        RuntimeRep::Float(64) => types::F64,
        _ => {
            return Err(super::CompileError::Unsupported(
                super::Unsupported::Expression {
                    binding: ValueId(0),
                    node: 0,
                },
            ))
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn w5_a2_arity_split_preserves_void_and_demanded_results() {
        let entry = Signature {
            arguments: vec![RuntimeRep::Void, RuntimeRep::Int(64)],
            results: tidepool_repr::execution_schema::ResultContract::Returns(vec![
                RuntimeRep::Int(64),
            ]),
        };
        assert_eq!(
            classify(
                &entry,
                0,
                &Signature {
                    arguments: vec![RuntimeRep::Void],
                    results: tidepool_repr::execution_schema::ResultContract::Returns(vec![
                        RuntimeRep::LiftedRef,
                    ])
                }
            ),
            Some(Application::Partial { total_pending: 1 })
        );
        assert_eq!(
            classify(
                &entry,
                1,
                &Signature {
                    arguments: vec![RuntimeRep::Int(64)],
                    results: tidepool_repr::execution_schema::ResultContract::Returns(vec![
                        RuntimeRep::Int(64),
                    ])
                }
            ),
            Some(Application::Exact)
        );
        assert!(classify(
            &entry,
            0,
            &Signature {
                arguments: vec![RuntimeRep::Int(64)],
                results: tidepool_repr::execution_schema::ResultContract::Returns(vec![
                    RuntimeRep::LiftedRef,
                ])
            }
        )
        .is_none());
        let layouts = layouts(
            &tidepool_repr::execution_schema::testing::target(),
            [(ValueId(9), &entry)],
        )
        .unwrap();
        let pap = &layouts[&(ValueId(9), 1)];
        assert_eq!(
            pap.descriptor.payload().logical_to_stored(),
            &[Some(0), None]
        );
    }

    #[test]
    fn no_success_saturates_prefix_and_never_admits_excess_suffix() {
        let entry = Signature {
            arguments: vec![RuntimeRep::Int(64)],
            results: tidepool_repr::execution_schema::ResultContract::NoSuccess,
        };
        let demand = Signature {
            arguments: vec![RuntimeRep::Int(64), RuntimeRep::Int(64)],
            results: tidepool_repr::execution_schema::ResultContract::Returns(vec![
                RuntimeRep::Int(64),
            ]),
        };
        assert_eq!(
            classify(&entry, 0, &demand),
            Some(Application::NoSuccess { consumed: 1 })
        );
    }

    #[test]
    fn no_success_partial_application_still_returns_a_pap() {
        let entry = Signature {
            arguments: vec![RuntimeRep::Int(64), RuntimeRep::Int(64)],
            results: tidepool_repr::execution_schema::ResultContract::NoSuccess,
        };
        let demand = Signature {
            arguments: vec![RuntimeRep::Int(64)],
            results: tidepool_repr::execution_schema::ResultContract::Returns(vec![
                RuntimeRep::LiftedRef,
            ]),
        };
        assert_eq!(
            classify(&entry, 0, &demand),
            Some(Application::Partial { total_pending: 1 })
        );
    }

    #[test]
    fn returning_function_cannot_prove_an_oversaturated_no_success_suffix() {
        let entry = Signature {
            arguments: vec![RuntimeRep::Void, RuntimeRep::Int(64)],
            results: tidepool_repr::execution_schema::ResultContract::Returns(vec![
                RuntimeRep::LiftedRef,
            ]),
        };
        let demand = Signature {
            arguments: vec![RuntimeRep::Void, RuntimeRep::Int(64), RuntimeRep::Word(64)],
            results: tidepool_repr::execution_schema::ResultContract::NoSuccess,
        };
        assert_eq!(classify(&entry, 0, &demand), None);
        let pap_demand = Signature {
            arguments: demand.arguments[1..].to_vec(),
            results: demand.results.clone(),
        };
        assert_eq!(classify(&entry, 1, &pap_demand), None);
    }

    #[test]
    fn no_success_thunks_are_admitted_only_at_their_exact_prefix() {
        let entry = Signature {
            arguments: Vec::new(),
            results: tidepool_repr::execution_schema::ResultContract::NoSuccess,
        };
        let demand = Signature {
            arguments: Vec::new(),
            results: tidepool_repr::execution_schema::ResultContract::NoSuccess,
        };
        assert_eq!(
            classify(&entry, 0, &demand),
            Some(Application::NoSuccess { consumed: 0 })
        );
    }
}
