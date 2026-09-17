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

const CELL: &str = r#"{-# LANGUAGE OverloadedLabels #-}
answer <- J.ask1 (J.state (String "retry loop in fetch; timeout branch at line 12"))
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
        r#"{-# LANGUAGE OverloadedLabels, OverloadedRecordDot #-}
let offers = J.alt #line_4 "if attempts > 3" (4 :: Int) J..| J.alt #line_12 "if elapsed > timeout" 12
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
        r#"{-# LANGUAGE OverloadedLabels #-}
answer <- J.ask1 (J.state (String "A cat is sitting on a warm windowsill in the sun."))
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
