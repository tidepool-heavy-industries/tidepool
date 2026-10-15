//! Recordings survive a real source publication while executable payloads
//! are reconstructed from the newly published module.
use super::test_campaign::{self, TestCampaign};
use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

const ARCHIVE: &str = ".exomonad/state/jev-observation.json";

#[derive(Default)]
struct RecordingBackend {
    calls: AtomicUsize,
}

impl exomonad_actor::JevBackend for RecordingBackend {
    fn ask(
        &self,
        body: String,
    ) -> futures_util::future::BoxFuture<'_, Result<String, exomonad_actor::JevCallFailure>> {
        let request: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(request["model"], "jev-latest");
        assert_eq!(
            request["state"],
            serde_json::json!({"evidence": "unchanged evidence"})
        );
        let questions = request["questions"].as_object().unwrap();
        assert_eq!(questions.len(), 1);
        let (key, question) = questions.iter().next().unwrap();
        assert_eq!(question["type"], "choice");
        self.calls.fetch_add(1, Ordering::SeqCst);
        let answers = serde_json::Map::from_iter([(
            key.clone(),
            serde_json::json!({
                "type": "choice", "choice": "add", "confidence": 0.99,
                "probabilities": {"add": 0.9, "subtract": 0.1}
            }),
        )]);
        let response = serde_json::json!({
            "answers": answers, "model": "recording-reload-fixture",
            "usage": {"input_tokens": 7, "output_tokens": 3}
        });
        Box::pin(async move { Ok(response.to_string()) })
    }
}

async fn displayed(
    campaign: &mut TestCampaign,
    endpoint: &dyn exomonad_actor::ResidentToolEndpoint,
    source: &str,
) -> String {
    let store = super::display_output::open_run_store(campaign.session_root.path()).unwrap();
    let origin = test_campaign::actor_output_origin(
        &super::runtime_namespace(campaign.session_root.path()),
        campaign.actor.identity(),
    );
    let mut receipt = campaign
        .drive_actor_output(&store, async {
            let result =
                test_campaign::dispatch_haskell_script_named(endpoint, "haskell_sync", source)
                    .await;
            assert_eq!(result["status"], "committed", "{result}");
            result
        })
        .await;
    // Bound failure observation independently of the model's page budget.
    // Successful values fit one page; diagnostics retain each page verbatim.
    const MAX_PAGES: usize = 64;
    const MAX_BYTES: usize = 512 * 1024;
    let mut text = String::new();
    let mut identity = None;
    let mut consumed = std::collections::HashSet::new();
    for page_number in 1..=MAX_PAGES {
        let page = test_campaign::retained_committed_display_text(&store, &origin, &receipt);
        let display = &test_campaign::explicit_display_operation(&receipt)["display"];
        let current: (i64, i64, i64) = serde_json::from_value(display["identity"].clone()).unwrap();
        if let Some(expected) = identity {
            assert_eq!(
                current, expected,
                "diagnostic continuation changed display identity"
            );
        } else {
            identity = Some(current);
        }
        let keys = display["expansions"].as_array().unwrap();
        assert_eq!(
            display["unavailable"],
            false,
            "diagnostic observation incomplete: retained detail unavailable after {page_number} pages/{} bytes; partial: {text}; page: {page}",
            text.len()
        );
        assert!(
            keys.len() <= 1,
            "diagnostic observation incomplete: expected a scalar suffix, got {} branches after {page_number} pages/{} bytes; partial: {text}; page: {page}",
            keys.len(),
            text.len()
        );
        let separator = usize::from(page_number > 1);
        let observed_bytes = text
            .len()
            .saturating_add(separator)
            .saturating_add(page.len());
        assert!(
            observed_bytes <= MAX_BYTES,
            "diagnostic observation incomplete: {observed_bytes} bytes exceeds {MAX_BYTES}-byte bound after {page_number} pages; partial: {text}"
        );
        if page_number > 1 {
            text.push('\n');
        }
        text.push_str(&page);
        let Some(key) = keys.first() else {
            return text;
        };
        let key = key[0].as_i64().unwrap();
        assert!(
            consumed.insert(key),
            "diagnostic observation incomplete: consumed key {key} reappeared after {page_number} pages/{} bytes; partial: {text}",
            text.len()
        );
        assert!(
            page_number < MAX_PAGES,
            "diagnostic observation incomplete: {page_number} pages reached {MAX_PAGES}-page bound at {} bytes; partial: {text}",
            text.len()
        );
        receipt = campaign.drive_actor_output(&store, endpoint.expand_display_boxed(current, key)).await
            .unwrap_or_else(|error| panic!("diagnostic observation incomplete: retained expansion failed after {page_number} pages/{} bytes: {error}; partial: {text}", text.len()));
    }
    unreachable!("bounded scalar observation returns or reports its limit")
}

