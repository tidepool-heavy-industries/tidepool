//! Cooperative interruption of host work in the owning compiler scope.

use std::io::{self, Read};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// Producer work in the compiler-input preparation path. These observations
/// carry no compiler, selection or runtime authority.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(usize)]
pub enum HostWorkCounter {
    CompileViewCapture,
    CompileViewBuild,
    CompileViewDigestInputByte,
    InjectedValueVisit,
    ValueInterfaceCapture,
    ReachableValueVisit,
    InspectionInputCapture,
    ActorCompileViewCapture,
    ActorSourcePrepare,
    PreparedInjectedNameVisit,
    WorldKeyFactRead,
    WorldBorrowHit,
    WorldEnsureResidentProbe,
    WorldEnsureResidentHit,
    WorldEnsureNeedsAcquisition,
    WorldEnsureComplete,
    WorldEnsureCompleteReady,
    ColdBootstrap,
    RetainedValueSupportMerge,
    RetainedLexicalRowCopy,
    ColdWorldBuilderRowVisit,
    ColdWorldCompilerBasisCapture,
    SemanticArtifactRowVisit,
    ArtifactReadRowVisit,
    ArtifactMetadataRowVisit,
    InputProjectionBindingVisit,
    InputProjectionProofVisit,
    InputProjectionProofEdgeVisit,
    InputProjectionClosureNode,
    InputProjectionClosureEdge,
    DemandSemanticRoleVisit,
    DemandNativeAvailabilityVisit,
    DemandNativeGroupVisit,
    CompilerMetadataSnapshot,
    SemanticEncoding,
    SemanticEncodedByte,
    SemanticDigestInputByte,
    ScopeEncodedByte,
    InputFdEnumeration,
    InputFdSelectedRowVisit,
    InputFdSliceVisit,
    /// Physical graph entries examined or digest lookups attempted, including misses.
    MaterializationGraphVisit,
    /// Distinct selected graph transports found during FD enumeration.
    /// Unrelated retained graphs are not included.
    InputFdSelectedGraphTransport,
    InputFdMetadataVisit,
    InputFdExport,
    ColdSourceFile,
    ColdSourceByte,
}

const HOST_WORK_COUNTERS: usize = HostWorkCounter::ColdSourceByte as usize + 1;

impl HostWorkCounter {
    pub const ALL: [Self; HOST_WORK_COUNTERS] = [
        Self::CompileViewCapture,
        Self::CompileViewBuild,
        Self::CompileViewDigestInputByte,
        Self::InjectedValueVisit,
        Self::ValueInterfaceCapture,
        Self::ReachableValueVisit,
        Self::InspectionInputCapture,
        Self::ActorCompileViewCapture,
        Self::ActorSourcePrepare,
        Self::PreparedInjectedNameVisit,
        Self::WorldKeyFactRead,
        Self::WorldBorrowHit,
        Self::WorldEnsureResidentProbe,
        Self::WorldEnsureResidentHit,
        Self::WorldEnsureNeedsAcquisition,
        Self::WorldEnsureComplete,
        Self::WorldEnsureCompleteReady,
        Self::ColdBootstrap,
        Self::RetainedValueSupportMerge,
        Self::RetainedLexicalRowCopy,
        Self::ColdWorldBuilderRowVisit,
        Self::ColdWorldCompilerBasisCapture,
        Self::SemanticArtifactRowVisit,
        Self::ArtifactReadRowVisit,
        Self::ArtifactMetadataRowVisit,
        Self::InputProjectionBindingVisit,
        Self::InputProjectionProofVisit,
        Self::InputProjectionProofEdgeVisit,
        Self::InputProjectionClosureNode,
        Self::InputProjectionClosureEdge,
        Self::DemandSemanticRoleVisit,
        Self::DemandNativeAvailabilityVisit,
        Self::DemandNativeGroupVisit,
        Self::CompilerMetadataSnapshot,
        Self::SemanticEncoding,
        Self::SemanticEncodedByte,
        Self::SemanticDigestInputByte,
        Self::ScopeEncodedByte,
        Self::InputFdEnumeration,
        Self::InputFdSelectedRowVisit,
        Self::InputFdSliceVisit,
        Self::MaterializationGraphVisit,
        Self::InputFdSelectedGraphTransport,
        Self::InputFdMetadataVisit,
        Self::InputFdExport,
        Self::ColdSourceFile,
        Self::ColdSourceByte,
    ];
}

thread_local! {
    static HOST_WORK: std::cell::RefCell<Vec<Arc<HostWorkObservation>>> = const {
        std::cell::RefCell::new(Vec::new())
    };
}

/// One explicitly observed synchronous compiler-input operation. Call `scope`
/// at each existing machine/blocking boundary when the operation moves threads.
/// Inactive production paths do not retain an observer or update atomics.
#[derive(Debug)]
pub struct HostWorkObservation {
    counts: [AtomicU64; HOST_WORK_COUNTERS],
}

