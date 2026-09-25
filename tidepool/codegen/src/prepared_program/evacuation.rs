//! Evacuation on a prepared machine: export the graph a handle roots into a
//! detached [`Parcel`], and import a parcel from another machine as a new
//! old-space arena rooted by a fresh handle. The heap-level copier and its
//! invariants live in `tidepool_heap::gc::evacuate`; this module supplies
//! the machine's spaces, ledger, roots and image manifest.
//!
//! What crosses: every object reachable from the handle that lives in this
//! machine's nursery or old space, copied; external payloads, copied into
//! parcel storage and re-registered on import; the images the copied
//! objects and static references belong to, named in the parcel and
//! installed on the importer if it lacks them; and the values those
//! images' import slots hold, copied in the same pass so the imported
//! copies share structure with the value (an image installs on the
//! importer bound to the copies, so a closure over an earlier binding sees
//! the sender's snapshot of that binding). An image the importer already
//! installed keeps its root block and import bindings, whether those slots
//! hold package tops or earlier notebook bindings: transferred code runs in
//! the importer's existing import environment, which the first parcel that
//! installed the image seeded, and a later parcel's copies of those slots
//! are released. Two closures from one image on one machine therefore
//! always share one environment. Static references and code are
//! shared by address with any machine that installed the same image
//! (`CompiledProgram::shared_statics`). What never crosses: a continuation
//! or an object under evaluation (typed refusals), and identity: a MutVar
//! or an imported binding arrives as its own copy.

use super::machine::{PreparedHandle, PreparedMachine, ProgramId};
use super::run::runtime_error;
use super::{CompiledProgram, ExecutionError, ImportBindings};
use crate::suspension::RealmId;
use std::collections::HashMap;
use std::sync::Arc;
use tidepool_heap::descriptor_region::DescriptorArena;
use tidepool_heap::execution_descriptor::{DescriptorTraceError, ObjectDescriptor};
use tidepool_heap::external_storage::ExternalStorageKind;
use tidepool_heap::gc::evacuate::{export_reachable, MachineSpaces, NurseryView};
use tidepool_repr::execution_schema::{RuntimeRep, SymbolIdentity};
use tidepool_repr::DataConId;

/// For each import slot of an image: the identity imported and the index
/// (into the parcel's roots) of the copied value the importer binds it to.
pub type ParcelImports = Vec<(SymbolIdentity, usize)>;

/// One image a parcel depends on: the compiled image itself and its
/// import slots as [`ParcelImports`].
pub struct ParcelImage {
    pub image: Arc<CompiledProgram>,
    pub imports: ParcelImports,
}

/// A constructor descriptor the copied objects use. Interned constructors
/// belong to no image (an image that declares one shares the process-wide
/// descriptor, and retiring that image leaves it), so a parcel carries them
/// itself, with the observation identity the importer registers.
pub struct ParcelConstructor {
    pub descriptor: Arc<ObjectDescriptor>,
    pub identity: DataConId,
    pub fields: Vec<RuntimeRep>,
}

/// A detached copy of one value together with everything a machine that
/// never saw its sender needs to hold and run it. Root 0 is the value; the
/// remaining roots are the import-slot values of the images in `images`.
pub struct Parcel {
    heap: tidepool_heap::gc::evacuate::Parcel,
    images: Vec<ParcelImage>,
    constructors: Vec<ParcelConstructor>,
}

impl Parcel {
    pub fn bytes(&self) -> usize {
        self.heap.bytes()
    }

    pub fn constructors(&self) -> &[ParcelConstructor] {
        &self.constructors
    }

    pub fn root(&self) -> usize {
        self.heap.root()
    }

    pub fn images(&self) -> &[ParcelImage] {
        &self.images
    }
}

/// The largest number of export rounds a manifest fixpoint may take: every
/// round adds at least one image, and an image's imports are bounded.
const MANIFEST_ROUNDS: usize = 64;

