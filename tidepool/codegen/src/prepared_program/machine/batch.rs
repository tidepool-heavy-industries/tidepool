//! Atomic installation of mutually importing native group images.

use super::*;

/// One import slot's already admitted owner. `Source` selects a top in this
/// sealed batch; it never accepts a spelling-only match.
pub enum BatchImport {
    Existing(PreparedHandle),
    Source { group: usize, binding: ValueId },
}

pub struct BatchProgram {
    pub image: Arc<CompiledProgram>,
    /// In the image's declared GlobalId order.
    pub imports: Vec<BatchImport>,
}

struct Candidate {
    image: Arc<CompiledProgram>,
    instance: Arc<InstanceImage>,
    roots: RootWords,
    environment: Box<InstallationEnvironment>,
    imports: Vec<BatchImport>,
    heap_extent: usize,
}

impl PreparedMachine<'_> {
    /// Install a closed batch of independently compiled original groups as
    /// one transaction. Every candidate receives a fresh root block, static
    /// instance and mutable CAFs; source imports may point forward or form a
    /// cycle. No candidate is visible to execution or retirement until all
    /// source tops and import slots have valid pointers.
    pub fn install_shared_batch(
        &mut self,
        batch: Vec<BatchProgram>,
    ) -> Result<Vec<ProgramId>, ExecutionError> {
        if batch.is_empty() {
            return Ok(Vec::new());
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
            });
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
            for (slot, origin) in candidate.image.import_slots.iter().zip(&candidate.imports) {
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
                        if export.identity != slot.identity || export.rep != slot.rep {
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
                    }
                    BatchImport::Existing(handle) => {
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
            committed: false,
        };
        let staged = transaction
            .machine
            .stage_batch(&candidates, &external, heap_reserve);
        let catalog = if staged.is_ok() && transaction.machine.static_catalog.is_none() {
            transaction
                .machine
                .machine
                .prepared_static_catalog()
                .map(Some)
                .map_err(|error| runtime_error(&transaction.machine.machine, error))
        } else {
            Ok(None)
        };
        transaction.committed = staged.is_ok() && catalog.is_ok();
        drop(transaction);
        staged?;
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
            self.programs.insert(
                id,
                InstalledProgram {
                    program: ProgramCustody::Shared(candidate.image),
                    statics: Arc::clone(&candidate.instance.statics),
                    instance: candidate.instance,
                    roots: candidate.roots,
                    environment: candidate.environment,
                    owned_headers,
                },
            );
            ids.push(id);
        }
        Ok(ids)
    }

    fn stage_batch(
        &mut self,
        candidates: &[Candidate],
        external: &[(usize, usize, PreparedHandle)],
        heap_reserve: usize,
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
            for slot in candidate
                .image
                .top_slots
                .values()
                .copied()
                .chain(candidate.image.import_slots.iter().map(|slot| slot.slot))
            {
                let root = candidate
                    .roots
                    .slot_address(slot)
                    .ok_or(ExecutionError::Invariant("batch root slot is invalid"))?;
                self.machine.register_persistent_root(root);
            }
        }

        // Registration and metadata publication only after the entire graph
        // is initialized. Every image still has its own instance and roots.
        for candidate in candidates {
            let registry = &self.descriptor_registry;
            self.descriptors.extend(
                candidate
                    .instance
                    .descriptors
                    .iter()
                    .filter(|descriptor| !registry.contains_key(&descriptor.initial_header_word()))
                    .cloned(),
            );
            self.descriptor_registry.extend(
                candidate
                    .instance
                    .descriptor_registry
                    .iter()
                    .map(|(&header, metadata)| (header, metadata.clone())),
            );
            self.machine.register_prepared_constructors(
                candidate
                    .instance
                    .descriptor_registry
                    .iter()
                    .filter_map(|(&header, metadata)| match &metadata.meaning {
                        super::super::DescriptorMeaning::Constructor(observation) => {
                            Some((header, observation.identity))
                        }
                        _ => None,
                    }),
            );
            self.machine.register_prepared_entries(
                (&*candidate.environment as *const InstallationEnvironment).cast(),
                candidate.image.callables.iter().map(|callable| {
                    (
                        candidate.instance.header(callable.header),
                        callable.signature.clone(),
                        candidate.image.pipeline.get_function_ptr(callable.function),
                    )
                }),
                candidate
                    .image
                    .thunk_entries
                    .iter()
                    .map(|&(header, function)| {
                        (
                            candidate.instance.header(header),
                            candidate.image.pipeline.get_function_ptr(function),
                        )
                    }),
            );
            self.machine.absorb_interned_bytes(&candidate.image.bytes);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tidepool_repr::execution_schema::{
        testing, CachedHomeOwner, CertifiedGroup, GlobalDecl, ImportOwner, ModuleVersion,
    };

    fn group(name: &str, ordinal: u32, other: &str) -> Arc<CompiledProgram> {
        let mut wire = testing::wire_program();
        if let tidepool_repr::execution_schema::Group::NonRecursive(top) = &mut wire.bindings[0] {
            top.identity = testing::identity("Fixture", name);
        }
        let imported = testing::identity("Fixture", other);
        wire.globals.push(GlobalDecl {
            identity: imported.clone(),
            rep: RuntimeRep::LiftedRef,
            entry_signature: None,
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

    #[test]
    fn cyclic_batch_installs_atomically_and_retires_after_final_pin() {
        let a = group("a", 2, "b");
        let b = group("b", 7, "a");
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
        assert_eq!(ids.len(), 2);
        machine.pin(ids[0]).unwrap();
        let first = machine.collect_major(machine.quiesce().unwrap()).unwrap();
        assert!(first.programs.is_empty());
        assert_eq!(machine.residency().programs, 2);
        assert!(machine.unpin(ids[0]));
        let last = machine.collect_major(machine.quiesce().unwrap()).unwrap();
        assert_eq!(last.programs.len(), 2);
        assert_eq!(machine.residency().programs, 0);
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
}
