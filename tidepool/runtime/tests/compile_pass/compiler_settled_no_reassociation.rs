use tidepool_runtime::{CompilerTransactionOutcome, CompilerTransactionSettled};

fn consume(product: CompilerTransactionSettled<usize>, outcome: CompilerTransactionOutcome<usize>) {
    let _ = product.into_outcome();
    let _ = outcome.action;
}

fn main() {}
