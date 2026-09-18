//! Reloading source from inside a live notebook — the run's, and a
//! checkout's.
//!
//! The reload mechanism's own transaction — identity, the rebuilt
//! reverse-dependency closure, and what a rejected candidate leaves behind —
//! is checked in `crate::shoal::source`. What is checked HERE is what only a
//! live session can answer: a later cell compiles against the published
//! revision while a value bound before the reload keeps the code it was built
//! from, and an actor's reload reaches its own layer and no other.

use std::path::Path;

use super::test_campaign::TestCampaign;
use super::tests::dispatch_haskell_script;
use super::*;

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

/// The authored package a source-reload campaign starts from: one configured
/// module, answering `1`.
fn write_workspace(workspace: &Path, answer: u32) {
    let authored = workspace.join(".shoal");
    std::fs::create_dir_all(authored.join("Project")).unwrap();
    std::fs::write(
        authored.join("config.toml"),
        "[defaults]\nmodel = 'gpt-5.6-sol'\n[haskell]\nsource_roots = ['.']\nmodules = ['Project.Work']\n",
    )
    .unwrap();
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

/// Keep the authored package out of the repository entirely, so a checkout cut
/// from the project's head has no `.shoal` and therefore no source of its own.
/// The run still reads the package from the working tree, and the tree stays
/// clean enough to cut a checkout from.
fn ignore_workspace(workspace: &Path) {
    std::fs::write(workspace.join(".gitignore"), ".shoal/\n").unwrap();
    commit(workspace, "keep the authored package out of the repository");
}

/// Where the run's own layer currently points. Reading the symlink is the
/// honest question "did a reload move the run's source?", because the
/// publication IS that one rename.
fn run_layer_target(campaign: &TestCampaign) -> std::path::PathBuf {
    std::fs::read_link(campaign.session_root.path().join("workspace/active")).unwrap()
}

/// Admit the next child, grant it its worktree, and let it run.
async fn next_child(campaign: &mut TestCampaign) -> tidepool_actor::LocalResidentInstallation {
    tokio::time::timeout(Duration::from_secs(180), async {
        loop {
            match campaign.deployments.recv().await.unwrap() {
                LocalResidentDeployment::PolicyInstalled(child) => {
                    campaign.authority.install_grant(
                        child.actor.identity().into(),
                        worktree_grant(child.effective_role.role()),
                    );
                    child.fork_gate.as_ref().unwrap().mark_ready().unwrap();
                    return *child;
                }
                LocalResidentDeployment::Retired { actor, terminal } => {
                    panic!("{actor:?} retired: {terminal:?}")
                }
                _ => {}
            }
        }
    })
    .await
    .expect("child admission")
}

const CODING_CHILD: &str = "let campaign = \"source-reload\" :: CampaignLabel\n\
     let group = \"checkout\" :: ForkGroupLabel\n\
     let leaf = \"editor\" :: Label\n\
     worker <- unfold (batch campaign group) (child (coding @Text projectHead (assignment leaf ())))\n";

/// A child editing Haskell in its OWN checkout reloads its own layer: its
/// later cells see the edit, the run's layer never moves, and the root — which
/// shares that run layer with every other actor — keeps compiling against what
/// it had.
#[tokio::test]
async fn a_child_reloads_its_own_checkout_and_leaves_the_run_alone() {
    let mut campaign = TestCampaign::start_with_config(
        tidepool_actor::ResearchPolicy::default(),
        |admission| admission,
        |config| {
            write_workspace(&config.workspace, 1);
            commit_workspace(&config.workspace);
            config.workspace_inputs = Some(
                crate::shoal::workspace::FrozenWorkspace::load(&config.workspace, &config.run_root)
                    .unwrap(),
            );
        },
    )
    .await;
    let root = campaign.root_installation.policy.clone();

    let launch = {
        let root = root.clone();
        tokio::spawn(async move { dispatch_haskell_script(root.as_ref(), CODING_CHILD).await })
    };
    let child = next_child(&mut campaign).await;
    assert_eq!(launch.await.unwrap()["status"], "committed");
    let published_before = run_layer_target(&campaign);

    // The checkout the child holds, and the module inside it that only the
    // child compiles against.
    let checkout = campaign
        .worktrees
        .lookup(&tidepool_worktree::WorktreeId::from_raw(
            &child.launch_worktrees[0],
        ))
        .unwrap()
        .unwrap();
    let module = checkout.cwd().join(".shoal/Project/Work.hs");
    assert!(module.exists(), "the checkout carries the authored package");

    // Both actors start on the same answer, from their separate layers.
    let before = committed(child.policy.as_ref(), "inspectFull answer").await;
    assert_eq!(before["items"][0]["output"], "1", "{before}");

    std::fs::write(&module, work_module(41)).unwrap();
    let reloaded = committed(
        child.policy.as_ref(),
        "Right outcome <- reloadSource []\ninspectFull (case outcome of { ReloadPublished _ _ changed -> changed; _ -> [\"not published\"] })",
    )
    .await;
    assert_eq!(
        reloaded["items"][1]["output"], "[Project.Work]",
        "{reloaded}"
    );

    // The child's LATER cell compiles against its own published revision…
    let later = committed(child.policy.as_ref(), "inspectFull answer").await;
    assert_eq!(later["items"][0]["output"], "41", "{later}");

    // …and the run's layer did not move, so the root still compiles against
    // the revision it started with.
    assert_eq!(run_layer_target(&campaign), published_before);
    let root_answer = committed(root.as_ref(), "inspectFull answer").await;
    assert_eq!(root_answer["items"][0]["output"], "1", "{root_answer}");

    // Provenance is per actor: the child's status names its own layer, at its
    // own generation, while the root's is still the run's first revision.
    let child_status = committed(
        child.policy.as_ref(),
        "Right now <- sourceStatus\ninspectFull (revisionGeneration (statusActive now))",
    )
    .await;
    assert_eq!(child_status["items"][1]["output"], "2", "{child_status}");
    let root_status = committed(
        root.as_ref(),
        "Right now <- sourceStatus\ninspectFull (revisionGeneration (statusActive now))",
    )
    .await;
    assert_eq!(root_status["items"][1]["output"], "1", "{root_status}");

    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

/// A child with no source of its own cannot publish into the run's layer.
/// It compiles against that layer and can read it — `sourceStatus` answers —
/// but `reloadSource` is refused, and the refusal is structural: the child was
/// never given a layer to publish into, so there is nothing for it to name.
#[tokio::test]
async fn a_child_without_its_own_source_cannot_republish_the_run() {
    let mut campaign = TestCampaign::start_with_config(
        tidepool_actor::ResearchPolicy::default(),
        |admission| admission,
        |config| {
            // Authored but never committed: the run reads it from the working
            // tree, and a checkout cut from the project's head has no `.shoal`.
            ignore_workspace(&config.workspace);
            write_workspace(&config.workspace, 1);
            config.workspace_inputs = Some(
                crate::shoal::workspace::FrozenWorkspace::load(&config.workspace, &config.run_root)
                    .unwrap(),
            );
        },
    )
    .await;
    let root = campaign.root_installation.policy.clone();

    let launch = {
        let root = root.clone();
        tokio::spawn(async move { dispatch_haskell_script(root.as_ref(), CODING_CHILD).await })
    };
    let child = next_child(&mut campaign).await;
    assert_eq!(launch.await.unwrap()["status"], "committed");
    let published_before = run_layer_target(&campaign);

    let checkout = campaign
        .worktrees
        .lookup(&tidepool_worktree::WorktreeId::from_raw(
            &child.launch_worktrees[0],
        ))
        .unwrap()
        .unwrap();
    assert!(
        !checkout.cwd().join(".shoal").exists(),
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
        campaign._repository.path().join(".shoal/Project/Work.hs"),
        work_module(41),
    )
    .unwrap();
    let refused = committed(
        child.policy.as_ref(),
        "outcome <- reloadSource []\ninspectFull (case outcome of { Left (SourceUnavailable _) -> \"refused\"; _ -> \"published\" })",
    )
    .await;
    assert_eq!(refused["items"][1]["output"], "refused", "{refused}");
    assert_eq!(run_layer_target(&campaign), published_before);
    let root_answer = committed(root.as_ref(), "inspectFull answer").await;
    assert_eq!(root_answer["items"][0]["output"], "1", "{root_answer}");

    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
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
