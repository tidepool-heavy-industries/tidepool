#![warn(clippy::unwrap_used, clippy::expect_used)]

mod listen_client;

#[derive(clap::Subcommand)]
enum Command {
    /// Connect to a resident harness's listen channel and print each frame
    /// it publishes to stdout, acknowledging each frame after flushing it.
    Listen(listen_client::ListenArgs),
}

#[derive(clap::Parser)]
#[command(name = "tidepool", about = "Tidepool utilities", arg_required_else_help = true)]
struct Args {
    #[command(subcommand)]
    command: Option<Command>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    use clap::Parser;

    match Args::parse().command {
        Some(Command::Listen(args)) => listen_client::run(&args).await,
        None => unreachable!("clap requires a subcommand"),
    }
}
