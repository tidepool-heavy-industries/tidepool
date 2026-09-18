use super::test_campaign::TestCampaign;
use super::tests::dispatch_haskell_script;
use super::*;
use tidepool_actor::{JevBackend, JevCallFailure};

/// Answers every request with one choice answer and records the requests.
struct FakeJev {
    requests: Mutex<Vec<serde_json::Value>>,
    answer: Result<String, JevCallFailure>,
}

impl JevBackend for FakeJev {
    fn ask(
        &self,
        request: String,
    ) -> futures_util::future::BoxFuture<'_, Result<String, JevCallFailure>> {
        self.requests
            .lock()
            .push(serde_json::from_str(&request).expect("request is JSON"));
        let answer = self.answer.clone();
        Box::pin(async move { answer })
    }
}

async fn campaign_with(backend: Arc<FakeJev>) -> TestCampaign {
    TestCampaign::start_with_config(
        tidepool_actor::ResearchPolicy::default(),
        |admission| admission,
        |config| config.jev = Some(backend),
    )
    .await
}

/// No `LANGUAGE` pragma: `OverloadedLabels` is in the cell dialect
/// (`session::dialect::EVAL_PRAGMAS`), so `#not_here` needs no ceremony.
const CELL: &str = r#"answer <- J.ask1 (J.state (String "retry loop in fetch; timeout branch at line 12"))
  (J.choice "Which line begins the retry-timeout branch?"
     (J.alt #not_here "The branch is not in this file" (0 :: Int)
        J..| J.many [("line-4", "if attempts > 3", 4), ("line-12", "if elapsed > timeout", 12)]))
