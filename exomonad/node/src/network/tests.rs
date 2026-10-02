use super::*;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixListener;
use tokio::sync::mpsc;

const USER: u64 = 77;
const PEER: &str = "100.90.80.70:45678";

struct Fixture {
    verifier: TailscalePeerVerifier,
    requests: mpsc::UnboundedReceiver<String>,
    task: tokio::task::JoinHandle<()>,
    _directory: tempfile::TempDir,
}

impl Fixture {
    fn new(replies: Vec<String>, stall_body: bool) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("tailscaled.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let (sender, requests) = mpsc::unbounded_channel();
        let task = tokio::spawn(async move {
            for reply in replies {
                let (mut connection, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") {
                    let mut byte = [0];
                    connection.read_exact(&mut byte).await.unwrap();
                    request.push(byte[0]);
                    assert!(request.len() < 8192);
                }
                let _ = sender.send(String::from_utf8(request).unwrap());
                // Oversize replies may be cut off before the body finishes.
                let _ = connection.write_all(reply.as_bytes()).await;
                if stall_body {
                    std::future::pending::<()>().await;
                }
            }
        });
        let verifier =
            TailscalePeerVerifier::new(socket, vec![NonZeroU64::new(USER).unwrap()]).unwrap();
        Self {
            verifier,
            requests,
            task,
            _directory: directory,
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn response(status: u16, body: &str) -> String {
    format!(
        "HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

fn identity(peer: SocketAddr) -> Value {
    json!({
        "Node": { "User": USER, "Addresses": [format!("{}/{}", peer.ip(), if peer.is_ipv4() { 32 } else { 128 })] },
        "UserProfile": { "ID": USER, "LoginName": "display-only@example.test" },
        "UnrelatedUpstreamField": true,
    })
}

#[test]
fn configuration_rejects_relative_socket_empty_and_duplicate_allowlist() {
    let user = NonZeroU64::new(USER).unwrap();
    assert!(matches!(
        TailscalePeerVerifier::new("relative.sock".into(), vec![user]),
        Err(TailscalePeerConfigError::InvalidSocketPath)
    ));
    assert!(matches!(
        TailscalePeerVerifier::new("/tmp/socket".into(), vec![]),
        Err(TailscalePeerConfigError::EmptyAllowlist)
    ));
    assert!(matches!(
        TailscalePeerVerifier::new("/tmp/socket".into(), vec![user, user]),
        Err(TailscalePeerConfigError::DuplicateUserId)
    ));
}

#[tokio::test]
async fn whois_uses_actual_ipv4_and_ipv6_socket_peer() {
    for peer in [PEER, "[fd7a:115c:a1e0::abcd]:45678"] {
        let peer: SocketAddr = peer.parse().unwrap();
        let mut fixture = Fixture::new(vec![response(200, &identity(peer).to_string())], false);
        assert_eq!(fixture.verifier.authorize_peer(peer).await, Ok(()));
        let request = fixture.requests.recv().await.unwrap();
        let mut line = request.lines().next().unwrap().split_whitespace();
        assert_eq!(line.next(), Some("GET"));
        let url = reqwest::Url::parse(&format!(
            "http://local-tailscaled.sock{}",
            line.next().unwrap()
        ))
        .unwrap();
        assert_eq!(url.path(), "/localapi/v0/whois");
        assert_eq!(
            url.query_pairs().collect::<Vec<_>>(),
            vec![
                ("addr".into(), peer.to_string().into()),
                ("proto".into(), "tcp".into())
            ]
        );
        assert!(request
            .lines()
            .any(|line| line.eq_ignore_ascii_case("Host: local-tailscaled.sock")));
    }
}

#[tokio::test]
async fn identity_admission_rejects_wrong_user_tags_sharing_expiry_and_routes() {
    let peer: SocketAddr = PEER.parse().unwrap();
    let valid = identity(peer);
    for (path, value) in [
        ("/UserProfile/ID", json!(88)),
        ("/UserProfile/ID", json!(0)),
        ("/Node/User", json!(88)),
        ("/Node/Tags", json!(["tag:server"])),
        ("/Node/Sharer", json!(88)),
        ("/Node/Expired", json!(true)),
        ("/Node/Addresses", json!([])),
        ("/Node/Addresses", json!(["100.90.80.71/32"])),
        ("/Node/Addresses", json!(["100.90.80.70/24"])),
        ("/Node/Addresses", json!(["100.90.80.70"])),
    ] {
        let mut body = valid.clone();
        let (object, key) = path.rsplit_once('/').unwrap();
        body.pointer_mut(object).unwrap()[key] = value;
        let fixture = Fixture::new(vec![response(200, &body.to_string())], false);
        assert_eq!(
            fixture.verifier.authorize_peer(peer).await,
            Err(TailscalePeerError::Denied),
            "{path}"
        );
    }
}

#[tokio::test]
async fn non_tailnet_and_local_addresses_are_denied_before_lookup() {
    let directory = tempfile::tempdir().unwrap();
    let verifier = TailscalePeerVerifier::new(
        directory.path().join("missing.sock"),
        vec![NonZeroU64::new(USER).unwrap()],
    )
    .unwrap();
    for peer in [
        "127.0.0.1:1",
        "192.168.1.1:1",
        "8.8.8.8:1",
        "100.63.255.255:1",
        "100.128.0.0:1",
        "[::1]:1",
        "[fd7a:115c:a1e1::1]:1",
    ] {
        assert_eq!(
            verifier.authorize_peer(peer.parse().unwrap()).await,
            Err(TailscalePeerError::Denied)
        );
    }
    let local = local_tailnet_addresses().unwrap();
    for address in &local {
        assert_eq!(
            verifier.authorize_peer(SocketAddr::new(*address, 1)).await,
            Err(TailscalePeerError::Denied)
        );
    }
    println!("Checked {} actual local tailscale0 addresses", local.len());
}

#[tokio::test]
async fn lookup_fails_closed_and_recovers_without_caching_identity() {
    let peer = PEER.parse().unwrap();
    let good = identity(peer);
    let mut wrong = good.clone();
    wrong["UserProfile"]["ID"] = json!(88);
    let fixture = Fixture::new(
        vec![
            response(404, "unknown peer"),
            response(200, &good.to_string()),
            response(200, &wrong.to_string()),
            response(200, &good.to_string()),
        ],
        false,
    );
    for expected in [
        Err(TailscalePeerError::Denied),
        Ok(()),
        Err(TailscalePeerError::Denied),
        Ok(()),
    ] {
        assert_eq!(fixture.verifier.authorize_peer(peer).await, expected);
    }
    for (status, body) in [
        (403, "read forbidden"),
        (500, "unavailable"),
        (302, "redirect"),
        (200, "not JSON"),
        (200, "{}"),
        (200, r#"{"Node":null,"UserProfile":{"ID":77}}"#),
        (
            200,
            r#"{"Node":{"User":77,"Addresses":[]},"UserProfile":null}"#,
        ),
        (
            200,
            r#"{"Node":{"User":77,"Addresses":[]},"UserProfile":{"ID":"77"}}"#,
        ),
    ] {
        let fixture = Fixture::new(vec![response(status, body)], false);
        assert_eq!(
            fixture.verifier.authorize_peer(peer).await,
            Err(TailscalePeerError::Unavailable),
            "{status}: {body}"
        );
    }
    let directory = tempfile::tempdir().unwrap();
    let missing = TailscalePeerVerifier::new(
        directory.path().join("missing.sock"),
        vec![NonZeroU64::new(USER).unwrap()],
    )
    .unwrap();
    assert_eq!(
        missing.authorize_peer(peer).await,
        Err(TailscalePeerError::Unavailable)
    );
}

#[tokio::test]
async fn declared_and_chunked_body_limits_fail_closed() {
    let peer = PEER.parse().unwrap();
    let body = "x".repeat(WHOIS_BODY_LIMIT + 1);
    let chunked = format!("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n{:x}\r\n{body}\r\n0\r\n\r\n", body.len());
    for reply in [response(200, &body), chunked] {
        let fixture = Fixture::new(vec![reply], false);
        assert_eq!(
            fixture.verifier.authorize_peer(peer).await,
            Err(TailscalePeerError::Unavailable)
        );
    }
}

#[tokio::test]
async fn localapi_redirects_cannot_supply_an_identity() {
    let peer = PEER.parse().unwrap();
    let mut fixture = Fixture::new(
        vec![
            "HTTP/1.1 302 Found\r\nLocation: http://local-tailscaled.sock/redirected\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into(),
            response(200, &identity(peer).to_string()),
        ],
        false,
    );
    assert_eq!(
        fixture.verifier.authorize_peer(peer).await,
        Err(TailscalePeerError::Unavailable)
    );
    assert!(fixture
        .requests
        .recv()
        .await
        .unwrap()
        .contains("/localapi/v0/whois?"));
    assert!(fixture.requests.try_recv().is_err());
}

#[tokio::test]
async fn total_lookup_timeout_includes_body_read() {
    let fixture = Fixture::new(
        vec!["HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n".into()],
        true,
    );
    let start = std::time::Instant::now();
    assert_eq!(
        fixture.verifier.authorize_peer(PEER.parse().unwrap()).await,
        Err(TailscalePeerError::Unavailable)
    );
    assert!(start.elapsed() >= WHOIS_TIMEOUT);
    assert!(start.elapsed() < WHOIS_TIMEOUT + Duration::from_secs(2));
}
