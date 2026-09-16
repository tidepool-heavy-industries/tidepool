//! Checked binder and closure layout facts used by every emitter consumer.

use super::roots::RootWords;
use super::static_bytes::PinnedBytes;
use super::CompileError;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use tidepool_heap::execution_descriptor::{EntryMetadata, ObjectDescriptor, ObjectKind};
use tidepool_repr::execution_schema::{
    AlternativePattern, Atom, CaseKind, ConstructorDecl, ExprFrame, Group, HeapBinding, HeapRhs,
    PreparedProgram, ResultContract, RuntimeRep, ScalarLiteral, Signature, StorageLayout,
    SymbolIdentity, ValueId, ValueRef,
};

pub(super) struct FunctionPlan<'a> {
    pub signature: &'a Signature,
    pub parameters: &'a [ValueId],
    pub captures: &'a [ValueRef],
    pub body: usize,
    pub descriptor: Arc<ObjectDescriptor>,
}

/// A thunk has the same captured environment layout as a function, but its
/// generated body is entered by the lazy state machine. Both ordinary lazy
/// references and terminal (NoSuccess) thunks retain their checked contract.
pub(super) struct ThunkPlan<'a> {
    pub signature: &'a Signature,
    pub captures: &'a [ValueRef],
    pub body: usize,
    pub policy: tidepool_repr::execution_schema::UpdatePolicy,
    pub descriptor: Arc<ObjectDescriptor>,
}

/// Invocation-owned top allocation recipe. The checked binding is cloned so
/// run initialization can reserve the complete group after static addresses
/// exist, without retaining the input artifact behind the compiled program.
pub(crate) struct HeapTopSpec {
    pub id: ValueId,
    pub descriptor: Arc<ObjectDescriptor>,
    pub binding: HeapBinding,
    pub reps: Vec<RuntimeRep>,
}

/// One admitted import's machine-wide top-table slot. Imports occupy the
/// contiguous range immediately after this program's own top slots (see
/// [`ProgramPlan::top_slots`]), so `PreparedMachine::install_program`'s
/// contiguity check spans both without distinguishing them. `rep` and
/// `required_evaluated` are copied from the checked [`GlobalDecl`] --
/// identity/signature/generation agreement was already proven by
/// `tidepool_repr::execution_schema::link_program` before this program
/// reached [`admission`](super::admission); only the runtime shape (handle
/// representation, evaluatedness) remains for the installing machine to
/// re-verify against the live handle it is actually given.
pub(crate) struct ImportSlot {
    pub identity: SymbolIdentity,
    pub slot: usize,
    pub rep: RuntimeRep,
    pub required_evaluated: bool,
}

/// What a `Call` site can know about its callee at compile time. Computed
/// ONCE per call site ([`callee`]) and consumed by both admission
/// ([`super::admission`]) and emission (`emit::emit_exact_call`), so the
/// two can never disagree about which calls this program accepts. Every
/// call is lowered through the demanded signature's dispatcher; the
/// classification only decides what can be refuted statically.
#[derive(Clone, Copy, Debug)]
pub(super) enum Callee<'a> {
    /// A function or thunk this program itself declares: its signature is
    /// authoritative, so an application `classify` cannot serve is refuted
    /// at admission.
    Known { signature: &'a Signature },
    /// An import. `entry` is the declared `GlobalDecl::entry_signature`,
    /// already proven equal to the exporter's real signature by
    /// `link_program`; `None` when the projection had no entry information
    /// for it, which leaves the decision to the runtime resolver.
    Import { entry: Option<&'a Signature> },
    /// A parameter, case binder or let-bound value whose descriptor is only
    /// known at run time. Under cross-program dispatch it may be ANY
    /// installed program's callable, so nothing about this program's own
    /// callables can refute the call: only an unlowerable demand can.
    Dynamic,
}

impl<'a> Callee<'a> {
    /// Whether a call of `demand` on this callee is admitted. `Known` and a
    /// signature-carrying `Import` are refuted exactly when
    /// [`super::apply::classify`] cannot split the application; `Dynamic`
    /// and an entry-less `Import` are admitted whenever `demand` itself has
    /// a native lowering (the dispatcher for it exists), because the machine
    /// resolves the actual callee at run time and reports a miss as the
    /// typed, reusable `UnresolvedCallee`. Rejecting such a call for lack of
    /// a LOCALLY shaped function was the closed-world defect the direct
    /// import-call gap and T2's coincidental admission both came from.
    pub(super) fn admits(
        &self,
        profile: &crate::entry_abi::NativeAbiProfile,
        demand: &Signature,
    ) -> bool {
        match self {
            Callee::Known { signature }
            | Callee::Import {
                entry: Some(signature),
            } => super::apply::classify(signature, 0, demand).is_some(),
            Callee::Import { entry: None } | Callee::Dynamic => {
                crate::entry_abi::EntryAbi::lower_internal(
                    profile,
                    demand,
                    crate::entry_abi::EnvironmentMode::Captured,
                )
                .is_ok()
            }
        }
    }
}

/// Classify a call site's callee atom. `known` answers the signature of a
/// function or thunk THIS program declares (admission and the plan build
/// that map differently, so it is passed in). `None` for a non-reference
/// atom (a literal, `Void`, `Rubbish`), which can never be applied.
pub(super) fn callee<'a>(
    program: &'a PreparedProgram,
    known: &dyn Fn(ValueId) -> Option<&'a Signature>,
    atom: &Atom,
) -> Option<Callee<'a>> {
    match atom {
        Atom::Ref(ValueRef::Local(id)) => Some(match known(*id) {
            Some(signature) => Callee::Known { signature },
            None => Callee::Dynamic,
        }),
        Atom::Ref(ValueRef::Global(id)) => {
            let declaration = program.globals().get(id.0 as usize)?;
            let entry = declaration
                .entry_signature
                .and_then(|id| program.signatures().get(id.0 as usize));
            Some(Callee::Import { entry })
        }
        Atom::Scalar(_) | Atom::Void | Atom::Rubbish(_) => None,
    }
}

