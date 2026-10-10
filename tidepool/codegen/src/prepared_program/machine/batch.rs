//! Atomic installation of mutually importing native group images.

use super::super::{DemandedImage, InheritedSourceDemand, SourceBinder, SourceInstanceLease};
use super::*;
use tidepool_repr::execution_schema::{CachedHomeOwner, Signature};

/// One import slot's already admitted owner. `Source` selects a top in this
/// sealed batch; it never accepts a spelling-only match.
pub enum BatchImport {
    Existing {
        handle: PreparedHandle,
        entry_signature: Option<Signature>,
    },
    Source {
        group: usize,
        binding: ValueId,
    },
}

pub struct BatchProgram {
    pub image: Arc<CompiledProgram>,
    /// In the image's declared GlobalId order.
    pub imports: Vec<BatchImport>,
}

/// One exact source binder to root as part of the atomic install. Only
/// requested binders receive a handle; unused exports acquire no lease.
pub struct BatchLeaseRequest {
    group: usize,
    image: Arc<CompiledProgram>,
    owner: CachedHomeOwner,
    original_ordinal: u32,
    binder: SourceBinder,
    binding: ValueId,
    domain: super::super::SourceInstanceDomain,
}

impl BatchLeaseRequest {
    /// Select a binder from the exact certified image that will be installed
    /// at `group`. The batch preflight also verifies pointer-identical image
    /// custody before any machine mutation.
    pub fn for_demanded(
        group: usize,
        demanded: &DemandedImage,
        binder: &SourceBinder,
    ) -> Result<Self, ExecutionError> {
        let source = demanded.group();
        let definitions = source.definitions();
        let value = definitions
            .bindings()
            .iter()
            .flat_map(|group| match group {
                tidepool_repr::execution_schema::Group::NonRecursive(top) => {
                    std::slice::from_ref(top)
                }
                tidepool_repr::execution_schema::Group::Recursive(tops) => tops.as_slice(),
            })
            .find(|top| top.identity == binder.binder && source.binders().contains(&top.identity))
            .map(|top| top.binding.id)
            .ok_or_else(|| ExecutionError::BatchSourceContract(Box::new(binder.binder.clone())))?;
        if binder.version != source.owner().module_version {
            return Err(ExecutionError::BatchSourceContract(Box::new(
                binder.binder.clone(),
            )));
        }
        Ok(Self {
            group,
            image: Arc::clone(demanded.image()),
            owner: source.owner().clone(),
            original_ordinal: source.original_ordinal(),
            binder: binder.clone(),
            binding: value,
            domain: demanded.domain(),
        })
    }
}

pub struct BatchInstallReceipt {
    pub programs: Vec<ProgramId>,
    pub leases: Vec<SourceInstanceLease>,
    pub source_attachments: Vec<super::super::SourceInstanceAttachment>,
}

struct Candidate {
    image: Arc<CompiledProgram>,
    instance: Arc<InstanceImage>,
    roots: RootWords,
    environment: Box<InstallationEnvironment>,
    imports: Vec<BatchImport>,
    heap_extent: usize,
    sites: BTreeMap<usize, SiteDependencies>,
}