impl PreparedMachine<'_> {
    /// Export the graph `handle` roots into a parcel. The machine is
    /// unchanged afterwards: every forwarding word the copy wrote is
    /// restored before this returns, on success and on failure alike.
    ///
    /// The parcel names every image its objects or static references belong
    /// to, and carries the current value of each such image's import slots
    /// as extra roots; the export repeats until that set is stable.
    pub fn export_parcel(&mut self, handle: PreparedHandle) -> Result<Parcel, ExecutionError> {
        self.ensure_handle_access()?;
        let _quiescent = self.quiesce()?;
        let value = {
            let entry = self
                .handles
                .handle(handle.raw())
                .ok_or(ExecutionError::UnknownPreparedHandle)?;
            // SAFETY: the ledger keeps the slot registered while the handle
            // is live, and the machine is quiescent.
            unsafe { entry.slot.current() }
        } as usize;
        let mut roots = vec![value];
        let mut images: Vec<(ProgramId, Arc<CompiledProgram>, ParcelImports)> = Vec::new();
        for _ in 0..MANIFEST_ROUNDS {
            let heap = self.export_roots(&roots)?;
            // Owners of every copied object and every static reference.
            // An interned constructor or an external wrapper has no owner
            // (see `install_shared`): externals are process-wide and every
            // machine knows them; constructors travel in the parcel. Any
            // other unowned header is a defect on this side, refused here
            // where the object can still be named.
            let mut owners: Vec<ProgramId> = Vec::new();
            let mut constructors: Vec<ParcelConstructor> = Vec::new();
            for header in heap.headers().map_err(ExecutionError::Evacuation)? {
                if let Some(id) = self.owner_of_header(header) {
                    if !owners.contains(&id) {
                        owners.push(id);
                    }
                    continue;
                }
                let Some(entry) = self.descriptor_registry.get(&header) else {
                    return Err(ExecutionError::Evacuation(
                        DescriptorTraceError::UnknownDescriptor { address: header },
                    ));
                };
                match &entry.meaning {
                    super::DescriptorMeaning::External => {}
                    super::DescriptorMeaning::Constructor(observation) => {
                        if !constructors
                            .iter()
                            .any(|known| known.descriptor.initial_header_word() == header)
                        {
                            constructors.push(ParcelConstructor {
                                descriptor: Arc::clone(&entry.descriptor),
                                identity: observation.identity,
                                fields: observation.fields.clone(),
                            });
                        }
                    }
                    super::DescriptorMeaning::Callable { .. } | super::DescriptorMeaning::Pap => {
                        tracing::error!(
                            header = format_args!("{header:#x}"),
                            kind = ?entry.descriptor.kind(),
                            "export_parcel: a callable's header belongs to no installed image"
                        );
                        return Err(ExecutionError::Invariant(
                            "export_parcel: a callable's header belongs to no installed image",
                        ));
                    }
                }
            }
            for address in heap
                .static_references()
                .map_err(ExecutionError::Evacuation)?
            {
                if let Some(id) = self.owner_of_static(address) {
                    if !owners.contains(&id) {
                        owners.push(id);
                    }
                } else {
                    return Err(ExecutionError::Invariant(
                        "export_parcel: a static reference names no installed program",
                    ));
                }
            }
            let mut grew = false;
            for id in owners {
                if images.iter().any(|(known, _, _)| *known == id) {
                    continue;
                }
                // A borrowed image (test harness custody) cannot be named
                // in a parcel; both machines must hold it already.
                let Some((image, slots)) = self.image_with_imports(id) else {
                    continue;
                };
                let mut imports = Vec::with_capacity(slots.len());
                for (identity, word) in slots {
                    let index = match roots.iter().position(|&root| root == word) {
                        Some(index) => index,
                        None => {
                            roots.push(word);
                            grew = true;
                            roots.len() - 1
                        }
                    };
                    imports.push((identity, index));
                }
                images.push((id, image, imports));
            }
            if !grew {
                return Ok(Parcel {
                    heap,
                    images: images
                        .into_iter()
                        .map(|(_, image, imports)| ParcelImage { image, imports })
                        .collect(),
                    constructors,
                });
            }
        }
        Err(ExecutionError::Invariant(
            "export_parcel: the image manifest did not converge",
        ))
    }

    /// One export round over `roots`; the source is restored afterwards.
    fn export_roots(
        &mut self,
        roots: &[usize],
    ) -> Result<tidepool_heap::gc::evacuate::Parcel, ExecutionError> {
        let mut state = self
            .machine
            .take_gc_state()
            .ok_or_else(|| runtime_error(&self.machine, crate::host_fns::bad_pointer()))?;
        let outcome = (|| {
            let used = (self.vmctx.alloc_ptr as usize)
                .checked_sub(state.active_start as usize)
                .filter(|used| *used <= state.active_size && used % 8 == 0)
                .ok_or_else(|| runtime_error(&self.machine, crate::host_fns::bad_pointer()))?;
            let prepared = state
                .prepared
                .as_mut()
                .ok_or_else(|| runtime_error(&self.machine, crate::host_fns::bad_pointer()))?;
            // SAFETY: quiescent machine; `used` is the initialized nursery
            // prefix and `prepared.space` pins every layout in it.
            let (nursery, nursery_externals) =
                unsafe { NurseryView::walk(&prepared.space, state.active_start, used) }
                    .map_err(ExecutionError::Evacuation)?;
            let mut arena_externals = 0;
            for arena in &self.old_space.prepared_arenas {
                arena
                    .walk_sealed(|_, descriptor| {
                        arena_externals += usize::from(descriptor.external_kind().is_some());
                        Ok(())
                    })
                    .map_err(ExecutionError::Evacuation)?;
            }
            let spaces = MachineSpaces {
                nursery,
                arenas: &self.old_space.prepared_arenas,
            };
            // SAFETY: the machine is quiescent and exclusively borrowed for
            // the whole export; the ledger describes and owns every payload.
            unsafe {
                export_reachable(
                    roots,
                    &spaces,
                    nursery_externals + arena_externals,
                    &mut prepared.space,
                    &self.descriptors,
                    &*self.machine,
                )
            }
            .map_err(ExecutionError::Evacuation)
        })();
        self.machine.put_gc_state(state);
        outcome
    }

    /// Import a parcel exported by another machine: its objects become one
    /// new old-space arena, its payloads become ledger allocations, every
    /// image it names that this machine lacks is installed (bound to the
    /// copies of its import slots), and the returned handle roots the value
    /// in `realm`. The second element is every distinct identity (across
    /// every newly installed image's `ParcelImports`, deduplicated) paired
    /// with the handle rooting its copied value — the session layer records
    /// these in the persistent binding store so a LATER compiled program on
    /// this machine can resolve the same identity by name and generation,
    /// exactly as it would on the machine the parcel came from. An identity
    /// belonging to an image that was already installed here contributes
    /// nothing (its value already had a live root before this import).
    pub fn import_parcel(
        &mut self,
        parcel: Parcel,
        realm: RealmId,
    ) -> Result<(PreparedHandle, Vec<(SymbolIdentity, PreparedHandle)>), ExecutionError> {
        self.ensure_handle_access()?;
        let _quiescent = self.quiesce()?;
        let Parcel {
            heap: mut parcel,
            images,
            constructors,
        } = parcel;
        if parcel.root() == 0 {
            return Err(ExecutionError::Evacuation(
                DescriptorTraceError::TaggedNull { value: 0 },
            ));
        }
        let missing: Vec<&ParcelImage> = images
            .iter()
            .filter(|entry| !self.has_image(&entry.image))
            .collect();
        let new_constructors: Vec<&ParcelConstructor> = constructors
            .iter()
            .filter(|entry| {
                !self
                    .descriptor_registry
                    .contains_key(&entry.descriptor.initial_header_word())
            })
            .collect();
        // Every header the parcel carries must be known here, carried as a
        // constructor, or brought by an image the parcel names.
        for header in parcel.headers().map_err(ExecutionError::Evacuation)? {
            let known = self.descriptor_registry.contains_key(&header)
                || self
                    .descriptors
                    .iter()
                    .any(|descriptor| descriptor.initial_header_word() == header)
                || new_constructors
                    .iter()
                    .any(|entry| entry.descriptor.initial_header_word() == header)
                || missing.iter().any(|entry| {
                    entry
                        .image
                        .descriptors
                        .iter()
                        .any(|descriptor| descriptor.initial_header_word() == header)
                });
            if !known {
                return Err(ExecutionError::Evacuation(
                    DescriptorTraceError::UnknownDescriptor { address: header },
                ));
            }
        }
        let roots = parcel.roots().len();
        self.handles.try_reserve_handles(roots).map_err(|_| {
            runtime_error(&self.machine, crate::host_fns::RuntimeError::HeapOverflow)
        })?;
        // Carried constructors join this machine's registry exactly as an
        // install's interned constructors do: shared, never owned, never
        // retired. Infallible, so it happens before any heap work.
        self.descriptors.reserve(new_constructors.len());
        for entry in &new_constructors {
            let header = entry.descriptor.initial_header_word();
            self.descriptors.push(Arc::clone(&entry.descriptor));
            self.descriptor_registry.insert(
                header,
                super::DescriptorMetadata {
                    descriptor: Arc::clone(&entry.descriptor),
                    meaning: super::DescriptorMeaning::Constructor(super::ConstructorObservation {
                        identity: entry.identity,
                        fields: entry.fields.clone(),
                    }),
                },
            );
            self.machine
                .register_prepared_constructors([(header, entry.identity)]);
        }
        let carried: Vec<Arc<ObjectDescriptor>> = new_constructors
            .iter()
            .map(|entry| Arc::clone(&entry.descriptor))
            .collect();
        drop(new_constructors);

        // Payloads first: the copier expands them through this machine's
        // ledger, so the parcel must already point at ledger allocations.
        let mut payload_map = HashMap::new();
        for payload in parcel.payloads() {
            let shape = payload.shape();
            let published = match shape.kind {
                ExternalStorageKind::Bytes => self
                    .machine
                    .allocate_external_bytes(shape.logical_len, shape.align),
                ExternalStorageKind::BoxedArray => self
                    .machine
                    .allocate_external_storage(shape.kind, shape.logical_len),
            }
            .map_err(|error| {
                ExecutionError::Evacuation(DescriptorTraceError::ExternalPayload(error))
            })?;
            let data = payload.data();
            // SAFETY: the ledger allocated `logical_len` bytes (or slots)
            // after the length prefix at `published`.
            unsafe { std::ptr::copy_nonoverlapping(data.as_ptr(), published.add(8), data.len()) };
            payload_map.insert(payload.published() as usize, published);
        }
        // SAFETY: the parcel is sealed and owned; no copy is in progress.
        unsafe { parcel.rewrite_payloads(&payload_map) }.map_err(ExecutionError::Evacuation)?;

        let relocated: Vec<usize> = if parcel.bytes() == 0 {
            // Static or otherwise stable roots: nothing to copy, but the
            // images that own them must still be installed below.
            parcel.roots().to_vec()
        } else {
            let mut state = self
                .machine
                .take_gc_state()
                .ok_or_else(|| runtime_error(&self.machine, crate::host_fns::bad_pointer()))?;
            let outcome: Result<Vec<usize>, ExecutionError> = (|| {
                let prepared = state
                    .prepared
                    .as_mut()
                    .ok_or_else(|| runtime_error(&self.machine, crate::host_fns::bad_pointer()))?;
                // The copier must recognise every header and static region
                // the parcel names before the images that own them are
                // installed (installing needs the copied import values, so
                // the copy comes first). Descriptors and static regions are
                // shared, immutable `Arc`s; knowing them early is harmless.
                let mut arena_descriptors = self.descriptors.clone();
                prepared
                    .space
                    .extend_descriptors(carried.iter().cloned())
                    .map_err(ExecutionError::Evacuation)?;
                for entry in &missing {
                    prepared
                        .space
                        .extend_descriptors(entry.image.descriptors.iter().cloned())
                        .map_err(ExecutionError::Evacuation)?;
                    prepared
                        .space
                        .extend_static_region(entry.image.shared_statics()?)
                        .map_err(ExecutionError::Evacuation)?;
                    arena_descriptors.extend(entry.image.descriptors.iter().cloned());
                }
                let mut arena = DescriptorArena::reserve(parcel.bytes(), arena_descriptors)
                    .map_err(ExecutionError::Evacuation)?;
                self.old_space.prepared_arenas.try_reserve(1).map_err(|_| {
                    runtime_error(&self.machine, crate::host_fns::RuntimeError::HeapOverflow)
                })?;
                let range = arena.allocation_range();
                self.machine
                    .register_old_space_arena(range.start as *const u8, range.end as *const u8);
                self.machine.arm_write_barrier();
                // SAFETY: quiescent, exclusively borrowed machine; the parcel
                // already points at this ledger's payloads.
                let copied = unsafe {
                    parcel.import_into(&mut arena, &mut prepared.space, None, &*self.machine)
                };
                let (relocated, _) = match copied {
                    Ok(copied) => copied,
                    Err(error) => {
                        self.machine.retire_old_space_arena(
                            range.start as *const u8,
                            range.end as *const u8,
                        );
                        return Err(ExecutionError::Evacuation(error));
                    }
                };
                let visited: Vec<_> = prepared.space.visited_external_payloads().collect();
                self.old_space.prepared_arenas.push(arena);
                self.machine
                    .retain_external_payloads(&visited)
                    .map_err(|error| {
                        ExecutionError::Evacuation(DescriptorTraceError::ExternalPayload(error))
                    })?;
                Ok(relocated)
            })();
            self.machine.put_gc_state(state);
            outcome?
        };

        // Every root gets a handle: the value's for the caller, the imports'
        // for the installs below and the caller after
        // (`Self::import_parcel`'s own doc comment — kept, not released,
        // once the blocks hold them too).
        let mut handles = Vec::with_capacity(relocated.len());
        for &pointer in &relocated {
            let slot = self
                .old_space
                .adopt_root(&self.machine, pointer as *mut u8)
                .map_err(|cause| runtime_error(&self.machine, cause))?;
            let raw = self
                .handles
                .insert_handle(slot, realm, RuntimeRep::LiftedRef);
            handles.push(PreparedHandle::new(raw, RuntimeRep::LiftedRef));
        }
        // The images install in manifest order, and an image's import values
        // may belong to a sibling that installs later: an install verifies
        // its imports through this machine's registry, so every missing
        // image's descriptors join the registry first. Shared `Arc`s; an
        // install's own extension of the registry skips what is present.
        for entry in &missing {
            for (&header, metadata) in &entry.image.descriptor_registry {
                if self.descriptor_registry.contains_key(&header) {
                    continue;
                }
                self.descriptors.push(Arc::clone(&metadata.descriptor));
                self.descriptor_registry.insert(header, metadata.clone());
                if let super::DescriptorMeaning::Constructor(observation) = &metadata.meaning {
                    self.machine
                        .register_prepared_constructors([(header, observation.identity)]);
                }
            }
        }
        let missing: Vec<(Arc<CompiledProgram>, ParcelImports)> = missing
            .into_iter()
            .map(|entry| (Arc::clone(&entry.image), entry.imports.clone()))
            .collect();
        // Kept, not released: the caller (session layer) roots these in the
        // persistent binding store, so a later compiled program's import
        // resolves against the SAME live value the images below install
        // bound to. An index not named by any kept identity (a duplicate
        // root two images both import) is released with the rest below.
        let mut imported: Vec<(SymbolIdentity, PreparedHandle)> = Vec::new();
        let mut kept_indices: std::collections::HashSet<usize> = std::collections::HashSet::new();
        let mut seen_identities: std::collections::BTreeSet<SymbolIdentity> =
            std::collections::BTreeSet::new();
        for (image, imports) in missing {
            let mut bindings = ImportBindings::new();
            for (identity, index) in imports {
                let handle = handles
                    .get(index)
                    .copied()
                    .ok_or(ExecutionError::Invariant(
                        "import_parcel: an image import names a root the parcel lacks",
                    ))?;
                bindings.insert(identity.clone(), handle);
                if seen_identities.insert(identity.clone()) {
                    imported.push((identity, handle));
                    kept_indices.insert(index);
                }
            }
            self.install_shared(image, bindings)?;
        }
        for (index, handle) in handles.iter().copied().enumerate().skip(1) {
            if !kept_indices.contains(&index) {
                self.release(handle);
            }
        }
        Ok((handles[0], imported))
    }
}
