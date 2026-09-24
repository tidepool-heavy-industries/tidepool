#![allow(
    clippy::disallowed_methods,
    reason = "test: launches real tmux/process fixtures directly, not through the production launcher"
)]
use super::test_campaign::TestCampaign;
use super::tests::{dispatch_haskell_script, dispatch_structured_tool};
use super::*;
use super::{command_jobs_tests::backend_request, command_jobs_tests::TestCommands};
use exomonad_actor::{JevBackend, JevCallFailure};

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

/// Answers one Score packet and then one Choice packet without contacting a
/// provider. Both requests are made by separate cell programs on the same
/// resident machine, so this covers installation of repeated `J..|` evidence.
struct SequentialScoreChoiceJev {
    requests: Mutex<Vec<serde_json::Value>>,
    calls: std::sync::atomic::AtomicUsize,
}

impl JevBackend for SequentialScoreChoiceJev {
    fn ask(
        &self,
        request: String,
    ) -> futures_util::future::BoxFuture<'_, Result<String, JevCallFailure>> {
        use std::sync::atomic::Ordering;

        let request: serde_json::Value = serde_json::from_str(&request).expect("request is JSON");
        self.requests.lock().push(request.clone());
        let call = self.calls.fetch_add(1, Ordering::Relaxed);
        let answers = request["questions"]
            .as_object()
            .expect("one or more Jev questions")
            .iter()
            .map(|(key, question)| {
                let answer = match (call, question["type"].as_str()) {
                    (0, Some("score")) => {
                        let criteria = question["criteria"]
                            .as_array()
                            .expect("score question criteria");
                        let selected = criteria.len().saturating_sub(1);
                        let legend = criteria
                            .iter()
                            .enumerate()
                            .map(|(index, value)| (index.to_string(), value.clone()))
                            .collect::<serde_json::Map<_, _>>();
                        let probabilities = criteria
                            .iter()
                            .enumerate()
                            .map(|(index, _)| {
                                (
                                    index.to_string(),
                                    serde_json::json!(if index == selected { 1.0 } else { 0.0 }),
                                )
                            })
                            .collect::<serde_json::Map<_, _>>();
                        serde_json::json!({
                            "type": "score",
                            "score": selected,
                            "confidence": 1.0,
                            "legend": legend,
                            "probabilities": probabilities,
                        })
                    }
                    (1, Some("choice")) => serde_json::json!({
                        "type": "choice",
                        "choice": "second",
                        "probabilities": {"first": 0.0, "second": 1.0},
                        "confidence": 1.0,
                    }),
                    (call, kind) => panic!("unexpected Jev request {call}: {kind:?}; {question}"),
                };
                (key.clone(), answer)
            })
            .collect::<serde_json::Map<_, _>>();
        let body = serde_json::json!({
            "model": "jev-test",
            "answers": answers,
            "usage": {},
        })
        .to_string();
        Box::pin(async move { Ok(body) })
    }
}

/// Mirrors every score question and makes sections containing `ESSENTIAL`
/// outrank the rest. The response legend is copied from the request so the
/// pinned DSL's exact response validation remains part of the test.
struct SectionScoreJev {
    requests: Mutex<Vec<serde_json::Value>>,
}

impl JevBackend for SectionScoreJev {
    fn ask(
        &self,
        request: String,
    ) -> futures_util::future::BoxFuture<'_, Result<String, JevCallFailure>> {
        let parsed: serde_json::Value = serde_json::from_str(&request).expect("request is JSON");
        self.requests.lock().push(parsed.clone());
        let answers = parsed["questions"]
            .as_object()
            .into_iter()
            .flatten()
            .map(|(key, question)| {
                let essential = question
                    .get("instructions")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|text| text.contains("ESSENTIAL"));
                let legend = question["criteria"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .enumerate()
                    .map(|(index, value)| (index.to_string(), value.clone()))
                    .collect::<serde_json::Map<_, _>>();
                let (score, probabilities) = if essential {
                    (
                        3.0,
                        serde_json::json!({"0": 0.0, "1": 0.0, "2": 0.0, "3": 1.0}),
                    )
                } else {
                    (
                        0.0,
                        serde_json::json!({"0": 1.0, "1": 0.0, "2": 0.0, "3": 0.0}),
                    )
                };
                (
                    key.clone(),
                    serde_json::json!({
                        "type": "score",
                        "score": score,
                        "confidence": 1.0,
                        "legend": legend,
                        "probabilities": probabilities,
                    }),
                )
            })
            .collect::<serde_json::Map<_, _>>();
        let body = serde_json::json!({
            "model": "jev-test",
            "answers": answers,
            "usage": {},
        })
        .to_string();
        Box::pin(async move { Ok(body) })
    }
}

/// Scores every offered declaration as directly useful, or fails on demand.
/// This tests selection limits without depending on a provider's judgment.
struct LookupScoreJev {
    requests: Mutex<Vec<serde_json::Value>>,
    fail: Mutex<bool>,
    score: Mutex<u8>,
}

impl JevBackend for LookupScoreJev {
    fn ask(
        &self,
        request: String,
    ) -> futures_util::future::BoxFuture<'_, Result<String, JevCallFailure>> {
        let request: serde_json::Value = serde_json::from_str(&request).unwrap();
        self.requests.lock().push(request.clone());
        if *self.fail.lock() {
            return Box::pin(async { Err(JevCallFailure::Unconfigured) });
        }
        let score = *self.score.lock();
        let probabilities = (0..4)
            .map(|index| {
                (
                    index.to_string(),
                    serde_json::json!(if index == score { 1.0 } else { 0.0 }),
                )
            })
            .collect::<serde_json::Map<_, _>>();
        let answers = request["questions"]
            .as_object()
            .expect("batched score questions")
            .iter()
            .map(|(key, question)| {
                let legend = question["criteria"]
                    .as_array()
                    .expect("ordered rubric")
                    .iter()
                    .enumerate()
                    .map(|(index, label)| (index.to_string(), label.clone()))
                    .collect::<serde_json::Map<_, _>>();
                (
                    key.clone(),
                    serde_json::json!({
                        "type": "score", "score": score, "confidence": 1.0,
                        "legend": legend,
                        "probabilities": probabilities
                    }),
                )
            })
            .collect::<serde_json::Map<_, _>>();
        Box::pin(async move {
            Ok(
                serde_json::json!({"model": "jev-test", "answers": answers, "usage": {}})
                    .to_string(),
            )
        })
    }
}

fn lookup_enrichment_workspace(config: &mut ActorHostConfig) {
    let package = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../exomonad/examples/workspace")
        .canonicalize()
        .unwrap();
    let authored = config.workspace.join(".exomonad");
    std::fs::create_dir_all(&authored).unwrap();
    std::fs::write(
        authored.join("LookupFixture.hs"),
        include_str!("lookup_enrichment_fixture.hs"),
    )
    .unwrap();
    std::fs::write(authored.join("config.toml"), format!(
        "[defaults]\nmodel = 'test-model'\n\n[haskell]\nsource_roots = ['.', '{}']\nmodules = ['LookupFixture', 'Project.Lookup']\nspec = 'AgentSpec.agentSpec'\n\n[haskell.flake_sources]\njev-dsl = ['core']\n",
        package.join(".exomonad").display()
    )).unwrap();
    for name in ["flake.nix", "flake.lock"] {
        std::fs::copy(package.join(name), config.workspace.join(name)).unwrap();
    }
    super::test_campaign::commit_workspace(&config.workspace);
    config.workspace_inputs = Some(
        crate::exomonad::workspace::FrozenWorkspace::load(&config.workspace, &config.run_root)
            .expect("resolve lookup template and fixture"),
    );
}

