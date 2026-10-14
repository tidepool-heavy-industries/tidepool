use tidepool_runtime::session::turn::{
    finish_cell_program_admitted, CellCheckFailure, PreparedRuntimeCell,
};
use tidepool_runtime::{CompilerTransactionSettled, HostAdmissionCancellation};

fn publish(
    outcome: CompilerTransactionSettled<Result<PreparedRuntimeCell, CellCheckFailure>>,
    cancellation: &HostAdmissionCancellation,
) {
    let _ = finish_cell_program_admitted(outcome, cancellation);
}

fn main() {}
