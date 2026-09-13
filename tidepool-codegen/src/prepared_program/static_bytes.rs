//! Immutable primitive-string storage and checked byte access.

use std::{collections::BTreeMap, sync::Arc};

use crate::pipeline::CodegenPipeline;
use cranelift_codegen::ir::{self, types, AbiParam, InstBuilder, MemFlags, Value};
use cranelift_frontend::FunctionBuilder;
use cranelift_module::{Linkage, Module};

/// Freeze after planning and retain the same Arc in CompiledProgram. Generated
/// host calls may embed Arc::as_ptr to this owner, never a pointer to a movable
/// map field. The address index is a second view of the same pinned storage.
pub(crate) struct PinnedBytes {
    by_value: BTreeMap<Vec<u8>, Arc<[u8]>>,
    by_address: Vec<Arc<[u8]>>,
}

impl PinnedBytes {
    pub(super) fn new(by_value: BTreeMap<Vec<u8>, Arc<[u8]>>) -> Self {
        let mut by_address: Vec<_> = by_value.values().cloned().collect();
        by_address.sort_unstable_by_key(|storage| storage.as_ptr() as usize);
        Self {
            by_value,
            by_address,
        }
    }

    pub(super) fn get(&self, logical: &[u8]) -> Option<&Arc<[u8]>> {
        self.by_value.get(logical)
    }

    /// C string length is admitted only within owned immutable storage. Scan
    /// its slice to the first NUL, never dereference the numeric input address
    /// or cross an allocation boundary looking for a terminator.
    pub(super) fn c_string_len(&self, address: usize) -> Option<usize> {
        let candidate = self
            .by_address
            .partition_point(|storage| storage.as_ptr() as usize <= address)
            .checked_sub(1)?;
        let storage = &self.by_address[candidate];
        let offset = address.checked_sub(storage.as_ptr() as usize)?;
        storage.get(offset..)?.iter().position(|byte| *byte == 0)
    }

    /// Permit an interior/one-past address with a signed offset only when the
    /// accessed byte belongs to that same allocation. Read through its owned
    /// slice, not through the untrusted numeric address. Backing storage includes
    /// GHC's implicit terminal NUL; logical wire bytes do not.
    pub(super) fn read_byte(&self, address: usize, index: i64) -> Option<u8> {
        let target = address.checked_add_signed(isize::try_from(index).ok()?)?;
        let candidate = self
            .by_address
            .partition_point(|storage| storage.as_ptr() as usize <= target)
            .checked_sub(1)?;
        let storage = &self.by_address[candidate];
        let base = storage.as_ptr() as usize;
        if address.checked_sub(base)? > storage.len() {
            return None;
        }
        storage.get(target.checked_sub(base)?).copied()
    }
}

/// Noncollecting strlen of a pointer into the compiled program's pinned
/// immutable byte pool. Numeric addresses are never dereferenced directly.
///
/// # Safety
/// vmctx and output belong to the active generated call; pool is the frozen
/// Arc allocation retained by its compiled owner throughout native execution.
pub(super) unsafe extern "C" fn prepared_c_string_len(
    vmctx: *mut crate::context::VMContext,
    pool: *const PinnedBytes,
    address: usize,
    output: *mut i64,
) -> i32 {
    let machine = unsafe { crate::machine_state::machine_state(vmctx) };
    let status = machine.prepared_call_status();
    if status != crate::prepared_control::CallStatus::Success {
        return status as i32;
    }
    let length = if pool.is_null() || output.is_null() {
        None
    } else {
        unsafe { &*pool }
            .c_string_len(address)
            .and_then(|length| i64::try_from(length).ok())
    };
    match length {
        Some(length) => {
            unsafe { output.write(length) };
            crate::prepared_control::CallStatus::Success as i32
        }
        None => {
            machine.set_first_cause(crate::host_fns::RuntimeError::BadPointer);
            machine.prepared_call_status() as i32
        }
    }
}

/// The host proves ownership and finds a terminator within that owner before
/// the generated entry publishes the length.
pub(super) fn emit_c_string_len(
    builder: &mut FunctionBuilder<'_>,
    pipeline: &mut CodegenPipeline,
    vmctx: Value,
    pool: &Arc<PinnedBytes>,
    address: Value,
) -> Result<Vec<Value>, super::CompileError> {
    let mut signature = ir::Signature::new(pipeline.isa.default_call_conv());
    signature.params = vec![AbiParam::new(types::I64); 4];
    signature.returns = vec![AbiParam::new(types::I32)];
    let host = pipeline
        .module
        .declare_function("prepared_c_string_len", Linkage::Import, &signature)
        .map_err(|error| crate::pipeline::PipelineError::Declaration(error.to_string()))?;
    let host = pipeline.module.declare_func_in_func(host, builder.func);

    let output_slot = builder.create_sized_stack_slot(ir::StackSlotData::new(
        ir::StackSlotKind::ExplicitSlot,
        8,
        3,
    ));
    let output = builder.ins().stack_addr(types::I64, output_slot, 0);
    let owner = builder
        .ins()
        .iconst(types::I64, Arc::as_ptr(pool) as usize as i64);
    let call = builder.ins().call(host, &[vmctx, owner, address, output]);
    let status = builder.inst_results(call)[0];
    let success = builder.ins().icmp_imm(
        ir::condcodes::IntCC::Equal,
        status,
        crate::prepared_control::CallStatus::Success as i64,
    );
    let valid = builder.create_block();
    let invalid = builder.create_block();
    builder.ins().brif(success, valid, &[], invalid, &[]);
    builder.switch_to_block(invalid);
    builder.seal_block(invalid);
    crate::alloc::emit_prepared_failure_return(builder, status);
    builder.switch_to_block(valid);
    builder.seal_block(valid);
    Ok(vec![builder.ins().load(
        types::I64,
        MemFlags::trusted(),
        output,
        0,
    )])
}

