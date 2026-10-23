//! The live values held by one checked prefix, including exact terminal uses.

use super::*;
use tidepool_codegen::prepared_program::PreparedHandle;
use tidepool_repr::SessionVarId;
use tidepool_toolchain::checked_cell::CheckedValueArtifact;
use tidepool_toolchain::compiler_world::SelectedDependencyTerminal;

#[derive(Debug)]
enum ScopedValueEvidence {
    Admitted {
        row: Arc<CheckedValueArtifact>,
        provenance: Arc<super::super::resident::ProgramProvenance>,
    },
    Produced {
        row: Arc<CheckedValueArtifact>,
        provenance: Arc<super::super::resident::ProgramProvenance>,
    },
    /// Missing metadata is only an observation. It cannot satisfy a checked
    /// dependency terminal or grant retained native execution.
    WithoutProgramProvenance,
}

#[derive(Debug)]
pub(in crate::session) struct ScopedResolvedValue {
    pub(in crate::session) id: SessionVarId,
    defining_scope: ScopeId,
    pub(in crate::session) identity: SymbolIdentity,
    pub(in crate::session) generation: u64,
    pub(in crate::session) handle: PreparedHandle,
    name: String,
    evidence: ScopedValueEvidence,
    _bindings: Arc<super::super::resident::BindingLease>,
}

impl ScopedResolvedValue {
    fn is_produced(&self) -> bool {
        matches!(self.evidence, ScopedValueEvidence::Produced { .. })
    }

    pub(in crate::session) fn provenance(
        &self,
    ) -> Result<&Arc<super::super::resident::ProgramProvenance>, SessionError> {
        match &self.evidence {
            ScopedValueEvidence::Admitted { provenance, .. }
            | ScopedValueEvidence::Produced { provenance, .. } => Ok(provenance),
            ScopedValueEvidence::WithoutProgramProvenance => {
                Err(super::super::SessionTransitionReason::RetainedValueUnavailable.into())
            }
        }
    }

    fn checked_row(&self) -> Result<&Arc<CheckedValueArtifact>, SessionError> {
        match &self.evidence {
            ScopedValueEvidence::Admitted { row, .. }
            | ScopedValueEvidence::Produced { row, .. } => Ok(row),
            ScopedValueEvidence::WithoutProgramProvenance => {
                Err(super::super::SessionTransitionReason::RetainedValueUnavailable.into())
            }
        }
    }

    fn validate(
        &self,
        session: &PersistentSession,
        reachable: &std::collections::HashSet<SessionVarId>,
    ) -> Result<(), SessionError> {
        let entry = session
            .bindings()
            .get(self.id)
            .ok_or(super::super::SessionTransitionReason::RetainedValueUnavailable)?;
        if !reachable.contains(&self.id) || entry.scope != self.defining_scope {
            return Err(super::super::SessionTransitionReason::ScopeRevoked.into());
        }
        if entry.value.identity != self.identity
            || entry.module != tidepool_repr::SessionModule::val(Generation(self.generation))
            || entry.value.handle != self.handle
            || session
                .prepared()
                .and_then(|engine| engine.prepared_handle_of(self.handle.raw()))
                != Some(self.handle)
        {
            return Err(super::super::SessionTransitionReason::PreparedValueChanged.into());
        }
        if let ScopedValueEvidence::Admitted { row, provenance }
        | ScopedValueEvidence::Produced { row, provenance } = &self.evidence
        {
            if session
                .retained_checked_value_artifact(entry.module)
                .is_none_or(|held| !Arc::ptr_eq(held, row))
                || session
                    .binding_provenance(self.id.raw())
                    .is_none_or(|held| !Arc::ptr_eq(held, provenance))
            {
                return Err(super::super::SessionTransitionReason::PreparedValueChanged.into());
            }
        }
        Ok(())
    }
}

#[derive(Debug)]
struct ScopedTerminalUse {
    terminal: SelectedDependencyTerminal,
    value: Arc<ScopedResolvedValue>,
}

/// The sole persistent runtime value owner for a checked prefix. The map is
/// both the import projection and the lookup index; stage uses borrow its
/// actual immutable values instead of maintaining another identity ledger.
#[derive(Clone, Debug)]
pub(in crate::session) struct ScopedResolvedInputs {
    scope: Arc<RuntimeLexicalScopeLease>,
    values: imbl::OrdMap<(SymbolIdentity, u64), Arc<ScopedResolvedValue>>,
    stages: Arc<BTreeMap<usize, Vec<ScopedTerminalUse>>>,
    pub(super) commitment: [u8; 32],
}

impl ScopedResolvedInputs {
    fn empty(scope: Arc<RuntimeLexicalScopeLease>) -> Self {
        Self {
            scope,
            values: Default::default(),
            stages: Arc::new(BTreeMap::new()),
            commitment: *blake3::hash(b"TidepoolScopedResolvedInputs1").as_bytes(),
        }
    }

