//! Immutable primitive-string storage and checked byte access.

use std::{collections::BTreeMap, sync::Arc};

use crate::pipeline::CodegenPipeline;
use cranelift_codegen::ir::{types, InstBuilder, MemFlags, Value};
use cranelift_frontend::FunctionBuilder;
use tidepool_heap::execution_descriptor::ObjectDescriptor;
use tidepool_heap::external_storage::ExternalStorageKind;

/// The machine-wide permanent literal pool (see [`crate::machine_state::MachineState::intern_literal_bytes`])
/// and, before install, one program's own view of it: every literal its
/// generated code embeds an address for, whether reused from an
/// already-installed program or newly minted by this compile.
///
/// Content-addressed and append-only: a logical byte string always maps to
/// the same storage once interned, so entries are never removed and a
/// conflicting re-insertion never happens. Generated host calls may embed
/// `Arc::as_ptr` to a stored owner, never a pointer to a movable map field --
/// the address index is a second view of the same pinned storage, and an
/// entry, once inserted, is never replaced or dropped from either map for
/// the life of the value.
#[derive(Clone)]
pub(crate) struct PinnedBytes {
    by_value: BTreeMap<Vec<u8>, Arc<[u8]>>,
    by_address: BTreeMap<usize, PinnedLiteral>,
}

#[derive(Clone)]
struct PinnedLiteral {
    storage: Arc<[u8]>,
    logical_len: usize,
}

impl PinnedBytes {
    pub(super) fn new(by_value: BTreeMap<Vec<u8>, Arc<[u8]>>) -> Self {
        let by_address = by_value
            .iter()
            .map(|(logical, storage)| {
                (
                    storage.as_ptr() as usize,
                    PinnedLiteral {
                        storage: Arc::clone(storage),
                        logical_len: logical.len(),
                    },
                )
            })
            .collect();
        Self {
            by_value,
            by_address,
        }
    }

    /// A pool with no interned literals: the machine's pool before any
    /// program installs, or a standalone compile's own starting point.
    pub(crate) fn empty() -> Self {
        Self::new(BTreeMap::new())
    }

    pub(super) fn get(&self, logical: &[u8]) -> Option<&Arc<[u8]>> {
        self.by_value.get(logical)
    }

    fn insert_owned(&mut self, value: Vec<u8>, storage: Arc<[u8]>) {
        self.by_address.insert(
            storage.as_ptr() as usize,
            PinnedLiteral {
                storage: Arc::clone(&storage),
                logical_len: value.len(),
            },
        );
        self.by_value.insert(value, storage);
    }

    /// This pool plus every entry of `additions` not already present here
    /// (by content). Content this pool already has keeps the address every
    /// earlier compile already baked into generated code -- `additions`
    /// never overrides an existing entry.
    pub(crate) fn merged(&self, additions: &BTreeMap<Vec<u8>, Arc<[u8]>>) -> Self {
        let mut merged = self.clone();
        for (value, storage) in additions {
            if !merged.by_value.contains_key(value) {
                merged.insert_owned(value.clone(), Arc::clone(storage));
            }
        }
        merged
    }

    /// Fold one installed program's literal view into this permanent
    /// machine-wide pool, in place. Idempotent.
    ///
    /// Content this pool already carries keeps its canonical storage in
    /// `by_value`, so later compiles reuse one address. But the program's OWN
    /// storage for that content is still what its generated code and the
    /// heap objects it built embed: two programs planned against the same
    /// pool snapshot each mint storage for content neither had, and the
    /// second to install must not lose its copy. Every distinct storage is
    /// therefore kept alive and resolvable through `by_address` for the
    /// machine's life, even when it is a content duplicate.
    pub(crate) fn absorb(&mut self, other: &PinnedBytes) {
        for (value, storage) in &other.by_value {
            if !self.by_value.contains_key(value) {
                self.insert_owned(value.clone(), Arc::clone(storage));
            } else {
                self.by_address
                    .entry(storage.as_ptr() as usize)
                    .or_insert_with(|| PinnedLiteral {
                        storage: Arc::clone(storage),
                        logical_len: value.len(),
                    });
            }
        }
    }

    /// Observe only logical literal bytes, excluding the implicit terminal NUL.
    pub(crate) fn logical_suffix(&self, address: usize) -> Option<&[u8]> {
        let (&base, literal) = self.by_address.range(..=address).next_back()?;
        let offset = address.checked_sub(base)?;
        literal.storage.get(offset..literal.logical_len)
    }

