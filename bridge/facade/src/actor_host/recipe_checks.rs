//! An ordinary Haskell check program drives the same resident host as native actors.
//! Only this entry point installs RecipeCheck; it never starts native providers.

use super::model_free::ModelFreeSession;
use super::*;
use crate::exomonad::workspace::FrozenWorkspace;
use crate::generated::recipe_check::RecipeCheckReq;
use exomonad_tool::{
    ConversationOrigin, OriginalOperation, ToolArguments, ToolInvocation, ToolInvocationContext,
};
use std::collections::HashSet;
use std::collections::VecDeque;
use std::future::Future;
use tidepool_bridge::FromHaskell;
use tidepool_bridge::HaskellValue;
use tidepool_effect::dispatch::{DispatchEffect, EffectContext, EffectDispatch, Response};
use tidepool_effect::error::EffectError;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
type CheckActor = (String, i64, i64);

pub(crate) async fn run(
    workspace: &Path,
    selected: &FrozenWorkspace,
    recipe: Option<&str>,
) -> Result<()> {
    if selected.checks.is_empty() {
        return Err(runtime_error(
            "no Haskell recipe checks configured in haskell.checks",
        ));
    }
    let entries = select_checks(&selected.checks, recipe)?;
    for entry in entries {
        println!("Recipe {entry}; definitions {}", selected.identity());
        let prepared = tokio::task::block_in_place(|| prepare_recipe(entry, selected))?;
        #[cfg(test)]
        let diagnostics =
            super::hosted_test_context::HostedTestDiagnostics::root_from_environment()
                .map_err(runtime_error)?;
        let mut driver = Driver::start(workspace, selected.clone()).await?;
        let result = driver.execute(&prepared);
        let cleanup = driver.shutdown_session().await;
        #[cfg(test)]
        if let Some(root) = diagnostics {
            use super::hosted_test_context::{
                CleanupOutcome, HostedTestDiagnostics, ScenarioOutcome, ScenarioPhase,
            };
            let scenario = match &result {
                Ok(()) => ScenarioOutcome::Passed,
                Err(error) => ScenarioOutcome::Failed {
                    phase: ScenarioPhase::Scenario,
                    message: error.to_string(),
                },
            };
            let outcome = match &cleanup {
                Ok(_) => CleanupOutcome::Confirmed,
                Err(error) => CleanupOutcome::Failed {
                    message: error.to_string(),
                },
            };
            HostedTestDiagnostics::report_model_free(
                &root,
                &scenario,
                &outcome,
                Some(format!("{cleanup:?}")),
            )
            .map_err(runtime_error)?;
        }
        let result = result.map_err(|mut error| {
            let mut retained = driver
                .installations
                .values()
                .map(|installation| retained_actor_evidence(&installation.actor))
                .collect::<Vec<_>>();
            retained.sort();
            error.detail.push_str(&format!(
                "\nRetained actor lifecycle after recipe shutdown (includes shutdown retirement):\n{}",
                retained.join("\n")
            ));
            Box::new(error) as Box<dyn std::error::Error>
        });
        driver.installations.clear();
        driver.pending.clear();
        match (result, cleanup) {
            (Err(error), Err(cleanup)) => {
                return Err(runtime_error(format!(
                    "{error}; resident cleanup also failed: {cleanup}"
                )));
            }
            (Err(error), _) | (_, Err(error)) => return Err(error),
            (Ok(()), Ok(_)) => {}
        }
        println!(
            "Recipe {entry}: {} assertions passed",
            driver.assertions.len()
        );
    }
    Ok(())
}

/// Render a recipe's compile failure with the same per-diagnostic text a
/// rejected interactive cell shows the model (`session::render_turn_compile_error`),
/// not just the `CompileError` `Display`'s one-line diagnostic count — the
/// summary line is kept in front of it. Every other `RuntimeError` variant
/// (a Jit/Prepared failure once compilation succeeded) keeps its own
/// `Display`; diagnostics only exist on the compile path.
fn render_recipe_compile_error(
    error: tidepool_runtime::RuntimeError,
    source: &str,
    entry: &str,
) -> RecipeCompilationFailure {
    let detail = match &error {
        tidepool_runtime::RuntimeError::Compile(compile_error) => {
            let detail = tidepool_runtime::session::render_turn_compile_error(
                compile_error,
                Some(source),
                entry,
                entry,
            );
            format!("recipe {entry} compilation failed: {error}\n{detail}")
        }
        other => other.to_string(),
    };
    RecipeCompilationFailure { error, detail }
}

#[derive(Debug)]
struct RecipeCompilationFailure {
    error: tidepool_runtime::RuntimeError,
    detail: String,
}
impl std::fmt::Display for RecipeCompilationFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.detail)
    }
}
impl std::error::Error for RecipeCompilationFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
}

#[derive(Debug)]
struct RecipeTurnFailure {
    error: exomonad_actor::ResidentToolError,
    detail: String,
}

impl RecipeTurnFailure {
    fn new(
        boundary: &str,
        source: &str,
        error: exomonad_actor::ResidentToolError,
        retained: String,
    ) -> Self {
        let detail = format!("resident recipe {boundary} failed:\n{source}\n{error}\n{retained}");
        Self { error, detail }
    }
}

impl std::fmt::Display for RecipeTurnFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.detail)
    }
}

impl std::error::Error for RecipeTurnFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
}

fn select_checks<'a>(checks: &'a [String], recipe: Option<&str>) -> Result<Vec<&'a String>> {
    match recipe {
        None => Ok(checks.iter().collect()),
        Some(requested) => match checks.iter().find(|entry| entry.as_str() == requested) {
            Some(entry) => Ok(vec![entry]),
            None => Err(runtime_error(format!(
                "recipe {requested:?} is not configured; choose one of: {}",
                checks.join(", ")
            ))),
        },
    }
}

