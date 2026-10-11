//! Native reply settlement through the hosted workspace and provider boundary.

use super::*;
use test_campaign::{dispatch_haskell_script, dispatch_haskell_script_response, TestCampaign};
use tidepool_runtime::session::{WorkbenchRunStatus, WorkbenchTerminalTransfer};

#[tokio::test]
async fn request_local_reply_helper_settles_exact_request_and_keeps_actor_live() {
    TestCampaign::start()
        .await
        .run_scenario(|campaign| {
            Box::pin(async move {
                let root = campaign.root_installation.policy.clone();
                let (setup, child) = campaign
                    .dispatch_native_spawn_setup(
                        "terminal reply target installation",
                        include_str!("fixtures/terminal_transfer_setup.hs"),
                        Duration::from_secs(240),
                    )
                    .await;
                assert_eq!(setup["status"], "committed", "{setup}");
                assert!(child.worktree_custody.is_some());
                assert!(!child.launch_worktrees.is_empty());

                // The startup cell has committed and cleaned up before a new
                // cell submits and retains the request through the installed driver.
                let submitted = dispatch_haskell_script(
                    root.as_ref(),
                    include_str!("fixtures/terminal_transfer_request.hs"),
                )
                .await;
                assert_eq!(submitted["status"], "committed", "{submitted}");
                let activation = campaign
                    .await_native_activation(
                        child.actor.identity(),
                        "terminal reply request activation",
                        Duration::from_secs(240),
                    )
                    .await;
                assert_eq!(
                    campaign.forest.open_request_without_reply(child.actor.identity()),
                    Some(activation.request)
                );
                let watched = dispatch_haskell_script(
                    root.as_ref(),
                    "terminalWatch <- watch Nothing (result response)",
                )
                .await;
                assert_eq!(watched["status"], "committed", "{watched}");

                let exomonad_actor::ResidentToolResponse::Workbench(reply) =
                    dispatch_haskell_script_response(child.policy.as_ref(), "respond (42 :: Int)")
                        .await
                        .expect("request-local helper settles its actual typed request")
                else {
                    panic!("native reply must retain its typed workbench receipt");
                };
                assert_eq!(reply.status, WorkbenchRunStatus::Replied, "{reply:?}");
                assert_eq!(
                    reply.items.iter().filter(|receipt| {
                        receipt.terminal_transfer == Some(WorkbenchTerminalTransfer::ReplyAccepted)
                    }).count(),
                    1,
                    "exactly one native terminal transfer"
                );
                campaign.await_watch_ready().await;
                assert_eq!(
                    campaign.forest.open_request_without_reply(child.actor.identity()),
                    None
                );
                let store = display_output::open_run_store(campaign.session_root.path()).unwrap();
                let observed = campaign.drive_actor_output(&store, dispatch_haskell_script(
                    root.as_ref(),
                    "settled <- pollResponse response\ndisplay (case settled of { ResponseReady answer -> responseValue answer == 42; _ -> False })",
                )).await;
                assert_eq!(test_campaign::committed_display_text(&observed), "True");
                assert!(child.actor.terminal().get().is_none(), "reply keeps receiver live");
                assert!(campaign.actor.terminal().get().is_none(), "requester remains live");
                let continued = dispatch_haskell_script(child.policy.as_ref(), "pure True").await;
                assert_eq!(continued["status"], "committed", "{continued}");
                let outcomes = campaign.observe_forest_shutdown().await;
                assert!(!outcomes.is_empty());
                assert!(outcomes.iter().all(exomonad_actor::ForestRootShutdown::is_confirmed), "{outcomes:?}");
                assert!(child.actor.terminal().cleanup().expect("retained child cleanup").is_confirmed());
            })
        })
        .await;
}