    /// C string length is admitted only within owned immutable storage. Scan
    /// its slice to the first NUL, never dereference the numeric input address
    /// or cross an allocation boundary looking for a terminator.
    pub(crate) fn c_string_len(&self, address: usize) -> Option<usize> {
        let (&base, literal) = self.by_address.range(..=address).next_back()?;
        let offset = address.checked_sub(base)?;
        literal
            .storage
            .get(offset..)?
            .iter()
            .position(|byte| *byte == 0)
    }

    /// A complete span from one pinned allocation. This also admits an empty
    /// span at its end; unknown addresses never become raw slices.
    pub(crate) fn read_range(&self, address: usize, length: usize) -> Option<&[u8]> {
        self.read_range_offset(address, 0, length)
    }

    /// Resolve a signed byte offset only within the pinned allocation owning
    /// the original address. The offset cannot acquire authority over a
    /// neighboring allocation, even when its numeric target lands inside one.
    pub(crate) fn read_range_offset(
        &self,
        address: usize,
        offset: i64,
        length: usize,
    ) -> Option<&[u8]> {
        let (&base, literal) = self.by_address.range(..=address).next_back()?;
        let storage = &literal.storage;
        if address.checked_sub(base)? > storage.len() {
            return None;
        }
        let target = address.checked_add_signed(isize::try_from(offset).ok()?)?;
        let target_offset = target.checked_sub(base)?;
        storage.get(target_offset..target_offset.checked_add(length)?)
    }

    /// Permit an interior/one-past address with a signed offset only when the
    /// accessed byte belongs to that same allocation. Read through its owned
    /// slice, not through the untrusted numeric address. Backing storage includes
    /// GHC's implicit terminal NUL; logical wire bytes do not.
    pub(crate) fn read_byte(&self, address: usize, index: i64) -> Option<u8> {
        self.read_range_offset(address, index, 1)
            .and_then(|bytes| bytes.first().copied())
    }
}

/// Copy a complete span from one pinned source owner to one authenticated
/// mutable byte-array owner. All bounds and provenance checks precede the
/// ledger-mediated write, so a failed call cannot partially mutate storage.
///
/// # Safety
/// vmctx belongs to the active generated call, descriptor is retained by its
/// compiled program, and dest_ref is an untrusted generated reference.
pub(super) unsafe extern "C" fn prepared_copy_addr_to_byte_array(
    vmctx: *mut crate::context::VMContext,
    descriptor: *const ObjectDescriptor,
    address: usize,
    dest_ref: *mut u8,
    offset: i64,
    count: i64,
) -> i32 {
    use crate::{host_fns::RuntimeError, prepared_control::CallStatus};

    let machine = unsafe { crate::machine_state::machine_state(vmctx) };
    if machine.prepared_call_status() != CallStatus::Success {
        return machine.prepared_call_status() as i32;
    }
    let result = (|| {
        let (published, len) = unsafe {
            super::arrays::active_payload(
                machine,
                vmctx,
                dest_ref,
                descriptor,
                ExternalStorageKind::Bytes,
            )
        }?;
        let offset = usize::try_from(offset)
            .ok()
            .filter(|&offset| offset <= len)
            .ok_or(RuntimeError::ArrayIndexOutOfBounds { index: offset, len })?;
        let count = usize::try_from(count)
            .map_err(|_| RuntimeError::ArrayIndexOutOfBounds { index: count, len })?;
        let end = offset
            .checked_add(count)
            .ok_or(RuntimeError::ArrayIndexOutOfBounds {
                index: i64::MAX,
                len,
            })?;
        if end > len {
            return Err(RuntimeError::ArrayIndexOutOfBounds {
                index: i64::try_from(end - 1).unwrap_or(i64::MAX),
                len,
            });
        }
        let literal = machine.resolve_literal_bytes(|pool| {
            pool.read_range(address, count)
                .map(|source| machine.store_external_bytes(published, offset, source))
        });
        match literal {
            Some(stored) => stored,
            None => {
                let external = machine
                    .read_external_address(address, count)
                    .map_err(|error| super::arrays::storage_error(error, 0))?;
                machine.store_external_bytes(published, offset, &external)
            }
        }
        .map_err(|error| super::arrays::storage_error(error, offset as i64))?;
        Ok(())
    })();
    match result {
        Ok(()) => CallStatus::Success as i32,
        Err(error) => super::arrays::array_error(machine, error),
    }
}

