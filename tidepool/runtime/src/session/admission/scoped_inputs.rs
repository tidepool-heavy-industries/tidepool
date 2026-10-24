//! The live values held by one checked prefix, including exact terminal uses.

use super::super::prepared::{PreparedRuntimeError, RetainedNativeRequirement};
use super::*;
use tidepool_codegen::prepared_program::PreparedHandle;
use tidepool_repr::SessionVarId;
use tidepool_toolchain::checked_cell::{CheckedValueArtifact, ExactCheckedItem, ExactCompiledItem};
use tidepool_toolchain::compiler_world::SelectedDependencyTerminal;
use tidepool_toolchain::compiler_world::{DependencyTerminalOrigin, WorldCellRuntimeInputOrigin};
use tidepool_toolchain::linked_native::{
    CellStageSlotWitness, HistoricalSlotWitness, LinkedNativeTarget, VerifiedRetainedSlotDemander,
};

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

/// A value loan bound to the checked execution and exact receiving native slot.
#[derive(Clone)]
pub(in crate::session) struct AuthorizedRetainedValue {
    item: ExactCheckedItem,
    admission_digest: [u8; 32],
    target: Arc<LinkedNativeTarget>,
    requirement: Arc<RetainedNativeRequirement>,
    value: Arc<ScopedResolvedValue>,
}

impl AuthorizedRetainedValue {
    pub(in crate::session) fn item(&self) -> &ExactCheckedItem {
        &self.item
    }
    pub(in crate::session) fn admission_digest(&self) -> &[u8; 32] {
        &self.admission_digest
    }
    pub(in crate::session) fn target(&self) -> &Arc<LinkedNativeTarget> {
        &self.target
    }
    pub(in crate::session) fn requirement(&self) -> &Arc<RetainedNativeRequirement> {
        &self.requirement
    }
    pub(in crate::session) fn value(&self) -> &Arc<ScopedResolvedValue> {
        &self.value
    }
    pub(in crate::session) fn matches_slot(
        &self,
        expected_item: &ExactCheckedItem,
        expected_admission_digest: [u8; 32],
        target: &Arc<LinkedNativeTarget>,
        requirement: &Arc<RetainedNativeRequirement>,
    ) -> bool {
        self.item == *expected_item
            && self.admission_digest == expected_admission_digest
            && Arc::ptr_eq(&self.target, target)
            && Arc::ptr_eq(&self.requirement, requirement)
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

    pub(in crate::session) fn validate_native_values<'a>(
        &self,
        session: &PersistentSession,
        expected_item: &ExactCheckedItem,
        expected_admission_digest: [u8; 32],
        expected_target: &Arc<LinkedNativeTarget>,
        values: impl Iterator<Item = &'a AuthorizedRetainedValue>,
    ) -> Result<(), SessionError> {
        session.validate_lexical_scope_lease(self.scope.scope(), &self.scope)?;
        let mut values = values.peekable();
        if values.peek().is_none() {
            return Ok(());
        }
        let reachable = session
            .bindings()
            .scope_reachable_binding_ids(session.scope_tree(), self.scope.scope())
            .into_iter()
            .collect();
        for authorized in values {
            if authorized.item() != expected_item
                || *authorized.admission_digest() != expected_admission_digest
                || !Arc::ptr_eq(authorized.target(), expected_target)
            {
                return Err(SessionError::Engine(
                    PreparedRuntimeError::certified_target_owners(),
                ));
            }
            authorized.value().validate(session, &reachable)?;
        }
        Ok(())
    }

