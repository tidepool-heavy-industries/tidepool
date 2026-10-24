#[derive(Debug)]
pub(in crate::session) struct PackageRootObservation {
    /// Pointer receipt only. Compare simultaneous observations; execution may
    /// collect or update the closure, and this number cannot keep it alive.
    pub(in crate::session) pointer: usize,
    pub(in crate::session) evaluated: bool,
}

/// Borrow a CAF from the actual held package group without forcing or
/// installing. The temporary root is always released before returning.
pub(in crate::session) fn observe_package_instance(
    engine: &mut PreparedEngine,
    lease: &tidepool_codegen::prepared_program::NativeInstanceLease,
    value: ValueId,
) -> Result<PackageRootObservation, PreparedRuntimeError> {
    use tidepool_codegen::prepared_program::NativeInstanceLease;
    let NativeInstanceLease::Package(package) = lease else {
        return Err(PreparedRuntimeError::certified_target_owners());
    };
    let program = lease.instance().program();
    let facts = engine
        .programs
        .get(&program)
        .ok_or_else(|| PreparedRuntimeError::Run(ExecutionError::UnknownProgram(program)))?;
    let image = package.image().image();
    let is_caf = package
        .image()
        .group()
        .definitions()
        .bindings()
        .iter()
        .flat_map(|group| match group {
            Group::NonRecursive(top) => std::slice::from_ref(top),
            Group::Recursive(tops) => tops.as_slice(),
        })
        .any(|top| {
            top.binding.id == value
                && matches!(
                    top.binding.rhs,
                    HeapRhs::Thunk {
                        update: UpdatePolicy::Memoize,
                        ..
                    }
                )
        });
    if !is_caf
        || !Arc::ptr_eq(&facts.definitions, image.definition_facts())
        || image.export_contract(lease.value()).is_none_or(|export| {
            export.identity != lease.export()
                || export.entry_signature != lease.entry_signature()
                || export.rep != lease.handle().rep()
        })
    {
        return Err(PreparedRuntimeError::certified_target_owners());
    }
    engine
        .machine
        .handle_is_evaluated(lease.handle())
        .map_err(PreparedRuntimeError::Run)?;
    let temporary = engine
        .machine
        .retain_top(program, value)
        .map_err(PreparedRuntimeError::Run)?;
    let observation = (|| {
        let root = engine
            .machine
            .handle_root(temporary)
            .ok_or(PreparedRuntimeError::Run(
                ExecutionError::UnknownPreparedHandle,
            ))?;
        let evaluated = engine
            .machine
            .handle_is_evaluated(temporary)
            .map_err(PreparedRuntimeError::Run)?;
        Ok(PackageRootObservation {
            pointer: root.current() as usize,
            evaluated,
        })
    })();
    if !engine.machine.release(temporary) {
        return Err(PreparedRuntimeError::Run(
            ExecutionError::UnknownPreparedHandle,
        ));
    }
    observation
}
