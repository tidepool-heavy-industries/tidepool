use super::command_jobs_tests::backend_request;
use super::command_test_support::TestCommands;
use super::test_campaign::TestCampaign;
use super::tests::{dispatch_haskell_script, dispatch_structured_tool};
use super::*;

#[tokio::test]
async fn record_service_survives_tool_return_and_runs_commands_in_its_handler() {
    let campaign = TestCampaign::start().await;
    campaign.run_scenario(|campaign| Box::pin(async move {
    let root = campaign.root_installation.policy.clone();
    let setup = dispatch_haskell_script(
        root.as_ref(),
        &tidepool_testing::fixture_source(
            "bridge/facade/src/actor_host/invocation_record_service.hs",
        ),
    )
    .await;
    assert_eq!(setup["status"], "committed", "{setup}");
    let address = setup["items"].as_array().unwrap().last().unwrap()["output"]
        .as_str()
        .unwrap()
        .trim_matches(['(', ')'])
        .split(',')
        .map(|part| part.trim().parse::<u64>().unwrap())
        .collect::<Vec<_>>();
    let record = ActorRef {
        id: exomonad_actor::ActorId(address[0]),
        incarnation: exomonad_actor::Incarnation(address[1]),
    };
    let owner = campaign.actor.identity();
    let membership = dispatch_haskell_script(
        root.as_ref(),
        &format!(
            "entries <- findAgentsByLabel \"invocation-persistent-counter\"\n\
             case entries of {{ [entry] -> rosterState entry == RosterRunning && \
             rosterActorId entry == {} && rosterActorIncarnation entry == {} && \
             rosterCreatorId entry == Just {} && rosterCreatorIncarnation entry == Just {}; _ -> False }}",
            record.id.0, record.incarnation.0, owner.id.0, owner.incarnation.0,
        ),
    )
    .await;
    assert_eq!(membership["items"][1]["output"], "True", "{membership}");

    let policy = root.clone();
    let called = tokio::spawn(async move {
        dispatch_haskell_script(
            policy.as_ref(),
            "value <- R.call (advanceCounter (R.client counterService)) 35\n\
             output <- R.call (runHandlerCommand (R.client counterService)) ()\n\
             value == 42 && output == \"record-handler-command\"",
        )
        .await
    });
    let request = backend_request(campaign).await;
    assert_eq!(
        request.owner, record,
        "the record actor owns handler commands"
    );
    let backend = TestCommands::completed("record-handler-command");
    request.supply(Ok(backend.clone()));
    let called = called.await.unwrap();
    assert_eq!(called["status"], "committed", "{called}");
    assert_eq!(called["items"][2]["output"], "True", "{called}");
    assert_eq!(backend.executions(), 1);
    let finished = dispatch_haskell_script(
        root.as_ref(),
        "finished <- R.finish counterService\nfinished == Actor.Completed 42",
    )
    .await;
    assert_eq!(finished["items"][1]["output"], "True", "{finished}");
})).await;
}

struct AfterToolDeadline(Option<std::ffi::OsString>);

impl AfterToolDeadline {
    fn install(milliseconds: u64) -> Self {
        let previous = std::env::var_os(exomonad_actor::AFTER_TOOL_WAIT_ENV);
        std::env::set_var(
            exomonad_actor::AFTER_TOOL_WAIT_ENV,
            milliseconds.to_string(),
        );
        Self(previous)
    }
}

impl Drop for AfterToolDeadline {
    fn drop(&mut self) {
        match self.0.take() {
            Some(previous) => std::env::set_var(exomonad_actor::AFTER_TOOL_WAIT_ENV, previous),
            None => std::env::remove_var(exomonad_actor::AFTER_TOOL_WAIT_ENV),
        }
    }
}

