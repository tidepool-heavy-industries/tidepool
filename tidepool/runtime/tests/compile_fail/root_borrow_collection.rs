use tidepool_codegen::prepared_program::{PreparedHandle, PreparedMachine};

fn collect_with_borrow(machine: &mut PreparedMachine<'_>, handle: PreparedHandle) {
    let root = machine.handle_root(handle).unwrap();
    machine.collect_major(machine.quiesce().unwrap()).unwrap();
    let _ = root.current();
}

fn main() {}
