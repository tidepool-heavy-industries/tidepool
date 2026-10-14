use tidepool_codegen::prepared_program::{ExecutionError, PreparedMachine};
use tidepool_codegen::suspension::RealmId;
use tidepool_repr::DataConId;
fn cross(machine: &mut PreparedMachine<'_>) -> Result<(), ExecutionError> {
    let mut saved = None;
    machine.managed_builder(|mut builder| {
        let node = builder.constructor(DataConId(0), [])?;
        saved = Some(builder.finish(RealmId::ROOT, node)?);
        Ok(())
    })
}
fn main() {}
