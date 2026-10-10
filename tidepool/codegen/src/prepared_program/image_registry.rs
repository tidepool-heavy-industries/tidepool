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
//! The key is the validated program's structural `Hash` and `Eq`: it carries
//! target and declared import contracts, but excludes the linked snapshot's
//! mutable evaluatedness and generation. Linking still validates each snapshot
//! before lookup, and installation validates its machine-local handles. Native
//! compilation consumes only the prepared definitions. `HashMap` checks equality
//! even when hashes collide. The registry owns no image: installed machines,
//! parcels, and active compiles hold strong references. Access removes a dead
//! key immediately; an amortized sweep reclaims dead keys in other buckets.
//! An in-flight entry holds only the election and wakeup state for callers
//! compiling after machine checkout release.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError, Weak};

use tidepool_repr::execution_schema::{
    CertifiedGroup, CertifiedGroupCode, LinkedProgram, PreparedProgram,
};

use super::CompiledProgram;

/// Shared by every [`super::machine::PreparedMachine`] of one run. Cheap to
/// clone the `Arc` around; the registry itself owns a lock, not a checkout.
#[derive(Default)]
pub struct ImageRegistry {
    entries: Mutex<Entries>,
    /// Lifetime lookups that found an existing image. Never decremented.
    hits: AtomicU64,
    /// Lifetime lookups or admissions that elected a compiler. Never decremented.
    misses: AtomicU64,
}

#[derive(Default)]
struct Entries {
    images: HashMap<ImageKey, Entry>,
    since_sweep: usize,
}

/// Distinct content domains share one weak registry and one flight protocol.
/// A certified group key includes its exact home/version, original ordinal,
/// neutral definitions and declared import contracts. Machine-local owner
/// ids and handles are checked at installation and cannot fragment this key.
#[derive(Clone, Eq, Hash, PartialEq)]
enum ImageKey {
    Program(PreparedProgram),
    LiteralProgram(
        PreparedProgram,
        super::package_literals::GroupPackageLiterals,
    ),
    Group(CertifiedGroupCode),
    LiteralGroup(
        CertifiedGroupCode,
        super::package_literals::GroupPackageLiterals,
    ),
}

enum Entry {
    Ready(Weak<CompiledProgram>),
    Compiling(Arc<Flight>),
}

#[derive(Default)]
struct Flight {
    finished: Mutex<bool>,
    changed: Condvar,
}

impl Flight {
    fn wait(&self) {
        let mut finished = self.finished.lock().unwrap_or_else(PoisonError::into_inner);
        while !*finished {
            finished = self
                .changed
                .wait(finished)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    fn finish(&self) {
        *self.finished.lock().unwrap_or_else(PoisonError::into_inner) = true;
        self.changed.notify_all();
    }
}

/// The elected compiler's publication obligation. Dropping it on a returned
/// error or panic clears the candidate and wakes followers to retry.
struct CompileLease<'a> {
    registry: &'a ImageRegistry,
    key: ImageKey,
    flight: Arc<Flight>,
    published: bool,
}

impl CompileLease<'_> {
    fn publish(mut self, image: Arc<CompiledProgram>) -> Arc<CompiledProgram> {
        let mut entries = self
            .registry
            .entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let (selected, publish) = match entries.images.get(&self.key) {
            Some(Entry::Ready(existing)) => (existing.upgrade().unwrap_or(image), true),
            Some(Entry::Compiling(current)) if Arc::ptr_eq(current, &self.flight) => (image, true),
            Some(Entry::Compiling(_)) => (image, false),
            None => (image, true),
        };
        if publish {
            entries
                .images
                .insert(self.key.clone(), Entry::Ready(Arc::downgrade(&selected)));
        }
        entries.tick();
        self.published = true;
        drop(entries);
        self.flight.finish();
        selected
    }
}

impl Drop for CompileLease<'_> {
    fn drop(&mut self) {
        if self.published {
            return;
        }
        let mut entries = self
            .registry
            .entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if matches!(entries.images.get(&self.key), Some(Entry::Compiling(current)) if Arc::ptr_eq(current, &self.flight))
        {
            entries.images.remove(&self.key);
        }
        entries.tick();
        drop(entries);
        self.flight.finish();
    }
}

