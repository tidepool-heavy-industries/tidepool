use tidepool_codegen::prepared_program::{PreparedHandle, PreparedMachine};

fn collect_then_reborrow(machine: &mut PreparedMachine<'_>, handle: PreparedHandle) {
    let address = machine.handle_root(handle).unwrap().addr() as usize;
    machine.collect_major(machine.quiesce().unwrap()).unwrap();
    let root = machine.handle_root(handle).unwrap();
    assert_eq!(root.addr() as usize, address);
    let _ = root.current();
}

fn main() {}
