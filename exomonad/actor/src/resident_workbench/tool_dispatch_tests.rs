use super::*;

#[test]
fn dispatch_envelope_preserves_authored_output_and_decodes_typed_refusals() {
    let output = serde_json::json!({"status": "refused", "output": [1, true, null]});
    let presentation = "presented independently";
    let reply: InstalledToolDispatchReply = serde_json::from_value(serde_json::json!({
        "status": "success", "output": output, "presentation": presentation,
    }))
    .unwrap();
    assert_eq!(
        reply.into_parts().unwrap(),
        (Some(output), presentation.to_owned())
    );
    for (kind, unknown) in [("unknown_tool", true), ("invalid_input", false)] {
        let mut payload = serde_json::json!({
            "status": "refused", "kind": kind, "error": "correct the call", "tool": "echo",
        });
        if !unknown {
            payload["detail"] = serde_json::json!("invalid argument");
        }
        let reply: InstalledToolDispatchReply = serde_json::from_value(payload).unwrap();
        let error = reply.into_parts().unwrap_err();
        assert_eq!(
            matches!(error, ToolDispatchError::UnknownTool { .. }),
            unknown
        );
        assert_eq!(error.to_string(), "correct the call");
        assert_eq!(error.tool(), "echo");
        assert_eq!(serde_json::to_value(error).unwrap()["kind"], kind);
    }
}

#[test]
fn dispatch_envelope_rejects_old_payloads_and_unknown_tags() {
    for payload in [
        serde_json::json!("old naked output"),
        serde_json::json!({"output": "old naked output"}),
        serde_json::json!({"status": "success"}),
        serde_json::json!({"status": "refused", "kind": "invented", "error": "bad"}),
        serde_json::json!({"Right": "accidental Either encoding"}),
        serde_json::json!({"status": "refused", "kind": "unknown_tool", "error": "bad"}),
        serde_json::json!({"status": "refused", "kind": "invalid_input", "error": "bad", "tool": "echo"}),
    ] {
        assert!(serde_json::from_value::<InstalledToolDispatchReply>(payload).is_err());
    }
}

#[test]
fn successful_dispatch_keeps_semantics_separate_from_presentation() {
    let semantic = serde_json::json!({"ok": true, "count": 3});
    let reply: InstalledToolDispatchReply = serde_json::from_value(serde_json::json!({
        "status": "success",
        "output": semantic,
        "presentation": "three items were accepted",
    }))
    .unwrap();

    let (value, presentation) = reply.into_parts().unwrap();
    assert_eq!(value, Some(semantic));
    assert_eq!(presentation, "three items were accepted");
}

#[test]
fn programmatic_actor_reply_remains_semantic_without_host_presentation() {
    let semantic = serde_json::json!({"accepted": true});
    let reply: ToolDispatchReply = serde_json::from_value(serde_json::json!({
        "status": "success",
        "output": semantic,
    }))
    .unwrap();
    assert_eq!(reply.into_output().unwrap(), semantic);
}

#[test]
fn malformed_completed_reply_is_not_a_dispatch_refusal() {
    let error =
        decode_tool_dispatch_reply(r#"{"status":"success","output":{"ok":true}}"#).unwrap_err();
    assert!(matches!(
        error,
        ResidentActorWorkbenchError::ActorProtocol(_)
    ));

    let error = decode_tool_dispatch_reply(
        r#"{"status":"refused","kind":"invalid_input","error":"bad input","tool":"inspect","detail":"wrong type"}"#,
    )
    .unwrap()
    .into_parts()
    .unwrap_err();
    assert!(matches!(error, ToolDispatchError::InvalidInput { .. }));
}
