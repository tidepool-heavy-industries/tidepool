use std::path::PathBuf;

use clap::{Parser, Subcommand};
use tidepool::exomonad::{ExomonadAgentDefaults, ExomonadEffort};

#[derive(Debug, Parser)]
#[command(name = "exomonad", about = "Run typed Tidepool actor ensembles")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Connect an Exomonad credential record to a ChatGPT plan.
    Auth {
        #[command(subcommand)]
        action: AuthCommand,
    },
    /// Complete a mount transition after its pre-exec syscall phase.
    #[command(name = "__exomonad_mount_helper", hide = true)]
    MountHelper,
    /// Verify containment before executing an internal launch payload.
    #[command(hide = true)]
    InSlice {
        #[arg(long)]
        slice: String,
        #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<String>,
    },
    /// Own shared command admission and cgroup custody across Exomonad runs.
    #[command(hide = true, name = "command-resources")]
    Resources {
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
        /// Lower bound relative to now, e.g. `90s`, `15m`, `2h`.
        #[arg(long, conflicts_with = "from_unix_ms", value_parser = tidepool::run_map::parse_duration_ms)]
        since: Option<u64>,
        /// Number of slowest hosted calls to list.
        #[arg(long, default_value_t = 15)]
        slowest: usize,
        /// Filter timeline events to actor key `ID@INCARNATION`.
        #[arg(long)]
        actor: Option<String>,
        /// Filter timeline events to a recorded execution identifier.
        #[arg(long)]
        execution: Option<String>,
        /// Filter timeline events to a recorded hosted call identifier.
        #[arg(long)]
        call_id: Option<String>,
        /// Emit a bounded Perfetto-compatible JSON trace.
        #[arg(long)]
        perfetto: bool,
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
    /// Submit a Haskell cell into a running Exomonad session as a resident operator (proxy) actor.
    Proxy {
        session: String,
        #[arg(required_unless_present = "actors")]
        file: Option<PathBuf>,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        fresh: bool,
        #[arg(long)]
        actors: bool,
    },
    /// Scaffold an Exomonad workspace package: configuration, the Jev pin, a
    /// starter agent spec, and the workspace skills.
    New {
        /// Empty directory, or the root of an existing Git repository.
        /// Defaults to the current directory.
        path: Option<PathBuf>,
    },
    /// Check workspace customization without launching native workers or providers.
    Check {
        #[arg(long)]
        workspace: Option<PathBuf>,
        /// Execute the candidate workspace's model-free Haskell recipe checks.
        #[arg(long)]
        recipes: bool,
        /// Run only this configured recipe entry (for example, Project.Checks.workbench).
        /// Supplying it also enables recipe execution.
        #[arg(long, value_name = "MODULE.FUNCTION")]
        recipe: Option<String>,
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
        /// Override `.exomonad/config.toml` for this run.
        #[arg(long)]
        model: Option<String>,
        /// Override `.exomonad/config.toml` for this run.
        #[arg(long, value_enum)]
        effort: Option<Effort>,
        /// Skip the launch preflight (workspace pin and compile daemons).
        #[arg(long, conflicts_with = "strict_preflight")]
        no_preflight: bool,
        /// Treat launch preflight warnings as failures.
        #[arg(long)]
        strict_preflight: bool,
    },
    /// Intentionally stop a supervised run, then close its tmux diagnostics session.
    Stop {
        #[arg(long)]
        run_id: String,
        #[arg(long)]
        session: String,
    },
    /// Run the resident actor host inside an Exomonad tmux session.
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
        #[arg(long, hide = true)]
        model: String,
        #[arg(long, value_enum, hide = true)]
        effort: Effort,
    },
}

#[derive(Debug, Subcommand)]
enum AuthCommand {
    /// Continue with ChatGPT using a protected Exomonad credential record.
    Login {
        /// Select one registration; defaults to the private Exomonad account file.
        #[arg(long)]
        credential_file: Option<PathBuf>,
        /// The browser returns to 127.0.0.1 on this port.
        #[arg(long, default_value_t = 1455)]
        port: u16,
    },
}

