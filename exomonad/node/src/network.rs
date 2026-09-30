//! Local network-interface observations for interactive service admission.

use std::io;
use std::net::IpAddr;

/// A tailnet listener must name an address currently assigned to tailscale0.
/// Membership in the Tailscale address ranges alone is not sufficient.
#[cfg(target_os = "linux")]
pub fn tailnet_address_is_local(address: IpAddr) -> io::Result<bool> {
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
        if candidate == address {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(not(target_os = "linux"))]
pub fn tailnet_address_is_local(_address: IpAddr) -> io::Result<bool> {
    Ok(false)
}
