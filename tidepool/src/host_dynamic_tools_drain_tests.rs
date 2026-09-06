use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tidepool_actor::ResidentToolFuture;
use tidepool_tool::CustomToolDeclaration;
use tokio::sync::Semaphore;

const THREAD: &str = "01a05a16-97f5-7722-aa8d-467e01e2e5b4";
const URL: &str = "http://localhost/v1/dynamic-tools";

struct GatedEndpoint {
    tools: Vec<HostedTool>,
    entered: Arc<Semaphore>,
    release: Arc<Semaphore>,
    calls: AtomicUsize,
    completions: AtomicUsize,
}
impl ResidentToolEndpoint for GatedEndpoint {
    fn tools(&self) -> &[HostedTool] {
        &self.tools
    }
    fn instructions(&self) -> Option<&str> {
        None
    }
    fn dispatch_boxed(&self, _: ToolInvocation) -> ResidentToolFuture {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.entered.add_permits(1);
        let release = self.release.clone();
        Box::pin(async move {
            release.acquire().await.unwrap().forget();
            Ok(serde_json::json!({"done":true}))
        })
    }
    fn complete_boxed(
        &self,
        _: tidepool_runtime::session::WorkbenchForkBoundary,
    ) -> ResidentToolFuture {
        self.completions.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(serde_json::json!({"completed":true})) })
    }
}
fn endpoint() -> Arc<GatedEndpoint> {
    Arc::new(GatedEndpoint {
        tools: vec![HostedTool::Custom(CustomToolDeclaration {
            name: "haskell".into(),
            description: "Run Haskell".into(),
        })],
        entered: Arc::new(Semaphore::new(0)),
        release: Arc::new(Semaphore::new(0)),
        calls: AtomicUsize::new(0),
        completions: AtomicUsize::new(0),
    })
}
fn client(socket: &std::path::Path) -> reqwest::Client {
    reqwest::Client::builder()
        .unix_socket(socket.to_path_buf())
        .no_proxy()
        .http1_only()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap()
}
fn call_request() -> serde_json::Value {
    serde_json::json!({"protocolVersion":PROTOCOL_VERSION,"threadId":THREAD,"turnId":"turn","callId":"call","contextCallId":"context","namespace":NAMESPACE,"tool":"haskell","arguments":"source"})
}
async fn attach(client: &reqwest::Client) {
    assert_eq!(
        client
            .post(format!("{URL}/session"))
            .json(&serde_json::json!({"protocolVersion":PROTOCOL_VERSION,"threadId":THREAD}))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NO_CONTENT
    );
}

