//! Read-only partial run-map consumer; not a launch/control command.
use std::path::PathBuf;
use tidepool::run_map::{read_run, Limits};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args_os().skip(1);
    let path = PathBuf::from(args.next().ok_or("usage: run_map RUN_DIRECTORY")?);
    if args.next().is_some() {
        return Err("usage: run_map RUN_DIRECTORY".into());
    }
    let report = read_run(&path, Limits::default())?;
    eprintln!("{}", report.concise());
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}
