use super::*;
use crate::operator::{self, OperatorService};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use tokio::sync::{Mutex, Semaphore};

struct Host {
    directory: tempfile::TempDir,
    service: OperatorService,
    client: reqwest::Client,
    calls: Arc<AtomicUsize>,
    entered: Arc<Semaphore>,
    release: Arc<Semaphore>,
    actors: Arc<Mutex<Vec<exomonad_actor::LocalActorRef>>>,
    tasks: Arc<Mutex<Vec<tokio::task::JoinHandle<()>>>>,
}

impl Host {
    async fn new(held: bool) -> Self {
        Self::with_cleanup(held, true).await
    }

    async fn with_cleanup(held: bool, confirmed: bool) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("operator/operator.sock");
        let calls = Arc::new(AtomicUsize::new(0));
        let entered = Arc::new(Semaphore::new(0));
        let release = Arc::new(Semaphore::new(if held { 0 } else { 100 }));
        let actors = Arc::new(Mutex::new(Vec::new()));
        let tasks = Arc::new(Mutex::new(Vec::new()));
        let service = OperatorService::bind(
            socket.clone(),
            {
                let calls = calls.clone();
                let entered = entered.clone();
                let release = release.clone();
                let actors = actors.clone();
                let tasks = tasks.clone();
                Arc::new(move || {
                    let calls = calls.clone();
                    let entered = entered.clone();
                    let release = release.clone();
                    let actors = actors.clone();
                    let tasks = tasks.clone();
                    Box::pin(async move {
                        calls.fetch_add(1, Ordering::SeqCst);
                        entered.add_permits(1);
                        release.acquire().await.unwrap().forget();
                        let (behavior, _, shutdown, _) =
                            operator::lifecycle_tests::behavior(confirmed, false);
                        shutdown.add_permits(1);
                        let (actor, task) = exomonad_actor::spawn_local_actor(None, behavior)
                            .await
                            .unwrap();
                        actors.lock().await.push(actor.clone());
                        tasks.lock().await.push(tokio::spawn(async move {
                            task.await.unwrap();
                        }));
                        Ok(actor)
                    })
                })
            },
            Arc::new(|_| Some(Vec::new())),
            Arc::new(|_, _| Box::pin(async { panic!("no artifact calls") })),
        )
        .await
        .unwrap();
        let client = reqwest::Client::builder()
            .unix_socket(socket)
            .retry(reqwest::retry::never())
            .build()
            .unwrap();
        Self {
            directory,
            service,
            client,
            calls,
            entered,
            release,
            actors,
            tasks,
        }
    }

    fn record_path(&self) -> PathBuf {
        self.directory.path().join("operator/proxy.json")
    }
    fn identity(&self) -> ServiceIdentity {
        self.service.state.service
    }
    fn count(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    async fn finish(self) {
        for actor in self.actors.lock().await.iter() {
            if actor.terminal().get().is_none() {
                actor
                    .shutdown(exomonad_actor::ActorTerminal {
                        kind: exomonad_actor::ActorExitKind::Cancelled,
                        summary: "test finished".into(),
                        diagnostic: None,
                    })
                    .await
                    .unwrap();
            }
        }
        self.service.shutdown().await;
        for task in self.tasks.lock().await.drain(..) {
            task.await.unwrap();
        }
    }
}

#[tokio::test]
async fn corrupt_or_unreadable_records_refuse_without_provisioning() {
    let host = Host::new(false).await;
    let path = host.record_path();
    assert!(read_proxy_record(&path).unwrap().is_none());
    for bytes in [
        b"{".as_slice(),
        b"null",
        b"{\"session\":3}",
        b"{\"version\":99,\"selection\":{\"state\":\"pending\",\"request\":{}}}",
    ] {
        std::fs::write(&path, bytes).unwrap();
        for fresh in [false, true] {
            assert!(
                resident_operator_session(host.directory.path(), &host.client, fresh)
                    .await
                    .is_err()
            );
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
            assert_eq!(host.count(), 0);
        }
    }
    let mut unsupported = ProxyRecord::new(ProxySelection::Pending {
        request: ProvisionRequest::new(host.identity()),
    });
    unsupported.version = 99;
    std::fs::write(&path, serde_json::to_vec(&unsupported).unwrap()).unwrap();
    assert!(
        resident_operator_session(host.directory.path(), &host.client, false)
            .await
            .is_err()
    );
    assert_eq!(host.count(), 0);
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    assert!(
        resident_operator_session(host.directory.path(), &host.client, false)
            .await
            .is_err()
    );
    assert!(path.is_dir());
    assert_eq!(host.count(), 0);
    host.finish().await;
}

