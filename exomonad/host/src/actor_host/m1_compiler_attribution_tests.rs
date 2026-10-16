use super::*;

fn operation(request: &str, actor: &str) -> OperationId {
    OperationId {
        origin: ConversationIdentity::Embedded {
            run: "run".into(),
            actor: AgentPath(actor.into()),
            incarnation: "incarnation".into(),
        },
        request: RequestId(request.into()),
        call: CallId("reused-call".into()),
    }
}

fn dispatched(operation: &OperationId, execution: &str) {
    tracing::info!(turn_id = %operation.request.0, context_call_id = %operation.call.0,
        execution = %execution, "workbench cell dispatched to its actor");
}

fn identified(admission_id: u64, request_ordinal: u64) {
    tracing::info!(target: "tidepool_extract_cmd::endpoint",
        daemon_epoch = %"a".repeat(64), admission_id, request_ordinal,
        compile_request = "0123456789abcdef", transport = "daemon",
        "compiler request identified");
}

fn invocation(admission_id: u64, request_ordinal: u64) -> CompilerInvocation {
    CompilerInvocation {
        daemon_epoch: "a".repeat(64),
        admission_id,
        request_ordinal,
        compile_request: "0123456789abcdef".into(),
    }
}

#[test]
fn preparation_classification_requires_the_exact_opt_in_value() {
    assert!(!preparation_opt_in_value(None));
    assert!(preparation_opt_in_value(Some("1")));
    assert!(std::panic::catch_unwind(|| preparation_opt_in_value(Some("true"))).is_err());
}

fn request_row(request: &CompilerInvocation, message: &str, workload: &str) -> Value {
    let mut fields = serde_json::to_value(request).unwrap();
    fields["message"] = json!(message);
    fields["compiler_workload"] = json!(workload);
    fields["daemon_pid"] = json!(123);
    fields["transport"] = json!("daemon");
    fields["exit_code"] = json!(0);
    fields["worker_pid"] = json!(456);
    fields["worker"] = json!(1);
    fields["compiler_jobs"] = json!(2);
    fields["compiler_capabilities"] = json!(3);
    fields["served"] = json!(2);
    fields["elapsed_ms"] = json!(17);
    fields["followed_rotation"] = json!(false);
    fields
}

fn trace_with_rows(rows: &[Value]) -> (tempfile::NamedTempFile, DaemonTrace) {
    let file = tempfile::NamedTempFile::new().unwrap();
    let mut text = String::new();
    for row in rows {
        text.push_str(&serde_json::to_string(&json!({"fields": row})).unwrap());
        text.push('\n');
    }
    std::fs::write(file.path(), text).unwrap();
    let mut trace = DaemonTrace::open(file.path());
    trace.completion_timeout = std::time::Duration::from_millis(20);
    (file, trace)
}

fn append_rows(file: &tempfile::NamedTempFile, rows: &[Value]) {
    use std::io::Write;
    let mut output = std::fs::OpenOptions::new()
        .append(true)
        .open(file.path())
        .unwrap();
    for row in rows {
        writeln!(
            output,
            "{}",
            serde_json::to_string(&json!({"fields": row})).unwrap()
        )
        .unwrap();
    }
}

#[test]
fn compiler_attribution_keeps_interleaved_duplicate_content_invocations_exact() {
    let clients = ClientRequests::default();
    let first = operation("request-one", "/root/first");
    let second = operation("request-two", "/root/second");
    clients.issue(&first);
    clients.issue(&second);
    tracing::subscriber::with_default(tracing_subscriber::registry().with(clients.clone()), || {
        dispatched(&first, "execution-one");
        dispatched(&second, "execution-two");
        let first_cell = tracing::info_span!("cell", execution = "execution-one");
        let second_cell = tracing::info_span!("cell", execution = "execution-two");
        second_cell.in_scope(|| identified(8, 1));
        first_cell.in_scope(|| identified(7, 1));
        second_cell.in_scope(|| identified(8, 2));
    });
    assert_eq!(clients.requests(&first), [invocation(7, 1)]);
    assert_eq!(
        clients.requests(&second),
        [invocation(8, 1), invocation(8, 2)]
    );
    assert_eq!(clients.0.lock().owners.len(), 3);
}

