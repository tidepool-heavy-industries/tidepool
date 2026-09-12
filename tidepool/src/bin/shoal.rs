use std::path::PathBuf;

use clap::{Parser, Subcommand};
use tidepool::shoal::{ShoalAgentDefaults, ShoalEffort};

#[derive(Debug, Parser)]
#[command(name = "shoal", about = "Run typed Tidepool actor ensembles")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Complete a mount transition after its pre-exec syscall phase.
    #[command(name = "__tidepool_mount_helper", hide = true)]
    MountHelper,
    /// Verify containment before executing an internal launch payload.
    #[command(hide = true)]
    InSlice {
        #[arg(long)]
        slice: String,
        #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<String>,
    },
    /// Own shared command admission and cgroup custody across Shoal runs.
    #[command(hide = true)]
    CommandResources {
        #[arg(long)]
        socket: PathBuf,
        #[arg(long)]
        policy: PathBuf,
    },
    /// Inspect unused build storage for a stopped run; preserve all source/Git state.
    Cleanup {
        #[arg(long)]
        run_root: PathBuf,
        /// Remove only storage whose mounts are confirmed unused.
        #[arg(long)]
        apply: bool,
        /// Include source layers only when their worktree is finalized.
        #[arg(long)]
        source: bool,
    },
    /// Run the private process-scope supervisor for one prepared launch.
    #[command(hide = true)]
    ProcessSupervisor {
        #[arg(long)]
        manifest: PathBuf,
    },
    /// Enter a host-retained workspace view before replacing this process.
    #[command(hide = true)]
    EnterView {
        #[arg(long)]
        view: String,
        #[arg(long)]
        cwd: PathBuf,
        #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<String>,
    },
    /// Inspect recorded run artifacts without launching or attaching to a host.
    RunMap {
        run_dir: PathBuf,
        /// Inclusive UTC Unix-millisecond lower bound; untimed events remain unknown.
        #[arg(long)]
        from_unix_ms: Option<u64>,
        /// Exclusive UTC Unix-millisecond upper bound.
        #[arg(long)]
        until_unix_ms: Option<u64>,
        #[arg(long)]
        json: bool,
    },
    /// Provision, inspect, or retire operator workbenches in a running host.
    Operator {
        /// The running host's protected operator socket.
        #[arg(long)]
        socket: PathBuf,
        #[command(subcommand)]
        action: OperatorCommand,
    },
    /// Initialize an empty Git repository for Shoal orchestration.
    New {
        /// Directory to initialize. Defaults to the current directory.
        path: Option<PathBuf>,
    },
    /// Check workspace customization without launching native workers or providers.
    Check {
        #[arg(long)]
        workspace: Option<PathBuf>,
        /// Execute the candidate workspace's model-free Haskell recipe checks.
        #[arg(long)]
        recipes: bool,
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
        /// Override `.shoal/config.toml` for this run.
        #[arg(long)]
        model: Option<String>,
        /// Override `.shoal/config.toml` for this run.
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
        #[arg(long, hide = true)]
        interactive_agent_bin: PathBuf,
        #[arg(long, hide = true)]
        interactive_agent_version: String,
        #[arg(long)]
        resume_root: bool,
        #[arg(long, hide = true)]
        model: String,
        #[arg(long, value_enum, hide = true)]
        effort: Effort,
    },
}