    pub(super) fn validate_item(
        &self,
        session: &PersistentSession,
        item: &tidepool_toolchain::checked_cell::ExactCheckedItem,
    ) -> Result<(), SessionError> {
        session.validate_lexical_scope_lease(self.scope.scope(), &self.scope)?;
        let inputs = item.world_selected_runtime_inputs();
        let stage = self
            .stages
            .get(&inputs.stage_first_ordinal())
            .ok_or(super::super::SessionTransitionReason::WrongPhase)?;
        if stage.is_empty() {
            return Ok(());
        }
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

    fn authorize_for_cell_stage(
        &self,
        item: &ExactCheckedItem,
        admission_digest: [u8; 32],
        target: &Arc<LinkedNativeTarget>,
        requirement: &Arc<RetainedNativeRequirement>,
        witness: CellStageSlotWitness<'_>,
        value: &Arc<ScopedResolvedValue>,
    ) -> Result<AuthorizedRetainedValue, PreparedRuntimeError> {
        let stage = item.world_selected_runtime_inputs().stage_first_ordinal();
        let selected = item
            .world_selected_runtime_inputs()
            .inputs()
            .iter()
            .find(|selected| {
                selected.session_var() == value.id
                    && selected.defining_scope() == value.defining_scope.0
                    && selected.generation() == witness.leaf().generation()
                    && *selected.symbol() == *witness.leaf().identity()
            })
            .ok_or_else(PreparedRuntimeError::certified_target_owners)?;
        let row = match (&value.evidence, selected.origin()) {
            (
                ScopedValueEvidence::Admitted { row, .. },
                WorldCellRuntimeInputOrigin::BaseWorld | WorldCellRuntimeInputOrigin::CellOperand,
            ) => {
                let expected = selected
                    .checked_row()
                    .ok_or_else(PreparedRuntimeError::certified_target_owners)?;
                if !Arc::ptr_eq(expected, row) {
                    return Err(PreparedRuntimeError::certified_target_owners());
                }
                row
            }
            (
                ScopedValueEvidence::Produced { row, .. },
                WorldCellRuntimeInputOrigin::PriorStage { producer, consumer },
            ) => {
                if producer >= consumer || consumer > stage {
                    return Err(PreparedRuntimeError::certified_target_owners());
                }
                row
            }
            _ => return Err(PreparedRuntimeError::certified_target_owners()),
        };
        if !witness
            .owner()
            .matches_retained_checked_value(witness.global_index(), witness.binding(), row)
            .map_err(|error| PreparedRuntimeError::NativeSourceEvidence(Box::new(error)))?
        {
            return Err(PreparedRuntimeError::certified_target_owners());
        }
        Ok(AuthorizedRetainedValue {
            item: item.clone(),
            admission_digest,
            target: Arc::clone(target),
            requirement: Arc::clone(requirement),
            value: Arc::clone(value),
        })
    }

    fn authorize_for_historical_home(
        &self,
        item: &ExactCheckedItem,
        admission_digest: [u8; 32],
        target: &Arc<LinkedNativeTarget>,
        requirement: &Arc<RetainedNativeRequirement>,
        witness: HistoricalSlotWitness<'_>,
        value: &Arc<ScopedResolvedValue>,
    ) -> Result<AuthorizedRetainedValue, PreparedRuntimeError> {
        let stage = item.world_selected_runtime_inputs().stage_first_ordinal();
        let matched = self.stages.get(&stage).is_some_and(|entries| {
            entries.iter().any(|selected| {
                selected.terminal.origin() == DependencyTerminalOrigin::NativeOriginal
                    && selected.terminal.demander() == witness.demander()
                    && selected.terminal.global_index() == witness.global_index()
                    && selected.terminal.binding() == witness.binding()
                    && selected.value.id == value.id
                    && selected.value.handle == value.handle
            })
        });
        if !matched {
            return Err(PreparedRuntimeError::certified_target_owners());
        }
        let ScopedValueEvidence::Admitted { row, .. } = &value.evidence else {
            return Err(PreparedRuntimeError::certified_target_owners());
        };
        if !witness
            .owner()
            .matches_retained_checked_value(witness.global_index(), witness.binding(), row)
            .map_err(|error| PreparedRuntimeError::NativeSourceEvidence(Box::new(error)))?
        {
            return Err(PreparedRuntimeError::certified_target_owners());
        }
        Ok(AuthorizedRetainedValue {
            item: item.clone(),
            admission_digest,
            target: Arc::clone(target),
            requirement: Arc::clone(requirement),
            value: Arc::clone(value),
        })
    }

    pub(in crate::session) fn resolve_native_requirement(
        &self,
        prefix: &RuntimeCheckedPrefix,
        execution: &Arc<ExactCompiledItem>,
        target: &Arc<LinkedNativeTarget>,
        requirement: &Arc<RetainedNativeRequirement>,
    ) -> Result<AuthorizedRetainedValue, PreparedRuntimeError> {
        let item = execution.item();
        let admission_digest = prefix.admission().digest();
        if !item.same_cell(&prefix.first_item)
            || item.admission_digest() != admission_digest
            || !execution.shares_target(target.prepared())
        {
            return Err(PreparedRuntimeError::certified_target_owners());
        }
        execution
            .validate_runtime_admission(admission_digest)
            .map_err(|_| PreparedRuntimeError::certified_target_owners())?;
        let value = self
            .value(requirement.identity(), requirement.generation())
            .ok_or_else(|| PreparedRuntimeError::MissingRetainedCertifiedOwner {
                identity: requirement.identity().clone(),
                generation: requirement.generation(),
            })?;
        value
            .provenance()
            .map_err(|_| PreparedRuntimeError::MissingRetainedSource { id: value.id })?;
        let slot = target
            .verify_retained_slot(
                requirement.owner(),
                requirement.global_index(),
                requirement.binding(),
                requirement.leaf(),
            )
            .map_err(|error| PreparedRuntimeError::NativeSourceEvidence(Box::new(error)))?;
        match slot {
            VerifiedRetainedSlotDemander::CurrentStage(witness)
            | VerifiedRetainedSlotDemander::PriorProgramStage(witness) => self
                .authorize_for_cell_stage(
                    item,
                    admission_digest,
                    target,
                    requirement,
                    witness,
                    value,
                ),
            VerifiedRetainedSlotDemander::HistoricalInput(witness) => self
                .authorize_for_historical_home(
                    item,
                    admission_digest,
                    target,
                    requirement,
                    witness,
                    value,
                ),
        }
    }
}

impl RuntimeCheckedPrefix {
    pub(in crate::session) fn scoped_inputs_for(
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
                .stage_first_ordinal();
            if let std::collections::btree_map::Entry::Vacant(entry) = stage_terminals.entry(stage)
            {
                entry.insert(
                    checked
                        .world_stage_dependencies()
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
        let mut owner = ScopedResolvedInputs::empty(scope);
        if !selected_ids.is_empty() {
            let bindings = Arc::new(self.lease_binding_ids(selected_ids.values().copied()));
            for id in selected_ids.into_values() {
                let entry = self
                    .bindings()
                    .get(id)
                    .ok_or(super::super::SessionTransitionReason::RetainedValueUnavailable)?;
                owner.insert(self.resolved_value(entry, Arc::clone(&bindings))?)?;
            }
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
}