#[test]
fn compiler_attribution_requires_the_endpoint_identified_event_in_an_actual_cell() {
    let clients = ClientRequests::default();
    let operation = operation("request", "/root");
    clients.issue(&operation);
    tracing::subscriber::with_default(tracing_subscriber::registry().with(clients.clone()), || {
        dispatched(&operation, "execution");
        identified(1, 1); // No active cell cannot acquire operation ownership.
        let cell = tracing::info_span!("cell", execution = "execution");
        cell.in_scope(|| {
            let digest = tracing::info_span!(target: "tidepool_extract_cmd::endpoint",
                "compile_request", compile_request = "0123456789abcdef");
            digest.in_scope(|| {
                tracing::info!(target: "other", daemon_epoch = %"a".repeat(64),
                    admission_id = 1_u64, request_ordinal = 1_u64,
                    compile_request = "0123456789abcdef", transport = "daemon",
                    "compiler request identified");
                tracing::info!(target: "tidepool_extract_cmd::endpoint", daemon_epoch = %"a".repeat(64),
                    admission_id = 1_u64, request_ordinal = 1_u64,
                    compile_request = "0123456789abcdef", transport = "direct",
                    "compiler request identified");
            });
        });
        assert!(clients.requests(&operation).is_empty());
        cell.in_scope(|| identified(1, 1));
    });
    assert_eq!(clients.requests(&operation), [invocation(1, 1)]);
}

#[test]
fn compiler_attribution_refuses_cross_operation_reuse_without_changing_the_owner() {
    let clients = ClientRequests::default();
    let first = operation("request-one", "/root/first");
    let second = operation("request-two", "/root/second");
    clients.issue(&first);
    clients.issue(&second);
    tracing::subscriber::with_default(tracing_subscriber::registry().with(clients.clone()), || {
        dispatched(&first, "execution-one");
        dispatched(&second, "execution-two");
        let first_cell = tracing::info_span!("cell", execution = "execution-one");
        let second_cell = tracing::info_span!("cell", execution = "execution-two");
        first_cell.in_scope(|| identified(7, 1));
        for cell in [&second_cell, &first_cell] {
            assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                cell.in_scope(|| identified(7, 1));
            }))
            .is_err());
            assert_eq!(clients.requests(&first), [invocation(7, 1)]);
            assert!(clients.requests(&second).is_empty());
            assert_eq!(clients.0.lock().owners[&invocation(7, 1).key()], first);
        }
    });
}

#[tokio::test]
async fn compiler_completion_joins_duplicate_content_by_exact_invocation() {
    let file = tempfile::NamedTempFile::new().unwrap();
    let first = invocation(7, 1);
    let second = invocation(8, 1);
    let rows = [
        (&second, "compiler request started"),
        (&first, "compiler request started"),
        (&first, "compiler request finished"),
        (&second, "compiler request finished"),
    ];
    let mut text = String::new();
    for (request, message) in rows {
        let fields = request_row(request, message, "foreground");
        text.push_str(&serde_json::to_string(&json!({"fields": fields})).unwrap());
        text.push('\n');
    }
    std::fs::write(file.path(), text).unwrap();
    let mut trace = DaemonTrace::open(file.path());
    let finished = trace
        .completion(
            &BTreeSet::from([first.clone(), second.clone()]),
            &"a".repeat(64),
            &json!(123),
            false,
        )
        .await;
    assert_eq!(finished.len(), 2);
    assert_eq!(
        finished
            .iter()
            .map(CompilerInvocation::from_trace)
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([first, second])
    );
}