#[tokio::test]
async fn lost_observer_and_concurrent_requests_share_one_admission() {
    let host = Host::new(true).await;
    let request = ProvisionRequest::new(host.identity());
    write_proxy_record(
        &host.record_path(),
        &ProxyRecord::new(ProxySelection::Pending { request }),
    )
    .unwrap();
    let state = host.service.state.clone();
    let observer = tokio::spawn(async move {
        operator::new(axum::extract::State(state), axum::Json(request)).await
    });
    host.entered.acquire().await.unwrap().forget();
    observer.abort();
    assert!(observer.await.unwrap_err().is_cancelled());
    let mut retries = Vec::new();
    for _ in 0..8 {
        let client = host.client.clone();
        retries.push(tokio::spawn(async move {
            client
                .post("http://localhost/host/operators")
                .json(&request)
                .send()
                .await
                .unwrap()
                .json::<Attachment>()
                .await
                .unwrap()
        }));
    }
    host.release.add_permits(1);
    let mut sessions = Vec::new();
    for retry in retries {
        sessions.push(retry.await.unwrap().session);
    }
    assert!(sessions.iter().all(|session| session == &sessions[0]));
    assert_eq!(host.count(), 1);
    let mut proxies = Vec::new();
    for _ in 0..4 {
        let client = host.client.clone();
        let root = host.directory.path().to_owned();
        proxies.push(tokio::spawn(async move {
            resident_operator_session(&root, &client, false)
                .await
                .unwrap()
        }));
    }
    for proxy in proxies {
        assert_eq!(proxy.await.unwrap(), sessions[0]);
    }
    assert_eq!(host.count(), 1);
    host.finish().await;
}

#[tokio::test]
async fn publication_fault_schedules_recover_exactly_one_actor() {
    let mut histories = 0;
    for published in [false, true] {
        for repetitions in [1, 2, 5] {
            let host = Host::new(false).await;
            let writer = move |path: &Path, record: &ProxyRecord| {
                if matches!(record.selection, ProxySelection::Ready { .. }) {
                    if published {
                        write_proxy_record(path, record)?;
                    }
                    Err(ProxyError("injected Ready publication failure".into()))
                } else {
                    write_proxy_record(path, record)
                }
            };
            assert!(
                resolve_proxy_selection(host.directory.path(), &host.client, false, &writer)
                    .await
                    .is_err()
            );
            assert_eq!(host.count(), 1);
            let original = host.actors.lock().await[0].identity();
            for _ in 0..repetitions {
                let selected =
                    resident_operator_session(host.directory.path(), &host.client, false)
                        .await
                        .unwrap();
                let sessions = host.service.state.sessions.lock().await;
                let Some(operator::LocalOperator::Available(actor)) = sessions.get(&selected)
                else {
                    panic!("selected live actor");
                };
                assert_eq!(actor.identity(), original);
                drop(sessions);
                assert_eq!(host.count(), 1);
            }
            histories += 1;
            host.finish().await;
        }
    }
    assert_eq!(histories, 6);
    eprintln!("proxy Ready publication fault histories: {histories}");
}