#[test]
fn lookup_tool_policy_matches_native_ghc_oracle() {
    let library = crate::haskell_sources::ensure_embedded_stdlib().unwrap();
    let effects = tidepool_mcp::ensure_effects_module(&exomonad_effect_declarations()).unwrap();
    let mut command = std::process::Command::new("runghc");
    for path in effects.include_paths() {
        command.arg(format!("-i{}", path.display()));
    }
    let output = command
        .arg(format!("-i{}", library.display()))
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/actor_host/lookup_policy_oracle.hs"
        ))
        .output()
        .expect("run through just with the pinned GHC toolchain");
    assert!(
        output.status.success(),
        "native oracle failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap().trim(),
        "lookup policy oracle passed (8 checks)"
    );
}

async fn lookup_campaign() -> (TestCampaign, Arc<LookupScoreJev>) {
    let backend = Arc::new(LookupScoreJev {
        requests: Mutex::new(Vec::new()),
        fail: Mutex::new(false),
        score: Mutex::new(3),
    });
    let campaign = TestCampaign::start_with_config(
        exomonad_actor::ResearchPolicy::default(),
        |admission| admission,
        |config| {
            config.jev = Some(Arc::clone(&backend) as exomonad_actor::JevBackendHandle);
            lookup_enrichment_workspace(config);
        },
    )
    .await;
    (campaign, backend)
}

