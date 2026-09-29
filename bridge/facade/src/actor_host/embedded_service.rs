use std::{num::NonZeroU64, path::Path, sync::Arc};

use exomonad_actor::LocalResidentInstallation;
use harness::{
    engine::EngineConfig,
    model::{AgentPath, Effort},
    server::{ClientCommand, QueuedCommand, ServerConfig, ServerControl, SessionSecret},
    transport::{auth::CodexFileAuth, ResponsesClient},
};
use tokio::{
    net::TcpListener,
    sync::{mpsc, watch},
    task::JoinHandle,
};

use super::{
    embedded_harness::{EmbeddedConversation, EmbeddedHarnessRuntime},
    embedded_policy::EmbeddedPolicyInstallation,
};
use crate::exomonad::EmbeddedLaunchConfig;

pub(super) struct EmbeddedService {
    pub(super) runtime: Arc<EmbeddedHarnessRuntime>,
    pub(super) commands: mpsc::Receiver<QueuedCommand>,
    pub(super) control: ServerControl,
    server: Option<JoinHandle<Result<(), String>>>,
    pub(super) address: std::net::SocketAddr,
}

pub(super) struct EmbeddedActor {
    pub(super) conversation: Arc<harness::embedding::Conversation>,
    pub(super) driver: EmbeddedConversation,
    pub(super) cancellation: watch::Sender<bool>,
    pub(super) cancellation_rx: watch::Receiver<bool>,
}

impl EmbeddedService {
    /// Bind the browser and open the sole run Store before admitting an actor.
    pub(super) async fn prepare(
        run_root: &Path,
        settings: &EmbeddedLaunchConfig,
    ) -> Result<Self, String> {
        settings.validate().map_err(|error| error.to_string())?;
        let runtime = Arc::new(
            EmbeddedHarnessRuntime::open(run_root, settings.concurrent_jobs)
                .map_err(|error| error.to_string())?,
        );
        let secret = std::fs::read_to_string(&settings.session_secret_file)
            .map_err(|error| error.to_string())?;
        let secret = SessionSecret::new(secret.trim_end_matches(['\r', '\n']).to_owned())
            .map_err(str::to_owned)?;
        let server_config = ServerConfig::new(settings.asset_root.clone())
            .with_history_store(runtime.store())
            .with_browser_session(secret, std::time::Duration::from_secs(8 * 60 * 60))
            .map_err(str::to_owned)?
            .with_public_origin_scheme("https")
            .map_err(str::to_owned)?;
        let listener = TcpListener::bind(settings.listen)
            .await
            .map_err(|error| error.to_string())?;
        let address = listener.local_addr().map_err(|error| error.to_string())?;
        let (router, control, commands) = harness::server::server_with_config(server_config);
        let server = tokio::spawn(async move {
            axum::serve(listener, router)
                .await
                .map_err(|error| error.to_string())
        });
        Ok(Self {
            runtime,
            commands,
            control,
            server: Some(server),
            address,
        })
    }

    pub(super) fn server_finished(&self) -> bool {
        self.server.as_ref().is_some_and(JoinHandle::is_finished)
    }

    pub(super) async fn shutdown(&mut self) -> Result<(), String> {
        let Some(server) = self.server.take() else {
            return Ok(());
        };
        if !server.is_finished() {
            server.abort();
        }
        match server.await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(error)) => Err(error),
            Err(error) if error.is_cancelled() => Ok(()),
            Err(error) => Err(error.to_string()),
        }
    }
}

pub(super) async fn attach_actor(
    service: &EmbeddedService,
    run_root: &Path,
    path: AgentPath,
    parent: Option<AgentPath>,
    installation: LocalResidentInstallation,
    initial_input: Option<String>,
) -> Result<EmbeddedActor, String> {
    let actor = installation.actor.identity();
    let identity = harness::embedding::HostIdentity {
        run: super::runtime_namespace(run_root),
        actor: path,
        incarnation: actor.incarnation.0.to_string(),
    };
    let policy = Arc::new(EmbeddedPolicyInstallation::from_installation(&installation));
    let embedded = service
        .runtime
        .attach(
            identity,
            installation.actor.clone(),
            policy,
            parent.as_ref(),
        )
        .map_err(|error| error.to_string())?;
    if let Some(input) = initial_input {
        embedded
            .conversation
            .input(
                &format!("launch:{}:{}", actor.id.0, actor.incarnation.0),
                "operator",
                &input,
            )
            .await
            .map_err(|error| error.to_string())?;
    }
    let conversation = Arc::clone(&embedded.conversation);
    let (cancel, cancellation) = watch::channel(false);
    Ok(EmbeddedActor {
        conversation,
        driver: embedded,
        cancellation: cancel,
        cancellation_rx: cancellation,
    })
}

impl Drop for EmbeddedService {
    fn drop(&mut self) {
        if let Some(server) = &self.server {
            server.abort();
        }
    }
}

