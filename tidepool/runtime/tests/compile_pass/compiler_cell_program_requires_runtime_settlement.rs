use tidepool_runtime::session::{PersistentSession, SettledCellProgram};

fn begin(session: &PersistentSession, settled: SettledCellProgram) {
    let _ = session.begin_cell_program(settled);
}

fn main() {}
