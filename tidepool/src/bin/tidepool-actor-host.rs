use std::path::PathBuf;

use clap::Parser;
use tidepool_agent::ReasoningEffort;

#[derive(Parser)]
#[command(
    name = "tidepool-actor-host",
    about = "Run an actor-native Tidepool swarm"
)]
struct Args {
    /// Workspace visible to every interactive actor in this first vertical.
    #[arg(long, default_value = ".")]
    workspace: PathBuf,

    /// Include root containing Tidepool/Actors/DevSwarm.hs.
    #[arg(long, default_value = "haskell/actors")]
    policy_root: PathBuf,

    /// Pane-owning node executable installed beside this daemon or on PATH.
    #[arg(long, default_value = "tidepool-agent-node")]
    node_program: String,

    /// Tmux session that owns the interactive actor panes.
    #[arg(long, default_value = "tidepool-actors")]
    tmux_session: String,

    #[arg(long)]
    model: Option<String>,

    #[arg(long, value_enum)]
    effort: Option<Effort>,
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum Effort {
    Low,
    Medium,
    High,
}

impl From<Effort> for ReasoningEffort {
    fn from(value: Effort) -> Self {
        match value {
            Effort::Low => Self::Low,
            Effort::Medium => Self::Medium,
            Effort::High => Self::High,
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    tidepool::actor_host::run(tidepool::actor_host::ActorHostConfig {
        workspace: std::fs::canonicalize(args.workspace)?,
        policy_root: std::fs::canonicalize(args.policy_root)?,
        node_program: args.node_program,
        tmux_session: args.tmux_session,
        model: args.model,
        effort: args.effort.map(Into::into),
    })
    .await
}
