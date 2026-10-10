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
use std::time::Instant;

use tidepool_repr::execution_schema::{
    CertifiedGroup, CertifiedGroupCode, LinkedProgram, PreparedProgram,
};

use super::CompiledProgram;

/// Shared by every [`super::machine::PreparedMachine`] of one run. Cheap to
/// clone the `Arc` around; the registry itself owns a lock, not a checkout.
pub struct ImageRegistry {
    identity: u64,
    next_entry: AtomicU64,
    next_observation: AtomicU64,
    next_compile: AtomicU64,
    entries: Mutex<Entries>,
    /// Lifetime lookups that found an existing image. Never decremented.
    hits: AtomicU64,
    /// Lifetime unavailable lookups or admissions that elected a compiler. Never decremented.
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

impl ImageKey {
    fn prepared(
        prepared: &PreparedProgram,
        literals: &super::package_literals::GroupPackageLiterals,
    ) -> Self {
        if literals.iter().next().is_none() {
            Self::Program(prepared.clone())
        } else {
            Self::LiteralProgram(prepared.clone(), literals.clone())
        }
    }

    fn group(
        group: &CertifiedGroupCode,
        literals: &super::package_literals::GroupPackageLiterals,
    ) -> Self {
        if literals.iter().next().is_none() {
            Self::Group(group.clone())
        } else {
            Self::LiteralGroup(group.clone(), literals.clone())
        }
    }
}

enum Entry {
    Ready(ReadyImage),
    Compiling(Arc<Flight>),
}

struct ReadyImage {
    image: Weak<CompiledProgram>,
    entry: u64,
}

struct Flight {
    entry: u64,
    compile: u64,
    finished: Mutex<bool>,
    changed: Condvar,
    outcome: AtomicU64,
}

static NEXT_REGISTRY: AtomicU64 = AtomicU64::new(1);

impl Default for ImageRegistry {
    fn default() -> Self {
        Self {
            identity: NEXT_REGISTRY.fetch_add(1, Ordering::Relaxed),
            next_entry: AtomicU64::new(1),
            next_observation: AtomicU64::new(1),
            next_compile: AtomicU64::new(1),
            entries: Mutex::default(),
            hits: AtomicU64::default(),
            misses: AtomicU64::default(),
        }
    }
}

/// Linux's monotonic clock also used by the worker. Failed clock reads remain
/// absent; elapsed durations use Instant independently. No image graph is hashed
/// or formatted for these bounded observations.
fn monotonic_ns() -> Option<u64> {
    let mut value = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: clock_gettime writes only the live, correctly aligned timespec.
    (unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut value) } == 0)
        .then(|| (value.tv_sec as u64) * 1_000_000_000 + value.tv_nsec as u64)
}

#[derive(Clone, Copy)]
enum Disposition {
    LiveHit,
    Absent,
    Expired,
    InFlight,
    ElectedNew,
    ElectedExpired,
    Inserted,
}

impl Disposition {
    fn label(self) -> &'static str {
        match self {
            Self::LiveHit => "live_hit",
            Self::Absent => "absent",
            Self::Expired => "expired",
            Self::InFlight => "in_flight",
            Self::ElectedNew => "elected_absent",
            Self::ElectedExpired => "elected_after_expiry",
            Self::Inserted => "inserted",
        }
    }
}

struct Observation<'a> {
    registry: &'a ImageRegistry,
    sequence: u64,
    operation: &'static str,
    domain: &'static str,
    started: Instant,
    start_ns: Option<u64>,
    entry: Option<u64>,
    image: Option<u64>,
    disposition: Disposition,
    outcome: &'static str,
    waits: u64,
    wait_ns: u64,
    failed_flights: u64,
    abandoned_flights: u64,
}