#[tokio::test]
async fn measured_trace_opt_in_separates_qualified_preparation_from_cell_requests() {
    let foreground = invocation(7, 1);
    let preparation = invocation(8, 1);
    let rows = [
        request_row(&preparation, "compiler request started", "preparation"),
        request_row(&foreground, "compiler request started", "foreground"),
        request_row(&preparation, "compiler request finished", "preparation"),
        request_row(&foreground, "compiler request finished", "foreground"),
    ];
    let (_file, mut trace) = trace_with_rows(&rows);
    let evidence = trace
        .completion_with_preparation(
            &BTreeSet::from([foreground.clone()]),
            &"a".repeat(64),
            &json!(123),
            true,
            Some(0),
            true,
        )
        .await;
    assert_eq!(
        evidence
            .foreground
            .iter()
            .map(CompilerInvocation::from_trace)
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([foreground])
    );
    assert_eq!(evidence.preparation_background.len(), 1);
    let background = &evidence.preparation_background[0];
    assert_eq!(background.invocation, preparation);
    assert_eq!(background.started["message"], "compiler request started");
    assert_eq!(background.finished["message"], "compiler request finished");
    assert_eq!(background.finished["worker_pid"], 456);
}

#[test]
fn preparation_refusal_controls_preserve_default_and_reject_foreign_classes() {
    let expected = invocation(7, 1);
    let preparation = invocation(8, 1);
    let foreground = invocation(9, 1);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    for (rows, allow_background) in [
        (
            vec![request_row(
                &preparation,
                "compiler request started",
                "preparation",
            )],
            false,
        ),
        (
            vec![request_row(
                &preparation,
                "compiler request started",
                "mystery",
            )],
            true,
        ),
        (
            vec![request_row(
                &foreground,
                "compiler request started",
                "foreground",
            )],
            true,
        ),
        (
            vec![
                request_row(&preparation, "compiler request started", "preparation"),
                {
                    let mut terminal =
                        request_row(&preparation, "compiler request finished", "preparation");
                    terminal["exit_code"] = json!(1);
                    terminal
                },
            ],
            true,
        ),
        (
            vec![request_row(
                &preparation,
                "compiler request abandoned by client",
                "preparation",
            )],
            true,
        ),
        (
            vec![request_row(
                &preparation,
                "compiler request failed",
                "preparation",
            )],
            true,
        ),
        (
            vec![
                request_row(&preparation, "compiler request started", "preparation"),
                {
                    let mut terminal =
                        request_row(&preparation, "compiler request finished", "preparation");
                    terminal["daemon_epoch"] = json!("b".repeat(64));
                    terminal
                },
            ],
            true,
        ),
        (
            vec![
                request_row(&preparation, "compiler request started", "preparation"),
                {
                    let mut terminal =
                        request_row(&preparation, "compiler request finished", "preparation");
                    terminal["compiler_jobs"] = json!(0);
                    terminal
                },
            ],
            true,
        ),
        (
            vec![
                request_row(&preparation, "compiler request started", "preparation"),
                {
                    let mut terminal =
                        request_row(&preparation, "compiler request finished", "preparation");
                    terminal["worker"] = json!(2);
                    terminal
                },
            ],
            true,
        ),
    ] {
        let (_file, mut trace) = trace_with_rows(&rows);
        let attempt = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            if allow_background {
                runtime.block_on(trace.completion_with_preparation(
                    &BTreeSet::from([expected.clone()]),
                    &"a".repeat(64),
                    &json!(123),
                    false,
                    Some(0),
                    true,
                ));
            } else {
                runtime.block_on(trace.completion(
                    &BTreeSet::from([expected.clone()]),
                    &"a".repeat(64),
                    &json!(123),
                    false,
                ));
            }
        }));
        assert!(attempt.is_err());
    }
}