#[tokio::test]
async fn after_tool_deadline_retires_exact_invocation_worker_and_retains_host_uncertainty() {
    let campaign = TestCampaign::start_with_config(
        |admission| admission,
        |config| {
            let authored = config.workspace.join(".exomonad");
            std::fs::create_dir_all(&authored).unwrap();
            std::fs::write(
                authored.join("AgentSpec.hs"),
                tidepool_testing::fixture_source(
                    "bridge/facade/src/actor_host/invocation_deadline_spec.hs",
                ),
            )
            .unwrap();
            crate::exomonad::write_fixture_project_config(&authored, "gpt-6-sol", |project| {
                project.haskell.source_roots = vec![".".into()];
                project.haskell.modules = vec!["AgentSpec".into()];
                project.haskell.spec = Some("AgentSpec.agentSpec".into());
            });
            config.workspace_inputs = Some(
                crate::exomonad::workspace::FrozenWorkspace::load(
                    &config.workspace,
                    &config.run_directory.path(),
                )
                .unwrap(),
            );
        },
    )
    .await;
    campaign
        .run_scenario_expecting_cleanup_failure(|campaign| {
            Box::pin(async move {
                campaign.forest.track_resource_release();
                let root = campaign.root_installation.policy.clone();
                // The root spec is already compiled. The parked command prevents a
                // successful slot answer, independently of command execution timing.
                let deadline = AfterToolDeadline::install(10_000);
                let policy = root.clone();
                let called = tokio::spawn(async move {
                    dispatch_structured_tool(
                        policy.as_ref(),
                        "lifetimeProbe",
                        serde_json::Value::Null,
                    )
                    .await
                });
                let child = campaign
                    .next_deployment(
                        "invocation worker policy publication before slot expiry",
                        Duration::from_secs(120),
                        |event| match event {
                            LocalResidentDeployment::PolicyInstalled(child)
                                if child.label == "invocation-deadline-child" =>
                            {
                                Ok(child)
                            }
                            other => Err(other),
                        },
                    )
                    .await;
                let exact = child.actor.identity();
                assert_eq!(child.creator, Some(campaign.actor.identity()));
                assert_eq!(child.supervisor_parent, Some(campaign.actor.identity()));
                assert!(child.actor.terminal().get().is_none());
                let request = backend_request(campaign).await;
                assert_eq!(request.owner, campaign.actor.identity());
                let backend = TestCommands::new();
                request.supply(Ok(backend.clone()));
                let release = campaign
                    .next_deployment(
                        "host cleanup for the exact invocation-owned worker",
                        Duration::from_secs(120),
                        |event| match event {
                            LocalResidentDeployment::ReleaseAwait(release) => Ok(release),
                            other => Err(other),
                        },
                    )
                    .await;
                assert_eq!(release.actor, exact);
                let terminal = child
                    .actor
                    .terminal()
                    .get()
                    .expect("worker stopped before host release");
                assert_eq!(terminal.kind, ActorExitKind::Cancelled);
                assert!(child.actor.terminal().cleanup().unwrap().is_confirmed());
                assert!(
                    !called.is_finished(),
                    "scope exit still owes external cleanup evidence"
                );
                const RETAINED: &str = "scripted provider installation remains published";
                assert!(release.answer(exomonad_actor::ResourceRelease::Retained(RETAINED.into())));
                let response = tokio::time::timeout(Duration::from_secs(30), called)
                    .await
                    .expect("retained host cleanup must settle the tool response")
                    .unwrap();
                drop(deadline);
                assert_eq!(response["status"], "committed", "{response}");
                let rendered = response.to_string();
                assert!(rendered.contains("original-result"), "{response}");
                assert!(rendered.contains("no answer within 10s"), "{response}");
                assert!(!rendered.contains("slot-finished"), "{response}");
                assert!(
                    rendered.contains("Invocation cleanup remains unconfirmed"),
                    "{response}"
                );
                assert!(rendered.contains(RETAINED), "{response}");
                assert!(rendered.contains(&format!("{exact:?}")), "{response}");
                assert_eq!(backend.executions(), 1);
                assert_eq!(
                    backend.control_count(),
                    1,
                    "unfinished slot command cancelled once"
                );
                let retired = campaign
                    .next_deployment(
                        "exact worker's retained retirement",
                        Duration::from_secs(30),
                        |event| match event {
                            LocalResidentDeployment::Retired { actor, terminal }
                                if actor == exact =>
                            {
                                Ok(terminal)
                            }
                            other => Err(other),
                        },
                    )
                    .await;
                assert_eq!(retired, terminal);
                let status = dispatch_structured_tool(
                    root.as_ref(),
                    "status",
                    serde_json::json!({"view":"live"}),
                )
                .await;
                assert!(status.to_string().contains(RETAINED), "{status}");
                assert!(
                    status
                        .to_string()
                        .contains("invocation cleanup unconfirmed"),
                    "{status}"
                );
                let available = dispatch_haskell_script(root.as_ref(), "40 + 2 :: Int").await;
                assert_eq!(available["items"][0]["output"], "42", "{available}");

                // Retirement retries the same retained invocation's cleanup. An external
                // owner still retaining resources must keep root cleanup unconfirmed.
                campaign.drain_ready();
                let mut deployments = campaign.take_deployments();
                let releases = tokio::spawn(async move {
                    while let Some(event) = deployments.recv().await {
                        if let LocalResidentDeployment::ReleaseAwait(release) = event {
                            assert_eq!(release.actor, exact);
                            release
                                .answer(exomonad_actor::ResourceRelease::Retained(RETAINED.into()));
                        }
                    }
                });
                campaign.observe_shutdown().await.expect_err("retained external owner keeps cleanup unconfirmed");
                releases.abort();
                assert!(releases.await.unwrap_err().is_cancelled());
                assert!(!campaign.actor.terminal().cleanup().unwrap().is_confirmed());
            })
        }, |failure| {
            assert_eq!(failure.hosted, super::test_campaign::CampaignHostedJoin::Joined);
            assert!(matches!(&failure.root, super::test_campaign::CampaignRootRetirement::Settled(shutdown) if !shutdown.cleanup.is_confirmed()));
        })
        .await;
}
