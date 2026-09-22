#![warn(clippy::unwrap_used, clippy::expect_used)]

#[derive(clap::Parser)]
#[command(
    name = "tidepool",
    about = "Tidepool utilities",
    arg_required_else_help = true
)]
struct Args {
    /// Record the deploy stamp — the content fingerprints of the extract
    /// binary and the stdlib tree this binary resolves — then exit. Run by
    /// `scripts/redeploy.sh` as its final step, so that every later server
    /// startup can detect an extract/stdlib pair that did NOT move together.
    /// Writer and checker share one implementation
    /// (`tidepool_toolchain::toolchain`), so they cannot drift.
    #[arg(long)]
    write_toolchain_stamp: bool,
}

/// Body of `--write-toolchain-stamp`. Prints the recorded fingerprints so the
/// deploy log carries which pair was blessed — the same detail a skew message
/// prints later, making a stamp/skew pair diffable by eye.
fn write_toolchain_stamp() -> Result<(), Box<dyn std::error::Error>> {
    use tidepool_toolchain::toolchain;

    let (endpoint, location) = toolchain::bind_extract_endpoint()?;
    let stdlib = tidepool::haskell_sources::ensure_stdlib()?;
    let stamp = toolchain::write_stamp(&endpoint, &location.path, &stdlib)?;
    println!(
        "toolchain stamp written to {}\n  extract {} ({})\n  stdlib  {} ({})",
        toolchain::stamp_path().display(),
        &stamp.extract[..12],
        stamp.extract_path,
        &stamp.stdlib[..12],
        stamp.stdlib_path,
    );
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    use clap::Parser;

    let args = Args::parse();
    if args.write_toolchain_stamp {
        write_toolchain_stamp()?;
    }
    Ok(())
}