either (const 0) (\a -> J.handle (J.chosen a) (#not_here id J..| J.onMany (\_ n -> n))) answer"#;

#[tokio::test]
async fn jev_choice_round_trips_through_the_host_backend() {
    let backend = Arc::new(FakeJev {
        requests: Mutex::new(Vec::new()),
        answer: Ok(serde_json::json!({
            "model": "jev-1.13.0",
            "answers": {"value": {
                "type": "choice",
                "choice": "line-12",
                "probabilities": {"not_here": 0.02, "line-4": 0.08, "line-12": 0.9},
                "confidence": 0.9
            }},
            "usage": {}
        })
        .to_string()),
    });
    let campaign = campaign_with(Arc::clone(&backend)).await;
    let result = dispatch_haskell_script(campaign.root_installation.policy.as_ref(), CELL).await;
    assert_eq!(result["status"], "committed", "{result}");
    let items = result["items"].as_array().unwrap();
    assert_eq!(items.last().unwrap()["output"], "12", "{result}");
    let requests = backend.requests.lock();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!(request["model"], "jev-latest", "{request}");
    assert_eq!(request["questions"]["value"]["type"], "choice", "{request}");
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

#[tokio::test]
async fn jev_call_failure_is_a_typed_left() {
    let backend = Arc::new(FakeJev {
        requests: Mutex::new(Vec::new()),
        answer: Err(JevCallFailure::Unconfigured),
    });
    let campaign = campaign_with(backend).await;
    let result = dispatch_haskell_script(
        campaign.root_installation.policy.as_ref(),
        &CELL.replace(
            "either (const 0) (\\a -> J.handle (J.chosen a) (#not_here id J..| J.onMany (\\_ n -> n))) answer",
            "either (const \"failed\") (const \"answered\") answer :: Text",
        ),
    )
    .await;
    assert_eq!(result["status"], "committed", "{result}");
    let items = result["items"].as_array().unwrap();
    assert_eq!(items.last().unwrap()["output"], "failed", "{result}");
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

/// Offers and packets bound in one statement are retained for later ones,
/// and the packet operators read unqualified.
#[tokio::test]
async fn retained_packet_bindings_reach_a_later_statement() {
    let backend = Arc::new(FakeJev {
        requests: Mutex::new(Vec::new()),
        answer: Ok(serde_json::json!({
            "model": "jev-1.13.0",
            "answers": {
                "place": {"type": "choice", "choice": "line_12",
                          "probabilities": {"line_4": 0.1, "line_12": 0.9}, "confidence": 0.9},
                "enough": {"type": "noul", "noul": 0.8}
            },
            "usage": {}
        })
        .to_string()),
    });
    let campaign = campaign_with(Arc::clone(&backend)).await;
    let result = dispatch_haskell_script(
        campaign.root_installation.policy.as_ref(),
        // No `LANGUAGE` pragma. This is the load-bearing case: the packet
        // needs `OverloadedLabels` and `(J.answers r).place` needs
        // `OverloadedRecordDot`, and the latter is deliberately absent from
        // `DECL_TEMPLATE_SOURCE`, the parse-only template GHC uses to pick a
        // cell item's shape. If template selection ever starts needing it,
        // this test is where that shows up.
        r#"let offers = J.alt #line_4 "if attempts > 3" (4 :: Int) J..| J.alt #line_12 "if elapsed > timeout" 12
let packet = #place := J.choice "Which line begins the retry-timeout branch?" offers :& #enough := J.noul "Is the branch visible?" :& J.Nil
answer <- J.ask (J.state (String "retry loop in fetch")) packet
either (const 0) (\r -> J.handle (J.chosen (J.answers r).place) (#line_4 id J..| #line_12 id)) answer"#,
    )
    .await;
    assert_eq!(result["status"], "committed", "{result}");
    let items = result["items"].as_array().unwrap();
    assert_eq!(items.last().unwrap()["output"], "12", "{result}");
    assert_eq!(backend.requests.lock().len(), 1);
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

/// The pooled packet in `doc jev` compiles, runs, and sends one request.
#[tokio::test]
async fn doc_jev_pool_example_sends_one_request() {
    let doc = include_str!("../../../prompts/shoal/docs/jev.md");
    let section = doc
        .split("## A packet with a pool")
        .nth(1)
        .expect("doc jev has the pool section");
    let cell = section
        .split("```haskell\n")
        .nth(1)
        .and_then(|rest| rest.split("```").next())
        .expect("the pool section has a Haskell cell");
    let backend = Arc::new(FakeJev {
        requests: Mutex::new(Vec::new()),
        answer: Err(JevCallFailure::Unconfigured),
    });
    let campaign = campaign_with(Arc::clone(&backend)).await;
    let result = dispatch_haskell_script(campaign.root_installation.policy.as_ref(), cell).await;
    assert_eq!(result["status"], "committed", "{result}");
    let output = result["items"].as_array().unwrap().last().unwrap()["output"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(output.contains("no Jev endpoint is configured"), "{result}");
    let requests = backend.requests.lock();
    assert_eq!(requests.len(), 1);
    let questions = &requests[0]["questions"];
    for key in ["best", "fixed"] {
        assert!(questions.get(key).is_some(), "{key} missing: {questions}");
    }
    drop(requests);
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

/// The Jev-dense cell in `doc jev` judges every file of a bound preview list
/// in one packet. The shell cell before it is not run here: the test campaign
/// has no command backend, so the previews are bound directly.
#[tokio::test]
async fn doc_jev_dense_example_sends_one_request() {
    let doc = include_str!("../../../prompts/shoal/docs/jev.md");
    let section = doc
        .split("## A Jev-dense cell")
        .nth(1)
        .expect("doc jev has the dense-cell section");
    let cell = section
        .split("```haskell\n")
        .nth(2)
        .and_then(|rest| rest.split("```").next())
        .expect("the dense-cell section has a second Haskell cell");
    // The cell leads with nothing to hoist past — the dialect supplies its
    // extensions — so the bound previews simply go first. Prepending after a
    // presumed pragma line instead would shift `let files` above this binding,
    // where `previews` resolves to Control.Lens's, not the list.
    let cell = format!(
        "let previews = [(\"README.md\", \"# jev-dsl\\ntyped packets\"), (\"LICENSE\", \"MIT\")] :: [(Text, Text)]\n{cell}"
    );
    let backend = Arc::new(FakeJev {
        requests: Mutex::new(Vec::new()),
        answer: Err(JevCallFailure::Unconfigured),
    });
    let campaign = campaign_with(Arc::clone(&backend)).await;
    let result = dispatch_haskell_script(campaign.root_installation.policy.as_ref(), &cell).await;
    assert_eq!(result["status"], "committed", "{result}");
    let output = result["items"].as_array().unwrap().last().unwrap()["output"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(output.contains("no Jev endpoint is configured"), "{result}");
    let requests = backend.requests.lock();
    assert_eq!(requests.len(), 1);
    let questions = &requests[0]["questions"];
    // `enough` is a top-level cell; `worth_reading` is an `eachIn` over the
    // pool, so each member flattens to a dotted wire key.
    assert!(questions.get("enough").is_some(), "enough missing: {questions}");
    let per_file = questions
        .as_object()
        .unwrap()
        .keys()
        .filter(|k| k.starts_with("worth_reading."))
        .count();
    assert_eq!(per_file, 2, "one question per previewed file: {questions}");
    drop(requests);
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

/// Haskell cell -> host effect -> live TypeSafe API -> typed answer. Opt-in:
/// `TYPESAFE_API_KEY` must be set; run with `--ignored`.
#[tokio::test]
#[ignore = "calls the live TypeSafe API; needs TYPESAFE_API_KEY"]
async fn live_jev_from_a_haskell_cell() {
    assert!(
        std::env::var("TYPESAFE_API_KEY").is_ok_and(|key| !key.is_empty()),
        "TYPESAFE_API_KEY is not set"
    );
    let campaign = TestCampaign::start_with_config(
        tidepool_actor::ResearchPolicy::default(),
        |admission| admission,
        |config| config.jev = None,
    )
    .await;
    let result = dispatch_haskell_script(
        campaign.root_installation.policy.as_ref(),
        r#"answer <- J.ask1 (J.state (String "A cat is sitting on a warm windowsill in the sun."))
  (J.choice "Where is the cat?"
     (J.alt #windowsill "On a windowsill" (1 :: Int)
        J..| J.alt #roof "On a roof" 2
        J..| J.alt #bed "In a bed" 3))
either (const 0) (\a -> J.handle (J.chosen a) (#windowsill id J..| #roof id J..| #bed id)) answer"#,
    )
    .await;
    assert_eq!(result["status"], "committed", "{result}");
    let items = result["items"].as_array().unwrap();
    assert_eq!(items.last().unwrap()["output"], "1", "{result}");
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}
