//! One read-only projection for native status and authenticated GUI inspection.
use harness::{
    embedding::HostIdentity, model::OperationId, operation_observation::*, store::Store,
    turn::JobScheduler,
};
use std::sync::Arc;

pub(super) async fn observe(
    store: Arc<Store>,
    scheduler: Arc<JobScheduler>,
    observer: HostIdentity,
    operation: OperationId,
) -> Result<OperationObservation, OperationObservationError> {
    // Authenticate current conversation membership before reading live state.
    // SQLite facts share one snapshot; live sources are independently sampled.
    let mut observation = tokio::task::spawn_blocking(move || {
        store.observe_embedded_operation(&observer, &operation)
    })
    .await
    .map_err(|error| OperationObservationError::Unavailable(error.to_string()))?
    .map_err(|error| match error {
        harness::store::StoreError::OperationOriginMismatch
        | harness::store::StoreError::InvalidEmbeddedBinding => {
            OperationObservationError::Unauthorized
        }
        error => OperationObservationError::Unavailable(error.to_string()),
    })?;
    if let Some(live) = scheduler.observe_operation(&observation.operation).await {
        observation.execution = live.execution;
        observation.provenance.scheduler = true;
        observation.provenance.native = live.native.execution != NativeOperationExecution::Unknown
            || live.native.finalization != OperationFinalization::Unknown;
        observation.native_execution = live.native.execution;
        observation.finalization = live.native.finalization;
    }
    Ok(observation)
}

pub(super) fn model_reader(
    store: Arc<Store>,
    scheduler: Arc<JobScheduler>,
    recovery: Arc<exomonad_actor::ActorRecoveryJournal>,
) -> exomonad_actor::OperationObservationReader {
    Arc::new(move |actor, original| {
        let store = store.clone();
        let scheduler = scheduler.clone();
        let recovery = recovery.clone();
        Box::pin(async move {
            use exomonad_actor::OperationInspectionError as Error;
            let conversation = recovery
                .active_application_conversation(actor)
                .await
                .map_err(|error| Error::Unavailable(error.to_string()))?
                .ok_or_else(|| Error::Unavailable("actor has no bound conversation".into()))?;
            let observer = super::embedded_recovery::identity_from_conversation(&conversation)
                .map_err(Error::Unavailable)?;
            let exomonad_tool::ConversationOrigin::Embedded {
                run,
                actor,
                incarnation,
            } = original.origin
            else {
                return Err(Error::Unauthorized);
            };
            let operation = OperationId {
                origin: harness::model::ConversationIdentity::Embedded {
                    run,
                    actor: harness::model::AgentPath(actor),
                    incarnation,
                },
                request: harness::model::RequestId(original.request_id),
                call: harness::model::CallId(original.call_id),
            };
            let observed = observe(store, scheduler, observer, operation)
                .await
                .map_err(|error| match error {
                    OperationObservationError::Unauthorized => Error::Unauthorized,
                    OperationObservationError::Unavailable(detail) => Error::Unavailable(detail),
                })?;
            serde_json::to_value(observed).map_err(|error| Error::Unavailable(error.to_string()))
        })
    })
}

