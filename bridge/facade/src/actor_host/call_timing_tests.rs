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
    let backend = TestCommands::completed("hi\n");
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
    let _ = exec_ms; // present with a real (possibly zero) command-effect total

    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}
