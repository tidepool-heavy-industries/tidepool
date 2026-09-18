//! Reloading workspace source from inside a live notebook.
//!
//! The reload mechanism's own transaction — identity, the rebuilt
//! reverse-dependency closure, and what a rejected candidate leaves behind —
//! is checked in `crate::shoal::source`. What is checked HERE is the property
//! only a live session can answer: a later cell compiles against the published
//! revision, while a value bound before the reload keeps the code it was built
//! from.

use super::test_campaign::TestCampaign;
use super::tests::dispatch_haskell_script;

async fn committed(
    policy: &dyn tidepool_actor::ResidentToolEndpoint,
    source: &str,
) -> serde_json::Value {
    let result = dispatch_haskell_script(policy, source).await;
    assert_eq!(result["status"], "committed", "{result:?}");
    result
}

fn work_module(answer: u32) -> String {
    format!("module Project.Work (answer) where\n\nanswer :: Int\nanswer = {answer}\n")
}

/// The whole point, end to end: write a module, reload, and the next cell has
/// it — without restarting, and without rewriting work already in flight.
#[tokio::test]
async fn a_reloaded_module_reaches_later_cells_and_leaves_bindings_alone() {
    let campaign = TestCampaign::start_with_config(
        tidepool_actor::ResearchPolicy::default(),
        |admission| admission,
        |config| {
            let authored = config.workspace.join(".shoal");
            std::fs::create_dir_all(authored.join("Project")).unwrap();
            std::fs::write(
                authored.join("config.toml"),
                "[defaults]\nmodel = 'gpt-5.6-sol'\n[haskell]\nsource_roots = ['.']\nmodules = ['Project.Work']\n",
            )
            .unwrap();
            std::fs::write(authored.join("Project/Work.hs"), work_module(1)).unwrap();
            config.workspace_inputs = Some(
                crate::shoal::workspace::FrozenWorkspace::load(&config.workspace, &config.run_root)
                    .unwrap(),
            );
        },
    )
    .await;
    let workspace = campaign._repository.path().to_path_buf();
    let policy = campaign.root_installation.policy.clone();
    let policy = policy.as_ref();

    // A value bound while the first revision is active.
    let bound = committed(policy, "saved <- pure answer\ninspectFull saved").await;
    assert_eq!(bound["items"][1]["output"], "1", "{bound}");

    // Nothing is waiting yet.
    let status = committed(
        policy,
        "Right before <- sourceStatus\ninspectFull (revisionIdentity (statusActive before) == revisionIdentity (statusDisk before))",
    )
    .await;
    assert_eq!(status["items"][1]["output"], "True", "{status}");

    // Edit the module with ordinary file tools, then reload from a cell.
    std::fs::write(workspace.join(".shoal/Project/Work.hs"), work_module(41)).unwrap();
    let reloaded = committed(
        policy,
        "Right outcome <- reloadSource []\ninspectFull (case outcome of { ReloadPublished _ _ changed -> changed; _ -> [\"not published\"] })",
    )
    .await;
    assert_eq!(
        reloaded["items"][1]["output"], "[Project.Work]",
        "{reloaded}"
    );

    // A LATER cell compiles against the published revision…
    let later = committed(policy, "inspectFull answer").await;
    assert_eq!(later["items"][0]["output"], "41", "{later}");

    // …and the binding made before the reload still holds the code it was
    // built from. No implicit rewriting of running work.
    let kept = committed(policy, "inspectFull saved").await;
    assert_eq!(kept["items"][0]["output"], "1", "{kept}");

    // Provenance is data: active now names the published revision, and the
    // module's digest moved with it.
    let after = committed(
        policy,
        "Right now <- sourceStatus\ninspectFull (revisionGeneration (statusActive now))\ninspectFull (revisionIdentity (statusActive now) == revisionIdentity (statusDisk now))",
    )
    .await;
    assert_eq!(after["items"][1]["output"], "2", "{after}");
    assert_eq!(after["items"][2]["output"], "True", "{after}");

    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

/// A reload whose affected set does not typecheck is an ordinary result a
/// program handles: it names the snapshot that failed, the previous graph is
/// still what later cells compile against, and the edited file is untouched.
#[tokio::test]
async fn a_rejected_reload_is_a_value_and_leaves_the_notebook_running() {
    let campaign = TestCampaign::start_with_config(
        tidepool_actor::ResearchPolicy::default(),
        |admission| admission,
        |config| {
            let authored = config.workspace.join(".shoal");
            std::fs::create_dir_all(authored.join("Project")).unwrap();
            std::fs::write(
                authored.join("config.toml"),
                "[defaults]\nmodel = 'gpt-5.6-sol'\n[haskell]\nsource_roots = ['.']\nmodules = ['Project.Work']\n",
            )
            .unwrap();
            std::fs::write(authored.join("Project/Work.hs"), work_module(1)).unwrap();
            config.workspace_inputs = Some(
                crate::shoal::workspace::FrozenWorkspace::load(&config.workspace, &config.run_root)
                    .unwrap(),
            );
        },
    )
    .await;
    let workspace = campaign._repository.path().to_path_buf();
    let policy = campaign.root_installation.policy.clone();
    let policy = policy.as_ref();

    let broken =
        "module Project.Work (answer) where\n\nanswer :: Int\nanswer = undefinedByThisReload\n";
    std::fs::write(workspace.join(".shoal/Project/Work.hs"), broken).unwrap();
    let rejected = committed(
        policy,
        "Right outcome <- reloadSource []\ninspectFull (case outcome of { ReloadRejected active failed _ -> revisionIdentity active /= revisionIdentity failed; _ -> False })\ninspectFull (case outcome of { ReloadRejected _ _ detail -> \"undefinedByThisReload\" `T.isInfixOf` detail; _ -> False })",
    )
    .await;
    assert_eq!(
        rejected["items"][1]["output"], "True",
        "the receipt names the snapshot that failed: {rejected}"
    );
    assert_eq!(
        rejected["items"][2]["output"], "True",
        "the receipt carries the diagnostics: {rejected}"
    );

    // The previously compiled graph is still what a later cell gets.
    let later = committed(policy, "inspectFull answer").await;
    assert_eq!(later["items"][0]["output"], "1", "{later}");

    // The edited source stays exactly as it was written.
    assert_eq!(
        std::fs::read_to_string(workspace.join(".shoal/Project/Work.hs")).unwrap(),
        broken
    );

    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}