#[tokio::test]
async fn http_quiesce_preserves_completion_and_drain_retains_blocked_call() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("tools.sock");
    let endpoint = endpoint();
    let service =
        HostDynamicToolService::new(endpoint.clone(), dir.path().join("binding"), None).unwrap();
    let control = service.control();
    let listener = UnixListener::bind(&socket).unwrap();
    let mut server = tokio::spawn(service.serve(listener));
    let keepalive = client(&socket);
    attach(&keepalive).await;
    let active_client = client(&socket);
    let active = tokio::spawn(async move {
        active_client
            .post(format!("{URL}/call"))
            .json(&call_request())
            .send()
            .await
            .unwrap()
            .json::<serde_json::Value>()
            .await
            .unwrap()
    });
    tokio::time::timeout(Duration::from_secs(5), endpoint.entered.acquire())
        .await
        .unwrap()
        .unwrap()
        .forget();
    control.quiesce();
    control.quiesce();
    for c in [&keepalive, &client(&socket)] {
        assert_eq!(
            c.get(format!("{URL}/registration"))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
        let denied: serde_json::Value = c
            .post(format!("{URL}/call"))
            .json(&call_request())
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(denied["success"], false);
        assert_eq!(
            c.post(format!("{URL}/session"))
                .json(&serde_json::json!({"protocolVersion":PROTOCOL_VERSION,"threadId":THREAD}))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(c.post(format!("{URL}/completed")).json(&serde_json::json!({"protocolVersion":PROTOCOL_VERSION,"threadId":THREAD,"contextCallId":"context"})).send().await.unwrap().status(),StatusCode::OK);
    }
    assert_eq!(endpoint.calls.load(Ordering::SeqCst), 1);
    assert_eq!(endpoint.completions.load(Ordering::SeqCst), 2);
    control.drain();
    control.quiesce(); // Cannot reopen.
    assert!(tokio::time::timeout(Duration::from_millis(50), &mut server)
        .await
        .is_err());
    endpoint.release.add_permits(1);
    assert_eq!(active.await.unwrap()["success"], true);
    tokio::time::timeout(Duration::from_secs(5), &mut server)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(client(&socket)
        .get(format!("{URL}/registration"))
        .send()
        .await
        .is_err());
}

#[tokio::test]
async fn http_idle_keepalive_connection_does_not_prevent_drain() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("tools.sock");
    let service =
        HostDynamicToolService::new(endpoint(), dir.path().join("binding"), None).unwrap();
    let control = service.control();
    let listener = UnixListener::bind(&socket).unwrap();
    let server = tokio::spawn(service.serve(listener));
    let c = client(&socket);
    c.get(format!("{URL}/registration"))
        .send()
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();
    let _idle = tokio::net::UnixStream::connect(&socket).await.unwrap();
    control.quiesce();
    control.drain();
    control.drain();
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn http_disconnected_client_does_not_authorize_effect_retirement() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("tools.sock");
    let endpoint = endpoint();
    let service =
        HostDynamicToolService::new(endpoint.clone(), dir.path().join("binding"), None).unwrap();
    let control = service.control();
    let listener = UnixListener::bind(&socket).unwrap();
    let mut server = tokio::spawn(service.serve(listener));
    let c = client(&socket);
    attach(&c).await;
    let active = tokio::spawn(async move {
        c.post(format!("{URL}/call"))
            .json(&call_request())
            .send()
            .await
    });
    tokio::time::timeout(Duration::from_secs(5), endpoint.entered.acquire())
        .await
        .unwrap()
        .unwrap()
        .forget();
    active.abort();
    assert!(active.await.unwrap_err().is_cancelled());
    control.quiesce();
    assert_eq!(endpoint.calls.load(Ordering::SeqCst), 1);
    // Release the endpoint explicitly; client cancellation is never used as an
    // endpoint completion signal. This fixture is not a resident-effect proof.
    endpoint.release.add_permits(1);
    control.drain();
    tokio::time::timeout(Duration::from_secs(5), &mut server)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(endpoint.calls.load(Ordering::SeqCst), 1);
}

