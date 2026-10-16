//! Source reload through the live actor: changed source starts a fresh
//! notebook while issued requests and children retain their exact source.
//! Capsule capture and recovery histories live in `crate::exomonad::source`.
#![allow(
    clippy::disallowed_methods,
    reason = "test: launches real tmux/process fixtures directly, not through the production launcher"
)]

use std::path::Path;

use super::test_campaign::TestCampaign;
use super::tests::dispatch_structured_tool;
use super::*;

async fn dispatch_haskell_script(
    policy: &dyn exomonad_actor::ResidentToolEndpoint,
    source: &str,
) -> serde_json::Value {
    super::test_campaign::dispatch_haskell_script_named(policy, "haskell_sync", source).await
}

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
                let published =
                    dispatch_structured_tool(root.as_ref(), "reload", serde_json::json!({})).await;
                assert_eq!(
                    published["items"][0]["value"]["Right"]["tag"], "ReloadPublished",
                    "{published}"
                );
                let first = published["items"][0]["value"]["Right"]["contents"][1]
                    ["revisionIdentity"]
                    .as_str()
                    .unwrap();
                assert!(issued_source_identity_matches(campaign, root.as_ref(), first).await);
                committed(root.as_ref(), "import SessionHelpers").await;
                let answer = displayed(campaign, root.as_ref(), "display helperAnswer").await;
                assert_eq!(
                    super::test_campaign::committed_display_text(&answer),
                    "41",
                    "{answer}"
                );

                let invalid = "module SessionHelpers where\nhelperAnswer = (\n";
                std::fs::write(&draft, invalid).unwrap();
                let rejected =
                    dispatch_structured_tool(root.as_ref(), "reload", serde_json::json!({})).await;
                assert_eq!(
                    rejected["items"][0]["value"]["Right"]["tag"], "ReloadRejected",
                    "{rejected}"
                );
                assert_eq!(
                    rejected["items"][0]["value"]["Right"]["contents"][0]["revisionIdentity"],
                    first
                );
                assert!(issued_source_identity_matches(campaign, root.as_ref(), first).await);
                assert_eq!(std::fs::read_to_string(&draft).unwrap(), invalid);
                let answer = displayed(campaign, root.as_ref(), "display helperAnswer").await;
                assert_eq!(
                    super::test_campaign::committed_display_text(&answer),
                    "41",
                    "{answer}"
                );

                std::fs::write(
                    &draft,
                    "module SessionHelpers where\nhelperAnswer :: Int\nhelperAnswer = 42\n",
                )
                .unwrap();
                let published =
                    dispatch_structured_tool(root.as_ref(), "reload", serde_json::json!({})).await;
                assert_eq!(
                    published["items"][0]["value"]["Right"]["tag"], "ReloadPublished",
                    "{published}"
                );
                let second = published["items"][0]["value"]["Right"]["contents"][1]
                    ["revisionIdentity"]
                    .as_str()
                    .unwrap();
                assert_ne!(second, first);
                assert!(issued_source_identity_matches(campaign, root.as_ref(), second).await);
                committed(root.as_ref(), "import SessionHelpers").await;
                let answer = displayed(campaign, root.as_ref(), "display helperAnswer").await;
                assert_eq!(
                    super::test_campaign::committed_display_text(&answer),
                    "42",
                    "{answer}"
                );

                let (launch, child) = campaign
                    .dispatch_native_spawn_setup(
                        "source reload child admission",
                        CODING_CHILD,
                        Duration::from_secs(180),
                    )
                    .await;
                assert_eq!(launch["status"], "committed", "{launch}");
                campaign.authority.install_grant(
                    child.actor.identity().into(),
                    ActorWorktreeGrant::Bound {
                        enumerate: false,
                        allocate: true,
                    },
                );
                assert!(
                    issued_source_identity_matches(campaign, child.policy.as_ref(), second).await
                );
                committed(child.policy.as_ref(), "import SessionHelpers").await;
                let child_answer =
                    displayed(campaign, child.policy.as_ref(), "display helperAnswer").await;
                assert_eq!(
                    super::test_campaign::committed_display_text(&child_answer),
                    "42",
                    "{child_answer}"
                );

                std::fs::write(
                    &draft,
                    "module SessionHelpers where\nhelperAnswer :: Int\nhelperAnswer = 43\n",
                )
                .unwrap();
                let published =
                    dispatch_structured_tool(root.as_ref(), "reload", serde_json::json!({})).await;
                assert_eq!(
                    published["items"][0]["value"]["Right"]["tag"], "ReloadPublished",
                    "{published}"
                );
                committed(root.as_ref(), "import SessionHelpers").await;
                let parent_answer =
                    displayed(campaign, root.as_ref(), "display helperAnswer").await;
                assert_eq!(
                    super::test_campaign::committed_display_text(&parent_answer),
                    "43",
                    "{parent_answer}"
                );
                assert!(
                    issued_source_identity_matches(campaign, child.policy.as_ref(), second).await
                );
                let child_answer =
                    displayed(campaign, child.policy.as_ref(), "display helperAnswer").await;
                assert_eq!(
                    super::test_campaign::committed_display_text(&child_answer),
                    "42",
                    "{child_answer}"
                );
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

