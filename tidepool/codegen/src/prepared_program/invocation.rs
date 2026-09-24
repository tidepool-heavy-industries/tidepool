//! One invocation owns every mutable address reached by its generated code.
//!
//! `run_entry` uses this owner's entry and observation paths. Entry admits
//! arguments, installs roots, executes and checks status before publishing
//! rooted results. Observation may collect and must re-read root slots.
//! Retention promotes registered result slots within this same lifetime;
//! no independent RootSlot or native pointer may escape this owner.
//!
//! Keep the compiled borrow and private OldSpace together: OldSpace's legacy
//! unsafe Send must not be used to transfer !Send code custody. This owner is
//! intentionally !Send via its compiled-program borrow. Rc holds MachineState
//! through shared access even when this owner moves. There is no self-borrowed
//! cleanup guard: Drop removes registries before releasing their allocations.

use super::roots::{OldSpaceScope, RootTables, RootWords};
use super::run::{
    heap_top_extent, initialize_heap_tops, runtime_error, runtime_error_for_status,
    runtime_error_from_machine, runtime_error_from_machine_or_observation,
    runtime_error_without_machine, try_words,
};
use super::safepoint::NativeStackBounds;
use super::{CompiledProgram, ExecutionError};
use crate::host_fns::{prepared_gc_trigger, RuntimeError};
use crate::prepared_control::{CallStatus, PreparedSafepoint};
use crate::{context::VMContext, machine_state::MachineState, old_space::OldSpace};
use std::rc::Rc;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use tidepool_heap::execution_descriptor::DescriptorTraceError;
use tidepool_heap::static_region::StaticRegion;
use tidepool_repr::execution_schema::{ResultContract, RuntimeRep, StorageLayout, ValueId};

pub(super) struct PreparedInvocation<'code> {
    pub(super) program: &'code CompiledProgram,
    // Never unwrap, replace, or acquire mutable Rc access while VMContext
    // contains its pointer. MachineState mutates through its existing cells.
    pub(super) machine: Rc<MachineState>,
    pub(super) vmctx: VMContext,
    static_catalog: tidepool_heap::static_region::StaticRegionCatalog,
    /// This invocation's own root block for the program's tops, published
    /// through `root_tables` exactly as an installed machine publishes it.
    pub(super) roots: RootWords,
    /// Owns the table `vmctx.root_tables` points at for the invocation's
    /// life; never read from Rust.
    _root_tables: RootTables,
    pub(super) results: RootWords,
    pub(super) result_contract: ResultContract,
    pub(super) result_layout: StorageLayout,
    pub(super) collections_before: u64,
    /// Boxed so the machine's borrowed admission pointer remains stable even
    /// while this invocation value is moved out of `enter`.
    old_space: Box<OldSpace>,
}

fn static_catalog(
    region: &Arc<StaticRegion>,
) -> Result<tidepool_heap::static_region::StaticRegionCatalog, RuntimeError> {
    let mut catalog = tidepool_heap::static_region::StaticRegionCatalog::new();
    catalog
        .insert(Arc::clone(region))
        .map_err(|error| match error {
            DescriptorTraceError::MetadataAllocation => RuntimeError::HeapOverflow,
            _ => crate::host_fns::bad_pointer(),
        })?;
    Ok(catalog)
}

impl<'code> PreparedInvocation<'code> {
    pub(super) fn enter(
        program: &'code CompiledProgram,
        entry: ValueId,
        arguments: &[u64],
        options: &super::run::RunOptions,
        cancel: Arc<AtomicBool>,
    ) -> Result<Self, ExecutionError> {
        Self::enter_with_poll_failure(program, entry, arguments, options, cancel, None)
    }

    #[cfg(test)]
    pub(super) fn enter_with_injected_poll_failure(
        program: &'code CompiledProgram,
        entry: ValueId,
        arguments: &[u64],
        options: &super::run::RunOptions,
        cancel: Arc<AtomicBool>,
        failure: (PreparedSafepoint, usize, RuntimeError),
    ) -> Result<Self, ExecutionError> {
        Self::enter_with_poll_failure(program, entry, arguments, options, cancel, Some(failure))
    }