#[tokio::test]
async fn saved_jev_observation_replays_after_reload_with_new_payload_and_no_provider_call() {
    let probe = tidepool_testing::fixture_source(
        "bridge/facade/src/actor_host/fixtures/jev-recording-reload.hs",
    );
    let backend = Arc::new(RecordingBackend::default());
    let owned_backend = Arc::clone(&backend);
    let campaign = TestCampaign::start_with_conversation(
        |admission| admission,
        |config| {
            config.jev = Some(owned_backend as exomonad_actor::JevBackendHandle);
            let authored = config.workspace.join(".exomonad");
            std::fs::create_dir_all(authored.join("Project")).unwrap();
            std::fs::create_dir_all(authored.join("state")).unwrap();
            std::fs::write(authored.join("Project/RecordingReloadProbe.hs"), &probe).unwrap();
            test_campaign::pinned_jev_workspace_with(config, |project| {
                project.haskell.source_roots.insert(0, ".".into());
                project.haskell.modules = vec!["Project.RecordingReloadProbe".into()];
            });
        },
        Some(Arc::new(|_, _| {
            Box::pin(async { Err(exomonad_actor::ConversationUnavailable::Unbound) })
        })),
    )
    .await;
    campaign.run_scenario(|campaign| Box::pin(async move {
        let endpoint = Arc::clone(&campaign.root_installation.policy);
        let first = displayed(campaign, endpoint.as_ref(),
            "import qualified Project.RecordingReloadProbe as Probe\nobserved <- Probe.observeAndSave \".exomonad/state/jev-observation.json\"\ndisplay (show observed)").await;
        assert_eq!(first, "Right 12");
        assert_eq!(backend.calls.load(Ordering::SeqCst), 1);
        let path = campaign._repository.path().join(ARCHIVE);
        let original_archive = std::fs::read(&path).unwrap();
        let decoded: serde_json::Value = serde_json::from_slice(&original_archive).unwrap();
        assert_eq!(decoded["version"], 1);
        assert_eq!(decoded["records"].as_array().unwrap().len(), 1);

        let edited = probe.replace("payloadOffset = 7", "payloadOffset = 31");
        assert_ne!(edited, probe);
        std::fs::write(campaign._repository.path().join(".exomonad/Project/RecordingReloadProbe.hs"), edited).unwrap();
        let published = displayed(campaign, endpoint.as_ref(),
            "outcome <- reloadSource []\ndisplay (case outcome of { Right (ReloadPublished before after changed _) | revisionIdentity before /= revisionIdentity after && \"Project.RecordingReloadProbe\" `elem` changed -> \"published\"; _ -> show outcome })").await;
        assert_eq!(published, "published", "reload must publish the edited module: {published}");
        let replayed = displayed(campaign, endpoint.as_ref(),
            "import qualified Project.RecordingReloadProbe as Probe\nreplayed <- Probe.replaySaved \".exomonad/state/jev-observation.json\"\ndisplay (show replayed)").await;
        assert_eq!(replayed, "Right 36", "new source supplies the payload; archive supplies only the observation");
        let refused = displayed(campaign, endpoint.as_ref(),
            "import qualified Project.RecordingReloadProbe as Probe\nrefused <- Probe.refusesChangedEvidence \".exomonad/state/jev-observation.json\"\ndisplay (show refused)").await;
        assert_eq!(refused, "Right True");
        assert_eq!(backend.calls.load(Ordering::SeqCst), 1, "replay and refusal never fall back to the provider");
        assert_eq!(std::fs::read(path).unwrap(), original_archive, "reload and replay do not overwrite retained evidence");
    })).await;
}