    pub(super) fn scope(&self) -> &Arc<RuntimeLexicalScopeLease> {
        &self.scope
    }

    pub(super) fn imports(&self) -> impl Iterator<Item = (&SymbolIdentity, u64)> {
        self.values
            .values()
            .map(|value| (&value.identity, value.generation))
    }

    pub(super) fn admitted_imports(&self) -> impl Iterator<Item = (&SymbolIdentity, u64)> {
        self.values
            .values()
            .filter(|value| !value.is_produced())
            .map(|value| (&value.identity, value.generation))
    }

    pub(super) fn settled_bindings(
        &self,
    ) -> impl Iterator<Item = (&str, &SymbolIdentity, u64, u64)> {
        self.values
            .values()
            .filter(|value| value.is_produced())
            .map(|value| {
                (
                    value.name.as_str(),
                    &value.identity,
                    value.generation,
                    value.id.raw(),
                )
            })
    }

    fn insert(&mut self, value: Arc<ScopedResolvedValue>) -> Result<(), SessionError> {
        let key = (value.identity.clone(), value.generation);
        if let Some(held) = self.values.get(&key) {
            if held.id != value.id
                || held.handle != value.handle
                || held.defining_scope != value.defining_scope
            {
                return Err(super::super::SessionTransitionReason::PreparedValueChanged.into());
            }
            return Ok(());
        }
        let mut hash = blake3::Hasher::new();
        hash.update(b"TidepoolScopedResolvedValue1");
        hash.update(&self.commitment);
        frame_native_identity(&mut hash, &value.identity);
        hash.update(&value.generation.to_le_bytes());
        hash.update(&value.defining_scope.0.to_le_bytes());
        hash.update(&(value.name.len() as u64).to_le_bytes());
        hash.update(value.name.as_bytes());
        hash.update(&value.id.raw().to_le_bytes());
        hash.update(&value.handle.raw().0.to_le_bytes());
        hash.update(&[u8::from(value.is_produced())]);
        self.commitment = *hash.finalize().as_bytes();
        self.values.insert(key, value);
        Ok(())
    }

    pub(super) fn with_captured(
        &self,
        scope: Arc<RuntimeLexicalScopeLease>,
        captured: &super::super::resident::CapturedCheckedValueSet,
    ) -> Result<Self, SessionError> {
        let mut next = self.clone();
        next.scope = scope;
        if !captured.values().is_empty() {
            let row = captured
                .row()
                .ok_or(super::super::SessionTransitionReason::PreparedValueChanged)?;
            for value in captured.values() {
                next.insert(Arc::new(ScopedResolvedValue {
                    id: value.id(),
                    defining_scope: value.defining_scope(),
                    identity: value.identity().clone(),
                    generation: value.generation().0,
                    handle: value.handle(),
                    name: value.name().to_owned(),
                    _bindings: Arc::clone(value.binding_lease()),
                    evidence: ScopedValueEvidence::Produced {
                        row: Arc::clone(row),
                        provenance: Arc::clone(captured.provenance()),
                    },
                }))?;
            }
        }
        Ok(next)
    }

    pub(super) fn with_scope(&self, scope: Arc<RuntimeLexicalScopeLease>) -> Self {
        Self {
            scope,
            ..self.clone()
        }
    }

    pub(in crate::session) fn validate_live(
        &self,
        session: &PersistentSession,
    ) -> Result<(), SessionError> {
        session.validate_lexical_scope_lease(self.scope.scope(), &self.scope)?;
        let reachable = session
            .bindings()
            .scope_reachable_binding_ids(session.scope_tree(), self.scope.scope())
            .into_iter()
            .collect();
        for value in self.values.values() {
            value.validate(session, &reachable)?;
        }
        Ok(())
    }

    pub(super) fn validate_item(
        &self,
        session: &PersistentSession,
        item: &tidepool_toolchain::checked_cell::ExactCheckedItem,
    ) -> Result<(), SessionError> {
        session.validate_lexical_scope_lease(self.scope.scope(), &self.scope)?;
        let Some(inputs) = item.world_selected_runtime_inputs() else {
            return Ok(());
        };
        let stage = self
            .stages
            .get(&inputs.stage_first_ordinal())
            .ok_or(super::super::SessionTransitionReason::WrongPhase)?;
        let reachable = session
            .bindings()
            .scope_reachable_binding_ids(session.scope_tree(), self.scope.scope())
            .into_iter()
            .collect();
        for selected in stage {
            selected.value.validate(session, &reachable)?;
        }
        Ok(())
    }