/// Issued only after recipe compilation and confirmed transaction close.
/// Its owned artifacts can be executed without retaining compiler authority.
struct PreparedRecipe {
    artifacts: tidepool_runtime::CompiledArtifacts,
    source: String,
    entry: String,
}

fn prepare_recipe(entry: &str, selected: &FrozenWorkspace) -> Result<PreparedRecipe> {
    let (module, _) = entry
        .rsplit_once('.')
        .ok_or("invalid configured check entry")?;
    let mut declarations = exomonad_effect_declarations();
    declarations.push(tidepool_mcp::recipe_check_decl());
    let effects = tidepool_mcp::ensure_effects_module(&declarations)?;
    let mut includes = if let Some(mut roots) = selected.runtime_catalog_roots() {
        if roots.first() != Some(&effects.core) {
            return Err("frozen stable effect source selection changed".into());
        }
        roots.push(effects.orchestration.clone());
        roots
    } else {
        let mut roots = effects.include_paths().to_vec();
        roots.push(selected.runtime_actors());
        roots.push(selected.runtime_stdlib());
        roots
    };
    includes.extend(selected.include.iter().cloned());
    // Assemble the same prepared-STG scaffold as ordinary one-shot runs.
    let preamble = format!(
        "{{-# LANGUAGE DataKinds #-}}\nmodule RecipeMain where\n\
         import Control.Monad.Freer\nimport Tidepool.Check\n\
         import qualified {module}\n"
    );
    let source = tidepool_runtime::session::assemble_expression_module(
        &preamble,
        "result",
        "'[RecipeCheck]",
        entry,
        tidepool_runtime::session::ExpressionLift::Effectful,
    );
    let target = tidepool_runtime::session::PREPARED_SCAFFOLD_TARGET;
    let artifacts = super::run_compiler_preparation(|settlement| {
        tidepool_runtime::compile_targets(&source, &[target], &includes, |_, _, _| {}, settlement)
            .map_err(|error| -> Box<dyn std::error::Error> {
                Box::new(render_recipe_compile_error(error.into(), &source, entry))
            })
    })?;
    Ok(PreparedRecipe {
        artifacts,
        source,
        entry: entry.to_owned(),
    })
}

struct Driver {
    // Retain storage until resident shutdown and custody cleanup finish.
    repository: tempfile::TempDir,
    runtime: tempfile::TempDir,
    config: ActorHostConfig,
    session: Option<ModelFreeSession>,
    output_store: Option<Arc<harness::store::Store>>,
    shutting_down: bool,
    installations: HashMap<ActorRef, LocalResidentInstallation>,
    pending: VecDeque<LocalResidentDeployment>,
    assertions: Vec<String>,
    executor: tokio::runtime::Handle,
    round: usize,
    resource_policy: exomonad_node::command_resources::CommandResourcePolicy,
    resource_run: String,
    command_resources: Option<Arc<exomonad_node::command_resources::CommandResourceClient>>,
    command_producers: HashSet<ActorRef>,
}

impl Driver {
    async fn acquire_session(config: &ActorHostConfig) -> Result<ModelFreeSession> {
        // Every recipe incarnation supplies the selected spec with Reflect.
        // Its typed unavailable service preserves the absence of a provider.
        ModelFreeSession::start_with_conversation(
            config,
            |admission| admission,
            Some(Arc::new(|_, _| {
                Box::pin(async { Err(exomonad_actor::ConversationUnavailable::Unbound) })
            })),
        )
        .await
    }

    async fn start(workspace: &Path, selected: FrozenWorkspace) -> Result<Self> {
        let repository = tempfile::tempdir()?;
        let runtime = tempfile::tempdir()?;
        let git = exomonad_worktree::GitCli::new();
        git.init_repository(repository.path(), &["--quiet"])?;
        git.try_run(
            repository.path(),
            &["config", "user.name", "Exomonad recipe check"],
        )?;
        git.try_run(
            repository.path(),
            &["config", "user.email", "recipe-check@localhost"],
        )?;
        git.try_run(repository.path(), &["config", "commit.gpgsign", "false"])?;
        git.try_run(
            repository.path(),
            &["config", "core.hooksPath", "/dev/null"],
        )?;
        crate::exomonad::workspace::copy_authored(workspace, repository.path())?;
        // The pin travels with the package: `nix` resolves a project's pinned
        // Haskell source from its tracked `flake.nix`.
        git.try_run(repository.path(), &["add", "--all", "--", "."])?;
        git.try_run(
            repository.path(),
            &[
                "-c",
                "commit.gpgsign=false",
                "commit",
                "--quiet",
                "-m",
                "candidate workspace",
            ],
        )?;
        let defaults = selected.config()?;
        let resource_policy = defaults.resources.clone();
        let haskell_root = selected.runtime_actors().to_path_buf();
        let config = ActorHostConfig {
            run_id: "selection-0".into(),
            systemd_slice: None,
            source_exclude: defaults.launch.source_exclude,
            source_import: defaults.launch.source,
            command_resources: None,
            exomonad_executable: std::env::current_exe()?,
            workspace_inputs: Some(selected),
            haskell_root,
            workspace: repository.path().to_path_buf(),
            run_directory: tidepool_atomic_write::DirectoryAnchor::open_existing(runtime.path())?
                .child("selection-0")?,
            root_binding_path: runtime.path().join("unused-native-binding.json"),

            embedded: None,
            tmux_session: "unused-in-recipe-check".into(),
            model: defaults.defaults.model,
            effort: defaults.defaults.effort.into(),

            pane_environment: crate::exomonad::pane_environment(),
            jev: None,
        };
        let session = Self::acquire_session(&config).await?;
        let installations =
            HashMap::from([(session.actor.identity(), session.root_installation.clone())]);
        Ok(Self {
            repository,
            runtime,
            config,
            session: Some(session),
            output_store: None,
            shutting_down: false,
            installations,
            pending: VecDeque::new(),
            assertions: Vec::new(),
            executor: tokio::runtime::Handle::current(),
            round: 0,
            resource_policy,
            resource_run: format!("recipe-{}", uuid::Uuid::new_v4().simple()),
            command_resources: None,
            command_producers: HashSet::new(),
        })
    }