/// Finite concrete results served by representation-polymorphic entries.
/// Lifted results are always offered to independently compiled consumers.
pub(super) fn result_instances(program: &PreparedProgram) -> BTreeSet<ResultContract> {
    program
        .signatures()
        .iter()
        .filter_map(|signature| match &signature.results {
            results @ ResultContract::Returns(_) => Some(results.clone()),
            _ => None,
        })
        .chain([ResultContract::Returns(vec![RuntimeRep::LiftedRef])])
        .collect()
}

pub(super) struct ProgramPlan<'a> {
    pub program: &'a PreparedProgram,
    pub value_reps: BTreeMap<ValueId, RuntimeRep>,
    pub functions: BTreeMap<ValueId, FunctionPlan<'a>>,
    pub thunks: BTreeMap<ValueId, ThunkPlan<'a>>,
    pub top_bindings: BTreeMap<ValueId, &'a HeapBinding>,
    pub constructors: Vec<Arc<ObjectDescriptor>>,
    /// Each declaration with the descriptor it compiled against (interned or
    /// fresh), handed to the installing machine.
    pub interned_constructors: Vec<(ConstructorDecl, Arc<ObjectDescriptor>)>,
    pub boxed_array: Arc<ObjectDescriptor>,
    /// Fixed one-slot mutable cells share the boxed payload ledger, not array
    /// identity. Hosts must authenticate this distinct descriptor before access.
    pub mut_var: Arc<ObjectDescriptor>,
    pub bytes_array: Arc<ObjectDescriptor>,
    /// The three above, as the machine-shared set the installing machine
    /// adopts or checks (see [`super::interner::ExternalDescriptors`]).
    pub externals: super::interner::ExternalDescriptors,
    /// Compact block-local slots, not ValueId-indexed allocation controlled
    /// by wire IDs.
    pub top_slots: BTreeMap<ValueId, usize>,
    /// Admitted imports' slots, indexed by `GlobalId` (dense from 0, matching
    /// `program.globals()`'s own order) -- `emit.rs`'s `ValueRef::Global(id)`
    /// lowering reads `import_slots[id.0]`. Occupies the block range
    /// immediately after `top_slots`.
    pub import_slots: Vec<ImportSlot>,
    /// This program's own fixed-address root block: one collector-updated
    /// word per top and import slot. Generated code embeds its address, so
    /// the block is allocated before emission and lives exactly as long as
    /// the code that names it.
    pub root_block: RootWords,
    /// Pinned literal payloads; emitters never embed a borrowed artifact buffer.
    pub bytes: Arc<PinnedBytes>,
    pub heap_tops: BTreeSet<ValueId>,
    pub heap_top_specs: Vec<HeapTopSpec>,
    /// Flattened pending argument layouts owned by the application emitter.
    /// Keys use the original function and logical (Void-inclusive) prefix
    /// length, so dispatch never infers storage slots from physical arity.
    pub pap_layouts: BTreeMap<(ValueId, usize), super::apply::PapLayout>,
}

