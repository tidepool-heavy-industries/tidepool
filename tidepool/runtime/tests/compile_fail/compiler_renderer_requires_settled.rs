use tidepool_runtime::session::resident::{PreparedRenderer, RendererProduction};
use tidepool_runtime::CompilerTransactionOutcome;

fn publish(
    producer: RendererProduction<usize>,
    outcome: CompilerTransactionOutcome<Result<PreparedRenderer<usize>, ()>>,
) {
    let _ = producer.settle_native(outcome);
}

fn main() {}
