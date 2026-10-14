use tidepool_runtime::session::turn::{
    finish_cell_program_admitted, CellCheckFailure, PreparedRuntimeCell,
};
use tidepool_runtime::{CompilerTransactionOutcome, HostAdmissionCancellation};

fn publish(
    outcome: CompilerTransactionOutcome<Result<PreparedRuntimeCell, CellCheckFailure>>,
    cancellation: &HostAdmissionCancellation,
) {
    let _ = finish_cell_program_admitted(outcome, cancellation);
}

fn main() {}