    fn enter_with_poll_failure(
        program: &'code CompiledProgram,
        entry: ValueId,
        arguments: &[u64],
        options: &super::run::RunOptions,
        cancel: Arc<AtomicBool>,
        failure: Option<(PreparedSafepoint, usize, RuntimeError)>,
    ) -> Result<Self, ExecutionError> {
        let compiled = program
            .entries
            .get(&entry)
            .ok_or(ExecutionError::MissingEntry(entry))?;
        if compiled
            .abi
            .semantic_arguments()
            .iter()
            .any(|rep| matches!(rep, RuntimeRep::LiftedRef | RuntimeRep::UnliftedRef))
        {
            return Err(ExecutionError::Unsupported(
                super::Unsupported::HostArguments(entry),
            ));
        }
        if arguments.len() != compiled.abi.physical_arguments().len() {
            return Err(ExecutionError::Arguments {
                expected: compiled.abi.physical_arguments().len(),
                actual: arguments.len(),
            });
        }

        let max_native_frame = program.pipeline.native_frame_maximum();
        let native_frame_reserve = max_native_frame
            .checked_mul(2)
            .ok_or_else(|| runtime_error_without_machine(RuntimeError::StackOverflow))?;
        let bounds = NativeStackBounds::current().map_err(runtime_error_without_machine)?;
        bounds
            .ensure_current_frame_reserve(native_frame_reserve)
            .map_err(runtime_error_without_machine)?;
        let prepared_stack_limit = bounds
            .limit_with_frame_reserve(max_native_frame)
            .map_err(runtime_error_without_machine)?;

        let statics = Arc::new(program.statics.instantiate()?);
        let static_catalog = static_catalog(&statics).map_err(runtime_error_without_machine)?;
        // This invocation's own root block carries the program's tops,
        // exactly as an installed machine's block does.
        let roots = RootWords::new(program.root_words)?;
        let top_table = &roots;
        for (&id, &slot) in &program.top_slots {
            if program.heap_top_specs.iter().any(|spec| spec.id == id) {
                continue;
            }
            let value = statics
                .entry(id)
                .or_else(|| {
                    program
                        .byte_tops
                        .get(&id)
                        .map(|bytes| bytes.as_ptr() as usize)
                })
                .ok_or(ExecutionError::MissingEntry(id))?;
            top_table.write(slot, value as u64)?;
        }

        let heap_reserve = heap_top_extent(&program.heap_top_specs)?;
        let nursery_bytes = options.nursery_bytes.max(heap_reserve);
        let nursery = try_words(nursery_bytes.div_ceil(std::mem::size_of::<u64>()))?;
        let mut argument_area = try_words(arguments.len())?;
        argument_area.copy_from_slice(arguments);
        let result_words = (compiled.abi.result_layout().payload_size() as usize)
            .div_ceil(std::mem::size_of::<u64>());
        let results = super::run::try_root_words(result_words.max(1))?;

        let machine = Rc::new(MachineState::new());
        machine.register_prepared_entries(
            program.callables.iter().map(|callable| {
                (
                    callable.header,
                    callable.signature.clone(),
                    program.pipeline.get_function_ptr(callable.function),
                )
            }),
            program
                .thunk_entries
                .iter()
                .map(|&(header, function)| (header, program.pipeline.get_function_ptr(function))),
        );
        machine.absorb_interned_bytes(&program.bytes);
        machine.set_cancel_flag(Arc::clone(&cancel));
        machine.set_stack_map_registry(&program.pipeline.stack_maps);
        if let Err(error) = machine.install_prepared_buffer_with_static_region(
            nursery,
            program.descriptors.clone(),
            Some(Arc::clone(&statics)),
        ) {
            machine.clear_stack_map_registry();
            machine.clear_cancel_flag();
            return Err(runtime_error(&machine, error));
        }
        let (start, size) = match machine.gc_active_range() {
            Some(range) => range,
            None => {
                machine.clear_gc_state();
                machine.clear_stack_map_registry();
                machine.clear_cancel_flag();
                return Err(runtime_error(&machine, crate::host_fns::bad_pointer()));
            }
        };
        let heap_used = match initialize_heap_tops(
            start,
            size,
            &program.heap_top_specs,
            &program.top_slots,
            top_table,
            &statics,
            &program.byte_tops,
            &program.bytes,
            &program.import_slots,
        ) {
            Ok(heap_used) => heap_used,
            Err(cause) => {
                machine.clear_rust_roots();
                machine.free_session_heap();
                machine.clear_stack_map_registry();
                machine.clear_cancel_flag();
                return Err(runtime_error(&machine, cause));
            }
        };
        let mut vmctx = unsafe { VMContext::new(start, start.add(size)) };
        vmctx.alloc_ptr = unsafe { start.add(heap_used) };
        vmctx.machine_state = Rc::as_ptr(&machine).cast_mut();
        vmctx.prepared_stack_limit = prepared_stack_limit;
        let mut root_tables = RootTables::default();
        vmctx.root_tables = root_tables.publish(program.image_slot, roots.as_mut_ptr())?;

        let mut invocation = Self {
            program,
            machine,
            vmctx,
            static_catalog,
            roots,
            _root_tables: root_tables,
            results,
            result_contract: compiled.abi.semantic_results().clone(),
            result_layout: compiled.abi.result_layout().clone(),
            collections_before: 0,
            old_space: Box::new(OldSpace::new()),
        };
        for spec in &invocation.program.heap_top_specs {
            if let Some(root) = invocation
                .program
                .top_slots
                .get(&spec.id)
                .and_then(|&slot| invocation.roots.slot_address(slot))
            {
                invocation.machine.register_rust_root(root);
            }
        }
        invocation.collections_before = invocation.machine.gc_generation();
        #[cfg(test)]
        if let Some((point, occurrence, cause)) = failure {
            invocation
                .machine
                .fail_prepared_at(point, occurrence, cause);
        }
        #[cfg(not(test))]
        let _ = failure;

        let pointer = invocation
            .program
            .pipeline
            .get_function_ptr(compiled.adapter);
        let raw_status = {
            let _intrinsic = super::ActiveIntrinsicScope::new(
                &invocation.machine,
                invocation.program,
                &invocation.static_catalog,
                &invocation.program.descriptor_registry,
            )?;
            let _scope = OldSpaceScope::new(&invocation.machine, &invocation.old_space)?;
            unsafe {
                let adapter: extern "C" fn(*mut VMContext, *mut u64, *const u64) -> i32 =
                    std::mem::transmute(pointer);
                adapter(
                    &mut invocation.vmctx,
                    invocation.results.as_mut_ptr(),
                    argument_area.as_ptr(),
                )
            }
        };
        let status = match CallStatus::from_raw(i64::from(raw_status)) {
            Ok(status) => status,
            Err(_) => {
                invocation
                    .machine
                    .set_first_cause(crate::host_fns::bad_pointer());
                return Err(runtime_error_from_machine(&invocation.machine));
            }
        };
        if status == CallStatus::IntegrityFailure {
            invocation
                .machine
                .set_first_cause(crate::host_fns::bad_pointer());
        }
        if status != CallStatus::Success
            || invocation.machine.prepared_call_status() != CallStatus::Success
        {
            super::forcing::describe_raised_exception(
                &invocation.machine,
                invocation.program,
                &mut invocation.vmctx,
                &invocation.static_catalog,
                &invocation.program.descriptor_registry,
                &invocation.old_space,
            );
            return Err(runtime_error_for_status(&invocation.machine, status));
        }
        if invocation.result_contract == ResultContract::NoSuccess {
            return Err(runtime_error(
                &invocation.machine,
                RuntimeError::NoSuccessReturned,
            ));
        }

        super::run::register_result_roots(
            &invocation.machine,
            &invocation.results,
            &invocation.result_layout,
        );
        if options.collect_before_observation {
            invocation.collect(0)?;
        }
        Ok(invocation)
    }

