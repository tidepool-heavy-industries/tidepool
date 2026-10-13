//! Source reload through the live actor: changed source starts a fresh
//! notebook while issued requests and children retain their exact source.
//! Capsule capture and recovery histories live in `crate::exomonad::source`.
#![allow(
    clippy::disallowed_methods,
    reason = "test: launches real tmux/process fixtures directly, not through the production launcher"
)]

use std::path::Path;

use super::test_campaign::TestCampaign;
use super::tests::{dispatch_haskell_script, dispatch_structured_tool};
use super::*;

async fn committed(
    policy: &dyn exomonad_actor::ResidentToolEndpoint,
    source: &str,
) -> serde_json::Value {
    let result = dispatch_haskell_script(policy, source).await;
    assert_eq!(result["status"], "committed", "{result:?}");
    result
}

async fn displayed(
    campaign: &mut TestCampaign,
    policy: &dyn exomonad_actor::ResidentToolEndpoint,
    source: &str,
) -> serde_json::Value {
    let store = super::display_output::open_run_store(campaign.session_root.path()).unwrap();
    campaign
        .drive_actor_output(&store, committed(policy, source))
        .await
}

fn work_module(answer: u32) -> String {
    format!("module Project.Work (answer) where\n\nanswer :: Int\nanswer = {answer}\n")
}

/// The authored package a source-reload campaign starts from: one configured
/// module, answering `1`.
fn write_workspace(workspace: &Path, answer: u32) {
    let authored = workspace.join(".exomonad");
    std::fs::create_dir_all(authored.join("Project")).unwrap();
    crate::exomonad::write_fixture_project_config(&authored, "gpt-6-sol", |project| {
        project.haskell.source_roots = vec![".".into()];
        project.haskell.modules = vec!["Project.Work".into()];
    });
    std::fs::write(authored.join("Project/Work.hs"), work_module(answer)).unwrap();
}

fn commit(workspace: &Path, message: &str) {
    let git = |args: &[&str]| {
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(workspace)
            .args(args)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    };
    git(&["add", "-A"]);
    git(&[
        "-c",
        "user.name=Source Reload Test",
        "-c",
        "user.email=source-reload@example.invalid",
        "commit",
        "-m",
        message,
    ]);
}

/// Commit the authored package, so a checkout cut from the project's head
/// carries it and can have a source layer of its own.
fn commit_workspace(workspace: &Path) {
    commit(workspace, "authored package");
}

