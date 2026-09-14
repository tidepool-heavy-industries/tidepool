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

/// One shared descriptor per constructor identity, with the declaration it
/// was minted from so a conflicting later declaration is refused rather
/// than silently aliased.
#[derive(Default)]
pub struct DescriptorInterner {
    constructors: BTreeMap<SymbolIdentity, (ConstructorDecl, Arc<ObjectDescriptor>)>,
}

impl DescriptorInterner {
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
        let layout = StorageLayout::for_reps(target, &declaration.field_reps)?;
        let descriptor = Arc::new(ObjectDescriptor::constructor(
            declaration.tag,
            layout,
            None,
        )?);
        self.constructors.insert(
            declaration.identity.clone(),
            (declaration.clone(), Arc::clone(&descriptor)),
        );
        Ok(descriptor)
    }

    /// Take on a compiled program's constructor descriptors, so programs
    /// compiled later against this interner share them. An identity already
    /// present must carry the same declaration; otherwise the conflicting
    /// identity is returned and nothing is absorbed. An identical
    /// declaration whose descriptor differs (the program was compiled
    /// standalone, not through this interner) is accepted and keeps the
    /// first-interned descriptor: that program's own cells and code agree
    /// with each other, later programs unify with the first, and nothing is
    /// unsound -- only unshared.
    pub(super) fn absorb(
        &mut self,
        entries: &[(ConstructorDecl, Arc<ObjectDescriptor>)],
    ) -> Result<(), SymbolIdentity> {
        for (declaration, _) in entries {
            if let Some((existing, _)) = self.constructors.get(&declaration.identity) {
                if existing != declaration {
                    return Err(declaration.identity.clone());
                }
            }
        }
        for (declaration, descriptor) in entries {
            self.constructors
                .entry(declaration.identity.clone())
                .or_insert_with(|| (declaration.clone(), Arc::clone(descriptor)));
        }
        Ok(())
    }
}