pub(super) fn gui_reader(
    store: Arc<Store>,
    scheduler: Arc<JobScheduler>,
) -> harness::server::OperationObserver {
    Arc::new(move |input| {
        let store = store.clone();
        let scheduler = scheduler.clone();
        Box::pin(observe(store, scheduler, input.observer, input.operation))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actor_host::{
        hosted_test_context::HostedTestRuntime,
        test_campaign::{hosted_script_provider_with_envelopes, hosted_test_settings},
    };
    use serde_json::json;
    use std::time::Duration;

    #[tokio::test]
    async fn native_status_and_authenticated_gui_observe_the_same_operation_while_notebook_is_busy()
    {
        let files = tempfile::tempdir().unwrap();
        let settings = hosted_test_settings(&files, 3);
        let (provider, mut requests) = hosted_script_provider_with_envelopes();
        let host = HostedTestRuntime::start(&settings, &provider)
            .await
            .expect("real host starts");
        host.run_scenario(|host| {
            Box::pin(async move {
                host.input("Inspect the exact pending notebook operation.")
                    .await
                    .unwrap();
                let first = host
                    .context
                    .while_root_live("initial provider request", requests.recv())
                    .await
                    .unwrap()
                    .unwrap();
                let observer = first.host_identity();
                let operation = first.operation("observation-busy");
                first.async_call("observation-busy", "sleep (seconds 300)");
                let next = host
                    .context
                    .while_root_live(
                        "provider continues while notebook is parked",
                        requests.recv(),
                    )
                    .await
                    .unwrap()
                    .unwrap();
                let pending = host
                    .context
                    .while_root_live(
                        "native operation admitted",
                        tokio::time::timeout(Duration::from_secs(60), async {
                            loop {
                                if let Ok(observed) = observe(
                                    host.runtime.store(),
                                    host.runtime.scheduler(),
                                    observer.clone(),
                                    operation.clone(),
                                )
                                .await
                                {
                                    if observed.native_execution
                                        == NativeOperationExecution::Pending
                                    {
                                        break observed;
                                    }
                                }
                                tokio::task::yield_now().await;
                            }
                        }),
                    )
                    .await
                    .unwrap()
                    .expect("native owner is independently observable");
                assert_eq!(pending.execution, OperationExecution::Running);
                assert_eq!(pending.settlement, OperationSettlement::Pending);
                assert_eq!(pending.delivery, OperationDelivery::NotInCurrentHistory);
                let original =
                    crate::actor_host::embedded_harness::original_operation(&observer, &operation)
                        .unwrap();
                next.function(
                    "inspect-operation",
                    "status",
                    json!({"view":"operation","operation":original}),
                );
                let after = host
                    .context
                    .while_root_live(
                        "native status bypasses busy notebook",
                        tokio::time::timeout(Duration::from_secs(30), requests.recv()),
                    )
                    .await
                    .unwrap()
                    .expect("status did not queue behind sleep")
                    .unwrap();
                let receipt = after.settled_output("inspect-operation");
                let model: OperationObservation = serde_json::from_str(
                    receipt["items"][0]["output"]
                        .as_str()
                        .expect("status metadata is explicit output"),
                )
                .unwrap();
                assert_eq!(model.operation, operation);
                assert_eq!(model.observer, observer);
                assert_eq!(model.native_execution, NativeOperationExecution::Pending);
                assert_eq!(model.settlement, OperationSettlement::Pending);
                assert_eq!(model.delivery, OperationDelivery::NotInCurrentHistory);

                let client = reqwest::Client::new();
                let api = format!("http://{}/api", host.address);
                let origin = format!("https://{}", host.address);
                let login = client
                    .post(format!("{api}/session"))
                    .header(reqwest::header::ORIGIN, &origin)
                    .json(&json!({"secret":"hosted-script-test-secret-32-bytes"}))
                    .send()
                    .await
                    .unwrap();
                assert_eq!(login.status(), reqwest::StatusCode::OK);
                let cookie = login.headers()[reqwest::header::SET_COOKIE]
                    .to_str()
                    .unwrap()
                    .split(';')
                    .next()
                    .unwrap();
                let response = client
                    .post(format!("{api}/operation"))
                    .header(reqwest::header::ORIGIN, &origin)
                    .header(reqwest::header::COOKIE, cookie)
                    .json(&json!({"observer":observer,"operation":operation}))
                    .send()
                    .await
                    .unwrap();
                assert_eq!(response.status(), reqwest::StatusCode::OK);
                let gui: OperationObservation = response.json().await.unwrap();
                assert_eq!(gui.operation, model.operation);
                assert_eq!(gui.observer, model.observer);
                assert_eq!(gui.native_execution, NativeOperationExecution::Pending);
                assert_eq!(gui.finalization, OperationFinalization::Pending);
                assert_eq!(gui.settlement, OperationSettlement::Pending);
                assert_eq!(gui.delivery, OperationDelivery::NotInCurrentHistory);
                assert!(host
                    .runtime
                    .scheduler()
                    .cancellation_acknowledgment(&operation)
                    .await
                    .unwrap()
                    .is_none());
                // Only this explicit cleanup control cancels work; observations did not.
                host.runtime.scheduler().cancel(&operation).await.unwrap();
                after.finish();
            })
        })
        .await;
    }
}
