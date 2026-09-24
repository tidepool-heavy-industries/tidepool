//! One `"call timing"` INFO line per hosted tool call, breaking down where
//! its wall time went (`exomonad_actor::call_timing`). A bash tool call
//! through the ordinary structured-tool path exercises the resident-machine
//! checkout, at least one compile round trip, and the command effect
//! handler in one call, so it is enough to prove the summary line carries
//! all of `checkout_wait_ms`/`checkout_hold_ms`/`compile_ms`/
//! `compile_count`/`jev_ms`/`jev_count`/`exec_ms`/`outcome`.
use super::command_jobs_tests::{backend_request, TestCommands};
use super::test_campaign::TestCampaign;
use super::*;
use exomonad_tool::{ToolArguments, ToolInvocation, ToolInvocationContext};

/// A minimal [`tracing_subscriber::fmt::MakeWriter`] capturing JSON log
/// lines into a shared buffer, mirroring
/// `resident_workbench`'s own `CapturedLog` test helper (see
/// `exomonad/actor/src/resident_workbench.rs`).
#[derive(Clone)]
struct CapturedLog(Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for CapturedLog {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CapturedLog {
    type Writer = CapturedLog;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

#[tokio::test]
async fn bash_call_logs_one_call_timing_summary_line() {
    let mut campaign = TestCampaign::start().await;
    let policy = campaign.root_installation.policy.clone();

    let log = CapturedLog(Arc::new(std::sync::Mutex::new(Vec::new())));
    let subscriber = tracing_subscriber::fmt()
        .json()
        .with_writer(log.clone())
        .with_max_level(tracing::Level::INFO)
        .finish();
    // A scoped, thread-local default: these tests run on `#[tokio::test]`'s
    // default current-thread runtime, so the spawned dispatch below still
    // polls on this same thread and stays under this guard.
    let _guard = tracing::subscriber::set_default(subscriber);

    let invocation = ToolInvocation {
        name: "bash".into(),
        arguments: ToolArguments::Structured(serde_json::json!({"cmd": "echo hi"})),
        context: Some(ToolInvocationContext {
            context_call_id: Some("call-timing-once".into()),
            thread_id: "call-timing-thread".into(),
            turn_id: "call-timing-turn".into(),
            call_id: "call-timing-once".into(),
            namespace: None,
        }),
    };
    let dispatch = tokio::spawn(policy.dispatch_boxed(invocation));
    // The command settles only after a real ~50ms delay, so the effect
    // boundary that awaits it (`Cmd.observe`'s `CommandAwaitWith`) spends
    // measurable wall time — proving `exec_ms` reports the command's own
    // execution, not just the near-instant `Cmd.start` dispatch.
    let backend = TestCommands::completed_after(std::time::Duration::from_millis(50), "hi\n");
    backend_request(&mut campaign)
        .await
        .supply(Ok(backend.clone()));
    let receipt = dispatch.await.unwrap().unwrap();
    assert_eq!(receipt["status"], "committed", "{receipt}");

    drop(_guard);
    let log_text = String::from_utf8(
        log.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone(),
    )
    .expect("captured log is UTF-8");

    let summary = log_text
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .find(|value| {
            value
                .get("fields")
                .and_then(|fields| fields.get("message"))
                .and_then(|message| message.as_str())
                == Some("call timing")
        })
        .unwrap_or_else(|| panic!("no \"call timing\" summary line in captured log: {log_text}"));

    let fields = summary
        .get("fields")
        .unwrap_or_else(|| panic!("\"call timing\" line has no fields: {summary}"));
    assert_eq!(fields["tool"], "bash", "{fields}");
    for field in [
        "actor",
        "incarnation",
        "total_ms",
        "checkout_wait_ms",
        "checkout_hold_ms",
        "compile_ms",
        "compile_count",
        "jev_ms",
        "jev_count",
        "exec_ms",
        "outcome",
    ] {
        assert!(fields.get(field).is_some(), "missing {field}: {fields}");
    }
    // The dispatched command's Cmd.start/Cmd.status/Cmd.output round trips
    // installed and drove a job binding, so this call compiled at least
    // once and spent time in the command effect handler.
    let compile_count = fields["compile_count"]
        .as_u64()
        .or_else(|| fields["compile_count"].as_str()?.parse().ok())
        .expect("compile_count is a number");
    assert!(compile_count >= 1, "{fields}");
    let exec_ms = fields["exec_ms"]
        .as_u64()
        .or_else(|| fields["exec_ms"].as_str()?.parse().ok())
        .expect("exec_ms is a number");
    // The backend settled only after a real ~50ms delay while this call's
    // Cmd.observe awaited it, so the command-effect total must reflect that
    // wait, not the near-instant Cmd.start dispatch alone.
    assert!(exec_ms > 0, "exec_ms did not capture the command's own execution: {fields}");

    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

/// A resident session's first cell bootstraps its machine (nothing exists
/// yet to snapshot against, so `PersistentSession::install_prepared` runs
/// single-checkout); its second cell has a machine to snapshot, so
/// `begin_ready_block_split` (`exomonad_actor::resident_workbench`) takes
/// the off-checkout compile path. Assert the second cell's
/// `tidepool_runtime::prepared_install` log line reports
/// `compiled_off_checkout=true`, proving the split -- not just the
/// single-checkout fallback -- actually ran for an ordinary cell install.
#[tokio::test]
async fn second_cell_install_compiles_off_checkout() {
    // The "prepared install" line is emitted from inside
    // `ResidentMachineAccess::with_host_machine`'s `spawn_blocking_in_span`
    // closure -- a genuinely different OS thread from Tokio's blocking
    // pool, which `tracing::subscriber::set_default`'s thread-local scope
    // (this file's other tests use it) does not reach. This test needs a
    // process-wide default instead; set it before `TestCampaign::start()`
    // so it wins over that helper's own best-effort `try_init()`. Safe
    // because nextest runs each test in its own process (no other test's
    // global default to collide with).
    let log = CapturedLog(Arc::new(std::sync::Mutex::new(Vec::new())));
    let subscriber = tracing_subscriber::fmt()
        .json()
        .with_writer(log.clone())
        .with_max_level(tracing::Level::INFO)
        .finish();
    tracing::subscriber::set_global_default(subscriber)
        .expect("first global subscriber in this test process");

    let campaign = TestCampaign::start().await;
    let policy = campaign.root_installation.policy.clone();

    // First cell: bootstraps the session's machine.
    super::tests::dispatch_haskell_script(policy.as_ref(), "let x = (1 :: Int)\nx\n").await;
    // Second cell: a machine already exists to snapshot against.
    super::tests::dispatch_haskell_script(policy.as_ref(), "let y = (2 :: Int)\ny\n").await;

    let log_text = String::from_utf8(
        log.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone(),
    )
    .expect("captured log is UTF-8");

    let installs: Vec<serde_json::Value> = log_text
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|value| {
            value.get("target").and_then(|t| t.as_str()) == Some("tidepool_runtime::prepared_install")
        })
        .collect();
    assert!(
        !installs.is_empty(),
        "no tidepool_runtime::prepared_install log lines captured: {log_text}"
    );
    let off_checkout = installs.iter().any(|line| {
        line.get("fields")
            .and_then(|fields| fields.get("compiled_off_checkout"))
            .and_then(|value| value.as_bool().or_else(|| value.as_str().map(|s| s == "true")))
            == Some(true)
    });
    assert!(
        off_checkout,
        "expected at least one prepared install with compiled_off_checkout=true \
         (the second cell's split install) among: {installs:?}"
    );

    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}
