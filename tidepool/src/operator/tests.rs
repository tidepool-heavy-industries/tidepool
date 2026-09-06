use super::*;
use tidepool_actor::{ActorWorkbenchSource, EffectiveRole, Incarnation, ResidentForest};
use tidepool_repr::{CoreFrame, DataConTable, Literal, SessionId, TreeBuilder};
use tidepool_runtime::session::{
    insert_preamble_imports, ModuleEnv, OutputSink, ResidentSession, SessionLib,
};

#[derive(Clone)]
struct Sink;
impl OutputSink for Sink {
    fn drain(&self) -> Vec<String> {
        Vec::new()
    }
    fn snapshot(&self) -> Vec<String> {
        Vec::new()
    }
}

#[tokio::test]
async fn unix_http_live_workbench_and_graph() {
    tidepool_testing::eval_harness::require_extract();
    let directory = tempfile::tempdir().unwrap();
    let session = SessionId(984521);
    let mut include = vec![
        tidepool_testing::eval_harness::prelude_path(),
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../haskell/actors"),
        crate::haskell_sources::ensure_stdlib().unwrap(),
    ];
    let declarations = crate::actor_host::shoal_effect_declarations();
    let effects = tidepool_mcp::ensure_effects_module(&declarations).unwrap();
    include.extend(effects.include_paths());
    let preamble = insert_preamble_imports(
        &tidepool_mcp::build_preamble(&[], false),
        "Tidepool.Actors.Shoal",
    );
    let lib = SessionLib::open(session, directory.path(), ModuleEnv::standalone_default())
        .unwrap()
        .with_validation_include(include.clone());
    let mut tree = TreeBuilder::new();
    tree.push(CoreFrame::Lit(Literal::LitInt(0)));
    let machine = ResidentSession::bootstrap(
        &tree.build(),
        DataConTable::new(),
        frunk::HNil,
        Sink,
        include.clone(),
        tidepool_runtime::DEFAULT_NURSERY_SIZE,
        Some(lib),
    )
    .unwrap();
    let (forest, _deployments) = ResidentForest::new(
        ActorWorkbenchSource::new(preamble, include),
        session,
        machine,
        None,
        Incarnation::FIRST,
    );
    let forest = Arc::new(forest);
    let provision = forest.clone();
    let inspection = forest.clone();
    let socket = directory.path().join("http/operator.sock");
    let service = OperatorService::bind(
        socket.clone(),
        Arc::new(move || {
            let forest = provision.clone();
            Box::pin(async move {
                forest
                    .new_workbench("operator".into(), EffectiveRole::root())
                    .await
                    .map_err(|e| e.to_string())
            })
        }),
        Arc::new(move |actor| inspection.inspect_graph(actor)),
    )
    .await
    .unwrap();
    let client = reqwest::Client::builder()
        .unix_socket(socket.as_path())
        .build()
        .unwrap();
    let base = "http://localhost";
    let first: Attachment = client
        .post(format!("{base}/host/operators"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let second: Attachment = client
        .post(format!("{base}/host/operators"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let url = format!("{base}/v1/sessions/{}", first.session);
    let info: SessionInfo = client.get(&url).send().await.unwrap().json().await.unwrap();
    assert_eq!(info.session, first.session);
    assert_eq!(info.protocol_version, 1);
    let submit = |source: &str| {
        client.post(format!("{url}/submit")).json(&SubmitRequest {
            source: source.into(),
        })
    };
    let result: SubmitResponse = submit("let greeting = \"λ hello\"\ngreeting")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(result.outcome, Outcome::Completed, "{result:?}");
    assert!(format!("{result:?}").contains("hello"));
    let retained: SubmitResponse = submit("greeting")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(retained.outcome, Outcome::Completed);
    let isolated: SubmitResponse = client
        .post(format!("{base}/v1/sessions/{}/submit", second.session))
        .json(&SubmitRequest {
            source: "greeting".into(),
        })
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(isolated.outcome, Outcome::Rejected, "{isolated:?}");
    let rejected: SubmitResponse =
        submit("let committedPrefix = 42\nmissingName\nlet neverRuns = 99")
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
    assert_eq!(rejected.outcome, Outcome::Rejected);
    let prefix: SubmitResponse = submit("committedPrefix")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(prefix.outcome, Outcome::Completed);
    let diagnostic: SubmitResponse = submit(":type absentDiagnosticName")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        diagnostic
            .blocks
            .iter()
            .any(|block| matches!(block, Block::Diagnostic(_))),
        "{diagnostic:?}"
    );
    let after_diagnostic: SubmitResponse = submit("committedPrefix")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(after_diagnostic.outcome, Outcome::Completed);
    let graph: serde_json::Value = client
        .get(format!("{url}/actors"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(graph["actors"].as_array().unwrap().len(), 2);
    assert!(graph["actors"]
        .as_array()
        .unwrap()
        .iter()
        .all(|n| n["supervisor_parent"].is_null() && n["provider_thread"].is_null()));
    let status: SubmitResponse = submit(":status\n:lineage")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(status.outcome, Outcome::Completed, "{status:?}");
    // The opaque segment is decoded exactly once, even with slash, Unicode, and percent text.
    let opaque = "query/λ%2F";
    let first_actor = service
        .state
        .sessions
        .lock()
        .await
        .get(&first.session)
        .unwrap()
        .clone();
    service
        .state
        .sessions
        .lock()
        .await
        .insert(opaque.into(), first_actor);
    let mut opaque_url = reqwest::Url::parse(&format!("{base}/v1/sessions/")).unwrap();
    opaque_url
        .path_segments_mut()
        .unwrap()
        .pop_if_empty()
        .push(opaque);
    let opaque_info: SessionInfo = client
        .get(opaque_url)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(opaque_info.session, opaque);
    service.state.sessions.lock().await.remove(opaque);
    let malformed = client
        .get(format!("{base}/v1/sessions/%FF"))
        .send()
        .await
        .unwrap();
    assert_eq!(malformed.status(), StatusCode::BAD_REQUEST);
    let _: ApiError = malformed.json().await.unwrap();
    // Sending a complete request and closing the socket cannot withdraw the actor's work.
    use tokio::io::AsyncWriteExt;
    let mut connection = tokio::net::UnixStream::connect(&socket).await.unwrap();
    let body = serde_json::to_string(&SubmitRequest {
        source: "afterDisconnect <- pure (123 :: Int)".into(),
    })
    .unwrap();
    let request = format!("POST /v1/sessions/{}/submit HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}", first.session, body.len(), body);
    connection.write_all(request.as_bytes()).await.unwrap();
    // Observe the admitted unit through the independent graph route, then detach.
    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        loop {
            let graph: serde_json::Value = client
                .get(format!("{url}/actors"))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            if graph["actors"]
                .as_array()
                .unwrap()
                .iter()
                .any(|node| node["workbench"]["kind"] == "running_unit")
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("observe admitted execution before disconnect");
    drop(connection);
    let disconnected: SubmitResponse = submit("afterDisconnect")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(disconnected.outcome, Outcome::Completed, "{disconnected:?}");
    if let Some(probe) = std::env::var_os("SHOAL_OPERATOR_CLIENT_PROBE") {
        let status = tokio::process::Command::new(probe)
            .arg(&socket)
            .arg(&first.session)
            .status()
            .await
            .unwrap();
        assert!(status.success(), "external client probe failed");
    }
    let (a, b) = tokio::join!(
        submit("let concurrentA = 1").send(),
        submit("let concurrentB = 2").send()
    );
    assert_eq!(a.unwrap().status(), StatusCode::OK);
    assert_eq!(b.unwrap().status(), StatusCode::OK);
    let large = client
        .post(format!("{url}/submit"))
        .body(vec![b'x'; MAX_REQUEST_BYTES + 1])
        .send()
        .await
        .unwrap();
    assert_eq!(large.status(), StatusCode::PAYLOAD_TOO_LARGE);
    let _: ApiError = large.json().await.unwrap();
    assert_eq!(
        client
            .post(format!("{base}/host/operators/{}/stop", first.session))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        client.get(&url).send().await.unwrap().status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        submit("1").send().await.unwrap().status(),
        StatusCode::NOT_FOUND
    );
    let sibling: SubmitResponse = client
        .post(format!("{base}/v1/sessions/{}/submit", second.session))
        .json(&SubmitRequest {
            source: "40 + 2".into(),
        })
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(sibling.outcome, Outcome::Completed);
    service.shutdown().await;
    forest.shutdown().await;
}

#[tokio::test]
async fn wire_terminal_mapping_and_response_bounds() {
    for status in [
        WorkbenchRunStatus::Committed,
        WorkbenchRunStatus::Completed,
        WorkbenchRunStatus::Replied,
        WorkbenchRunStatus::RequestCancelled,
        WorkbenchRunStatus::Rejected,
    ] {
        let result = map_result(WorkbenchResponse {
            status,
            items: vec![],
            next_index: 0,
            total: 0,
        });
        assert_eq!(
            result.outcome,
            if status == WorkbenchRunStatus::Rejected {
                Outcome::Rejected
            } else {
                Outcome::Completed
            }
        );
        assert_eq!(
            result
                .receipt
                .as_ref()
                .unwrap()
                .structured
                .as_ref()
                .unwrap()["status"],
            serde_json::to_value(status).unwrap()
        );
    }
    let result = SubmitResponse {
        blocks: vec![Block::Output("λ".repeat(MAX_RESPONSE_BYTES))],
        outcome: Outcome::Completed,
        receipt: Some(Receipt {
            display: "retained".into(),
            structured: None,
        }),
    };
    let response = bounded_response(result);
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), MAX_RESPONSE_BYTES)
        .await
        .unwrap();
    let result: SubmitResponse = serde_json::from_slice(&bytes).unwrap();
    assert!(matches!(&result.blocks[0], Block::Diagnostic(text) if text.contains("truncated")));
    assert_eq!(result.receipt.unwrap().display, "retained");
    let response = bounded_response(SubmitResponse {
        blocks: vec![],
        outcome: Outcome::Completed,
        receipt: Some(Receipt {
            display: "x".repeat(MAX_RESPONSE_BYTES),
            structured: None,
        }),
    });
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
}