impl<'a> ProgramPlan<'a> {
    /// Slots are block-local: every program owns its own root block, so no
    /// machine-wide base exists and two programs never share a slot range.
    pub fn new(
        program: &'a PreparedProgram,
        interner: &mut super::DescriptorInterner,
    ) -> Result<Self, CompileError> {
        // wave4:LAYOUT_PLAN — collect top/local RHS types, function and join
        // parameter reps, case binder/alternative reps from checked signatures
        // and ConstructorDecl. Multi-component case binder is non-value Void.
        // Use authoritative captures, not a second free-variable analysis.
        // Only after all IDs have reps, construct pinned function layouts from
        // capture reps and constructor layouts from their owning declarations.
        let mut values = BTreeMap::new();
        let mut top_bindings = BTreeMap::new();
        let mut top_slots = BTreeMap::new();
        let mut bytes = BTreeMap::new();

        for group in program.bindings() {
            for top in group_items(group) {
                top_bindings.insert(top.binding.id, &top.binding);
                let slot = top_slots.len();
                top_slots.insert(top.binding.id, slot);
                binding_rep(program, &top.binding, &mut values);
                collect_binding_literals(&top.binding, &mut bytes);
            }
        }

        for frame in &program.expressions().nodes {
            match frame {
                ExprFrame::Return(atoms)
                | ExprFrame::Jump {
                    arguments: atoms, ..
                } => {
                    collect_atoms(atoms, &mut bytes);
                }
                ExprFrame::Enter { callee, .. } => collect_atom(callee, &mut bytes),
                ExprFrame::Call {
                    callee, arguments, ..
                } => {
                    collect_atom(callee, &mut bytes);
                    collect_atoms(arguments, &mut bytes);
                }
                ExprFrame::Operation { arguments, .. }
                | ExprFrame::Construct {
                    fields: arguments, ..
                } => collect_atoms(arguments, &mut bytes),
                ExprFrame::Case {
                    binder,
                    scrutinee_results,
                    kind,
                    alternatives,
                    ..
                } => {
                    let scrutinee_reps = scrutinee_results.returned_reps().unwrap_or(&[]);
                    let binder_rep = if *kind == CaseKind::MultiValue {
                        Some(RuntimeRep::Void)
                    } else if let [rep] = scrutinee_reps {
                        Some(*rep)
                    } else {
                        None
                    };
                    if let Some(binder_rep) = binder_rep {
                        values.insert(*binder, binder_rep);
                    }
                    for alternative in alternatives {
                        if let AlternativePattern::Literal(literal) = &alternative.pattern {
                            collect_literal(literal, &mut bytes);
                        }
                        let reps: &[RuntimeRep] = match (&alternative.pattern, kind) {
                            (_, CaseKind::MultiValue) => scrutinee_reps,
                            (AlternativePattern::Constructor(id), _) => {
                                &program.constructors()[id.0 as usize].field_reps
                            }
                            _ => &[],
                        };
                        for (id, rep) in alternative.binders.iter().zip(reps) {
                            values.insert(*id, *rep);
                        }
                    }
                }
                ExprFrame::Let { bindings, .. } => {
                    for binding in group_items(bindings) {
                        binding_rep(program, binding, &mut values);
                        collect_binding_literals(binding, &mut bytes);
                    }
                }
                ExprFrame::LetJoins { bindings, .. } => {
                    for binding in group_items(bindings) {
                        let signature = signature(program, binding.signature);
                        for (id, rep) in binding.parameters.iter().zip(&signature.arguments) {
                            values.insert(*id, *rep);
                        }
                    }
                }
            }
        }

        let target = &program.envelope().target;
        let mut constructors = Vec::with_capacity(program.constructors().len());
        let mut interned_constructors = Vec::with_capacity(program.constructors().len());
        for declaration in program.constructors() {
            let descriptor = interner.intern(target, declaration)?;
            interned_constructors.push((declaration.clone(), Arc::clone(&descriptor)));
            constructors.push(descriptor);
        }

        let bindings = all_bindings(program);
        for binding in &bindings {
            if let HeapRhs::Function {
                signature: signature_id,
                parameters,
                ..
            } = &binding.rhs
            {
                let signature = signature(program, *signature_id);
                for (id, rep) in parameters.iter().zip(&signature.arguments) {
                    values.insert(*id, *rep);
                }
            }
        }

        let mut functions = BTreeMap::new();
        let mut thunks = BTreeMap::new();
        for binding in bindings {
            match &binding.rhs {
                HeapRhs::Function {
                    signature: signature_id,
                    parameters,
                    captures,
                    body,
                } => {
                    let signature = signature(program, *signature_id);
                    let capture_reps = captures
                        .iter()
                        .map(|capture| value_ref_rep(program, &values, capture))
                        .collect::<Result<Vec<_>, _>>()?;
                    let layout = StorageLayout::for_reps(target, &capture_reps)?;
                    let descriptor = Arc::new(ObjectDescriptor::new(
                        ObjectKind::Function,
                        layout,
                        Some(EntryMetadata::new(
                            signature.clone(),
                            u64::from(binding.id.0),
                        )),
                    )?);
                    functions.insert(
                        binding.id,
                        FunctionPlan {
                            signature,
                            parameters,
                            captures,
                            body: *body,
                            descriptor,
                        },
                    );
                }
                HeapRhs::Thunk {
                    signature: signature_id,
                    update,
                    captures,
                    body,
                } => {
                    let signature = signature(program, *signature_id);
                    let valid_result = matches!(
                        &signature.results,
                        ResultContract::Returns(reps) if reps.as_slice() == [RuntimeRep::LiftedRef]
                    ) || signature.results == ResultContract::NoSuccess;
                    if !signature.arguments.is_empty() || !valid_result {
                        return Err(CompileError::Unsupported(
                            super::Unsupported::ThunkSignature(binding.id),
                        ));
                    }
                    let capture_reps = captures
                        .iter()
                        .map(|capture| value_ref_rep(program, &values, capture))
                        .collect::<Result<Vec<_>, _>>()?;
                    let layout = StorageLayout::for_reps(target, &capture_reps)?;
                    let descriptor = Arc::new(ObjectDescriptor::new(
                        ObjectKind::Thunk,
                        layout,
                        Some(EntryMetadata::new(
                            signature.clone(),
                            u64::from(binding.id.0),
                        )),
                    )?);
                    thunks.insert(
                        binding.id,
                        ThunkPlan {
                            signature,
                            captures,
                            body: *body,
                            policy: *update,
                            descriptor,
                        },
                    );
                }
                _ => {}
            }
        }

        // Imports occupy the contiguous range immediately after this
        // program's own top slots. Admission already proved every declared
        // global's representation is supported, so no rep check is repeated
        // here -- this pass only assigns slots in `GlobalId` order.
        let import_slots = program
            .globals()
            .iter()
            .enumerate()
            .map(|(index, declaration)| ImportSlot {
                identity: declaration.identity.clone(),
                slot: top_slots.len() + index,
                rep: declaration.rep,
                required_evaluated: declaration.required_evaluated,
            })
            .collect::<Vec<_>>();
        let root_block = RootWords::new(top_slots.len() + import_slots.len())
            .map_err(|_| CompileError::RootBlock)?;

        let heap_tops = super::image::heap_top_partition(&top_bindings);
        let pap_layouts = super::apply::layouts(
            target,
            functions
                .iter()
                .map(|(&id, function)| (id, function.signature)),
        )?;
        let mut heap_top_specs = Vec::new();
        for (&id, binding) in &top_bindings {
            if !heap_tops.contains(&id) {
                continue;
            }
            let descriptor = match &binding.rhs {
                HeapRhs::Function { .. } => functions
                    .get(&id)
                    .map(|function| Arc::clone(&function.descriptor)),
                HeapRhs::Thunk { .. } => thunks.get(&id).map(|thunk| Arc::clone(&thunk.descriptor)),
                HeapRhs::Constructor { constructor, .. } => {
                    constructors.get(constructor.0 as usize).cloned()
                }
                HeapRhs::Bytes(_) => None,
            }
            .ok_or(CompileError::MissingRepresentation(id))?;
            let reps = match &binding.rhs {
                HeapRhs::Constructor { constructor, .. } => program.constructors()
                    [constructor.0 as usize]
                    .field_reps
                    .clone(),
                HeapRhs::Function { captures, .. } | HeapRhs::Thunk { captures, .. } => captures
                    .iter()
                    .map(|capture| value_ref_rep(program, &values, capture))
                    .collect::<Result<Vec<_>, _>>()?,
                HeapRhs::Bytes(_) => Vec::new(),
            };
            heap_top_specs.push(HeapTopSpec {
                id,
                descriptor,
                binding: (*binding).clone(),
                reps,
            });
        }

        let externals = interner.externals(target)?;
        Ok(Self {
            program,
            functions,
            thunks,
            value_reps: values,
            top_bindings,
            constructors,
            boxed_array: Arc::clone(&externals.boxed_array),
            bytes_array: Arc::clone(&externals.bytes_array),
            mut_var: Arc::clone(&externals.mut_var),
            externals,
            top_slots,
            import_slots,
            root_block,
            interned_constructors,
            bytes: Arc::new(PinnedBytes::new(bytes)),
            heap_tops,
            heap_top_specs,
            pap_layouts,
        })
    }
}

