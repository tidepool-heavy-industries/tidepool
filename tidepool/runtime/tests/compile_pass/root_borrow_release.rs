use tidepool_codegen::prepared_program::{PreparedHandle, PreparedMachine};

fn release_after_borrow(machine: &mut PreparedMachine<'_>, handle: PreparedHandle) {
    let _address = machine.handle_root(handle).unwrap().addr() as usize;
    machine.release(handle);
    assert!(machine.handle_root(handle).is_none());
}

fn main() {}