impl PreparedMachine<'_> {
    /// Root a newly demanded sibling binder from an exact lexical group
    /// instance already held by `anchor`. The original program's CAFs stay
    /// shared; this creates one new scoped handle without reinstalling code.
    pub fn retain_certified_source_top(
        &mut self,
        requested: &InheritedSourceDemand,
    ) -> Result<SourceInstanceLease, ExecutionError> {
        let anchor = requested.anchor();
        let binder = requested.binder();
        let mismatch = || ExecutionError::BatchSourceContract(Box::new(binder.binder.clone()));
        if anchor.owner() != requested.owner()
            || anchor.original_ordinal() != requested.original_ordinal()
            || binder.version != requested.owner().module_version
        {
            return Err(mismatch());
        }
        let value = requested.value();
        let installed = self
            .programs
            .get(&anchor.instance().program())
            .ok_or(ExecutionError::UnknownProgram(anchor.instance().program()))?;
        let image = installed.program.get();
        if image.certified_source.as_ref()
            != Some(&(requested.owner().clone(), requested.original_ordinal()))
            || image
                .top_exports
                .get(&anchor.value())
                .map(|top| &top.identity)
                != Some(&anchor.binder().binder)
            || image.top_exports.get(&value).map(|top| &top.identity) != Some(&binder.binder)
        {
            return Err(mismatch());
        }
        let entry_signature = image.top_exports[&value].entry_signature.clone();
        self.handle_is_evaluated(anchor.handle())?;
        let handle = self.retain_top(anchor.instance().program(), value)?;
        Ok(SourceInstanceLease::new(
            anchor.instance(),
            requested.owner().clone(),
            requested.original_ordinal(),
            binder.clone(),
            value,
            handle,
            entry_signature,
        ))
    }

    /// Issue a sibling attachment only from this machine's fresh rooted lease.
    pub fn retain_certified_source_attachment(
        &mut self,
        requested: &InheritedSourceDemand,
    ) -> Result<super::super::SourceInstanceAttachment, ExecutionError> {
        let lease = self.retain_certified_source_top(requested)?;
        Ok(
            super::super::SourceInstanceAttachment::inherited(requested, lease)
                .expect("machine validated exact sibling provenance"),
        )
    }

    /// Attach one staged physical sibling to another exact selected domain.
    pub fn share_certified_source_attachment(
        &self,
        requested: &InheritedSourceDemand,
        staged: &super::super::SourceInstanceAttachment,
    ) -> Result<super::super::SourceInstanceAttachment, ExecutionError> {
        let token = staged.lease();
        self.handle_is_evaluated(token.handle())?;
        self.handle_is_evaluated(requested.anchor().handle())?;
        let image = self
            .programs
            .get(&requested.anchor().instance().program())
            .ok_or(ExecutionError::UnknownProgram(
                requested.anchor().instance().program(),
            ))?
            .program
            .get();
        if image.certified_source.as_ref()
            != Some(&(requested.owner().clone(), requested.original_ordinal()))
            || image
                .top_exports
                .get(&requested.value())
                .map(|top| &top.identity)
                != Some(&requested.binder().binder)
        {
            return Err(ExecutionError::BatchSourceContract(Box::new(
                requested.binder().binder.clone(),
            )));
        }
        super::super::SourceInstanceAttachment::inherited(requested, token.clone()).map_err(|_| {
            ExecutionError::BatchSourceContract(Box::new(requested.binder().binder.clone()))
        })
    }

    /// Validate a one-shot source attachment against this still-live machine.
    pub fn validate_source_attachment(
        &self,
        attachment: &super::super::SourceInstanceAttachment,
    ) -> Result<(), ExecutionError> {
        let lease = attachment.lease();
        self.handle_is_evaluated(lease.handle())?;
        let image = self
            .programs
            .get(&lease.instance().program())
            .ok_or(ExecutionError::UnknownProgram(lease.instance().program()))?
            .program
            .get();
        let mismatch =
            || ExecutionError::BatchSourceContract(Box::new(lease.binder().binder.clone()));
        if image.certified_source.as_ref()
            != Some(&(lease.owner().clone(), lease.original_ordinal()))
            || lease.binder().version != lease.owner().module_version
        {
            return Err(mismatch());
        }
        let top = image.top_exports.get(&lease.value()).ok_or_else(mismatch)?;
        if top.identity != lease.binder().binder
            || top.entry_signature.as_ref() != lease.entry_signature()
        {
            return Err(mismatch());
        }
        Ok(())
    }

    /// Install a closed batch of independently compiled original groups as
    /// one transaction. Every candidate receives a fresh root block, static
    /// instance and mutable CAFs; source imports may point forward or form a
    /// cycle. No candidate is visible to execution or retirement until all
    /// source tops and import slots have valid pointers.
    pub fn install_shared_batch(
        &mut self,
        batch: Vec<BatchProgram>,
    ) -> Result<Vec<ProgramId>, ExecutionError> {
        self.install_shared_batch_with_leases(batch, Vec::new())
            .map(|receipt| receipt.programs)
    }

    pub fn install_shared_batch_with_leases(
        &mut self,
        batch: Vec<BatchProgram>,
        requests: Vec<BatchLeaseRequest>,
    ) -> Result<BatchInstallReceipt, ExecutionError> {
        if batch.is_empty() {
            return if requests.is_empty() {
                Ok(BatchInstallReceipt {
                    programs: Vec::new(),
                    leases: Vec::new(),
                    source_attachments: Vec::new(),
                })
            } else {
                Err(ExecutionError::Invariant(
                    "source lease without batch group",
                ))
            };
        }
        let count = u32::try_from(batch.len())
            .map_err(|_| runtime_error_without_machine(RuntimeError::HeapOverflow))?;
        self.next_program
            .checked_add(count)
            .ok_or_else(|| runtime_error_without_machine(RuntimeError::HeapOverflow))?;

        let mut candidates = Vec::with_capacity(batch.len());
        for item in batch {
            let instance = InstanceImage::new(&item.image)?;
            let roots = RootWords::new(item.image.root_words)?;
            let environment = Box::new(InstallationEnvironment {
                roots: roots.as_mut_ptr(),
                descriptors: instance.descriptor_words.as_ptr(),
            });
            let heap_extent = heap_top_extent(&instance.heap_top_specs)?;
            candidates.push(Candidate {
                image: item.image,
                instance,
                roots,
                environment,
                imports: item.imports,
                heap_extent,
                sites: BTreeMap::new(),
            });
        }

        let mut requested = HashSet::new();
        for request in &requests {
            if !requested.insert((request.group, request.binding)) {
                return Err(ExecutionError::Invariant("duplicate source lease request"));
            }
            let candidate = candidates
                .get(request.group)
                .ok_or(ExecutionError::Invariant(
                    "source lease selects absent batch group",
                ))?;
            let export = candidate
                .image
                .top_exports
                .get(&request.binding)
                .ok_or(ExecutionError::MissingEntry(request.binding))?;
            if candidate.image.byte_tops.contains_key(&request.binding)
                || !Arc::ptr_eq(&candidate.image, &request.image)
                || candidate.image.certified_source.as_ref()
                    != Some(&(request.owner.clone(), request.original_ordinal))
                || export.identity != request.binder.binder
                || request.binder.version != request.owner.module_version
                || request.binder.binder.unit != request.owner.unit
                || request.binder.binder.module != request.owner.module
            {
                return Err(ExecutionError::BatchSourceContract(Box::new(
                    request.binder.binder.clone(),
                )));
            }
        }

        // Validate the entire import graph and external handle shapes while
        // this machine still has no candidate descriptors or roots. A source
        // top must be the exact identity and representation the global names.
        let mut external = Vec::new();
        for (index, candidate) in candidates.iter().enumerate() {
            if candidate.imports.len() != candidate.image.import_slots.len() {
                return Err(ExecutionError::Invariant(
                    "batch import count differs from image",
                ));
            }
            for (import_position, (slot, origin)) in candidate
                .image
                .import_slots
                .iter()
                .zip(&candidate.imports)
                .enumerate()
            {
                match origin {
                    BatchImport::Source { group, binding } => {
                        let target = candidates
                            .get(*group)
                            .ok_or(ExecutionError::UnknownPreparedHandle)?;
                        let export = target
                            .image
                            .top_exports
                            .get(binding)
                            .ok_or(ExecutionError::MissingEntry(*binding))?;
                        if export.identity != slot.identity
                            || slot.entry_signature.as_ref().is_some_and(|expected| {
                                export.entry_signature.as_ref() != Some(expected)
                            })
                        {
                            return Err(ExecutionError::BatchImportContract(Box::new(
                                super::super::BatchImportContractMismatch {
                                    program: index,
                                    import_position,
                                    owner: None,
                                    selected: super::super::BatchImportSelection::Source {
                                        group: *group,
                                        binding: *binding,
                                    },
                                    required_identity: slot.identity.clone(),
                                    offered_identity: Some(export.identity.clone()),
                                    required_signature: slot.entry_signature.clone(),
                                    offered_signature: export.entry_signature.clone(),
                                },
                            )));
                        }
                        if export.rep != slot.rep {
                            return Err(ExecutionError::ImportShape {
                                identity: Box::new(slot.identity.clone()),
                                expected: ImportShapeFact::Representation(slot.rep),
                                found: ImportShapeFact::Representation(export.rep),
                            });
                        }
                        if slot.required_evaluated && !export.evaluated {
                            return Err(ExecutionError::ImportShape {
                                identity: Box::new(slot.identity.clone()),
                                expected: ImportShapeFact::Evaluated(true),
                                found: ImportShapeFact::Evaluated(false),
                            });
                        }
                        if let Some(literal) = &slot.literal {
                            if target.image.byte_tops.get(binding).map(Arc::as_ref)
                                != Some(literal.as_ref())
                            {
                                return Err(ExecutionError::BatchSourceContract(Box::new(
                                    slot.identity.clone(),
                                )));
                            }
                        }
                        if let Some(source) = &slot.literal_source {
                            if target.image.source_literal_producer.as_ref().is_none_or(
                                |(value, producer)| value != binding || producer != source,
                            ) || export.identity != source.binder.binder
                                || source.binder.version != source.owner.module_version
                            {
                                return Err(ExecutionError::BatchSourceContract(Box::new(
                                    slot.identity.clone(),
                                )));
                            }
                        }
                    }
                    BatchImport::Existing {
                        handle,
                        entry_signature,
                    } => {
                        if slot.literal.is_some() {
                            return Err(ExecutionError::BatchSourceContract(Box::new(
                                slot.identity.clone(),
                            )));
                        }
                        if slot
                            .entry_signature
                            .as_ref()
                            .is_some_and(|expected| entry_signature.as_ref() != Some(expected))
                        {
                            return Err(ExecutionError::BatchImportContract(Box::new(
                                super::super::BatchImportContractMismatch {
                                    program: index,
                                    import_position,
                                    owner: None,
                                    selected: super::super::BatchImportSelection::Existing {
                                        handle: *handle,
                                    },
                                    required_identity: slot.identity.clone(),
                                    offered_identity: None,
                                    required_signature: slot.entry_signature.clone(),
                                    offered_signature: entry_signature.clone(),
                                },
                            )));
                        }
                        if handle.rep != slot.rep {
                            return Err(ExecutionError::ImportShape {
                                identity: Box::new(slot.identity.clone()),
                                expected: ImportShapeFact::Representation(slot.rep),
                                found: ImportShapeFact::Representation(handle.rep),
                            });
                        }
                        let retained = self
                            .handles
                            .handle(handle.raw)
                            .ok_or(ExecutionError::UnknownPreparedHandle)?;
                        if unsafe { retained.slot.current() }.is_null() {
                            return Err(ExecutionError::UnknownPreparedHandle);
                        }
                        if slot.required_evaluated && !self.handle_is_evaluated(*handle)? {
                            return Err(ExecutionError::ImportShape {
                                identity: Box::new(slot.identity.clone()),
                                expected: ImportShapeFact::Evaluated(true),
                                found: ImportShapeFact::Evaluated(false),
                            });
                        }
                        external.push((index, slot.slot, *handle));
                    }
                }
            }
        }

        // Follow only this batch's admitted import graph. Source-group tops
        // conservatively inherit their group's exact imported site tokens.
        for &(index, slot, handle) in &external {
            let entry = self
                .handles
                .handle(handle.raw)
                .ok_or(ExecutionError::UnknownPreparedHandle)?;
            candidates[index].sites.insert(slot, entry.sites.clone());
        }
        loop {
            let previous = candidates
                .iter()
                .map(|candidate| {
                    candidate
                        .sites
                        .values()
                        .flatten()
                        .copied()
                        .collect::<SiteDependencies>()
                })
                .collect::<Vec<_>>();
            let mut changed = false;
            for candidate in &mut candidates {
                for (slot, origin) in candidate.image.import_slots.iter().zip(&candidate.imports) {
                    if slot.literal.is_some() {
                        continue;
                    }
                    if let BatchImport::Source { group, .. } = origin {
                        let sites = candidate.sites.entry(slot.slot).or_default();
                        let before = sites.len();
                        sites.extend(&previous[*group]);
                        changed |= before != sites.len();
                    }
                }
            }
            if !changed {
                break;
            }
        }

        let mut staged_interner = self.interner.clone();
        for candidate in &candidates {
            staged_interner
                .check_absorb(&candidate.image.interned_constructors)
                .map_err(|conflict| match conflict {
                    super::super::interner::AbsorbConflict::Identity { existing, incoming } => {
                        ExecutionError::DescriptorShape {
                            identity: Box::new(incoming.identity.clone()),
                            existing_field_reps: existing.field_reps.clone(),
                            incoming_field_reps: incoming.field_reps.clone(),
                        }
                    }
                    super::super::interner::AbsorbConflict::HostId {
                        host_id,
                        identity,
                        existing,
                    } => ExecutionError::HostIdConflict {
                        host_id,
                        identity,
                        existing,
                    },
                })?;
            staged_interner.commit_absorb(&candidate.image.interned_constructors);
            staged_interner.commit_externals(&candidate.image.externals);
        }

        let heap_reserve = candidates.iter().try_fold(0usize, |total, candidate| {
            total
                .checked_add(candidate.heap_extent)
                .ok_or_else(|| runtime_error_without_machine(RuntimeError::HeapOverflow))
        })?;
        self.handles
            .try_reserve_handles(requests.len())
            .map_err(|_| runtime_error_without_machine(RuntimeError::HeapOverflow))?;
        self.machine
            .begin_prepared_call()
            .map_err(ExecutionError::Runtime)?;
        let owners = self.machine.mark_prepared_descriptor_owners();
        let stack_maps = self.machine.stack_map_link_count();
        let blocks = candidates
            .iter()
            .map(|candidate| (candidate.roots.as_mut_ptr(), candidate.roots.len()))
            .collect();
        let mut transaction = InstallTransaction {
            machine: self,
            blocks,
            owners,
            stack_maps,
            candidate_alloc_start: None,
            provisional_handles: Vec::with_capacity(requests.len()),
            committed: false,
        };
        let staged = transaction.machine.stage_batch(
            &candidates,
            &external,
            heap_reserve,
            &mut transaction.candidate_alloc_start,
        );
        let leased = if staged.is_ok() {
            transaction.machine.stage_batch_leases(
                &candidates,
                &requests,
                &mut transaction.provisional_handles,
            )
        } else {
            Ok(())
        };
        let catalog = if staged.is_ok() && leased.is_ok() {
            transaction.machine.install_catalog_after_stage()
        } else {
            Ok(None)
        };
        if staged.is_ok() && leased.is_ok() && catalog.is_ok() {
            transaction.machine.commit_batch_metadata(&candidates);
        }
        transaction.committed = staged.is_ok() && leased.is_ok() && catalog.is_ok();
        let handles = if transaction.committed {
            std::mem::take(&mut transaction.provisional_handles)
        } else {
            Vec::new()
        };
        drop(transaction);
        staged?;
        leased?;
        if let Some(catalog) = catalog? {
            self.static_catalog = Some(catalog);
        }

        // Everything below is infallible publication of fully initialized
        // candidates. The machine's descriptor owner mark is committed.
        self.interner = staged_interner;
        let mut ids = Vec::with_capacity(candidates.len());
        for candidate in candidates {
            let id = ProgramId(self.next_program);
            self.next_program += 1;
            let shared: HashSet<_> = candidate
                .image
                .interned_constructors
                .iter()
                .map(|(_, descriptor)| descriptor.initial_header_word())
                .chain(candidate.image.externals.headers())
                .collect();
            let owned_headers: Vec<_> = candidate
                .instance
                .descriptors
                .iter()
                .map(|descriptor| descriptor.initial_header_word())
                .filter(|header| !shared.contains(header))
                .collect();
            self.header_owners
                .extend(owned_headers.iter().map(|&header| (header, id)));
            if !candidate.instance.statics.is_empty() {
                self.region_owners
                    .insert(candidate.instance.statics.address_range().start, id);
            }
            if candidate.image.charge_codegen_once() {
                self.compiled_functions += candidate.image.pipeline.functions_defined();
                self.compiled_code_bytes += candidate.image.pipeline.code_bytes();
            }
            let image_instance = candidate.image.image_instance_id();
            self.programs.insert(
                id,
                InstalledProgram {
                    program: ProgramCustody::Shared(candidate.image),
                    statics: Arc::clone(&candidate.instance.statics),
                    instance: candidate.instance,
                    roots: candidate.roots,
                    environment: candidate.environment,
                    owned_headers,
                    sites: candidate.sites,
                },
            );
            tracing::info!(target: "tidepool_codegen::image_install", image_instance, process_id = std::process::id(), machine_owner = self as *const Self as usize, program = id.0, outcome = "machine_published", "native image install");
            ids.push(id);
        }
        let mut source_attachments = Vec::new();
        let leases = requests
            .into_iter()
            .zip(handles)
            .map(|(request, handle)| {
                let installed = &self.programs[&ids[request.group]];
                let export = &installed.program.get().top_exports[&request.binding];
                let lease = SourceInstanceLease::new(
                    GroupInstanceId::from(ids[request.group]),
                    request.owner,
                    request.original_ordinal,
                    request.binder,
                    request.binding,
                    handle,
                    export.entry_signature.clone(),
                );
                source_attachments.push(super::super::SourceInstanceAttachment::installed(
                    request.domain,
                    lease.clone(),
                ));
                lease
            })
            .collect();
        Ok(BatchInstallReceipt {
            programs: ids,
            leases,
            source_attachments,
        })
    }

    fn stage_batch_leases(
        &mut self,
        candidates: &[Candidate],
        requests: &[BatchLeaseRequest],
        provisional: &mut Vec<PreparedHandle>,
    ) -> Result<(), ExecutionError> {
        for request in requests {
            let candidate = &candidates[request.group];
            let export = &candidate.image.top_exports[&request.binding];
            let slot = candidate.image.top_slots[&request.binding];
            let word = candidate
                .roots
                .read(slot)
                .map_err(|cause| runtime_error(&self.machine, cause))?;
            if word == 0 {
                return Err(ExecutionError::MissingEntry(request.binding));
            }
            // Candidate objects are fully initialized. No collection runs
            // before commit or rollback, and rollback releases this root.
            let root = crate::old_space::OwnedRootCell::new(&self.machine, word as *mut u8)
                .map_err(|cause| runtime_error(&self.machine, cause))?;
            let sites = candidate.sites.values().flatten().copied().collect();
            let raw = self
                .handles
                .insert_handle_with_sites(root, RealmId::ROOT, export.rep, sites);
            provisional.push(PreparedHandle {
                raw,
                rep: export.rep,
            });
        }
        Ok(())
    }

    fn stage_batch(
        &mut self,
        candidates: &[Candidate],
        external: &[(usize, usize, PreparedHandle)],
        heap_reserve: usize,
        candidate_alloc_start: &mut Option<*mut u8>,
    ) -> Result<(), ExecutionError> {
        let first = self.machine.gc_active_range().is_none();
        for (index, candidate) in candidates.iter().enumerate() {
            if first && index == 0 {
                let nursery = try_words(
                    self.nursery_bytes
                        .max(heap_reserve)
                        .div_ceil(std::mem::size_of::<u64>()),
                )?;
                self.machine
                    .set_stack_map_registry(&candidate.image.pipeline.stack_maps);
                self.machine
                    .install_prepared_buffer_with_static_region(
                        nursery,
                        candidate.instance.descriptors.clone(),
                        Some(Arc::clone(&candidate.instance.statics)),
                    )
                    .map_err(|error| runtime_error(&self.machine, error))?;
            } else {
                self.machine
                    .push_stack_map_registry(&candidate.image.pipeline.stack_maps);
                self.machine
                    .extend_prepared_descriptors(
                        candidate.instance.descriptors.clone(),
                        Arc::clone(&candidate.instance.statics),
                    )
                    .map_err(|error| runtime_error(&self.machine, error))?;
            }
        }
        if !first {
            collect_on(
                &self.machine,
                &mut self.vmctx,
                &self.old_space,
                heap_reserve,
            )?;
        }
        let (start, size) = self
            .machine
            .gc_active_range()
            .ok_or(ExecutionError::Invariant(
                "batch installed no active nursery",
            ))?;
        let cursor = if first {
            0
        } else {
            (self.vmctx.alloc_ptr as usize)
                .checked_sub(start as usize)
                .filter(|cursor| *cursor <= size)
                .ok_or(ExecutionError::Invariant("batch nursery cursor is invalid"))?
        };
        if heap_reserve > size - cursor {
            return Err(runtime_error(&self.machine, RuntimeError::HeapOverflow));
        }
        let base = unsafe { start.add(cursor) };
        *candidate_alloc_start = Some(base);
        let mut offsets = Vec::with_capacity(candidates.len());
        let mut used = 0usize;
        for candidate in candidates {
            offsets.push(used);
            let mut local = 0usize;
            let heap_tops: BTreeMap<_, _> = candidate
                .instance
                .heap_top_specs
                .iter()
                .map(|spec| {
                    let offset = local;
                    local += spec.descriptor.allocation_extent() as usize;
                    (spec.id, (offset, &spec.descriptor))
                })
                .collect();
            for (&id, &slot) in &candidate.image.top_slots {
                let address = if let Some(&(offset, descriptor)) = heap_tops.get(&id) {
                    (unsafe { base.add(used + offset) } as usize) + usize::from(descriptor.tag())
                } else {
                    candidate
                        .instance
                        .statics
                        .entry(id)
                        .or_else(|| {
                            candidate
                                .image
                                .byte_tops
                                .get(&id)
                                .map(|v| v.as_ptr() as usize)
                        })
                        .ok_or(ExecutionError::MissingEntry(id))?
                };
                candidate.roots.write(slot, address as u64)?;
            }
            used += candidate.heap_extent;
        }
        for &(index, slot, handle) in external {
            let current = self
                .handles
                .handle(handle.raw)
                .ok_or(ExecutionError::UnknownPreparedHandle)?;
            candidates[index]
                .roots
                .write(slot, unsafe { current.slot.current() } as u64)?;
        }
        for candidate in candidates {
            for (slot, origin) in candidate.image.import_slots.iter().zip(&candidate.imports) {
                if let BatchImport::Source { group, binding } = origin {
                    if let Some(literal) = &slot.literal {
                        candidate.roots.write(slot.slot, literal.as_ptr() as u64)?;
                        continue;
                    }
                    let target = &candidates[*group];
                    let top = target.image.top_slots[binding];
                    candidate.roots.write(
                        slot.slot,
                        target
                            .roots
                            .read(top)
                            .map_err(|error| runtime_error(&self.machine, error))?,
                    )?;
                }
            }
        }
        for (index, candidate) in candidates.iter().enumerate() {
            let object_start = unsafe { base.add(offsets[index]) };
            initialize_heap_tops(
                object_start,
                candidate.heap_extent,
                &candidate.instance.heap_top_specs,
                &candidate.image.top_slots,
                &candidate.roots,
                &candidate.instance.statics,
                &candidate.image.byte_tops,
                &candidate.image.bytes,
                &candidate.image.import_slots,
            )
            .map_err(|error| runtime_error(&self.machine, error))?;
        }
        self.vmctx.alloc_ptr = unsafe { base.add(heap_reserve) };
        if first {
            self.vmctx.alloc_limit = unsafe { start.add(size) };
            self.vmctx.machine_state = Rc::as_ptr(&self.machine).cast_mut();
        }

        for candidate in candidates {
            for slot in candidate.image.reference_slots() {
                let root = candidate
                    .roots
                    .slot_address(slot)
                    .ok_or(ExecutionError::Invariant("batch root slot is invalid"))?;
                self.machine.register_persistent_root(root);
            }
        }

        Ok(())
    }

    /// No fallible work remains after catalog acquisition. In particular,
    /// raw code/environment pointers must not enter the dispatch table while
    /// an installation error can still drop their owners.
    fn commit_batch_metadata(&mut self, candidates: &[Candidate]) {
        for candidate in candidates {
            self.commit_install_metadata(
                &candidate.image,
                &candidate.instance,
                &candidate.environment,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prepared_program::{GroupInventory, ImageRegistry};
    use tidepool_repr::execution_schema::{
        testing, Atom, CachedHomeOwner, CertifiedGroup, CheckedLayout, ConstructorDecl,
        ConstructorId, ExprFrame, FieldLayout, GlobalDecl, GlobalId, Group, HeapBinding, HeapRhs,
        ImportOwner, ModuleVersion, Signature, SignatureId, TopBinding, UpdatePolicy, ValueRef,
    };

    fn group(name: &str, ordinal: u32, other: &str) -> Arc<CompiledProgram> {
        let mut wire = testing::wire_program();
        wire.signatures[0].results = ResultContract::Returns(vec![RuntimeRep::LiftedRef]);
        wire.expressions.nodes[0] =
            ExprFrame::Return(vec![Atom::Ref(ValueRef::Global(GlobalId(0)))]);
        if let tidepool_repr::execution_schema::Group::NonRecursive(top) = &mut wire.bindings[0] {
            top.identity = testing::identity("Fixture", name);
        }
        let imported = testing::identity("Fixture", other);
        wire.globals.push(GlobalDecl {
            identity: imported.clone(),
            rep: RuntimeRep::LiftedRef,
            entry_signature: Some(SignatureId(0)),
            required_evaluated: false,
            required_generation: None,
        });
        let group = CertifiedGroup::admit(
            CachedHomeOwner {
                unit: "fixture".into(),
                module: "Fixture".into(),
                module_version: ModuleVersion([1; 32]),
                skinny_iface_sha256: [2; 32],
                product_sha256: [3; 32],
            },
            testing::projected_group(wire, ordinal).unwrap(),
            vec![ImportOwner::Source {
                version: ModuleVersion([1; 32]),
                binder: imported,
            }],
        )
        .unwrap();
        Arc::new(CompiledProgram::compile_certified_group(&group).unwrap())
    }

    fn cyclic_heap_top(name: &str, ordinal: u32, other: &str) -> Arc<CompiledProgram> {
        let mut wire = testing::wire_program();
        wire.constructors.push(ConstructorDecl {
            identity: testing::identity("Cycle", "Node"),
            family: testing::identity("Cycle", "Node"),
            host_id: tidepool_repr::DataConId(90_003),
            result_rep: RuntimeRep::LiftedRef,
            field_reps: vec![RuntimeRep::LiftedRef],
            strict_fields: vec![false],
            layout: CheckedLayout {
                fields: vec![FieldLayout {
                    rep: RuntimeRep::LiftedRef,
                    offset: 0,
                }],
                alignment: 8,
                payload_size: 8,
                root_mask: vec![true],
            },
            tag: 1,
            family_size: 1,
        });
        wire.expressions.nodes.clear();
        if let tidepool_repr::execution_schema::Group::NonRecursive(top) = &mut wire.bindings[0] {
            top.identity = testing::identity("Fixture", name);
            top.binding.rhs = HeapRhs::Constructor {
                constructor: ConstructorId(0),
                fields: vec![Atom::Ref(ValueRef::Global(GlobalId(0)))],
            };
        }
        let imported = testing::identity("Fixture", other);
        wire.globals.push(GlobalDecl {
            identity: imported.clone(),
            rep: RuntimeRep::LiftedRef,
            entry_signature: None,
            required_evaluated: false,
            required_generation: None,
        });
        let certified = CertifiedGroup::admit(
            CachedHomeOwner {
                unit: "fixture".into(),
                module: "Fixture".into(),
                module_version: ModuleVersion([1; 32]),
                skinny_iface_sha256: [2; 32],
                product_sha256: [3; 32],
            },
            testing::projected_group(wire, ordinal).unwrap(),
            vec![ImportOwner::Source {
                version: ModuleVersion([1; 32]),
                binder: imported,
            }],
        )
        .unwrap();
        Arc::new(CompiledProgram::compile_certified_group(&certified).unwrap())
    }

    fn static_group() -> Arc<CompiledProgram> {
        let mut wire = testing::wire_program();
        wire.constructors.push(ConstructorDecl {
            identity: testing::identity("Static", "Tag"),
            family: testing::identity("Static", "Tag"),
            host_id: tidepool_repr::DataConId(90_001),
            result_rep: RuntimeRep::LiftedRef,
            field_reps: vec![],
            strict_fields: vec![],
            layout: CheckedLayout {
                fields: vec![],
                alignment: 1,
                payload_size: 0,
                root_mask: vec![],
            },
            tag: 1,
            family_size: 1,
        });
        if let tidepool_repr::execution_schema::Group::NonRecursive(top) = &mut wire.bindings[0] {
            top.identity = testing::identity("Fixture", "static");
            top.binding.rhs = HeapRhs::Constructor {
                constructor: ConstructorId(0),
                fields: vec![],
            };
        }
        wire.expressions.nodes.clear();
        let group = CertifiedGroup::admit(
            CachedHomeOwner {
                unit: "fixture".into(),
                module: "Fixture".into(),
                module_version: ModuleVersion([1; 32]),
                skinny_iface_sha256: [2; 32],
                product_sha256: [3; 32],
            },
            testing::projected_group(wire, 11).unwrap(),
            vec![],
        )
        .unwrap();
        Arc::new(CompiledProgram::compile_certified_group(&group).unwrap())
    }

    fn caf_group() -> Arc<CompiledProgram> {
        Arc::new(CompiledProgram::compile_certified_group(&certified_caf_group()).unwrap())
    }

    fn certified_caf_group() -> CertifiedGroup {
        let mut wire = testing::wire_program();
        wire.signatures[0].results = ResultContract::Returns(vec![RuntimeRep::LiftedRef]);
        wire.constructors.push(ConstructorDecl {
            identity: testing::identity("Caf", "Value"),
            family: testing::identity("Caf", "Value"),
            host_id: tidepool_repr::DataConId(90_002),
            result_rep: RuntimeRep::LiftedRef,
            field_reps: vec![],
            strict_fields: vec![],
            layout: CheckedLayout {
                fields: vec![],
                alignment: 1,
                payload_size: 0,
                root_mask: vec![],
            },
            tag: 1,
            family_size: 1,
        });
        wire.expressions.nodes[0] = ExprFrame::Construct {
            constructor: ConstructorId(0),
            fields: vec![],
        };
        if let tidepool_repr::execution_schema::Group::NonRecursive(top) = &mut wire.bindings[0] {
            top.identity = testing::identity("Fixture", "caf");
            top.binding.rhs = HeapRhs::Thunk {
                signature: SignatureId(0),
                update: UpdatePolicy::Memoize,
                captures: vec![],
                body: 0,
            };
        }
        CertifiedGroup::admit(
            CachedHomeOwner {
                unit: "fixture".into(),
                module: "Fixture".into(),
                module_version: ModuleVersion([1; 32]),
                skinny_iface_sha256: [2; 32],
                product_sha256: [3; 32],
            },
            testing::projected_group(wire, 12).unwrap(),
            vec![],
        )
        .unwrap()
    }

    #[test]
    fn batch_literal_tops_survive_promotion_and_retirement() {
        const LITERAL: &[u8] = b"batch literal\0";
        let mut producer = testing::wire_program();
        producer.signatures[0].results = ResultContract::Returns(vec![RuntimeRep::LiftedRef]);
        producer.constructors.push(ConstructorDecl {
            identity: testing::identity("Literal", "Value"),
            family: testing::identity("Literal", "Value"),
            host_id: tidepool_repr::DataConId(90_004),
            result_rep: RuntimeRep::LiftedRef,
            field_reps: vec![],
            strict_fields: vec![],
            layout: CheckedLayout {
                fields: vec![],
                alignment: 1,
                payload_size: 0,
                root_mask: vec![],
            },
            tag: 1,
            family_size: 1,
        });
        producer.expressions.nodes[0] = ExprFrame::Construct {
            constructor: ConstructorId(0),
            fields: vec![],
        };
        producer.bindings.push(Group::NonRecursive(TopBinding {
            identity: testing::identity("Fixture", "literal"),
            binding: HeapBinding {
                id: ValueId(1),
                rhs: HeapRhs::Bytes(LITERAL.to_vec()),
            },
        }));
        producer.signatures.push(Signature {
            arguments: vec![],
            results: ResultContract::Returns(vec![RuntimeRep::Address]),
        });
        producer
            .expressions
            .nodes
            .push(ExprFrame::Return(vec![Atom::Ref(ValueRef::Local(
                ValueId(1),
            ))]));
        producer.bindings.push(Group::NonRecursive(TopBinding {
            identity: testing::identity("Fixture", "readLiteral"),
            binding: HeapBinding {
                id: ValueId(2),
                rhs: HeapRhs::Function {
                    signature: SignatureId(1),
                    parameters: vec![],
                    captures: vec![],
                    body: 1,
                },
            },
        }));
        let producer = Arc::new(
            CompiledProgram::compile_prepared_definitions(&testing::prepare(producer).unwrap())
                .unwrap(),
        );
        let mut machine = PreparedMachine::empty(PreparedMachineOptions {
            nursery_bytes: 4096,
        })
        .unwrap();
        let ids = machine
            .install_shared_batch(vec![BatchProgram {
                image: producer,
                imports: vec![],
            }])
            .unwrap();
        machine.pin(ids[0]).unwrap();
        let call = PreparedCallOptions {
            observation_budget: 0,
            collect_before_observation: false,
        };
        // The result starts in the nursery. Retention promotes it and fixes
        // sibling roots while the byte top is live.
        let result = machine
            .run_entry_retained(ids[0], ValueId(0), &[], call, RealmId::ROOT)
            .unwrap();
        let [PreparedResult::Managed(value)] = result.values.as_slice() else {
            panic!("producer must return a managed constructor");
        };
        assert!(matches!(
            machine.inspect_outer(*value, RealmId::ROOT).unwrap(),
            PreparedOuter::Constructor { .. }
        ));
        assert!(machine.release(*value));
        let literal_before = machine
            .run_entry_retained(ids[0], ValueId(2), &[], call, RealmId::ROOT)
            .unwrap();
        let [PreparedResult::Scalar(address)] = literal_before.values.as_slice() else {
            panic!("accessor must return the raw literal address");
        };
        let address = *address;
        let retired = machine.collect_major(machine.quiesce().unwrap()).unwrap();
        assert!(retired.programs.is_empty());
        let literal_after = machine
            .run_entry_retained(ids[0], ValueId(2), &[], call, RealmId::ROOT)
            .unwrap();
        assert_eq!(literal_after.values, vec![PreparedResult::Scalar(address)]);
        assert!(machine.unpin(ids[0]));
        let retired = machine.collect_major(machine.quiesce().unwrap()).unwrap();
        assert_eq!(retired.programs, vec![ids[0]]);
        // SAFETY: this address came from the admitted Bytes top; the machine's
        // permanent literal pool retains its allocation after producer retirement.
        assert_eq!(
            unsafe { std::slice::from_raw_parts(address as *const u8, LITERAL.len()) },
            LITERAL
        );
        assert_eq!(machine.residency().programs, 0);
    }

    #[test]
    fn cyclic_batch_installs_atomically_and_retires_after_final_pin() {
        let a = group("a", 2, "b");
        let b = group("b", 7, "a");
        let mut machine = PreparedMachine::empty(PreparedMachineOptions {
            nursery_bytes: 4096,
        })
        .unwrap();
        let batch = || {
            vec![
                BatchProgram {
                    image: Arc::clone(&a),
                    imports: vec![BatchImport::Source {
                        group: 1,
                        binding: ValueId(0),
                    }],
                },
                BatchProgram {
                    image: Arc::clone(&b),
                    imports: vec![BatchImport::Source {
                        group: 0,
                        binding: ValueId(0),
                    }],
                },
            ]
        };
        let ids = machine.install_shared_batch(batch()).unwrap();
        assert_eq!(ids.len(), 2);
        let second = machine.install_shared_batch(batch()).unwrap();
        let first_b = machine.retain_top(ids[1], ValueId(0)).unwrap();
        let second_b = machine.retain_top(second[1], ValueId(0)).unwrap();
        assert_ne!(
            machine.handle_current_pointer(first_b),
            machine.handle_current_pointer(second_b)
        );
        let call = PreparedCallOptions {
            observation_budget: 0,
            collect_before_observation: true,
        };
        let first_a = machine
            .run_entry_retained(ids[0], ValueId(0), &[], call, RealmId::ROOT)
            .unwrap();
        let [PreparedResult::Managed(returned_b)] = first_a.values.as_slice() else {
            panic!("group a must return imported group b's closure");
        };
        assert_eq!(
            machine.handle_current_pointer(*returned_b),
            machine.handle_current_pointer(first_b)
        );
        assert!(machine.release(*returned_b));
        let second_a = machine
            .run_entry_retained(second[0], ValueId(0), &[], call, RealmId::ROOT)
            .unwrap();
        let [PreparedResult::Managed(returned_second_b)] = second_a.values.as_slice() else {
            panic!("second group a must return its own group b's closure");
        };
        assert_eq!(
            machine.handle_current_pointer(*returned_second_b),
            machine.handle_current_pointer(second_b)
        );
        assert!(machine.release(*returned_second_b));
        assert!(machine.release(first_b));
        assert!(machine.release(second_b));
        machine.pin(ids[0]).unwrap();
        let first = machine.collect_major(machine.quiesce().unwrap()).unwrap();
        assert_eq!(first.programs.len(), 2);
        assert_eq!(machine.residency().programs, 2);
        assert!(machine.unpin(ids[0]));
        let last = machine.collect_major(machine.quiesce().unwrap()).unwrap();
        assert_eq!(last.programs.len(), 2);
        assert_eq!(machine.residency().programs, 0);
    }

    #[test]
    fn cross_group_heap_top_cycle_survives_collection_and_retires() {
        let a = cyclic_heap_top("a", 21, "b");
        let b = cyclic_heap_top("b", 22, "a");
        let mut machine = PreparedMachine::empty(PreparedMachineOptions {
            nursery_bytes: 4096,
        })
        .unwrap();
        let ids = machine
            .install_shared_batch(vec![
                BatchProgram {
                    image: a,
                    imports: vec![BatchImport::Source {
                        group: 1,
                        binding: ValueId(0),
                    }],
                },
                BatchProgram {
                    image: b,
                    imports: vec![BatchImport::Source {
                        group: 0,
                        binding: ValueId(0),
                    }],
                },
            ])
            .unwrap();
        let first = machine.retain_top(ids[0], ValueId(0)).unwrap();
        let second = machine.retain_top(ids[1], ValueId(0)).unwrap();
        for id in &ids {
            machine.pin(*id).unwrap();
        }
        let first_collection = machine.collect_major(machine.quiesce().unwrap()).unwrap();
        assert!(first_collection.programs.is_empty(), "{first_collection:?}");
        assert_eq!(machine.residency().programs, 2);
        let PreparedOuter::Constructor {
            fields: first_fields,
            ..
        } = machine.inspect_outer(first, RealmId::ROOT).unwrap();
        let [PreparedResult::Managed(first_peer)] = first_fields.as_slice() else {
            panic!("first heap top must point to its peer");
        };
        assert_eq!(
            machine.handle_current_pointer(*first_peer),
            machine.handle_current_pointer(second)
        );
        let PreparedOuter::Constructor {
            fields: second_fields,
            ..
        } = machine.inspect_outer(second, RealmId::ROOT).unwrap();
        let [PreparedResult::Managed(second_peer)] = second_fields.as_slice() else {
            panic!("second heap top must point back to its peer");
        };
        assert_eq!(
            machine.handle_current_pointer(*second_peer),
            machine.handle_current_pointer(first)
        );
        for handle in [*first_peer, *second_peer, first, second] {
            assert!(machine.release(handle));
        }
        assert_eq!(machine.residency().programs, 2);
        for id in &ids {
            assert!(machine.unpin(*id));
        }
        let retired = machine.collect_major(machine.quiesce().unwrap()).unwrap();
        assert_eq!(retired.programs.len(), 2);
        assert_eq!(machine.residency().programs, 0);
    }

    #[test]
    fn target_and_reachable_source_group_share_one_atomic_install() {
        let groups = [certified_caf_group()];
        let binder = SourceBinder {
            version: ModuleVersion([1; 32]),
            binder: testing::identity("Fixture", "caf"),
        };
        let demand = GroupInventory::new(&groups)
            .unwrap()
            .seal([binder.clone()])
            .unwrap();
        let registry = ImageRegistry::new();
        let images = demand.compile(&registry).unwrap();
        let mut wire = testing::wire_program();
        wire.signatures[0].results = ResultContract::Returns(vec![RuntimeRep::LiftedRef]);
        wire.expressions.nodes[0] =
            ExprFrame::Return(vec![Atom::Ref(ValueRef::Global(GlobalId(0)))]);
        wire.globals.push(GlobalDecl {
            identity: binder.binder.clone(),
            rep: RuntimeRep::LiftedRef,
            entry_signature: None,
            required_evaluated: false,
            required_generation: None,
        });
        let prepared = testing::prepare(wire).unwrap();
        let target = registry
            .get_or_compile_prepared(&prepared, || {
                CompiledProgram::compile_prepared_definitions(&prepared).map(Arc::new)
            })
            .unwrap();
        let lease = BatchLeaseRequest::for_demanded(0, &images[0], &binder).unwrap();
        let mut machine = PreparedMachine::empty(PreparedMachineOptions {
            nursery_bytes: 4096,
        })
        .unwrap();
        let installed = machine
            .install_shared_batch_with_leases(
                vec![
                    BatchProgram {
                        image: Arc::clone(images[0].image()),
                        imports: vec![],
                    },
                    BatchProgram {
                        image: target,
                        imports: vec![BatchImport::Source {
                            group: 0,
                            binding: ValueId(0),
                        }],
                    },
                ],
                vec![lease],
            )
            .unwrap();
        assert_eq!(installed.programs.len(), 2);
        let [source_lease] = installed.leases.as_slice() else {
            panic!("one requested source binder must be materialized");
        };
        assert_eq!(source_lease.instance().program(), installed.programs[0]);
        assert_eq!(source_lease.binder(), &binder);
        let target_result = machine
            .run_entry_retained(
                installed.programs[1],
                prepared.entry(),
                &[],
                PreparedCallOptions {
                    observation_budget: 0,
                    collect_before_observation: false,
                },
                RealmId::ROOT,
            )
            .unwrap();
        let [PreparedResult::Managed(returned)] = target_result.values.as_slice() else {
            panic!("target must return its certified source CAF");
        };
        assert_eq!(
            machine.handle_current_pointer(*returned),
            machine.handle_current_pointer(source_lease.handle())
        );
        assert!(machine.release(*returned));
        assert!(machine.release(source_lease.handle()));
    }

    #[test]
    fn bad_source_edge_keeps_existing_machine_unchanged() {
        let image = group("a", 2, "b");
        let mut machine = PreparedMachine::empty(PreparedMachineOptions {
            nursery_bytes: 4096,
        })
        .unwrap();
        let before = machine.residency();
        assert!(machine
            .install_shared_batch(vec![BatchProgram {
                image,
                imports: vec![BatchImport::Source {
                    group: 1,
                    binding: ValueId(0),
                }],
            }])
            .is_err());
        assert_eq!(machine.residency(), before);
    }

    #[test]
    fn source_signature_mismatch_rejects_entire_batch() {
        let mut a = group("a", 2, "b");
        let b = group("b", 7, "a");
        Arc::get_mut(&mut a).unwrap().import_slots[0].entry_signature = Some(Signature {
            arguments: vec![],
            results: ResultContract::Returns(vec![RuntimeRep::Int(64)]),
        });
        let mut machine = PreparedMachine::empty(PreparedMachineOptions {
            nursery_bytes: 4096,
        })
        .unwrap();
        let result = machine.install_shared_batch(vec![
            BatchProgram {
                image: a,
                imports: vec![BatchImport::Source {
                    group: 1,
                    binding: ValueId(0),
                }],
            },
            BatchProgram {
                image: b,
                imports: vec![BatchImport::Source {
                    group: 0,
                    binding: ValueId(0),
                }],
            },
        ]);
        let Err(ExecutionError::BatchImportContract(evidence)) = result else {
            panic!("wrong source signature must refuse the whole batch");
        };
        assert_eq!((evidence.program, evidence.import_position), (0, 0));
        assert_eq!(
            evidence.required_identity,
            testing::identity("Fixture", "b")
        );
        assert_eq!(
            evidence.offered_identity,
            Some(evidence.required_identity.clone())
        );
        assert_eq!(
            evidence.required_signature,
            Some(Signature {
                arguments: vec![],
                results: ResultContract::Returns(vec![RuntimeRep::Int(64)]),
            })
        );
        assert_eq!(
            evidence.offered_signature,
            Some(Signature {
                arguments: vec![],
                results: ResultContract::Returns(vec![RuntimeRep::LiftedRef]),
            })
        );
        assert!(matches!(
            evidence.selected,
            super::super::super::BatchImportSelection::Source {
                group: 1,
                binding: ValueId(0)
            }
        ));
        assert_eq!(evidence.owner, None);
        assert_eq!(machine.residency(), ResidencyCounts::default());
    }

    #[test]
    fn batch_copies_static_regions_per_installation() {
        let image = static_group();
        let mut machine = PreparedMachine::empty(PreparedMachineOptions {
            nursery_bytes: 4096,
        })
        .unwrap();
        let first = machine
            .install_shared_batch(vec![BatchProgram {
                image: Arc::clone(&image),
                imports: vec![],
            }])
            .unwrap()[0];
        let second = machine
            .install_shared_batch(vec![BatchProgram {
                image,
                imports: vec![],
            }])
            .unwrap()[0];
        let first_top = machine.retain_top(first, ValueId(0)).unwrap();
        let second_top = machine.retain_top(second, ValueId(0)).unwrap();
        assert_ne!(
            machine.handle_current_pointer(first_top),
            machine.handle_current_pointer(second_top)
        );
        assert!(machine.release(first_top));
        assert!(machine.release(second_top));
        let retired = machine.collect_major(machine.quiesce().unwrap()).unwrap();
        assert_eq!(retired.programs.len(), 2);
        assert_eq!(machine.residency().programs, 0);
    }

    #[test]
    fn collection_after_final_program_retirement_keeps_machine_reusable() {
        let image = static_group();
        let mut machine = PreparedMachine::empty(PreparedMachineOptions {
            nursery_bytes: 4096,
        })
        .unwrap();
        machine
            .install_shared_batch(vec![BatchProgram {
                image: Arc::clone(&image),
                imports: vec![],
            }])
            .unwrap();
        assert_eq!(
            machine
                .collect_major(machine.quiesce().unwrap())
                .unwrap()
                .programs
                .len(),
            1
        );
        assert_eq!(machine.machine.stack_map_link_count(), 0);
        assert!(machine
            .collect_major(machine.quiesce().unwrap())
            .unwrap()
            .programs
            .is_empty());
        assert_eq!(machine.machine.stack_map_link_count(), 0);
        let second = machine
            .install_shared_batch(vec![BatchProgram {
                image,
                imports: vec![],
            }])
            .unwrap();
        assert_eq!(second.len(), 1);
        assert_eq!(machine.residency().programs, 1);
    }

    #[test]
    fn batch_installs_independent_mutable_cafs() {
        let image = caf_group();
        let mut machine = PreparedMachine::empty(PreparedMachineOptions {
            nursery_bytes: 4096,
        })
        .unwrap();
        let first = machine
            .install_shared_batch(vec![BatchProgram {
                image: Arc::clone(&image),
                imports: vec![],
            }])
            .unwrap()[0];
        let second = machine
            .install_shared_batch(vec![BatchProgram {
                image,
                imports: vec![],
            }])
            .unwrap()[0];
        let first_caf = machine.retain_top(first, ValueId(0)).unwrap();
        let second_caf = machine.retain_top(second, ValueId(0)).unwrap();
        assert_ne!(
            machine.handle_current_pointer(first_caf),
            machine.handle_current_pointer(second_caf)
        );
        let call = PreparedCallOptions {
            observation_budget: 0,
            collect_before_observation: false,
        };
        let first_value = machine
            .run_entry_retained(first, ValueId(0), &[], call, RealmId::ROOT)
            .unwrap();
        let second_value = machine
            .run_entry_retained(second, ValueId(0), &[], call, RealmId::ROOT)
            .unwrap();
        let [PreparedResult::Managed(first_result)] = first_value.values.as_slice() else {
            panic!("first CAF must return a managed constructor");
        };
        let [PreparedResult::Managed(second_result)] = second_value.values.as_slice() else {
            panic!("second CAF must return a managed constructor");
        };
        assert_ne!(
            machine.handle_current_pointer(*first_result),
            machine.handle_current_pointer(*second_result)
        );
        assert!(machine.release(*first_result));
        assert!(machine.release(*second_result));
        assert!(machine.release(first_caf));
        assert!(machine.release(second_caf));
        let retired = machine.collect_major(machine.quiesce().unwrap()).unwrap();
        assert_eq!(retired.programs.len(), 2);
    }

    #[test]
    fn late_failure_releases_provisional_source_lease() {
        let groups = [certified_caf_group()];
        let demand = GroupInventory::new(&groups)
            .unwrap()
            .seal([SourceBinder {
                version: ModuleVersion([1; 32]),
                binder: testing::identity("Fixture", "caf"),
            }])
            .unwrap();
        let images = demand.compile(&ImageRegistry::new()).unwrap();
        let selected = &images[0];
        let image = Arc::clone(selected.image());
        let request = || {
            BatchLeaseRequest::for_demanded(
                0,
                selected,
                &SourceBinder {
                    version: ModuleVersion([1; 32]),
                    binder: testing::identity("Fixture", "caf"),
                },
            )
            .unwrap()
        };
        let batch = || {
            vec![BatchProgram {
                image: Arc::clone(&image),
                imports: vec![],
            }]
        };
        let mut machine = PreparedMachine::empty(PreparedMachineOptions {
            nursery_bytes: 4096,
        })
        .unwrap();
        let mut mislabeled = request();
        mislabeled.owner.product_sha256 = [9; 32];
        assert!(matches!(
            machine.install_shared_batch_with_leases(batch(), vec![mislabeled]),
            Err(ExecutionError::BatchSourceContract(_))
        ));
        assert_eq!(machine.residency(), ResidencyCounts::default());
        machine.fail_catalog_after_stage = true;
        assert!(machine
            .install_shared_batch_with_leases(batch(), vec![request()])
            .is_err());
        assert_eq!(machine.residency(), ResidencyCounts::default());
        assert_eq!(machine.handle_count(), 0);
        assert_eq!(machine.machine.root_cell_allocation_counts(), (0, 1));
        assert!(machine.descriptor_registry.is_empty());

        machine.fail_catalog_after_stage = false;
        let installed = machine
            .install_shared_batch_with_leases(batch(), vec![request()])
            .unwrap();
        assert_eq!(installed.programs.len(), 1);
        assert_eq!(installed.leases.len(), 1);
        assert_eq!(
            installed.leases[0].instance().program(),
            installed.programs[0]
        );
        assert_eq!(machine.handle_count(), 1);
        machine.collect_major(machine.quiesce().unwrap()).unwrap();
        assert_eq!(machine.residency().programs, 1);
        machine.fail_catalog_after_stage = true;
        assert!(machine
            .install_shared_batch_with_leases(batch(), vec![request()])
            .is_err());
        assert_eq!(machine.handle_count(), 1);
        assert_eq!(machine.machine.root_cell_allocation_counts(), (1, 3));
        assert_eq!(machine.residency().programs, 1);
        machine.fail_catalog_after_stage = false;
        assert!(machine.release(installed.leases[0].handle()));
        assert_eq!(machine.machine.root_cell_allocation_counts(), (0, 3));
        let retired = machine.collect_major(machine.quiesce().unwrap()).unwrap();
        assert_eq!(retired.programs, installed.programs);
    }

    #[test]
    fn late_catalog_failure_rolls_back_batch_and_single_install() {
        let a = group("a", 2, "b");
        let b = group("b", 7, "a");
        let mut machine = PreparedMachine::empty(PreparedMachineOptions {
            nursery_bytes: 4096,
        })
        .unwrap();
        machine.fail_catalog_after_stage = true;
        let batch = || {
            vec![
                BatchProgram {
                    image: Arc::clone(&a),
                    imports: vec![BatchImport::Source {
                        group: 1,
                        binding: ValueId(0),
                    }],
                },
                BatchProgram {
                    image: Arc::clone(&b),
                    imports: vec![BatchImport::Source {
                        group: 0,
                        binding: ValueId(0),
                    }],
                },
            ]
        };
        assert!(machine.install_shared_batch(batch()).is_err());
        assert_eq!(machine.residency(), ResidencyCounts::default());
        assert!(machine.descriptor_registry.is_empty());
        machine.fail_catalog_after_stage = false;
        let ids = machine.install_shared_batch(batch()).unwrap();
        machine.pin(ids[0]).unwrap();
        let before = machine.residency();
        machine.fail_catalog_after_stage = true;
        assert!(machine.install_shared_batch(batch()).is_err());
        assert_eq!(machine.residency(), before);
        machine.collect_major(machine.quiesce().unwrap()).unwrap();
        assert_eq!(machine.residency().programs, 2);
        machine.fail_catalog_after_stage = false;

        let prepared = testing::prepare(testing::wire_program()).unwrap();
        let linked = tidepool_repr::execution_schema::link_program(
            prepared,
            &tidepool_repr::execution_schema::MachineImports::default(),
        )
        .unwrap();
        let single = Arc::new(CompiledProgram::compile(&linked).unwrap());
        machine.fail_catalog_after_stage = true;
        let before = machine.residency();
        assert!(machine
            .install_shared(Arc::clone(&single), ImportBindings::new())
            .is_err());
        assert_eq!(machine.residency(), before);
        machine.collect_major(machine.quiesce().unwrap()).unwrap();
        machine.fail_catalog_after_stage = false;
        machine
            .install_shared(single, ImportBindings::new())
            .unwrap();
    }
}