pub(super) async fn drive_conversation(
    embedded: EmbeddedConversation,
    runtime: Arc<EmbeddedHarnessRuntime>,
    settings: &EmbeddedLaunchConfig,
    model: String,
    effort: Effort,
    instructions: String,
    cancellation: watch::Receiver<bool>,
    lifecycle: watch::Sender<harness::server::HostActorLifecycle>,
) -> Result<(), String> {
    drive_conversation_with_transport::<CodexFileAuth, _>(
        embedded,
        runtime,
        settings,
        model,
        effort,
        instructions,
        cancellation,
        lifecycle,
        ResponsesClient::new(CodexFileAuth::new(settings.codex_auth_file.clone())),
    )
    .await
}

pub(super) async fn drive_conversation_with_transport<A, C>(
    embedded: EmbeddedConversation,
    runtime: Arc<EmbeddedHarnessRuntime>,
    settings: &EmbeddedLaunchConfig,
    model: String,
    effort: Effort,
    instructions: String,
    mut cancellation: watch::Receiver<bool>,
    lifecycle: watch::Sender<harness::server::HostActorLifecycle>,
    transport: C,
) -> Result<(), String>
where
    A: harness::transport::Auth,
    C: harness::engine::ResponsesTransport,
{
    let EmbeddedConversation {
        conversation,
        mut incoming,
        ..
    } = embedded;
    let actor = conversation.identity().actor.clone();
    let engine = conversation
        .engine::<A, _>(
            transport,
            runtime.scheduler(),
            EngineConfig {
                instructions,
                tools: Vec::new(),
                model,
                effort,
                session_id: format!(
                    "{}:{}:{}",
                    conversation.identity().run,
                    actor.0,
                    conversation.identity().incarnation
                ),
                agent: actor.clone(),
            },
            NonZeroU64::new(settings.context_capacity_tokens).ok_or("zero context capacity")?,
        )
        .map_err(|error| error.to_string())?;
    let store = runtime.store();
    let mut recovering = true;
    loop {
        if *cancellation.borrow() {
            return Ok(());
        }
        // Wakes are hints. A prior Engine round may have returned while a
        // forwarded hint remained in its receiver; the Store is authoritative.
        let first = match store
            .unread(&actor.0)
            .map_err(|error| error.to_string())?
            .first()
        {
            Some(envelope) => harness::mailbox::DurableMailboxWake {
                envelope_id: envelope.id,
            },
            None => tokio::select! {
                biased;
                changed = cancellation.changed() => {
                    if changed.is_err() || *cancellation.borrow() { return Ok(()); }
                    continue;
                }
                wake = incoming.recv() => match wake { Some(wake) => wake, None => return Ok(()) },
            },
        };
        let head = store
            .agent(&actor)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| format!("missing embedded agent {}", actor.0))?
            .head_request;
        let (forward, forwarded) = mpsc::unbounded_channel();
        forward
            .send(first)
            .map_err(|_| "embedded Engine wake receiver closed")?;
        let recovering_this_round = recovering;
        lifecycle.send_replace(harness::server::HostActorLifecycle::Running);
        let run = async {
            if recovering_this_round {
                engine
                    .run_recovering_embedded(
                        head.clone(),
                        Vec::new(),
                        cancellation.clone(),
                        forwarded,
                    )
                    .await
            } else {
                engine
                    .run_embedded(head.clone(), Vec::new(), cancellation.clone(), forwarded)
                    .await
            }
        };
        tokio::pin!(run);
        let mut incoming_closed = false;
        let completion = loop {
            tokio::select! {
                biased;
                result = &mut run => break result.map_err(|error| error.to_string())?,
                wake = incoming.recv(), if !incoming_closed => match wake {
                    Some(wake) => { forward.send(wake).ok(); }
                    None => { incoming_closed = true; }
                },
            }
        };
        if !store
            .advance_agent_head(&actor, head.as_ref(), Some(&completion.head_request))
            .map_err(|error| error.to_string())?
        {
            return Err(format!(
                "embedded conversation {} lost its durable head",
                actor.0
            ));
        }
        lifecycle.send_replace(harness::server::HostActorLifecycle::Waiting);
        recovering = false;
    }
}

pub(super) async fn submit_browser_command(
    command: QueuedCommand,
    root: &harness::embedding::Conversation,
    control: &ServerControl,
) -> Result<harness::embedding::InputReceipt, String> {
    match command.command {
        ClientCommand::Submit { command: text } => {
            let receipt = root
                .input(&command.command_id, "browser", &text)
                .await
                .map_err(|error| error.to_string())?;
            control.publish(
                "command.admitted",
                serde_json::json!({
                    "commandId": command.command_id,
                    "envelopeId": receipt.envelope_id,
                    "wakeError": receipt.wake_error,
                }),
            );
            Ok(receipt)
        }
    }
}
