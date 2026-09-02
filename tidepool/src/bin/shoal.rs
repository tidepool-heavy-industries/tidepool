use std::path::PathBuf;

use clap::{Parser, Subcommand};
use tidepool_agent::ReasoningEffort;

#[derive(Debug, Parser)]
#[command(name = "shoal", about = "Run typed Tidepool actor ensembles")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Initialize an empty Git repository for Shoal orchestration.
    New {
        /// Directory to initialize. Defaults to the current directory.
        path: Option<PathBuf>,
    },
    /// Create a project-local actor run in its own tmux session.
    Init {
        /// Repository the ensemble will work in. Defaults to the current project.
        #[arg(long)]
        workspace: Option<PathBuf>,
        #[arg(long)]
        session: Option<String>,
        #[arg(long)]
        recreate: bool,
        #[arg(long)]
        no_attach: bool,
        #[arg(long)]
        model: Option<String>,
        #[arg(long, value_enum)]
        effort: Option<Effort>,
    },
    /// Run the resident actor host inside a Shoal tmux session.
    Host {
        #[arg(long)]
        workspace: PathBuf,
        #[arg(long)]
        session: String,
        #[arg(long)]
        run_id: String,
        #[arg(long)]
        run_root: PathBuf,
        #[arg(long)]
        status_path: PathBuf,
        #[arg(long)]
        root_binding_path: PathBuf,
        #[arg(long)]
        resume_root: bool,
        #[arg(long)]
        model: Option<String>,
        #[arg(long, value_enum)]
        effort: Option<Effort>,
    },
    /// Proxy one Codex stdio MCP child to its authenticated resident actor.
    Proxy,
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
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
    match Cli::parse().command {
        Command::New { path } => tidepool::shoal::new(tidepool::shoal::NewOptions { path }).await,
        Command::Init {
            workspace,
            session,
            recreate,
            no_attach,
            model,
            effort,
        } => {
            tidepool::shoal::init(tidepool::shoal::InitOptions {
                workspace,
                session,
                recreate,
                no_attach,
                model,
                effort: effort.map(Into::into),
            })
            .await
        }
        Command::Host {
            workspace,
            session,
            run_id,
            run_root,
            status_path,
            root_binding_path,
            resume_root,
            model,
            effort,
        } => {
            let _log_path = tidepool::shoal::init_host_tracing(&workspace, &run_id)?;
            tidepool::shoal::host(tidepool::shoal::HostOptions {
                workspace,
                session,
                run_id,
                run_root,
                status_path,
                root_binding_path,
                resume_root,
                model,
                effort: effort.map(Into::into),
            })
            .await
        }
        Command::Proxy => tidepool_agent::run_interactive_proxy().await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clap_exposes_the_user_commands_and_process_boundaries() {
        assert!(matches!(
            Cli::try_parse_from(["shoal", "new", "/tmp/project"])
                .unwrap()
                .command,
            Command::New { path: Some(path) }
                if path == std::path::Path::new("/tmp/project")
        ));
        assert!(matches!(
            Cli::try_parse_from([
                "shoal",
                "init",
                "--workspace",
                "/tmp/project",
                "--no-attach"
            ])
                .unwrap()
                .command,
            Command::Init {
                workspace: Some(workspace),
                no_attach: true,
                ..
            } if workspace == std::path::Path::new("/tmp/project")
        ));
        assert!(matches!(
            Cli::try_parse_from(["shoal", "proxy"]).unwrap().command,
            Command::Proxy
        ));
        let help = Cli::try_parse_from(["shoal", "--help"]).unwrap_err();
        let rendered = help.to_string();
        for command in ["new", "init", "host", "proxy"] {
            assert!(rendered.contains(command), "{rendered}");
        }
    }
}
