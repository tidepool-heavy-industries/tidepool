//! An ordinary Haskell check program drives the same resident host as native actors.
//! Only this entry point installs RecipeCheck; it never starts native providers.

use super::model_free::ModelFreeSession;
use super::*;
use crate::generated::recipe_check::RecipeCheckReq;
use crate::shoal::workspace::FrozenWorkspace;
use std::collections::VecDeque;
use tidepool_bridge::FromCore;
use tidepool_effect::dispatch::{DispatchEffect, EffectContext, Response};
use tidepool_effect::error::EffectError;
use tidepool_eval::Value;
use tidepool_tool::{ToolArguments, ToolInvocation, ToolInvocationContext};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
type CheckActor = (String, i64, i64);

pub(crate) async fn run(workspace: &Path, selected: &FrozenWorkspace) -> Result<()> {
    if selected.checks.is_empty() {
        return Err(runtime_error(
            "no Haskell recipe checks configured in haskell.checks",
        ));
    }
    for entry in &selected.checks {
        println!("Recipe {entry}; definitions {}", selected.identity());
        let mut driver = Driver::start(workspace, selected.clone()).await?;
        let result = driver.evaluate(entry, selected);
        let cleanup = match driver.session.take() {
            Some(session) => session.shutdown().await,
            None => Ok(()),
        };
        driver.installations.clear();
        driver.pending.clear();
        match (result, cleanup) {
            (Err(error), Err(cleanup)) => {
                return Err(runtime_error(format!(
                    "{error}; resident cleanup also failed: {cleanup}"
                )))
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
}

impl Driver {
    async fn start(workspace: &Path, selected: FrozenWorkspace) -> Result<Self> {
        let repository = tempfile::tempdir()?;
        let runtime = tempfile::tempdir()?;
        let git = tidepool_worktree::GitCli::new();
        git.try_run(repository.path(), &["init", "--quiet"])?;
        git.try_run(
            repository.path(),
            &["config", "user.name", "Shoal recipe check"],
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
        crate::shoal::workspace::copy_authored(workspace, repository.path())?;
        git.try_run(repository.path(), &["add", "--", ".shoal"])?;
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
        let config = ActorHostConfig {
            systemd_slice: None,
            command_resources: None,
            shoal_executable: std::env::current_exe()?,
            workspace_inputs: Some(selected),
            haskell_root: crate::haskell_sources::ensure_shoal_haskell()?,
            workspace: repository.path().to_path_buf(),
            run_root: runtime.path().join("selection-0"),
            root_binding_path: runtime.path().join("unused-native-binding.json"),
            // Used only as launch-preview metadata; no native launch consumer exists here.
            interactive_agent: tidepool_agent::native_interactive_agent_from_parts(
                std::env::current_exe()?,
                "model-free recipe check".into(),
            )?,
            tmux_session: "unused-in-recipe-check".into(),
            model: defaults.defaults.model,
            effort: defaults.defaults.effort.into(),
            research_policy: defaults.research,
            root_launch_mode: InteractiveLaunchMode::Fresh,
            pane_environment: BTreeMap::new(),
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
            id: tidepool_actor::ActorId(key.1.try_into()?),
            incarnation: tidepool_actor::Incarnation(key.2.try_into()?),
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
            .lookup(&tidepool_worktree::WorktreeId::from_raw(worktree))?
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
        let mut declarations = shoal_effect_declarations();
        declarations.push(tidepool_mcp::recipe_check_decl());
        let effects = tidepool_mcp::ensure_effects_module(&declarations)?;
        let mut includes = effects.include_paths().to_vec();
        includes.push(self.config.haskell_root.clone());
        includes.push(crate::haskell_sources::ensure_embedded_stdlib()?);
        includes.extend(selected.include.iter().cloned());
        let refs = includes.iter().map(PathBuf::as_path).collect::<Vec<_>>();
        let source = format!("{{-# LANGUAGE DataKinds #-}}\nmodule RecipeMain where\nimport Control.Monad.Freer\nimport Tidepool.Check\nimport qualified {module}\nresult :: Eff '[RecipeCheck] ()\nresult = {entry}\n");
        tokio::task::block_in_place(|| {
            tidepool_runtime::compile_and_run(&source, "result", &refs, self, &())
        })?;
        Ok(())
    }

    async fn turn(&mut self, key: CheckActor, source: String) -> Result<String> {
        let endpoint = self.installation(key)?.policy.clone();
        let call_id = uuid::Uuid::new_v4().simple().to_string();
        let result = endpoint
            .dispatch_boxed(ToolInvocation {
                context: Some(ToolInvocationContext {
                    context_call_id: Some(call_id.clone()),
                    thread_id: "recipe-check".into(),
                    turn_id: call_id.clone(),
                    call_id: call_id.clone(),
                    namespace: Some("haskell".into()),
                }),
                name: tidepool_actor::HASKELL_TOOL.into(),
                arguments: ToolArguments::Raw(source.clone()),
            })
            .await;
        // Complete the actual admitting tool boundary before another actor is driven.
        let completion = endpoint
            .complete_boxed(tidepool_runtime::session::WorkbenchForkBoundary {
                thread_id: "recipe-check".into(),
                call_id,
            })
            .await;
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
                if let LocalResidentDeployment::PolicyInstalled(installation) = event {
                    let session = self.session()?;
                    if let [worktree] = installation.launch_worktrees.as_slice() {
                        let tree = session
                            .worktrees
                            .lookup(&tidepool_worktree::WorktreeId::from_raw(worktree))?
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
                    session.authority.install_grant(
                        installation.actor.identity().into(),
                        worktree_grant(installation.effective_role.role()),
                    );
                    if let Some(gate) = &installation.fork_gate {
                        gate.mark_ready()?;
                    }
                    self.installations
                        .insert(installation.actor.identity(), installation);
                } else if matches(&event) {
                    return Ok(event);
                } else if matches!(
                    event,
                    LocalResidentDeployment::SessionReady { .. }
                        | LocalResidentDeployment::RequestUpdate { .. }
                ) {
                    self.pending.push_back(event);
                }
            }
        })
        .await
        .map_err(|_| runtime_error("recipe timed out waiting for the requested resident event"))?
    }

    async fn restart(&mut self) -> Result<String> {
        self.session
            .take()
            .ok_or("closed check session")?
            .shutdown()
            .await?;
        self.installations.clear();
        self.pending.clear();
        self.round += 1;
        self.config.run_root = self
            .runtime
            .path()
            .join(format!("selection-{}", self.round));
        let selected = FrozenWorkspace::load(self.repository.path(), &self.config.run_root)?;
        let identity = selected.identity().to_owned();
        let defaults = selected.config()?;
        self.config.model = defaults.defaults.model;
        self.config.effort = defaults.defaults.effort.into();
        self.config.research_policy = defaults.research;
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
                cx.respond((
                    self.actor_key(actor.actor.identity())?,
                    (actor.label.clone(), activation.message, actor.model.clone()),
                ))?
            }
            RecipeGit(actor, arguments) => cx.respond(
                tidepool_worktree::GitCli::new()
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
                    return Err(runtime_error(format!("recipe assertion failed: {name}")));
                }
                println!("  passed: {name}");
                self.assertions.push(name);
                cx.respond(())?
            }
            RecipeRestart => cx.respond(executor.block_on(self.restart())?)?,
        })
    }
}

impl DispatchEffect for Driver {
    fn dispatch(
        &mut self,
        request: &Value,
        cx: &EffectContext<'_>,
    ) -> std::result::Result<Option<Response>, EffectError> {
        let request = RecipeCheckReq::from_value(request, cx.table())?;
        self.service(request, cx)
            .map(Some)
            .map_err(|error| EffectError::Handler(error.to_string()))
    }
}
