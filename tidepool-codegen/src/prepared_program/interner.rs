//! Machine-wide constructor descriptor interning.
//!
//! Every dispatch site in generated code compares an object's header word
//! (the pinned `ObjectDescriptor`'s own address) against descriptor
//! addresses baked in at codegen. A constructor's descriptor is determined
//! by its declaration, never by the program declaring it, so two programs
//! installed on one machine must share one descriptor per constructor
//! identity for a later program's `Case`, evaluated-constructor enter and
//! observation to recognise cells the earlier program built. The interner
//! is that sharing: the owning `PreparedMachine` compiles every later
//! program against it, and absorbs each installed program's own entries.

use std::collections::BTreeMap;
use std::sync::Arc;

use tidepool_heap::execution_descriptor::ObjectDescriptor;
use tidepool_repr::execution_schema::{
    ConstructorDecl, StorageLayout, SymbolIdentity, TargetDescriptor,
};

use super::CompileError;
use tidepool_repr::DataConId;

#[derive(Debug)]
pub(super) enum AbsorbConflict {
    Identity(SymbolIdentity),
    HostId {
        host_id: DataConId,
        identity: Box<SymbolIdentity>,
        existing: Box<SymbolIdentity>,
    },
    /// The program's external wrapper descriptors are not this machine's:
    /// it was compiled against another interner.
    Externals,
}

/// The machine's one set of external wrapper descriptors. A `ByteArray#`,
/// boxed array or `MutVar#` is authenticated by header identity against the
/// descriptor the *reading* code was compiled with, so every program on a
/// machine must share these three exactly as it shares constructor
/// descriptors; a value one program built is then readable by every later
/// one (a `Text` bound in one turn and used in the next, a host-built answer).
#[derive(Clone)]
pub(crate) struct ExternalDescriptors {
    pub(crate) boxed_array: Arc<ObjectDescriptor>,
    /// Fixed one-slot mutable cells share the boxed payload ledger, not array
    /// identity. Hosts authenticate this distinct descriptor before access.
    pub(crate) mut_var: Arc<ObjectDescriptor>,
    pub(crate) bytes_array: Arc<ObjectDescriptor>,
}

impl ExternalDescriptors {
    fn mint(target: &TargetDescriptor) -> Result<Self, CompileError> {
        use tidepool_heap::external_storage::ExternalStorageKind;
        Ok(Self {
            boxed_array: Arc::new(ObjectDescriptor::external(
                ExternalStorageKind::BoxedArray,
                target,
            )?),
            mut_var: Arc::new(ObjectDescriptor::external(
                ExternalStorageKind::BoxedArray,
                target,
            )?),
            bytes_array: Arc::new(ObjectDescriptor::external(
                ExternalStorageKind::Bytes,
                target,
            )?),
        })
    }

    fn same_as(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.boxed_array, &other.boxed_array)
            && Arc::ptr_eq(&self.mut_var, &other.mut_var)
            && Arc::ptr_eq(&self.bytes_array, &other.bytes_array)
    }

    /// Header words of the three descriptors.
    pub(crate) fn headers(&self) -> [usize; 3] {
        [
            self.boxed_array.initial_header_word(),
            self.mut_var.initial_header_word(),
            self.bytes_array.initial_header_word(),
        ]
    }
}

/// One shared descriptor per constructor identity, with the declaration it
/// was minted from so a conflicting later declaration is refused rather
/// than silently aliased; and the one set of external wrapper descriptors.
#[derive(Clone, Default)]
pub struct DescriptorInterner {
    constructors: BTreeMap<SymbolIdentity, (ConstructorDecl, Arc<ObjectDescriptor>)>,
    by_host: BTreeMap<DataConId, SymbolIdentity>,
    externals: Option<ExternalDescriptors>,
}

impl DescriptorInterner {
    /// The machine's external wrapper descriptors, minted on first use.
    pub(super) fn externals(
        &mut self,
        target: &TargetDescriptor,
    ) -> Result<ExternalDescriptors, CompileError> {
        if let Some(externals) = &self.externals {
            return Ok(externals.clone());
        }
        let minted = ExternalDescriptors::mint(target)?;
        self.externals = Some(minted.clone());
        Ok(minted)
    }

