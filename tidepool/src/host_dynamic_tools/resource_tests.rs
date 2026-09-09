//! Run inside a fresh delegated systemd scope with the native test executable.
use super::*;
use tidepool_node::command_resources::{CommandResourcePolicy, CommandResources};

#[tokio::test]
#[ignore = "requires delegated cgroups and SHOAL_NATIVE_RESOURCE_TEST executable"]
async fn matched_native_command_resources() {
    let native = std::env::var_os("SHOAL_NATIVE_RESOURCE_TEST").expect("native test executable");
    let owner = CommandResources::delegated(CommandResourcePolicy {
        memory_high_bytes: None,
        memory_max_bytes: 64 * 1024 * 1024,
        swap_max_bytes: 0,
        ..Default::default()
    })
    .unwrap();
    let directory = tempfile::tempdir().unwrap();
    let socket = directory.path().join("host.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let service =
        HostDynamicToolService::new(test_endpoint(), directory.path().join("binding.json"), None)
            .unwrap()
            .with_command_resources(Some((owner.clone(), "native".into())));
    let control = service.control();
    let server = tokio::spawn(service.serve(listener));
    let status = tokio::process::Command::new(native)
        .args([
            "--ignored",
            "--exact",
            "shoal_command_resources",
            "--nocapture",
        ])
        .env("CODEX_WORKSPACE_SNAPSHOTS", "1")
        .env("CODEX_COMMAND_RESOURCE_SOCKET", &socket)
        .env(
            "CODEX_COMMAND_WRITER_CGROUP",
            owner.actor_directory("native").unwrap(),
        )
        .status()
        .await
        .unwrap();
    for id in ["held-one", "held-two"] {
        assert!(matches!(
            owner.acquire("native", id).await.unwrap(),
            tidepool_node::command_resources::CommandResourceStatus::Admitted { .. }
        ));
    }
    let client = reqwest::Client::builder()
        .unix_socket(socket)
        .no_proxy()
        .build()
        .unwrap();
    let queued = tokio::spawn(async move {
        client
            .post("http://localhost/v1/commands/resources")
            .json(&serde_json::json!({"id": "queued-at-quiescence", "operation": "acquire"}))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json::<tidepool_node::command_resources::CommandResourceStatus>()
            .await
            .unwrap()
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while owner.status("native", "queued-at-quiescence").is_err() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    control.quiesce();
    assert!(matches!(
        queued.await.unwrap(),
        tidepool_node::command_resources::CommandResourceStatus::CancelledBeforeStart
    ));
    control.drain();
    server.await.unwrap().unwrap();
    assert!(status.success());
}