impl Entries {
    /// A full scan after at least `len` indexed operations makes dead-key
    /// collection amortized constant work per operation. Exact-key dead
    /// entries are removed immediately, including between sweeps.
    fn tick(&mut self) {
        self.since_sweep += 1;
        if self.since_sweep >= self.images.len().max(1) {
            self.images.retain(|_, entry| match entry {
                Entry::Ready(image) => image.strong_count() != 0,
                Entry::Compiling(_) => true,
            });
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
        let key = ImageKey::Program(key.prepared().clone());
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        let found = match entries.images.get(&key) {
            Some(Entry::Ready(image)) => image.upgrade(),
            _ => None,
        };
        if matches!(entries.images.get(&key), Some(Entry::Ready(_))) && found.is_none() {
            entries.images.remove(&key);
        }
        entries.tick();
        if found.is_some() {
            self.hits.fetch_add(1, Ordering::Relaxed);
        } else {
            self.misses.fetch_add(1, Ordering::Relaxed);
        }
        found
    }

    /// Register `image` as the compiled form of `key`. A live first writer
    /// wins, including against an in-flight compiler. This nonblocking entry
    /// point is for callers that cannot wait while holding machine checkout.
    #[must_use]
    pub fn insert(&self, key: LinkedProgram, image: Arc<CompiledProgram>) -> Arc<CompiledProgram> {
        let key = ImageKey::Program(key.prepared().clone());
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(existing) = match entries.images.get(&key) {
            Some(Entry::Ready(image)) => image.upgrade(),
            _ => None,
        } {
            entries.tick();
            return existing;
        }
        let displaced = entries
            .images
            .insert(key, Entry::Ready(Arc::downgrade(&image)));
        entries.tick();
        drop(entries);
        if let Some(Entry::Compiling(flight)) = displaced {
            flight.finish();
        }
        image
    }

    /// Share one in-flight compile for this exact linked program. Call only
    /// after releasing a machine checkout: followers may wait while another
    /// thread compiles. The closure runs outside the registry lock; an error
    /// or panic drops the lease and wakes followers to elect another compiler.
    pub fn get_or_compile<E>(
        &self,
        key: &LinkedProgram,
        compile: impl FnOnce() -> Result<Arc<CompiledProgram>, E>,
    ) -> Result<Arc<CompiledProgram>, E> {
        self.get_or_compile_key(ImageKey::Program(key.prepared().clone()), compile)
    }

    /// Share target code compiled from validated definitions before live
    /// imports are selected. Installation still checks every owner and handle.
    pub fn get_or_compile_prepared<E>(
        &self,
        prepared: &PreparedProgram,
        compile: impl FnOnce() -> Result<Arc<CompiledProgram>, E>,
    ) -> Result<Arc<CompiledProgram>, E> {
        self.get_or_compile_key(ImageKey::Program(prepared.clone()), compile)
    }

    pub(super) fn get_or_compile_literal_prepared<E>(
        &self,
        prepared: &PreparedProgram,
        literals: &super::package_literals::GroupPackageLiterals,
        compile: impl FnOnce() -> Result<Arc<CompiledProgram>, E>,
    ) -> Result<Arc<CompiledProgram>, E> {
        if literals.iter().next().is_none() {
            return self.get_or_compile_prepared(prepared, compile);
        }
        self.get_or_compile_key(
            ImageKey::LiteralProgram(prepared.clone(), literals.clone()),
            compile,
        )
    }

    /// Share an exact worker-certified source group across concurrent native
    /// demand. Failed or panicked compiles release their flight for retry.
    pub fn get_or_compile_group<E>(
        &self,
        group: &CertifiedGroup,
        compile: impl FnOnce() -> Result<Arc<CompiledProgram>, E>,
    ) -> Result<Arc<CompiledProgram>, E> {
        self.get_or_compile_group_code(&group.code_identity(), compile)
    }

    pub fn get_or_compile_group_code<E>(
        &self,
        group: &CertifiedGroupCode,
        compile: impl FnOnce() -> Result<Arc<CompiledProgram>, E>,
    ) -> Result<Arc<CompiledProgram>, E> {
        self.get_or_compile_key(ImageKey::Group(group.clone()), compile)
    }

    pub(super) fn get_or_compile_literal_group<E>(
        &self,
        group: &CertifiedGroupCode,
        literals: &super::package_literals::GroupPackageLiterals,
        compile: impl FnOnce() -> Result<Arc<CompiledProgram>, E>,
    ) -> Result<Arc<CompiledProgram>, E> {
        if literals.iter().next().is_none() {
            return self.get_or_compile_group_code(group, compile);
        }
        self.get_or_compile_key(
            ImageKey::LiteralGroup(group.clone(), literals.clone()),
            compile,
        )
    }

    fn get_or_compile_key<E>(
        &self,
        key: ImageKey,
        compile: impl FnOnce() -> Result<Arc<CompiledProgram>, E>,
    ) -> Result<Arc<CompiledProgram>, E> {
        enum Admission {
            Ready(Arc<CompiledProgram>),
            Wait(Arc<Flight>),
            Compile(Arc<Flight>),
        }
        let mut compile = Some(compile);
        loop {
            let admission = {
                let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
                let admission = match entries.images.get(&key) {
                    Some(Entry::Ready(image)) => image.upgrade().map(Admission::Ready),
                    Some(Entry::Compiling(flight)) => Some(Admission::Wait(Arc::clone(flight))),
                    None => None,
                };
                let admission = admission.unwrap_or_else(|| {
                    let flight = Arc::new(Flight::default());
                    entries
                        .images
                        .insert(key.clone(), Entry::Compiling(Arc::clone(&flight)));
                    Admission::Compile(flight)
                });
                entries.tick();
                admission
            };
            match admission {
                Admission::Ready(image) => {
                    self.hits.fetch_add(1, Ordering::Relaxed);
                    return Ok(image);
                }
                Admission::Wait(flight) => flight.wait(),
                Admission::Compile(flight) => {
                    self.misses.fetch_add(1, Ordering::Relaxed);
                    let lease = CompileLease {
                        registry: self,
                        key: key.clone(),
                        flight,
                        published: false,
                    };
                    let image = compile.take().expect("one compile closure per admission")()?;
                    return Ok(lease.publish(image));
                }
            }
        }
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
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc;
    use std::time::Duration;
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

    fn wait_for_follower(registry: &ImageRegistry, key: &LinkedProgram) {
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        loop {
            let admitted = {
                let entries = registry.entries.lock().unwrap();
                matches!(entries.images.get(&ImageKey::Program(key.prepared().clone())), Some(Entry::Compiling(flight)) if Arc::strong_count(flight) >= 3)
            };
            if admitted {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "follower did not join flight"
            );
            std::thread::yield_now();
        }
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
    fn linked_import_snapshots_share_immutable_code() {
        use tidepool_repr::execution_schema::{
            GlobalDecl, ImportedValue, RuntimeRep, SymbolIdentity,
        };

        let identity = SymbolIdentity {
            unit: "fixture".into(),
            module: "Imports".into(),
            namespace: "value".into(),
            occurrence: "shared".into(),
            record_parent: None,
        };
        let mut wire = testing::wire_program();
        wire.globals.push(GlobalDecl {
            identity: identity.clone(),
            rep: RuntimeRep::LiftedRef,
            entry_signature: None,
            required_evaluated: false,
            required_generation: None,
        });
        let prepared = testing::prepare(wire).unwrap();
        let link = |generation, evaluated| {
            link_program(
                prepared.clone(),
                &MachineImports {
                    values: [(
                        identity.clone(),
                        ImportedValue {
                            identity: identity.clone(),
                            rep: RuntimeRep::LiftedRef,
                            entry_signature: None,
                            generation,
                            evaluated,
                        },
                    )]
                    .into(),
                },
            )
            .unwrap()
        };
        let first = link(1, false);
        let later = link(2, true);
        assert_ne!(first, later, "the installation snapshots remain distinct");
        let registry = ImageRegistry::new();
        let image = registry
            .get_or_compile(&first, || CompiledProgram::compile(&first).map(Arc::new))
            .unwrap();
        let reused = registry
            .get_or_compile(&later, || -> Result<_, super::super::CompileError> {
                panic!("mutable import state must not recompile immutable code")
            })
            .unwrap();
        assert!(Arc::ptr_eq(&image, &reused));
        assert_eq!(registry.misses(), 1);
        assert_eq!(registry.hits(), 1);
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

    #[test]
    fn concurrent_requests_compile_once_and_share_the_winner() {
        let registry = Arc::new(ImageRegistry::new());
        let key = program();
        let compiles = Arc::new(AtomicUsize::new(0));
        let (started_send, started_recv) = mpsc::channel();
        let (release_send, release_recv) = mpsc::channel();
        let leader_registry = Arc::clone(&registry);
        let leader_key = key.clone();
        let leader_compiles = Arc::clone(&compiles);
        let first = std::thread::spawn(move || {
            leader_registry
                .get_or_compile(&leader_key, || {
                    leader_compiles.fetch_add(1, Ordering::SeqCst);
                    started_send.send(()).unwrap();
                    release_recv.recv().unwrap();
                    Ok::<_, ()>(compiled())
                })
                .unwrap()
        });
        started_recv.recv_timeout(Duration::from_secs(2)).unwrap();
        let follower_registry = Arc::clone(&registry);
        let follower_key = key.clone();
        let follower_compiles = Arc::clone(&compiles);
        let second = std::thread::spawn(move || {
            follower_registry
                .get_or_compile(&follower_key, || {
                    follower_compiles.fetch_add(1, Ordering::SeqCst);
                    Ok::<_, ()>(compiled())
                })
                .unwrap()
        });
        wait_for_follower(&registry, &key);
        release_send.send(()).unwrap();
        let first = first.join().unwrap();
        let second = second.join().unwrap();
        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(compiles.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn nonblocking_insert_wins_over_an_in_flight_compile() {
        let registry = Arc::new(ImageRegistry::new());
        let key = program();
        let (started_send, started_recv) = mpsc::channel();
        let (release_send, release_recv) = mpsc::channel();
        let leader_registry = Arc::clone(&registry);
        let leader_key = key.clone();
        let leader = std::thread::spawn(move || {
            leader_registry
                .get_or_compile(&leader_key, || {
                    started_send.send(()).unwrap();
                    release_recv.recv().unwrap();
                    Ok::<_, ()>(compiled())
                })
                .unwrap()
        });
        started_recv.recv_timeout(Duration::from_secs(2)).unwrap();
        let inserted = registry.insert(key.clone(), compiled());
        release_send.send(()).unwrap();
        let elected = leader.join().unwrap();
        assert!(Arc::ptr_eq(&inserted, &elected));
        assert!(Arc::ptr_eq(&inserted, &registry.lookup(&key).unwrap()));
    }

    #[test]
    fn panicked_compiler_wakes_follower_to_retry() {
        let registry = Arc::new(ImageRegistry::new());
        let key = program();
        let (started_send, started_recv) = mpsc::channel();
        let (release_send, release_recv) = mpsc::channel();
        let leader_registry = Arc::clone(&registry);
        let leader_key = key.clone();
        let leader = std::thread::spawn(move || {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                leader_registry.get_or_compile(
                    &leader_key,
                    || -> Result<Arc<CompiledProgram>, ()> {
                        started_send.send(()).unwrap();
                        release_recv.recv().unwrap();
                        panic!("injected compiler panic");
                    },
                )
            }))
        });
        started_recv.recv_timeout(Duration::from_secs(2)).unwrap();
        let (result_send, result_recv) = mpsc::channel();
        let follower_registry = Arc::clone(&registry);
        let follower_key = key.clone();
        let follower = std::thread::spawn(move || {
            let image = follower_registry
                .get_or_compile(&follower_key, || Ok::<_, ()>(compiled()))
                .unwrap();
            result_send.send(image).unwrap();
        });
        wait_for_follower(&registry, &key);
        release_send.send(()).unwrap();
        assert!(leader.join().unwrap().is_err());
        assert!(result_recv.recv_timeout(Duration::from_secs(2)).is_ok());
        follower.join().unwrap();
    }

    #[test]
    fn failed_compiler_wakes_follower_to_retry() {
        let registry = Arc::new(ImageRegistry::new());
        let key = program();
        let (started_send, started_recv) = mpsc::channel();
        let (release_send, release_recv) = mpsc::channel();
        let leader_registry = Arc::clone(&registry);
        let leader_key = key.clone();
        let leader = std::thread::spawn(move || {
            leader_registry.get_or_compile(&leader_key, || {
                started_send.send(()).unwrap();
                release_recv.recv().unwrap();
                Err::<Arc<CompiledProgram>, _>("injected compile error")
            })
        });
        started_recv.recv_timeout(Duration::from_secs(2)).unwrap();
        let (result_send, result_recv) = mpsc::channel();
        let follower_registry = Arc::clone(&registry);
        let follower_key = key.clone();
        let follower = std::thread::spawn(move || {
            let image = follower_registry
                .get_or_compile(&follower_key, || Ok::<_, &'static str>(compiled()))
                .unwrap();
            result_send.send(image).unwrap();
        });
        wait_for_follower(&registry, &key);
        release_send.send(()).unwrap();
        assert!(leader.join().unwrap().is_err());
        assert!(result_recv.recv_timeout(Duration::from_secs(2)).is_ok());
        follower.join().unwrap();
    }
}

#[cfg(test)]
#[path = "image_registry/cost_tests.rs"]
mod cost_tests;
