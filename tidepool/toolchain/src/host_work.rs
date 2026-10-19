//! Cooperative interruption of host work in the owning compiler scope.

use std::io::{self, Read};

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
