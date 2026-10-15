//! Recordings survive a real source publication while executable payloads
//! are reconstructed from the newly published module.
use super::test_campaign::{self, TestCampaign};
use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

const PROBE: &str = include_str!("fixtures/jev-recording-reload.hs");
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
        assert_eq!(request["state"], serde_json::json!({"evidence": "unchanged evidence"}));
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
    let receipt = campaign
        .drive_actor_output(&store, async {
            let result = test_campaign::dispatch_haskell_script_named(endpoint, "haskell_sync", source).await;
            assert_eq!(result["status"], "committed", "{result}");
            result
        })
        .await;
    test_campaign::committed_display_text(&receipt).to_owned()
}

#[tokio::test]
async fn saved_jev_observation_replays_after_reload_with_new_payload_and_no_provider_call() {
    let backend = Arc::new(RecordingBackend::default());
    let owned_backend = Arc::clone(&backend);
    let campaign = TestCampaign::start_with_conversation(
        |admission| admission,
        |config| {
            config.jev = Some(owned_backend as exomonad_actor::JevBackendHandle);
            test_campaign::pinned_jev_workspace(config);
            let authored = config.workspace.join(".exomonad");
            std::fs::create_dir_all(authored.join("Project")).unwrap();
            std::fs::create_dir_all(authored.join("state")).unwrap();
            std::fs::write(authored.join("Project/RecordingReloadProbe.hs"), PROBE).unwrap();
            let installed = test_campaign::declared_workspace_package().join(".exomonad");
            crate::exomonad::write_fixture_project_config(&authored, "test-model", |project| {
                project.haskell.source_roots = vec![".".into(), installed];
                project.haskell.modules = vec!["Project.RecordingReloadProbe".into()];
                project.haskell.flake_sources.insert("jev-dsl".into(), vec!["core".into()]);
            });
            config.workspace_inputs = Some(
                crate::exomonad::workspace::FrozenWorkspace::load(
                    &config.workspace, &config.run_directory.path(),
                ).expect("capture authored probe and exact installed Jev sources"),
            );
        },
        Some(Arc::new(|_, _| {
            Box::pin(async { Err(exomonad_actor::ConversationUnavailable::Unbound) })
        })),
    ).await;
    campaign.run_scenario(|campaign| Box::pin(async move {
        let endpoint = Arc::clone(&campaign.root_installation.policy);
        let first = displayed(campaign, endpoint.as_ref(),
            "observed <- observeAndSave \".exomonad/state/jev-observation.json\"\ndisplay (show observed)").await;
        assert_eq!(first, "Right 12");
        assert_eq!(backend.calls.load(Ordering::SeqCst), 1);
        let path = campaign._repository.path().join(ARCHIVE);
        let original_archive = std::fs::read(&path).unwrap();
        let decoded: serde_json::Value = serde_json::from_slice(&original_archive).unwrap();
        assert_eq!(decoded["version"], 1);
        assert_eq!(decoded["records"].as_array().unwrap().len(), 1);

        let edited = PROBE.replace("payloadOffset = 7", "payloadOffset = 31");
        assert_ne!(edited, PROBE);
        std::fs::write(campaign._repository.path().join(".exomonad/Project/RecordingReloadProbe.hs"), edited).unwrap();
        let published = displayed(campaign, endpoint.as_ref(),
            "Right outcome <- reloadSource []\ndisplay (case outcome of { ReloadPublished before after changed _ -> revisionIdentity before /= revisionIdentity after && \"Project.RecordingReloadProbe\" `elem` changed; _ -> False })").await;
        assert_eq!(published, "True");
        let replayed = displayed(campaign, endpoint.as_ref(),
            "replayed <- replaySaved \".exomonad/state/jev-observation.json\"\ndisplay (show replayed)").await;
        assert_eq!(replayed, "Right 36", "new source supplies the payload; archive supplies only the observation");
        let refused = displayed(campaign, endpoint.as_ref(),
            "refused <- refusesChangedEvidence \".exomonad/state/jev-observation.json\"\ndisplay (show refused)").await;
        assert_eq!(refused, "Right True");
        assert_eq!(backend.calls.load(Ordering::SeqCst), 1, "replay and refusal never fall back to the provider");
        assert_eq!(std::fs::read(path).unwrap(), original_archive, "reload and replay do not overwrite retained evidence");
    })).await;
}
