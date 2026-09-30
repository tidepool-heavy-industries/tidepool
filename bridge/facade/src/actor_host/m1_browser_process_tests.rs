use super::*;

fn fixture(script: &str) -> (tempfile::TempDir, BrowserProcess) {
    let files = tempfile::tempdir().unwrap();
    let driver = files.path().join("fake-node.sh");
    let bash = std::env::var_os("TIDEPOOL_TEST_BASH").expect("declared test Bash is required");
    let sleep = std::env::var("TIDEPOOL_TEST_SLEEP").expect("declared test sleep is required");
    assert!(Path::new(&bash).is_absolute() && Path::new(&sleep).is_absolute());
    let sleep = format!("'{}'", sleep.replace('\'', "'\\''"));
    std::fs::write(&driver, script.replace("@SLEEP@", &sleep)).unwrap();
    // These fixtures use declared shell inputs and launch no browser, provider,
    // or resident compiler.
    let process = BrowserProcess::spawn(Path::new(&bash), &driver, files.path()).unwrap();
    (files, process)
}

#[tokio::test]
async fn oversized_unterminated_frame_is_rejected_before_eof() {
    let (_files, mut process) = fixture("printf '%20000s' x\nread -r ignored\n");
    let result = tokio::time::timeout(Duration::from_secs(2), process.next_frame())
        .await
        .unwrap();
    assert_eq!(result.unwrap_err(), "browser frame exceeded protocol limit");
    assert!(process.partial_frame.len() <= FRAME_LIMIT);
    assert!(process
        .finish(Err("expected frame rejection".into()), "")
        .await
        .is_err());
}

#[tokio::test]
async fn complete_frames_and_eof_are_observed_without_replay() {
    let (_files, mut process) = fixture("read -r frame\nprintf '%s\\n' '{\"type\":\"driver_started\"}' '{\"type\":\"driver_result\",\"ok\":true}'\n");
    process
        .send_frame(&serde_json::json!({"type":"ready"}))
        .await
        .unwrap();
    assert_eq!(
        process.next_frame().await.unwrap().unwrap()["type"],
        "driver_started"
    );
    assert_eq!(process.next_frame().await.unwrap().unwrap()["ok"], true);
    assert!(process.next_frame().await.unwrap().is_none());
    process.finish(Ok(()), "").await.unwrap();
}

#[tokio::test]
async fn eof_with_partial_frame_is_a_protocol_failure() {
    let (_files, mut process) = fixture("printf '%s' '{\"type\":\"driver_started\"}'\n");
    assert_eq!(
        process.next_frame().await.unwrap_err(),
        "browser exited with an unterminated frame"
    );
    process.finish(Ok(()), "").await.unwrap();
}

#[tokio::test]
async fn cancelled_read_retains_partial_frame_for_next_select() {
    let (_files, mut process) = fixture(
        "printf '%s' '{\"type\":'\nread -r release\nprintf '%s\\n' '\"driver_started\"}'\n",
    );
    assert!(
        tokio::time::timeout(Duration::from_secs(1), process.next_frame())
            .await
            .is_err()
    );
    assert!(!process.partial_frame.is_empty());
    process
        .send_frame(&serde_json::json!({"release":true}))
        .await
        .unwrap();
    assert_eq!(
        process.next_frame().await.unwrap().unwrap()["type"],
        "driver_started"
    );
    process.finish(Ok(()), "").await.unwrap();
}

#[tokio::test]
async fn deadline_stops_descendant_and_drains_bounded_redacted_stderr() {
    let secret = "private-fixture-secret";
    let (_files, mut process) = fixture(
        "trap '' TERM\n@SLEEP@ 30 &\nprintf '{\"pid\":%s}\\n' \"$!\"\nprintf 'private-fixture-secret\\n' >&2\nprintf '%70000s' x >&2\nwait\n",
    );
    let descendant = process.next_frame().await.unwrap().unwrap()["pid"]
        .as_u64()
        .unwrap();
    let retained = Arc::clone(&process.stderr);
    let error = tokio::time::timeout(
        Duration::from_secs(16),
        process.finish(Err("journey deadline".into()), secret),
    )
    .await
    .expect("process cleanup must be bounded")
    .unwrap_err();
    assert!(error.contains("journey deadline"));
    assert!(error.contains("[test-secret]"));
    assert!(!error.contains(secret));
    assert!(retained.lock().unwrap().len() <= STDERR_LIMIT);
    // An orphan reaped by init can briefly remain a zombie; it must not run.
    let stat = std::fs::read_to_string(format!("/proc/{descendant}/stat"));
    if let Ok(stat) = stat {
        assert!(
            stat.rsplit_once(") ").unwrap().1.starts_with('Z'),
            "descendant survived cleanup: {stat}"
        );
    }
}

#[tokio::test]
async fn successful_leader_exit_also_stops_pipe_holding_descendant() {
    let (_files, mut process) = fixture("@SLEEP@ 30 &\nprintf '{\"pid\":%s}\\n' \"$!\"\nexit 0\n");
    let descendant = process.next_frame().await.unwrap().unwrap()["pid"]
        .as_u64()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), process.finish(Ok(()), ""))
        .await
        .unwrap()
        .unwrap();
    if let Ok(stat) = std::fs::read_to_string(format!("/proc/{descendant}/stat")) {
        assert!(
            stat.rsplit_once(") ").unwrap().1.starts_with('Z'),
            "descendant survived leader: {stat}"
        );
    }
}