pub(super) fn emit_copy_addr_to_byte_array(
    builder: &mut FunctionBuilder<'_>,
    pipeline: &mut CodegenPipeline,
    vmctx: Value,
    descriptor: &ObjectDescriptor,
    arguments: &[Value],
) -> Result<Vec<Value>, super::CompileError> {
    let host =
        super::arrays::declare_host(builder, pipeline, "prepared_copy_addr_to_byte_array", 6)?;
    let descriptor_owner = builder.ins().iconst(
        types::I64,
        descriptor as *const ObjectDescriptor as usize as i64,
    );
    let call = builder.ins().call(
        host,
        &[
            vmctx,
            descriptor_owner,
            arguments[0],
            arguments[1],
            arguments[2],
            arguments[3],
        ],
    );
    let status = builder.inst_results(call)[0];
    super::arrays::finish_checked_call(builder, status);
    Ok(Vec::new())
}

/// Noncollecting strlen of a pointer into the compiled program's pinned
/// immutable byte pool. Numeric addresses are never dereferenced directly.
///
/// # Safety
/// vmctx and output belong to the active generated call. Literal bytes are
/// authenticated through the pools registered with its machine.
pub(super) unsafe extern "C" fn prepared_c_string_len(
    vmctx: *mut crate::context::VMContext,
    address: usize,
    output: *mut i64,
) -> i32 {
    let machine = unsafe { crate::machine_state::machine_state(vmctx) };
    let status = machine.prepared_call_status();
    if status != crate::prepared_control::CallStatus::Success {
        return status as i32;
    }
    let length = if output.is_null() {
        None
    } else {
        machine
            .resolve_literal_bytes(|pool| pool.c_string_len(address))
            .or_else(|| machine.external_c_string_len(address).ok())
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
    address: Value,
) -> Result<Vec<Value>, super::CompileError> {
    let host = super::arrays::declare_host(builder, pipeline, "prepared_c_string_len", 3)?;
    let output = super::arrays::output_slot(builder);
    let call = builder.ins().call(host, &[vmctx, address, output]);
    let status = builder.inst_results(call)[0];
    super::arrays::finish_checked_call(builder, status);
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
/// vmctx and output belong to the active generated call. Literal bytes are
/// authenticated through the pools registered with its machine.
/// address/index are untrusted and are never dereferenced as a raw pointer.
pub(super) unsafe extern "C" fn prepared_index_char(
    vmctx: *mut crate::context::VMContext,
    address: usize,
    index: i64,
    output: *mut u64,
) -> i32 {
    let machine = unsafe { crate::machine_state::machine_state(vmctx) };
    let status = machine.prepared_call_status();
    if status != crate::prepared_control::CallStatus::Success {
        return status as i32;
    }
    let byte = if output.is_null() {
        Err(crate::host_fns::RuntimeError::BadPointer)
    } else if let Some(byte) = machine.resolve_literal_bytes(|pool| pool.read_byte(address, index))
    {
        Ok(byte)
    } else {
        machine
            .read_external_address_offset(address, index, 1)
            .map_err(|error| super::arrays::storage_error(error, index))
            .and_then(|bytes| {
                bytes
                    .first()
                    .copied()
                    .ok_or(crate::host_fns::RuntimeError::BadPointer)
            })
    };
    match byte {
        Ok(byte) => {
            unsafe { output.write(u64::from(byte)) };
            crate::prepared_control::CallStatus::Success as i32
        }
        Err(error) => {
            machine.set_first_cause(error);
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
    address: Value,
    index: Value,
) -> Result<Vec<Value>, super::CompileError> {
    let host = super::arrays::declare_host(builder, pipeline, "prepared_index_char", 4)?;
    let output = super::arrays::output_slot(builder);
    let call = builder.ins().call(host, &[vmctx, address, index, output]);
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
    use super::*;

    fn storage_for(value: &[u8]) -> Arc<[u8]> {
        let mut storage = value.to_vec();
        storage.push(0);
        Arc::from(storage)
    }

    /// Two programs planned against the same pool snapshot each mint their
    /// own storage for content neither had. After both install, the pool
    /// resolves BOTH addresses (each program's code and heap objects embed
    /// its own), while content lookup keeps the first as canonical.
    #[test]
    fn absorb_keeps_a_duplicate_content_storage_resolvable_by_address() {
        let first = storage_for(b"shared");
        let second = storage_for(b"shared");
        assert_ne!(first.as_ptr(), second.as_ptr());
        let program_a = PinnedBytes::new(BTreeMap::from([(b"shared".to_vec(), Arc::clone(&first))]));
        let program_b =
            PinnedBytes::new(BTreeMap::from([(b"shared".to_vec(), Arc::clone(&second))]));

        let mut pool = PinnedBytes::empty();
        pool.absorb(&program_a);
        pool.absorb(&program_b);

        assert!(Arc::ptr_eq(pool.get(b"shared").unwrap(), &first));
        assert_eq!(pool.logical_suffix(first.as_ptr() as usize), Some(&b"shared"[..]));
        assert_eq!(pool.logical_suffix(second.as_ptr() as usize), Some(&b"shared"[..]));
        assert_eq!(pool.logical_suffix(second.as_ptr() as usize + 2), Some(&b"ared"[..]));
    }
    use std::collections::HashSet;
    use tidepool_heap::external_storage::ExternalStorageValidationError;
    use tidepool_repr::execution_schema::{Architecture, Endianness, TargetDescriptor};

    fn with_copy_fixture(
        test: impl FnOnce(
            &mut crate::context::VMContext,
            &crate::machine_state::MachineState,
            &Arc<ObjectDescriptor>,
            usize,
            *mut u8,
            *mut u8,
        ),
    ) {
        let target = TargetDescriptor {
            architecture: Architecture::X86_64,
            endianness: Endianness::Little,
            pointer_width: 64,
            word_width: 64,
            abi: "sysv64".into(),
            features: Vec::new(),
        };
        let descriptor =
            Arc::new(ObjectDescriptor::external(ExternalStorageKind::Bytes, &target).unwrap());
        let machine = crate::machine_state::MachineState::new();
        let extent = descriptor.allocation_extent() as usize;
        machine
            .install_prepared_buffer(vec![0_u64; extent / 8], vec![descriptor.clone()])
            .unwrap();
        let (start, size) = machine.gc_active_range().unwrap();
        let payload = machine
            .allocate_external_storage(ExternalStorageKind::Bytes, 4)
            .unwrap();
        machine.store_external_bytes(payload, 0, b"zzzz").unwrap();
        unsafe {
            descriptor.initialize_header(start);
            descriptor
                .external_payload_slot(start, extent)
                .unwrap()
                .write(payload);
        }
        let mut vmctx = unsafe {
            crate::context::VMContext::new(start, start.add(size), crate::host_fns::gc_trigger)
        };
        vmctx.alloc_ptr = unsafe { start.add(extent) };
        vmctx.machine_state = &machine as *const _ as *mut _;
        let storage: Arc<[u8]> = Arc::from(&b"ab\0"[..]);
        let base = storage.as_ptr() as usize;
        machine.absorb_interned_bytes(&Arc::new(PinnedBytes::new(BTreeMap::from([(
            b"ab".to_vec(),
            storage,
        )]))));
        let dest_ref = (start as usize | usize::from(descriptor.tag())) as *mut u8;
        test(&mut vmctx, &machine, &descriptor, base, dest_ref, payload);
    }

    #[test]
    fn logical_suffix_is_bounded_by_literal_bytes() {
        let storage: Arc<[u8]> = Arc::from(&b"ab\0tail\0"[..]);
        let base = storage.as_ptr() as usize;
        let pool = PinnedBytes::new(BTreeMap::from([(b"ab\0tail".to_vec(), storage)]));
        assert_eq!(pool.logical_suffix(base), Some(&b"ab\0tail"[..]));
        assert_eq!(pool.logical_suffix(base + 2), Some(&b"\0tail"[..]));
        assert_eq!(pool.logical_suffix(base + 7), Some(&b""[..]));
        assert_eq!(pool.logical_suffix(base + 8), None);
        assert_eq!(pool.logical_suffix(0), None);
        assert_eq!(pool.logical_suffix(usize::MAX), None);
    }

    #[test]
    fn string_hosts_accept_authenticated_external_byte_addresses() {
        with_copy_fixture(|vmctx, machine, descriptor, _, dest_ref, payload| {
            let source = machine.allocate_external_bytes(4, 8).unwrap();
            machine.store_external_bytes(source, 0, b"ab\0z").unwrap();
            let address = machine.external_byte_address(source).unwrap();
            let mut length = -1;
            let mut character = 0;
            unsafe {
                assert_eq!(prepared_c_string_len(vmctx, address, &mut length), 0);
                assert_eq!(
                    prepared_index_char(vmctx, address + 1, -1, &mut character),
                    0
                );
                assert_eq!(
                    prepared_copy_addr_to_byte_array(
                        vmctx,
                        Arc::as_ptr(descriptor),
                        address,
                        dest_ref,
                        0,
                        4
                    ),
                    0
                );
            }
            assert_eq!(length, 2);
            assert_eq!(character, u64::from(b'a'));
            assert_eq!(machine.copy_external_bytes(payload).unwrap(), b"ab\0z");
        });
    }

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

    #[test]
    fn read_range_accepts_owned_empty_end_but_never_crosses_allocation() {
        let storage: Arc<[u8]> = Arc::from(&b"ab\0"[..]);
        let base = storage.as_ptr() as usize;
        let pool = PinnedBytes::new(BTreeMap::from([(b"ab".to_vec(), storage)]));
        assert_eq!(pool.read_range(base, 3), Some(&b"ab\0"[..]));
        assert_eq!(pool.read_range(base + 1, 2), Some(&b"b\0"[..]));
        assert_eq!(pool.read_range(base + 3, 0), Some(&b""[..]));
        assert_eq!(pool.read_range(base + 2, 2), None);
        assert_eq!(pool.read_range(base + 4, 0), None);
        assert_eq!(pool.read_range(0, 0), None);
        assert_eq!(pool.read_range(usize::MAX, 1), None);
    }

    #[test]
    fn signed_ranges_retain_the_original_pinned_owner() {
        let first: Arc<[u8]> = Arc::from(&b"abcd"[..]);
        let second: Arc<[u8]> = Arc::from(&b"wxyz"[..]);
        let first_address = first.as_ptr() as usize;
        let second_address = second.as_ptr() as usize;
        let pool = PinnedBytes::new(BTreeMap::from([
            (b"first".to_vec(), first),
            (b"second".to_vec(), second),
        ]));

        assert_eq!(
            pool.read_range_offset(first_address + 4, -4, 4),
            Some(&b"abcd"[..])
        );
        assert_eq!(
            pool.read_range_offset(first_address + 2, -1, 3),
            Some(&b"bcd"[..])
        );
        assert_eq!(pool.read_range_offset(first_address + 2, -3, 1), None);
        assert_eq!(pool.read_range_offset(first_address + 2, i64::MAX, 1), None);

        let cross_owner_offset =
            i64::try_from(second_address as i128 - first_address as i128).unwrap();
        assert_eq!(
            pool.read_range_offset(first_address, cross_owner_offset, 1),
            None
        );
    }

    #[test]
    fn copy_addr_host_uses_ledger_write_and_invalidates_stale_sweep_plan() {
        with_copy_fixture(|vmctx, machine, descriptor, base, dest_ref, payload| {
            let plan = machine
                .plan_external_sweep(&HashSet::from([payload]))
                .unwrap();
            let status = unsafe {
                prepared_copy_addr_to_byte_array(
                    vmctx,
                    Arc::as_ptr(descriptor),
                    base,
                    dest_ref,
                    1,
                    3,
                )
            };
            assert_eq!(status, crate::prepared_control::CallStatus::Success as i32);
            assert_eq!(machine.copy_external_bytes(payload).unwrap(), b"zab\0");
            assert!(matches!(
                machine.commit_external_sweep(plan),
                Err(ExternalStorageValidationError::LedgerChanged)
            ));
        });
    }

    #[test]
    fn copy_addr_host_rejects_bad_spans_before_mutating_destination() {
        for (source_shift, offset, count, invalid_dest, expected) in [
            (None, 0, 1, false, crate::host_fns::RuntimeError::BadPointer),
            (
                Some(2),
                0,
                2,
                false,
                crate::host_fns::RuntimeError::BadPointer,
            ),
            (
                Some(0),
                -1,
                1,
                false,
                crate::host_fns::RuntimeError::ArrayIndexOutOfBounds { index: -1, len: 4 },
            ),
            (
                Some(0),
                0,
                -1,
                false,
                crate::host_fns::RuntimeError::ArrayIndexOutOfBounds { index: -1, len: 4 },
            ),
            (
                Some(0),
                3,
                2,
                false,
                crate::host_fns::RuntimeError::ArrayIndexOutOfBounds { index: 4, len: 4 },
            ),
            (
                Some(0),
                0,
                1,
                true,
                crate::host_fns::RuntimeError::BadPointer,
            ),
        ] {
            with_copy_fixture(|vmctx, machine, descriptor, base, dest_ref, payload| {
                let address = source_shift.map_or(0, |shift| base + shift);
                let destination = if invalid_dest {
                    std::ptr::null_mut()
                } else {
                    dest_ref
                };
                let status = unsafe {
                    prepared_copy_addr_to_byte_array(
                        vmctx,
                        Arc::as_ptr(descriptor),
                        address,
                        destination,
                        offset,
                        count,
                    )
                };
                assert_ne!(status, crate::prepared_control::CallStatus::Success as i32);
                assert_eq!(machine.copy_external_bytes(payload).unwrap(), b"zzzz");
                assert_eq!(machine.take_runtime_error(), Some(expected));
            });
        }
    }
}