    fn session(&self) -> Result<&ModelFreeSession> {
        self.session
            .as_ref()
            .ok_or_else(|| runtime_error("recipe session is closed"))
    }

    fn actor_key(&self, actor: ActorRef) -> Result<CheckActor> {
        Ok((
            self.session()?.session_root.path().display().to_string(),
            actor.id.0.try_into()?,
            actor.incarnation.0.try_into()?,
        ))
    }

    fn installation(&self, key: CheckActor) -> Result<&LocalResidentInstallation> {
        if key.0 != self.session()?.session_root.path().display().to_string() {
            return Err(runtime_error("check actor belongs to an earlier swarm"));
        }
        let actor = ActorRef {
            id: exomonad_actor::ActorId(key.1.try_into()?),
            incarnation: exomonad_actor::Incarnation(key.2.try_into()?),
        };
        self.installations
            .get(&actor)
            .ok_or_else(|| runtime_error("check actor has no installed resident policy"))
    }

    fn directory(&self, key: CheckActor) -> Result<PathBuf> {
        let installation = self.installation(key)?;
        if installation.actor.identity() == self.session()?.actor.identity() {
            return Ok(self.repository.path().to_path_buf());
        }
        let [worktree] = installation.launch_worktrees.as_slice() else {
            return Err(runtime_error("check operation requires one owned worktree"));
        };
        Ok(self
            .session()?
            .worktrees
            .lookup(&exomonad_worktree::WorktreeId::from_raw(worktree))?
            .ok_or("check worktree disappeared")?
            .cwd()
            .to_path_buf())
    }

    fn path(&self, actor: CheckActor, path: &str) -> Result<PathBuf> {
        tidepool_handlers::FsReadHandler::new(self.directory(actor)?)
            .resolve(path)
            .map_err(|error| runtime_error(format!("recipe file operation: {error:?}")))
    }

    fn execute(
        &mut self,
        prepared: &PreparedRecipe,
    ) -> std::result::Result<(), RecipeCompilationFailure> {
        tokio::task::block_in_place(|| {
            tidepool_runtime::run_compiled_target(
                &prepared.artifacts,
                tidepool_runtime::session::PREPARED_SCAFFOLD_TARGET,
                tidepool_runtime::DEFAULT_NURSERY_SIZE,
                self,
                &(),
                |_| {},
            )
        })
        .map_err(|error| render_recipe_compile_error(error, &prepared.source, &prepared.entry))?;
        Ok(())
    }

    async fn turn(&mut self, key: CheckActor, source: String) -> Result<String> {
        let installation = self.installation(key)?;
        let endpoint = installation.policy.clone();
        let actor = installation.actor.clone();
        let call_id = uuid::Uuid::new_v4().simple().to_string();
        let operation = OriginalOperation {
            origin: ConversationOrigin::External {
                thread_id: "recipe-check".into(),
            },
            request_id: call_id.clone(),
            call_id,
        };
        let result = self
            .pump(endpoint.dispatch_boxed(ToolInvocation {
                context: Some(ToolInvocationContext::original(operation.clone())),
                name: exomonad_actor::HASKELL_TOOL.into(),
                arguments: ToolArguments::Raw(source.clone()),
            }))
            .await?;
        // Complete the actual admitting tool boundary before another actor is driven.
        let completion = self
            .pump(
                endpoint
                    .complete_boxed(exomonad_actor::ContextCheckpointBoundary::Hosted(operation)),
            )
            .await?;
        let result = result.map_err(|error| {
            Box::new(RecipeTurnFailure::new(
                "turn",
                &source,
                error,
                retained_actor_evidence(&actor),
            )) as Box<dyn std::error::Error>
        })?;
        completion.map_err(|error| {
            Box::new(RecipeTurnFailure::new(
                "turn completion",
                &source,
                error,
                retained_actor_evidence(&actor),
            )) as Box<dyn std::error::Error>
        })?;
        Ok(serde_json::to_string(&result)?)
    }

    async fn event(&mut self, activation: bool) -> Result<LocalResidentDeployment> {
        let matches = |event: &LocalResidentDeployment| {
            if activation {
                matches!(event, LocalResidentDeployment::SessionReady { .. })
            } else {
                matches!(event, LocalResidentDeployment::RequestUpdate { .. })
            }
        };
        if let Some(index) = self.pending.iter().position(matches) {
            return self
                .pending
                .remove(index)
                .ok_or_else(|| runtime_error("missing pending check event"));
        }
        tokio::time::timeout(Duration::from_secs(120), async {
            loop {
                let event = self
                    .session
                    .as_mut()
                    .ok_or("closed check session")?
                    .deployments
                    .recv()
                    .await
                    .ok_or("check deployment stream closed")?;
                if let Some(event) = self.handle_deployment(event).await? {
                    if matches(&event) {
                        return Ok(event);
                    }
                    self.pending.push_back(event);
                }
            }
        })
        .await
        .map_err(|_| runtime_error("recipe timed out waiting for the requested resident event"))?
    }