#[tokio::test]
async fn authored_helpers_publish_explicitly_and_children_keep_their_inherited_revision() {
    let campaign = TestCampaign::start_with_config(
        |admission| admission,
        |config| {
            write_workspace(&config.workspace, 1);
            let helpers = config.workspace.join(".exomonad/helpers");
            std::fs::create_dir_all(&helpers).unwrap();
            std::fs::write(
                helpers.join("SessionHelpers.hs"),
                "module SessionHelpers where\nhelperAnswer :: Int\nhelperAnswer = 41\n",
            )
            .unwrap();
            commit_workspace(&config.workspace);
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
        .run_scenario(|campaign| {
            Box::pin(async move {
                let helper_root = campaign
                    .worktrees
                    .managed_root()
                    .join(".resources")
                    .join(runtime_namespace(campaign.session_root.path()))
                    .join("helpers");
                let draft = helper_root.join("drafts/run/SessionHelpers.hs");
                let root = campaign.root_installation.policy.clone();
                assert!(std::fs::read_to_string(&draft)
                    .unwrap()
                    .contains("helperAnswer = 41"));
                let initial = issued_source_identity(root.as_ref()).await;

                let published =
                    dispatch_structured_tool(root.as_ref(), "reload", serde_json::json!({})).await;
                assert_eq!(
                    published["items"][0]["value"]["outcome"], "published",
                    "{published}"
                );
                let first = issued_source_identity(root.as_ref()).await;
                assert_ne!(first, initial);
                assert_eq!(
                    published["items"][0]["value"]["revision"]["identity"],
                    first
                );
                committed(root.as_ref(), "import SessionHelpers").await;
                let answer = committed(root.as_ref(), "inspectFull helperAnswer").await;
                assert_eq!(answer["items"][0]["output"], "41", "{answer}");

                let invalid = "module SessionHelpers where\nhelperAnswer = (\n";
                std::fs::write(&draft, invalid).unwrap();
                let rejected =
                    dispatch_structured_tool(root.as_ref(), "reload", serde_json::json!({})).await;
                assert_eq!(
                    rejected["items"][0]["value"]["outcome"], "rejected",
                    "{rejected}"
                );
                assert_eq!(
                    rejected["items"][0]["value"]["issuedRevision"]["identity"],
                    first
                );
                assert_eq!(issued_source_identity(root.as_ref()).await, first);
                assert_eq!(std::fs::read_to_string(&draft).unwrap(), invalid);
                let answer = committed(root.as_ref(), "inspectFull helperAnswer").await;
                assert_eq!(answer["items"][0]["output"], "41", "{answer}");

                std::fs::write(
                    &draft,
                    "module SessionHelpers where\nhelperAnswer :: Int\nhelperAnswer = 42\n",
                )
                .unwrap();
                let published =
                    dispatch_structured_tool(root.as_ref(), "reload", serde_json::json!({})).await;
                assert_eq!(
                    published["items"][0]["value"]["outcome"], "published",
                    "{published}"
                );
                let second = issued_source_identity(root.as_ref()).await;
                assert_ne!(second, first);
                assert_eq!(
                    published["items"][0]["value"]["revision"]["identity"],
                    second
                );
                committed(root.as_ref(), "import SessionHelpers").await;
                let answer = committed(root.as_ref(), "inspectFull helperAnswer").await;
                assert_eq!(answer["items"][0]["output"], "42", "{answer}");

                let launch = {
                    let root = root.clone();
                    tokio::spawn(async move {
                        dispatch_haskell_script(root.as_ref(), CODING_CHILD).await
                    })
                };
                let child = next_child(campaign).await;
                assert_eq!(launch.await.unwrap()["status"], "committed");
                committed(child.policy.as_ref(), "import SessionHelpers").await;
                let child_answer =
                    committed(child.policy.as_ref(), "inspectFull helperAnswer").await;
                assert_eq!(child_answer["items"][0]["output"], "42", "{child_answer}");

                std::fs::write(
                    &draft,
                    "module SessionHelpers where\nhelperAnswer :: Int\nhelperAnswer = 43\n",
                )
                .unwrap();
                let published =
                    dispatch_structured_tool(root.as_ref(), "reload", serde_json::json!({})).await;
                assert_eq!(
                    published["items"][0]["value"]["outcome"], "published",
                    "{published}"
                );
                committed(root.as_ref(), "import SessionHelpers").await;
                let parent_answer = committed(root.as_ref(), "inspectFull helperAnswer").await;
                assert_eq!(parent_answer["items"][0]["output"], "43", "{parent_answer}");
                let child_answer =
                    committed(child.policy.as_ref(), "inspectFull helperAnswer").await;
                assert_eq!(child_answer["items"][0]["output"], "42", "{child_answer}");
            })
        })
        .await;
}

/// Keep the authored package out of the repository entirely, so a checkout cut
/// from the project's head has no `.exomonad` and therefore no source of its own.
/// The run still reads the package from the working tree, and the tree stays
/// clean enough to cut a checkout from.
fn ignore_workspace(workspace: &Path) {
    std::fs::write(workspace.join(".gitignore"), ".exomonad/\n").unwrap();
    commit(workspace, "keep the authored package out of the repository");
}

/// Read source identity through a newly issued request's production Source effect.
async fn issued_source_identity(policy: &dyn exomonad_actor::ResidentToolEndpoint) -> String {
    let issued = policy
        .snapshot_for_request()
        .expect("issued source generation");
    let result = committed(
        issued.as_ref(),
        "Right selectedSource <- sourceStatus\ninspectFull (revisionIdentity (statusActive selectedSource))",
    )
    .await;
    result["items"][1]["output"].as_str().unwrap().to_owned()
}

/// Admit the next child, grant it its worktree, and let it run.
async fn next_child(campaign: &mut TestCampaign) -> exomonad_actor::LocalResidentInstallation {
    let child = campaign
        .next_deployment(
            "child admission",
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
        ActorWorktreeGrant::Bound {
            enumerate: false,
            allocate: true,
        },
    );
    campaign.acknowledge_native_spawn(&child);
    child
}

const CODING_CHILD: &str = "import qualified Tidepool.Agent.Contract as A\n\
     import Tidepool.Actors.Exomonad\n\
     import Tidepool.Effects.Core (Commands, Lookup)\n\
     import Tidepool.Effects.Core (Source)\n\
     Right seed <- checkpoint \"source-reload child context\"\n\
     Right child <- spawnSubagent (ForkCtx seed) (ForkWorktree projectHead)\n\
       ((defaultSpawnOptions (A.defaultWorkbenchSpec @'[Replies, Commands, Lookup, BoundWorktree, Source]))\n\
         { spawnLabel = Just \"editor\", spawnLifetime = ActorOwned\n\
         , spawnInstructions = Just \"Use the configured workspace tools for this request and return the typed result.\" })\n\
     Right worker <- request @Text child () (defaultRequestOptions { requestLabel = Just \"editor\" })\n";

/// A historical checkout can carry stale or missing tooling. A child uses the
/// run's current graph and cannot publish checkout edits into that graph.
#[tokio::test]
async fn a_child_with_stale_or_missing_checkout_tooling_uses_the_run_graph() {
    let campaign = TestCampaign::start_with_config(
        |admission| admission,
        |config| {
            write_workspace(&config.workspace, 1);
            commit_workspace(&config.workspace);
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
    campaign.run_scenario(|campaign| Box::pin(async move {
    let root = campaign.root_installation.policy.clone();

    let launch = {
        let root = root.clone();
        tokio::spawn(async move { dispatch_haskell_script(root.as_ref(), CODING_CHILD).await })
    };
    let child = next_child(campaign).await;
    assert_eq!(launch.await.unwrap()["status"], "committed");
    let published_before = issued_source_identity(root.as_ref()).await;

    // The checkout carries a historical copy of the authored package.
    let checkout = campaign
        .worktrees
        .lookup(&exomonad_worktree::WorktreeId::from_raw(
            &child.launch_worktrees[0],
        ))
        .unwrap()
        .unwrap();
    let module = checkout.cwd().join(".exomonad/Project/Work.hs");
    assert!(module.exists(), "the checkout carries the authored package");

    std::fs::write(&module, work_module(41)).unwrap();
    let stale = committed(child.policy.as_ref(), "inspectFull answer").await;
    assert_eq!(stale["items"][0]["output"], "1", "{stale}");
    std::fs::remove_file(&module).unwrap();
    let missing = committed(child.policy.as_ref(), "inspectFull answer").await;
    assert_eq!(missing["items"][0]["output"], "1", "{missing}");

    let refused = committed(
        child.policy.as_ref(),
        "outcome <- reloadSource []\ninspectFull (case outcome of { Left (SourceUnavailable _) -> \"refused\"; _ -> \"published\" })",
    )
    .await;
    assert_eq!(refused["items"][1]["output"], "refused", "{refused}");
    assert_eq!(issued_source_identity(root.as_ref()).await, published_before);
    let root_answer = committed(root.as_ref(), "inspectFull answer").await;
    assert_eq!(root_answer["items"][0]["output"], "1", "{root_answer}");

    // Both source status views name the run's current revision.
    let child_status = committed(
        child.policy.as_ref(),
        "Right now <- sourceStatus\ninspectFull (revisionGeneration (statusActive now))",
    )
    .await;
    assert_eq!(child_status["items"][1]["output"], "1", "{child_status}");
    let root_status = committed(
        root.as_ref(),
        "Right now <- sourceStatus\ninspectFull (revisionGeneration (statusActive now))",
    )
    .await;
    assert_eq!(root_status["items"][1]["output"], "1", "{root_status}");
})).await;
}

/// A child with no source of its own cannot publish into the run's layer.
/// It compiles against that layer and can read it — `sourceStatus` answers —
/// but `reloadSource` is refused, and the refusal is structural: the child was
/// never given a layer to publish into, so there is nothing for it to name.
#[tokio::test]
async fn a_child_without_its_own_source_cannot_republish_the_run() {
    let campaign = TestCampaign::start_with_config(
        |admission| admission,
        |config| {
            // Authored but never committed: the run reads it from the working
            // tree, and a checkout cut from the project's head has no `.exomonad`.
            ignore_workspace(&config.workspace);
            write_workspace(&config.workspace, 1);
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
    campaign.run_scenario(|campaign| Box::pin(async move {
    let root = campaign.root_installation.policy.clone();

    let launch = {
        let root = root.clone();
        tokio::spawn(async move { dispatch_haskell_script(root.as_ref(), CODING_CHILD).await })
    };
    let child = next_child(campaign).await;
    assert_eq!(launch.await.unwrap()["status"], "committed");
    let published_before = issued_source_identity(root.as_ref()).await;

    let checkout = campaign
        .worktrees
        .lookup(&exomonad_worktree::WorktreeId::from_raw(
            &child.launch_worktrees[0],
        ))
        .unwrap()
        .unwrap();
    assert!(
        !checkout.cwd().join(".exomonad").exists(),
        "this checkout carries no authored source"
    );

    // It reads the layer it actually compiles against…
    let status = committed(
        child.policy.as_ref(),
        "Right now <- sourceStatus\ninspectFull (revisionGeneration (statusActive now))",
    )
    .await;
    assert_eq!(status["items"][1]["output"], "1", "{status}");

    // …and cannot publish into it. The edit the root made is still waiting for
    // the root, not for this child.
    std::fs::write(
        campaign
            ._repository
            .path()
            .join(".exomonad/Project/Work.hs"),
        work_module(41),
    )
    .unwrap();
    let refused = committed(
        child.policy.as_ref(),
        "outcome <- reloadSource []\ninspectFull (case outcome of { Left (SourceUnavailable _) -> \"refused\"; _ -> \"published\" })",
    )
    .await;
    assert_eq!(refused["items"][1]["output"], "refused", "{refused}");
    assert_eq!(issued_source_identity(root.as_ref()).await, published_before);
    let root_answer = committed(root.as_ref(), "inspectFull answer").await;
    assert_eq!(root_answer["items"][0]["output"], "1", "{root_answer}");
})).await;
}

/// The whole point, end to end: write a module, reload, and the next cell has
/// it — with a fresh notebook, and without rewriting work already in flight.
#[tokio::test]
async fn a_reloaded_module_starts_a_fresh_notebook_and_preserves_issued_bindings() {
    let campaign = TestCampaign::start_with_config(
        |admission| admission,
        |config| {
            let authored = config.workspace.join(".exomonad");
            std::fs::create_dir_all(authored.join("Project")).unwrap();
            crate::exomonad::write_fixture_project_config(&authored, "gpt-6-sol", |project| {
                project.haskell.source_roots = vec![".".into()];
                project.haskell.modules = vec!["Project.Work".into()];
            });
            std::fs::write(authored.join("Project/Work.hs"), work_module(1)).unwrap();
            std::fs::create_dir_all(authored.join("state/memory")).unwrap();
            std::fs::write(authored.join("state/memory/persona.md"), "retained memory").unwrap();
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
    campaign.run_scenario(|campaign| Box::pin(async move {
    let policy = campaign.root_installation.policy.clone();
    let policy = policy.as_ref();
    let issued_a = policy.snapshot_for_request().expect("installed generation A");

    // A value bound while the first revision is active.
    let bound = displayed(campaign, policy, "saved <- pure answer\ndisplay saved").await;
    assert_eq!(super::test_campaign::committed_display_text(&bound), "1", "{bound}");

    // Nothing is waiting yet.
    let status = displayed(
        campaign, policy,
        "Right before <- sourceStatus\ndisplay (revisionIdentity (statusActive before) == revisionIdentity (statusDisk before))",
    )
    .await;
    assert_eq!(super::test_campaign::committed_display_text(&status), "True", "{status}");

    // Edit the module with ordinary file tools, then reload from a cell.
    let edited = displayed(campaign, policy, &format!(
        "import qualified Tidepool.Effects.Authored as Files\nRight () <- Files.writeFile \".exomonad/Project/Work.hs\" {}\ndisplay answer",
        serde_json::to_string(&work_module(41)).unwrap()
    )).await;
    assert_eq!(super::test_campaign::committed_display_text(&edited), "1", "draft edits preserve active code: {edited}");
    let reloaded = displayed(
        campaign, policy,
        "Right outcome <- reloadSource []\ndisplay (case outcome of { ReloadPublished _ _ changed -> changed == [\"Project.Work\"]; _ -> False })",
    )
    .await;
    assert_eq!(
        super::test_campaign::committed_display_text(&reloaded), "True",
        "{reloaded}"
    );

    // A LATER cell compiles against the published revision…
    let later = displayed(campaign, policy, "display answer").await;
    assert_eq!(super::test_campaign::committed_display_text(&later), "41", "{later}");

    let absent = dispatch_haskell_script(policy, "display saved").await;
    assert_eq!(absent["status"], "rejected", "B must not inherit A bindings: {absent}");

    // An issued A request still sees A bindings; B starts without those names.
    let kept = displayed(campaign, issued_a.as_ref(), "display saved").await;
    assert_eq!(super::test_campaign::committed_display_text(&kept), "1", "{kept}");

    let memory = displayed(campaign, policy,
        "import qualified Tidepool.Effects.Authored as Files\nRight body <- Files.readFile \".exomonad/state/memory/persona.md\"\ndisplay (body == \"retained memory\")"
    ).await;
    assert_eq!(super::test_campaign::committed_display_text(&memory), "True", "reload preserves memory: {memory}");

    // Provenance is data: active now names the published revision, and the
    // module's digest moved with it.
    let after = displayed(
        campaign, policy,
        "Right now <- sourceStatus\ndisplay (revisionGeneration (statusActive now) == 2 && revisionIdentity (statusActive now) == revisionIdentity (statusDisk now))",
    )
    .await;
    assert_eq!(super::test_campaign::committed_display_text(&after), "True", "{after}");
})).await;
}

#[tokio::test]
async fn reload_admits_a_valid_new_module_into_the_complete_graph() {
    let campaign = TestCampaign::start_with_config(
        |admission| admission,
        |config| {
            write_workspace(&config.workspace, 1);
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
    campaign.run_scenario(|campaign| Box::pin(async move {
    let workspace = campaign._repository.path().to_path_buf();
    let policy = campaign.root_installation.policy.clone();
    let published_before = issued_source_identity(policy.as_ref()).await;
    let new_module = workspace.join(".exomonad/Project/Vibe.hs");
    std::fs::write(
        &new_module,
        "module Project.Vibe (vibeAnswer) where\n\nvibeAnswer :: Int\nvibeAnswer = 42\n",
    )
    .unwrap();

    let result = committed(
        policy.as_ref(),
        "Right before <- sourceStatus\nRight outcome <- reloadSource []\nRight after <- sourceStatus\ninspectFull (case outcome of { ReloadPublished _ _ changed -> \"Project.Vibe\" `elem` changed; _ -> False })\ninspectFull (revisionIdentity (statusActive before) == revisionIdentity (statusActive after))",
    )
    .await;
    assert_eq!(result["items"][3]["output"], "True", "{result}");
    assert_eq!(result["items"][4]["output"], "True", "{result}");
    assert!(
        new_module.is_file(),
        "publication leaves the authored edit in place"
    );
    assert_ne!(issued_source_identity(policy.as_ref()).await, published_before);
    let fresh = committed(policy.as_ref(), "import Project.Vibe\ninspectFull vibeAnswer").await;
    assert_eq!(fresh["items"][1]["output"], "42", "{fresh}");
})).await;
}

/// A reload whose affected set does not typecheck is an ordinary result a
/// program handles: it names the snapshot that failed, the previous graph is
/// still what later cells compile against, and the edited file is untouched.
#[tokio::test]
async fn a_rejected_reload_is_a_value_and_leaves_the_notebook_running() {
    let campaign = TestCampaign::start_with_config(
        |admission| admission,
        |config| {
            let authored = config.workspace.join(".exomonad");
            std::fs::create_dir_all(authored.join("Project")).unwrap();
            crate::exomonad::write_fixture_project_config(&authored, "gpt-6-sol", |project| {
                project.haskell.source_roots = vec![".".into()];
                project.haskell.modules = vec!["Project.Work".into()];
            });
            std::fs::write(authored.join("Project/Work.hs"), work_module(1)).unwrap();
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
    campaign.run_scenario(|campaign| Box::pin(async move {
    let workspace = campaign._repository.path().to_path_buf();
    let policy = campaign.root_installation.policy.clone();
    let policy = policy.as_ref();

    let broken =
        "module Project.Work (answer) where\n\nanswer :: Int\nanswer = undefinedByThisReload\n";
    let edited = committed(policy, &format!(
        "import qualified Tidepool.Effects.Authored as Files\nRight () <- Files.writeFile \".exomonad/Project/Work.hs\" {}",
        serde_json::to_string(broken).unwrap()
    )).await;
    assert!(!edited["items"].as_array().unwrap().is_empty(), "{edited}");
    let rejected = displayed(
        campaign, policy,
        "Right outcome <- reloadSource []\ndisplay ((case outcome of { ReloadRejected active failed _ -> revisionIdentity active /= revisionIdentity failed; _ -> False }) && (case outcome of { ReloadRejected _ _ detail -> \"undefinedByThisReload\" `T.isInfixOf` detail; _ -> False }))",
    )
    .await;
    assert_eq!(
        super::test_campaign::committed_display_text(&rejected), "True",
        "the receipt names the snapshot that failed: {rejected}"
    );
    assert_eq!(
        super::test_campaign::committed_display_text(&rejected), "True",
        "the receipt carries the diagnostics: {rejected}"
    );

    // The previously compiled graph is still what a later cell gets.
    let later = displayed(campaign, policy, "display answer").await;
    assert_eq!(super::test_campaign::committed_display_text(&later), "1", "{later}");

    // The edited source stays exactly as it was written.
    assert_eq!(
        std::fs::read_to_string(workspace.join(".exomonad/Project/Work.hs")).unwrap(),
        broken
    );
})).await;
}