impl<'a> Observation<'a> {
    fn new(registry: &'a ImageRegistry, operation: &'static str, key: &ImageKey) -> Self {
        let domain = match key {
            ImageKey::Program(_) => "program",
            ImageKey::LiteralProgram(..) => "literal_program",
            ImageKey::Group(_) => "group",
            ImageKey::LiteralGroup(..) => "literal_group",
        };
        Self {
            registry,
            sequence: registry.next_observation.fetch_add(1, Ordering::Relaxed),
            operation,
            domain,
            started: Instant::now(),
            start_ns: monotonic_ns(),
            entry: None,
            image: None,
            disposition: Disposition::Absent,
            outcome: "abandoned",
            waits: 0,
            wait_ns: 0,
            failed_flights: 0,
            abandoned_flights: 0,
        }
    }
}

impl Drop for Observation<'_> {
    fn drop(&mut self) {
        tracing::info!(target: "tidepool_codegen::image_registry",
            schema = 1, process_id = std::process::id(), clock_domain = "CLOCK_MONOTONIC",
            observation_sequence = self.sequence, image_registry = self.registry.identity,
            operation = self.operation, key_domain = self.domain, image_entry = self.entry,
            image_instance = self.image, disposition = self.disposition.label(),
            outcome = self.outcome, start_ns = self.start_ns, end_ns = monotonic_ns(),
            wall_ns = self.started.elapsed().as_nanos() as u64,
            shared_wait_count = self.waits, shared_wait_ns = self.wait_ns,
            failed_flights_observed = self.failed_flights, abandoned_flights_observed = self.abandoned_flights,
            "native image registry decision");
    }
}

#[derive(Clone, Copy)]
enum FlightOutcome {
    Published = 1,
    Failed = 2,
    Abandoned = 3,
    Superseded = 4,
}

impl Flight {
    fn wait(&self) -> u64 {
        let mut finished = self.finished.lock().unwrap_or_else(PoisonError::into_inner);
        while !*finished {
            finished = self
                .changed
                .wait(finished)
                .unwrap_or_else(PoisonError::into_inner);
        }
        self.outcome.load(Ordering::Relaxed)
    }

    fn finish(&self, outcome: FlightOutcome) {
        self.outcome.store(outcome as u64, Ordering::Relaxed);
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
    failed: bool,
    started: Instant,
    start_ns: Option<u64>,
}

impl CompileLease<'_> {
    fn publish(mut self, image: Arc<CompiledProgram>) -> Arc<CompiledProgram> {
        let mut entries = self
            .registry
            .entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let (selected, entry, publish) = match entries.images.get(&self.key) {
            Some(Entry::Ready(existing)) => (
                existing.image.upgrade().unwrap_or(image),
                existing.entry,
                true,
            ),
            Some(Entry::Compiling(current)) if Arc::ptr_eq(current, &self.flight) => {
                (image, current.entry, true)
            }
            Some(Entry::Compiling(_)) => (image, self.flight.entry, false),
            None => (image, self.flight.entry, true),
        };
        if publish {
            entries.images.insert(
                self.key.clone(),
                Entry::Ready(ReadyImage {
                    image: Arc::downgrade(&selected),
                    entry,
                }),
            );
        }
        entries.tick();
        self.published = true;
        drop(entries);
        self.record(
            if publish { "published" } else { "superseded" },
            Some(selected.image_instance_id()),
        );
        self.flight.finish(FlightOutcome::Published);
        selected
    }

    fn record(&self, outcome: &'static str, image: Option<u64>) {
        tracing::info!(target: "tidepool_codegen::image_registry",
            schema = 1, process_id = std::process::id(), clock_domain = "CLOCK_MONOTONIC",
            image_registry = self.registry.identity, image_entry = self.flight.entry,
            image_compile = self.flight.compile, image_instance = image, outcome,
            start_ns = self.start_ns, end_ns = monotonic_ns(),
            wall_ns = self.started.elapsed().as_nanos() as u64,
            "native image producer settled");
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
        self.flight.finish(if self.failed {
            FlightOutcome::Failed
        } else {
            FlightOutcome::Abandoned
        });
        self.record(if self.failed { "failed" } else { "abandoned" }, None);
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
                Entry::Ready(image) => image.image.strong_count() != 0,
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
        self.lookup_key(ImageKey::Program(key.prepared().clone()))
    }

