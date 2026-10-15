//! Authored synchronous reload refusal preserves independent notebook work.
use super::*;

async fn hosted_tool(
    fixture: &ConcurrentResident,
    key: &str,
    name: &str,
    arguments: serde_json::Value,
) -> ResidentCellCall {
    let policy = fixture.policy.clone();
    let context = reload_context(key);
    let name = name.to_owned();
    let mut invocation = policy.dispatch_json_boxed(ToolInvocation {
        context: Some(context),
        name,
        arguments: ToolArguments::Structured(arguments),
    });
    // The local endpoint submits its workbench message synchronously on first poll.
    // Submit this call before the caller sends the next control or cell request.
    let initial = std::future::poll_fn(|cx| {
        std::task::Poll::Ready(std::future::Future::poll(invocation.as_mut(), cx))
    })
    .await;
    tokio::spawn(async move {
        match initial {
            std::task::Poll::Ready(reply) => reply,
            std::task::Poll::Pending => invocation.await,
        }
    })
}

fn reload_context(key: &str) -> ToolInvocationContext {
    ToolInvocationContext::external(
        "reload-progress".into(),
        key.into(),
        key.into(),
        Some(key.into()),
        None,
    )
}

#[tokio::test]
async fn unavailable_reload_leaves_parked_cells_and_control_independent() {
    let markers = ["reload-A", "reload-B"];
    let mut fixture = ConcurrentResident::new_with_source(191, &markers, None).await;
    fixture.read("reload-seed", "x <- pure (0 :: Int)").await;
    let source = include_str!("shadow_cell.hs");
    let mut a = fixture.spawn_cell(
        markers[0],
        source
            .replace("OLD_BINDING", "oldA")
            .replace("MARKER", markers[0])
            .replace("RESULT_VALUE", "11"),
    );
    let a_execution = fixture.wait_started(markers[0], &mut a).await;
    let mut b = fixture.spawn_cell(
        markers[1],
        source
            .replace("OLD_BINDING", "oldB")
            .replace("MARKER", markers[1])
            .replace("RESULT_VALUE", "22"),
    );
    let b_execution = fixture.wait_started(markers[1], &mut b).await;
    let reload = ConcurrentResident::settle(
        hosted_tool(
            &fixture,
            "unavailable-reload",
            "reload",
            serde_json::json!({}),
        )
        .await,
    )
    .await
    .unwrap();
    assert_eq!(reload["status"], "committed");
    assert_eq!(
        reload["items"][0]["value"]["Left"]["tag"],
        "ReloadUnavailable"
    );
    assert!(!a.is_finished() && !b.is_finished());
    fixture.read("reload-C", "c <- pure (42 :: Int)").await;
    let status = ConcurrentResident::settle(
        hosted_tool(&fixture, "reload-status", "status", serde_json::json!({})).await,
    )
    .await
    .unwrap();
    assert_eq!(status["status"], "committed");
    fixture.release(markers[0]);
    assert_eq!(
        committed_execution(&ConcurrentResident::settle(a).await.unwrap()),
        a_execution
    );
    fixture.release(markers[1]);
    assert_eq!(
        committed_execution(&ConcurrentResident::settle(b).await.unwrap()),
        b_execution
    );
    fixture
        .check_value(
            "reload-joined",
            "x == 22 && c == 42 && oldA == 0 && oldB == 0",
        )
        .await;
    fixture.shutdown(&markers).await;
}

#[tokio::test]
async fn invalid_authored_reload_arguments_release_the_invocation() {
    let fixture = ConcurrentResident::new_with_source(192, &[], None).await;
    fixture
        .read("reload-kept", "kept <- pure (42 :: Int)")
        .await;
    let rejected = ConcurrentResident::settle(
        hosted_tool(
            &fixture,
            "invalid-reload",
            "reload",
            serde_json::json!({"also_check":true}),
        )
        .await,
    )
    .await
    .unwrap();
    assert_eq!(rejected["status"], "rejected");
    fixture
        .check_value("retained-after-refusal", "kept == 42")
        .await;
    let next = ConcurrentResident::settle(
        hosted_tool(&fixture, "next-reload", "reload", serde_json::json!({})).await,
    )
    .await
    .unwrap();
    assert_eq!(
        next["items"][0]["value"]["Left"]["tag"],
        "ReloadUnavailable"
    );
    fixture.shutdown(&[]).await;
}
