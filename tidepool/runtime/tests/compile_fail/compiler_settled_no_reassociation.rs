use tidepool_runtime::{CompilerTransactionOutcome, CompilerTransactionSettled};

fn reassociate(
    mut product: CompilerTransactionSettled<usize>,
    outcome: CompilerTransactionOutcome<usize>,
) {
    product.outcome = outcome;
}

fn main() {}