    async fn pump<F: Future>(&mut self, future: F) -> Result<F::Output> {
        tokio::pin!(future);
        loop {
            let event = tokio::select! {
                output = &mut future => return Ok(output),
                event = self.session.as_mut().ok_or("closed check session")?.deployments.recv() =>
                    event.ok_or("check deployment stream closed")?,
            };
            if let Some(event) = self.handle_deployment(event).await? {
                self.pending.push_back(event);
            }
        }
    }

    async fn shutdown_session(&mut self) -> Result<Vec<exomonad_actor::ForestRootShutdown>> {
        self.shutting_down = true;
        let mut deployment_error = None;
        while let Some(event) = self.pending.pop_front() {
            if let Err(error) = self.handle_deployment(event).await {
                deployment_error.get_or_insert(error);
            }
        }
        let shutdown = if let Some(session) = &self.session {
            let forest = Arc::clone(&session.forest);
            self.pump(forest.shutdown()).await
        } else {
            Err(runtime_error(
                "recipe shutdown has no resident owner observation",
            ))
        };
        let outcomes = match shutdown {
            Ok(outcomes) => outcomes,
            Err(error) => {
                let resources = self.finish_resources().await;
                return Err(match resources {
                    Ok(()) => error,
                    Err(resources) => runtime_error(format!(
                        "resident shutdown failed: {error}; command resource cleanup also failed: {resources}"
                    )),
                });
            }
        };
        let hosted_result = self.await_hosted().await;
        let resources = self.finish_resources().await;
        drop(self.output_store.take());
        drop(self.session.take());
        match (hosted_result, resources) {
            (Err(hosted), Err(resources)) => Err(runtime_error(format!(
                "resident host cleanup failed: {hosted}; command resource cleanup also failed: {resources}"
            ))),
            (Err(error), _) | (_, Err(error)) => Err(error),
            (Ok(()), Ok(())) if deployment_error.is_some() => {
                Err(deployment_error.expect("observed deployment error"))
            }
            (Ok(()), Ok(()))
                if !outcomes.is_empty()
                    && outcomes.iter().all(exomonad_actor::ForestRootShutdown::is_confirmed) =>
            {
                Ok(outcomes)
            }
            (Ok(()), Ok(())) => Err(runtime_error(format!(
                "resident forest cleanup unconfirmed: {outcomes:?}"
            ))),
        }
    }

    async fn await_hosted(&mut self) -> Result<()> {
        loop {
            let event = {
                let session = self.session.as_mut().ok_or("closed check session")?;
                tokio::select! {
                    biased;
                    event = session.deployments.recv() => event,
                    result = &mut session.hosted => {
                        result?;
                        break;
                    }
                }
            };
            match event {
                Some(event) => {
                    if let Some(event) = self.handle_deployment(event).await? {
                        self.pending.push_back(event);
                    }
                }
                None => {
                    (&mut self.session.as_mut().ok_or("closed check session")?.hosted).await?;
                    break;
                }
            }
        }
        while let Ok(event) = self
            .session
            .as_mut()
            .ok_or("closed check session")?
            .deployments
            .try_recv()
        {
            if let Some(event) = self.handle_deployment(event).await? {
                self.pending.push_back(event);
            }
        }
        Ok(())
    }