/// Issue a fresh request from the live actor endpoint and compare its selected
/// source revision with the exact reload receipt. A previously issued endpoint
/// remains pinned to its earlier generation.
async fn issued_source_identity_matches(
    campaign: &mut TestCampaign,
    policy: &dyn exomonad_actor::ResidentToolEndpoint,
    expected: &str,
) -> bool {
    let issued = policy
        .snapshot_for_request()
        .expect("issued source generation");
    let source = format!(
        "import qualified Data.Text as ReceiptText\nRight selectedSource <- sourceStatus\ndisplay (revisionIdentity (statusActive selectedSource) == ReceiptText.pack {expected:?})"
    );
    let result = displayed(campaign, issued.as_ref(), &source).await;
    super::test_campaign::committed_display_text(&result) == "True"
}

/// Retain the active revision as a typed value in the root's current notebook.
async fn bind_current_source_identity(
    campaign: &mut TestCampaign,
    policy: &dyn exomonad_actor::ResidentToolEndpoint,
) -> String {
    let result = displayed(
        campaign,
        policy,
        "Right selectedSource <- sourceStatus\npublishedSourceBefore <- pure (revisionIdentity (statusActive selectedSource))\ndisplay publishedSourceBefore",
    )
    .await;
    let identity = super::test_campaign::committed_display_text(&result).to_owned();
    assert!(!identity.is_empty(), "{result}");
    identity
}

/// Compare against the retained typed value without rendering the identity.
async fn current_source_matches_bound_identity(
    campaign: &mut TestCampaign,
    policy: &dyn exomonad_actor::ResidentToolEndpoint,
) -> bool {
    let result = displayed(
        campaign,
        policy,
        "Right selectedSource <- sourceStatus\ndisplay (publishedSourceBefore == revisionIdentity (statusActive selectedSource))",
    )
    .await;
    super::test_campaign::committed_display_text(&result) == "True"
}

