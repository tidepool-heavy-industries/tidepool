//! Execute the installed composition through the native JSON and Jev effects.
use super::*;

#[derive(Default)]
struct BatchBackend(Mutex<Vec<serde_json::Value>>);

impl JevBackend for BatchBackend {
    fn ask(
        &self,
        request: String,
    ) -> futures_util::future::BoxFuture<'_, Result<String, JevCallFailure>> {
        let request: serde_json::Value = serde_json::from_str(&request).unwrap();
        assert_eq!(request["model"], "jev-latest");
        let questions = request["questions"].as_object().unwrap();
        assert_eq!(
            questions.len(),
            2,
            "equal evidence batches independent questions"
        );
        let answers: serde_json::Map<String, serde_json::Value> = questions.iter().map(|(key, question)| {
            let answer = match question["type"].as_str().unwrap() {
                "choice" => {
                    let criteria = question["criteria"].as_object().unwrap();
                    let winner = criteria.keys().find(|key| key.contains("call")).unwrap();
                    let masses: serde_json::Map<String, serde_json::Value> = criteria.keys().map(|key| {
                        let mass = if key == winner { 0.7 } else if key.contains("other") { 0.0 }
                            else if key == "insufficient" { 0.2 } else { 0.1 };
                        (key.clone(), serde_json::json!(mass))
                    }).collect();
                    serde_json::json!({"type":"choice","choice":winner,"confidence":0.8,"probabilities":masses})
                }
                "noul" => serde_json::json!({"type":"noul","noul":0.9}),
                other => panic!("unexpected installed example question {other}"),
            };
            (key.clone(), answer)
        }).collect();
        self.0.lock().push(request);
        let response = serde_json::json!({"answers":answers,"model":"composed-fixture","usage":{"input_tokens":0,"output_tokens":0}}).to_string();
        Box::pin(async move { Ok(response) })
    }
}

#[tokio::test]
async fn installed_composition_retains_full_distribution_replays_and_runs_only_explicit_action() {
    let mut fixture = Fixture::new(782, false).await;
    let backend = Arc::new(BatchBackend::default());
    fixture.forest.set_jev_backend(backend.clone());
    fixture
        .execute(&tidepool_testing::fixture_source(
            "exomonad/actor/src/resident_actor/jev_form_runtime_tests/composed.hs",
        ))
        .await;
    assert_eq!(
        backend.0.lock().len(),
        1,
        "reconstructing/replaying sends no provider calls"
    );
    {
        let state = fixture.host.0.lock();
        assert_eq!(
            state.views.len(),
            1,
            "only explicit runSelection executes the selected action"
        );
        assert!(state.views[0].to_string().contains("replacement-original"));
    }
    fixture.finish().await;
}

#[tokio::test]
async fn installed_batch_notebook_example_executes_against_mocked_provider() {
    let mut fixture = Fixture::new(783, false).await;
    let backend = Arc::new(BatchBackend::default());
    fixture.forest.set_jev_backend(backend.clone());
    let workspace =
        std::env::var_os("TIDEPOOL_JEV_WORKSPACE_DIR").expect("declared workspace input");
    let source =
        std::fs::read_to_string(PathBuf::from(workspace).join("checks/jev-batch-example.hs"))
            .unwrap();
    fixture.execute(&format!("{source}\nsay \"True\"\n")).await;
    assert_eq!(backend.0.lock().len(), 1);
    fixture.finish().await;
}

#[derive(Default)]
struct UnavailableBackend(Mutex<usize>);

impl JevBackend for UnavailableBackend {
    fn ask(
        &self,
        request: String,
    ) -> futures_util::future::BoxFuture<'_, Result<String, JevCallFailure>> {
        let request: serde_json::Value = serde_json::from_str(&request).unwrap();
        let state = &request["state"];
        assert_eq!(state["primary_issue"], "fixture primary unavailable");
        assert_eq!(state["selection_status"], "primary unavailable");
        assert_eq!(
            state["primary_receipt_status"]["disposition"],
            "not executed"
        );
        assert!(
            state["declarations"]
                .as_str()
                .unwrap()
                .contains("fixture primary unavailable")
        );
        assert_eq!(state["capture_report_truncated"], false);
        assert_eq!(state["declarations_truncated"], false);
        let questions = request["questions"].as_object().unwrap();
        assert_eq!(questions.len(), 1);
        let question = questions.keys().next().unwrap();
        *self.0.lock() += 1;
        let response = serde_json::json!({"answers":{question:{"type":"noul","noul":0.1}},"model":"insufficient-fixture"}).to_string();
        Box::pin(async move { Ok(response) })
    }
}

#[tokio::test]
async fn installed_lookup_reassessment_retains_unavailable_primary_and_insufficient_result() {
    let mut fixture = Fixture::new(784, false).await;
    let backend = Arc::new(UnavailableBackend::default());
    fixture.forest.set_jev_backend(backend.clone());
    fixture
        .execute(&tidepool_testing::fixture_source(
            "exomonad/actor/src/resident_actor/jev_form_runtime_tests/composed_lookup.hs",
        ))
        .await;
    assert_eq!(
        *backend.0.lock(),
        1,
        "unavailable primary skips ranking but reassesses once"
    );
    fixture.finish().await;
}
