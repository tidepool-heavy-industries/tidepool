//! An ordinary Haskell check program drives the same resident host as native actors.
//! Only this entry point installs RecipeCheck; it never starts native providers.

use super::model_free::ModelFreeSession;
use super::*;
use crate::exomonad::workspace::FrozenWorkspace;
use crate::generated::recipe_check::RecipeCheckReq;
use exomonad_tool::{ToolArguments, ToolInvocation, ToolInvocationContext};
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
        let mut driver = Driver::start(workspace, selected.clone()).await?;
        let result = driver.evaluate(entry, selected);
        let cleanup = driver.shutdown_session().await;
        driver.installations.clear();
        driver.pending.clear();
        match (result, cleanup) {
            (Err(error), Err(cleanup)) => {
                return Err(runtime_error(format!(
                    "{error}; resident cleanup also failed: {cleanup}"
                )));
            }
            (Err(error), _) | (_, Err(error)) => return Err(error),
            (Ok(()), Ok(())) => {}
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
    error: &tidepool_runtime::RuntimeError,
    source: &str,
    entry: &str,
) -> Box<dyn std::error::Error> {
    match error {
        tidepool_runtime::RuntimeError::Compile(compile_error) => {
            let detail = tidepool_runtime::session::render_turn_compile_error(
                compile_error,
                Some(source),
                entry,
                entry,
            );
            runtime_error(format!(
                "recipe {entry} compilation failed: {error}\n{detail}"
            ))
        }
        other => runtime_error(other.to_string()),
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

struct Driver {
    // Retain storage until resident shutdown and custody cleanup finish.
    repository: tempfile::TempDir,
    runtime: tempfile::TempDir,
    config: ActorHostConfig,
    session: Option<ModelFreeSession>,
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

            pane_environment: BTreeMap::new(),
            jev: None,
        };
        let session = ModelFreeSession::start(&config, |admission| admission).await?;
        let installations =
            HashMap::from([(session.actor.identity(), session.root_installation.clone())]);
        Ok(Self {
            repository,
            runtime,
            config,
            session: Some(session),
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

    fn evaluate(&mut self, entry: &str, selected: &FrozenWorkspace) -> Result<()> {
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
        let refs = includes.iter().map(PathBuf::as_path).collect::<Vec<_>>();
        // Pieces, not a whole module string: `compile_and_run` assembles the
        // prepared-STG scaffold itself (session::assemble_expression_module)
        // — see its doc for why splicing that into an already-composed
        // module string isn't safe in general.
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
        tokio::task::block_in_place(|| {
            tidepool_runtime::compile_and_run(&source, "result", &refs, self, &())
        })
        .map_err(|error| render_recipe_compile_error(&error, &source, entry))?;
        Ok(())
    }

    async fn turn(&mut self, key: CheckActor, source: String) -> Result<String> {
        let endpoint = self.installation(key)?.policy.clone();
        let call_id = uuid::Uuid::new_v4().simple().to_string();
        let result = self
            .pump(endpoint.dispatch_boxed(ToolInvocation {
                context: Some(ToolInvocationContext::external(
                    "recipe-check".into(),
                    call_id.clone(),
                    call_id.clone(),
                    Some(call_id.clone()),
                    Some("haskell".into()),
                )),
                name: exomonad_actor::HASKELL_TOOL.into(),
                arguments: ToolArguments::Raw(source.clone()),
            }))
            .await?;
        // Complete the actual admitting tool boundary before another actor is driven.
        let completion = self
            .pump(endpoint.complete_boxed(
                tidepool_runtime::session::WorkbenchForkBoundary::external(
                    "recipe-check".into(),
                    call_id.clone(),
                    call_id,
                ),
            ))
            .await?;
        let result = result.map_err(|error| {
            runtime_error(format!("resident recipe turn failed:\n{source}\n{error}"))
        })?;
        completion?;
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

    async fn shutdown_session(&mut self) -> Result<()> {
        let shutdown = if let Some(session) = &self.session {
            let forest = Arc::clone(&session.forest);
            self.pump(forest.shutdown()).await.map(|_| ())
        } else {
            Ok(())
        };
        if let Err(error) = shutdown {
            let resources = self.finish_resources().await;
            return Err(match resources {
                Ok(()) => error,
                Err(resources) => runtime_error(format!(
                    "resident shutdown failed: {error}; command resource cleanup also failed: {resources}"
                )),
            });
        }
        let hosted_result = self.await_hosted().await;
        let resources = self.finish_resources().await;
        drop(self.session.take());
        match (hosted_result, resources) {
            (Err(hosted), Err(resources)) => Err(runtime_error(format!(
                "resident host cleanup failed: {hosted}; command resource cleanup also failed: {resources}"
            ))),
            (Err(error), _) | (_, Err(error)) => Err(error),
            (Ok(()), Ok(())) => Ok(()),
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
            LocalResidentDeployment::PolicyInstalled(installation) => {
                let session = self.session()?;
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
                        .current(tree.id())
                        .map(|row| row.agent())
                        != Some(&principal)
                        || installation.worktree_custody.is_none()
                    {
                        return Err(runtime_error(
                            "recipe actor lacks exact admitted worktree custody",
                        ));
                    }
                }
                if installation.creator.is_none() {
                    session.authority.install_grant(
                        installation.actor.identity().into(),
                        ActorWorktreeGrant::Repository,
                    );
                }
                if let Some(gate) = &installation.fork_gate {
                    gate.mark_ready()?;
                }
                self.installations
                    .insert(installation.actor.identity(), *installation);
                Ok(None)
            }
            LocalResidentDeployment::CommandBackend(request) => {
                let resources = match &self.command_resources {
                    Some(resources) => Ok(Arc::clone(resources)),
                    None => crate::exomonad::resources::connect_existing(
                        self.resource_policy.clone(),
                        &self.resource_run,
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
            LocalResidentDeployment::Retired { actor, .. } => {
                self.session()?.authority.remove_grant(actor.into());
                if let Err(error) = self.seal_actor(actor).await {
                    tracing::warn!(?actor, %error, "recipe command producer retirement remains unconfirmed");
                }
                Ok(None)
            }
            other => Ok(Some(other)),
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
        let session = ModelFreeSession::start(&self.config, |admission| admission).await?;
        self.installations
            .insert(session.actor.identity(), session.root_installation.clone());
        self.session = Some(session);
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

fn producer(actor: ActorRef) -> String {
    format!("{}-{}", actor.id.0, actor.incarnation.0)
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
    use super::select_checks;

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