    /// The external descriptors every installed program shares; `None`
    /// before the first program compiles against this interner.
    pub(crate) fn shared_externals(&self) -> Option<&ExternalDescriptors> {
        self.externals.as_ref()
    }

    /// Adopt a program's external descriptors: the first program's become
    /// the machine's; every later program must carry exactly those.
    pub(super) fn absorb_externals(
        &mut self,
        incoming: &ExternalDescriptors,
    ) -> Result<(), AbsorbConflict> {
        match &self.externals {
            None => {
                self.externals = Some(incoming.clone());
                Ok(())
            }
            Some(existing) if existing.same_as(incoming) => Ok(()),
            Some(_) => Err(AbsorbConflict::Externals),
        }
    }

    /// The descriptor for `declaration`: the already-interned one when this
    /// identity was minted before with an identical declaration, a fresh one
    /// otherwise. A different declaration under the same identity is
    /// [`CompileError::DescriptorShape`].
    pub(super) fn intern(
        &mut self,
        target: &TargetDescriptor,
        declaration: &ConstructorDecl,
    ) -> Result<Arc<ObjectDescriptor>, CompileError> {
        if let Some((existing, descriptor)) = self.constructors.get(&declaration.identity) {
            if existing != declaration {
                return Err(CompileError::DescriptorShape {
                    identity: Box::new(declaration.identity.clone()),
                });
            }
            return Ok(Arc::clone(descriptor));
        }
        if let Some(existing) = self.by_host.get(&declaration.host_id) {
            return Err(CompileError::HostIdConflict {
                host_id: declaration.host_id,
                identity: Box::new(declaration.identity.clone()),
                existing: Box::new(existing.clone()),
            });
        }
        let layout = StorageLayout::for_reps(target, &declaration.field_reps)?;
        let descriptor = Arc::new(ObjectDescriptor::constructor(
            declaration.tag,
            layout,
            None,
        )?);
        self.by_host
            .insert(declaration.host_id, declaration.identity.clone());
        self.constructors.insert(
            declaration.identity.clone(),
            (declaration.clone(), Arc::clone(&descriptor)),
        );
        Ok(descriptor)
    }

    /// Absorb declarations atomically. Both constructor identities and host
    /// IDs must agree with existing entries and with the entire incoming batch.
    /// Identical declarations retain the first canonical descriptor.
    pub(super) fn absorb(
        &mut self,
        entries: &[(ConstructorDecl, Arc<ObjectDescriptor>)],
    ) -> Result<(), AbsorbConflict> {
        let mut identities = BTreeMap::new();
        let mut hosts = BTreeMap::new();
        for (declaration, _) in entries {
            let existing = self
                .constructors
                .get(&declaration.identity)
                .map(|(decl, _)| decl)
                .or_else(|| identities.get(&declaration.identity).copied());
            if existing.is_some_and(|existing| existing != declaration) {
                return Err(AbsorbConflict::Identity(declaration.identity.clone()));
            }
            let existing = self
                .by_host
                .get(&declaration.host_id)
                .or_else(|| hosts.get(&declaration.host_id).copied());
            if let Some(existing) = existing.filter(|existing| *existing != &declaration.identity) {
                return Err(AbsorbConflict::HostId {
                    host_id: declaration.host_id,
                    identity: Box::new(declaration.identity.clone()),
                    existing: Box::new(existing.clone()),
                });
            }
            identities.insert(&declaration.identity, declaration);
            hosts.insert(&declaration.host_id, &declaration.identity);
        }
        for (declaration, descriptor) in entries {
            self.by_host
                .insert(declaration.host_id, declaration.identity.clone());
            self.constructors
                .entry(declaration.identity.clone())
                .or_insert_with(|| (declaration.clone(), Arc::clone(descriptor)));
        }
        Ok(())
    }

