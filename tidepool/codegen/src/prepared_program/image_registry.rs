//! One compiled image, shared by every machine of a run that asks for it.
//!
//! `ImageRegistry` sits above [`super::machine::PreparedMachine`]: a
//! `PreparedEngine` (or any other caller minting a `PreparedMachine`) holds
//! an `Arc<ImageRegistry>` shared with every other machine of the same run,
//! looks a linked program up before compiling it, and installs whatever
//! comes back with [`super::machine::PreparedMachine::install_shared`]
//! instead of compiling its own copy. `CompiledProgram: Send + Sync` (see
//! its own `unsafe impl`) is what makes handing the same `Arc` to a machine
//! on another thread sound.
//!
//! The key is content equality on the linked program itself: a
//! [`LinkedProgram`] already carries its target ([`TargetDescriptor`], via
//! `PreparedProgram`'s envelope) and its full resolved-import shape, and it
//! derives `Eq`, so two calls that would compile byte-identical code compare
//! equal here with no extra fingerprint to keep in sync. This deliberately
//! costs a linear scan and a full structural comparison per lookup rather
//! than hashing -- correct and small; a session installs at most a handful
//! of distinct images, not thousands. A future caller with a hot path
//! through many distinct images should add a real content hash instead of
//! widening this scan, not the other way around.
//!
//! No eviction in this parcel: an entry lives as long as the registry
//! itself. An image with no machine still holding it and no registry
//! reference is dropped when the registry is (see the module's own doc for
//! why sharing needs no eviction sooner than that -- this cache never
//! outlives the run it belongs to). Retiring an image from every machine
//! that installed it, while the registry itself stays alive across many
//! more installs than any one image should be charged for, is future work.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use tidepool_repr::execution_schema::LinkedProgram;

use super::CompiledProgram;

/// Shared by every [`super::machine::PreparedMachine`] of one run. Cheap to
/// clone the `Arc` around; the registry itself owns a lock, not a checkout.
#[derive(Default)]
pub struct ImageRegistry {
    entries: Mutex<Vec<(Arc<LinkedProgram>, Arc<CompiledProgram>)>>,
    /// Lifetime lookups that found an existing image. Never decremented.
    hits: AtomicU64,
    /// Lifetime lookups that found nothing (the caller then compiles and
    /// [`Self::insert`]s). Never decremented.
    misses: AtomicU64,
}

impl ImageRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// An existing image compiled for exactly `key`'s content and target, if
    /// any machine of this run has already compiled or inserted one.
    #[must_use]
    pub fn lookup(&self, key: &LinkedProgram) -> Option<Arc<CompiledProgram>> {
        let entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        let found = entries
            .iter()
            .find(|(existing, _)| existing.as_ref() == key)
            .map(|(_, image)| Arc::clone(image));
        if found.is_some() {
            self.hits.fetch_add(1, Ordering::Relaxed);
        } else {
            self.misses.fetch_add(1, Ordering::Relaxed);
        }
        found
    }

    /// Register `image` as the compiled form of `key`. First writer wins:
    /// if another caller already inserted an entry for content-and-target-
    /// equal `key` (a race between two machines that both missed the same
    /// lookup and compiled concurrently), the ALREADY-registered `Arc` is
    /// returned and `image` is dropped -- every later caller, and this one,
    /// then shares exactly one compiled image for that content, never two.
    #[must_use]
    pub fn insert(&self, key: LinkedProgram, image: Arc<CompiledProgram>) -> Arc<CompiledProgram> {
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((_, existing)) = entries.iter().find(|(existing, _)| **existing == key) {
            return Arc::clone(existing);
        }
        entries.push((Arc::new(key), Arc::clone(&image)));
        image
    }

    /// Lifetime lookups that hit an already-compiled image.
    #[must_use]
    pub fn hits(&self) -> u64 {
        self.hits.load(Ordering::Relaxed)
    }

    /// Lifetime lookups that found nothing and required a compile.
    #[must_use]
    pub fn misses(&self) -> u64 {
        self.misses.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tidepool_repr::execution_schema::{link_program, testing, MachineImports};

    fn program() -> LinkedProgram {
        let wire = testing::wire_program();
        let prepared = testing::prepare(wire).expect("fixture prepares");
        link_program(prepared, &MachineImports::default()).expect("fixture links")
    }

    fn compiled() -> Arc<CompiledProgram> {
        Arc::new(CompiledProgram::compile(&program()).expect("fixture compiles"))
    }

    #[test]
    fn a_miss_then_insert_then_lookup_returns_the_same_arc() {
        let registry = ImageRegistry::new();
        let key = program();
        assert!(
            registry.lookup(&key).is_none(),
            "nothing registered yet: a miss"
        );
        assert_eq!(registry.misses(), 1);
        assert_eq!(registry.hits(), 0);

        let image = compiled();
        let inserted = registry.insert(key.clone(), Arc::clone(&image));
        assert!(Arc::ptr_eq(&inserted, &image));

        let hit = registry.lookup(&key).expect("now registered: a hit");
        assert!(
            Arc::ptr_eq(&hit, &image),
            "a hit returns the exact Arc `insert` registered, not a copy"
        );
        assert_eq!(registry.hits(), 1);
        assert_eq!(registry.misses(), 1);
    }

    #[test]
    fn a_racing_insert_keeps_the_first_writer() {
        let registry = ImageRegistry::new();
        let key = program();
        let first = compiled();
        let second = compiled();

        let kept_first = registry.insert(key.clone(), Arc::clone(&first));
        assert!(Arc::ptr_eq(&kept_first, &first));

        // A second compile of content-and-target-equal `key` -- as two
        // machines racing the same registry miss would each produce -- must
        // not replace the first: every later caller shares one image.
        let kept_second = registry.insert(key, Arc::clone(&second));
        assert!(
            Arc::ptr_eq(&kept_second, &first),
            "first writer wins the race"
        );
        assert!(!Arc::ptr_eq(&kept_second, &second));
    }
}