    async fn handle_deployment(
        &mut self,
        event: LocalResidentDeployment,
    ) -> Result<Option<LocalResidentDeployment>> {
        match event {
            LocalResidentDeployment::DisplayPublished(request) => {
                if self.output_store.is_none() {
                    match display_output::open_run_store(self.session()?.session_root.path()) {
                        Ok(store) => self.output_store = Some(store),
                        Err(error) => {
                            request.answer(exomonad_actor::DisplayPublicationOutcome::Unconfirmed(
                                format!("recipe output store is unavailable: {error}"),
                            ));
                            return Ok(None);
                        }
                    }
                }
                let session = self.session()?;
                display_output::publish(
                    &session.forest,
                    self.output_store
                        .as_ref()
                        .expect("recipe output store was opened"),
                    &runtime_namespace(session.session_root.path()),
                    None,
                    None,
                    &request,
                );
                Ok(None)
            }
            LocalResidentDeployment::PolicyInstalled(installation) => {
                let session = self.session()?;
                let workspace_ready = (|| -> Result<()> {
                    if let [worktree] = installation.launch_worktrees.as_slice() {
                        let tree = session
                            .worktrees
                            .lookup(&exomonad_worktree::WorktreeId::from_raw(worktree))?
                            .ok_or("admitted check worktree missing")?;
                        let principal = WorktreePrincipal::exact_actor(
                            &runtime_namespace(session.session_root.path()),
                            installation.actor.identity().id.0,
                            installation.actor.identity().incarnation.0,
                        );
                        if session
                            .bindings
                            .lock()
                            .membership(tree.id(), &principal)
                            .is_none()
                            || installation.worktree_custody.is_none()
                        {
                            return Err(runtime_error(
                                "recipe actor lacks exact admitted worktree custody",
                            ));
                        }
                    }
                    Ok(())
                })();
                if let Err(error) = workspace_ready {
                    if let Some(admission) = &installation.spawn_admission {
                        admission.fail(error.to_string());
                    }
                    return Err(error);
                }
                if installation.creator.is_none() {
                    session.authority.install_grant(
                        installation.actor.identity().into(),
                        ActorWorktreeGrant::Repository,
                    );
                }
                let actor = installation.actor.identity();
                let admission = installation.spawn_admission.clone();
                self.installations.insert(actor, *installation);
                if let Some(admission) = admission {
                    admission.acknowledge(actor)?;
                }
                Ok(None)
            }
            LocalResidentDeployment::CommandBackend(request) => {
                let resources = match &self.command_resources {
                    Some(resources) => Ok(Arc::clone(resources)),
                    None => crate::exomonad::resources::connect_existing(
                        self.resource_policy.clone(),
                        exomonad_node::command_resources::CommandRunIdentity::new(
                            self.resource_run.clone(),
                            self.session
                                .as_ref()
                                .ok_or("recipe session unavailable")?
                                .host_incarnation
                                .incarnation()
                                .0,
                        )?,
                    )
                    .await
                    .map(|resources| {
                        self.command_resources = Some(Arc::clone(&resources));
                        resources
                    }),
                };
                let backend = resources
                    .map_err(|error| {
                        tidepool_bridge_effects::CommandError::CommandUnavailable(format!(
                            "recipe command resources unavailable: {error}"
                        ))
                    })
                    .and_then(|resources| {
                        let bubblewrap = resolve_scope_bubblewrap(&self.config.pane_environment)
                            .map_err(|error| {
                                tidepool_bridge_effects::CommandError::CommandUnavailable(format!(
                                    "cannot resolve bubblewrap for recipe commands: {error}"
                                ))
                            })?;
                        let session = self.session().map_err(|error| {
                            tidepool_bridge_effects::CommandError::CommandUnavailable(
                                error.to_string(),
                            )
                        })?;
                        Ok(Arc::new(commands::HostCommandBackend::new(
                            resources,
                            request.owner,
                            resident_command_roots(
                                &session.authority,
                                &session.worktrees,
                                &self.config.workspace,
                                request.owner,
                            )
                            .map_err(|error| {
                                tidepool_bridge_effects::CommandError::CommandUnavailable(format!(
                                    "cannot resolve recipe command authority: {error}"
                                ))
                            })?,
                            bubblewrap,
                        ))
                            as Arc<dyn exomonad_actor::command_jobs::CommandBackend>)
                    });
                if backend.is_ok() {
                    self.command_producers.insert(request.owner);
                }
                request.supply(backend);
                Ok(None)
            }
            LocalResidentDeployment::NotificationSend(command) => {
                command.rejected(exomonad_actor::NotificationError::Unavailable);
                Ok(None)
            }
            LocalResidentDeployment::NotificationPoll(command) => {
                command.observed(Err(exomonad_actor::NotificationError::Unavailable));
                Ok(None)
            }
            LocalResidentDeployment::ReleaseAwait(request) => {
                let release = match self.seal_actor(request.actor).await {
                    Ok(()) => exomonad_actor::ResourceRelease::Released,
                    Err(error) => exomonad_actor::ResourceRelease::Retained(format!(
                        "command resource producer cleanup remains unconfirmed: {error}"
                    )),
                };
                request.answer(release);
                Ok(None)
            }
            LocalResidentDeployment::Retired { actor, terminal } => {
                tracing::info!(target: "exomonad_actor::lifecycle", ?actor, ?terminal,
                    cleanup = ?self.installations.get(&actor).and_then(|installation| installation.actor.terminal().cleanup()),
                    "recipe actor retired");
                self.session()?.authority.remove_grant(actor.into());
                if let Err(error) = self.seal_actor(actor).await {
                    tracing::warn!(?actor, %error, "recipe command producer retirement remains unconfirmed");
                }
                Ok(None)
            }
            LocalResidentDeployment::RequestUpdate { delivery } if self.shutting_down => {
                if let Some(presentation) = delivery.begin() {
                    presentation.not_presented("recipe session is shutting down".into());
                }
                Ok(None)
            }
            event @ (LocalResidentDeployment::SessionReady { .. }
            | LocalResidentDeployment::RequestUpdate { .. }) => Ok(Some(event)),
            LocalResidentDeployment::ChildExited { notice } => {
                tracing::info!(target: "exomonad_actor::lifecycle", owner = ?notice.owner,
                    child = ?notice.child.identity(), terminal = ?notice.terminal,
                    cleanup = ?notice.child.terminal().cleanup(), "recipe child exited");
                Ok(None)
            }
            LocalResidentDeployment::WatchChanged { .. }
            | LocalResidentDeployment::SettlementChanged { .. }
            | LocalResidentDeployment::RequestCancellation { .. } => {
                // Recipe effects observe the actor/request owners directly. These
                // notices neither acknowledge requests nor certify target cleanup.
                Ok(None)
            }
        }
    }

    async fn seal_actor(&mut self, actor: ActorRef) -> std::io::Result<()> {
        if self.command_producers.contains(&actor) {
            let resources = self
                .command_resources
                .as_ref()
                .ok_or_else(|| std::io::Error::other("command resource owner is unavailable"))?;
            resources.seal_producer(&producer(actor)).await?;
            self.command_producers.remove(&actor);
        }
        Ok(())
    }

    async fn finish_resources(&mut self) -> Result<()> {
        let mut failures = Vec::new();
        if let Some(resources) = &self.command_resources {
            for actor in self.command_producers.clone() {
                match resources.seal_producer(&producer(actor)).await {
                    Ok(()) => {
                        self.command_producers.remove(&actor);
                    }
                    Err(error) => failures.push(format!("{actor:?}: {error}")),
                }
            }
        } else if !self.command_producers.is_empty() {
            return Err(runtime_error(
                "command resource producers remain unsealed: resource owner unavailable",
            ));
        }
        if failures.is_empty() {
            self.command_resources = None;
            Ok(())
        } else {
            Err(runtime_error(format!(
                "command resource producers remain unsealed: {}",
                failures.join("; ")
            )))
        }
    }

