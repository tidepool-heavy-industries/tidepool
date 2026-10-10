//! Evacuation on a prepared machine: export the graph a handle roots into a
//! detached [`Parcel`], and import a parcel from another machine as a new
//! old-space arena rooted by a fresh handle. The heap-level copier and its
//! invariants live in `tidepool_heap::gc::evacuate`; this module supplies
//! the machine's spaces, ledger, roots and image manifest.
//!
//! What crosses: every object reachable from the handle that lives in this
//! machine's nursery or old space, copied; external payloads, copied into
//! parcel storage and re-registered on import; the images the copied
//! objects and static references belong to, named by instance identity in
//! the parcel and installed on the importer if it lacks them; and the values
//! those instances' import slots hold, copied in the same pass. An instance
//! already installed on the importer keeps its machine-local roots and
//! bindings. Static references and descriptors retain instance identity
//! across machines, while native dispatch selects the importer's local
//! environment. What never crosses: a continuation
//! or an object under evaluation (typed refusals), and identity: a MutVar
//! or an imported binding arrives as its own copy.

use super::instance::InstanceImage;
use super::machine::{PreparedHandle, PreparedMachine, ProgramId};
use super::run::runtime_error;
use super::{CompiledProgram, ExecutionError, ImportBindings};
use crate::resource_ledger::{SiteDependencies, SiteDependency};
use crate::suspension::RealmId;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;
use tidepool_heap::descriptor_region::DescriptorArena;
use tidepool_heap::execution_descriptor::{DescriptorTraceError, ObjectDescriptor};
use tidepool_heap::external_storage::ExternalStorageKind;
use tidepool_heap::gc::evacuate::{export_reachable, MachineSpaces, NurseryView};
use tidepool_repr::execution_schema::{RuntimeRep, SymbolIdentity};
use tidepool_repr::DataConId;

/// Admit static roots carried by a parcel until their owning
/// instances install. A failed import must not leave ownerless regions in the
/// receiving machine's exact-address catalog.
struct PendingStaticRegions {
    catalog: Rc<RefCell<tidepool_heap::static_region::StaticRegionCatalog>>,
    pending: Vec<Arc<tidepool_heap::static_region::StaticRegion>>,
}

impl PendingStaticRegions {
    fn new(
        catalog: Rc<RefCell<tidepool_heap::static_region::StaticRegionCatalog>>,
        images: &[&ParcelImage],
    ) -> Result<Self, ExecutionError> {
        let mut admission = Self {
            catalog,
            pending: Vec::new(),
        };
        for image in images {
            let region = &image.instance.statics;
            if admission
                .catalog
                .borrow_mut()
                .insert(Arc::clone(region))
                .map_err(ExecutionError::Evacuation)?
            {
                admission.pending.push(Arc::clone(region));
            }
        }
        Ok(admission)
    }

    fn committed(&mut self) {
        self.pending.clear();
    }
}

impl Drop for PendingStaticRegions {
    fn drop(&mut self) {
        let mut catalog = self.catalog.borrow_mut();
        for region in &self.pending {
            catalog.remove(region);
        }
    }
}

/// For each import slot of an image: the identity imported and the index
/// (into the parcel's roots) of the copied value the importer binds it to.
pub type ParcelImports = Vec<(SymbolIdentity, usize)>;

/// Logical evidence for a managed parcel root. Descriptor shape cannot tell
/// lifted and unlifted references apart; addresses travel with literal owners.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum ParcelRootRep {
    Lifted,
    Unlifted,
}

impl ParcelRootRep {
    fn runtime_rep(self) -> RuntimeRep {
        match self {
            Self::Lifted => RuntimeRep::LiftedRef,
            Self::Unlifted => RuntimeRep::UnliftedRef,
        }
    }
}

/// One exact machine root and its retained semantic representation.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) struct ParcelRoot {
    word: usize,
    rep: ParcelRootRep,
}