#[tokio::test]
async fn pending_publication_failure_never_submits_and_retry_keeps_visible_identity() {
    for published in [false, true] {
        let host = Host::new(false).await;
        let writer = move |path: &Path, record: &ProxyRecord| {
            if published {
                write_proxy_record(path, record)?;
            }
            Err(ProxyError("injected Pending publication failure".into()))
        };
        assert!(
            resolve_proxy_selection(host.directory.path(), &host.client, false, &writer)
                .await
                .is_err()
        );
        assert_eq!(host.count(), 0);
        let request = if published {
            let Some(StoredProxyRecord::Current(ProxyRecord {
                selection: ProxySelection::Pending { request },
                ..
            })) = read_proxy_record(&host.record_path()).unwrap()
            else {
                panic!("pending retained");
            };
            Some(request)
        } else {
            assert!(!host.record_path().exists());
            None
        };
        resident_operator_session(host.directory.path(), &host.client, false)
            .await
            .unwrap();
        assert_eq!(host.count(), 1);
        if let Some(request) = request {
            assert!(host
                .service
                .state
                .provisions
                .lock()
                .await
                .contains_key(&request.operation));
        }
        host.finish().await;
    }
}

#[tokio::test]
async fn service_change_refuses_pending_and_ready_even_when_fresh() {
    let host = Host::new(false).await;
    let foreign = ServiceIdentity {
        incarnation: uuid::Uuid::new_v4(),
    };
    for selection in [
        ProxySelection::Pending {
            request: ProvisionRequest::new(foreign),
        },
        ProxySelection::Ready {
            service: foreign,
            session: "old".into(),
            operation: None,
        },
    ] {
        let record = ProxyRecord::new(selection);
        write_proxy_record(&host.record_path(), &record).unwrap();
        let original = std::fs::read(host.record_path()).unwrap();
        for fresh in [false, true] {
            assert!(
                resident_operator_session(host.directory.path(), &host.client, fresh)
                    .await
                    .is_err()
            );
            assert_eq!(host.count(), 0);
            assert_eq!(std::fs::read(host.record_path()).unwrap(), original);
        }
    }
    let response = host
        .client
        .post("http://localhost/host/operators")
        .json(&ProvisionRequest::new(foreign))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::CONFLICT);
    assert_eq!(host.count(), 0);
    host.finish().await;
}

#[tokio::test]
async fn legacy_live_selection_migrates_but_dead_legacy_refuses() {
    let host = Host::new(false).await;
    let session = resident_operator_session(host.directory.path(), &host.client, false)
        .await
        .unwrap();
    std::fs::write(
        host.record_path(),
        serde_json::to_vec(&serde_json::json!({"session":session})).unwrap(),
    )
    .unwrap();
    assert_eq!(
        resident_operator_session(host.directory.path(), &host.client, false)
            .await
            .unwrap(),
        session
    );
    let Some(StoredProxyRecord::Current(ProxyRecord {
        selection: ProxySelection::Ready {
            service, operation, ..
        },
        ..
    })) = read_proxy_record(&host.record_path()).unwrap()
    else {
        panic!("migrated Ready");
    };
    assert_eq!(service, host.identity());
    assert!(operation.is_none());
    assert_eq!(host.count(), 1);
    std::fs::write(host.record_path(), b"{\"session\":\"missing\"}").unwrap();
    for fresh in [false, true] {
        assert!(
            resident_operator_session(host.directory.path(), &host.client, fresh)
                .await
                .is_err()
        );
        assert_eq!(host.count(), 1);
    }
    host.finish().await;
}

#[tokio::test]
async fn fresh_reconciles_pending_then_confirms_stop_before_new_identity() {
    let host = Host::new(false).await;
    let request = ProvisionRequest::new(host.identity());
    write_proxy_record(
        &host.record_path(),
        &ProxyRecord::new(ProxySelection::Pending { request }),
    )
    .unwrap();
    let selected = resident_operator_session(host.directory.path(), &host.client, true)
        .await
        .unwrap();
    assert_eq!(host.count(), 2);
    let actors = host.actors.lock().await;
    assert!(actors[0].terminal().get().is_some());
    assert!(actors[0].terminal().cleanup().unwrap().is_confirmed());
    assert!(actors[1].terminal().get().is_none());
    drop(actors);
    // Retired completion remains tied to the original admission forever in
    // this service; retrying it does not provision a replacement.
    let old: Attachment = host
        .client
        .post("http://localhost/host/operators")
        .json(&request)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_ne!(old.session, selected);
    assert_eq!(host.count(), 2);
    assert!(!session_alive(&host.client, &old.session).await.unwrap());
    host.finish().await;
}