    pub(super) fn lookup_literal_prepared(
        &self,
        prepared: &PreparedProgram,
        literals: &super::package_literals::GroupPackageLiterals,
    ) -> Option<Arc<CompiledProgram>> {
        self.lookup_key(ImageKey::prepared(prepared, literals))
    }

    pub(super) fn lookup_literal_group(
        &self,
        group: &CertifiedGroupCode,
        literals: &super::package_literals::GroupPackageLiterals,
    ) -> Option<Arc<CompiledProgram>> {
        self.lookup_key(ImageKey::group(group, literals))
    }

    /// Never elect a compiler or join a flight. The registry lock protects only
    /// the weak image lookup; a missing, expired or in-flight key is unavailable.
    fn lookup_key(&self, key: ImageKey) -> Option<Arc<CompiledProgram>> {
        let mut observation = Observation::new(self, "lookup", &key);
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        let found = match entries.images.get(&key) {
            Some(Entry::Ready(image)) => {
                observation.entry = Some(image.entry);
                let found = image.image.upgrade();
                observation.disposition = if found.is_some() {
                    Disposition::LiveHit
                } else {
                    Disposition::Expired
                };
                found
            }
            Some(Entry::Compiling(flight)) => {
                observation.entry = Some(flight.entry);
                observation.disposition = Disposition::InFlight;
                None
            }
            None => None,
        };
        if matches!(entries.images.get(&key), Some(Entry::Ready(_))) && found.is_none() {
            entries.images.remove(&key);
        }
        entries.tick();
        if let Some(image) = &found {
            self.hits.fetch_add(1, Ordering::Relaxed);
            observation.image = Some(image.image_instance_id());
            observation.outcome = "ready";
        } else {
            self.misses.fetch_add(1, Ordering::Relaxed);
            observation.outcome = "unavailable";
        }
        drop(entries);
        found
    }

    /// Register `image` as the compiled form of `key`. A live first writer
    /// wins, including against an in-flight compiler. This nonblocking entry
    /// point is for callers that cannot wait while holding machine checkout.
    #[must_use]
    pub fn insert(&self, key: LinkedProgram, image: Arc<CompiledProgram>) -> Arc<CompiledProgram> {
        let key = ImageKey::Program(key.prepared().clone());
        let mut observation = Observation::new(self, "insert", &key);
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((existing, entry)) = match entries.images.get(&key) {
            Some(Entry::Ready(image)) => image.image.upgrade().map(|live| (live, image.entry)),
            _ => None,
        } {
            entries.tick();
            observation.entry = Some(entry);
            observation.image = Some(existing.image_instance_id());
            observation.disposition = Disposition::LiveHit;
            observation.outcome = "ready";
            drop(entries);
            return existing;
        }
        let entry = match entries.images.get(&key) {
            Some(Entry::Ready(image)) => image.entry,
            Some(Entry::Compiling(flight)) => flight.entry,
            None => self.next_entry.fetch_add(1, Ordering::Relaxed),
        };
        let displaced = entries.images.insert(
            key,
            Entry::Ready(ReadyImage {
                image: Arc::downgrade(&image),
                entry,
            }),
        );
        entries.tick();
        drop(entries);
        if let Some(Entry::Compiling(flight)) = displaced {
            flight.finish(FlightOutcome::Superseded);
        }
        observation.entry = Some(entry);
        observation.image = Some(image.image_instance_id());
        observation.disposition = Disposition::Inserted;
        observation.outcome = "ready";
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
        self.get_or_compile_key(ImageKey::prepared(prepared, literals), compile)
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
        self.get_or_compile_key(ImageKey::group(group, literals), compile)
    }