    async fn restart(&mut self) -> Result<String> {
        self.shutdown_session().await?;
        self.installations.clear();
        self.pending.clear();
        self.round += 1;
        self.resource_run = format!("recipe-{}", uuid::Uuid::new_v4().simple());
        self.config.run_directory =
            tidepool_atomic_write::DirectoryAnchor::open_existing(self.runtime.path())?
                .child(format!("selection-{}", self.round))?;
        let selected =
            FrozenWorkspace::load(self.repository.path(), &self.config.run_directory.path())?;
        let identity = selected.identity().to_owned();
        let defaults = selected.config()?;
        self.config.model = defaults.defaults.model;
        self.config.effort = defaults.defaults.effort.into();
        self.config.workspace_inputs = Some(selected);
        let session = Self::acquire_session(&self.config).await?;
        self.installations
            .insert(session.actor.identity(), session.root_installation.clone());
        self.session = Some(session);
        self.shutting_down = false;
        Ok(identity)
    }

    fn service(&mut self, request: RecipeCheckReq, cx: &EffectContext<'_>) -> Result<Response> {
        use RecipeCheckReq::*;
        let executor = self.executor.clone();
        Ok(match request {
            RecipeRoot => cx.respond(self.actor_key(self.session()?.actor.identity())?)?,
            RecipeTurn(actor, source) => {
                cx.respond(executor.block_on(self.turn(actor, source))?)?
            }
            RecipeActivation => {
                let LocalResidentDeployment::SessionReady { activation } =
                    executor.block_on(self.event(true))?
                else {
                    return Err(runtime_error("expected activation"));
                };
                let actor = self
                    .installations
                    .get(&activation.id.actor())
                    .ok_or("activation without installation")?;
                let model = actor
                    .model
                    .as_ref()
                    .map(|model| super::resolve_model(&self.config, model))
                    .transpose()
                    .map_err(runtime_error)?;
                cx.respond((
                    self.actor_key(actor.actor.identity())?,
                    (actor.label.clone(), activation.message, model),
                ))?
            }
            RecipeGit(actor, arguments) => cx.respond(
                exomonad_worktree::GitCli::new()
                    .try_run(&self.directory(actor)?, &arguments)?
                    .trimmed()
                    .to_owned(),
            )?,
            RecipeWrite(actor, path, contents) => {
                let path = self.path(actor, &path)?;
                std::fs::create_dir_all(path.parent().ok_or("recipe file has no parent")?)?;
                tidepool_atomic_write::write_durable(&path, contents.as_bytes())?;
                cx.respond(())?
            }
            RecipeRead(actor, path) => {
                cx.respond(std::fs::read_to_string(self.path(actor, &path)?)?)?
            }
            RecipePresent | RecipeNotPresented(_) | RecipeUnconfirmed(_) => {
                let LocalResidentDeployment::RequestUpdate { delivery } =
                    executor.block_on(self.event(false))?
                else {
                    return Err(runtime_error("expected request update"));
                };
                let presentation = delivery
                    .begin()
                    .ok_or("request update was no longer presentable")?;
                let message = presentation.message().to_owned();
                match request {
                    RecipePresent => presentation.presented(),
                    RecipeNotPresented(reason) => presentation.not_presented(reason),
                    RecipeUnconfirmed(reason) => presentation.unconfirmed(reason),
                    _ => unreachable!(),
                }
                cx.respond(message)?
            }
            RecipeAssert(name, holds) => {
                if !holds {
                    return Err(runtime_error(format!(
                        "recipe assertion failed: {name}; expected: True; observed: {holds}"
                    )));
                }
                println!("  passed: {name}");
                self.assertions.push(name);
                cx.respond(())?
            }
            RecipeRestart => cx.respond(executor.block_on(self.restart())?)?,
        })
    }
}

fn producer(actor: ActorRef) -> exomonad_node::command_resources::CommandActorIdentity {
    exomonad_node::command_resources::CommandActorIdentity::new(actor.id.0, actor.incarnation.0)
}

fn retained_actor_evidence(actor: &exomonad_actor::LocalActorRef) -> String {
    let terminal = actor.terminal();
    let retained = match terminal.get() {
        Some(terminal) => format!("{terminal:?}"),
        None => "not yet published (does not establish liveness)".into(),
    };
    let cleanup = match terminal.cleanup() {
        Some(cleanup) => format!("{cleanup:?}"),
        None => "not yet observed (does not establish release)".into(),
    };
    format!(
        "actor {}; retained terminal: {retained}; retained cleanup: {cleanup}",
        actor.identity()
    )
}

impl DispatchEffect for Driver {
    fn dispatch(
        &mut self,
        request: &HaskellValue,
        cx: &EffectContext<'_>,
    ) -> std::result::Result<Option<Response>, EffectError> {
        let request = RecipeCheckReq::from_value(request, cx.table())?;
        self.service(request, cx)
            .map(Some)
            .map_err(|error| EffectError::Handler(error.to_string()))
    }

    fn prepare_dispatch(
        &mut self,
        request: &HaskellValue,
        cx: &EffectContext<'_>,
    ) -> std::result::Result<EffectDispatch, EffectError> {
        // RecipeCheck runs a private one-shot machine whose driver owns the
        // mutable check session; no shared actor checkout waits on service.
        self.dispatch(request, cx)
            .map(|response| response.map_or(EffectDispatch::Unhandled, EffectDispatch::Immediate))
    }
}

#[cfg(test)]
mod tests {
    use super::resolve_scope_bubblewrap;
    use super::select_checks;
    use std::cell::RefCell;
    use std::collections::BTreeMap;

