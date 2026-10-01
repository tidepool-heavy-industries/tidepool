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
        let mut fields = serde_json::to_value(request).unwrap();
        fields["message"] = json!(message);
        fields["daemon_pid"] = json!(123);
        fields["transport"] = json!("daemon");
        fields["exit_code"] = json!(0);
        fields["worker_pid"] = json!(456);
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