    pub(in crate::session) fn value(
        &self,
        identity: &SymbolIdentity,
        generation: u64,
    ) -> Option<&Arc<ScopedResolvedValue>> {
        self.values.get(&(identity.clone(), generation))
    }

    pub(in crate::session) fn resolve_native_requirement(
        &self,
        item: &tidepool_toolchain::checked_cell::ExactCheckedItem,
        requirement: &super::super::prepared::RetainedNativeRequirement,
    ) -> Result<Arc<ScopedResolvedValue>, super::super::prepared::PreparedRuntimeError> {
        use super::super::prepared::{PreparedRuntimeError, RetainedNativeOrigin};
        use tidepool_toolchain::compiler_world::DependencyTerminalOrigin;
        use tidepool_toolchain::linked_native::NativeOccurrenceId;
        let value = self
            .value(requirement.identity(), requirement.generation())
            .ok_or_else(|| PreparedRuntimeError::MissingRetainedCertifiedOwner {
                identity: requirement.identity().clone(),
                generation: requirement.generation(),
            })?;
        value
            .provenance()
            .map_err(|_| PreparedRuntimeError::MissingRetainedSource { id: value.id })?;
        match requirement.origin() {
            RetainedNativeOrigin::Target { global_index } => {
                // The actual target owns its ordered direct globals. This
                // branch never invents an OriginalUse for a fresh cell input.
                let _ = global_index;
            }
            RetainedNativeOrigin::Acquired {
                owner,
                global_index,
                binding,
            } => {
                let NativeOccurrenceId::Home(demander) = owner.occurrence() else {
                    return Err(PreparedRuntimeError::CertifiedTargetOwners);
                };
                let stage = item
                    .world_selected_runtime_inputs()
                    .ok_or(PreparedRuntimeError::CertifiedTargetOwners)?
                    .stage_first_ordinal();
                let uses = self
                    .stages
                    .get(&stage)
                    .ok_or(PreparedRuntimeError::CertifiedTargetOwners)?;
                let mut matched = false;
                for selected in uses.iter().filter(|selected| {
                    selected.terminal.origin() == DependencyTerminalOrigin::NativeOriginal
                        && selected.terminal.demander() == demander
                        && selected.terminal.global_index() == *global_index
                        && selected.terminal.binding() == *binding
                }) {
                    matched = true;
                    if selected.value.id != value.id || selected.value.handle != value.handle {
                        return Err(PreparedRuntimeError::CertifiedTargetOwners);
                    }
                }
                if !matched && !value.is_produced() {
                    return Err(PreparedRuntimeError::CertifiedTargetOwners);
                }
            }
        }
        Ok(Arc::clone(value))
    }
}

impl RuntimeCheckedPrefix {
    pub(super) fn scoped_inputs_for(
        &self,
        session: &PersistentSession,
        execution: &tidepool_toolchain::checked_cell::ExactCompiledItem,
    ) -> Result<ScopedResolvedInputs, SessionError> {
        if !self.admission.belongs_to(session) {
            return Err(super::super::SessionTransitionReason::OwnerChanged.into());
        }
        let state = self.state.lock();
        if !execution.item().same_cell(&self.first_item)
            || execution.item().index() != state.snapshot().compiler_prefix.next_item()
        {
            return Err(super::super::SessionTransitionReason::WrongPhase.into());
        }
        state
            .snapshot()
            .scoped_inputs
            .validate_item(session, execution.item())?;
        Ok(state.snapshot().scoped_inputs.clone())
    }
}

impl PersistentSession {
    fn resolved_value(
        &self,
        entry: &tidepool_codegen::binding_table::BindingEntry,
        bindings: Arc<super::super::resident::BindingLease>,
    ) -> Result<Arc<ScopedResolvedValue>, SessionError> {
        if self
            .prepared()
            .and_then(|engine| engine.prepared_handle_of(entry.value.handle.raw()))
            != Some(entry.value.handle)
        {
            return Err(super::super::SessionTransitionReason::PreparedValueChanged.into());
        }
        let evidence = match self.binding_runtime_origin(entry.id) {
            Some(super::super::persistent::BindingRuntimeOrigin::Program(provenance)) => {
                let row = self
                    .retained_checked_value_artifact(entry.module)
                    .ok_or(SessionError::MissingRetainedValueInterface(entry.module))?;
                ScopedValueEvidence::Admitted {
                    row: Arc::clone(row),
                    provenance: Arc::clone(provenance),
                }
            }
            Some(super::super::persistent::BindingRuntimeOrigin::WithoutProgramProvenance) => {
                ScopedValueEvidence::WithoutProgramProvenance
            }
            None => {
                return Err(super::super::SessionTransitionReason::RetainedValueUnavailable.into())
            }
        };
        Ok(Arc::new(ScopedResolvedValue {
            id: entry.id,
            defining_scope: entry.scope,
            identity: entry.value.identity.clone(),
            generation: entry.module.gen.0,
            handle: entry.value.handle,
            name: entry.name.0.clone(),
            evidence,
            _bindings: bindings,
        }))
    }

