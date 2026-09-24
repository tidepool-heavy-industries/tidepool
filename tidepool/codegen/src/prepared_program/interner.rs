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
use std::sync::{Arc, OnceLock};

use tidepool_heap::execution_descriptor::ObjectDescriptor;
use tidepool_repr::execution_schema::{
    ConstructorDecl, StorageLayout, SymbolIdentity, TargetDescriptor,
};

use super::CompileError;
use tidepool_repr::DataConId;

#[derive(Debug)]
pub(super) enum AbsorbConflict {
    /// The incoming batch declares `identity` with a shape (arity, field
    /// representations) that disagrees with `existing`, the declaration
    /// already interned for it -- both are carried so the message can name
    /// the mismatch, not just the identity.
    Identity {
        existing: Box<ConstructorDecl>,
        incoming: Box<ConstructorDecl>,
    },
    HostId {
        host_id: DataConId,
        identity: Box<SymbolIdentity>,
        existing: Box<SymbolIdentity>,
    },
}

/// The one set of external wrapper descriptors for the whole process. A
/// `ByteArray#`, boxed array or `MutVar#` is authenticated by header
/// identity against the descriptor the *reading* code was compiled with, so
/// every program compiled in this process -- on any machine -- must share
/// these three exactly, the same way every machine shares constructor
/// descriptors for a given identity; a value one program built is then
/// readable by every later one on any machine (a `Text` bound in one turn
/// and used in the next, a host-built answer, a machine-to-machine handoff).
#[derive(Clone)]
pub(crate) struct ExternalDescriptors {
    pub(crate) boxed_array: Arc<ObjectDescriptor>,
    /// Fixed one-slot mutable cells share the boxed payload ledger, not array
    /// identity. Hosts authenticate this distinct descriptor before access.
    pub(crate) mut_var: Arc<ObjectDescriptor>,
    pub(crate) bytes_array: Arc<ObjectDescriptor>,
}

