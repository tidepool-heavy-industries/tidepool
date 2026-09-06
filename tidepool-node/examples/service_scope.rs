//! Explicit canary consumer; never used by existing tmux launches.
use std::time::{Duration, Instant};
use tidepool_node::{ProcessInvocation, ProcessMountBoundary};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args_os().skip(1);
    let bubblewrap = args.next().ok_or("expected absolute bwrap path")?;
    let workspace = std::fs::canonicalize(args.next().ok_or("expected canary workspace")?)?;
    let boundary = ProcessMountBoundary::new(
        &workspace,
        [&workspace].map(Clone::clone),
        [&workspace].map(Clone::clone),
    )?;
    let prepared = boundary.prepare_service_scope(
        bubblewrap.into(),
        ProcessInvocation {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), "exit 0".into()],
        },
    )?;
    let log = std::fs::File::create(workspace.join("service-scope.log"))?;
    let mut scope = prepared.spawn(Default::default(), log)?;
    if let Err(error) = scope.pin_init(Instant::now() + Duration::from_secs(10)) {
        // No validated init means even monitor exit cannot certify cleanup.
        let cleanup = scope.terminate_and_wait(Instant::now() + Duration::from_secs(10));
        return Err(format!(
            "pin failed: {error}; cleanup: {cleanup:?}; no custody release authorized"
        )
        .into());
    }
    if let Err(error) = scope.release_command() {
        let cleanup = scope.terminate_and_wait(Instant::now() + Duration::from_secs(10));
        return Err(format!("release failed: {error}; cleanup: {cleanup:?}").into());
    }
    let receipt = scope.terminate_and_wait(Instant::now() + Duration::from_secs(10))?;
    println!(
        "confirmed namespace drain; monitor status: {}",
        receipt.monitor_status()
    );
    Ok(())
}
