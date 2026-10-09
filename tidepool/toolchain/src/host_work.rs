//! Cooperative interruption of host work in the owning compiler scope.

use std::io::{self, Read};

pub(crate) fn checkpoint() -> io::Result<()> {
    tidepool_extract_cmd::compiler_host_checkpoint()
}

/// Check cancellation between bounded reads. An individual filesystem call
/// still belongs to the OS and cannot be preempted by this cooperative edge.
pub(crate) fn read_to_end(reader: &mut impl Read, bytes: &mut Vec<u8>) -> io::Result<()> {
    let mut chunk = [0; 64 * 1024];
    loop {
        checkpoint()?;
        let count = reader.read(&mut chunk)?;
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