/// Noncollecting indexCharOffAddr# slow path. The result is Word(64), not
/// Word(32): GHC Char# has WordRep on the pinned 64-bit profile.
///
/// # Safety
/// vmctx and output belong to the active generated call; pool is the frozen
/// Arc allocation retained by its compiled owner throughout native execution.
/// address/index are untrusted and are never dereferenced as a raw pointer.
pub(super) unsafe extern "C" fn prepared_index_char(
    vmctx: *mut crate::context::VMContext,
    pool: *const PinnedBytes,
    address: usize,
    index: i64,
    output: *mut u64,
) -> i32 {
    let machine = unsafe { crate::machine_state::machine_state(vmctx) };
    let status = machine.prepared_call_status();
    if status != crate::prepared_control::CallStatus::Success {
        return status as i32;
    }
    let byte = if pool.is_null() || output.is_null() {
        None
    } else {
        unsafe { &*pool }.read_byte(address, index)
    };
    match byte {
        Some(byte) => {
            unsafe { output.write(u64::from(byte)) };
            crate::prepared_control::CallStatus::Success as i32
        }
        None => {
            machine.set_first_cause(crate::host_fns::RuntimeError::BadPointer);
            machine.prepared_call_status() as i32
        }
    }
}

/// The host proves ownership before reading its slice. The generated entry
/// publishes a character only after the host returns Success; failures retain
/// the invocation's first cause and return its actual status.
pub(super) fn emit_index_char(
    builder: &mut FunctionBuilder<'_>,
    pipeline: &mut CodegenPipeline,
    vmctx: Value,
    pool: &Arc<PinnedBytes>,
    address: Value,
    index: Value,
) -> Result<Vec<Value>, super::CompileError> {
    let mut signature = ir::Signature::new(pipeline.isa.default_call_conv());
    signature.params = vec![AbiParam::new(types::I64); 5];
    signature.returns = vec![AbiParam::new(types::I32)];
    let host = pipeline
        .module
        .declare_function("prepared_index_char", Linkage::Import, &signature)
        .map_err(|error| crate::pipeline::PipelineError::Declaration(error.to_string()))?;
    let host = pipeline.module.declare_func_in_func(host, builder.func);

    let output_slot = builder.create_sized_stack_slot(ir::StackSlotData::new(
        ir::StackSlotKind::ExplicitSlot,
        8,
        3,
    ));
    let output = builder.ins().stack_addr(types::I64, output_slot, 0);
    let owner = builder
        .ins()
        .iconst(types::I64, Arc::as_ptr(pool) as usize as i64);
    let call = builder
        .ins()
        .call(host, &[vmctx, owner, address, index, output]);
    let status = builder.inst_results(call)[0];
    let success = builder.ins().icmp_imm(
        ir::condcodes::IntCC::Equal,
        status,
        crate::prepared_control::CallStatus::Success as i64,
    );
    let valid = builder.create_block();
    let invalid = builder.create_block();
    builder.ins().brif(success, valid, &[], invalid, &[]);
    builder.switch_to_block(invalid);
    builder.seal_block(invalid);
    crate::alloc::emit_prepared_failure_return(builder, status);
    builder.switch_to_block(valid);
    builder.seal_block(valid);
    Ok(vec![builder.ins().load(
        types::I64,
        MemFlags::trusted(),
        output,
        0,
    )])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_pool_checks_bounds_without_dereferencing_raw_addresses() {
        let storage: Arc<[u8]> = Arc::from(&b"x\0"[..]);
        let base = storage.as_ptr() as usize;
        let pool = PinnedBytes::new(BTreeMap::from([(b"x".to_vec(), storage)]));
        assert_eq!(pool.read_byte(base, 0), Some(b'x'));
        assert_eq!(pool.read_byte(base, 1), Some(0));
        assert_eq!(pool.read_byte(base + 2, -1), Some(0));
        assert_eq!(pool.read_byte(base + 1, -1), Some(b'x'));
        assert_eq!(pool.read_byte(base, -1), None);
        assert_eq!(pool.read_byte(base, 2), None);
        assert_eq!(pool.read_byte(usize::MAX, 1), None);
        assert_eq!(pool.read_byte(0, 0), None);
    }

    #[test]
    fn c_string_len_stays_inside_its_pinned_owner() {
        let storage: Arc<[u8]> = Arc::from(&b"ab\0tail\0"[..]);
        let base = storage.as_ptr() as usize;
        let unterminated: Arc<[u8]> = Arc::from(&b"no nul"[..]);
        let unterminated_base = unterminated.as_ptr() as usize;
        let pool = PinnedBytes::new(BTreeMap::from([
            (b"ab\0tail".to_vec(), storage),
            (b"no nul".to_vec(), unterminated),
        ]));
        assert_eq!(pool.c_string_len(base), Some(2));
        assert_eq!(pool.c_string_len(base + 1), Some(1));
        assert_eq!(pool.c_string_len(base + 2), Some(0));
        assert_eq!(pool.c_string_len(base + 3), Some(4));
        assert_eq!(pool.c_string_len(base + 8), None);
        assert_eq!(pool.c_string_len(unterminated_base), None);
        assert_eq!(pool.c_string_len(0), None);
        assert_eq!(pool.c_string_len(usize::MAX), None);
    }
}
