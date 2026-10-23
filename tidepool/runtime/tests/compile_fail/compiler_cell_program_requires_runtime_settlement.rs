use tidepool_runtime::session::{PersistentSession, SettledCellProgram};

fn begin(session: &mut PersistentSession, settled: SettledCellProgram) {
    let _ = session.begin_cell_program(settled.program().clone());
}

fn main() {}
