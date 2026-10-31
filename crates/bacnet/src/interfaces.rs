//! The machine's IPv4 interfaces, for choosing which one to serve on.

use std::net::Ipv4Addr;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NetInterface {
    pub name: String,
    pub ip: Ipv4Addr,
    pub netmask: Ipv4Addr,
    /// The subnet's directed broadcast address.
    pub broadcast: Ipv4Addr,
}

/// IPv4 interfaces that are up. The one the default route uses comes first,
/// then the rest in the system's order, self-assigned (169.254) addresses
/// after those and loopback last.
pub fn interfaces() -> Vec<NetInterface> {
    let mut list = system_interfaces();
    list.dedup_by(|a, b| a.name == b.name && a.ip == b.ip);
    let routed = default_route_ip();
    list.sort_by_key(|i| {
        (
            i.ip.is_loopback(),
            i.ip.is_link_local(),
            Some(i.ip) != routed,
        )
    });
    list
}

/// The local address the OS would send internet traffic from. Connecting a
/// UDP socket only picks a route; nothing is sent.
fn default_route_ip() -> Option<Ipv4Addr> {
    let socket = std::net::UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    socket.connect((Ipv4Addr::new(192, 0, 2, 1), 47808)).ok()?;
    match socket.local_addr().ok()?.ip() {
        std::net::IpAddr::V4(ip) if !ip.is_unspecified() => Some(ip),
        _ => None,
    }
}

#[cfg(unix)]
fn system_interfaces() -> Vec<NetInterface> {
    use std::ffi::CStr;

    let mut list = Vec::new();
    let mut head: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: getifaddrs hands us a linked list that stays valid until the
    // freeifaddrs below; we only read it in between.
    if unsafe { libc::getifaddrs(&mut head) } != 0 {
        return list;
    }
    let mut cur = head;
    while !cur.is_null() {
        let ifa = unsafe { &*cur };
        cur = ifa.ifa_next;
        if ifa.ifa_flags & libc::IFF_UP as libc::c_uint == 0 {
            continue;
        }
        let Some(ip) = (unsafe { ipv4(ifa.ifa_addr) }) else {
            continue;
        };
        // A missing mask (seen on some tunnels) gets the limited broadcast.
        let netmask = unsafe { ipv4(ifa.ifa_netmask) }.unwrap_or(Ipv4Addr::UNSPECIFIED);
        let name = unsafe { CStr::from_ptr(ifa.ifa_name) }
            .to_string_lossy()
            .into_owned();
        let broadcast = Ipv4Addr::from(u32::from(ip) | !u32::from(netmask));
        list.push(NetInterface {
            name,
            ip,
            netmask,
            broadcast,
        });
    }
    unsafe { libc::freeifaddrs(head) };
    list
}

/// # Safety
/// `sa` must be null or point at a valid sockaddr.
#[cfg(unix)]
unsafe fn ipv4(sa: *const libc::sockaddr) -> Option<Ipv4Addr> {
    if sa.is_null() || unsafe { (*sa).sa_family } as libc::c_int != libc::AF_INET {
        return None;
    }
    let sin = unsafe { &*(sa as *const libc::sockaddr_in) };
    Some(Ipv4Addr::from(u32::from_be(sin.sin_addr.s_addr)))
}

#[cfg(windows)]
fn system_interfaces() -> Vec<NetInterface> {
    let Ok(addrs) = if_addrs::get_if_addrs() else {
        return Vec::new();
    };
    addrs
        .into_iter()
        .filter(|i| i.is_oper_up())
        .filter_map(|i| match i.addr {
            if_addrs::IfAddr::V4(v4) => Some(NetInterface {
                broadcast: v4
                    .broadcast
                    .unwrap_or(Ipv4Addr::from(u32::from(v4.ip) | !u32::from(v4.netmask))),
                ip: v4.ip,
                netmask: v4.netmask,
                name: i.name,
            }),
            _ => None,
        })
        .collect()
}

#[cfg(not(any(unix, windows)))]
fn system_interfaces() -> Vec<NetInterface> {
    vec![NetInterface {
        name: "loopback".into(),
        ip: Ipv4Addr::LOCALHOST,
        netmask: Ipv4Addr::new(255, 0, 0, 0),
        broadcast: Ipv4Addr::new(127, 255, 255, 255),
    }]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_is_listed_last() {
        let list = interfaces();
        assert!(list.iter().any(|i| i.ip == Ipv4Addr::LOCALHOST), "{list:?}");
        let first_loopback = list.iter().position(|i| i.ip.is_loopback()).unwrap();
        assert!(list[first_loopback..].iter().all(|i| i.ip.is_loopback()));
        let lo = list.iter().find(|i| i.ip == Ipv4Addr::LOCALHOST).unwrap();
        assert_eq!(lo.netmask, Ipv4Addr::new(255, 0, 0, 0));
        assert_eq!(lo.broadcast, Ipv4Addr::new(127, 255, 255, 255));
    }
}
