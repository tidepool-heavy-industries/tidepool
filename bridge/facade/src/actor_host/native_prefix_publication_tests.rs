//! Native publication keeps completed binding prefixes and private declaration dependencies.

use super::test_campaign::{dispatch_haskell_script, dispatch_haskell_script_result, TestCampaign};

async fn committed(
    policy: &dyn exomonad_actor::ResidentToolEndpoint,
    source: &str,
) -> serde_json::Value {
    let result = dispatch_haskell_script(policy, source).await;
    assert_eq!(result["status"], "committed", "{result}");
    result
}

#[tokio::test]
async fn notebook_failed_cells_preserve_completed_native_prefix() {
    let campaign = TestCampaign::start_with_config(
        |admission| admission,
        |_| {},
    )
    .await;
    let policy = campaign.root_installation.policy.as_ref();
    // The native prefix must replace a public declaration without publishing
    // the failed cell's new private declaration suffix.
    committed(policy, include_str!("notebook_prefix_baseline.hs")).await;
    let failed = dispatch_haskell_script_result(policy, include_str!("notebook_prefix_failure.hs"))
        .await
        .expect_err("the pattern match fails after completed native bindings");
    let exomonad_actor::ResidentToolError::Invocation(
        exomonad_actor::KernelInvocationFailure::Workbench(failure),
    ) = failed
    else {
        panic!("native failure classification required: {failed}");
    };
    assert_eq!(
        failure
            .diagnostic
            .as_ref()
            .expect("native failure diagnostic")
            .phase,
        tidepool_toolchain::failclass::Phase::Run,
    );
    let Some(tidepool_runtime::session::WorkbenchPublicationOutcome::Published { bindings }) =
        &failure.publication
    else {
        panic!("completed native prefix publication required: {failure}");
    };
    assert!(
        !bindings
            .iter()
            .any(|binding| binding == "privatePrefixHelper"),
        "private declaration falsely reported as published: {failure}"
    );
    assert!(
        failure.receipts.iter().any(|receipt| receipt.kind
            == Some(tidepool_runtime::session::WorkbenchCellItemKind::Declaration)),
        "error receipts must retain checked declaration metadata: {failure}"
    );
    for binding in ["actorsBeforeFailure", "prefixGetter", "prefixValue"] {
        assert!(
            failure.receipts.iter().any(|receipt| receipt
                .installed_bindings
                .iter()
                .any(|name| name == binding)),
            "{failure}"
        );
    }
    let recovered = committed(policy, "prefixValue").await;
    assert_eq!(recovered["items"][0]["output"], "41", "{recovered}");
    let retained = committed(policy, "prefixGetter 41").await;
    assert_eq!(retained["items"][0]["output"], "42", "{retained}");
    for binding in ["privatePrefixHelper", "impossible", "tailValue"] {
        let missing = dispatch_haskell_script(policy, binding).await;
        assert_eq!(missing["status"], "rejected", "{missing}");
        assert!(missing.to_string().contains("not in scope"), "{missing}");
    }
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}
