use tidepool_codegen::prepared_program::{ExecutionError, PreparedMachine};
use tidepool_repr::DataConId;
fn cross(machine: &mut PreparedMachine<'_>) -> Result<(), ExecutionError> {
    let mut saved = None;
    machine.managed_builder(|mut builder| {
        saved = Some(builder.constructor(DataConId(0), [])?);
        Ok(())
    })
}
fn main() {}