#[derive(Debug, Subcommand)]
enum OperatorCommand {
    New,
    List,
    Stop {
        session: String,
    },
    /// Inspect retained build backing paths for one exact actor.
    Artifact {
        actor: u64,
        incarnation: u64,
        /// Path relative to the actor's .exomonad/build/cargo mount.
        path: PathBuf,
    },
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
enum Effort {
    Low,
    Medium,
    High,
}

impl From<Effort> for ExomonadEffort {
    fn from(value: Effort) -> Self {
        match value {
            Effort::Low => Self::Low,
            Effort::Medium => Self::Medium,
            Effort::High => Self::High,
        }
    }
}

/// The default `Result`-returning `main` prints an unhandled `Err` via
/// `Debug`, not `Display` — so every `runtime_error`/`ConfigError`/`BinError`
/// message this binary hand-crafts for an operator (including
/// `ConfigError::NoWorkspace`'s and `BinError`'s Display impls) was silently
/// discarded in favor of the derived struct/enum dump (`NoWorkspace {
/// workspace: "...", config: "..." }`, `Os { code: 2, kind: NotFound, ... }`)
/// that operators actually saw. Print the Display text ourselves instead.
fn main() {
    if let Err(error) = try_main() {
        eprintln!("Error: {error}");
        std::process::exit(1);
    }
}

fn try_main() -> Result<(), Box<dyn std::error::Error>> {
    let command = Cli::parse().command;
    if let Command::MountHelper = command {
        return Ok(());
    }
    if let Command::InSlice { slice, command } = command {
        use std::os::unix::process::CommandExt;
        let slice = exomonad_node::systemd_slice::SystemdSlice::try_from(slice)?;
        slice.current_membership()?;
        #[allow(
            clippy::disallowed_methods,
            reason = "exec() replaces this process image; there is no child to route through the launcher"
        )]
        return Err(std::process::Command::new(&command[0])
            .args(&command[1..])
            .exec()
            .into());
    }
    if let Command::ProcessSupervisor { manifest } = command {
        // Like namespace entry, the scope supervisor must run before Tokio,
        // compiler discovery, provider construction, or host initialization.
        return tidepool::exomonad::process_supervisor(manifest);
    }
    if let Command::EnterView { view, cwd, command } = command {
        use std::os::unix::process::CommandExt;
        let entry: exomonad_node::NamespaceEntry = serde_json::from_str(&view)?;
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
        Command::Auth {
            action:
                AuthCommand::Login {
                    credential_file,
                    port,
                },
        } => {
            let path = match credential_file {
                Some(path) => path,
                None => harness::transport::auth::ChatGptPlanAuth::default_path()?,
            };
            harness::transport::auth::chatgpt::login(path.clone(), port, |url| {
                println!("Continue with ChatGPT in your browser:\n{url}");
                println!("The callback uses 127.0.0.1 on the host running this command.");
            })
            .await?;
            println!(
                "Saved the protected Exomonad registration at {}",
                path.display()
            );
            Ok(())
        }
        Command::MountHelper => unreachable!("handled before runtime construction"),
        Command::InSlice { .. } => unreachable!("handled before runtime construction"),
        Command::Resources { socket, policy } => {
            tidepool::exomonad::resources::serve(socket, policy).await
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
            since,
            slowest,
            actor,
            execution,
            call_id,
            perfetto,
            json,
        } => {
            let now_unix_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_millis()
                .try_into()?;

            let codex_home = None;
            let observation = tidepool::run_map::Observation {
                now_unix_ms,
                codex_home,
                slowest_calls: slowest,
            };
            let report = tidepool::run_map::read_observed_run_filtered(
                &run_dir,
                tidepool::run_map::Limits::default(),
                tidepool::run_map::TimeWindow {
                    from_unix_ms: since
                        .map(|since| now_unix_ms.saturating_sub(since))
                        .or(from_unix_ms),
                    until_unix_ms,
                },
                &observation,
                &tidepool::run_map::TimelineFilter {
                    actor,
                    execution,
                    call_id,
                },
            )?;
            if perfetto {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&report.perfetto_trace())?
                );
            } else if json {
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
                OperatorCommand::Artifact {
                    actor,
                    incarnation,
                    path,
                } => tidepool::operator::OperatorAction::Artifact {
                    actor: exomonad_actor::ActorRef {
                        id: exomonad_actor::ActorId(actor),
                        incarnation: exomonad_actor::Incarnation(incarnation),
                    },
                    relative_path: path,
                },
            };
            tidepool::operator::command(&socket, action).await
        }
        Command::Proxy {
            session,
            file,
            json,
            fresh,
            actors,
        } => {
            tidepool::operator::proxy::proxy(tidepool::operator::proxy::ProxyOptions {
                session,
                file,
                json,
                fresh,
                actors,
                runs_dir: None,
            })
            .await
        }
        Command::New { path } => tidepool::exomonad::new(tidepool::exomonad::NewOptions {
            path,
            ..Default::default()
        }),
        Command::Check {
            workspace,
            recipes,
            recipe,
        } => {
            if std::env::var("RUST_LOG").is_ok_and(|filter| !filter.is_empty()) {
                tracing_subscriber::fmt()
                    .with_ansi(false)
                    .with_writer(std::io::stderr)
                    .with_env_filter(tidepool_codegen::debug::tracing_env_filter("off"))
                    .try_init()
                    .map_err(|error| {
                        std::io::Error::other(format!(
                            "could not initialize Exomonad check tracing: {error}"
                        ))
                    })?;
            }
            tidepool::exomonad::check_recipe(workspace, recipes, recipe).await
        }
        Command::Init {
            workspace,
            session,
            recreate,
            no_attach,
            model,
            effort,
            no_preflight,
            strict_preflight,
        } => {
            use tidepool::exomonad::PreflightMode;
            tidepool::exomonad::init(tidepool::exomonad::InitOptions {
                workspace,
                session,
                recreate,
                no_attach,
                model,
                effort: effort.map(Into::into),
                preflight: if no_preflight {
                    PreflightMode::Skip
                } else if strict_preflight {
                    PreflightMode::Strict
                } else {
                    PreflightMode::Warn
                },
            })
            .await
        }
        Command::Stop { run_id, session } => tidepool::exomonad::stop(&run_id, &session).await,
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
            // `_trace_guard` must stay a named binding: dropping it closes the
            // trace appender's flush channel and the JSONL file stops growing.
            let (_log_path, _trace_guard) =
                tidepool::exomonad::init_host_tracing(&workspace, &run_id)?;

            tidepool::exomonad::host(tidepool::exomonad::HostOptions {
                workspace,
                session,
                run_id,
                run_root,
                status_path,
                root_binding_path,
                resume_root,
                agent: ExomonadAgentDefaults {
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
    #[test]
    fn native_chatgpt_login_cli_selects_private_record_and_loopback_port() {
        let cli = super::Cli::try_parse_from([
            "exomonad",
            "auth",
            "login",
            "--credential-file",
            "/private/exomonad.json",
            "--port",
            "54321",
        ])
        .unwrap();
        assert!(
            matches!(cli.command, super::Command::Auth { action: super::AuthCommand::Login { credential_file: Some(path), port: 54321 } } if path == std::path::PathBuf::from("/private/exomonad.json"))
        );
        let cli = super::Cli::try_parse_from(["exomonad", "auth", "login"]).unwrap();
        assert!(matches!(
            cli.command,
            super::Command::Auth {
                action: super::AuthCommand::Login {
                    credential_file: None,
                    port: 1455
                }
            }
        ));
        assert!(
            super::Cli::try_parse_from(["exomonad", "auth", "login", "--port", "65536"]).is_err()
        );
    }

    use super::*;

    #[test]
    fn operator_artifact_query_selects_exact_actor_and_build_relative_path() {
        assert!(matches!(
            Cli::try_parse_from([
                "exomonad", "operator", "--socket", "/private/operator.sock",
                "artifact", "7", "2", "debug/deps/evidence.json"
            ]).unwrap().command,
            Command::Operator {
                socket,
                action: OperatorCommand::Artifact { actor: 7, incarnation: 2, path }
            } if socket == std::path::Path::new("/private/operator.sock")
                && path == std::path::Path::new("debug/deps/evidence.json")
        ));
    }

    #[test]
    fn run_map_cli_preserves_explicit_directory_and_window() {
        assert!(matches!(
            Cli::try_parse_from([
                "exomonad", "run-map", "/sanitized/run", "--from-unix-ms", "10",
                "--until-unix-ms", "20", "--json"
            ]).unwrap().command,
            Command::RunMap { run_dir, from_unix_ms: Some(10), until_unix_ms: Some(20), since: None, slowest: 15, actor: None, execution: None, call_id: None, perfetto: false, json: true }
                if run_dir == std::path::Path::new("/sanitized/run")
        ));
        assert!(Cli::try_parse_from(["exomonad", "run-map"]).is_err());
        assert!(matches!(
            Cli::try_parse_from([
                "exomonad",
                "run-map",
                "/run",
                "--since",
                "15m",
                "--slowest",
                "3"
            ])
            .unwrap()
            .command,
            Command::RunMap {
                since: Some(900_000),
                slowest: 3,
                ..
            }
        ));
        assert!(Cli::try_parse_from([
            "exomonad",
            "run-map",
            "/run",
            "--since",
            "15m",
            "--from-unix-ms",
            "1"
        ])
        .is_err());
        assert!(Cli::try_parse_from([
            "exomonad",
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
            Cli::try_parse_from(["exomonad", exomonad_node::MOUNT_HELPER_COMMAND])
                .unwrap()
                .command,
            Command::MountHelper
        ));
        assert!(matches!(
            Cli::try_parse_from([
                "exomonad",
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
            Cli::try_parse_from(["exomonad", "new", "/tmp/project"])
                .unwrap()
                .command,
            Command::New { path: Some(path) }
                if path == std::path::Path::new("/tmp/project")
        ));
        assert!(matches!(
            Cli::try_parse_from([
                "exomonad",
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
                no_preflight: false,
                strict_preflight: false,
                ..
            } if workspace == std::path::Path::new("/tmp/project")
        ));
        assert!(matches!(
            Cli::try_parse_from(["exomonad", "init", "--strict-preflight"])
                .unwrap()
                .command,
            Command::Init {
                strict_preflight: true,
                no_preflight: false,
                ..
            }
        ));
        assert!(
            Cli::try_parse_from(["exomonad", "init", "--no-preflight", "--strict-preflight"])
                .is_err()
        );
        let help = Cli::try_parse_from(["exomonad", "--help"]).unwrap_err();
        let rendered = help.to_string();
        for command in ["new", "init", "host", "run-map", "proxy"] {
            assert!(rendered.contains(command), "{rendered}");
        }
    }

    #[test]
    fn proxy_cli_parses_session_file_and_json() {
        assert!(matches!(
            Cli::try_parse_from(["exomonad", "proxy", "run7", "cell.hs", "--json"])
                .unwrap()
                .command,
            Command::Proxy { session, file: Some(file), json: true, fresh: false, actors: false }
                if session == "run7" && file == std::path::Path::new("cell.hs")
        ));
    }

    #[test]
    fn check_cli_selects_one_configured_recipe() {
        assert!(matches!(
            Cli::try_parse_from([
                "exomonad",
                "check",
                "--workspace",
                "/tmp/project",
                "--recipe",
                "Project.JevChecks.reflex"
            ])
            .unwrap()
            .command,
            Command::Check {
                workspace: Some(workspace),
                recipes: false,
                recipe: Some(recipe),
            } if workspace == std::path::Path::new("/tmp/project")
                && recipe == "Project.JevChecks.reflex"
        ));
    }
}