const CODING_CHILD: &str = include_str!("fixtures/source_reload_coding_child.hs");

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
    let parent_identity = bind_current_source_identity(campaign, root.as_ref()).await;

    let (launch, child) = campaign
        .dispatch_native_spawn_setup(
            "source reload child admission",
            CODING_CHILD,
            Duration::from_secs(180),
        )
        .await;
    assert_eq!(launch["status"], "committed", "{launch}");
    campaign.authority.install_grant(
        child.actor.identity().into(),
        ActorWorktreeGrant::Bound {
            enumerate: false,
            allocate: true,
        },
    );
    assert!(issued_source_identity_matches(campaign, child.policy.as_ref(), &parent_identity).await);

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
    let stale = displayed(campaign, child.policy.as_ref(), "display answer").await;
    assert_eq!(super::test_campaign::committed_display_text(&stale), "1", "{stale}");
    std::fs::remove_file(&module).unwrap();
    let missing = displayed(campaign, child.policy.as_ref(), "display answer").await;
    assert_eq!(super::test_campaign::committed_display_text(&missing), "1", "{missing}");

    let refused = displayed(
        campaign,
        child.policy.as_ref(),
        "outcome <- reloadSource []\ndisplay (case outcome of { Left (ReloadUnavailable _) -> \"refused\"; _ -> \"published\" })",
    )
    .await;
    assert_eq!(super::test_campaign::committed_display_text(&refused), "refused", "{refused}");
    assert!(issued_source_identity_matches(campaign, child.policy.as_ref(), &parent_identity).await);
    let child_after_refusal = displayed(campaign, child.policy.as_ref(), "display answer").await;
    assert_eq!(super::test_campaign::committed_display_text(&child_after_refusal), "1", "{child_after_refusal}");
    assert!(current_source_matches_bound_identity(campaign, root.as_ref()).await);
    let root_answer = displayed(campaign, root.as_ref(), "display answer").await;
    assert_eq!(super::test_campaign::committed_display_text(&root_answer), "1", "{root_answer}");

    // Both source status views name the run's exact issued revision.
    assert!(issued_source_identity_matches(campaign, child.policy.as_ref(), &parent_identity).await);
    assert!(issued_source_identity_matches(campaign, root.as_ref(), &parent_identity).await);
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
    let parent_identity = bind_current_source_identity(campaign, root.as_ref()).await;

    let (launch, child) = campaign
        .dispatch_native_spawn_setup(
            "source reload child admission",
            CODING_CHILD,
            Duration::from_secs(180),
        )
        .await;
    assert_eq!(launch["status"], "committed", "{launch}");
    campaign.authority.install_grant(
        child.actor.identity().into(),
        ActorWorktreeGrant::Bound {
            enumerate: false,
            allocate: true,
        },
    );
    assert!(issued_source_identity_matches(campaign, child.policy.as_ref(), &parent_identity).await);

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

    // It reads the exact layer the parent issued…
    assert!(issued_source_identity_matches(campaign, child.policy.as_ref(), &parent_identity).await);

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
    let refused = displayed(
        campaign,
        child.policy.as_ref(),
        "outcome <- reloadSource []\ndisplay (case outcome of { Left (ReloadUnavailable _) -> \"refused\"; _ -> \"published\" })",
    )
    .await;
    assert_eq!(super::test_campaign::committed_display_text(&refused), "refused", "{refused}");
    assert!(issued_source_identity_matches(campaign, child.policy.as_ref(), &parent_identity).await);
    let child_after_refusal = displayed(campaign, child.policy.as_ref(), "display answer").await;
    assert_eq!(super::test_campaign::committed_display_text(&child_after_refusal), "1", "{child_after_refusal}");
    assert!(current_source_matches_bound_identity(campaign, root.as_ref()).await);
    let root_answer = displayed(campaign, root.as_ref(), "display answer").await;
    assert_eq!(super::test_campaign::committed_display_text(&root_answer), "1", "{root_answer}");
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
        "Right outcome <- reloadSource []\ndisplay (case outcome of { ReloadPublished _ _ changed _ -> changed == [\"Project.Work\"]; _ -> False })",
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
        "Right now <- sourceStatus\ndisplay (revisionIdentity (statusActive now) == revisionIdentity (statusDisk now))",
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
    let new_module = workspace.join(".exomonad/Project/Vibe.hs");
    std::fs::write(
        &new_module,
        "module Project.Vibe (vibeAnswer) where\n\nvibeAnswer :: Int\nvibeAnswer = 42\n",
    )
    .unwrap();

    let result = displayed(
        campaign,
        policy.as_ref(),
        "Right before <- sourceStatus\nRight outcome <- reloadSource []\nRight after <- sourceStatus\ndisplay ((case outcome of { ReloadPublished _ _ changed _ -> \"Project.Vibe\" `elem` changed; _ -> False }) && (revisionIdentity (statusActive before) == revisionIdentity (statusActive after)) && (case outcome of { ReloadPublished previous current _ _ -> revisionIdentity previous == revisionIdentity (statusActive before) && revisionIdentity current /= revisionIdentity (statusActive before); _ -> False }))",
    )
    .await;
    assert_eq!(super::test_campaign::committed_display_text(&result), "True", "{result}");
    assert!(
        new_module.is_file(),
        "publication leaves the authored edit in place"
    );
    let fresh = displayed(campaign, policy.as_ref(), "import Project.Vibe\ndisplay vibeAnswer").await;
    assert_eq!(super::test_campaign::committed_display_text(&fresh), "42", "{fresh}");
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