#[tokio::test]
async fn template_lookup_raw_namespace_and_selection_policy_contracts() {
    let (campaign, backend) = lookup_campaign().await;
    let policy = campaign.root_installation.policy.as_ref();
    let helpers = dispatch_haskell_script(
        policy,
        "pure (LookupFixture.rankingCheck, LookupFixture.packingCheck)",
    )
    .await;
    assert_eq!(
        helpers["items"][0]["output"].as_str().map(|text| text
            .chars()
            .filter(|character| !character.is_whitespace())
            .collect::<String>()),
        Some("(True,True)".to_owned()),
        "pure policy contracts: {helpers}"
    );
    let raw = dispatch_haskell_script(policy, "LookupFixture.rawLookupCheck")
        .await
        .to_string();
    assert!(raw.contains("True"), "raw lookup failed: {raw}");
    let namespaces = dispatch_haskell_script(policy, "LookupFixture.namespaceCheck")
        .await
        .to_string();
    assert!(
        namespaces.contains("True"),
        "namespace identity lost: {namespaces}"
    );
    let empty = dispatch_haskell_script(policy, "LookupFixture.emptySelectionCheck")
        .await
        .to_string();
    assert!(empty.contains("True"), "empty selection failed: {empty}");
    assert!(
        backend.requests.lock().is_empty(),
        "raw and empty paths must bypass Jev"
    );
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

#[tokio::test]
async fn template_lookup_batches_related_declarations_and_recovers_from_jev_failure() {
    let (campaign, backend) = lookup_campaign().await;
    let policy = campaign.root_installation.policy.as_ref();
    let output = dispatch_structured_tool(
        policy,
        "lookup",
        serde_json::json!({
            "queries": ["LookupFixture.relatedRoot", "LookupFixture.alternativeRoot"]
        }),
    )
    .await
    .to_string();
    assert!(output.contains("relatedRoot"), "{output}");
    assert!(output.contains("Related declarations"), "{output}");
    assert_eq!(
        output.matches("related to:").count(),
        4,
        "four-declaration cap: {output}"
    );
    assert_eq!(
        backend.requests.lock().len(),
        1,
        "one scoring batch; no recursive expansion"
    );
    let request = backend.requests.lock()[0].clone();
    assert!(
        request["questions"]
            .as_object()
            .unwrap()
            .values()
            .all(|question| !question["instructions"]
                .as_str()
                .unwrap_or_default()
                .contains("\nLookupFixture.HiddenSecondDegree\nFor queries:")),
        "second-degree reference was scored as a candidate: {request}"
    );

    backend.requests.lock().clear();
    let missing = dispatch_structured_tool(
        policy,
        "lookup",
        serde_json::json!({
            "queries": ["LookupFixture.alternativeRoo"]
        }),
    )
    .await
    .to_string();
    assert!(
        missing.contains("alternativeRoo") && missing.contains("no match:"),
        "original miss must remain: {missing}"
    );
    assert!(missing.contains("Related declarations"), "{missing}");
    assert!(
        missing.contains("alternativeRoot"),
        "qualified-module alternative missing: {missing}"
    );
    assert_eq!(backend.requests.lock().len(), 1);

    *backend.score.lock() = 0;
    backend.requests.lock().clear();
    let irrelevant = dispatch_structured_tool(
        policy,
        "lookup",
        serde_json::json!({
            "queries": ["LookupFixture.relatedRoot"]
        }),
    )
    .await
    .to_string();
    assert!(irrelevant.contains("relatedRoot"), "{irrelevant}");
    assert!(
        !irrelevant.contains("Related declarations"),
        "irrelevant additions: {irrelevant}"
    );
    assert_eq!(backend.requests.lock().len(), 1);

    *backend.fail.lock() = true;
    backend.requests.lock().clear();
    let fallback = dispatch_structured_tool(
        policy,
        "lookup",
        serde_json::json!({
            "queries": ["LookupFixture.relatedRoot"]
        }),
    )
    .await
    .to_string();
    assert!(
        fallback.contains("relatedRoot"),
        "ordinary result lost on Jev failure: {fallback}"
    );
    assert!(!fallback.contains("Related declarations"), "{fallback}");
    assert_eq!(backend.requests.lock().len(), 1);
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

/// The Jev surface is pinned source, not Tidepool library: a run reaches it
/// through a workspace whose `flake.nix` names the jev-dsl revision and whose
/// own `Jev/Operators.hs` fixes that library's JSON type to Tidepool's. These
/// tests select the package this repository ships, so what they compile is
/// what a project gets — including the pin.
pub(super) fn pinned_jev_workspace(config: &mut ActorHostConfig) {
    let package = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../exomonad/examples/workspace")
        .canonicalize()
        .expect("the Exomonad workspace package this repository ships");
    let authored = config.workspace.join(".exomonad");
    std::fs::create_dir_all(&authored).unwrap();
    std::fs::write(
        authored.join("config.toml"),
        format!(
            "[defaults]\nmodel = 'test-model'\n\n[haskell]\nsource_roots = ['{}']\n\n[haskell.flake_sources]\njev-dsl = ['core']\n",
            package.join(".exomonad").display()
        ),
    )
    .unwrap();
    for name in ["flake.nix", "flake.lock"] {
        std::fs::copy(package.join(name), config.workspace.join(name)).unwrap();
    }
    super::test_campaign::commit_workspace(&config.workspace);
    config.workspace_inputs = Some(
        crate::exomonad::workspace::FrozenWorkspace::load(&config.workspace, &config.run_root)
            .expect("resolve the pinned Haskell source"),
    );
}

async fn campaign_with<B: JevBackend + 'static>(backend: Arc<B>) -> TestCampaign {
    TestCampaign::start_with_config(
        exomonad_actor::ResearchPolicy::default(),
        |admission| admission,
        |config| {
            config.jev = Some(backend as exomonad_actor::JevBackendHandle);
            pinned_jev_workspace(config);
        },
    )
    .await
}

fn selected_shell_workspace(config: &mut ActorHostConfig) {
    let package = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../exomonad/examples/workspace")
        .canonicalize()
        .expect("the shipped workspace template");
    let authored = config.workspace.join(".exomonad");
    std::fs::create_dir_all(&authored).unwrap();
    std::fs::write(
        authored.join("config.toml"),
        format!(
            "[defaults]\nmodel = 'test-model'\n\n\
             [haskell]\nsource_roots = ['{}']\n\
             modules = ['Project.Shell']\n\
             spec = 'AgentSpec.agentSpec'\n\n\
             [haskell.flake_sources]\njev-dsl = ['core']\n",
            package.join(".exomonad").display()
        ),
    )
    .unwrap();
    for name in ["flake.nix", "flake.lock"] {
        std::fs::copy(package.join(name), config.workspace.join(name)).unwrap();
    }
    super::test_campaign::commit_workspace(&config.workspace);
    config.workspace_inputs = Some(
        crate::exomonad::workspace::FrozenWorkspace::load(&config.workspace, &config.run_root)
            .expect("resolve the selected-shell template"),
    );
}

#[tokio::test]
async fn template_bash_scores_before_display_and_keeps_recovery() {
    let backend = Arc::new(SectionScoreJev {
        requests: Mutex::new(Vec::new()),
    });
    let mut campaign = TestCampaign::start_with_config(
        exomonad_actor::ResearchPolicy::default(),
        |admission| admission,
        |config| {
            config.jev = Some(Arc::clone(&backend) as exomonad_actor::JevBackendHandle);
            selected_shell_workspace(config);
        },
    )
    .await;
    let policy = Arc::clone(&campaign.root_installation.policy);
    let invoked_policy = Arc::clone(&policy);
    let command = format!(
        "for i in $(seq 1 900); do echo background-$i; done; echo ESSENTIAL-diagnostic; # {}COMMAND-TAIL",
        "x".repeat(9000)
    );
    let intent = format!(
        "retain the decisive diagnostic; {}INTENT-TAIL",
        "y".repeat(5000)
    );
    let mut invoked = tokio::spawn(async move {
        dispatch_structured_tool(
            invoked_policy.as_ref(),
            "bash",
            serde_json::json!({
                "cmd": command,
                "workdir": null,
                "environment": null,
                "memory_mib": null,
                "tty": null,
                "stdin": null,
                "yield_time_ms": 30000,
                "max_output_bytes": 2048,
                "intent": intent
            }),
        )
        .await
    });
    let command_output = (1..=900)
        .map(|index| format!("background-{index}\n"))
        .collect::<String>();
    let commands = TestCommands::completed_streams(&command_output, "ESSENTIAL-diagnostic\n");
    let request = tokio::select! {
        request = backend_request(&mut campaign) => request,
        result = &mut invoked => panic!("bash completed before requesting its command backend: {result:?}"),
    };
    request.supply(Ok(commands.clone()));
    let response = invoked.await.unwrap();
    assert_eq!(response["status"], "committed", "{response}");
    let output = response["items"][0]["output"].as_str().unwrap();
    assert!(output.contains("ESSENTIAL-diagnostic"), "{output}");
    assert!(output.contains("<s"), "section markers missing: {output}");
    assert!(
        output.contains("omitted:"),
        "omission summary missing: {output}"
    );
    let binding = response["items"][0]["installedBindings"][0]
        .as_str()
        .expect("bash installs its retained job binding");
    assert!(
        output.contains(&format!("Project.Shell.outputSnapshot {binding}")),
        "recovery should name the real retained binding {binding}, not a placeholder: {output}"
    );
    assert!(
        !output.contains("jobN") && !output.contains("{{job_binding}}"),
        "recovery must not leak an unresolved binding placeholder: {output}"
    );
    assert!(
        output.len() <= 2048,
        "selected output exceeded its byte budget"
    );
    let section_footer_prefix = "Project.Shell.section snap (Project.Shell.SectionId ";
    let section_footer = output
        .rsplit_once(section_footer_prefix)
        .map(|(_, rest)| rest)
        .unwrap_or_default();
    assert!(
        section_footer
            .strip_suffix(").")
            .is_some_and(|digits| !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit())),
        "recovery footer should name a real omitted section id, not a placeholder: {output}"
    );
    assert!(
        output.ends_with(")."),
        "recovery footer was truncated: {output}"
    );
    assert!(
        !output.lines().any(|line| line == "background-600"),
        "a low-ranked section exceeded the selection budget: {output}"
    );

    {
        let requests = backend.requests.lock();
        assert!(!requests.is_empty(), "long output must invoke Jev");
        assert!(
            requests[0]["state"]
                .as_str()
                .is_some_and(|state| state.contains("for i in $(seq 1 900)")),
            "command absent from Jev state: {}",
            requests[0]["state"]
        );
        assert_eq!(commands.executions(), 1, "selection must not re-execute");
        let state = requests[0]["state"].as_str().expect("Jev state text");
        assert!(
            !state.contains("COMMAND-TAIL"),
            "command tail escaped its prompt bound"
        );
        assert!(
            !state.contains("INTENT-TAIL"),
            "intent tail escaped its prompt bound"
        );
    }

    commands.shorten_slice_read(3);
    let short_page = dispatch_haskell_script(
        campaign.root_installation.policy.as_ref(),
        &format!(
            "Project.Shell.sectionPage (Project.Shell.outputSnapshot {binding} {} {}) (Project.Shell.SectionId 1)",
            command_output.len(),
            "ESSENTIAL-diagnostic\n".len()
        ),
    )
    .await
    .to_string();
    assert!(
        short_page.contains("SnapshotExpired Stdout"),
        "a short section reread must report snapshot loss: {short_page}"
    );
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

/// Output under the raw-display line threshold and within the presentation
/// budget is shown as-is, without ever asking Jev to score it.
#[tokio::test]
async fn template_bash_shows_short_output_raw_without_jev() {
    let backend = Arc::new(SectionScoreJev {
        requests: Mutex::new(Vec::new()),
    });
    let mut campaign = TestCampaign::start_with_config(
        exomonad_actor::ResearchPolicy::default(),
        |admission| admission,
        |config| {
            config.jev = Some(Arc::clone(&backend) as exomonad_actor::JevBackendHandle);
            selected_shell_workspace(config);
        },
    )
    .await;
    let policy = Arc::clone(&campaign.root_installation.policy);
    let invoked_policy = Arc::clone(&policy);
    let mut invoked = tokio::spawn(async move {
        dispatch_structured_tool(
            invoked_policy.as_ref(),
            "bash",
            serde_json::json!({
                "cmd": "printf 'line1\\nline2\\nline3\\n'",
                "workdir": null,
                "environment": null,
                "memory_mib": null,
                "tty": null,
                "stdin": null,
                "yield_time_ms": 30000,
                "max_output_bytes": null,
                "intent": "short raw check"
            }),
        )
        .await
    });
    let commands = TestCommands::completed_streams("line1\nline2\nline3\n", "");
    let request = tokio::select! {
        request = backend_request(&mut campaign) => request,
        result = &mut invoked => panic!("bash completed before requesting its command backend: {result:?}"),
    };
    request.supply(Ok(commands));
    let response = invoked.await.unwrap();
    assert_eq!(response["status"], "committed", "{response}");
    let output = response["items"][0]["output"].as_str().unwrap();
    assert!(
        output.contains("line1") && output.contains("line3"),
        "{output}"
    );
    assert!(
        !output.contains("<s"),
        "short output should not be sectioned or marked up: {output}"
    );
    assert!(
        !any_section_scoring_request(&backend),
        "short output must not invoke Jev to score sections: {output}\nrequests: {:?}",
        backend.requests.lock()
    );
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

/// Requests carrying a `Project.Shell` section-relevance question, as
/// opposed to the campaign's own unrelated per-call heuristics review (a
/// destructive-command/repeating-itself check the harness runs regardless
/// of command output, and which never mentions "sections").
fn any_section_scoring_request(backend: &SectionScoreJev) -> bool {
    backend.requests.lock().iter().any(|request| {
        request["questions"]
            .as_object()
            .is_some_and(|questions| questions.keys().any(|key| key.starts_with("sections.")))
    })
}

/// A `max_output_bytes` below the supported 1024..32768 range is clamped
/// rather than rejected: the call still starts and completes.
#[tokio::test]
async fn template_bash_accepts_undersized_max_output_bytes() {
    let backend = Arc::new(SectionScoreJev {
        requests: Mutex::new(Vec::new()),
    });
    let mut campaign = TestCampaign::start_with_config(
        exomonad_actor::ResearchPolicy::default(),
        |admission| admission,
        |config| {
            config.jev = Some(Arc::clone(&backend) as exomonad_actor::JevBackendHandle);
            selected_shell_workspace(config);
        },
    )
    .await;
    let policy = Arc::clone(&campaign.root_installation.policy);
    let invoked_policy = Arc::clone(&policy);
    let mut invoked = tokio::spawn(async move {
        dispatch_structured_tool(
            invoked_policy.as_ref(),
            "bash",
            serde_json::json!({
                "cmd": "printf 'ok\\n'",
                "workdir": null,
                "environment": null,
                "memory_mib": null,
                "tty": null,
                "stdin": null,
                "yield_time_ms": 30000,
                "max_output_bytes": 800,
                "intent": "undersized budget"
            }),
        )
        .await
    });
    let commands = TestCommands::completed_streams("ok\n", "");
    let request = tokio::select! {
        request = backend_request(&mut campaign) => request,
        result = &mut invoked => panic!("bash completed before requesting its command backend: {result:?}"),
    };
    request.supply(Ok(commands));
    let response = invoked.await.unwrap();
    assert_eq!(response["status"], "committed", "{response}");
    let output = response["items"][0]["output"].as_str().unwrap();
    assert!(
        !output.contains("Rejected"),
        "an undersized max_output_bytes must be clamped, not rejected: {output}"
    );
    assert!(output.contains("ok"), "{output}");
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

/// No `LANGUAGE` pragma: `OverloadedLabels` is in the cell dialect
/// (`session::dialect::EVAL_PRAGMAS`), so `#not_here` needs no ceremony.
const CELL: &str = r#"answer <- J.ask1 (J.rawState (String "retry loop in fetch; timeout branch at line 12"))
  (J.choice "Which line begins the retry-timeout branch?"
     (J.alt #not_here "The branch is not in this file" (0 :: Int)
        J..| J.many #line (\(k, _, _) -> k) (\(_, w, _) -> w)
               [("line-4", "if attempts > 3", 4), ("line-12", "if elapsed > timeout", 12)]))
either (const 0) (\a -> J.handle a (#not_here id J..| #line (\_ (_, _, n) -> n))) answer"#;

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
    {
        let requests = backend.requests.lock();
        assert_eq!(requests.len(), 1);
        let request = &requests[0];
        assert_eq!(request["model"], "jev-latest", "{request}");
        assert_eq!(request["questions"]["value"]["type"], "choice", "{request}");
    }
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

#[tokio::test]
async fn score_then_choice_packets_with_alternatives_install_on_one_machine() {
    let backend = Arc::new(SequentialScoreChoiceJev {
        requests: Mutex::new(Vec::new()),
        calls: std::sync::atomic::AtomicUsize::new(0),
    });
    let campaign = campaign_with(Arc::clone(&backend)).await;

    let score = dispatch_haskell_script(
        campaign.root_installation.policy.as_ref(),
        r#"let rubric = J.level #low "low" (0 :: Int) J..| J.level #high "high" 1
answer <- J.ask (J.rawState (String "one machine regression")) (#q := J.score "How important is this?" rubric)
fmap (\a -> a.q.expectation) answer"#,
    )
    .await;
    assert_eq!(score["status"], "committed", "Score packet failed: {score}");

    let choice = dispatch_haskell_script(
        campaign.root_installation.policy.as_ref(),
        r#"answer <- J.ask1 (J.rawState (String "one machine regression"))
  (J.choice "Which branch?" (J.alt #first "First branch" (1 :: Int) J..| J.alt #second "Second branch" 2))
either (const 0) (\selected -> J.handle selected (#first id J..| #second id)) answer"#,
    )
    .await;
    assert_eq!(
        choice["status"], "committed",
        "Choice packet failed after Score: {choice}"
    );
    assert_eq!(
        choice["items"].as_array().unwrap().last().unwrap()["output"],
        "2",
        "Choice answer did not reach the selected branch: {choice}"
    );

    {
        let requests = backend.requests.lock();
        assert_eq!(requests.len(), 2, "both cells should ask the fake backend");
        assert_eq!(
            requests[0]["questions"]["q"]["type"], "score",
            "{requests:?}"
        );
        assert_eq!(
            requests[1]["questions"]["value"]["type"], "choice",
            "{requests:?}"
        );
    }

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
            "either (const 0) (\\a -> J.handle a (#not_here id J..| #line (\\_ (_, _, n) -> n))) answer",
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
let packet = #place := J.choice "Which line begins the retry-timeout branch?" offers :& #enough := J.noul "Is the branch visible?"
answer <- J.ask (J.rawState (String "retry loop in fetch")) packet
either (const 0) (\r -> J.handle (J.answers r).place (#line_4 id J..| #line_12 id)) answer"#,
    )
    .await;
    assert_eq!(result["status"], "committed", "{result}");
    let items = result["items"].as_array().unwrap();
    assert_eq!(items.last().unwrap()["output"], "12", "{result}");
    assert_eq!(backend.requests.lock().len(), 1);
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

/// A Jev-dense cell judges every file of a bound preview list in one packet,
/// and an unconfigured endpoint reaches the cell as an ordinary `Left`. The
/// per-row battery flattens to dotted wire keys, one per row, beside the
/// top-level cell.
#[tokio::test]
async fn a_per_row_battery_sends_one_request_with_one_question_per_row() {
    let backend = Arc::new(FakeJev {
        requests: Mutex::new(Vec::new()),
        answer: Err(JevCallFailure::Unconfigured),
    });
    let campaign = campaign_with(Arc::clone(&backend)).await;
    let result = dispatch_haskell_script(
        campaign.root_installation.policy.as_ref(),
        r##"let previews = [("README.md", "# jev-dsl\ntyped packets"), ("LICENSE", "MIT")] :: [(Text, Text)]
answer <- J.ask (J.rawState (String "choosing what to read next"))
  ( #enough := J.noul "Is the listing enough to choose from?"
 :& #worth_reading := J.each fst (\(_, body) -> J.noul ("Worth reading in full? " <> body)) previews )
either (T.pack . show) (const "answered") answer :: Text"##,
    )
    .await;
    assert_eq!(result["status"], "committed", "{result}");
    let output = result["items"].as_array().unwrap().last().unwrap()["output"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(output.contains("no Jev endpoint is configured"), "{result}");
    {
        let requests = backend.requests.lock();
        assert_eq!(requests.len(), 1);
        let questions = &requests[0]["questions"];
        assert!(
            questions.get("enough").is_some(),
            "enough missing: {questions}"
        );
        let per_file = questions
            .as_object()
            .unwrap()
            .keys()
            .filter(|k| k.starts_with("worth_reading."))
            .count();
        assert_eq!(per_file, 2, "one question per previewed file: {questions}");
    }
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
        exomonad_actor::ResearchPolicy::default(),
        |admission| admission,
        |config| {
            config.jev = None;
            pinned_jev_workspace(config);
        },
    )
    .await;
    let result = dispatch_haskell_script(
        campaign.root_installation.policy.as_ref(),
        r#"answer <- J.ask1 (J.rawState (String "A cat is sitting on a warm windowsill in the sun."))
  (J.choice "Where is the cat?"
     (J.alt #windowsill "On a windowsill" (1 :: Int)
        J..| J.alt #roof "On a roof" 2
        J..| J.alt #bed "In a bed" 3))
either (const 0) (\a -> J.handle a (#windowsill id J..| #roof id J..| #bed id)) answer"#,
    )
    .await;
    assert_eq!(result["status"], "committed", "{result}");
    let items = result["items"].as_array().unwrap();
    assert_eq!(items.last().unwrap()["output"], "1", "{result}");
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

// ---------------------------------------------------------------------------
// A pre-flight for the live demo: the agent's OWN hosted tool body, and its
// after-tool slot, each asking Jev from inside the retained compiled agent
// spec, rather than from a notebook cell. A cell reaches the facade as `J`
// through the workbench; a module under a source root does not and must
// import it itself.
// ---------------------------------------------------------------------------

/// `Project.Tools`: one tool whose body asks Jev a yes/no question about its
/// own `topic` argument and answers with text naming the branch the scripted
/// answer took. `Member Jev effects` is worked out from `Jev.Operators.ask1`'s
/// own `Member Jev effs` constraint, and carried on both `probeBody` and
/// `tools` so the constraint reaches whatever concrete row the spec compiles
/// against.
const AGENT_SPEC_JEV_TOOLS_MODULE: &str = r#"{-# LANGUAGE DeriveAnyClass #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE OverloadedLabels #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeOperators #-}
module Project.Tools (SpecTools (..), Probe (..), probeBody, tools) where

import Control.Monad.Freer (Eff, Member)
import Data.Text (Text)
import GHC.Generics (Generic)
import Tidepool.Aeson.FromJSON (FromJSON)
import Tidepool.Aeson.Value (Value (String))
import Tidepool.Agent.Contract
import Tidepool.Effects.Core (Jev)
import qualified Jev.Operators as J

newtype Probe = Probe { topic :: Text }
  deriving (Generic, FromJSON, JsonSchema)

newtype SpecTools mode = SpecTools { probe :: mode :- Call Probe Text }
  deriving (Generic)

-- | The tool's implementation as ordinary source: one Jev yes/no question
-- about its own argument, answered with text that names the branch the
-- scripted answer took.
probeBody :: Member Jev effects => Probe -> Eff effects Text
probeBody request = do
  answer <-
    J.ask1
      (J.rawState (String (topic request)))
      ( J.choice
          "Does this topic warrant investigation?"
          ( J.alt #yes "The topic clearly warrants investigation" ()
              J..| J.alt #no "The topic does not warrant investigation" ()
          )
      )
  pure $ case answer of
    Left _ -> "tool-body: jev unavailable for " <> topic request
    Right a ->
      J.handle
        a
        ( #yes (\_ -> "tool-body branch: yes, investigate " <> topic request)
            J..| #no (\_ -> "tool-body branch: no, skip " <> topic request)
        )

tools :: Member Jev effects => SpecTools (AsServerT (Eff effects))
tools =
  SpecTools
    { probe = tool "Answer one fixed question about a topic, judged by Jev." probeBody }
"#;

/// `AgentSpec`: the same tools record, with an after-tool slot that asks Jev
/// a second, independent yes/no question about `toolResultOutput result` (not
/// about the tool's argument) and returns `Annotated` text naming the branch
/// ITS answer took.
const AGENT_SPEC_JEV_SPEC_MODULE: &str = r#"{-# LANGUAGE OverloadedLabels #-}
{-# LANGUAGE OverloadedStrings #-}
module AgentSpec (agentSpec) where

import Control.Monad.Freer (Eff, Member)
import qualified Data.Text as T
import Tidepool.Aeson.Value (Value (String))
import Tidepool.Agent.Contract
import Tidepool.Effects.Core (Jev)
import qualified Jev.Operators as J
import qualified Project.Tools as Tools

agentSpec :: Member Jev effects => AgentSpec Tools.SpecTools effects
agentSpec =
  defaultSpec
    { specTools = Tools.tools
    , afterTool = Just noted
    }

-- | Shown every finished @probe@ call and what it answered. Asks Jev its OWN
-- question about the tool's output, independently of what the tool body
-- asked about the argument.
noted :: Member Jev effects => ToolCall -> ToolResult -> Eff effects Annotation
noted call result
  | toolCallName call /= T.pack "probe" = pure NoAnnotation
  | otherwise = do
      answer <-
        J.ask1
          (J.rawState (String (toolResultOutput result)))
          ( J.choice
              "Does this tool output look complete?"
              ( J.alt #yes "The output looks complete" ()
                  J..| J.alt #no "The output looks incomplete" ()
              )
          )
      pure $ case answer of
        Left _ -> Annotated (T.pack "after-tool branch: jev unavailable")
        Right a ->
          Annotated
            ( T.pack "ordinal=" <> T.pack (show (toolResultOrdinal result))
                <> T.pack " handle=" <> toolResultHandle result
                <> T.pack " branch=" <> J.handle a
                ( #yes (\_ -> T.pack "after-tool branch: complete")
                    J..| #no (\_ -> T.pack "after-tool branch: incomplete")
                )
            )
"#;

/// A workspace that combines `pinned_jev_workspace`'s pinned Jev facade with
/// an authored `Project/Tools.hs` + `AgentSpec.hs`: two source roots at once
/// (this workspace's own `.exomonad`, and the pinned package's), so a module
/// under either root can `import qualified Jev.Operators as J` itself.
fn pinned_jev_agent_spec_workspace(config: &mut ActorHostConfig) {
    // Everything `pinned_jev_workspace` does, inlined rather than called: that
    // helper ends by calling `FrozenWorkspace::load`, which memoizes its
    // result at `run_root/workspace/selection.json` and would otherwise hand
    // a SECOND call here the stale pre-authored selection instead of
    // re-reading the config and files written below. One workspace, one load.
    let package = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../exomonad/examples/workspace")
        .canonicalize()
        .expect("the Exomonad workspace package this repository ships");
    let authored = config.workspace.join(".exomonad");
    std::fs::create_dir_all(authored.join("Project")).unwrap();
    std::fs::write(
        authored.join("config.toml"),
        format!(
            "[defaults]\nmodel = 'test-model'\n\n\
             [haskell]\nsource_roots = ['.', '{}']\n\
             modules = ['Project.Tools', 'AgentSpec']\n\
             tools = 'Project.Tools.tools'\n\
             spec = 'AgentSpec.agentSpec'\n\n\
             [haskell.flake_sources]\njev-dsl = ['core']\n",
            package.join(".exomonad").display()
        ),
    )
    .unwrap();
    std::fs::write(
        authored.join("Project/Tools.hs"),
        AGENT_SPEC_JEV_TOOLS_MODULE,
    )
    .unwrap();
    std::fs::write(authored.join("AgentSpec.hs"), AGENT_SPEC_JEV_SPEC_MODULE).unwrap();
    for name in ["flake.nix", "flake.lock"] {
        std::fs::copy(package.join(name), config.workspace.join(name)).unwrap();
    }
    // `nix flake archive` reads only tracked files, and a child worktree
    // admission refuses a dirty source repository.
    super::test_campaign::commit_workspace(&config.workspace);
    config.workspace_inputs = Some(
        crate::exomonad::workspace::FrozenWorkspace::load(&config.workspace, &config.run_root)
            .expect("resolve the combined pinned + authored Haskell source"),
    );
}

async fn probe_topic(policy: &dyn exomonad_actor::ResidentToolEndpoint, topic: &str) -> String {
    dispatch_structured_tool(policy, "probe", serde_json::json!({"topic": topic}))
        .await
        .to_string()
}

async fn detailed_status(policy: &dyn exomonad_actor::ResidentToolEndpoint) -> String {
    dispatch_structured_tool(policy, "status", serde_json::json!({"view": "detailed"}))
        .await
        .to_string()
}

/// The pre-flight the live demo depends on: nobody has yet proven that both
/// an agent's own hosted tool body AND its after-tool slot can ask Jev from
/// inside the retained compiled agent spec (as opposed to a notebook cell,
/// which is the only path every other Jev test exercises). One scripted
/// answer serves both requests, because both questions offer the same
/// `#yes`/`#no` alternatives; what is under test is that each call site can
/// reach the Jev effect on its own, not that they read different content.
#[tokio::test]
async fn a_tool_body_and_a_slot_can_both_ask_jev() {
    let backend = Arc::new(FakeJev {
        requests: Mutex::new(Vec::new()),
        answer: Ok(serde_json::json!({
            "model": "jev-1.13.0",
            "answers": {"value": {
                "type": "choice",
                "choice": "yes",
                "probabilities": {"yes": 0.9, "no": 0.1},
                "confidence": 0.9
            }},
            "usage": {}
        })
        .to_string()),
    });

    let campaign = tokio::time::timeout(
        Duration::from_secs(120),
        TestCampaign::start_with_config(
            exomonad_actor::ResearchPolicy::default(),
            |admission| admission,
            |config| {
                config.jev = Some(Arc::clone(&backend) as exomonad_actor::JevBackendHandle);
                pinned_jev_agent_spec_workspace(config);
            },
        ),
    )
    .await
    .expect("campaign did not start within 120s");
    let policy = campaign.root_installation.policy.clone();
    let policy = policy.as_ref();

    let result = tokio::time::timeout(
        Duration::from_secs(120),
        probe_topic(policy, "the retry loop"),
    )
    .await
    .expect("probe did not answer within 120s");

    // (1) The tool body's own Jev branch, in the tool's own output.
    assert!(
        result.contains("tool-body branch: yes, investigate the retry loop"),
        "{result}"
    );
    // (2) The after-tool slot's own Jev branch, delivered as derived context
    // beside — not instead of — the tool's output.
    assert!(result.contains("[after-tool]"), "{result}");
    assert!(
        result.contains("Derived context, not part of the tool's output"),
        "{result}"
    );
    assert!(result.contains("ordinal=1 handle=toolResult1"), "{result}");
    assert!(result.contains("after-tool branch: complete"), "{result}");

    // (3) The fake backend saw two independent Jev requests: one from the
    // tool body, one from the slot.
    assert_eq!(
        backend.requests.lock().len(),
        2,
        "one request from the tool body and one from the after-tool slot"
    );

    // (4) `status` shows exactly one after-tool row, annotated.
    let status = tokio::time::timeout(Duration::from_secs(120), detailed_status(policy))
        .await
        .expect("status did not answer within 120s");
    assert_eq!(
        status.matches("after-tool#").count(),
        1,
        "one call, one invocation: {status}"
    );
    assert!(status.contains("after-tool#1"), "{status}");
    assert!(status.contains("annotated"), "{status}");

    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

// ---------------------------------------------------------------------------
// A parent's watchdog: `Project.Watchdog` (the shipped worked example, read
// verbatim from this repository's own `exomonad/examples/workspace/.exomonad`,
// exactly as `Jev.Operators` is) asks one packet of heuristics about a
// child's finished tool call. A tripped `Nudge` writes advice straight onto
// the child's own result and sends nothing; a tripped `Escalate` sends the
// parent one message naming the reason and the child's actor address and
// leaves only a short record on the child's side.
// ---------------------------------------------------------------------------

/// Mirrors whatever the packet asked, answering every leaf question as a
/// `noul` at one scripted likelihood. `Project.Watchdog` asks only nouls, so
/// this is the whole shape the wire needs: same keys as `questions`, each
/// answered `{"type": "noul", "noul": <likelihood>}`, exactly the transport
/// `jev-dsl`'s own `test/Mini.hs` stub uses for an unrecognised question type.
struct ScriptedNoulJev {
    requests: Mutex<Vec<serde_json::Value>>,
    likelihood: f64,
}

impl JevBackend for ScriptedNoulJev {
    fn ask(
        &self,
        request: String,
    ) -> futures_util::future::BoxFuture<'_, Result<String, JevCallFailure>> {
        let parsed: serde_json::Value = serde_json::from_str(&request).expect("request is JSON");
        self.requests.lock().push(parsed.clone());
        let keys: Vec<String> = parsed["questions"]
            .as_object()
            .map(|object| object.keys().cloned().collect())
            .unwrap_or_default();
        let likelihood = self.likelihood;
        let answers: serde_json::Map<String, serde_json::Value> = keys
            .into_iter()
            .map(|key| (key, serde_json::json!({"type": "noul", "noul": likelihood})))
            .collect();
        let body = serde_json::json!({
            "model": "jev-test",
            "answers": answers,
            "usage": {},
        })
        .to_string();
        Box::pin(async move { Ok(body) })
    }
}

/// One `probe` tool (shell tools nested beside it), and a spec whose slot is
/// `Project.Watchdog.watchBy monitorsFor`: `monitorsFor` reads the calling
/// actor's own path and gives an escalation heuristic to a child labelled
/// `escalate-child`, an advisory one to a child labelled `nudge-child`, and
/// nothing to anybody else — including the root, which has no such label.
const WATCHDOG_TOOLS_MODULE: &str = r#"{-# LANGUAGE DeriveAnyClass #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeOperators #-}
module Project.Tools (WatchdogTools (..), Probe (..), probeBody, tools) where

import Control.Monad.Freer (Eff, Member)
import Data.Text (Text)
import GHC.Generics (Generic)
import Tidepool.Aeson.FromJSON (FromJSON)
import Tidepool.Agent.Contract
import qualified Tidepool.Command as Cmd
import qualified Tidepool.Command.Tools as Shell

newtype Probe = Probe { topic :: Text }
  deriving (Generic, FromJSON, JsonSchema)

data WatchdogTools mode = WatchdogTools
  { shell :: Shell.ShellTools mode
  , probe :: mode :- Call Probe Text
  }
  deriving (Generic)

tools :: Member Cmd.Commands effects => WatchdogTools (AsServerT (Eff effects))
tools = WatchdogTools { shell = Shell.tools, probe = tool "Answer one fixed question about a topic." probeBody }

probeBody :: Probe -> Eff effects Text
probeBody request = pure ("probed " <> topic request)
"#;

const WATCHDOG_SPEC_MODULE: &str = r#"{-# LANGUAGE OverloadedStrings #-}
module AgentSpec (agentSpec) where

import Control.Monad.Freer (Eff, Member)
import Data.Text (Text)
import qualified Data.Text as T
import Tidepool.Agent.Contract
import Tidepool.Effects.Core (ActorContext, Jev, Notifications, Reflect)
import qualified Tidepool.Command as Cmd
import qualified Project.Tools as Tools
import qualified Project.Watchdog as Watchdog

agentSpec
  :: (Member Cmd.Commands effects, Member Jev effects, Member ActorContext effects, Member Notifications effects, Member Reflect effects)
  => AgentSpec Tools.WatchdogTools effects
agentSpec = defaultSpec
  { specTools = Tools.tools
  , afterTool = Just (Watchdog.watchBy monitorsFor)
  }

monitorsFor :: Text -> [Watchdog.Heuristic]
monitorsFor path
  | "escalate-child" `T.isInfixOf` path = [Watchdog.outOfScope]
  | "nudge-child" `T.isInfixOf` path = [Watchdog.repeatingItself]
  | otherwise = []
"#;

/// The shipped package's own `.exomonad` (which carries `Jev/Operators.hs` AND
/// `Project/Watchdog.hs`, the worked example this test exercises verbatim) as
/// a second source root, beside a per-test `AgentSpec.hs` and `Project/Tools.hs`
/// that install `Watchdog.watchBy` as the after-tool slot.
fn pinned_watchdog_workspace(config: &mut ActorHostConfig) {
    let package = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../exomonad/examples/workspace")
        .canonicalize()
        .expect("the Exomonad workspace package this repository ships");
    let authored = config.workspace.join(".exomonad");
    std::fs::create_dir_all(authored.join("Project")).unwrap();
    std::fs::write(
        authored.join("config.toml"),
        format!(
            "[defaults]\nmodel = 'test-model'\n\n\
             [haskell]\nsource_roots = ['.', '{}']\n\
             modules = ['Project.Tools', 'AgentSpec']\n\
             tools = 'Project.Tools.tools'\n\
             spec = 'AgentSpec.agentSpec'\n\n\
             [haskell.flake_sources]\njev-dsl = ['core']\n",
            package.join(".exomonad").display()
        ),
    )
    .unwrap();
    std::fs::write(authored.join("Project/Tools.hs"), WATCHDOG_TOOLS_MODULE).unwrap();
    std::fs::write(authored.join("AgentSpec.hs"), WATCHDOG_SPEC_MODULE).unwrap();
    for name in ["flake.nix", "flake.lock"] {
        std::fs::copy(package.join(name), config.workspace.join(name)).unwrap();
    }
    // `nix flake archive` reads only tracked files, and a child worktree
    // admission refuses a dirty source repository.
    super::test_campaign::commit_workspace(&config.workspace);
    config.workspace_inputs = Some(
        crate::exomonad::workspace::FrozenWorkspace::load(&config.workspace, &config.run_root)
            .expect("resolve the combined pinned + authored Haskell source"),
    );
}

fn watchdog_child_script(label: &str) -> String {
    format!(
        "let campaign = \"watchdog\" :: CampaignLabel\n\
         let group = \"children\" :: ForkGroupLabel\n\
         let leaf = [label|{label}|]\n\
         worker <- unfold (batch campaign group) (child (coding @Text projectHead (assignment leaf ())))\n"
    )
}

/// Local to this file: `agent_spec_tests::next_child` is private to its own
/// module. Same wait-for-admission loop.
async fn next_watchdog_child(
    campaign: &mut TestCampaign,
) -> exomonad_actor::LocalResidentInstallation {
    let child = campaign
        .next_deployment(
            "watchdog child admission",
            Duration::from_secs(180),
            |event| match event {
                LocalResidentDeployment::PolicyInstalled(child) => Ok(*child),
                LocalResidentDeployment::Retired { actor, terminal } => {
                    panic!("{actor:?} retired: {terminal:?}")
                }
                other => Err(other),
            },
        )
        .await;
    campaign.authority.install_grant(
        child.actor.identity().into(),
        worktree_grant(child.effective_role.role()),
    );
    child.fork_gate.as_ref().unwrap().mark_ready().unwrap();
    child
}

async fn watchdog_probe(policy: &dyn exomonad_actor::ResidentToolEndpoint, topic: &str) -> String {
    dispatch_structured_tool(policy, "probe", serde_json::json!({"topic": topic}))
        .await
        .to_string()
}

/// A child's own admission and turn-activation traffic (`SessionReady`,
/// `ChildExited`, …) shares this campaign's one deployment channel with a
/// watchdog's `NotificationSend`, and the two interleave in whatever order
/// the resident host happens to schedule them. Waits up to `budget` for the
/// next `NotificationSend` specifically; any other event seen along the way
/// is parked on the campaign, not dropped, so a later wait can still find
/// it. `None` means no `NotificationSend` arrived inside the budget (used to
/// assert silence).
async fn next_notification_send(
    campaign: &mut TestCampaign,
    budget: Duration,
) -> Result<Arc<exomonad_actor::NotificationSend>, String> {
    campaign
        .next_deployment_opt(budget, |event| match event {
            LocalResidentDeployment::NotificationSend(command) => Ok(command),
            other => Err(other),
        })
        .await
        .ok_or_else(|| {
            let pending = campaign.pending_kinds();
            if pending.is_empty() {
                "no event at all within the budget".to_string()
            } else {
                format!("no NotificationSend within the budget; still pending: {pending:?}")
            }
        })
}

#[tokio::test]
async fn a_childs_watchdog_slot_escalates_to_its_parent() {
    let backend = Arc::new(ScriptedNoulJev {
        requests: Mutex::new(Vec::new()),
        likelihood: 0.9,
    });
    let mut campaign = tokio::time::timeout(
        Duration::from_secs(120),
        TestCampaign::start_with_config(
            exomonad_actor::ResearchPolicy::default(),
            |admission| admission,
            |config| {
                config.jev = Some(Arc::clone(&backend) as exomonad_actor::JevBackendHandle);
                pinned_watchdog_workspace(config);
            },
        ),
    )
    .await
    .expect("campaign did not start within 120s");
    let root = campaign.root_installation.policy.clone();
    let root_identity = campaign.actor.identity();

    // Launch the escalation child and the nudge child from the root.
    let launch_escalate = {
        let root = root.clone();
        let script = watchdog_child_script("escalate-child");
        tokio::spawn(async move { dispatch_haskell_script(root.as_ref(), &script).await })
    };
    let escalate_child = next_watchdog_child(&mut campaign).await;
    assert_eq!(launch_escalate.await.unwrap()["status"], "committed");

    let launch_nudge = {
        let root = root.clone();
        let script = watchdog_child_script("nudge-child");
        tokio::spawn(async move { dispatch_haskell_script(root.as_ref(), &script).await })
    };
    let nudge_child = next_watchdog_child(&mut campaign).await;
    assert_eq!(launch_nudge.await.unwrap()["status"], "committed");

    // (a)+(b): an escalation heuristic trips on the labelled child. Its own
    // result carries the short escalation record, and the PARENT (the root)
    // actually receives a native message naming the reason and the child's
    // actor address — observed the same way `sendMessage` is proven to reach
    // its target elsewhere in this suite (see
    // `notification_admission_and_poll_preserve_typed_request_bindings`).
    // A judgment otherwise leaves almost no trace: capture what the child's
    // slot and its Jev call actually wrote to the trace, over the same
    // `#[tokio::test]` current-thread runtime the escalation itself runs on
    // (`tracing::subscriber::set_default`'s thread-local scope covers every
    // task polled on this thread for as long as the guard is held).
    let trace_log = tempfile::NamedTempFile::new().unwrap();
    let trace_subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .with_max_level(tracing::Level::DEBUG)
        .with_writer(std::sync::Mutex::new(trace_log.reopen().unwrap()))
        .finish();
    let trace_guard = tracing::subscriber::set_default(trace_subscriber);

    let escalate_policy = escalate_child.policy.clone();
    let escalate_call = tokio::spawn(async move {
        tokio::time::timeout(
            Duration::from_secs(120),
            watchdog_probe(escalate_policy.as_ref(), "anything"),
        )
        .await
        .expect("escalating probe did not answer within 120s")
    });
    let command = next_notification_send(&mut campaign, Duration::from_secs(120))
        .await
        .expect("the watchdog's escalation reaches the deployment channel");
    assert_eq!(command.owner(), escalate_child.actor.identity());
    assert_eq!(command.target(), root_identity);
    assert!(
        command.message().contains("out_of_scope"),
        "{}",
        command.message()
    );
    assert!(
        command
            .message()
            .contains(&escalate_child.actor.identity().id.0.to_string()),
        "the child's own actor id is in the note: {}",
        command.message()
    );
    let directory = tempfile::tempdir().unwrap();
    let inbox = ActorInbox::open(
        directory.path().join("rows"),
        directory.path().join("cursor"),
    )
    .unwrap();
    admit_notification(&command, "watchdog-inbox".into(), &inbox);
    let escalated_result = escalate_call.await.unwrap();
    assert!(
        escalated_result.contains("[after-tool]"),
        "{escalated_result}"
    );
    assert!(
        escalated_result.contains("escalated to your parent"),
        "{escalated_result}"
    );

    drop(trace_guard);
    let traced = std::fs::read_to_string(trace_log.path()).unwrap();
    // The Jev call the slot made: a compact `info` line, packet and answer
    // bodies only at `debug`.
    assert!(traced.contains("jev call packet"), "{traced}");
    assert!(traced.contains("jev call answer"), "{traced}");
    assert!(traced.contains("jev call answered"), "{traced}");
    // The slot invocation itself, with the outcome's reason text — the
    // likelihood-bearing judgment a live run could not previously answer for.
    assert!(traced.contains("after-tool slot invoked"), "{traced}");
    assert!(traced.contains("after_tool_slot"), "{traced}");
    assert!(traced.contains("out_of_scope"), "{traced}");
    assert!(traced.contains("escalated to your parent"), "{traced}");
    // The escalation itself: which child, and who it told.
    assert!(traced.contains("actor notification sent"), "{traced}");
    assert!(traced.contains("from_slot=true"), "{traced}");
    assert!(
        traced.contains(&escalate_child.actor.identity().id.0.to_string()),
        "the child's own actor id is traced: {traced}"
    );
    // The generic baseline: every effect the slot's body made (not only the
    // Jev call) gets a settlement line, real time, because a slot
    // invocation has no later cell receipt to batch one into.
    assert!(
        traced.matches("effect settled").count() >= 2,
        "actorContext and notify effects settle too, not only jev: {traced}"
    );

    // (c) a nudge heuristic trips on the OTHER labelled child: its own result
    // carries the advice, and the parent receives NOTHING — no
    // `NotificationSend` reaches the deployment channel at all.
    let nudged_result = tokio::time::timeout(
        Duration::from_secs(120),
        watchdog_probe(nudge_child.policy.as_ref(), "anything"),
    )
    .await
    .expect("nudged probe did not answer within 120s");
    assert!(nudged_result.contains("[after-tool]"), "{nudged_result}");
    assert!(
        nudged_result.contains("Read the earlier failure"),
        "{nudged_result}"
    );
    assert!(
        next_notification_send(&mut campaign, Duration::from_millis(500))
            .await
            .is_err(),
        "a nudge alone must never reach the parent"
    );

    // (d) the ROOT's own probe, same scripted answer: `monitorsFor` gives the
    // root no heuristics at all (its own path matches neither label), so the
    // slot abstains before ever asking Jev, and nothing is sent.
    let root_result = tokio::time::timeout(
        Duration::from_secs(120),
        watchdog_probe(root.as_ref(), "anything"),
    )
    .await
    .expect("root probe did not answer within 120s");
    assert!(!root_result.contains("[after-tool]"), "{root_result}");
    assert!(
        next_notification_send(&mut campaign, Duration::from_millis(500))
            .await
            .is_err(),
        "the root must never message a parent it does not have"
    );

    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

/// `Project.Watchdog.trivialCall`: a short, successful, plainly non-destructive
/// bash call is abstained on before 'Watchdog.watchBy' ever asks Jev — even
/// though the shipped template's own `monitorsFor` gives every actor,
/// including a root with no label, `Watchdog.coreHeuristics` (which is exactly
/// `[repeatingItself, destructiveCommand]`). A command whose text carries a
/// destructive token still reaches the battery, exit code 0 notwithstanding.
#[tokio::test]
async fn trivial_bash_call_abstains_before_jev_but_destructive_text_still_asks() {
    let backend = Arc::new(ScriptedNoulJev {
        requests: Mutex::new(Vec::new()),
        likelihood: 0.0,
    });
    let mut campaign = campaign_with(Arc::clone(&backend)).await;
    let policy = campaign.root_installation.policy.clone();

    async fn run_bash(
        campaign: &mut TestCampaign,
        policy: &Arc<dyn exomonad_actor::ResidentToolEndpoint>,
        cmd: &str,
        stdout: &str,
    ) -> serde_json::Value {
        let invoked_policy = Arc::clone(policy);
        let cmd = cmd.to_string();
        let mut invoked = tokio::spawn(async move {
            dispatch_structured_tool(
                invoked_policy.as_ref(),
                "bash",
                serde_json::json!({
                    "cmd": cmd,
                    "workdir": null,
                    "environment": null,
                    "memory_mib": null,
                    "tty": null,
                    "stdin": null,
                    "yield_time_ms": 30000,
                    "max_output_bytes": 2048,
                    "intent": null
                }),
            )
            .await
        });
        let commands = TestCommands::completed(stdout);
        let request = tokio::select! {
            request = backend_request(campaign) => request,
            result = &mut invoked => panic!("bash completed before requesting its command backend: {result:?}"),
        };
        request.supply(Ok(commands));
        let response = invoked.await.unwrap();
        assert_eq!(response["status"], "committed", "{response}");
        response
    }

    run_bash(&mut campaign, &policy, "ls", "file-a\nfile-b\n").await;
    assert!(
        backend.requests.lock().is_empty(),
        "a short, successful, non-destructive bash call must never ask Jev: {:?}",
        backend.requests.lock()
    );

    run_bash(
        &mut campaign,
        &policy,
        "rm -rf ./trivial-call-test-nonexistent",
        "",
    )
    .await;
    {
        let requests = backend.requests.lock();
        assert!(
            !requests.is_empty(),
            "a command whose text reads as destructive must still reach the heuristics battery"
        );
        assert!(
            requests
                .iter()
                .any(|r| r.to_string().contains("destructive_command")),
            "expected a destructive_command heuristic question among the requests: {requests:?}"
        );
    }

    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}