struct FailingSealEndpoint {
    inner: Arc<GatedEndpoint>,
    seals: AtomicUsize,
    release_seal: Arc<Semaphore>,
}
impl ResidentToolEndpoint for FailingSealEndpoint {
    fn tools(&self) -> &[HostedTool] {
        self.inner.tools()
    }
    fn instructions(&self) -> Option<&str> {
        None
    }
    fn dispatch_boxed(&self, invocation: ToolInvocation) -> ResidentToolFuture {
        self.inner.dispatch_boxed(invocation)
    }
    fn complete_boxed(
        &self,
        boundary: tidepool_runtime::session::WorkbenchForkBoundary,
    ) -> ResidentToolFuture {
        self.inner.complete_boxed(boundary)
    }
    fn seal_hosted_work_boxed(
        &self,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<tidepool_actor::HostedWorkSeal, ResidentToolError>,
                > + Send
                + 'static,
        >,
    > {
        self.seals.fetch_add(1, Ordering::SeqCst);
        let release = self.release_seal.clone();
        Box::pin(async move {
            release.acquire().await.unwrap().forget();
            Err(ResidentToolError::Unavailable(
                "explicit seal failure".into(),
            ))
        })
    }
}

async fn assert_quiesced_but_completion_available(c: &reqwest::Client) {
    let denied: serde_json::Value = c
        .post(format!("{URL}/call"))
        .json(&call_request())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(denied["success"], false);
    assert_eq!(
        c.post(format!("{URL}/session"))
            .json(&serde_json::json!({"protocolVersion":PROTOCOL_VERSION,"threadId":THREAD}))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(c.post(format!("{URL}/completed"))
        .json(&serde_json::json!({"protocolVersion":PROTOCOL_VERSION,"threadId":THREAD,"contextCallId":"context"}))
        .send().await.unwrap().status(), StatusCode::OK);
}

#[tokio::test]
async fn http_seal_unsupported_quiesces_before_poll_without_draining() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("tools.sock");
    let endpoint = endpoint(); // Uses the real trait's unsupported default.
    let service =
        HostDynamicToolService::new(endpoint.clone(), dir.path().join("binding"), None).unwrap();
    let control = service.control();
    let mut server = tokio::spawn(service.serve(UnixListener::bind(&socket).unwrap()));
    let c = client(&socket);
    attach(&c).await;
    let seal =
        control.quiesce_and_seal(tidepool_actor::ActorRef::first(tidepool_actor::ActorId(41)));
    assert_quiesced_but_completion_available(&c).await; // Future not polled yet.
    assert!(matches!(
        seal.await,
        Err(HostToolSealError::Endpoint(ResidentToolError::Unavailable(
            _
        )))
    ));
    assert_quiesced_but_completion_available(&client(&socket)).await;
    assert_eq!(endpoint.calls.load(Ordering::SeqCst), 0);
    assert_eq!(endpoint.completions.load(Ordering::SeqCst), 2);
    assert!(tokio::time::timeout(Duration::from_millis(30), &mut server)
        .await
        .is_err());
    control.drain();
    tokio::time::timeout(Duration::from_secs(5), &mut server)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn http_seal_timeout_retains_single_future_and_failure_keeps_completion() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("tools.sock");
    let endpoint = Arc::new(FailingSealEndpoint {
        inner: endpoint(),
        seals: AtomicUsize::new(0),
        release_seal: Arc::new(Semaphore::new(0)),
    });
    let service =
        HostDynamicToolService::new(endpoint.clone(), dir.path().join("binding"), None).unwrap();
    let control = service.control();
    let mut server = tokio::spawn(service.serve(UnixListener::bind(&socket).unwrap()));
    let c = client(&socket);
    attach(&c).await;
    let mut seal =
        control.quiesce_and_seal(tidepool_actor::ActorRef::first(tidepool_actor::ActorId(42)));
    assert_eq!(endpoint.seals.load(Ordering::SeqCst), 0);
    assert_quiesced_but_completion_available(&c).await;
    for _ in 0..2 {
        assert!(tokio::time::timeout(Duration::from_millis(30), &mut seal)
            .await
            .is_err());
        assert_eq!(endpoint.seals.load(Ordering::SeqCst), 1);
    }
    endpoint.release_seal.add_permits(1);
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(5), &mut seal)
            .await
            .unwrap(),
        Err(HostToolSealError::Endpoint(ResidentToolError::Unavailable(
            _
        )))
    ));
    assert_eq!(endpoint.seals.load(Ordering::SeqCst), 1);
    assert_quiesced_but_completion_available(&client(&socket)).await;
    assert_eq!(endpoint.inner.calls.load(Ordering::SeqCst), 0);
    assert_eq!(endpoint.inner.completions.load(Ordering::SeqCst), 2);
    assert!(tokio::time::timeout(Duration::from_millis(30), &mut server)
        .await
        .is_err());
    control.drain();
    tokio::time::timeout(Duration::from_secs(5), &mut server)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn http_seal_already_draining_never_invokes_endpoint() {
    for serve_first in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("tools.sock");
        let endpoint = Arc::new(FailingSealEndpoint {
            inner: endpoint(),
            seals: AtomicUsize::new(0),
            release_seal: Arc::new(Semaphore::new(0)),
        });
        let service =
            HostDynamicToolService::new(endpoint.clone(), dir.path().join("binding"), None)
                .unwrap();
        let control = service.control();
        let listener = UnixListener::bind(&socket).unwrap();
        let mut server = if serve_first {
            let server = tokio::spawn(service.serve(listener));
            attach(&client(&socket)).await;
            control.drain();
            server
        } else {
            control.drain();
            tokio::spawn(service.serve(listener))
        };
        let seal =
            control.quiesce_and_seal(tidepool_actor::ActorRef::first(tidepool_actor::ActorId(43)));
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(5), seal)
                .await
                .unwrap(),
            Err(HostToolSealError::AlreadyDraining)
        ));
        assert_eq!(endpoint.seals.load(Ordering::SeqCst), 0);
        assert!(!control.admits(AdmissionKind::CompletionOrRead));
        tokio::time::timeout(Duration::from_secs(5), &mut server)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }
}