/// The process-wide singleton, keyed by the target it was minted for. Every
/// `TargetDescriptor` this process compiles against is the same ABI, so this
/// mints once and every later caller gets clones of the same three `Arc`s
/// regardless of which interner or machine asked.
static PROCESS_EXTERNALS: OnceLock<(TargetDescriptor, ExternalDescriptors)> = OnceLock::new();

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

    /// The process's external wrapper descriptors, minted once for the
    /// process and shared by every later caller. A concurrent first call
    /// from two threads may mint twice; [`OnceLock::set`] lets exactly one
    /// win, and both callers return clones of that winner, so no caller ever
    /// sees a second, divergent set.
    fn shared(target: &TargetDescriptor) -> Result<Self, CompileError> {
        if let Some((existing_target, externals)) = PROCESS_EXTERNALS.get() {
            debug_assert_eq!(
                existing_target, target,
                "process external descriptors were minted for a different target"
            );
            return Ok(externals.clone());
        }
        let minted = Self::mint(target)?;
        Ok(
            match PROCESS_EXTERNALS.set((target.clone(), minted.clone())) {
                Ok(()) => minted,
                // Another thread won the race; use its winning value instead.
                Err(_) => PROCESS_EXTERNALS
                    .get()
                    .map(|(_, externals)| externals.clone())
                    .unwrap_or(minted),
            },
        )
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

type ConstructorEntry = (ConstructorDecl, Arc<ObjectDescriptor>);

#[derive(Clone, Default)]
struct Tables {
    constructors: BTreeMap<SymbolIdentity, ConstructorEntry>,
    by_host: BTreeMap<DataConId, SymbolIdentity>,
}

/// One shared descriptor per constructor identity, with the declaration it
/// was minted from so a conflicting later declaration is refused rather
/// than silently aliased; and the one set of external wrapper descriptors.
///
/// The tables are an overlay: `base` is shared by reference-counted
/// pointer, `local` holds what this value added while `base` was shared.
/// Cloning the machine's interner for a compile therefore copies only the
/// (normally empty) local layer; the compile's own new constructors land
/// in its local layer. The machine's interner owns its base uniquely once
/// no compile is outstanding, so absorbing an install inserts into the
/// base directly. The two layers never hold the same key.
#[derive(Clone, Default)]
pub struct DescriptorInterner {
    base: Arc<Tables>,
    local: Tables,
    externals: Option<ExternalDescriptors>,
}

impl DescriptorInterner {
    fn constructor(&self, identity: &SymbolIdentity) -> Option<&ConstructorEntry> {
        self.local
            .constructors
            .get(identity)
            .or_else(|| self.base.constructors.get(identity))
    }

    fn host_identity(&self, host_id: &DataConId) -> Option<&SymbolIdentity> {
        self.local
            .by_host
            .get(host_id)
            .or_else(|| self.base.by_host.get(host_id))
    }

    /// Insert an identity known to be absent from both layers.
    fn insert_new(&mut self, declaration: &ConstructorDecl, descriptor: Arc<ObjectDescriptor>) {
        let tables = match Arc::get_mut(&mut self.base) {
            Some(base) => base,
            None => &mut self.local,
        };
        tables
            .by_host
            .insert(declaration.host_id, declaration.identity.clone());
        tables.constructors.insert(
            declaration.identity.clone(),
            (declaration.clone(), descriptor),
        );
    }

    /// This interner's cached clone of the process-wide external wrapper
    /// descriptors ([`ExternalDescriptors::shared`]), fetched on first use.
    /// Every interner in the process converges on the same three `Arc`s, so
    /// this cache exists only to avoid the `OnceLock` read on every compile.
    pub(super) fn externals(
        &mut self,
        target: &TargetDescriptor,
    ) -> Result<ExternalDescriptors, CompileError> {
        if let Some(externals) = &self.externals {
            return Ok(externals.clone());
        }
        let shared = ExternalDescriptors::shared(target)?;
        self.externals = Some(shared.clone());
        Ok(shared)
    }

    /// The external descriptors every installed program shares; `None`
    /// before the first program compiles against this interner.
    pub(crate) fn shared_externals(&self) -> Option<&ExternalDescriptors> {
        self.externals.as_ref()
    }

    /// Cache a program's external descriptors on this interner. Every
    /// program in the process mints its externals from the same
    /// [`ExternalDescriptors::shared`] singleton, so an incoming program's
    /// externals can never disagree with what this interner already cached;
    /// the assertion proves that invariant rather than guarding against a
    /// conflict that can no longer arise.
    pub(super) fn commit_externals(&mut self, incoming: &ExternalDescriptors) {
        match &self.externals {
            Some(existing) => debug_assert!(
                existing.same_as(incoming),
                "external descriptors diverged despite process-wide sharing"
            ),
            None => self.externals = Some(incoming.clone()),
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
        if let Some((existing, descriptor)) = self.constructor(&declaration.identity) {
            if existing != declaration {
                return Err(CompileError::DescriptorShape {
                    identity: Box::new(declaration.identity.clone()),
                    existing_field_reps: existing.field_reps.clone(),
                    incoming_field_reps: declaration.field_reps.clone(),
                });
            }
            return Ok(Arc::clone(descriptor));
        }
        if let Some(existing) = self.host_identity(&declaration.host_id) {
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
        self.insert_new(declaration, Arc::clone(&descriptor));
        Ok(descriptor)
    }

    /// Validate a batch for [`Self::commit_absorb`] without changing
    /// anything. Both constructor identities and host IDs must agree with
    /// existing entries and with the entire incoming batch.
    pub(super) fn check_absorb(
        &self,
        entries: &[(ConstructorDecl, Arc<ObjectDescriptor>)],
    ) -> Result<(), AbsorbConflict> {
        let mut identities = BTreeMap::new();
        let mut hosts = BTreeMap::new();
        for (declaration, _) in entries {
            let existing = self
                .constructor(&declaration.identity)
                .map(|(decl, _)| decl)
                .or_else(|| identities.get(&declaration.identity).copied());
            if let Some(existing) = existing.filter(|existing| *existing != declaration) {
                return Err(AbsorbConflict::Identity {
                    existing: Box::new(existing.clone()),
                    incoming: Box::new(declaration.clone()),
                });
            }
            let existing = self
                .host_identity(&declaration.host_id)
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
        Ok(())
    }

    /// Insert a batch already accepted by [`Self::check_absorb`] against
    /// this same, unchanged interner. Identical declarations retain the
    /// first canonical descriptor.
    pub(super) fn commit_absorb(&mut self, entries: &[(ConstructorDecl, Arc<ObjectDescriptor>)]) {
        for (declaration, descriptor) in entries {
            if self.constructor(&declaration.identity).is_none() {
                self.insert_new(declaration, Arc::clone(descriptor));
            }
        }
    }

    /// Absorb declarations atomically (check, then commit).
    #[cfg(test)]
    pub(super) fn absorb(
        &mut self,
        entries: &[(ConstructorDecl, Arc<ObjectDescriptor>)],
    ) -> Result<(), AbsorbConflict> {
        self.check_absorb(entries)?;
        self.commit_absorb(entries);
        Ok(())
    }

    /// Resolve a bridge constructor identity to the machine's canonical descriptor.
    pub fn by_host(&self, id: DataConId) -> Option<&(ConstructorDecl, Arc<ObjectDescriptor>)> {
        self.host_identity(&id)
            .and_then(|identity| self.constructor(identity))
    }

    #[cfg(test)]
    fn constructor_count(&self) -> usize {
        self.base.constructors.len() + self.local.constructors.len()
    }

    #[cfg(test)]
    fn host_count(&self) -> usize {
        self.base.by_host.len() + self.local.by_host.len()
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
    fn independent_interners_share_the_same_external_descriptors() {
        // Two interners that never overlay or absorb from one another --
        // standing in for two separately compiled programs, possibly on two
        // separate machines -- must still mint identical external wrapper
        // descriptors: they come from the process-wide singleton, not from
        // either interner's own state.
        let mut a = DescriptorInterner::default();
        let mut b = DescriptorInterner::default();
        let externals_a = a.externals(&target()).unwrap();
        let externals_b = b.externals(&target()).unwrap();
        assert!(externals_a.same_as(&externals_b));
        assert!(Arc::ptr_eq(
            &externals_a.boxed_array,
            &externals_b.boxed_array
        ));
        assert!(Arc::ptr_eq(&externals_a.mut_var, &externals_b.mut_var));
        assert!(Arc::ptr_eq(
            &externals_a.bytes_array,
            &externals_b.bytes_array
        ));
        assert_eq!(externals_a.headers(), externals_b.headers());
    }

    #[test]
    fn commit_externals_caches_the_shared_singleton_without_conflict() {
        // `commit_externals` used to refuse a second program's externals
        // unless they were `Arc`-identical to whatever the machine had
        // already adopted. With one process-wide singleton there is nothing
        // left to refuse: every program's externals are already the same
        // value, so commit is just a cache fill that the debug assertion
        // inside proves rather than guards.
        let mut interner = DescriptorInterner::default();
        assert!(interner.shared_externals().is_none());
        let first = ExternalDescriptors::shared(&target()).unwrap();
        interner.commit_externals(&first);
        let cached = interner
            .shared_externals()
            .expect("commit_externals caches on first call");
        assert!(cached.same_as(&first));

        // A later, independently-minted set (e.g. another program's) is the
        // same singleton and commits again without changing anything.
        let second = ExternalDescriptors::shared(&target()).unwrap();
        interner.commit_externals(&second);
        assert!(interner.shared_externals().unwrap().same_as(&second));
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
        assert_eq!(interner.constructor_count(), 1);
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
        assert_eq!(interner.constructor_count(), 1);
        assert!(interner.constructor(&new.identity).is_none());
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
    fn descriptor_shape_conflict_names_both_declarations_field_reps() {
        let mut interner = DescriptorInterner::default();
        let mut existing = declaration("First", 1);
        existing.field_reps = vec![RuntimeRep::Int(64)];
        existing.strict_fields = vec![true];
        existing.layout = CheckedLayout {
            fields: vec![tidepool_repr::execution_schema::FieldLayout {
                rep: RuntimeRep::Int(64),
                offset: 0,
            }],
            alignment: 8,
            payload_size: 8,
            root_mask: vec![false],
        };
        interner.intern(&target(), &existing).unwrap();

        let mut incoming = existing.clone();
        incoming.field_reps = vec![RuntimeRep::LiftedRef, RuntimeRep::Int(32)];

        // Exercised through `intern()` (a same-compile conflict against an
        // already-interned identity)...
        let error = interner.intern(&target(), &incoming).unwrap_err();
        let message = error.to_string();
        assert!(
            message.contains(&format!("{:?}", existing.field_reps)),
            "message should name the interned field reps: {message}"
        );
        assert!(
            message.contains(&format!("{:?}", incoming.field_reps)),
            "message should name the incoming field reps: {message}"
        );

        // ...and through `check_absorb` (a cross-program conflict at
        // install), which is the same conflict class under a different
        // caller.
        let descriptor = DescriptorInterner::default()
            .intern(&target(), &incoming)
            .unwrap();
        let conflict = interner
            .check_absorb(&[(incoming.clone(), descriptor)])
            .unwrap_err();
        match conflict {
            AbsorbConflict::Identity {
                existing: found_existing,
                incoming: found_incoming,
            } => {
                assert_eq!(found_existing.field_reps, existing.field_reps);
                assert_eq!(found_incoming.field_reps, incoming.field_reps);
            }
            other => panic!("expected an Identity conflict, got {other:?}"),
        }
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
            Err(AbsorbConflict::Identity { .. })
        ));
        assert_eq!(interner.constructor_count(), 0);
        assert_eq!(interner.host_count(), 0);
    }

    #[test]
    fn clone_overlays_additions_without_touching_the_shared_base() {
        let mut machine = DescriptorInterner::default();
        let first = declaration("First", 1);
        let shared = machine.intern(&target(), &first).unwrap();
        let mut compile = machine.clone();
        assert!(Arc::ptr_eq(
            &compile.intern(&target(), &first).unwrap(),
            &shared
        ));
        let second = declaration("Second", 2);
        let added = compile.intern(&target(), &second).unwrap();
        assert_eq!(compile.local.constructors.len(), 1);
        assert_eq!(machine.constructor_count(), 1);
        assert!(machine.by_host(second.host_id).is_none());
        let entries = [(first, shared), (second.clone(), added.clone())];
        machine.check_absorb(&entries).unwrap();
        drop(compile);
        machine.commit_absorb(&entries);
        // With the compile gone the machine owns its base again.
        assert!(machine.local.constructors.is_empty());
        assert!(Arc::ptr_eq(
            &machine.by_host(second.host_id).unwrap().1,
            &added
        ));
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
        assert_eq!(interner.constructor_count(), 0);
        assert_eq!(interner.host_count(), 0);
    }
}