#[test]
fn preparation_background_can_finish_in_a_later_cell_without_entering_foreground_history() {
    let expected = invocation(7, 1);
    let preparation = invocation(8, 1);
    let next_foreground = invocation(9, 1);
    let first_rows = [
        request_row(&expected, "compiler request started", "foreground"),
        request_row(&preparation, "compiler request started", "preparation"),
        request_row(&expected, "compiler request finished", "foreground"),
    ];
    let (file, mut trace) = trace_with_rows(&first_rows);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    let first = runtime.block_on(trace.completion_with_preparation(
        &BTreeSet::from([expected]),
        &"a".repeat(64),
        &json!(123),
        false,
        Some(0),
        true,
    ));
    assert!(first.preparation_background.is_empty());
    assert_eq!(trace.preparation_pending.len(), 1);
    let mut cold_background_terminal =
        request_row(&preparation, "compiler request finished", "preparation");
    cold_background_terminal["served"] = json!(0);
    append_rows(
        &file,
        &[
            cold_background_terminal,
            request_row(&next_foreground, "compiler request started", "foreground"),
            request_row(&next_foreground, "compiler request finished", "foreground"),
        ],
    );
    let second = runtime.block_on(trace.completion_with_preparation(
        &BTreeSet::from([next_foreground]),
        &"a".repeat(64),
        &json!(123),
        true,
        Some(1),
        true,
    ));
    assert_eq!(second.foreground.len(), 1);
    assert_eq!(second.preparation_background.len(), 1);
    assert_eq!(second.preparation_background[0].start_index, 0);
    assert_eq!(second.preparation_background[0].terminal_index, 1);
    assert_eq!(second.preparation_background[0].finished["served"], 0);
    assert!(trace.preparation_pending.is_empty());
    assert!(runtime
        .block_on(trace.final_preparation_drain(&"a".repeat(64), &json!(123), 1))
        .is_empty());
}

#[test]
fn final_preparation_drain_rejects_a_missing_terminal() {
    let expected = invocation(7, 1);
    let preparation = invocation(8, 1);
    let rows = [
        request_row(&expected, "compiler request started", "foreground"),
        request_row(&preparation, "compiler request started", "preparation"),
        request_row(&expected, "compiler request finished", "foreground"),
    ];
    let (_file, mut trace) = trace_with_rows(&rows);
    trace.completion_timeout = std::time::Duration::from_millis(100);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    runtime.block_on(trace.completion_with_preparation(
        &BTreeSet::from([expected]),
        &"a".repeat(64),
        &json!(123),
        false,
        Some(0),
        true,
    ));
    let attempt = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        runtime.block_on(trace.final_preparation_drain(&"a".repeat(64), &json!(123), 1))
    }));
    assert!(attempt.is_err());
}

#[test]
fn fixed_startup_compilations_are_validated_then_discarded() {
    let startup = invocation(1, 1);
    let rows = [
        request_row(&startup, "compiler request started", "preparation"),
        request_row(&startup, "compiler request finished", "preparation"),
    ];
    let (_file, mut trace) = trace_with_rows(&rows);
    trace.completion_timeout = std::time::Duration::from_millis(100);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    runtime.block_on(trace.startup_drain(&"a".repeat(64), &json!(123)));
    assert!(trace.preparation_pending.is_empty());
}

#[test]
fn preparation_background_observations_have_a_hard_bound() {
    let expected = invocation(1, 1);
    let mut rows = vec![request_row(
        &expected,
        "compiler request started",
        "foreground",
    )];
    for admission in 2..=MAX_PENDING_PREPARATION as u64 + 2 {
        rows.push(request_row(
            &invocation(admission, 1),
            "compiler request started",
            "preparation",
        ));
    }
    rows.push(request_row(
        &expected,
        "compiler request finished",
        "foreground",
    ));
    let (_file, mut trace) = trace_with_rows(&rows);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    let attempt = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        runtime.block_on(trace.completion_with_preparation(
            &BTreeSet::from([expected]),
            &"a".repeat(64),
            &json!(123),
            false,
            Some(0),
            true,
        ));
    }));
    assert!(attempt.is_err());
}
