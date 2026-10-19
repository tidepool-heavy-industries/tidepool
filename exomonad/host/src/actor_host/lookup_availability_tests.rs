use super::test_campaign::{
    configure_notebook_lookup_workspace, dispatch_haskell_script, dispatch_lookup, TestCampaign,
};

#[tokio::test]
async fn hosted_lookup_uses_actual_actor_row_for_constraint_availability() {
    let campaign = TestCampaign::start_with_config(
        |admission| admission,
        |config| {
            configure_notebook_lookup_workspace(config);
        },
    )
    .await;
    campaign.run_scenario(|campaign| Box::pin(async move {
    let policy = campaign.root_installation.policy.as_ref();

    let setup = dispatch_haskell_script(
        policy,
        &tidepool_testing::fixture_source(
            "exomonad/host/src/actor_host/lookup_availability_setup.hs",
        ),
    )
    .await;
    assert_eq!(setup["status"], "committed", "{setup:?}");
    let names = dispatch_lookup(
        policy,
        &[
            "pollResponse",
            "sleep",
            "LookupEffects.readFile",
            "installSpec",
        ],
    )
    .await;
    assert_eq!(names["status"], "committed", "{names:?}");
    let output = names["items"][0]["output"].as_str().unwrap();
    for (name, availability) in [
        ("pollResponse", "available"),
        ("sleep", "available"),
        ("LookupEffects.readFile", "unavailable"),
        ("installSpec", "unknown"),
    ] {
        let block = output
            .split("\n\n")
            .find(|block| block.starts_with(&format!("{name}\n")))
            .unwrap_or_else(|| panic!("missing {name} lookup block: {output}"));
        assert!(
            block.contains(&format!("[{availability}]")),
            "{name} has wrong availability: {block}"
        );
    }

    let matches = dispatch_lookup(
        policy,
        &[
            ":: Response result -> Eff effects (ResponseState result)",
            ":: FilePath -> Eff effects Bool",
        ],
    )
    .await;
    assert_eq!(matches["status"], "committed", "{matches:?}");
    let matches = matches["items"][0]["output"].as_str().unwrap();
    assert!(
        matches
            .lines()
            .any(|line| line.contains("[available] pollResponse ::")),
        "{matches}"
    );
    assert!(
        matches
            .lines()
            .any(|line| line.contains("[unavailable]") && line.contains("doesFileExist ::")),
        "{matches}"
    );

    let type_search = dispatch_lookup(policy, &[":: Int -> Int"]).await;
    assert_eq!(type_search["status"], "committed", "{type_search:?}");
    let type_output = type_search["items"][0]["output"].as_str().unwrap();
    assert!(
        type_output.contains(":: Int -> Int")
            && (type_output.contains("[available]") || type_output.contains("[polymorphic]"))
            && !type_output.contains("no match:")
            && !type_output.contains("error:"),
        "the hosted type-search query must return matches through the captured compiler Id: {type_output}"
    );
})).await;
}

#[tokio::test]
async fn hosted_lookup_binding_keeps_a_pure_second_cell_compilable() {
    let campaign =
        TestCampaign::start_with_config(|admission| admission, configure_notebook_lookup_workspace)
            .await;
    campaign
        .run_scenario(|campaign| {
            Box::pin(async move {
                let policy = campaign.root_installation.policy.clone();
                let store = super::display_output::open_run_store(campaign.session_root.path())
                    .expect("hosted output store");
                let first = campaign
                    .drive_actor_output(
                        &store,
                        dispatch_haskell_script(
                            policy.as_ref(),
                            "import qualified Tidepool.Lookup as LookupApi\n\
                             import qualified Tidepool.Lookup.Tools as LookupTools\n\
                             topics <- LookupApi.lookupRaw (LookupApi.lookupRequest [\"doc workbench\"])\n\
                             display (LookupTools.renderBatch topics)",
                        ),
                    )
                    .await;
                assert_eq!(first["status"], "committed", "{first:?}");
                assert!(
                    !super::test_campaign::committed_display_text(&first).is_empty(),
                    "the first cell must execute lookup and publish its result: {first:?}"
                );

                let second = campaign
                    .drive_actor_output(
                        &store,
                        dispatch_haskell_script(policy.as_ref(), "display (1 + 1 :: Int)"),
                    )
                    .await;
                assert_eq!(
                    super::test_campaign::committed_display_text(&second),
                    "2",
                    "the independent pure cell must compile after lookup: {second:?}"
                );
            })
        })
        .await;
}
