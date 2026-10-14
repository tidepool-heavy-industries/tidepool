use tidepool_runtime::session::resident::{PreparedRenderer, RendererProduction};
use tidepool_runtime::CompilerTransactionSettled;

fn publish(
    producer: RendererProduction<usize>,
    outcome: CompilerTransactionSettled<Result<PreparedRenderer<Result<usize, ()>>, ()>>,
) {
    let _ = producer.settle_native(outcome);
}

fn main() {}
