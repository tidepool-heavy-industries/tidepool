//! Atomic installation of mutually importing native group images.

use super::*;
use tidepool_repr::execution_schema::Signature;

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
                        if export.identity != slot.identity
                            || slot.entry_signature.as_ref().is_some_and(|expected| {
                                export.entry_signature.as_ref() != Some(expected)
                            })
                        {
                            return Err(ExecutionError::BatchSourceContract(Box::new(
                                slot.identity.clone(),
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
                    }
                    BatchImport::Existing {
                        handle,
                        entry_signature,
                    } => {
                        if slot
                            .entry_signature
                            .as_ref()
                            .is_some_and(|expected| entry_signature.as_ref() != Some(expected))
                        {
                            return Err(ExecutionError::BatchSourceContract(Box::new(
                                slot.identity.clone(),
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
            candidate_alloc_start: None,
            committed: false,
        };
        let staged = transaction.machine.stage_batch(
            &candidates,
            &external,
            heap_reserve,
            &mut transaction.candidate_alloc_start,
        );
        let catalog = if staged.is_ok() {
            transaction.machine.install_catalog_after_stage()
        } else {
            Ok(None)
        };
        if staged.is_ok() && catalog.is_ok() {
            transaction.machine.commit_batch_metadata(&candidates);
        }
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
    use tidepool_repr::execution_schema::{
        testing, Atom, CachedHomeOwner, CertifiedGroup, CheckedLayout, ConstructorDecl,
        ConstructorId, ExprFrame, GlobalDecl, GlobalId, HeapRhs, ImportOwner, ModuleVersion,
        Signature, SignatureId, ValueRef,
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
        assert!(matches!(
            machine.install_shared_batch(vec![
                BatchProgram {
                    image: a,
                    imports: vec![BatchImport::Source {
                        group: 1,
                        binding: ValueId(0)
                    }],
                },
                BatchProgram {
                    image: b,
                    imports: vec![BatchImport::Source {
                        group: 0,
                        binding: ValueId(0)
                    }],
                },
            ]),
            Err(ExecutionError::BatchSourceContract(_))
        ));
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
