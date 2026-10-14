use tidepool_runtime::CompilerTransactionSettled;

fn consume(outcome: CompilerTransactionSettled<usize>) {
    let _first = outcome.into_outcome();
    let _second = outcome.into_outcome();
}

fn main() {}