impl HostWorkObservation {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            counts: std::array::from_fn(|_| AtomicU64::new(0)),
        })
    }

    pub fn when_timing_enabled() -> Option<Arc<Self>> {
        tracing::enabled!(target: "exomonad_harness::timing", tracing::Level::DEBUG).then(Self::new)
    }

    pub fn scope<T>(self: &Arc<Self>, action: impl FnOnce() -> T) -> T {
        struct Scope;
        impl Drop for Scope {
            fn drop(&mut self) {
                HOST_WORK.with(|active| {
                    active.borrow_mut().pop();
                });
            }
        }
        HOST_WORK.with(|active| active.borrow_mut().push(Arc::clone(self)));
        let _scope = Scope;
        action()
    }

    pub fn snapshot(&self) -> HostWorkSnapshot {
        HostWorkSnapshot {
            counts: std::array::from_fn(|index| self.counts[index].load(Ordering::Relaxed)),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostWorkSnapshot {
    counts: [u64; HOST_WORK_COUNTERS],
}

impl HostWorkSnapshot {
    pub fn get(&self, counter: HostWorkCounter) -> u64 {
        self.counts[counter as usize]
    }

    /// Emit every counter, including zeroes, in the caller's existing query
    /// span. Only a settled observation can establish complete operation work.
    pub fn emit(&self) {
        for counter in HostWorkCounter::ALL {
            tracing::debug!(target: "exomonad_harness::timing",
                phase = "compiler_input_work", counter = ?counter,
                count = self.get(counter), "compiler input work");
        }
    }
}

pub fn record_host_work(counter: HostWorkCounter, amount: u64) {
    HOST_WORK.with(|active| {
        let active = active.borrow();
        for (index, observer) in active.iter().enumerate() {
            // Nested calls can reuse the same observation. Count the producer
            // once there while preserving inclusive outer observations.
            if active[..index]
                .iter()
                .any(|previous| Arc::ptr_eq(previous, observer))
            {
                continue;
            }
            let _ = observer.counts[counter as usize].fetch_update(
                Ordering::Relaxed,
                Ordering::Relaxed,
                |previous| Some(previous.saturating_add(amount)),
            );
        }
    });
}

pub(crate) fn checkpoint() -> io::Result<()> {
    tidepool_extract_cmd::compiler_host_checkpoint()
}

/// Wait for an immutable-input owner without losing the compiler stop edge.
/// The callback observes a blocked acquisition; it cannot alter lock policy.
pub(crate) fn lock_with_wait<'a, T>(
    owner: &'a std::sync::Mutex<T>,
    mut waiting: impl FnMut(),
) -> io::Result<std::sync::MutexGuard<'a, T>> {
    loop {
        checkpoint()?;
        match owner.try_lock() {
            Ok(held) => return Ok(held),
            Err(std::sync::TryLockError::WouldBlock) => {
                waiting();
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
            Err(std::sync::TryLockError::Poisoned(_)) => panic!("immutable input owner lock"),
        }
    }
}

/// Check cancellation between bounded reads. An individual filesystem call
/// still belongs to the OS and cannot be preempted by this cooperative edge.
pub(crate) fn read_to_end(reader: &mut impl Read, bytes: &mut Vec<u8>) -> io::Result<()> {
    let mut chunk = [0; 64 * 1024];
    loop {
        checkpoint()?;
        let count = match reader.read(&mut chunk) {
            Ok(count) => count,
            // Retry a transient syscall interruption through the scope's next
            // checkpoint; only that owner can turn a stop into a refusal.
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        if count == 0 {
            return checkpoint();
        }
        bytes.extend_from_slice(&chunk[..count]);
    }
}

pub(crate) fn read(path: &std::path::Path) -> io::Result<Vec<u8>> {
    checkpoint()?;
    let mut file = std::fs::File::open(path)?;
    let mut bytes = Vec::new();
    read_to_end(&mut file, &mut bytes)?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tidepool_extract_cmd::{
        with_compiler_transaction_cancellable, CompilerTransactionCancellation,
        CompilerTransactionClose,
    };

    #[test]
    fn input_work_observation_follows_explicit_thread_scopes_and_unwind() {
        let outer = HostWorkObservation::new();
        let inner = HostWorkObservation::new();
        outer.scope(|| {
            record_host_work(HostWorkCounter::CompileViewCapture, 1);
            outer.scope(|| {
                inner.scope(|| record_host_work(HostWorkCounter::ReachableValueVisit, 3));
            });
        });
        assert_eq!(outer.snapshot().get(HostWorkCounter::CompileViewCapture), 1);
        assert_eq!(
            outer.snapshot().get(HostWorkCounter::ReachableValueVisit),
            3
        );
        assert_eq!(
            inner.snapshot().get(HostWorkCounter::ReachableValueVisit),
            3
        );
        let thread_observer = Arc::clone(&outer);
        std::thread::spawn(move || {
            thread_observer.scope(|| record_host_work(HostWorkCounter::SemanticEncodedByte, 17));
        })
        .join()
        .unwrap();
        let unwound = std::panic::catch_unwind(|| {
            outer.scope(|| {
                record_host_work(HostWorkCounter::ColdBootstrap, 1);
                panic!("cancelled observation");
            });
        });
        assert!(unwound.is_err());
        record_host_work(HostWorkCounter::ColdBootstrap, 100);
        assert_eq!(
            outer.snapshot().get(HostWorkCounter::SemanticEncodedByte),
            17
        );
        assert_eq!(outer.snapshot().get(HostWorkCounter::ColdBootstrap), 1);
        assert_eq!(
            inner.snapshot().get(HostWorkCounter::SemanticEncodedByte),
            0
        );
    }

    #[test]
    fn cancelled_owner_wait_refuses_before_the_holder_releases() {
        let owner = std::sync::Mutex::new(());
        let held = owner.lock().unwrap();
        let cancellation = CompilerTransactionCancellation::new();
        let (waiting, observed) = std::sync::mpsc::channel();
        std::thread::scope(|scope| {
            let waiting_cancellation = cancellation.clone();
            let task = scope.spawn(|| {
                with_compiler_transaction_cancellable(
                    waiting_cancellation,
                    |_| {},
                    || {
                        let mut waiting = Some(waiting);
                        lock_with_wait(&owner, || {
                            if let Some(waiting) = waiting.take() {
                                waiting.send(()).unwrap();
                            }
                        })
                        .map(drop)
                    },
                )
            });
            observed.recv().unwrap();
            cancellation.cancel();
            let refused = task.join().unwrap();
            assert_eq!(
                refused.action.unwrap_err().kind(),
                io::ErrorKind::Interrupted
            );
            assert_eq!(refused.close, CompilerTransactionClose::NotStarted);
        });
        drop(held);
        drop(lock_with_wait(&owner, || {}).unwrap());
    }

    #[test]
    fn transient_read_interruption_recovers_but_cancelled_retry_refuses() {
        struct InterruptedOnce {
            first: bool,
            cancellation: Option<CompilerTransactionCancellation>,
            bytes: io::Cursor<&'static [u8]>,
        }
        impl Read for InterruptedOnce {
            fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
                if self.first {
                    self.first = false;
                    if let Some(cancellation) = &self.cancellation {
                        cancellation.cancel();
                    }
                    return Err(io::Error::from(io::ErrorKind::Interrupted));
                }
                self.bytes.read(bytes)
            }
        }
        let mut reader = InterruptedOnce {
            first: true,
            cancellation: None,
            bytes: io::Cursor::new(b"complete"),
        };
        let mut bytes = Vec::new();
        let recovered = with_compiler_transaction_cancellable(
            CompilerTransactionCancellation::new(),
            |_| {},
            || read_to_end(&mut reader, &mut bytes),
        );
        recovered.action.unwrap();
        assert_eq!(bytes, b"complete");
        assert_eq!(recovered.close, CompilerTransactionClose::NotStarted);
        let cancellation = CompilerTransactionCancellation::new();
        let mut reader = InterruptedOnce {
            first: true,
            cancellation: Some(cancellation.clone()),
            bytes: io::Cursor::new(b"must not read"),
        };
        let mut bytes = Vec::new();
        let refused = with_compiler_transaction_cancellable(
            cancellation,
            |_| {},
            || read_to_end(&mut reader, &mut bytes),
        );
        assert_eq!(
            refused.action.unwrap_err().kind(),
            io::ErrorKind::Interrupted
        );
        assert!(bytes.is_empty());
        assert_eq!(reader.bytes.position(), 0);
        assert_eq!(refused.close, CompilerTransactionClose::NotStarted);
    }

    #[test]
    fn chunked_host_read_interrupts_between_reads_and_fresh_scope_recovers() {
        struct CancelAfterRead {
            cancellation: CompilerTransactionCancellation,
            reads: usize,
        }
        impl Read for CancelAfterRead {
            fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
                self.reads += 1;
                bytes.fill(7);
                self.cancellation.cancel();
                Ok(bytes.len())
            }
        }
        let cancellation = CompilerTransactionCancellation::new();
        let mut reader = CancelAfterRead {
            cancellation: cancellation.clone(),
            reads: 0,
        };
        let mut bytes = Vec::new();
        let outcome = with_compiler_transaction_cancellable(
            cancellation,
            |_| {},
            || read_to_end(&mut reader, &mut bytes),
        );
        assert_eq!(
            outcome.action.unwrap_err().kind(),
            io::ErrorKind::Interrupted
        );
        assert_eq!(reader.reads, 1);
        assert_eq!(bytes.len(), 64 * 1024);
        assert_eq!(outcome.close, CompilerTransactionClose::NotStarted);
        let mut recovered = Vec::new();
        let outcome = with_compiler_transaction_cancellable(
            CompilerTransactionCancellation::new(),
            |_| {},
            || read_to_end(&mut io::Cursor::new(b"complete"), &mut recovered),
        );
        outcome.action.unwrap();
        assert_eq!(recovered, b"complete");
        assert_eq!(outcome.close, CompilerTransactionClose::NotStarted);
    }
}