    #[test]
    fn recipe_turn_diagnostics_preserve_original_transport_error() {
        let error = super::RecipeTurnFailure::new(
            "turn completion",
            "pure ()",
            exomonad_actor::ResidentToolError::Unavailable("original completion failure".into()),
            "actor 2@1; retained terminal: not yet published".into(),
        );
        let original = std::error::Error::source(&error)
            .unwrap()
            .downcast_ref::<exomonad_actor::ResidentToolError>()
            .unwrap();
        assert!(
            matches!(original, exomonad_actor::ResidentToolError::Unavailable(detail)
            if detail == "original completion failure")
        );
        let detail = error.to_string();
        assert!(detail.contains(&original.to_string()), "{detail}");
        assert!(
            detail.contains("actor 2@1; retained terminal: not yet published"),
            "{detail}"
        );
    }

    #[test]
    fn preparation_result_reaches_execution_only_after_confirmed_close() {
        let events = RefCell::new(Vec::new());
        let prepared = super::super::run_compiler_preparation(|settlement| {
            events.borrow_mut().push("compile");
            settlement(tidepool_runtime::CompilerTransactionClose::Clean);
            events.borrow_mut().push("closed");
            Ok(42)
        })
        .unwrap();
        events.borrow_mut().push("execute");
        assert_eq!(prepared, 42);
        assert_eq!(*events.borrow(), ["compile", "closed", "execute"]);
    }

    #[test]
    fn preparation_refusal_and_unconfirmed_cancellation_never_authorize_execution() {
        for refuses in [false, true] {
            for confirmed in [false, true] {
                let mut executed = false;
                let result = super::super::run_compiler_preparation(|settlement| {
                    settlement(if confirmed {
                        tidepool_runtime::CompilerTransactionClose::Clean
                    } else {
                        tidepool_runtime::CompilerTransactionClose::Unconfirmed(
                            tidepool_extract_cmd::CompilerTransactionCloseEvidence::new(
                                tidepool_extract_cmd::CompilerTransactionCloseReason::Cancelled,
                                tidepool_extract_cmd::CompilerTransactionRetirement::DaemonUnobserved {
                                    disconnect: None,
                                },
                            ),
                        )
                    });
                    if refuses {
                        Err(super::runtime_error("recipe source refused"))
                    } else {
                        Ok(42)
                    }
                });
                let result = result.map(|prepared| {
                    executed = true;
                    prepared
                });
                assert_eq!(executed, confirmed && !refuses);
                assert_eq!(result.is_ok(), confirmed && !refuses);
                if !confirmed {
                    assert!(result
                        .unwrap_err()
                        .is::<super::super::PreparationCleanupUnconfirmed<i32>>());
                }
            }
        }
    }

