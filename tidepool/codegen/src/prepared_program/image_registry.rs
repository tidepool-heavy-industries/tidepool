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
//! The key is the linked program's structural `Hash` and `Eq`: it carries
//! target and resolved-import shape, and `HashMap` checks complete equality
//! even when hashes collide. The registry owns no image: installed machines,
//! parcels, and active compiles hold strong references. Access removes a dead
//! key immediately; an amortized sweep reclaims dead keys in other buckets.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError, Weak};

use tidepool_repr::execution_schema::LinkedProgram;

use super::CompiledProgram;

/// Shared by every [`super::machine::PreparedMachine`] of one run. Cheap to
/// clone the `Arc` around; the registry itself owns a lock, not a checkout.
#[derive(Default)]
pub struct ImageRegistry {
    entries: Mutex<Entries>,
    /// Lifetime lookups that found an existing image. Never decremented.
    hits: AtomicU64,
    /// Lifetime lookups that found nothing (the caller then compiles and
    /// [`Self::insert`]s). Never decremented.
    misses: AtomicU64,
}

#[derive(Default)]
struct Entries {
    images: HashMap<LinkedProgram, Weak<CompiledProgram>>,
    since_sweep: usize,
}

impl Entries {
    /// A full scan after at least `len` indexed operations makes dead-key
    /// collection amortized constant work per operation. Exact-key dead
    /// entries are removed immediately, including between sweeps.
    fn tick(&mut self) {
        self.since_sweep += 1;
        if self.since_sweep >= self.images.len().max(1) {
            self.images.retain(|_, image| image.strong_count() != 0);
            self.since_sweep = 0;
        }
    }
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
        let found = entries.images.get(key).and_then(Weak::upgrade);
        if found.is_none() {
            entries.images.remove(key);
        }
        entries.tick();
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
        if let Some(existing) = entries.images.get(&key).and_then(Weak::upgrade) {
            entries.tick();
            return existing;
        }
        entries.images.insert(key, Arc::downgrade(&image));
        entries.tick();
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
    use tidepool_repr::execution_schema::{
        link_program, testing, Atom, ExprFrame, MachineImports, ScalarLiteral,
    };

    fn program() -> LinkedProgram {
        program_returning(42)
    }

    fn program_returning(value: i64) -> LinkedProgram {
        let mut wire = testing::wire_program();
        wire.expressions.nodes[0] = ExprFrame::Return(vec![Atom::Scalar(ScalarLiteral::Int {
            bits: 64,
            bytes: value.to_be_bytes().to_vec(),
        })]);
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
        assert_eq!(registry.entries.lock().unwrap().images.len(), 1);
        assert!(weak.upgrade().is_some(), "machine owns the live image");

        drop(machine);
        assert!(weak.upgrade().is_none(), "the registry owns no image");
        assert!(registry.lookup(&key).is_none());
        assert!(
            registry.entries.lock().unwrap().images.is_empty(),
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
        assert_eq!(registry.entries.lock().unwrap().images.len(), 1);
        let hit = registry.lookup(&key).expect("replacement remains live");
        assert!(Arc::ptr_eq(&hit, &replacement));
    }

    #[test]
    fn indexed_lookups_eventually_prune_an_unrelated_dead_key() {
        let registry = ImageRegistry::new();
        let live_key = program();
        let dead_key = program_returning(43);
        let live = registry.insert(live_key.clone(), compiled());
        let dead = Arc::new(CompiledProgram::compile(&dead_key).expect("other fixture compiles"));
        let dead = registry.insert(dead_key, dead);
        drop(dead);
        assert_eq!(registry.entries.lock().unwrap().images.len(), 2);
        assert!(Arc::ptr_eq(&registry.lookup(&live_key).unwrap(), &live));
        assert!(Arc::ptr_eq(&registry.lookup(&live_key).unwrap(), &live));
        assert_eq!(registry.entries.lock().unwrap().images.len(), 1);
    }
}