impl<'a> ProgramPlan<'a> {
    /// [`callee`] against this plan's own function/thunk tables.
    pub(super) fn callee(&self, atom: &Atom) -> Option<Callee<'a>> {
        callee(
            self.program,
            &|id| {
                self.functions
                    .get(&id)
                    .map(|function| function.signature)
                    .or_else(|| self.thunks.get(&id).map(|thunk| thunk.signature))
            },
            atom,
        )
    }
}

fn group_items<T>(group: &Group<T>) -> &[T] {
    match group {
        Group::NonRecursive(item) => std::slice::from_ref(item),
        Group::Recursive(items) => items,
    }
}

fn all_bindings(program: &PreparedProgram) -> Vec<&HeapBinding> {
    let mut bindings = Vec::new();
    for group in program.bindings() {
        bindings.extend(group_items(group).iter().map(|top| &top.binding));
    }
    for frame in &program.expressions().nodes {
        if let ExprFrame::Let {
            bindings: group, ..
        } = frame
        {
            bindings.extend(group_items(group));
        }
    }
    bindings
}

fn signature(
    program: &PreparedProgram,
    id: tidepool_repr::execution_schema::SignatureId,
) -> &Signature {
    &program.signatures()[id.0 as usize]
}

fn binding_rep(
    program: &PreparedProgram,
    binding: &HeapBinding,
    values: &mut BTreeMap<ValueId, RuntimeRep>,
) {
    let rep = match &binding.rhs {
        HeapRhs::Bytes(_) => RuntimeRep::Address,
        HeapRhs::Function { .. } | HeapRhs::Thunk { .. } => RuntimeRep::LiftedRef,
        HeapRhs::Constructor { constructor, .. } => {
            program.constructors()[constructor.0 as usize].result_rep
        }
    };
    values.insert(binding.id, rep);
}

