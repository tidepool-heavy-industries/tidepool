#![warn(clippy::unwrap_used, clippy::expect_used)]

mod listen_client;

#[derive(clap::Subcommand)]
enum Command {
    /// Connect to a resident harness's listen channel and print each frame
    /// it publishes to stdout, acknowledging each frame after flushing it.
    Listen(listen_client::ListenArgs),
}

#[derive(clap::Parser)]
#[command(
    name = "tidepool",
    about = "Tidepool utilities",
    arg_required_else_help = true
)]
struct Args {
    #[command(subcommand)]
    command: Option<Command>,

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
    use tidepool_runtime::toolchain;

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

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    use clap::Parser;

    let args = Args::parse();

    // `--write-toolchain-stamp`: record the pair and exit, before any
    // subcommand dispatch.
    if args.write_toolchain_stamp {
        return write_toolchain_stamp();
    }

    match args.command {
        Some(Command::Listen(args)) => listen_client::run(&args).await,
        None => unreachable!("clap requires a subcommand"),
    }
}
