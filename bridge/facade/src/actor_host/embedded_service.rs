use std::{num::NonZeroU64, path::Path};

use harness::{
    engine::EngineConfig,
    model::Effort,
    server::{ClientCommand, QueuedCommand, ServerConfig, ServerControl, SessionSecret},
    transport::{auth::CodexFileAuth, ResponsesClient},
};
use tokio::{
    net::TcpListener,
    sync::{mpsc, watch},
    task::JoinHandle,
};

use super::embedded_harness::{EmbeddedConversation, EmbeddedHarnessRuntime};
use crate::exomonad::EmbeddedLaunchConfig;

pub(super) struct EmbeddedService {
    pub(super) runtime: EmbeddedHarnessRuntime,
    pub(super) commands: mpsc::Receiver<QueuedCommand>,
    pub(super) control: ServerControl,
    pub(super) server: JoinHandle<Result<(), String>>,
    pub(super) address: std::net::SocketAddr,
}

impl EmbeddedService {
    /// Bind the browser and open the sole run Store before admitting an actor.
    pub(super) async fn prepare(
        run_root: &Path,
        settings: &EmbeddedLaunchConfig,
    ) -> Result<Self, String> {
        settings.validate().map_err(|error| error.to_string())?;
        let runtime = EmbeddedHarnessRuntime::open(run_root, settings.concurrent_jobs)
            .map_err(|error| error.to_string())?;
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
            server,
            address,
        })
    }
}

pub(super) async fn drive_conversation(
    embedded: EmbeddedConversation,
    runtime: &EmbeddedHarnessRuntime,
    settings: &EmbeddedLaunchConfig,
    model: String,
    effort: Effort,
    instructions: String,
    mut cancellation: watch::Receiver<bool>,
) -> Result<(), String> {
    let EmbeddedConversation {
        conversation,
        mut incoming,
        ..
    } = embedded;
    let actor = conversation.identity().actor.clone();
    let engine = conversation
        .engine::<CodexFileAuth, _>(
            ResponsesClient::new(CodexFileAuth::new(settings.codex_auth_file.clone())),
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
        let first = tokio::select! {
            biased;
            changed = cancellation.changed() => {
                if changed.is_err() || *cancellation.borrow() { return Ok(()); }
                continue;
            }
            wake = incoming.recv() => match wake { Some(wake) => wake, None => return Ok(()) },
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
        let completion = loop {
            tokio::select! {
                biased;
                result = &mut run => break result.map_err(|error| error.to_string())?,
                wake = incoming.recv() => match wake {
                    Some(wake) => { forward.send(wake).map_err(|_| "embedded Engine wake receiver closed")?; }
                    None => return Ok(()),
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
        recovering = false;
    }
}

pub(super) async fn submit_browser_command(
    command: QueuedCommand,
    root: &harness::embedding::Conversation,
    control: &ServerControl,
) -> Result<(), String> {
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
            Ok(())
        }
    }
}