    #[tokio::test]
    async fn cancelled_preparation_closes_admission_without_publishing_its_result() {
        let mut owner = exomonad_actor::CompilerPreparationOwner::new();
        let cleanup = owner.cleanup();
        let (entered, admitted) = tokio::sync::oneshot::channel();
        let mut operation = Box::pin(owner.scope(async {
            let prepared = exomonad_actor::ActorCompilerCloseOwner::current()
                .unwrap()
                .run(|settlement| {
                    settlement(tidepool_runtime::CompilerTransactionClose::Clean);
                    42
                })
                .unwrap();
            entered.send(()).unwrap();
            std::future::pending::<()>().await;
            prepared.action
        }));
        tokio::select! {
            _ = &mut operation => panic!("preparation must still be pending"),
            ready = admitted => ready.unwrap(),
        }
        assert!(!cleanup.observation().admission_closed);
        drop(operation);
        assert!(cleanup.observation().admission_closed);
        assert!(cleanup.observation().is_confirmed());
        assert_eq!(
            cleanup.observation().work,
            vec![exomonad_actor::CompilerWorkClose::Settled(
                tidepool_runtime::CompilerTransactionClose::Clean
            ),]
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn recipe_fresh_worker_publishes_installed_originals_before_typed_request() {
        use super::*;

        let workspace = tempfile::tempdir().unwrap();
        crate::exomonad::workspace::copy_authored(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../exomonad/examples/workspace"),
            workspace.path(),
        )
        .unwrap();
        let authored = workspace.path().join(".exomonad");
        std::fs::write(
            authored.join("Project/EpochStartupChecks.hs"),
            include_str!("fixtures/recipe_fresh_original_epoch.hs"),
        )
        .unwrap();
        crate::exomonad::edit_fixture_project_config(&authored, |project| {
            project.haskell.modules.clear();
            project.haskell.checks =
                vec!["Project.EpochStartupChecks.installedOriginalEpoch".into()];
        });
        let selection = tempfile::tempdir().unwrap();
        let selected = FrozenWorkspace::load(workspace.path(), selection.path()).unwrap();
        run(
            workspace.path(),
            &selected,
            Some("Project.EpochStartupChecks.installedOriginalEpoch"),
        )
        .await
        .expect("the real fresh worker must publish its installed source before request execution");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn recipe_display_commits_and_reopens_in_each_restarted_session() {
        use super::*;
        use harness::store::actor_output::ActorOutputOrigin;

        let workspace = tempfile::tempdir().unwrap();
        crate::exomonad::workspace::copy_authored(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../exomonad/examples/workspace"),
            workspace.path(),
        )
        .unwrap();
        let authored = workspace.path().join(".exomonad");
        std::fs::write(
            authored.join("Project/RecipeBoundaryChecks.hs"),
            include_str!("fixtures/recipe_unbound_restart.hs"),
        )
        .unwrap();
        crate::exomonad::edit_fixture_project_config(&authored, |project| {
            project.haskell.modules.clear();
            project.haskell.checks = vec!["Project.RecipeBoundaryChecks.displayPublication".into()];
        });
        let selection = tempfile::tempdir().unwrap();
        let selected = FrozenWorkspace::load(workspace.path(), selection.path()).unwrap();
        let prepared = tokio::task::block_in_place(|| {
            prepare_recipe("Project.RecipeBoundaryChecks.displayPublication", &selected)
        })
        .unwrap();
        let mut driver = Driver::start(workspace.path(), selected).await.unwrap();
        let mut origins = Vec::new();
        for round in 0..2 {
            let result = driver.execute(&prepared);
            if let Err(error) = result {
                driver.shutdown_session().await.unwrap();
                panic!("actual display recipe failed: {error}");
            }
            assert_eq!(driver.assertions.len(), round + 1);
            let session = driver.session().unwrap();
            let root = Arc::clone(&session.session_root);
            let actor = session.actor.identity();
            let origin = ActorOutputOrigin {
                run: runtime_namespace(root.path()),
                native_actor: actor.id.0,
                incarnation: actor.incarnation.0,
            };
            let history = driver
                .output_store
                .as_ref()
                .unwrap()
                .actor_output_page(&origin, 0, 10)
                .unwrap();
            assert_eq!(history.outputs.len(), 1);
            assert_eq!(history.outputs[0].emission().page.text, "True");
            origins.push(origin.clone());
            if round == 0 {
                driver.restart().await.unwrap();
                assert!(driver.output_store.is_none());
            } else {
                let actor = session.actor.clone();
                let key = driver.actor_key(actor.identity()).unwrap();
                let refusal = driver
                    .turn(key.clone(), "pure (1 :: Bool)".into())
                    .await
                    .unwrap();
                let refusal: serde_json::Value = serde_json::from_str(&refusal).unwrap();
                assert_eq!(refusal["status"], "rejected", "{refusal}");
                let retained = retained_actor_evidence(&actor);
                assert!(
                    retained.contains("retained terminal: not yet published"),
                    "{retained}"
                );
                assert!(
                    retained.contains("retained cleanup: not yet observed"),
                    "{retained}"
                );
                assert!(actor.terminal().get().is_none());
                let continued = driver.turn(key.clone(), "pure ()".into()).await.unwrap();
                let continued: serde_json::Value = serde_json::from_str(&continued).unwrap();
                assert_eq!(continued["status"], "committed", "{continued}");
                let terminal = exomonad_actor::ActorTerminal::new(
                    exomonad_actor::ActorExitKind::Cancelled,
                    "exact recipe diagnostic retirement",
                );
                driver
                    .pump(actor.retire_by(actor.identity(), terminal.clone()))
                    .await
                    .unwrap()
                    .unwrap();
                let rejection = driver
                    .turn(key, "pure ()".into())
                    .await
                    .unwrap_err()
                    .to_string();
                assert!(
                    rejection.contains("actor mailbox admission is closed"),
                    "{rejection}"
                );
                assert!(
                    rejection.contains(&format!("actor {}; retained terminal:", actor.identity())),
                    "{rejection}"
                );
                assert!(rejection.contains(&terminal.summary), "{rejection}");
                assert!(
                    rejection.contains("retained cleanup: ResidentCleanupOutcome"),
                    "{rejection}"
                );
                assert!(actor.terminal().cleanup().unwrap().is_confirmed());
                driver.shutdown_session().await.unwrap();
            }
            let reopened = display_output::open_run_store(root.path()).unwrap();
            let recovered = reopened.actor_output_page(&origin, 0, 10).unwrap();
            assert_eq!(recovered.outputs.len(), 1);
            assert_eq!(
                recovered.outputs[0].reference(),
                history.outputs[0].reference()
            );
            assert_eq!(
                recovered.outputs[0].emission(),
                history.outputs[0].emission()
            );
        }
        assert_ne!(origins[0].run, origins[1].run);
    }

    #[test]
    fn recipe_commands_use_the_frozen_declared_environment() {
        let environment = crate::exomonad::pane_environment();
        let path = environment
            .get("PATH")
            .expect("recipe commands require the frozen command PATH");
        assert_eq!(std::env::var("PATH").unwrap(), *path);
        let bubblewrap = resolve_scope_bubblewrap(&environment)
            .expect("declared recipe command tools include executable bubblewrap");
        assert!(bubblewrap.is_absolute());

        let unavailable = tempfile::tempdir().unwrap();
        let unavailable_environment =
            BTreeMap::from([("PATH".into(), unavailable.path().to_str().unwrap().into())]);
        assert_eq!(
            resolve_scope_bubblewrap(&unavailable_environment)
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::NotFound,
            "an explicit frozen selection cannot fall back to ambient tools"
        );
    }

    #[test]
    fn recipe_selector_matches_one_configured_entry_exactly() {
        let checks = vec![
            "Project.Checks.workbench".to_owned(),
            "Project.JevChecks.reflex".to_owned(),
        ];

        assert_eq!(
            select_checks(&checks, Some("Project.JevChecks.reflex")).unwrap(),
            vec![&checks[1]]
        );
        assert_eq!(
            select_checks(&checks, None).unwrap(),
            vec![&checks[0], &checks[1]]
        );
    }

    #[test]
    fn recipe_selector_rejects_unconfigured_entries() {
        let checks = vec!["Project.Checks.workbench".to_owned()];
        let error = select_checks(&checks, Some("Project.Checks.missing")).unwrap_err();
        assert!(error.to_string().contains("Project.Checks.missing"));
        assert!(error.to_string().contains("Project.Checks.workbench"));
    }
}

#[cfg(test)]
mod prepared_contract_tests;