#[tokio::test(flavor = "current_thread")]
async fn contended_lock_wait_is_cancellable_and_does_not_block_executor() {
    let directory = tempfile::tempdir().unwrap();
    let held = ProxyLock::acquire(directory.path()).await.unwrap();
    let root = directory.path().to_owned();
    let waiting = tokio::spawn(async move { ProxyLock::acquire(&root).await });
    tokio::time::timeout(
        std::time::Duration::from_secs(1),
        tokio::time::sleep(std::time::Duration::from_millis(30)),
    )
    .await
    .unwrap();
    waiting.abort();
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(1), waiting)
            .await
            .unwrap()
            .unwrap_err()
            .is_cancelled()
    );
    drop(held);
    drop(
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            ProxyLock::acquire(directory.path()),
        )
        .await
        .unwrap()
        .unwrap(),
    );
}

#[tokio::test]
async fn retained_provision_failure_is_not_retried() {
    let directory = tempfile::tempdir().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let service = OperatorService::bind(
        directory.path().join("operator/operator.sock"),
        {
            let calls = calls.clone();
            Arc::new(move || {
                calls.fetch_add(1, Ordering::SeqCst);
                Box::pin(async { Err("compiler unavailable".into()) })
            })
        },
        Arc::new(|_| None),
        Arc::new(|_, _| panic!("no artifacts")),
    )
    .await
    .unwrap();
    let client = reqwest::Client::builder()
        .unix_socket(service.state.socket.clone())
        .build()
        .unwrap();
    let request = ProvisionRequest::new(service.state.service);
    for _ in 0..3 {
        assert_eq!(
            client
                .post("http://localhost/host/operators")
                .json(&request)
                .send()
                .await
                .unwrap()
                .status(),
            reqwest::StatusCode::SERVICE_UNAVAILABLE
        );
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    service.shutdown().await;
}

#[tokio::test]
async fn confirmed_dead_ready_replaces_but_unknown_cleanup_refuses() {
    for confirmed in [false, true] {
        let host = Host::with_cleanup(false, confirmed).await;
        let selected = resident_operator_session(host.directory.path(), &host.client, false)
            .await
            .unwrap();
        let actor = host.actors.lock().await[0].clone();
        let result = actor
            .shutdown_with_cleanup(exomonad_actor::ActorTerminal {
                kind: exomonad_actor::ActorExitKind::Cancelled,
                summary: "test natural retirement".into(),
                diagnostic: None,
            })
            .await
            .unwrap();
        assert_eq!(result.cleanup.is_confirmed(), confirmed);
        let before = std::fs::read(host.record_path()).unwrap();
        let next = resident_operator_session(host.directory.path(), &host.client, false).await;
        if confirmed {
            assert_ne!(next.unwrap(), selected);
            assert_eq!(host.count(), 2);
        } else {
            assert!(next.is_err());
            assert_eq!(host.count(), 1);
            assert_eq!(std::fs::read(host.record_path()).unwrap(), before);
        }
        host.finish().await;
    }
}

#[tokio::test]
async fn fresh_refuses_unconfirmed_stop_without_admitting_next_operation() {
    let host = Host::with_cleanup(false, false).await;
    let request = ProvisionRequest::new(host.identity());
    write_proxy_record(
        &host.record_path(),
        &ProxyRecord::new(ProxySelection::Pending { request }),
    )
    .unwrap();
    for _ in 0..2 {
        assert!(
            resident_operator_session(host.directory.path(), &host.client, true)
                .await
                .is_err()
        );
        assert_eq!(host.count(), 1);
        let Some(StoredProxyRecord::Current(ProxyRecord {
            selection: ProxySelection::Ready { operation, .. },
            ..
        })) = read_proxy_record(&host.record_path()).unwrap()
        else {
            panic!("original result retained");
        };
        assert_eq!(operation, Some(request.operation));
        assert_eq!(host.service.state.provisions.lock().await.len(), 1);
    }
    host.finish().await;
}

#[tokio::test]
async fn changed_service_with_same_actor_identity_cannot_address_old_session() {
    let host = Host::new(false).await;
    let old = resident_operator_session(host.directory.path(), &host.client, false)
        .await
        .unwrap();
    let actor = host.actors.lock().await[0].clone();
    let directory = tempfile::tempdir().unwrap();
    let service = OperatorService::bind(
        directory.path().join("operator.sock"),
        {
            let actor = actor.clone();
            Arc::new(move || {
                let actor = actor.clone();
                Box::pin(async move { Ok(actor) })
            })
        },
        Arc::new(|_| Some(Vec::new())),
        Arc::new(|_, _| panic!("no artifact calls")),
    )
    .await
    .unwrap();
    let client = reqwest::Client::builder()
        .unix_socket(service.state.socket.clone())
        .retry(reqwest::retry::never())
        .build()
        .unwrap();
    let request = ProvisionRequest::new(service.state.service);
    let new: Attachment = client
        .post("http://localhost/host/operators")
        .json(&request)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_ne!(new.session, old);
    assert!(service.state.sessions.lock().await.get(&old).is_none());
    assert_eq!(
        client
            .get(format!("http://localhost/v1/sessions/{old}"))
            .send()
            .await
            .unwrap()
            .status(),
        reqwest::StatusCode::NOT_FOUND
    );
    for suffix in ["stop", "submit"] {
        let path = if suffix == "stop" {
            format!("http://localhost/host/operators/{old}/stop")
        } else {
            format!("http://localhost/v1/sessions/{old}/submit")
        };
        let response = client
            .post(path)
            .json(&SubmitRequest {
                source: "display (42 :: Int)".into(),
            })
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::NOT_FOUND);
    }
    assert!(actor.terminal().get().is_none());
    assert_eq!(
        client
            .get(format!("http://localhost/v1/sessions/{}", new.session))
            .send()
            .await
            .unwrap()
            .status(),
        reqwest::StatusCode::OK
    );
    service.shutdown().await;
    host.finish().await;
}

