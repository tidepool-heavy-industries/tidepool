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