impl ParcelRoot {
    pub(super) fn new(word: usize, rep: RuntimeRep) -> Result<Self, ExecutionError> {
        let rep = match rep {
            RuntimeRep::LiftedRef => ParcelRootRep::Lifted,
            RuntimeRep::UnliftedRef => ParcelRootRep::Unlifted,
            _ => {
                return Err(ExecutionError::Invariant(
                    "export_parcel: a root has a non-managed representation",
                ))
            }
        };
        Ok(Self { word, rep })
    }
}

/// One installation instance a parcel depends on, its shared code, and its
/// import slots as [`ParcelImports`].
pub struct ParcelImage {
    pub image: Arc<CompiledProgram>,
    instance: Arc<InstanceImage>,
    pub imports: ParcelImports,
}

#[cfg(test)]
impl ParcelImage {
    pub(super) fn names_instance(&self, instance: &Arc<InstanceImage>) -> bool {
        Arc::ptr_eq(&self.instance, instance)
    }
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

/// A previously issued declaring-row dependency bound to the parcel's exact
/// instance manifest. Its old machine-local owner is never used on import.
struct ParcelSiteDependency {
    image: usize,
    site: SiteDependency,
}

/// A detached copy of one value together with everything a machine that
/// never saw its sender needs to hold and run it. Root 0 is the value; the
/// remaining roots are the import-slot values of the images in `images`.
pub struct Parcel {
    heap: tidepool_heap::gc::evacuate::Parcel,
    /// In heap-root order; the heap owner alone retains relocated words.
    root_reps: Vec<ParcelRootRep>,
    images: Vec<ParcelImage>,
    constructors: Vec<ParcelConstructor>,
    sites: Vec<Vec<ParcelSiteDependency>>,
}

/// Successful import of a value and its exact native instance owners. The
/// program report follows the parcel image order, including already installed
/// instances, so session metadata can admit those same image declarations.
pub struct ImportedParcel {
    pub value: PreparedHandle,
    pub imports: Vec<(SymbolIdentity, PreparedHandle)>,
    pub programs: Vec<(ProgramId, Arc<CompiledProgram>)>,
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