    pub(super) fn observe(
        &mut self,
        budget: usize,
    ) -> Result<super::run::RunResult, ExecutionError> {
        if self.machine.prepared_call_status() != CallStatus::Success {
            return Err(runtime_error_from_machine(&self.machine));
        }
        let result_reps = self
            .result_contract
            .returned_reps()
            .ok_or_else(|| runtime_error(&self.machine, RuntimeError::NoSuccessReturned))?;
        let _scope = OldSpaceScope::new(&self.machine, &self.old_space)?;
        let result_words = self.results.snapshot();
        let result_seeds =
            match super::observe::snapshot_results(&result_words, result_reps, &self.result_layout)
            {
                Ok(seeds) => seeds,
                Err(error @ super::ObservationFailure::Integrity(_)) => {
                    self.machine.set_first_cause(crate::host_fns::bad_pointer());
                    return Err(runtime_error_from_machine_or_observation(
                        &self.machine,
                        error,
                    ));
                }
                Err(error) => return Err(error.into()),
            };
        let values = match super::forcing::observe_results(
            &self.machine,
            self.program,
            &mut self.vmctx,
            &self.static_catalog,
            &self.program.descriptor_registry,
            &self.old_space,
            &result_seeds,
            budget,
            crate::observation::BudgetPolicy::Complete,
        ) {
            Ok(values) => values,
            Err(ExecutionError::Observation(error @ super::ObservationFailure::Integrity(_))) => {
                self.machine.set_first_cause(crate::host_fns::bad_pointer());
                return Err(runtime_error_from_machine_or_observation(
                    &self.machine,
                    error,
                ));
            }
            Err(error) => return Err(error),
        };
        Ok(super::run::RunResult {
            values,
            collections: self
                .machine
                .gc_generation()
                .saturating_sub(self.collections_before),
        })
    }

