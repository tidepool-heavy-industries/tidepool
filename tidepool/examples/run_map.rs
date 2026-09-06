//! Read-only partial run-map consumer; not a launch/control command.
use std::path::PathBuf;
use tidepool::run_map::{read_windowed_run, Limits, TimeWindow};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args_os().skip(1);
    let path = PathBuf::from(args.next().ok_or("usage: run_map RUN_DIRECTORY")?);
    let mut window = TimeWindow::default();
    while let Some(option) = args.next() {
        let value = args
            .next()
            .ok_or("window flag requires UTC Unix milliseconds")?
            .into_string()
            .map_err(|_| "timestamp must be UTF-8")?
            .parse::<u64>()?;
        match option.to_str() {
            Some("--from-unix-ms") => window.from_unix_ms = Some(value),
            Some("--until-unix-ms") => window.until_unix_ms = Some(value),
            _ => return Err("expected --from-unix-ms or --until-unix-ms".into()),
        }
    }
    let report = read_windowed_run(&path, Limits::default(), window)?;
    eprintln!("{}", report.concise());
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}