    #[cfg(test)]
    pub(super) fn images_mut(&mut self) -> &mut [ParcelImage] {
        &mut self.images
    }
}

impl PreparedMachine<'_> {
    /// Export the graph `handle` roots into a parcel. The machine is
    /// unchanged afterwards: every forwarding word the copy wrote is
    /// restored before this returns, on success and on failure alike.
    ///
    /// The parcel names every image its objects or static references belong
    /// to, and carries the current value of each such image's import slots
    /// as extra roots. A finite read-only worklist discovers that closure;
    /// the physical heap copier runs once after discovery finishes.
    pub fn export_parcel(&mut self, handle: PreparedHandle) -> Result<Parcel, ExecutionError> {
        self.ensure_handle_access()?;
        let _quiescent = self.quiesce()?;
        let (value, retained_sites) = {
            let entry = self
                .handles
                .handle(handle.raw())
                .filter(|entry| entry.rep == handle.rep())
                .ok_or(ExecutionError::UnknownPreparedHandle)?;
            // SAFETY: the ledger keeps the slot registered while the handle
            // is live, and the machine is quiescent.
            (
                ParcelRoot::new(unsafe { entry.slot.current() } as usize, entry.rep)?,
                entry.sites.clone(),
            )
        };
        let mut roots = vec![value];
        let mut sites = vec![retained_sites];
        let mut root_indices = HashMap::from([(value, 0)]);
        let mut images = Vec::new();
        let mut constructors = Vec::new();
        let mut image_ids = HashMap::new();
        let mut image_work = sites[0].iter().map(|site| site.owner()).collect::<Vec<_>>();
        let mut constructor_headers = HashSet::new();
        let mut capacity = 0usize;
        {
            let heap = self.observation_heap()?;
            let mut work = vec![value.word];
            let mut visited = HashSet::new();
            while !work.is_empty() || !image_work.is_empty() {
                let id = if let Some(id) = image_work.pop() {
                    id
                } else {
                    let word = work.pop().expect("nonempty traversal work");
                    if word == 0 {
                        continue;
                    }
                    if !visited.insert(tidepool_heap::managed_reference::untag(word)) {
                        continue;
                    }
                    let step = heap.trace_parcel_step(word)?;
                    let owner = match step {
                        super::observe::ParcelTraceStep::Updated { target } => {
                            work.push(target);
                            continue;
                        }
                        super::observe::ParcelTraceStep::Static { region_start } => {
                            Some(self.owner_of_static_region(region_start).ok_or(
                                ExecutionError::Invariant(
                                    "export_parcel: a static reference names no installed program",
                                ),
                            )?)
                        }
                        super::observe::ParcelTraceStep::Object {
                            header,
                            bytes,
                            children,
                        } => {
                            capacity =
                                capacity
                                    .checked_add(bytes)
                                    .ok_or(ExecutionError::Evacuation(
                                        DescriptorTraceError::InvalidRange,
                                    ))?;
                            work.extend(children);
                            if let Some(id) = self.owner_of_header(header) {
                                Some(id)
                            } else {
                                let entry = self.descriptor_registry.get(&header).ok_or(
                                    ExecutionError::Evacuation(
                                        DescriptorTraceError::UnknownDescriptor { address: header },
                                    ),
                                )?;
                                match &entry.meaning {
                                    super::DescriptorMeaning::External => {}
                                    super::DescriptorMeaning::Constructor(observation) => {
                                        if constructor_headers.insert(header) {
                                            constructors.push(ParcelConstructor {
                                                descriptor: Arc::clone(&entry.descriptor),
                                                identity: observation.identity,
                                                fields: observation.fields.clone(),
                                            });
                                        }
                                    }
                                    super::DescriptorMeaning::Callable { .. }
                                    | super::DescriptorMeaning::Pap => {
                                        return Err(ExecutionError::Invariant(
                                        "export_parcel: a callable's header belongs to no installed image",
                                    ));
                                    }
                                }
                                None
                            }
                        }
                    };
                    let Some(id) = owner else {
                        continue;
                    };
                    id
                };
                if image_ids.contains_key(&id) {
                    continue;
                }
                image_ids.insert(id, images.len());
                let (image, instance, slots) = self.image_with_imports(id)?;
                let mut imports = Vec::with_capacity(slots.len());
                for (identity, root, retained_sites) in slots {
                    let index = *root_indices.entry(root).or_insert_with(|| {
                        let index = roots.len();
                        roots.push(root);
                        sites.push(SiteDependencies::new());
                        work.push(root.word);
                        index
                    });
                    image_work.extend(retained_sites.iter().map(|site| site.owner()));
                    sites[index].extend(retained_sites);
                    imports.push((identity, index));
                }
                images.push(ParcelImage {
                    image,
                    instance,
                    imports,
                });
            }
        }
        let sites = sites
            .into_iter()
            .map(|sites| {
                sites
                    .into_iter()
                    .map(|site| {
                        let image =
                            *image_ids
                                .get(&site.owner())
                                .ok_or(ExecutionError::Invariant(
                                    "export_parcel: declaring site has no carried image",
                                ))?;
                        if images[image]
                            .image
                            .definition_facts()
                            .sites
                            .get(site.row())
                            .is_none()
                        {
                            return Err(ExecutionError::Invariant(
                                "export_parcel: declaring site row is absent",
                            ));
                        }
                        Ok(ParcelSiteDependency { image, site })
                    })
                    .collect::<Result<Vec<_>, ExecutionError>>()
            })
            .collect::<Result<Vec<_>, ExecutionError>>()?;
        let words: Vec<_> = roots.iter().map(|root| root.word).collect();
        let heap = self.export_roots(&words, capacity)?;
        Ok(Parcel {
            heap,
            root_reps: roots.into_iter().map(|root| root.rep).collect(),
            images,
            constructors,
            sites,
        })
    }