#[derive(Debug, Subcommand)]
enum OperatorCommand {
    New,
    List,
    Stop { session: String },
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
enum Effort {
    Low,
    Medium,
    High,
}

impl From<Effort> for ShoalEffort {
    fn from(value: Effort) -> Self {
        match value {
            Effort::Low => Self::Low,
            Effort::Medium => Self::Medium,
            Effort::High => Self::High,
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let command = Cli::parse().command;
    if let Command::MountHelper = command {
        return Ok(());
    }
    if let Command::InSlice { slice, command } = command {
        use std::os::unix::process::CommandExt;
        let slice = tidepool_node::systemd_slice::SystemdSlice::try_from(slice)?;
        slice.current_membership()?;
        return Err(std::process::Command::new(&command[0])
            .args(&command[1..])
            .exec()
            .into());
    }
    if let Command::ProcessSupervisor { manifest } = command {
        // Like namespace entry, the scope supervisor must run before Tokio,
        // compiler discovery, provider construction, or host initialization.
        return tidepool::shoal::process_supervisor(manifest);
    }
    if let Command::EnterView { view, cwd, command } = command {
        use std::os::unix::process::CommandExt;
        let entry: tidepool_node::NamespaceEntry = serde_json::from_str(&view)?;
        let mut process = entry.command(&cwd, command[0].as_ref())?;
        process.args(&command[1..]);
        // Namespace entry must precede construction of the multithreaded runtime.
        return Err(process.exec().into());
    }
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(run(command))
}

async fn run(command: Command) -> Result<(), Box<dyn std::error::Error>> {
    match command {
        Command::MountHelper => unreachable!("handled before runtime construction"),
        Command::InSlice { .. } => unreachable!("handled before runtime construction"),
        Command::CommandResources { socket, policy } => {
            tidepool::shoal::resources::serve(socket, policy).await
        }
        Command::Cleanup {
            run_root,
            apply,
            source,
        } => {
            println!(
                "{}",
                serde_json::to_string_pretty(&tidepool::actor_host::workspace_cleanup::cleanup(
                    &run_root, apply, source
                )?)?
            );
            Ok(())
        }
        Command::ProcessSupervisor { .. } => {
            unreachable!("handled before runtime construction")
        }
        Command::EnterView { .. } => unreachable!("handled before runtime construction"),
        Command::RunMap {
            run_dir,
            from_unix_ms,
            until_unix_ms,
            json,
        } => {
            let report = tidepool::run_map::read_windowed_run(
                &run_dir,
                tidepool::run_map::Limits::default(),
                tidepool::run_map::TimeWindow {
                    from_unix_ms,
                    until_unix_ms,
                },
            )?;
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                println!("{}", report.concise());
            }
            Ok(())
        }
        Command::Operator { socket, action } => {
            let action = match action {
                OperatorCommand::New => tidepool::operator::OperatorAction::New,
                OperatorCommand::List => tidepool::operator::OperatorAction::List,
                OperatorCommand::Stop { session } => {
                    tidepool::operator::OperatorAction::Stop { session }
                }
            };
            tidepool::operator::command(&socket, action).await
        }
        Command::New { path } => tidepool::shoal::new(tidepool::shoal::NewOptions { path }).await,
        Command::Check { workspace, recipes } => tidepool::shoal::check(workspace, recipes).await,
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
            interactive_agent_bin,
            interactive_agent_version,
            resume_root,
            model,
            effort,
        } => {
            let _log_path = tidepool::shoal::init_host_tracing(&workspace, &run_id)?;
            let interactive_agent = tidepool_agent::native_interactive_agent_from_parts(
                interactive_agent_bin,
                interactive_agent_version,
            )?;
            tidepool::shoal::host(tidepool::shoal::HostOptions {
                workspace,
                session,
                run_id,
                run_root,
                status_path,
                root_binding_path,
                interactive_agent,
                resume_root,
                agent: ShoalAgentDefaults {
                    model,
                    effort: effort.into(),
                },
            })
            .await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_map_cli_preserves_explicit_directory_and_window() {
        assert!(matches!(
            Cli::try_parse_from([
                "shoal", "run-map", "/sanitized/run", "--from-unix-ms", "10",
                "--until-unix-ms", "20", "--json"
            ]).unwrap().command,
            Command::RunMap { run_dir, from_unix_ms: Some(10), until_unix_ms: Some(20), json: true }
                if run_dir == std::path::Path::new("/sanitized/run")
        ));
        assert!(Cli::try_parse_from(["shoal", "run-map"]).is_err());
        assert!(Cli::try_parse_from([
            "shoal",
            "run-map",
            "/sanitized/run",
            "--from-unix-ms",
            "invalid"
        ])
        .is_err());
    }

    #[test]
    fn clap_exposes_the_user_commands_and_process_boundaries() {
        assert!(matches!(
            Cli::try_parse_from([
                "shoal",
                "process-supervisor",
                "--manifest",
                "/private/launch.json"
            ])
            .unwrap()
            .command,
            Command::ProcessSupervisor { manifest }
                if manifest == std::path::Path::new("/private/launch.json")
        ));
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
        let help = Cli::try_parse_from(["shoal", "--help"]).unwrap_err();
        let rendered = help.to_string();
        for command in ["new", "init", "host", "run-map"] {
            assert!(rendered.contains(command), "{rendered}");
        }
        assert!(!rendered.contains("proxy"), "{rendered}");
    }
}
