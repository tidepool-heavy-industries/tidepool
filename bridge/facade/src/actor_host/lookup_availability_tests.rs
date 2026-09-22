use super::test_campaign::TestCampaign;
use super::tests::{dispatch_haskell_script, dispatch_lookup};

#[tokio::test]
async fn hosted_lookup_uses_actual_actor_row_for_constraint_availability() {
    let campaign = TestCampaign::start().await;
    let policy = campaign.root_installation.policy.as_ref();

    let setup = dispatch_haskell_script(policy, include_str!("lookup_availability_setup.hs")).await;
    assert_eq!(setup["status"], "committed", "{setup:?}");
    let names = dispatch_lookup(
        policy,
        &[
            "pollResponse",
            "sleep",
            "LookupEffects.readFile",
            "installTools",
        ],
    )
    .await;
    assert_eq!(names["status"], "committed", "{names:?}");
    let output = names["items"][0]["output"].as_str().unwrap();
    for (name, availability) in [
        ("pollResponse", "available"),
        ("sleep", "available"),
        ("LookupEffects.readFile", "unavailable"),
        ("installTools", "unknown"),
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

    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}
