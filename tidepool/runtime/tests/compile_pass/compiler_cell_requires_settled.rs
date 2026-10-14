use tidepool_runtime::session::turn::{
    finish_cell_program_admitted, CellCheckFailure, PreparedRuntimeCell,
};
use tidepool_runtime::CompilerTransactionSettled;

fn publish(outcome: CompilerTransactionSettled<Result<PreparedRuntimeCell, CellCheckFailure>>) {
    let _ = finish_cell_program_admitted(outcome);
}

fn main() {}
