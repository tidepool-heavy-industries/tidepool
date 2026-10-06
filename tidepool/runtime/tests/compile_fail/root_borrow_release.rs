use tidepool_codegen::prepared_program::{PreparedHandle, PreparedMachine};

fn release_with_borrow(machine: &mut PreparedMachine<'_>, handle: PreparedHandle) {
    let root = machine.handle_root(handle).unwrap();
    machine.release(handle);
    let _ = root.current();
}

fn main() {}