#[derive(Default)]
struct BatchBackend {
    calls: AtomicUsize,
}

impl exomonad_actor::JevBackend for BatchBackend {
    fn ask(
        &self,
        body: String,
    ) -> futures_util::future::BoxFuture<'_, Result<String, exomonad_actor::JevCallFailure>> {
        let request: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(request["model"], "jev-latest");
        assert_eq!(request["state"]["intent"], "repair the E0061 call");
        let questions = request["questions"].as_object().unwrap();
        assert_eq!(
            questions.len(),
            2,
            "independent questions share one request"
        );
        let answers: serde_json::Map<String, serde_json::Value> = questions
            .iter()
            .map(|(key, question)| {
                let answer = match question["type"].as_str().unwrap() {
                    "choice" => {
                        let criteria = question["criteria"].as_object().unwrap();
                        assert_eq!(criteria.len(), 4);
                        let winner = criteria.keys().find(|key| key.contains("call")).unwrap();
                        let masses: serde_json::Map<String, serde_json::Value> = criteria
                            .keys()
                            .map(|key| {
                                let mass = if key == winner {
                                    0.7
                                } else if key.contains("other") {
                                    0.0
                                } else if key == "insufficient" {
                                    0.2
                                } else {
                                    0.1
                                };
                                (key.clone(), serde_json::json!(mass))
                            })
                            .collect();
                        serde_json::json!({"type":"choice","choice":winner,"confidence":0.8,"probabilities":masses})
                    }
                    "noul" => serde_json::json!({"type":"noul","noul":0.9}),
                    other => panic!("unexpected installed example question {other}"),
                };
                (key.clone(), answer)
            })
            .collect();
        self.calls.fetch_add(1, Ordering::SeqCst);
        let response = serde_json::json!({
            "answers": answers, "model": "installed-batch-fixture",
            "usage": {"input_tokens": 0, "output_tokens": 0}
        });
        Box::pin(async move { Ok(response.to_string()) })
    }
}

#[tokio::test]
async fn installed_batch_notebook_example_executes_against_mocked_provider() {
    let source = std::fs::read_to_string(
        test_campaign::declared_workspace_package().join(".exomonad/checks/jev-batch-example.hs"),
    )
    .expect("canonical installed batch notebook source");
    let backend = Arc::new(BatchBackend::default());
    let owned_backend = Arc::clone(&backend);
    let campaign = TestCampaign::start_with_conversation(
        |admission| admission,
        |config| {
            config.jev = Some(owned_backend as exomonad_actor::JevBackendHandle);
            test_campaign::pinned_jev_workspace(config);
        },
        Some(Arc::new(|_, _| {
            Box::pin(async { Err(exomonad_actor::ConversationUnavailable::Unbound) })
        })),
    )
    .await;
    campaign
        .run_scenario(|campaign| {
            Box::pin(async move {
                let endpoint = Arc::clone(&campaign.root_installation.policy);
                let store =
                    super::display_output::open_run_store(campaign.session_root.path()).unwrap();
                let origin = test_campaign::actor_output_origin(
                    &super::runtime_namespace(campaign.session_root.path()),
                    campaign.actor.identity(),
                );
                let receipt = campaign
                    .drive_actor_output(&store, async {
                        test_campaign::dispatch_haskell_script_named(
                            endpoint.as_ref(),
                            "haskell_sync",
                            &source,
                        )
                        .await
                    })
                    .await;
                let displays =
                    test_campaign::retained_committed_display_texts(&store, &origin, &receipt);
                assert_eq!(displays.len(), 4, "{receipt}");
                assert!(displays[0].contains("Evaluation"), "{displays:?}");
                assert!(
                    displays[1].contains("call") && displays[1].contains("other"),
                    "{displays:?}"
                );
                assert!(displays[2].contains("Doubt"), "{displays:?}");
                assert!(
                    displays[3].contains("Inspect the original call at src/main.rs:83."),
                    "explicit selected continuation result: {displays:?}"
                );
                assert!(!displays[3].contains("original match"), "{displays:?}");
                assert_eq!(backend.calls.load(Ordering::SeqCst), 1);
            })
        })
        .await;
}
