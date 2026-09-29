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
//! [`LinkedProgram`] already carries its target and full resolved-import
//! shape, and it derives `Eq`. There is no canonical hash for this linked
//! shape, so lookup compares live keys structurally. The registry owns no
//! image: installed machines, parcels, and active compiles hold the strong
//! references. Each lookup or insert removes entries whose image has died,
//! including their linked-program keys.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError, Weak};

use tidepool_repr::execution_schema::LinkedProgram;

use super::CompiledProgram;

/// Shared by every [`super::machine::PreparedMachine`] of one run. Cheap to
/// clone the `Arc` around; the registry itself owns a lock, not a checkout.
#[derive(Default)]
pub struct ImageRegistry {
    entries: Mutex<Vec<(LinkedProgram, Weak<CompiledProgram>)>>,
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

    /// An existing live image compiled for exactly `key`'s content and target.
    #[must_use]
    pub fn lookup(&self, key: &LinkedProgram) -> Option<Arc<CompiledProgram>> {
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        entries.retain(|(_, image)| image.strong_count() != 0);
        let found = entries.iter().find_map(|(existing, image)| {
            if existing == key {
                image.upgrade()
            } else {
                None
            }
        });
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
        entries.retain(|(_, image)| image.strong_count() != 0);
        if let Some(existing) = entries.iter().find_map(|(existing, candidate)| {
            if existing == &key {
                candidate.upgrade()
            } else {
                None
            }
        }) {
            return existing;
        }
        entries.push((key, Arc::downgrade(&image)));
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
    use super::super::{PreparedMachine, PreparedMachineOptions, RunOptions};
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

    #[test]
    fn dead_image_and_key_are_reclaimed_while_registry_lives() {
        let registry = ImageRegistry::new();
        let key = program();
        let image = compiled();
        let weak = Arc::downgrade(&image);
        let registered = registry.insert(key.clone(), image);
        let (machine, _) = PreparedMachine::new_shared(
            registered,
            PreparedMachineOptions {
                nursery_bytes: RunOptions::default().nursery_bytes,
            },
        )
        .expect("machine installs image");
        assert_eq!(registry.entries.lock().unwrap().len(), 1);
        assert!(weak.upgrade().is_some(), "machine owns the live image");

        drop(machine);
        assert!(weak.upgrade().is_none(), "the registry owns no image");
        assert!(registry.lookup(&key).is_none());
        assert!(
            registry.entries.lock().unwrap().is_empty(),
            "dead key pruned"
        );
    }

    #[test]
    fn insert_prunes_dead_key_and_accepts_a_new_image() {
        let registry = ImageRegistry::new();
        let key = program();
        let old = registry.insert(key.clone(), compiled());
        drop(old);

        let replacement = compiled();
        let inserted = registry.insert(key.clone(), Arc::clone(&replacement));
        assert!(Arc::ptr_eq(&inserted, &replacement));
        assert_eq!(registry.entries.lock().unwrap().len(), 1);
        let hit = registry.lookup(&key).expect("replacement remains live");
        assert!(Arc::ptr_eq(&hit, &replacement));
    }
}