    /// Collect through the same scoped owner used by entry and forcing.
    pub(super) fn collect(&mut self, reserve: usize) -> Result<(), ExecutionError> {
        if self.machine.prepared_call_status() != CallStatus::Success {
            return Err(runtime_error_from_machine(&self.machine));
        }
        let _scope = OldSpaceScope::new(&self.machine, &self.old_space)?;
        let raw = unsafe { prepared_gc_trigger(&mut self.vmctx, reserve) };
        let status = CallStatus::from_raw(i64::from(raw))
            .map_err(|_| runtime_error(&self.machine, crate::host_fns::bad_pointer()))?;
        if status != CallStatus::Success
            || self.machine.prepared_call_status() != CallStatus::Success
        {
            return Err(runtime_error_for_status(&self.machine, status));
        }
        Ok(())
    }

    /// Promote the managed result at a logical (Void-inclusive) result index
    /// into invocation-owned descriptor old space. The result slot itself is
    /// the registered root storage; no `RootSlot` or raw pointer escapes.
    #[cfg(test)]
    pub(super) fn promote_result(&mut self, logical_index: usize) -> Result<(), ExecutionError> {
        if self.machine.prepared_call_status() != CallStatus::Success {
            return Err(runtime_error_from_machine(&self.machine));
        }
        let Some(rep) = self
            .result_contract
            .returned_reps()
            .and_then(|reps| reps.get(logical_index))
            .copied()
        else {
            return Err(runtime_error(&self.machine, crate::host_fns::bad_pointer()));
        };
        if !matches!(rep, RuntimeRep::LiftedRef | RuntimeRep::UnliftedRef) {
            return Err(runtime_error(&self.machine, crate::host_fns::bad_pointer()));
        }
        let Some(stored) = self
            .result_layout
            .logical_to_stored()
            .get(logical_index)
            .copied()
            .flatten()
        else {
            return Err(runtime_error(&self.machine, crate::host_fns::bad_pointer()));
        };
        let Some(field) = self.result_layout.fields().get(stored as usize) else {
            return Err(runtime_error(&self.machine, crate::host_fns::bad_pointer()));
        };
        let slot = unsafe {
            self.results
                .as_mut_ptr()
                .cast::<u8>()
                .add(field.offset() as usize)
                .cast::<*mut u8>()
        };
        let result = unsafe {
            self.old_space.promote_prepared(
                &self.machine,
                &mut self.vmctx,
                &[slot],
                &self.program.descriptors,
            )
        };
        result.map_err(|cause| runtime_error(&self.machine, cause))
    }
}

impl Drop for PreparedInvocation<'_> {
    fn drop(&mut self) {
        // Native execution has returned before this owner can be dropped.
        // Teardown consults ownership tables only, even after terminal failure.
        self.machine.clear_prepared_old_space();
        self.machine.clear_prepared_entries();
        self.machine.clear_rust_roots();
        self.machine.free_session_heap();
        self.machine.clear_stack_map_registry();
        self.machine.clear_cancel_flag();
        self.vmctx.machine_state = std::ptr::null_mut();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_space_scope_clears_on_error_and_unwind() {
        let machine = MachineState::new();
        let owner = OldSpace::new();

        let error = (|| -> Result<(), ExecutionError> {
            let _scope = OldSpaceScope::new(&machine, &owner)?;
            assert!(unsafe { machine.prepared_old_space() }.is_some());
            Err(runtime_error(&machine, crate::host_fns::bad_pointer()))
        })();
        assert!(error.is_err());
        assert!(unsafe { machine.prepared_old_space() }.is_none());

        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _scope = OldSpaceScope::new(&machine, &owner).unwrap();
            assert!(unsafe { machine.prepared_old_space() }.is_some());
            panic!("scope cleanup");
        }));
        assert!(panic.is_err());
        assert!(unsafe { machine.prepared_old_space() }.is_none());
    }
}
