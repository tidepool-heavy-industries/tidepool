//! Local network observations and direct Tailscale peer authentication.

use std::io;
use std::net::{IpAddr, SocketAddr};
use std::num::NonZeroU64;
use std::path::PathBuf;
use std::time::Duration;

const WHOIS_TIMEOUT: Duration = Duration::from_secs(2);
const WHOIS_BODY_LIMIT: usize = 64 * 1024;

/// Tailscale identity admission for direct TCP connections to a tailnet listener.
/// The caller supplies the accepted socket's peer, never a forwarding header.
pub struct TailscalePeerVerifier {
    client: reqwest::Client,
    allowed_user_ids: Vec<NonZeroU64>,
}

#[derive(Debug, thiserror::Error)]
pub enum TailscalePeerConfigError {
    #[error("Tailscale LocalAPI socket path must be absolute")]
    InvalidSocketPath,
    #[error("Tailscale user allowlist must be nonempty")]
    EmptyAllowlist,
    #[error("Tailscale user allowlist contains duplicate IDs")]
    DuplicateUserId,
    #[error("direct Tailscale peer authentication requires Linux")]
    UnsupportedPlatform,
    #[error("could not initialize Tailscale LocalAPI client")]
    ClientInitialization(#[source] reqwest::Error),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum TailscalePeerError {
    #[error("Tailscale peer is not authorized")]
    Denied,
    #[error("Tailscale peer authentication is unavailable")]
    Unavailable,
}

impl TailscalePeerVerifier {
    pub fn new(
        socket: PathBuf,
        mut allowed_user_ids: Vec<NonZeroU64>,
    ) -> Result<Self, TailscalePeerConfigError> {
        if !socket.is_absolute() {
            return Err(TailscalePeerConfigError::InvalidSocketPath);
        }
        if allowed_user_ids.is_empty() {
            return Err(TailscalePeerConfigError::EmptyAllowlist);
        }
        allowed_user_ids.sort_unstable();
        if allowed_user_ids.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(TailscalePeerConfigError::DuplicateUserId);
        }
        #[cfg(target_os = "linux")]
        {
            let client = reqwest::Client::builder()
                .unix_socket(socket)
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(WHOIS_TIMEOUT)
                .build()
                .map_err(TailscalePeerConfigError::ClientInitialization)?;
            Ok(Self {
                client,
                allowed_user_ids,
            })
        }
        #[cfg(not(target_os = "linux"))]
        Err(TailscalePeerConfigError::UnsupportedPlatform)
    }

    pub async fn authorize_peer(&self, peer: SocketAddr) -> Result<(), TailscalePeerError> {
        if !is_tailnet_peer_address(peer.ip()) {
            return Err(TailscalePeerError::Denied);
        }
        // Whois can resolve the daemon's own node. A local process connecting
        // from that address must not inherit the host owner's browser identity.
        if tailnet_address_is_local(peer.ip()).map_err(|_| TailscalePeerError::Unavailable)? {
            return Err(TailscalePeerError::Denied);
        }
        let response = self.whois(peer).await?;
        let user = NonZeroU64::new(response.user_profile.id).ok_or(TailscalePeerError::Denied)?;
        if self.allowed_user_ids.binary_search(&user).is_err()
            || response.node.user != user.get()
            || !response.node.tags.is_empty()
            || response.node.sharer != 0
            || response.node.expired
            || !response.node.addresses.iter().any(|address| {
                let Some((ip, prefix)) = address.split_once('/') else {
                    return false;
                };
                ip.parse::<IpAddr>().ok() == Some(peer.ip())
                    && prefix == if peer.is_ipv4() { "32" } else { "128" }
            })
        {
            return Err(TailscalePeerError::Denied);
        }
        Ok(())
    }

