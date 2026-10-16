use super::command_jobs_tests::backend_request;
use super::command_test_support::TestCommands;
use super::test_campaign::{original_tool_call, TestCampaign};
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
            "exomonad/host/src/actor_host/invocation_record_service.hs",
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

#[tokio::test]
async fn after_tool_completion_waits_and_retires_exact_worker_without_replay() {
    let campaign = start_with_after_tool_worker().await;
    let (releases, backend) = campaign
        .run_scenario(|campaign| {
            Box::pin(async move {
                campaign.forest.track_resource_release();
                let root = campaign.root_installation.policy.clone();
                // The command backend controls hook completion independently of timing.
                let policy = root.clone();
                let called = tokio::spawn(async move {
                    let (invocation, completion) =
                        original_tool_call(exomonad_tool::OriginalOperation {
                            origin: exomonad_tool::ConversationOrigin::External {
                                thread_id: "after-tool-retained-cleanup".into(),
                            },
                            request_id: "retained-probe".into(),
                            call_id: "retained-probe".into(),
                        });
                    let result = policy
                        .dispatch_json_boxed(exomonad_tool::ToolInvocation {
                            context: Some(invocation),
                            name: "lifetime_probe".into(),
                            arguments: exomonad_tool::ToolArguments::Structured(
                                serde_json::json!({"topic": "lifecycle"}),
                            ),
                        })
                        .await
                        .expect("retained cleanup preserves tool success");
                    (result, completion)
                });
                let child = campaign
                    .next_deployment(
                        "invocation worker policy publication before presentation completes",
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
                campaign.acknowledge_native_spawn(&child);
                assert_eq!(child.creator, Some(campaign.actor.identity()));
                assert_eq!(child.supervisor_parent, Some(campaign.actor.identity()));
                assert!(child.actor.terminal().get().is_none());
                let request = backend_request(campaign).await;
                assert_eq!(request.owner, campaign.actor.identity());
                let backend = TestCommands::new();
                request.supply(Ok(backend.clone()));
                assert!(!called.is_finished(), "final result awaits hook effects");
                backend.finish();
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
                let (response, completion) = tokio::time::timeout(Duration::from_secs(30), called)
                    .await
                    .expect("retained host cleanup must settle the tool response")
                    .unwrap();
                assert_eq!(response["status"], "committed", "{response}");
                let rendered = response.to_string();
                assert!(rendered.contains("original-result"), "{response}");
                assert!(!rendered.contains("the slot did not answer"), "{response}");
                assert!(rendered.contains("slot-finished"), "{response}");
                assert!(
                    rendered.contains("Invocation cleanup remains unconfirmed"),
                    "{response}"
                );
                assert!(rendered.contains(RETAINED), "{response}");
                assert!(rendered.contains(&format!("{exact:?}")), "{response}");
                assert_eq!(backend.executions(), 1);
                assert_eq!(
                    backend.control_count(),
                    0,
                    "completed hook command is never cancelled or replayed"
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
                let store =
                    super::display_output::open_run_store(campaign.session_root.path()).unwrap();
                let available = campaign
                    .drive_actor_output(
                        &store,
                        dispatch_haskell_script(root.as_ref(), "display (40 + 2 :: Int)"),
                    )
                    .await;
                assert_eq!(
                    super::test_campaign::committed_display_text(&available),
                    "42",
                    "{available}"
                );

                // Completion retries the same external owner. Retirement can release
                // it only after the retained refusal has been observed.
                campaign.drain_ready();
                let mut deployments = campaign.take_deployments();
                let released = Arc::new(std::sync::atomic::AtomicBool::new(false));
                let release_state = released.clone();
                let releases = tokio::spawn(async move {
                    while let Some(event) = deployments.recv().await {
                        if let LocalResidentDeployment::ReleaseAwait(release) = event {
                            assert_eq!(release.actor, exact);
                            let outcome =
                                if release_state.load(std::sync::atomic::Ordering::Acquire) {
                                    exomonad_actor::ResourceRelease::Released
                                } else {
                                    exomonad_actor::ResourceRelease::Retained(RETAINED.into())
                                };
                            assert!(release.answer(outcome));
                        }
                    }
                });
                let completion = root
                    .complete_boxed(completion)
                    .await
                    .expect_err("retained external owner keeps invocation cleanup unconfirmed");
                assert!(matches!(
                    completion,
                    exomonad_actor::ResidentToolError::Unavailable(_)
                ));
                released.store(true, std::sync::atomic::Ordering::Release);
                (releases, backend)
            })
        })
        .await;
    releases.abort();
    if let Err(error) = releases.await {
        assert!(
            error.is_cancelled(),
            "cleanup responder must not panic: {error}"
        );
    }
    assert_eq!(
        backend.executions(),
        1,
        "retirement must never replay hook effects"
    );
}

async fn start_with_after_tool_worker() -> TestCampaign {
    TestCampaign::start_with_config(
        |admission| admission,
        |config| {
            let authored = config.workspace.join(".exomonad");
            std::fs::create_dir_all(&authored).unwrap();
            std::fs::write(
                authored.join("AgentSpec.hs"),
                tidepool_testing::fixture_source(
                    "exomonad/host/src/actor_host/invocation_deadline_spec.hs",
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
    .await
}

#[tokio::test]
async fn runtime_shutdown_cleans_up_a_pending_after_tool_without_replay() {
    let campaign = start_with_after_tool_worker().await;
    let (called, backend, child) = campaign
        .run_scenario(|campaign| {
            Box::pin(async move {
                let policy = campaign.root_installation.policy.clone();
                let (invocation, _completion) =
                    original_tool_call(exomonad_tool::OriginalOperation {
                        origin: exomonad_tool::ConversationOrigin::External {
                            thread_id: "after-tool-shutdown".into(),
                        },
                        request_id: "shutdown-probe".into(),
                        call_id: "shutdown-probe".into(),
                    });
                let observed_invocation = invocation.clone();
                let observer = policy.clone();
                let called = tokio::spawn(async move {
                    policy
                        .dispatch_json_boxed(exomonad_tool::ToolInvocation {
                            context: Some(invocation),
                            name: "lifetime_probe".into(),
                            arguments: exomonad_tool::ToolArguments::Structured(
                                serde_json::json!({"topic": "lifecycle"}),
                            ),
                        })
                        .await
                });
                let child = campaign
                    .next_deployment(
                        "hook's invocation-owned worker",
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
                campaign.acknowledge_native_spawn(&child);
                let request = backend_request(campaign).await;
                let backend = TestCommands::new();
                request.supply(Ok(backend.clone()));
                tokio::time::timeout(Duration::from_secs(30), async {
                    while backend.executions() == 0 {
                        let terminal = observer
                            .retained_operation(observed_invocation.clone())
                            .expect("admitted operation retains its native owner")
                            .terminal();
                        assert!(
                            !matches!(
                                terminal,
                                exomonad_actor::HostedOperationTerminal::Settled(_)
                            ),
                            "hook invocation settled before its command started"
                        );
                        tokio::time::sleep(Duration::from_millis(5)).await;
                    }
                })
                .await
                .expect("hook command starts before runtime shutdown");
                drop(observer);
                assert!(
                    !called.is_finished(),
                    "pending hook cannot deliver final output"
                );
                (called, backend, child)
            })
        })
        .await;
    let response = called.await.expect("dispatch task joins without a panic");
    if let Ok(response) = response {
        assert!(
            !response.to_string().contains("slot-finished"),
            "{response}"
        );
    }
    assert_eq!(
        backend.executions(),
        1,
        "shutdown must never replay hook effects"
    );
    assert_eq!(
        backend.control_count(),
        1,
        "unfinished hook command is cancelled by shutdown"
    );
    assert!(child.actor.terminal().get().is_some());
    assert!(child.actor.terminal().cleanup().unwrap().is_confirmed());
}