#[tokio::test]
async fn lost_or_malformed_provision_reply_retains_pending_for_reconciliation() {
    for lost in [false, true] {
        let host = Host::new(false).await;
        let request = ProvisionRequest::new(host.identity());
        write_proxy_record(
            &host.record_path(),
            &ProxyRecord::new(ProxySelection::Pending { request }),
        )
        .unwrap();
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("fault.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let owner = host.service.state.clone();
        let app = axum::Router::new().route(
            "/host/operators",
            axum::routing::post(move |request: axum::Json<ProvisionRequest>| {
                let owner = owner.clone();
                async move {
                    let response = operator::new(axum::extract::State(owner), request).await;
                    assert_eq!(response.status(), axum::http::StatusCode::OK);
                    drop(response);
                    let body = if lost {
                        axum::body::Body::from_stream(futures_util::stream::once(async {
                            Err::<Vec<u8>, std::io::Error>(std::io::Error::new(
                                std::io::ErrorKind::ConnectionReset,
                                "injected response loss",
                            ))
                        }))
                    } else {
                        axum::body::Body::from("{")
                    };
                    axum::response::Response::new(body)
                }
            }),
        );
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let client = reqwest::Client::builder()
            .unix_socket(socket.clone())
            .retry(reqwest::retry::never())
            .build()
            .unwrap();
        assert!(
            provision(&client, &host.record_path(), request, &write_proxy_record)
                .await
                .is_err()
        );
        let Some(StoredProxyRecord::Current(ProxyRecord {
            selection: ProxySelection::Pending { request: saved },
            ..
        })) = read_proxy_record(&host.record_path()).unwrap()
        else {
            panic!("pending retained");
        };
        assert_eq!(saved, request);
        assert_eq!(host.count(), 1);
        resident_operator_session(host.directory.path(), &host.client, false)
            .await
            .unwrap();
        assert_eq!(host.count(), 1);
        server.abort();
        assert!(server.await.unwrap_err().is_cancelled());
        host.finish().await;
    }
}
