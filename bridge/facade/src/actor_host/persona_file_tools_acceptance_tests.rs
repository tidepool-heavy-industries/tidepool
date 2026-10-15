//! Native file operations remain callable after a notebook typecheck refusal.
use super::ResidentToolEndpointTestExt;
use super::hosted_test_context::HostedTestRuntime;
use super::test_campaign::{
    HostedScriptSelection, actor_output_origin, hosted_script_provider, hosted_test_settings,
    next_hosted_child_round, next_hosted_script_round, retained_committed_display_text,
    select_hosted_script_round,
};
use exomonad_tool::{ToolArguments, ToolInvocation};
use serde_json::{Value, json};
use std::collections::VecDeque;

const DRAFT: &str = ".exomonad/RepairDraft.hs";
const FIRST: &str = "module RepairDraft where\nanswer = True + 1\n";
const SECOND: &str = "module RepairDraft where\nanswer = False + 1\n";
const FIXED: &str = "module RepairDraft where\nanswer = True\n";

fn configure(config: &mut super::ActorHostConfig) {
    super::startup_persona_acceptance_tests::configure_persona(
        config,
        "AgentSpec.agentSpec",
        |workspace, _| {
            std::fs::write(
                workspace.join(".exomonad/AgentSpec.hs"),
                tidepool_testing::fixture_source(
                    "bridge/facade/src/actor_host/fixtures/supplied_live_root.hs",
                ),
            )
            .unwrap();
        },
    );
}

fn published(output: &Value, path: &std::path::Path, bytes: &str) -> Value {
    let outcome = &output["items"][0]["value"]["Right"];
    assert_eq!(outcome["tag"], "Published", "{output}");
    assert_eq!(outcome["publication"], json!({"tag":"Durable"}), "{output}");
    let reference = outcome["writtenFile"].clone();
    assert_eq!(
        reference["path"],
        path.canonicalize().unwrap().to_str().unwrap()
    );
    assert_eq!(
        reference["version"],
        blake3::hash(bytes.as_bytes()).to_hex().to_string()
    );
    assert_eq!(std::fs::read(path).unwrap(), bytes.as_bytes());
    reference
}