    async fn whois(&self, peer: SocketAddr) -> Result<WhoIsResponse, TailscalePeerError> {
        let mut url = reqwest::Url::parse("http://local-tailscaled.sock/localapi/v0/whois")
            .expect("static LocalAPI URL is valid");
        url.query_pairs_mut()
            .append_pair("addr", &peer.to_string())
            .append_pair("proto", "tcp");
        let mut response = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|_| TailscalePeerError::Unavailable)?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(TailscalePeerError::Denied);
        }
        if response.status() != reqwest::StatusCode::OK
            || response
                .content_length()
                .is_some_and(|length| length > WHOIS_BODY_LIMIT as u64)
        {
            return Err(TailscalePeerError::Unavailable);
        }
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| TailscalePeerError::Unavailable)?
        {
            if chunk.len() > WHOIS_BODY_LIMIT - body.len() {
                return Err(TailscalePeerError::Unavailable);
            }
            body.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&body).map_err(|_| TailscalePeerError::Unavailable)
    }
}

fn is_tailnet_peer_address(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => {
            let octets = address.octets();
            octets[0] == 100 && (64..=127).contains(&octets[1])
        }
        IpAddr::V6(address) => address.octets()[..6] == [0xfd, 0x7a, 0x11, 0x5c, 0xa1, 0xe0],
    }
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "PascalCase")]
struct WhoIsResponse {
    node: WhoIsNode,
    user_profile: WhoIsUserProfile,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "PascalCase")]
struct WhoIsNode {
    user: u64,
    addresses: Vec<String>,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    sharer: u64,
    #[serde(default)]
    expired: bool,
}

#[derive(serde::Deserialize)]
struct WhoIsUserProfile {
    #[serde(rename = "ID")]
    id: u64,
}

/// A tailnet listener must name an address currently assigned to tailscale0.
/// Membership in the Tailscale address ranges alone is not sufficient.
#[cfg(target_os = "linux")]
pub fn tailnet_address_is_local(address: IpAddr) -> io::Result<bool> {
    Ok(local_tailnet_addresses()?.contains(&address))
}

#[cfg(target_os = "linux")]
fn local_tailnet_addresses() -> io::Result<Vec<IpAddr>> {
    use std::ffi::CStr;
    use std::net::{Ipv4Addr, Ipv6Addr};

    struct Interfaces(*mut libc::ifaddrs);
    impl Drop for Interfaces {
        fn drop(&mut self) {
            // SAFETY: getifaddrs allocated this list; this guard is its sole owner.
            unsafe { libc::freeifaddrs(self.0) };
        }
    }

    let mut head = std::ptr::null_mut();
    // SAFETY: head points to writable storage for the allocated list.
    if unsafe { libc::getifaddrs(&mut head) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let interfaces = Interfaces(head);
    let mut addresses = Vec::new();
    let mut cursor = interfaces.0;
    while !cursor.is_null() {
        // SAFETY: cursor is a node in the live getifaddrs list.
        let interface = unsafe { &*cursor };
        cursor = interface.ifa_next;
        if interface.ifa_name.is_null()
            || interface.ifa_addr.is_null()
            || interface.ifa_flags & libc::IFF_UP as u32 == 0
        {
            continue;
        }
        // SAFETY: getifaddrs supplies a NUL-terminated interface name.
        if unsafe { CStr::from_ptr(interface.ifa_name) }.to_bytes() != b"tailscale0" {
            continue;
        }
        // SAFETY: getifaddrs supplies a sockaddr of the declared family.
        let candidate = unsafe {
            match i32::from((*interface.ifa_addr).sa_family) {
                libc::AF_INET => {
                    let socket = &*interface.ifa_addr.cast::<libc::sockaddr_in>();
                    IpAddr::V4(Ipv4Addr::from(socket.sin_addr.s_addr.to_ne_bytes()))
                }
                libc::AF_INET6 => {
                    let socket = &*interface.ifa_addr.cast::<libc::sockaddr_in6>();
                    IpAddr::V6(Ipv6Addr::from(socket.sin6_addr.s6_addr))
                }
                _ => continue,
            }
        };
        addresses.push(candidate);
    }
    Ok(addresses)
}

#[cfg(not(target_os = "linux"))]
pub fn tailnet_address_is_local(_address: IpAddr) -> io::Result<bool> {
    Ok(false)
}

#[cfg(all(test, target_os = "linux"))]
mod tests;