    /// One physical copy over the discovered roots; the source is restored.
    fn export_roots(
        &mut self,
        roots: &[usize],
        capacity: usize,
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
                    capacity,
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

    /// Distinct imported identities that importing this parcel would bind.
    /// Already installed instances preserve their existing bindings and add
    /// no identities. This read-only query uses the importer's exact instance
    /// selection so a session can refuse occupied IDs before native mutations.
    #[must_use]
    pub fn pending_parcel_import_identities(&self, parcel: &Parcel) -> Vec<SymbolIdentity> {
        self.missing_parcel_images(&parcel.images)
            .into_iter()
            .flat_map(|entry| entry.imports.iter().map(|(identity, _)| identity.clone()))
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    fn missing_parcel_images<'p>(&self, images: &'p [ParcelImage]) -> Vec<&'p ParcelImage> {
        images
            .iter()
            .filter(|entry| !self.has_instance(&entry.instance))
            .collect()
    }

    /// Import a parcel exported by another machine: its objects become one
    /// new old-space arena, its payloads become ledger allocations, every
    /// image it names that this machine lacks is installed (bound to the
    /// copies of its import slots), and the returned handle roots the value
    /// in `realm`. The import report carries every distinct identity (across
    /// every newly installed image's `ParcelImports`, deduplicated) paired
    /// with the handle rooting its copied value — the session layer records
    /// these in the persistent binding store so a LATER compiled program on
    /// this machine can resolve the same identity by name and generation,
    /// exactly as it would on the machine the parcel came from. An identity
    /// belonging to an image that was already installed here contributes
    /// nothing (its value already had a live root before this import).
    ///
    /// [`Self::pending_parcel_import_identities`] exposes the exact identities
    /// that require new session bindings before this method mutates the machine.
    pub fn import_parcel(
        &mut self,
        parcel: Parcel,
        realm: RealmId,
    ) -> Result<ImportedParcel, ExecutionError> {
        self.ensure_handle_access()?;
        let _quiescent = self.quiesce()?;
        let Parcel {
            heap: mut parcel,
            root_reps,
            images,
            constructors,
            sites,
        } = parcel;
        if parcel.root() == 0 {
            return Err(ExecutionError::Evacuation(
                DescriptorTraceError::TaggedNull { value: 0 },
            ));
        }
        // Validate the sealed dependency manifest before admitting descriptors,
        // allocating roots or installing native images.
        if sites.len() != parcel.roots().len() || root_reps.len() != sites.len() {
            return Err(ExecutionError::Invariant(
                "import_parcel: root dependency count differs",
            ));
        }
        for dependency in sites.iter().flatten() {
            let image = images
                .get(dependency.image)
                .ok_or(ExecutionError::Invariant(
                    "import_parcel: declaring site image is absent",
                ))?;
            if image
                .image
                .definition_facts()
                .sites
                .get(dependency.site.row())
                .is_none()
            {
                return Err(ExecutionError::Invariant(
                    "import_parcel: declaring site row is absent",
                ));
            }
        }
        let missing = self.missing_parcel_images(&images);
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
                        .instance
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
        let mut handles = Vec::with_capacity(roots);
        let mut installed_ids = Vec::new();
        let mut provisional_headers = Vec::new();
        let mut gc_provisional_headers = Vec::new();
        let mut static_admission = None;
        let result = (|| {
            // Carried constructors join this machine's registry exactly as an
            // install's interned constructors do. Their provisional registrations
            // stay available through failure collection and commit with the import.
            self.descriptors.reserve(new_constructors.len());
            for entry in &new_constructors {
                let header = entry.descriptor.initial_header_word();
                provisional_headers.push(header);
                self.descriptors.push(Arc::clone(&entry.descriptor));
                self.descriptor_registry.insert(
                    header,
                    super::DescriptorMetadata {
                        descriptor: Arc::clone(&entry.descriptor),
                        meaning: super::DescriptorMeaning::Constructor(
                            super::ConstructorObservation {
                                identity: entry.identity,
                                fields: entry.fields.clone(),
                            },
                        ),
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

            // A static root needs no heap copy, but an imported instance can
            // verify one of its imports before the instance owning that root is
            // installed. Admit only the exact immutable regions named by this
            // parcel for that interval; installation takes over their ownership.
            let catalog = self
                .machine
                .prepared_static_catalog()
                .map_err(|cause| runtime_error(&self.machine, cause))?;
            static_admission = Some(PendingStaticRegions::new(catalog, &missing)?);

            // Copy and rollback collection both need every image layout
            // before any instance installs. Dispatch is still unavailable
            // until the instance's native installation succeeds.
            for entry in &missing {
                for (&header, metadata) in &entry.instance.descriptor_registry {
                    if self.descriptor_registry.contains_key(&header) {
                        continue;
                    }
                    provisional_headers.push(header);
                    self.descriptors.push(Arc::clone(&metadata.descriptor));
                    self.descriptor_registry.insert(header, metadata.clone());
                    if let super::DescriptorMeaning::Constructor(observation) = &metadata.meaning {
                        self.machine
                            .register_prepared_constructors([(header, observation.identity)]);
                    }
                }
            }

            // Copy and installation admit descriptor-space keys independently
            // of the native registry. Remember only previously absent keys;
            // failed image retirement leaves shared constructor keys admitted.
            let state = self
                .machine
                .take_gc_state()
                .ok_or_else(|| runtime_error(&self.machine, crate::host_fns::bad_pointer()))?;
            let keys = state.prepared.as_ref().map(|prepared| {
                carried
                    .iter()
                    .chain(
                        missing
                            .iter()
                            .flat_map(|entry| entry.instance.descriptors.iter()),
                    )
                    .map(|descriptor| descriptor.initial_header_word())
                    .filter(|header| prepared.space.live_descriptor(*header).is_none())
                    .collect::<Vec<_>>()
            });
            self.machine.put_gc_state(state);
            gc_provisional_headers =
                keys.ok_or_else(|| runtime_error(&self.machine, crate::host_fns::bad_pointer()))?;

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
                unsafe {
                    std::ptr::copy_nonoverlapping(data.as_ptr(), published.add(8), data.len())
                };
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
                    let prepared = state.prepared.as_mut().ok_or_else(|| {
                        runtime_error(&self.machine, crate::host_fns::bad_pointer())
                    })?;
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
                            .extend_descriptors(entry.instance.descriptors.iter().cloned())
                            .map_err(ExecutionError::Evacuation)?;
                        prepared
                            .space
                            .extend_static_region(Arc::clone(&entry.instance.statics))
                            .map_err(ExecutionError::Evacuation)?;
                        arena_descriptors.extend(entry.instance.descriptors.iter().cloned());
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

            let missing: Vec<(Arc<CompiledProgram>, Arc<InstanceImage>, ParcelImports)> = missing
                .into_iter()
                .map(|entry| {
                    (
                        Arc::clone(&entry.image),
                        Arc::clone(&entry.instance),
                        entry.imports.clone(),
                    )
                })
                .collect();
            // Kept, not released: the caller (session layer) roots these in the
            // persistent binding store, so a later compiled program's import
            // resolves against the SAME live value the images below install
            // bound to. An index not named by any kept identity (a duplicate
            // root two images both import) is released with the rest below.
            // Every root is retained before installation can allocate or collect.
            for (&pointer, rep) in relocated.iter().zip(&root_reps) {
                let rep = rep.runtime_rep();
                let slot = crate::old_space::OwnedRootCell::new(&self.machine, pointer as *mut u8)
                    .map_err(|cause| runtime_error(&self.machine, cause))?;
                let raw = self.handles.insert_handle(slot, realm, rep);
                handles.push(PreparedHandle::new(raw, rep));
            }
            let mut imported: Vec<(SymbolIdentity, PreparedHandle)> = Vec::new();
            let mut kept_indices: std::collections::HashSet<usize> =
                std::collections::HashSet::new();
            let mut seen_identities: std::collections::BTreeSet<SymbolIdentity> =
                std::collections::BTreeSet::new();
            for (image, instance, imports) in missing {
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
                installed_ids.push(self.install_instance(image, instance, bindings)?);
            }
            // Only the successful exact instance map can remap issued owners.
            // Existing instances retain their current local import custody.
            let image_programs = images
                .iter()
                .map(|image| {
                    self.program_for_instance(&image.instance)
                        .expect("installed parcel instance")
                })
                .collect::<Vec<_>>();
            let mapped_sites = sites
                .iter()
                .map(|sites| {
                    sites
                        .iter()
                        .map(|dependency| dependency.site.remap(image_programs[dependency.image]))
                        .collect::<SiteDependencies>()
                })
                .collect::<Vec<_>>();
            for (handle, sites) in handles.iter().zip(&mapped_sites) {
                self.handles
                    .extend_handle_sites(handle.raw(), sites.clone());
            }
            for (image, program) in images.iter().zip(&image_programs) {
                if !installed_ids.contains(program) {
                    continue;
                }
                self.retain_parcel_import_sites(*program, &image.imports, &mapped_sites)?;
            }
            for (index, handle) in handles.iter().copied().enumerate().skip(1) {
                if !kept_indices.contains(&index) {
                    self.release(handle);
                }
            }
            Ok((handles[0], imported))
        })();
        match result {
            Ok(imported) => {
                if let Some(admission) = &mut static_admission {
                    admission.committed();
                }
                let (value, imports) = imported;
                let programs = images
                    .into_iter()
                    .map(|image| {
                        let program = self
                            .program_for_instance(&image.instance)
                            .expect("successful parcel import installs every exact instance");
                        (program, image.image)
                    })
                    .collect();
                Ok(ImportedParcel {
                    value,
                    imports,
                    programs,
                })
            }
            Err(error) => {
                for handle in handles {
                    self.release(handle);
                }
                if let Err(cleanup) = self.retire_failed_parcel_installs(&installed_ids) {
                    tracing::error!(
                        ?error,
                        ?cleanup,
                        "parcel import failed with incomplete rollback"
                    );
                    // Uncertain cleanup fences the machine. Retain admitted
                    // regions until teardown so residual code cannot dangle.
                    if let Some(admission) = &mut static_admission {
                        admission.committed();
                    }
                    return Err(cleanup);
                }
                // Collection proved no copied object still names these
                // provisional layouts. The copy's registrations need cleanup
                // even when the first image never installed.
                let Some(mut state) = self.machine.take_gc_state() else {
                    if let Some(admission) = &mut static_admission {
                        admission.committed();
                    }
                    return Err(runtime_error(&self.machine, crate::host_fns::bad_pointer()));
                };
                let cleaned = if let Some(prepared) = state.prepared.as_mut() {
                    prepared.space.retire_owner(&gc_provisional_headers, None);
                    true
                } else {
                    false
                };
                self.machine.put_gc_state(state);
                if !cleaned {
                    if let Some(admission) = &mut static_admission {
                        admission.committed();
                    }
                    return Err(runtime_error(&self.machine, crate::host_fns::bad_pointer()));
                }
                self.machine
                    .retire_prepared_constructors(&provisional_headers);
                self.descriptors.retain(|descriptor| {
                    !provisional_headers.contains(&descriptor.initial_header_word())
                });
                for header in provisional_headers {
                    self.descriptor_registry.remove(&header);
                }
                Err(error)
            }
        }
    }
}