#[tokio::test]
async fn installed_file_tools_repair_a_draft_after_notebook_refusal_without_compilation() {
    let files = tempfile::tempdir().unwrap();
    let settings = hosted_test_settings(&files, 4);
    let (provider, mut requests) = hosted_script_provider();
    let host = HostedTestRuntime::start_configured(&settings, &provider, configure)
        .await
        .unwrap();
    host.run_scenario(|host| Box::pin(async move {
        let root = host.context.conversation_identity().unwrap().actor;
        let origin = actor_output_origin(host.runtime.run_identity(), host.context.actor.identity());
        let mut pending = VecDeque::new();
        host.input("Validate the complete Persona tool surface and repair a draft with its installed file tools.").await.unwrap();
        let first = next_hosted_script_round(&mut requests, &mut pending, &root).await;
        let spawn = first.operation("validated-file-companion");
        first.call("validated-file-companion", &tidepool_testing::fixture_source(
            "bridge/facade/src/actor_host/fixtures/persona_file_tools_spawn.hs"));
        let root_round = host.while_operation_succeeds(&spawn,
            next_hosted_script_round(&mut requests, &mut pending, &root)).await.unwrap();
        let output = root_round.settled_output("validated-file-companion");
        assert_eq!(retained_committed_display_text(&host.runtime.store(), &origin, &output), "True");
        let repair = root_round.operation("file-repair-request");
        root_round.call("file-repair-request", &tidepool_testing::fixture_source(
            "bridge/facade/src/actor_host/fixtures/persona_file_tools_request.hs"));
        let child = host.while_operation_succeeds(&repair,
            select_hosted_script_round(&mut requests, &mut pending, HostedScriptSelection::Any)).await.unwrap();
        if child.host_identity().actor == root {
            panic!("file request settled before child inference: {}", child.settled_output("file-repair-request"));
        }
        let child_identity = child.host_identity();
        let graph = host.context.forest.inspect_host_graph().await.unwrap();
        let actor = graph.iter().find(|node| host.context.binding(node.actor)
            .and_then(|binding| binding.conversation())
            .is_some_and(|conversation| conversation.identity() == &child_identity)).unwrap().actor;
        let child_origin = actor_output_origin(host.runtime.run_identity(), actor);
        child.sync_call_named("reply-bypass-refused", "haskell", &tidepool_testing::fixture_source(
            "bridge/facade/src/actor_host/fixtures/persona_file_tools_reply_bypass.hs"));
        let child = host.while_operation_succeeds(&repair,
            next_hosted_child_round(&mut requests, &mut pending, &child_identity.actor, &root, "file-repair-request")).await.unwrap();
        let refusal = child.settled_output("reply-bypass-refused");
        assert_eq!(retained_committed_display_text(&host.runtime.store(), &child_origin, &refusal), "True",
            "ordinary Replies cannot settle an explicitly JSON-tool-mode request");
        assert!(child.request.tools.iter().any(|tool| tool["name"] == "reply"), "same activation remains available");
        child.sync_call_named("notebook-refused", "haskell", "display (1 + True)");
        let created = host.while_operation_succeeds(&repair,
            next_hosted_child_round(&mut requests, &mut pending, &child_identity.actor, &root, "file-repair-request")).await.unwrap();
        let refused = created.settled_output("notebook-refused");
        assert_eq!(refused["status"], "rejected", "{refused}");
        let baseline = tidepool_extract_cmd::extract_spawn_count();
        let path = host.context.config.workspace.join(DRAFT);
        let installation = host.context.observer.installation(actor).await;
        let invalid = json!({"operation":{"tag":"Create","contents":FIRST,"file":{"path":DRAFT,"version":"0".repeat(64)}}});
        let refusal = installation.policy.dispatch_json_boxed(ToolInvocation {
            context: None, name: "write_file".into(), arguments: ToolArguments::Structured(invalid),
        }).await.expect_err("the actual installed Create decoder requires its path field");
        let exomonad_actor::ResidentToolError::Invocation(
            exomonad_actor::KernelInvocationFailure::Workbench(failure)) = refusal else {
                panic!("expected exact installed input-unit refusal: {refusal}")
            };
        assert_eq!(failure.point, tidepool_runtime::session::WorkbenchFailurePoint::InputUnit { index: 0 });
        assert_eq!(failure.total, 1);
        assert!(failure.receipts.is_empty());
        assert_eq!(failure.publication, Some(tidepool_runtime::session::WorkbenchPublicationOutcome::NotPublished {
            reason: tidepool_runtime::session::WorkbenchNotPublishedReason::Failed,
        }));
        assert!(!path.try_exists().unwrap(), "refused Create publishes no file");
        created.function("create-draft", "write_file", json!({"operation":{"tag":"Create","path":DRAFT,"contents":FIRST}}));
        let replaced = host.while_operation_succeeds(&repair,
            next_hosted_child_round(&mut requests, &mut pending, &child_identity.actor, &root, "file-repair-request")).await.unwrap();
        let initial_ref = published(&replaced.settled_output("create-draft"), &path, FIRST);
        replaced.function("replace-draft", "write_file", json!({"operation":{"tag":"Replace","file":initial_ref,"contents":SECOND}}));
        let read = host.while_operation_succeeds(&repair,
            next_hosted_child_round(&mut requests, &mut pending, &child_identity.actor, &root, "file-repair-request")).await.unwrap();
        let stale_ref = published(&read.settled_output("replace-draft"), &path, SECOND);
        read.function("read-draft", "read_file", json!({"path":DRAFT,"expectedVersion":stale_ref["version"],"range":null}));
        let preview = host.while_operation_succeeds(&repair,
            next_hosted_child_round(&mut requests, &mut pending, &child_identity.actor, &root, "file-repair-request")).await.unwrap();
        let read = preview.settled_output("read-draft");
        assert_eq!(read["items"][0]["value"]["Right"]["content"], SECOND, "{read}");
        assert_eq!(read["items"][0]["value"]["Right"]["file"], stale_ref);
        let edit = json!({"file":stale_ref,"edits":[{"oldText":"False + 1","newText":"True"}]});
        preview.function("preview-draft", "preview_edit", edit.clone());
        let edit_round = host.while_operation_succeeds(&repair,
            next_hosted_child_round(&mut requests, &mut pending, &child_identity.actor, &root, "file-repair-request")).await.unwrap();
        let preview = edit_round.settled_output("preview-draft");
        assert_eq!(preview["items"][0]["value"]["Right"]["preview"], "Edit 1\n- False + 1\n+ True\n", "{preview}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), SECOND, "preview does not publish");
        edit_round.function("edit-draft", "edit_file", edit);
        let reread = host.while_operation_succeeds(&repair,
            next_hosted_child_round(&mut requests, &mut pending, &child_identity.actor, &root, "file-repair-request")).await.unwrap();
        let final_ref = published(&reread.settled_output("edit-draft"), &path, FIXED);
        reread.function("read-repaired", "read_file", json!({"path":DRAFT,"expectedVersion":final_ref["version"],"range":null}));
        let conflict = host.while_operation_succeeds(&repair,
            next_hosted_child_round(&mut requests, &mut pending, &child_identity.actor, &root, "file-repair-request")).await.unwrap();
        let read = conflict.settled_output("read-repaired");
        assert_eq!(read["items"][0]["value"]["Right"]["content"], FIXED, "{read}");
        assert_eq!(read["items"][0]["value"]["Right"]["file"], final_ref);
        conflict.function("stale-replace", "write_file", json!({"operation":{"tag":"Replace","file":stale_ref,"contents":"must not publish"}}));
        let done = host.while_operation_succeeds(&repair,
            next_hosted_child_round(&mut requests, &mut pending, &child_identity.actor, &root, "file-repair-request")).await.unwrap();
        let conflict = done.settled_output("stale-replace");
        let conflict = &conflict["items"][0]["value"]["Right"];
        assert_eq!(conflict["tag"], "Conflict");
        assert_eq!(conflict["expectedVersion"], stale_ref["version"]);
        assert_eq!(conflict["actualVersion"], final_ref["version"]);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), FIXED);
        assert_eq!(tidepool_extract_cmd::extract_spawn_count(), baseline,
            "installed native file handlers submit zero logical extractor requests after notebook refusal");
        done.function("file-repaired", "reply", json!({"result":true}));
        let completed = host.while_operation_succeeds(&repair,
            next_hosted_script_round(&mut requests, &mut pending, &root)).await.unwrap();
        assert_eq!(retained_committed_display_text(&host.runtime.store(), &origin,
            &completed.settled_output("file-repair-request")), "True");
        completed.finish();
    })).await;
}
