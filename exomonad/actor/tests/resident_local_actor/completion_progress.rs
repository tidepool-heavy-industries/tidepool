//! The production hosted result acknowledgement is scoped to its own execution.

use super::*;
use tidepool_runtime::session::ContextCheckpointBoundary;

fn boundary(key: &str) -> ContextCheckpointBoundary {
    ContextCheckpointBoundary::external("concurrent-publication".into(), key.into(), key.into())
}

async fn submit_ack(fixture: &ConcurrentResident, key: &str) -> ResidentCellCall {
    let mut acknowledgement = fixture.policy.complete_boxed(boundary(key));
    let initial = std::future::poll_fn(|cx| {
        std::task::Poll::Ready(std::future::Future::poll(acknowledgement.as_mut(), cx))
    })
    .await;
    tokio::spawn(async move {
        match initial {
            std::task::Poll::Ready(reply) => reply,
            std::task::Poll::Pending => acknowledgement.await,
        }
    })
}

#[tokio::test]
async fn parked_cell_does_not_block_another_calls_acknowledgement_or_control() {
    let markers = ["completion-A"];
    let mut fixture = ConcurrentResident::new(194, &markers).await;
    fixture
        .read("completion-seed", "x <- pure (0 :: Int)")
        .await;
    let mut a = fixture.spawn_cell(
        markers[0],
        include_str!("shadow_cell.hs")
            .replace("OLD_BINDING", "oldA")
            .replace("MARKER", markers[0])
            .replace("RESULT_VALUE", "11"),
    );
    let a_execution = fixture.wait_started(markers[0], &mut a).await;
    let own_ack = submit_ack(&fixture, markers[0]).await;
    let b = fixture
        .read(
            "completion-B",
            "b <- pure (42 :: Int)\nif b == 42 then pure () else error \"B binding changed\"",
        )
        .await;
    let before_ack = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        fixture
            .policy
            .reconcile_workbench_boxed(boundary("completion-B")),
    )
    .await
    .expect("settled B reconciliation progresses while A parks")
    .expect("B reconcile reply");
    assert!(
        matches!(
            before_ack,
            exomonad_actor::WorkbenchBoundaryReconciliation::Recovered { .. }
        ),
        "{before_ack:?}"
    );
    let cancellation = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        fixture
            .policy
            .cancel_workbench_boxed(ConcurrentResident::cell_context("completion-B")),
    )
    .await
    .expect("settled B cancellation inspection progresses")
    .expect("B cancellation reply");
    let exomonad_actor::WorkbenchCancellationOutcome::PublicationSettled {
        reply: Ok(retained),
        ..
    } = cancellation
    else {
        panic!("B's committed publication was not retained: {cancellation:?}");
    };
    assert_eq!(serde_json::to_value(retained).unwrap(), b);
    let ack = submit_ack(&fixture, "completion-B").await;
    let acknowledgement = tokio::time::timeout(std::time::Duration::from_secs(5), ack)
        .await
        .expect("B's real result acknowledgement completes before A resumes")
        .expect("B acknowledgement task")
        .expect("B acknowledgement accepted");
    assert!(acknowledgement.is_null());
    fixture
        .check_value("completion-C", "b + (1 :: Int) == 43")
        .await;
    let status = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        fixture.policy.dispatch_json_boxed(ToolInvocation {
            context: Some(ConcurrentResident::cell_context("completion-status")),
            name: "status".into(),
            arguments: ToolArguments::Structured(serde_json::json!({"view": "summary"})),
        }),
    )
    .await
    .expect("status progresses after B acknowledgement")
    .expect("status reply");
    assert_eq!(status["status"], "committed", "{status:?}");
    assert!(
        !a.is_finished() && !own_ack.is_finished(),
        "A's own boundary is still fenced"
    );
    let after_ack = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        fixture
            .policy
            .reconcile_workbench_boxed(boundary("completion-B")),
    )
    .await
    .expect("B acknowledgement remains observable")
    .expect("B settled boundary");
    assert!(matches!(
        after_ack,
        exomonad_actor::WorkbenchBoundaryReconciliation::Settled
    ));
    fixture.release(markers[0]);
    let a_reply = ConcurrentResident::settle(a).await.expect("A resumes");
    assert_eq!(committed_execution(&a_reply), a_execution);
    tokio::time::timeout(std::time::Duration::from_secs(5), own_ack)
        .await
        .expect("A acknowledgement follows actual A settlement")
        .expect("A ack task")
        .expect("A ack accepted");
    fixture.shutdown(&markers).await;
}