    fn get_or_compile_key<E>(
        &self,
        key: ImageKey,
        compile: impl FnOnce() -> Result<Arc<CompiledProgram>, E>,
    ) -> Result<Arc<CompiledProgram>, E> {
        enum Admission {
            Ready(Arc<CompiledProgram>, u64),
            Wait(Arc<Flight>),
            Compile(Arc<Flight>, bool),
        }
        let mut observation = Observation::new(self, "get_or_compile", &key);
        let mut compile = Some(compile);
        loop {
            let admission = {
                let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
                let expired = matches!(entries.images.get(&key), Some(Entry::Ready(image)) if image.image.strong_count() == 0);
                let admission = match entries.images.get(&key) {
                    Some(Entry::Ready(image)) => image
                        .image
                        .upgrade()
                        .map(|live| Admission::Ready(live, image.entry)),
                    Some(Entry::Compiling(flight)) => Some(Admission::Wait(Arc::clone(flight))),
                    None => None,
                };
                let admission = admission.unwrap_or_else(|| {
                    let flight = Arc::new(Flight {
                        entry: match entries.images.get(&key) {
                            Some(Entry::Ready(image)) => image.entry,
                            _ => self.next_entry.fetch_add(1, Ordering::Relaxed),
                        },
                        compile: self.next_compile.fetch_add(1, Ordering::Relaxed),
                        finished: Mutex::new(false),
                        changed: Condvar::new(),
                        outcome: AtomicU64::new(0),
                    });
                    entries
                        .images
                        .insert(key.clone(), Entry::Compiling(Arc::clone(&flight)));
                    Admission::Compile(flight, expired)
                });
                entries.tick();
                admission
            };
            match admission {
                Admission::Ready(image, entry) => {
                    self.hits.fetch_add(1, Ordering::Relaxed);
                    observation.entry = Some(entry);
                    observation.image = Some(image.image_instance_id());
                    observation.disposition = Disposition::LiveHit;
                    observation.outcome = "ready";
                    return Ok(image);
                }
                Admission::Wait(flight) => {
                    let wait = Instant::now();
                    observation.waits += 1;
                    match flight.wait() {
                        value if value == FlightOutcome::Failed as u64 => {
                            observation.failed_flights += 1
                        }
                        value if value == FlightOutcome::Abandoned as u64 => {
                            observation.abandoned_flights += 1
                        }
                        _ => {}
                    }
                    observation.wait_ns += wait.elapsed().as_nanos() as u64;
                }
                Admission::Compile(flight, expired) => {
                    self.misses.fetch_add(1, Ordering::Relaxed);
                    observation.entry = Some(flight.entry);
                    observation.disposition = if expired {
                        Disposition::ElectedExpired
                    } else {
                        Disposition::ElectedNew
                    };
                    let mut lease = CompileLease {
                        registry: self,
                        key: key.clone(),
                        flight,
                        published: false,
                        failed: false,
                        started: Instant::now(),
                        start_ns: monotonic_ns(),
                    };
                    let span = tracing::info_span!(target: "tidepool_codegen::image_registry", "native_image_producer",
                        image_registry = self.identity, image_entry = lease.flight.entry, image_compile = lease.flight.compile);
                    let _entered = span.enter();
                    match compile.take().expect("one compile closure per admission")() {
                        Ok(image) => {
                            let selected = lease.publish(image);
                            observation.image = Some(selected.image_instance_id());
                            observation.outcome = "ready";
                            return Ok(selected);
                        }
                        Err(error) => {
                            lease.failed = true;
                            observation.outcome = "failed";
                            return Err(error);
                        }
                    }
                }
            }
        }
    }

    /// Lifetime lookups that hit an already-compiled image.
    #[must_use]
    pub fn hits(&self) -> u64 {
        self.hits.load(Ordering::Relaxed)
    }

    /// Lifetime unavailable lookups or admissions that elected a compiler.
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