/// A capture/field's representation for layout purposes -- called for every
/// function/thunk binding's capture list (heap top, static top, or a local
/// `Let` binding alike), well before [`super::image::heap_top_partition`]
/// decides which tops end up static vs. heap. A `Global` reference here
/// needs only the import's DECLARED representation (already checked by
/// `link_program` before this program reached `admission`) to size the
/// capture slot -- not its runtime pointer, which is unknown until install.
/// So this resolves through `program.globals()` (indexed by `GlobalId`,
/// exactly as `import_slots` is later) rather than rejecting the reference.
fn value_ref_rep(
    program: &PreparedProgram,
    values: &BTreeMap<ValueId, RuntimeRep>,
    value: &ValueRef,
) -> Result<RuntimeRep, CompileError> {
    match value {
        ValueRef::Local(id) => values
            .get(id)
            .copied()
            .ok_or(CompileError::MissingRepresentation(*id)),
        ValueRef::Global(id) => program
            .globals()
            .get(id.0 as usize)
            .map(|declaration| declaration.rep)
            .ok_or(CompileError::Unsupported(super::Unsupported::Global(*id))),
    }
}

fn collect_binding_literals(binding: &HeapBinding, bytes: &mut BTreeMap<Vec<u8>, Arc<[u8]>>) {
    match &binding.rhs {
        HeapRhs::Bytes(value) => pin_bytes(value, bytes),
        HeapRhs::Constructor { fields, .. } => collect_atoms(fields, bytes),
        HeapRhs::Function { .. } | HeapRhs::Thunk { .. } => {}
    }
}

fn collect_atoms(atoms: &[Atom], bytes: &mut BTreeMap<Vec<u8>, Arc<[u8]>>) {
    for atom in atoms {
        collect_atom(atom, bytes);
    }
}

fn collect_atom(atom: &Atom, bytes: &mut BTreeMap<Vec<u8>, Arc<[u8]>>) {
    if let Atom::Scalar(literal) = atom {
        collect_literal(literal, bytes);
    }
}

fn collect_literal(literal: &ScalarLiteral, bytes: &mut BTreeMap<Vec<u8>, Arc<[u8]>>) {
    if let ScalarLiteral::Bytes(value) = literal {
        pin_bytes(value, bytes);
    }
}

fn pin_bytes(value: &[u8], bytes: &mut BTreeMap<Vec<u8>, Arc<[u8]>>) {
    bytes.entry(value.to_vec()).or_insert_with(|| {
        // GHC's primitive string literals have an implicit trailing NUL; the
        // wire payload is the logical key, not the complete backing storage.
        let mut storage = Vec::with_capacity(value.len() + 1);
        storage.extend_from_slice(value);
        storage.push(0);
        Arc::<[u8]>::from(storage)
    });
}
