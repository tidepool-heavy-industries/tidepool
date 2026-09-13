//! Explicit-worklist native emission over the checked flat arena.

use cranelift_codegen::ir::{self, types, Block, InstBuilder, MemFlags, Value};
use cranelift_frontend::FunctionBuilder;
use cranelift_module::FuncId;
use std::collections::BTreeMap;
use std::sync::Arc;
use tidepool_heap::execution_descriptor::ObjectDescriptor;
use tidepool_repr::execution_schema::ValueId;
use super::{plan::ProgramPlan, CompileError};
use crate::pipeline::CodegenPipeline;

/// All generated entry points share (vmctx, tagged_environment, physical args).
/// Case/let continuations are native blocks, not recursively emitted Rust calls.
pub(super) fn emit_function(
    _plan: &ProgramPlan<'_>,
    _id: ValueId,
    _functions: &BTreeMap<ValueId, FuncId>,
    _pipeline: &mut CodegenPipeline,
) -> Result<(), CompileError> {
    todo!("wave4:EMIT_FUNCTION")
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
    let null = builder.ins().icmp_imm(ir::condcodes::IntCC::Equal, object, 0);
    builder.ins().brif(null, invalid, &[], nonnull, &[]);
    builder.switch_to_block(nonnull);
    builder.seal_block(nonnull);
    let header = builder.ins().load(types::I64, MemFlags::trusted(), object, 0);
    // Initial header is a live pinned descriptor address. A stateful or foreign
    // family object cannot match any branch, even with convincing tag bits.
    for descriptor in family {
        let next = builder.create_block();
        let matched = builder.create_block();
        let same = builder.ins().icmp_imm(
            ir::condcodes::IntCC::Equal, header, descriptor.initial_header_word() as i64);
        builder.ins().brif(same, matched, &[], next, &[]);
        builder.switch_to_block(matched);
        builder.seal_block(matched);
        let tag = builder.ins().band_imm(scrutinee, 7);
        let unknown = builder.ins().icmp_imm(ir::condcodes::IntCC::Equal, tag, 0);
        let generic = builder.ins().icmp_imm(ir::condcodes::IntCC::Equal, tag, 7);
        let canonical = builder.ins().icmp_imm(ir::condcodes::IntCC::Equal, tag, descriptor.tag() as i64);
        let valid = builder.ins().bor(unknown, generic);
        let valid = builder.ins().bor(valid, canonical);
        let destination = alternatives.iter()
            .find(|(candidate, _)| Arc::ptr_eq(candidate, descriptor))
            .map(|(_, block)| *block).or(default).unwrap_or(invalid);
        builder.ins().brif(valid, destination, &[], invalid, &[]);
        builder.switch_to_block(next);
        builder.seal_block(next);
    }
    builder.ins().jump(invalid, &[]);
}