    pub(super) fn bind_world_scoped_inputs(
        &mut self,
        program: &tidepool_toolchain::checked_cell::CellProgram,
        scope: Arc<RuntimeLexicalScopeLease>,
    ) -> Result<ScopedResolvedInputs, SessionError> {
        use tidepool_toolchain::checked_cell::CheckedPlannedCellSlot as Slot;
        self.validate_lexical_scope_lease(scope.scope(), &scope)?;
        let mut stage_terminals = BTreeMap::new();
        for item in program.items() {
            let checked = item.checked_item();
            let stage = checked
                .world_selected_runtime_inputs()
                .ok_or(super::super::SessionTransitionReason::WrongPhase)?
                .stage_first_ordinal();
            if let std::collections::btree_map::Entry::Vacant(entry) = stage_terminals.entry(stage)
            {
                entry.insert(
                    checked
                        .world_stage_dependencies()
                        .ok_or(super::super::SessionTransitionReason::WrongPhase)?
                        .terminals()
                        .collect::<Vec<_>>(),
                );
            }
        }
        let own_values = program
            .slots()
            .iter()
            .filter_map(|slot| match slot {
                Slot::Bind { value } => Some(*value),
                Slot::Expression { capture, .. } => Some(*capture),
                _ => None,
            })
            .collect::<std::collections::BTreeSet<_>>();
        let scoped = self.scoped_prepared_bindings_in(scope.scope());
        let mut selected_ids = BTreeMap::new();
        for (identity, generation) in stage_terminals
            .values()
            .flatten()
            .map(|terminal| (terminal.identity(), terminal.generation()))
            .chain(
                program
                    .items()
                    .iter()
                    .filter_map(|item| item.native())
                    .flat_map(|native| native.required_native_imports())
                    .filter(|(identity, generation)| {
                        !(own_values.contains(generation)
                            && identity.unit == "main"
                            && identity.module
                                == tidepool_repr::SessionModule::val(Generation(*generation))
                                    .module_name())
                    }),
            )
        {
            let entry = scoped
                .get(&(
                    identity,
                    tidepool_repr::SessionModule::val(Generation(generation)),
                ))
                .ok_or(super::super::SessionTransitionReason::RetainedValueUnavailable)?;
            selected_ids.insert((identity.clone(), generation), entry.id);
        }
        drop(scoped);
        // This is the existing binding owner's real dependency custody, issued
        // before a compiler owner can outlive the mutable session checkout.
        let bindings = Arc::new(self.lease_binding_ids(selected_ids.values().copied()));
        let mut owner = ScopedResolvedInputs::empty(scope);
        for id in selected_ids.into_values() {
            let entry = self
                .bindings()
                .get(id)
                .ok_or(super::super::SessionTransitionReason::RetainedValueUnavailable)?;
            owner.insert(self.resolved_value(entry, Arc::clone(&bindings))?)?;
        }
        let mut stages = BTreeMap::new();
        for (stage, terminals) in stage_terminals {
            let mut uses = Vec::with_capacity(terminals.len());
            for terminal in terminals {
                let value = Arc::clone(
                    owner
                        .value(terminal.identity(), terminal.generation())
                        .ok_or(super::super::SessionTransitionReason::RetainedValueUnavailable)?,
                );
                if !terminal.matches_checked_value(value.checked_row()?)? {
                    return Err(super::super::SessionTransitionReason::PreparedValueChanged.into());
                }
                uses.push(ScopedTerminalUse { terminal, value });
            }
            stages.insert(stage, uses);
        }
        owner.stages = Arc::new(stages);
        Ok(owner)
    }

    pub(super) fn capture_admitted_scoped_inputs(
        &mut self,
        scope: Arc<RuntimeLexicalScopeLease>,
    ) -> Result<ScopedResolvedInputs, SessionError> {
        self.validate_lexical_scope_lease(scope.scope(), &scope)?;
        let ids = self
            .scoped_prepared_bindings_in(scope.scope())
            .into_values()
            .map(|entry| entry.id)
            .collect::<Vec<_>>();
        let bindings = Arc::new(self.lease_binding_ids(ids.iter().copied()));
        let mut owner = ScopedResolvedInputs::empty(scope);
        for id in ids {
            let entry = self
                .bindings()
                .get(id)
                .ok_or(super::super::SessionTransitionReason::RetainedValueUnavailable)?;
            owner.insert(self.resolved_value(entry, Arc::clone(&bindings))?)?;
        }
        Ok(owner)
    }
}