    #[test]
    fn observations_preserve_live_reuse_expiry_failure_and_abandonment() {
        #[derive(Clone)]
        struct Writer(Arc<Mutex<Vec<u8>>>);
        impl std::io::Write for Writer {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let output = Arc::new(Mutex::new(Vec::new()));
        let writer = Writer(Arc::clone(&output));
        let subscriber = tracing_subscriber::fmt()
            .json()
            .with_ansi(false)
            .with_max_level(tracing::Level::INFO)
            .with_writer(move || writer.clone())
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            let registry = ImageRegistry::new();
            let key = program();
            let first = registry
                .get_or_compile(&key, || Ok::<_, ()>(compiled()))
                .unwrap();
            let reused = registry
                .get_or_compile(&key, || -> Result<_, ()> {
                    panic!("live image must not invoke producer")
                })
                .unwrap();
            assert!(Arc::ptr_eq(&first, &reused));
            drop(first);
            drop(reused);
            assert!(registry
                .get_or_compile(&key, || Err::<Arc<CompiledProgram>, _>(()))
                .is_err());
            let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                registry.get_or_compile(&key, || -> Result<Arc<CompiledProgram>, ()> {
                    panic!("deliberate producer abandonment")
                })
            }));
            assert!(panic.is_err());
            let recovered = registry
                .get_or_compile(&key, || Ok::<_, ()>(compiled()))
                .unwrap();
            assert_eq!(registry.hits(), 1);
            assert_eq!(registry.misses(), 4);
            drop(recovered);
        });
        let bytes = output.lock().unwrap();
        let rows: Vec<serde_json::Value> = std::str::from_utf8(&bytes)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let decisions: Vec<_> = rows
            .iter()
            .filter(|row| row["fields"]["message"] == "native image registry decision")
            .collect();
        assert_eq!(decisions.len(), 5);
        assert_eq!(decisions[1]["fields"]["disposition"], "live_hit");
        assert_eq!(
            decisions[0]["fields"]["image_instance"],
            decisions[1]["fields"]["image_instance"]
        );
        assert_eq!(
            decisions[2]["fields"]["disposition"],
            "elected_after_expiry"
        );
        assert_eq!(
            decisions[0]["fields"]["image_entry"],
            decisions[2]["fields"]["image_entry"]
        );
        let outcomes: Vec<_> = rows
            .iter()
            .filter(|row| row["fields"]["message"] == "native image producer settled")
            .map(|row| row["fields"]["outcome"].as_str().unwrap())
            .collect();
        assert_eq!(outcomes, ["published", "failed", "abandoned", "published"]);
        for row in decisions {
            assert_eq!(row["fields"]["clock_domain"], "CLOCK_MONOTONIC");
            assert!(
                row["fields"]["end_ns"].as_u64().unwrap()
                    >= row["fields"]["start_ns"].as_u64().unwrap()
            );
        }
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
    fn lookup_refuses_absent_expired_and_inflight_without_compiling_or_waiting() {
        let registry = Arc::new(ImageRegistry::new());
        let key = program();
        assert!(registry.lookup(&key).is_none());
        let image = registry.insert(key.clone(), compiled());
        assert!(Arc::ptr_eq(&image, &registry.lookup(&key).unwrap()));
        drop(image);
        assert!(
            registry.lookup(&key).is_none(),
            "weak-only expired image is unavailable"
        );
        let (started, observe) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let producer_registry = registry.clone();
        let producer_key = key.clone();
        let producer = std::thread::spawn(move || {
            producer_registry
                .get_or_compile(&producer_key, || {
                    started.send(()).unwrap();
                    released.recv_timeout(Duration::from_secs(5)).unwrap();
                    Ok::<_, ()>(compiled())
                })
                .unwrap()
        });
        observe.recv_timeout(Duration::from_secs(2)).unwrap();
        let before = CompiledProgram::successful_image_compilations();
        let (looked_up, observed) = mpsc::channel();
        let lookup_registry = registry.clone();
        let lookup = std::thread::spawn(move || {
            looked_up
                .send(lookup_registry.lookup(&key).is_none())
                .unwrap();
        });
        assert!(observed
            .recv_timeout(Duration::from_secs(2))
            .expect("lookup must refuse before producer is released"));
        assert_eq!(CompiledProgram::successful_image_compilations(), before);
        release.send(()).unwrap();
        lookup.join().unwrap();
        let image = producer.join().unwrap();
        assert!(Arc::ptr_eq(&image, &registry.lookup(&program()).unwrap()));
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
