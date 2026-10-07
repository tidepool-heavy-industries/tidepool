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
    let policy = campaign.root_installation.policy.as_ref();

    let setup = dispatch_haskell_script(policy, include_str!("lookup_availability_setup.hs")).await;
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

    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}
