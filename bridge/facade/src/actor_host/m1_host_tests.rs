use super::*;
use crate::exomonad::EmbeddedLaunchConfig;
use futures_util::StreamExt;
use std::time::Duration;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

#[tokio::test]
async fn production_host_marks_embedded_root_ready_and_retires_invalid_auth_failure() {
    let files = tempfile::tempdir().unwrap();
    let assets = files.path().join("assets");
    std::fs::create_dir_all(&assets).unwrap();
    std::fs::write(assets.join("index.html"), "<!doctype html>").unwrap();
    let secret_file = files.path().join("browser-secret");
    std::fs::write(&secret_file, "offline-host-test-secret-32-bytes-long").unwrap();
    let auth_file = files.path().join("codex-auth.json");
    std::fs::write(&auth_file, "{}").unwrap();
    let settings = EmbeddedLaunchConfig {
        listen: "127.0.0.1:0".parse().unwrap(),
        asset_root: assets,
        session_secret_file: secret_file,
        codex_auth_file: auth_file,
        context_capacity_tokens: 200_000,
        concurrent_jobs: 1,
    };
    let mut campaign = test_campaign::TestCampaign::start_with_config(
        exomonad_actor::ResearchPolicy::default(),
        |admission| admission,
        |config| {
            config.backend = crate::exomonad::ExomonadBackend::Embedded;
            config.embedded = Some(settings.clone());
        },
    )
    .await;
    std::fs::create_dir_all(&campaign.config.run_root).unwrap();
    let mut root_installation = campaign.root_installation.clone();
    root_installation.initial_user_message = Some("begin offline authentication check".into());
    let (lifecycle_tx, lifecycle_rx) = mpsc::channel(32);
    lifecycle_tx
        .send(LocalResidentDeployment::PolicyInstalled(Box::new(
            root_installation,
        )))
        .await
        .unwrap();
    let deployments = campaign.take_deployments();
    let forward = tokio::spawn(async move {
        let mut deployments = deployments;
        while let Some(deployment) = deployments.recv().await {
            if lifecycle_tx.send(deployment).await.is_err() {
                break;
            }
        }
    });

    let embedded_service =
        embedded_service::EmbeddedService::prepare(&campaign.config.run_root, &settings)
            .await
            .unwrap();
    let (readiness_tx, mut readiness_rx) = mpsc::unbounded_channel();
    let (shutdown_tx, shutdown_rx) = watch::channel(None);
    let (_config_tx, config_rx) = watch::channel(campaign.config.clone());
    let actor = campaign.actor.clone();
    let fleet = InteractiveFleet {
        root: actor,
        config: campaign.config.clone(),
        run_root: campaign.config.run_root.clone(),
        tmux: TmuxSession::new(&campaign.config.tmux_session).unwrap(),
        backend: native_interactive_backend(campaign.config.interactive_agent.clone()),
        worktrees: campaign.worktrees.clone(),
        bindings: campaign.bindings.clone(),
        readiness: readiness_tx,
        worktree_authority: campaign.authority.clone(),
        watch_retention: Arc::new(|_, _| false),
        watch_observation: Arc::new(|_, _, _| false),
        open_request: Arc::new(|_| None),
        source_layers: None,
        actor_recovery: exomonad_actor::ActorRecoveryJournal::open(
            campaign.config.run_root.join("actor-lifecycle.v2.jsonl"),
        )
        .unwrap(),
        recovered_threads: Arc::new(BTreeMap::new()),
        recovered_root_predecessor: None,
    };
    let host = tokio::spawn(run_interactive_applications(
        lifecycle_rx,
        Arc::new(Mutex::new(HashMap::new())),
        fleet,
        shutdown_rx,
        config_rx,
        Some(embedded_service),
    ));

    let readiness = tokio::time::timeout(Duration::from_secs(30), readiness_rx.recv())
        .await
        .expect("production host did not publish embedded readiness")
        .expect("host dropped readiness before publishing it");
    let address = match readiness {
        ActorHostReadiness::EmbeddedReady { root, address } => {
            assert_eq!(root, campaign.actor.identity());
            address
        }
        other => panic!("production host reported unexpected readiness: {other:?}"),
    };
    let terminal = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if let Some(terminal) = campaign.actor.terminal().get() {
                break terminal;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("invalid credentials did not fail and retire the hosted root");
    assert_eq!(terminal.kind, ActorExitKind::Failed, "{terminal:?}");

    let api = format!("http://{address}/api");
    let origin = format!("https://{address}");
    let client = reqwest::Client::new();
    let secret = std::fs::read_to_string(settings.session_secret_file).unwrap();
    let login = client
        .post(format!("{api}/session"))
        .header("Origin", &origin)
        .json(&serde_json::json!({ "secret": secret }))
        .send()
        .await
        .unwrap();
    assert_eq!(login.status(), reqwest::StatusCode::OK);
    let cookie = login.headers()[reqwest::header::SET_COOKIE]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let mut request = format!("ws://{address}/api/ws")
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert("Origin", origin.parse().unwrap());
    request
        .headers_mut()
        .insert("Cookie", cookie.parse().unwrap());
    let (mut socket, _) = tokio_tungstenite::connect_async(request).await.unwrap();
    let snapshot: serde_json::Value =
        serde_json::from_str(socket.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
    assert_eq!(snapshot["snapshot"]["actors"][0]["lifecycle"], "lost");
    assert_eq!(
        snapshot["snapshot"]["conversations"][0]["state"],
        "unavailable"
    );
    let response = client
        .post(format!("{api}/commands"))
        .header("Origin", &origin)
        .header(reqwest::header::COOKIE, &cookie)
        .json(&harness::server::ClientCommand::Submit {
            command: "must not wake a retired root".into(),
        })
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::ACCEPTED);
    let rejection = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let frame = socket.next().await.unwrap().unwrap();
            let event: serde_json::Value = serde_json::from_str(frame.to_text().unwrap()).unwrap();
            if event["event"]["event"]["kind"] == "command.rejected" {
                break event;
            }
        }
    })
    .await
    .expect("host did not reject browser input after root retirement");
    assert_eq!(
        rejection["event"]["event"]["value"]["error"],
        "embedded root is unavailable"
    );
    socket.close(None).await.unwrap();

    shutdown_tx.send_replace(Some(NativeRetirement::Terminate));
    let host_result = tokio::time::timeout(Duration::from_secs(30), host)
        .await
        .expect("production host did not shut down")
        .expect("production host task panicked");
    assert!(host_result.is_ok(), "host cleanup failed: {host_result:?}");
    campaign.forest.shutdown().await;
    let _ = campaign.hosted.await;
    forward.abort();
    assert!(forward.await.unwrap_err().is_cancelled());
}