    /// Resolve a bridge constructor identity to the machine's canonical descriptor.
    pub fn by_host(&self, id: DataConId) -> Option<&(ConstructorDecl, Arc<ObjectDescriptor>)> {
        self.by_host
            .get(&id)
            .and_then(|identity| self.constructors.get(identity))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tidepool_repr::execution_schema::{Architecture, CheckedLayout, Endianness, RuntimeRep};
    use tidepool_repr::DataConId;

    fn declaration(name: &str, host: u64) -> ConstructorDecl {
        let identity = SymbolIdentity {
            unit: "test".into(),
            module: "Test".into(),
            namespace: "constructor".into(),
            occurrence: name.into(),
            record_parent: None,
        };
        ConstructorDecl {
            family: identity.clone(),
            identity,
            host_id: DataConId(host),
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
        }
    }

    fn target() -> TargetDescriptor {
        TargetDescriptor {
            architecture: Architecture::X86_64,
            endianness: Endianness::Little,
            pointer_width: 64,
            word_width: 64,
            abi: "sysv".into(),
            features: vec![],
        }
    }

    #[test]
    fn reinterning_preserves_descriptor_and_host_lookup() {
        let mut interner = DescriptorInterner::default();
        let declared = declaration("First", 1);
        let first = interner.intern(&target(), &declared).unwrap();
        let second = interner.intern(&target(), &declared).unwrap();
        assert!(Arc::ptr_eq(&first, &second));
        let (found, descriptor) = interner.by_host(declared.host_id).unwrap();
        assert_eq!(found, &declared);
        assert!(Arc::ptr_eq(&first, descriptor));
        assert!(interner.by_host(DataConId(2)).is_none());
    }

    #[test]
    fn conflicting_host_identity_is_refused() {
        let mut interner = DescriptorInterner::default();
        interner
            .intern(&target(), &declaration("First", 1))
            .unwrap();
        assert!(matches!(
            interner.intern(&target(), &declaration("Second", 1)),
            Err(CompileError::HostIdConflict {
                host_id: DataConId(1),
                ..
            })
        ));
        assert_eq!(
            interner
                .by_host(DataConId(1))
                .unwrap()
                .0
                .identity
                .occurrence,
            "First"
        );
        assert_eq!(interner.constructors.len(), 1);
    }

    #[test]
    fn absorb_refuses_host_conflicts_without_partial_insertion() {
        let mut interner = DescriptorInterner::default();
        interner
            .intern(&target(), &declaration("First", 1))
            .unwrap();
        let mut incoming = DescriptorInterner::default();
        let new = declaration("New", 2);
        let conflict = declaration("Conflict", 1);
        let entries = vec![
            (new.clone(), incoming.intern(&target(), &new).unwrap()),
            (
                conflict.clone(),
                incoming.intern(&target(), &conflict).unwrap(),
            ),
        ];
        assert!(interner.absorb(&entries).is_err());
        assert_eq!(interner.constructors.len(), 1);
        assert!(!interner.constructors.contains_key(&new.identity));
        assert!(interner.by_host(DataConId(2)).is_none());
        assert_eq!(
            interner
                .by_host(DataConId(1))
                .unwrap()
                .0
                .identity
                .occurrence,
            "First"
        );
    }

    #[test]
    fn successful_absorb_publishes_host_lookup() {
        let mut source = DescriptorInterner::default();
        let declared = declaration("First", 1);
        let descriptor = source.intern(&target(), &declared).unwrap();
        let mut destination = DescriptorInterner::default();
        destination
            .absorb(&[(declared.clone(), descriptor.clone())])
            .unwrap();
        let (found, canonical) = destination.by_host(declared.host_id).unwrap();
        assert_eq!(found, &declared);
        assert!(Arc::ptr_eq(canonical, &descriptor));
    }

    #[test]
    fn absorb_refuses_divergent_declarations_within_batch() {
        let first = declaration("First", 1);
        let mut second = first.clone();
        second.host_id = DataConId(2);
        let descriptor = DescriptorInterner::default()
            .intern(&target(), &first)
            .unwrap();
        let mut interner = DescriptorInterner::default();
        assert!(matches!(
            interner.absorb(&[(first, descriptor.clone()), (second, descriptor)]),
            Err(AbsorbConflict::Identity(_))
        ));
        assert!(interner.constructors.is_empty());
        assert!(interner.by_host.is_empty());
    }

    #[test]
    fn absorb_checks_conflicts_within_the_incoming_batch() {
        let mut interner = DescriptorInterner::default();
        let first = declaration("First", 1);
        let second = declaration("Second", 1);
        let descriptor = DescriptorInterner::default()
            .intern(&target(), &first)
            .unwrap();
        assert!(interner
            .absorb(&[(first, descriptor.clone()), (second, descriptor)])
            .is_err());
        assert!(interner.constructors.is_empty());
        assert!(interner.by_host.is_empty());
    }
}
