//! Kernel `netchan` — qcommon/net_chan.c (`Netchan_*`: sequencing, the
//! qport, the challenge checksum of protocol 71, fragmentation of messages
//! of `FRAGMENT_SIZE` and more, the loopback buffers, out-of-band packets)
//! and qcommon/net_ip.c (`netadr_t`, `NET_StringToAdr` with the
//! `net_enabled` family priority, `NET_CompareAdr` /
//! `NET_CompareBaseAdrMask`, `Sys_IsLANAddress`, `NET_OpenIP` with
//! `NET_IPSocket` and `NET_IP6Socket` (IPV6_V6ONLY), `NET_SetMulticast6`,
//! `NET_JoinMulticast6` / `NET_LeaveMulticast6` and the NA_MULTICAST6 LAN
//! scan, over Rust's non-blocking `UdpSocket`; dec037).
//!
//! One `Net` is the C's pair of sockets (NS_CLIENT / NS_SERVER) with the
//! loopback queues between them; the host passes it to the server and the
//! client each frame (dec033). Ports are kept in host byte order; the C's
//! `BigShort` conversions disappear.

use super::huffman;
use super::msg::{Msg, MAX_MSGLEN};
use std::collections::VecDeque;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs, UdpSocket};

/// `MAX_PACKETLEN`: max size of a network packet.
pub const MAX_PACKETLEN: usize = 1400;
/// `FRAGMENT_SIZE`.
pub const FRAGMENT_SIZE: usize = MAX_PACKETLEN - 100;
/// `FRAGMENT_BIT`.
pub const FRAGMENT_BIT: u32 = 1 << 31;
/// `PORT_MASTER` / `PORT_SERVER` / `NUM_SERVER_PORTS`.
pub const PORT_MASTER: u16 = 30710;
pub const PORT_SERVER: u16 = 30720;
pub const NUM_SERVER_PORTS: u16 = 4;
/// `MAX_LOOPBACK`.
const MAX_LOOPBACK: usize = 16;
/// `net_enabled` bits (qcommon.h).
pub const NET_ENABLEV4: i32 = 0x01;
pub const NET_ENABLEV6: i32 = 0x02;
/// if this flag is set, always attempt ipv6 connections instead of ipv4 if a v6 address is found.
pub const NET_PRIOV6: i32 = 0x04;
/// disables ipv6 multicast support if set.
pub const NET_DISABLEMCAST: i32 = 0x08;
/// `NET_MULTICAST_IP6`: the LAN scan group ("ioquake3" in hex).
pub const NET_MULTICAST_IP6: &str = "ff04::696f:7175:616b:6533";

/// The process-wide `net_enabled` that `NET_StringToAdr` reads (the C
/// cvar is global too); hosts set it with `set_net_enabled`.
static NET_ENABLED: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(NET_ENABLEV4 | NET_ENABLEV6);

/// `net_enabled` for address resolution (`NET_StringToAdr`).
pub fn set_net_enabled(v: i32) {
    NET_ENABLED.store(v, std::sync::atomic::Ordering::Relaxed);
}

pub fn net_enabled() -> i32 {
    NET_ENABLED.load(std::sync::atomic::Ordering::Relaxed)
}

/// `netadrtype_t`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum NetAdrType {
    #[default]
    Bad,
    Loopback,
    Broadcast,
    Ip,
    Ip6,
    Multicast6,
    Unspec,
}

/// `netadr_t` (port in host order).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct NetAdr {
    pub kind: NetAdrType,
    pub ip: [u8; 4],
    pub ip6: [u8; 16],
    pub port: u16,
    /// `scope_id` of a link-local ipv6 address
    pub scope_id: u32,
}

/// `netsrc_t`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NetSrc {
    Client = 0,
    Server = 1,
}

impl NetAdr {
    pub fn loopback() -> Self {
        NetAdr { kind: NetAdrType::Loopback, ..Default::default() }
    }
    /// `SockadrToNetadr`.
    pub fn from_socket(s: &SocketAddr) -> Self {
        match s {
            SocketAddr::V4(a) => NetAdr { kind: NetAdrType::Ip, ip: a.ip().octets(), port: a.port(), ..Default::default() },
            SocketAddr::V6(a) => match a.ip().to_ipv4_mapped() {
                Some(v4) => NetAdr { kind: NetAdrType::Ip, ip: v4.octets(), port: a.port(), ..Default::default() },
                None => NetAdr { kind: NetAdrType::Ip6, ip6: a.ip().octets(), port: a.port(), scope_id: a.scope_id(), ..Default::default() },
            },
        }
    }
    /// `NetadrToSockadr`.
    pub fn to_socket(&self) -> Option<SocketAddr> {
        match self.kind {
            NetAdrType::Ip => Some(SocketAddr::new(IpAddr::V4(Ipv4Addr::from(self.ip)), self.port)),
            NetAdrType::Broadcast => Some(SocketAddr::new(IpAddr::V4(Ipv4Addr::BROADCAST), self.port)),
            NetAdrType::Ip6 => Some(SocketAddr::V6(std::net::SocketAddrV6::new(Ipv6Addr::from(self.ip6), self.port, 0, self.scope_id))),
            _ => None,
        }
    }
    /// `NET_AdrToString`.
    pub fn to_string_base(&self) -> String {
        match self.kind {
            NetAdrType::Loopback => "loopback".into(),
            NetAdrType::Ip | NetAdrType::Broadcast => Ipv4Addr::from(self.ip).to_string(),
            // Sys_SockaddrToString: getnameinfo( NI_NUMERICHOST ), "%scope" when scoped
            NetAdrType::Ip6 if self.scope_id != 0 => format!("{}%{}", Ipv6Addr::from(self.ip6), scope_name(self.scope_id)),
            NetAdrType::Ip6 => Ipv6Addr::from(self.ip6).to_string(),
            _ => String::new(),
        }
    }
    /// `NET_AdrToStringwPort`.
    pub fn to_string_port(&self) -> String {
        match self.kind {
            NetAdrType::Loopback => "loopback".into(),
            NetAdrType::Ip | NetAdrType::Broadcast => format!("{}:{}", self.to_string_base(), self.port),
            NetAdrType::Ip6 => format!("[{}]:{}", self.to_string_base(), self.port),
            _ => String::new(),
        }
    }
}

/// `NET_CompareBaseAdrMask`: the first `netmask` bits of the address
/// (-1: all of them).
pub fn compare_base_adr_mask(a: &NetAdr, b: &NetAdr, netmask: i32) -> bool {
    if a.kind != b.kind {
        return false;
    }
    if a.kind == NetAdrType::Loopback {
        return true;
    }
    let (addra, addrb, mut netmask): (&[u8], &[u8], i32) = match a.kind {
        NetAdrType::Ip => (&a.ip, &b.ip, if !(0..=32).contains(&netmask) { 32 } else { netmask }),
        NetAdrType::Ip6 => (&a.ip6, &b.ip6, if !(0..=128).contains(&netmask) { 128 } else { netmask }),
        _ => return false,
    };
    let curbyte = (netmask >> 3) as usize;
    if curbyte > 0 && addra[..curbyte] != addrb[..curbyte] {
        return false;
    }
    netmask &= 7;
    if netmask != 0 {
        let cmpmask = (((1u32 << netmask) - 1) << (8 - netmask)) as u8;
        (addra[curbyte] & cmpmask) == (addrb[curbyte] & cmpmask)
    } else {
        true
    }
}

/// `NET_CompareBaseAdr`: compares without the port.
pub fn compare_base_adr(a: &NetAdr, b: &NetAdr) -> bool {
    compare_base_adr_mask(a, b, -1)
}

/// `NET_CompareAdr`.
pub fn compare_adr(a: &NetAdr, b: &NetAdr) -> bool {
    if !compare_base_adr(a, b) {
        return false;
    }
    match a.kind {
        NetAdrType::Ip | NetAdrType::Ip6 => a.port == b.port,
        _ => true,
    }
}

/// `NET_IsLocalAddress`.
pub fn is_local_address(a: &NetAdr) -> bool {
    a.kind == NetAdrType::Loopback
}

/// `Sys_IsLANAddress` (RFC 1918, loopback and link-local; the comparison
/// against the host's own interfaces is left to the OS routing).
pub fn is_lan_address(a: &NetAdr) -> bool {
    match a.kind {
        NetAdrType::Loopback => true,
        NetAdrType::Ip => a.ip[0] == 10 || (a.ip[0] == 172 && (a.ip[1] & 0xf0) == 16) || (a.ip[0] == 192 && a.ip[1] == 168) || a.ip[0] == 127,
        NetAdrType::Ip6 => (a.ip6[0] == 0xfe && (a.ip6[1] & 0xc0) == 0x80) || (a.ip6[0] & 0xfe) == 0xfc || a.ip6 == Ipv6Addr::LOCALHOST.octets(),
        _ => false,
    }
}

/// `NET_StringToAdr`: 0 not found, 1 found with a port, 2 found without
/// one (the port is then PORT_SERVER). "localhost" is the loopback.
/// This machine's name (`gethostname`).
fn host_name() -> String {
    #[cfg(windows)]
    {
        std::env::var("COMPUTERNAME").unwrap_or_default()
    }
    #[cfg(unix)]
    {
        let mut buf = [0u8; 256];
        // SAFETY: the buffer and its length
        let r = unsafe { libc::gethostname(buf.as_mut_ptr() as *mut libc::c_char, buf.len()) };
        if r != 0 {
            return String::new();
        }
        let n = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
        String::from_utf8_lossy(&buf[..n]).into_owned()
    }
    #[cfg(not(any(windows, unix)))]
    {
        String::new()
    }
}

/// This machine's IPv4 addresses on its networks, loopback and link-local
/// left out: every adapter's (`getifaddrs`; on Windows the host name's
/// addresses, which are every adapter's). Port addition (dec045): the LAN
/// scan and the server's "join me at" line.
pub fn local_ipv4() -> Vec<Ipv4Addr> {
    let mut out: Vec<Ipv4Addr> = Vec::new();
    #[cfg(unix)]
    // SAFETY: getifaddrs' list, walked and freed here
    unsafe {
        let mut ifs: *mut libc::ifaddrs = std::ptr::null_mut();
        if libc::getifaddrs(&mut ifs) == 0 {
            let mut p = ifs;
            while !p.is_null() {
                let a = (*p).ifa_addr;
                if !a.is_null() && (*a).sa_family as i32 == libc::AF_INET {
                    let sin = &*(a as *const libc::sockaddr_in);
                    out.push(Ipv4Addr::from(u32::from_be(sin.sin_addr.s_addr)));
                }
                p = (*p).ifa_next;
            }
            libc::freeifaddrs(ifs);
        }
    }
    let host = host_name();
    if !host.is_empty() {
        if let Ok(it) = (host.as_str(), 0u16).to_socket_addrs() {
            out.extend(it.filter_map(|a| match a.ip() {
                IpAddr::V4(v) => Some(v),
                _ => None,
            }));
        }
    }
    out.retain(|v| !v.is_loopback() && !v.is_link_local() && !v.is_unspecified());
    out.sort();
    out.dedup();
    out
}

/// The subnet-directed broadcast of each local address (as a /24, the
/// home-network size). Windows sends 255.255.255.255 out of one adapter
/// only (the best route: a VPN's or a virtual machine's adapter can take
/// it), so the LAN scan also broadcasts on each network by its own address.
pub fn lan_broadcasts(locals: &[Ipv4Addr]) -> Vec<[u8; 4]> {
    let mut out: Vec<[u8; 4]> = locals.iter().map(|a| { let o = a.octets(); [o[0], o[1], o[2], 255] }).collect();
    out.sort();
    out.dedup();
    out
}

pub fn string_to_adr(s: &str, family: NetAdrType) -> (i32, NetAdr) {
    if s == "localhost" {
        return (1, NetAdr::loopback());
    }
    let (host, port) = if s.starts_with('[') || s.matches(':').count() > 1 {
        // an ipv6 address
        let b = s.trim_start_matches('[');
        match b.split_once(']') {
            Some((h, rest)) => (h.to_string(), rest.strip_prefix(':').map(|p| p.to_string())),
            None => (b.to_string(), None),
        }
    } else {
        match s.split_once(':') {
            Some((h, p)) => (h.to_string(), Some(p.to_string())),
            None => (s.to_string(), None),
        }
    };
    let port_num = port.as_deref().map(|p| super::spawn::c_atoi(p) as u16);
    // Sys_StringToAdr: getaddrinfo, then SearchAddrInfo in net_enabled's order
    let Ok(addrs) = (host.as_str(), port_num.unwrap_or(PORT_SERVER)).to_socket_addrs() else { return (0, NetAdr::default()) };
    let addrs: Vec<SocketAddr> = addrs.collect();
    let v4 = || addrs.iter().find(|a| a.is_ipv4());
    let v6 = || addrs.iter().find(|a| a.is_ipv6());
    let en = net_enabled();
    let pick = match family {
        NetAdrType::Ip => v4(),
        NetAdrType::Ip6 => v6(),
        _ if en & NET_PRIOV6 != 0 => {
            let mut search = if en & NET_ENABLEV6 != 0 { v6() } else { None };
            if search.is_none() && en & NET_ENABLEV4 != 0 {
                search = v4();
            }
            search
        }
        _ => {
            let mut search = if en & NET_ENABLEV4 != 0 { v4() } else { None };
            if search.is_none() && en & NET_ENABLEV6 != 0 {
                search = v6();
            }
            search
        }
    };
    match pick {
        Some(a) => (if port.is_some() { 1 } else { 2 }, NetAdr::from_socket(a)),
        None => (0, NetAdr::default()),
    }
}

/// The interface name of a scope id (`if_indextoname`), else the number.
fn scope_name(id: u32) -> String {
    #[cfg(unix)]
    {
        let mut buf = [0 as libc::c_char; libc::IF_NAMESIZE];
        // SAFETY: buf has IF_NAMESIZE bytes as if_indextoname requires
        let p = unsafe { libc::if_indextoname(id, buf.as_mut_ptr()) };
        if !p.is_null() {
            // SAFETY: if_indextoname wrote a nul-terminated name into buf
            return unsafe { std::ffi::CStr::from_ptr(buf.as_ptr()) }.to_string_lossy().into_owned();
        }
    }
    id.to_string()
}

/// The interface index of `net_mcast6iface` (`if_nametoindex`; a number on
/// Windows as in the C).
fn interface_index(name: &str) -> u32 {
    if name.is_empty() {
        return 0;
    }
    #[cfg(unix)]
    {
        if let Ok(c) = std::ffi::CString::new(name) {
            // SAFETY: c is a valid nul-terminated string
            let i = unsafe { libc::if_nametoindex(c.as_ptr()) };
            if i != 0 {
                return i;
            }
        }
    }
    name.parse().unwrap_or(0)
}

/// `NET_IP6Socket`'s socket: non-blocking, IPV6_V6ONLY so the ipv4 socket
/// can share the port, bound to `addr`.
fn ip6_socket(addr: std::net::SocketAddrV6) -> std::io::Result<UdpSocket> {
    #[cfg(unix)]
    {
        use std::os::fd::FromRawFd;
        // SAFETY: plain socket calls on a descriptor this function owns;
        // sockaddr_in6 is zero-initialised before its fields are set
        unsafe {
            let fd = libc::socket(libc::AF_INET6, libc::SOCK_DGRAM, libc::IPPROTO_UDP);
            if fd < 0 {
                return Err(std::io::Error::last_os_error());
            }
            let one: libc::c_int = 1;
            // ipv4 addresses should not be allowed to connect via this socket.
            libc::setsockopt(fd, libc::IPPROTO_IPV6, libc::IPV6_V6ONLY, &one as *const libc::c_int as *const libc::c_void, std::mem::size_of::<libc::c_int>() as libc::socklen_t);
            let mut sa: libc::sockaddr_in6 = std::mem::zeroed();
            sa.sin6_family = libc::AF_INET6 as libc::sa_family_t;
            sa.sin6_port = addr.port().to_be();
            sa.sin6_addr.s6_addr = addr.ip().octets();
            sa.sin6_scope_id = addr.scope_id();
            #[cfg(any(target_os = "macos", target_os = "ios", target_os = "freebsd", target_os = "openbsd", target_os = "netbsd"))]
            {
                sa.sin6_len = std::mem::size_of::<libc::sockaddr_in6>() as u8;
            }
            if libc::bind(fd, &sa as *const libc::sockaddr_in6 as *const libc::sockaddr, std::mem::size_of::<libc::sockaddr_in6>() as libc::socklen_t) != 0 {
                let e = std::io::Error::last_os_error();
                libc::close(fd);
                return Err(e);
            }
            let s = UdpSocket::from_raw_fd(fd);
            s.set_nonblocking(true)?;
            Ok(s)
        }
    }
    #[cfg(not(unix))]
    {
        // Windows sockets are ipv6-only by default
        let s = UdpSocket::bind(addr)?;
        s.set_nonblocking(true)?;
        Ok(s)
    }
}

/// Socket buffer size asked of the OS (port addition): a pk3 download
/// sends a whole window of blocks in one burst, which the platform defaults
/// (64 KB on Windows) would drop.
const SOCKET_BUFFER_BYTES: i32 = 4 << 20;

/// Raises SO_RCVBUF / SO_SNDBUF to `SOCKET_BUFFER_BYTES`; the OS may clamp
/// it and a failure keeps its default.
fn grow_socket_buffers(s: &UdpSocket) {
    let v = SOCKET_BUFFER_BYTES;
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        // SAFETY: setsockopt on a live descriptor with an int option value
        unsafe {
            for opt in [libc::SO_RCVBUF, libc::SO_SNDBUF] {
                libc::setsockopt(s.as_raw_fd(), libc::SOL_SOCKET, opt, &v as *const i32 as *const libc::c_void, std::mem::size_of::<i32>() as libc::socklen_t);
            }
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawSocket;
        #[link(name = "ws2_32")]
        extern "system" {
            fn setsockopt(s: usize, level: i32, optname: i32, optval: *const u8, optlen: i32) -> i32;
        }
        const SOL_SOCKET: i32 = 0xffff;
        const SO_SNDBUF: i32 = 0x1001;
        const SO_RCVBUF: i32 = 0x1002;
        // SAFETY: setsockopt on a live socket with an int option value
        unsafe {
            for opt in [SO_RCVBUF, SO_SNDBUF] {
                setsockopt(s.as_raw_socket() as usize, SOL_SOCKET, opt, &v as *const i32 as *const u8, 4);
            }
        }
    }
}

/// The host's sockets and loopback queues (net_ip.c `ip_socket` per
/// source, net_chan.c `loopbacks[2]`), plus counters for the tests.
pub struct Net {
    /// `ip_socket` per source
    sockets: [Option<UdpSocket>; 2],
    /// `ip6_socket` per source
    sockets6: [Option<UdpSocket>; 2],
    /// `multicast6_socket` per source: its own socket, or (true) the ip6 one
    mcast6: [Option<UdpSocket>; 2],
    mcast6_shared: [bool; 2],
    /// `net_port6` actually bound per source (0: none)
    pub ports6: [u16; 2],
    /// `net_enabled` (NET_ENABLEV4 | NET_ENABLEV6 | NET_PRIOV6 | NET_DISABLEMCAST)
    pub enabled: i32,
    /// `net_mcast6addr` / `net_mcast6iface`
    pub mcast6addr: String,
    pub mcast6iface: String,
    loopbacks: [VecDeque<Vec<u8>>; 2],
    /// `net_port` actually bound per source (0: none)
    pub ports: [u16; 2],
    /// `net_dropsim`: percentage of incoming packets dropped (tests)
    pub drop_sim: u32,
    drop_seed: u32,
    pub packets_sent: u64,
    pub bytes_sent: u64,
    /// `showpackets`
    pub show_packets: bool,
    /// tests: non-loopback packets are captured here instead of sent
    pub capture: Option<Vec<(NetAdr, Vec<u8>)>>,
    /// `cl_packetdelay` / `sv_packetdelay` (ms) per source
    pub packet_delay: [i32; 2],
    /// `packetQueue`: (release time, source, destination, data)
    queue: VecDeque<(i32, NetSrc, NetAdr, Vec<u8>)>,
    /// a simulated `Sys_Milliseconds` (tests); None: the real clock
    pub clock: Option<i32>,
    /// STUN transactions sent from a game socket (stun.rs): (server, id)
    stun_wait: Vec<(SocketAddr, [u8; 12])>,
    /// their answers, taken out before the Q3 packet path
    stun_replies: Vec<(SocketAddr, Vec<u8>)>,
}

/// `Sys_Milliseconds`: milliseconds since the first call.
pub fn sys_milliseconds() -> i32 {
    static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    START.get_or_init(std::time::Instant::now).elapsed().as_millis() as i32
}

impl Default for Net {
    fn default() -> Self {
        Net {
            sockets: [None, None],
            sockets6: [None, None],
            mcast6: [None, None],
            mcast6_shared: [false; 2],
            ports6: [0; 2],
            enabled: NET_ENABLEV4 | NET_ENABLEV6,
            mcast6addr: NET_MULTICAST_IP6.into(),
            mcast6iface: String::new(),
            loopbacks: Default::default(),
            ports: [0; 2],
            drop_sim: 0,
            drop_seed: 0,
            packets_sent: 0,
            bytes_sent: 0,
            show_packets: false,
            capture: None,
            packet_delay: [0; 2],
            queue: VecDeque::new(),
            clock: None,
            stun_wait: Vec::new(),
            stun_replies: Vec::new(),
        }
    }
}

impl Net {
    pub fn new() -> Self {
        Self::default()
    }

    /// `NET_OpenIP` for one source: bind `port` (0: any free port), scanning
    /// the next 9 like the C so several servers can share a machine.
    /// Returns the bound port.
    pub fn open(&mut self, src: NetSrc, ip: &str, port: u16) -> std::io::Result<u16> {
        let ip = if ip.is_empty() || ip == "0.0.0.0" { "0.0.0.0" } else { ip };
        let mut last = None;
        for i in 0..10u16 {
            if port == 0 && i > 0 {
                break;
            }
            match UdpSocket::bind((ip, port.wrapping_add(i))) {
                Ok(s) => {
                    s.set_nonblocking(true)?;
                    let _ = s.set_broadcast(true);
                    let p = s.local_addr()?.port();
                    grow_socket_buffers(&s);
                    self.sockets[src as usize] = Some(s);
                    self.ports[src as usize] = p;
                    return Ok(p);
                }
                Err(e) => last = Some(e),
            }
        }
        Err(last.unwrap_or_else(|| std::io::Error::other("bind failed")))
    }

    /// Close one source's socket (`NET_Config( qfalse )`).
    pub fn close(&mut self, src: NetSrc) {
        self.leave_multicast6(src);
        self.sockets[src as usize] = None;
        self.sockets6[src as usize] = None;
        self.ports[src as usize] = 0;
        self.ports6[src as usize] = 0;
        self.loopbacks[src as usize].clear();
    }

    /// The bound ipv4 port of a source, when its socket is open.
    pub fn port(&self, src: NetSrc) -> Option<u16> {
        self.sockets[src as usize].as_ref().map(|_| self.ports[src as usize])
    }

    pub fn is_open(&self, src: NetSrc) -> bool {
        self.sockets[src as usize].is_some() || self.sockets6[src as usize].is_some()
    }

    /// Is an ipv6 socket open for `src`.
    pub fn is_open6(&self, src: NetSrc) -> bool {
        self.sockets6[src as usize].is_some()
    }

    /// `NET_IP6Socket` with NET_OpenIP's scan of the next 9 ports: the
    /// bound port. EAFNOSUPPORT (no ipv6 on the host) stops the scan.
    pub fn open6(&mut self, src: NetSrc, ip6: &str, port: u16) -> std::io::Result<u16> {
        let ip: Ipv6Addr = if ip6.is_empty() || ip6 == "::" { Ipv6Addr::UNSPECIFIED } else {
            let h = ip6.trim_start_matches('[').trim_end_matches(']');
            match h.parse() {
                Ok(a) => a,
                Err(_) => match string_to_adr(ip6, NetAdrType::Ip6) {
                    (r, a) if r != 0 => Ipv6Addr::from(a.ip6),
                    _ => return Err(std::io::Error::other(format!("bad net_ip6 {ip6}"))),
                },
            }
        };
        let mut last = None;
        for i in 0..10u16 {
            if port == 0 && i > 0 {
                break;
            }
            match ip6_socket(std::net::SocketAddrV6::new(ip, port.wrapping_add(i), 0, 0)) {
                Ok(s) => {
                    let p = s.local_addr()?.port();
                    grow_socket_buffers(&s);
                    self.sockets6[src as usize] = Some(s);
                    self.ports6[src as usize] = p;
                    return Ok(p);
                }
                Err(e) => {
                    let afno = e.raw_os_error() == Some(97) || e.kind() == std::io::ErrorKind::Unsupported;
                    last = Some(e);
                    if afno {
                        break;
                    }
                }
            }
        }
        Err(last.unwrap_or_else(|| std::io::Error::other("bind failed")))
    }

    /// `NET_OpenIP` for one source per `net_enabled`: the ipv6 socket, then
    /// the ipv4 one; the console lines it printed.
    pub fn open_ip(&mut self, src: NetSrc, ip: &str, port: u16, ip6: &str, port6: u16) -> String {
        let mut out = String::new();
        if self.enabled & NET_ENABLEV6 != 0 && self.sockets6[src as usize].is_none() {
            let shown = if ip6.is_empty() { "::" } else { ip6 };
            match self.open6(src, ip6, port6) {
                Ok(p) => out.push_str(&format!("Opening IP6 socket: [{shown}]:{p}\n")),
                Err(e) => out.push_str(&format!("WARNING: NET_IP6Socket: bind: {e}\nWARNING: Couldn't bind to a v6 ip address.\n")),
            }
        }
        if self.enabled & NET_ENABLEV4 != 0 && self.sockets[src as usize].is_none() {
            match self.open(src, ip, port) {
                Ok(p) => out.push_str(&format!("Opening IP socket: {}:{p}\n", if ip.is_empty() { "0.0.0.0" } else { ip })),
                Err(e) => out.push_str(&format!("WARNING: NET_IPSocket: bind: {e}\nWARNING: Couldn't bind to a v4 ip address.\n")),
            }
        }
        out
    }

    /// The client's sockets on any port (`NET_Config` for NS_CLIENT): the
    /// families of `net_enabled`, ipv4 when nothing could be opened.
    pub fn open_client(&mut self) {
        if self.is_open(NetSrc::Client) {
            return;
        }
        let _ = self.open_ip(NetSrc::Client, "", 0, "", 0);
        if !self.is_open(NetSrc::Client) {
            let _ = self.open(NetSrc::Client, "", 0);
        }
    }

    /// `NET_SetMulticast6`: the group and interface, or None (and
    /// NET_DISABLEMCAST set) for a bad `net_mcast6addr`.
    fn multicast6_group(&mut self) -> Option<(Ipv6Addr, u32)> {
        let a = self.mcast6addr.trim_start_matches('[').trim_end_matches(']');
        match a.parse::<Ipv6Addr>() {
            Ok(g) => Some((g, interface_index(&self.mcast6iface))),
            Err(_) => {
                self.enabled |= NET_DISABLEMCAST;
                None
            }
        }
    }

    /// `NET_JoinMulticast6` on the `src` ipv6 socket (SV_SpawnServer: so
    /// clients can scan for us on the local network). Console output.
    pub fn join_multicast6(&mut self, src: NetSrc) -> String {
        let si = src as usize;
        if self.sockets6[si].is_none() || self.mcast6[si].is_some() || self.mcast6_shared[si] || self.enabled & NET_DISABLEMCAST != 0 {
            return String::new();
        }
        let Some((group, iface)) = self.multicast6_group() else {
            return "WARNING: NET_JoinMulticast6: Incorrect multicast address given, please set cvar net_mcast6addr to a sane value.\n".into();
        };
        let bound = self.sockets6[si].as_ref().and_then(|s| s.local_addr().ok());
        let port = bound.map(|b| b.port()).unwrap_or(0);
        let unspecified = matches!(bound, Some(SocketAddr::V6(b)) if b.ip().is_unspecified() || b.ip().is_multicast());
        let own = if unspecified {
            // The way the socket was bound does not prohibit receiving
            // multi-cast packets. So we don't need to open a new one.
            None
        } else {
            // If the OS does not support binding to multicast addresses,
            // at least try with the normal file descriptor.
            ip6_socket(std::net::SocketAddrV6::new(group, port, 0, 0)).ok()
        };
        let sock = own.as_ref().or(self.sockets6[si].as_ref()).unwrap();
        if let Err(e) = sock.join_multicast_v6(&group, iface) {
            if own.is_some() {
                return format!("NET_JoinMulticast6: Couldn't join multicast group: {e}\n");
            }
            // the ip6 socket stays the multicast one, as in the C
            self.mcast6_shared[si] = true;
            return format!("NET_JoinMulticast6: Couldn't join multicast group: {e}\n");
        }
        match own {
            Some(s) => self.mcast6[si] = Some(s),
            None => self.mcast6_shared[si] = true,
        }
        String::new()
    }

    /// `NET_LeaveMulticast6` (SV_Shutdown).
    pub fn leave_multicast6(&mut self, src: NetSrc) {
        let si = src as usize;
        if self.mcast6[si].take().is_some() {
            return;
        }
        if self.mcast6_shared[si] {
            self.mcast6_shared[si] = false;
            if let (Some(s), Some((g, iface))) = (self.sockets6[si].as_ref(), self.mcast6addr.trim_start_matches('[').trim_end_matches(']').parse::<Ipv6Addr>().ok().map(|g| (g, interface_index(&self.mcast6iface)))) {
                let _ = s.leave_multicast_v6(&g, iface);
            }
        }
    }

    /// Has `src` joined the multicast group.
    pub fn in_multicast6(&self, src: NetSrc) -> bool {
        self.mcast6[src as usize].is_some() || self.mcast6_shared[src as usize]
    }

    fn now(&self) -> i32 {
        self.clock.unwrap_or_else(sys_milliseconds)
    }

    /// `NET_QueuePacket`: hold a packet `offset` ms (at most 999).
    fn queue_packet(&mut self, src: NetSrc, data: &[u8], to: &NetAdr, offset: i32) {
        let release = self.now() + offset.min(999);
        self.queue.push_back((release, src, *to, data.to_vec()));
    }

    /// `NET_FlushPacketQueue` (once per Com_Frame): send what is due.
    pub fn flush_packet_queue(&mut self) {
        while let Some((release, ..)) = self.queue.front() {
            if *release >= self.now() {
                break;
            }
            let (_, src, to, data) = self.queue.pop_front().unwrap();
            self.sys_send_packet(&data, &to, src);
        }
    }

    /// `Sys_SendPacket` (either socket will do; `src` is preferred).
    fn sys_send_packet(&mut self, data: &[u8], to: &NetAdr, src: NetSrc) {
        if let Some(c) = self.capture.as_mut() {
            c.push((*to, data.to_vec()));
            return;
        }
        let (sa, v6) = match to.kind {
            NetAdrType::Multicast6 => {
                if self.enabled & NET_DISABLEMCAST != 0 {
                    return;
                }
                let Some((g, iface)) = self.multicast6_group() else { return };
                (SocketAddr::V6(std::net::SocketAddrV6::new(g, to.port, 0, iface)), true)
            }
            _ => match to.to_socket() {
                Some(sa) => (sa, to.kind == NetAdrType::Ip6),
                None => return,
            },
        };
        let socks = if v6 { &self.sockets6 } else { &self.sockets };
        let sock = socks[src as usize].as_ref().or(socks[src as usize ^ 1].as_ref());
        if let Some(s) = sock {
            if s.send_to(data, sa).is_ok() {
                self.packets_sent += 1;
                self.bytes_sent += data.len() as u64;
            }
        }
    }

    /// `NET_SendPacket` (loopback goes to the other source's queue).
    pub fn send_packet(&mut self, src: NetSrc, data: &[u8], to: &NetAdr) {
        if self.show_packets && data.len() >= 4 && data[..4] == [0xff; 4] {
            eprintln!("send packet {:4}", data.len());
        }
        match to.kind {
            NetAdrType::Loopback => {
                // NET_SendLoopPacket
                let q = &mut self.loopbacks[src as usize ^ 1];
                q.push_back(data.to_vec());
                while q.len() > MAX_LOOPBACK {
                    q.pop_front();
                }
            }
            NetAdrType::Bad => {}
            _ => {
                let delay = self.packet_delay[src as usize];
                if delay > 0 {
                    self.queue_packet(src, data, to, delay);
                } else {
                    self.sys_send_packet(data, to, src);
                }
            }
        }
    }

    /// `NET_GetLoopPacket` then `NET_GetPacket` for one source.
    pub fn get_packet(&mut self, src: NetSrc) -> Option<(NetAdr, Vec<u8>)> {
        if let Some(p) = self.loopbacks[src as usize].pop_front() {
            return Some((NetAdr::loopback(), p));
        }
        let si = src as usize;
        let mut buf = vec![0u8; MAX_MSGLEN];
        // NET_Sleep's select order: ip_socket, ip6_socket, multicast6_socket
        for k in 0..3 {
            let s = match k {
                0 => self.sockets[si].as_ref(),
                1 => self.sockets6[si].as_ref(),
                _ => self.mcast6[si].as_ref(),
            };
            let Some(s) = s else { continue };
            loop {
                match s.recv_from(&mut buf) {
                    Ok((n, from)) => {
                        // the answer to our STUN request (dec076), not a Q3 packet
                        if !self.stun_wait.is_empty() && super::stun::answers(&self.stun_wait, &from, &buf[..n]) {
                            self.stun_replies.push((from, buf[..n].to_vec()));
                            continue;
                        }
                        if self.drop_sim > 0 {
                            self.drop_seed = self.drop_seed.wrapping_mul(1103515245).wrapping_add(12345);
                            if (self.drop_seed >> 16) % 100 < self.drop_sim {
                                continue;
                            }
                        }
                        return Some((NetAdr::from_socket(&from), buf[..n].to_vec()));
                    }
                    // Windows reports an ICMP port-unreachable for an earlier
                    // send (a LAN scan port with nobody on it, a client that
                    // went away) as WSAECONNRESET on the next recvfrom;
                    // NET_GetPacket ignores it and keeps reading
                    Err(e) if e.kind() == std::io::ErrorKind::ConnectionReset => continue,
                    Err(_) => break,
                }
            }
        }
        None
    }

    /// A STUN Binding request to `to` from `src`'s game socket (the host's
    /// NAT check, dec076); its answer goes to `take_stun_replies`.
    pub fn stun_send(&mut self, src: NetSrc, to: SocketAddr, txid: [u8; 12]) {
        if !self.stun_wait.contains(&(to, txid)) {
            self.stun_wait.push((to, txid));
        }
        let d = super::stun::binding_request(&txid);
        self.sys_send_packet(&d, &NetAdr::from_socket(&to), src);
    }

    /// The STUN answers received since the last call.
    pub fn take_stun_replies(&mut self) -> Vec<(SocketAddr, Vec<u8>)> {
        std::mem::take(&mut self.stun_replies)
    }

    /// The check is over: later STUN-shaped packets take the Q3 path again.
    pub fn stun_done(&mut self) {
        self.stun_wait.clear();
        self.stun_replies.clear();
    }

    /// `NET_OutOfBandPrint`: 0xffffffff then the text.
    pub fn out_of_band_print(&mut self, src: NetSrc, to: &NetAdr, text: &str) {
        let mut d = vec![0xffu8; 4];
        d.extend_from_slice(text.as_bytes());
        self.send_packet(src, &d, to);
    }

    /// `NET_OutOfBandData` (only for "connect"): Huffman-compressed from byte 12.
    pub fn out_of_band_data(&mut self, src: NetSrc, to: &NetAdr, data: &[u8]) {
        let mut d = vec![0xffu8; 4];
        d.extend_from_slice(data);
        huffman::compress(&mut d, 12);
        self.send_packet(src, &d, to);
    }
}

/// `NETCHAN_GENCHECKSUM`.
pub fn gen_checksum(challenge: i32, sequence: i32) -> i32 {
    challenge ^ sequence.wrapping_mul(challenge)
}

/// `netchan_t`.
#[derive(Clone, Debug)]
pub struct Netchan {
    pub sock: NetSrc,
    /// between last packet and previous
    pub dropped: i32,
    pub remote_address: NetAdr,
    /// qport value to write when transmitting
    pub qport: i32,
    pub incoming_sequence: i32,
    pub outgoing_sequence: i32,
    // incoming fragment assembly buffer
    pub fragment_sequence: i32,
    pub fragment_buffer: Vec<u8>,
    // outgoing fragment buffer
    pub unsent_fragments: bool,
    pub unsent_fragment_start: usize,
    pub unsent_buffer: Vec<u8>,
    pub challenge: i32,
    pub last_sent_time: i32,
    pub last_sent_size: usize,
    /// the connection's protocol: 71 (legacy, the challenge checksum in
    /// every header) or 69 (Tremulous 1.1.0's net_chan.c: no checksum; dec040)
    pub protocol: i32,
}

impl Netchan {
    /// `Netchan_Setup`.
    pub fn setup(sock: NetSrc, adr: NetAdr, qport: i32, challenge: i32) -> Self {
        Netchan {
            sock,
            dropped: 0,
            remote_address: adr,
            qport,
            incoming_sequence: 0,
            outgoing_sequence: 1,
            fragment_sequence: 0,
            fragment_buffer: Vec::new(),
            unsent_fragments: false,
            unsent_fragment_start: 0,
            unsent_buffer: Vec::new(),
            challenge,
            last_sent_time: 0,
            last_sent_size: 0,
            protocol: super::sv::PROTOCOL_VERSION,
        }
    }

    /// `Netchan_Setup` for a protocol (dec040): 69 is Tremulous 1.1.0's
    /// netchan, whose headers carry no challenge checksum.
    pub fn setup_protocol(sock: NetSrc, adr: NetAdr, qport: i32, challenge: i32, protocol: i32) -> Self {
        Netchan { protocol, ..Self::setup(sock, adr, qport, challenge) }
    }

    fn vanilla(&self) -> bool {
        self.protocol == super::v11::PROTOCOL_VANILLA
    }

    fn header(&self, seq: u32) -> Msg {
        let mut send = Msg::new_oob(MAX_PACKETLEN);
        send.write_long(seq as i32);
        // send the qport if we are a client
        if self.sock == NetSrc::Client {
            send.write_short(self.qport);
        }
        // 1.1.0's Netchan_Transmit / Netchan_TransmitNextFragment write no
        // checksum (dec040)
        if !self.vanilla() {
            send.write_long(gen_checksum(self.challenge, self.outgoing_sequence));
        }
        send
    }

    /// `Netchan_TransmitNextFragment`: send one fragment of the current message.
    pub fn transmit_next_fragment(&mut self, net: &mut Net, now: i32) {
        let mut send = self.header(self.outgoing_sequence as u32 | FRAGMENT_BIT);
        let unsent_length = self.unsent_buffer.len();
        let mut fragment_length = FRAGMENT_SIZE;
        if self.unsent_fragment_start + fragment_length > unsent_length {
            fragment_length = unsent_length - self.unsent_fragment_start;
        }
        send.write_short(self.unsent_fragment_start as i32);
        send.write_short(fragment_length as i32);
        send.write_data(&self.unsent_buffer[self.unsent_fragment_start..self.unsent_fragment_start + fragment_length]);
        net.send_packet(self.sock, send.bytes(), &self.remote_address);
        self.last_sent_time = now;
        self.last_sent_size = send.cursize;
        self.unsent_fragment_start += fragment_length;
        // a packet that is exactly the fragment length still needs a
        // second packet of zero length so the other side can tell there
        // aren't more to follow
        if self.unsent_fragment_start == unsent_length && fragment_length != FRAGMENT_SIZE {
            self.outgoing_sequence += 1;
            self.unsent_fragments = false;
        }
    }

    /// `Netchan_Transmit`: fragments messages of FRAGMENT_SIZE or more; a 0
    /// length still generates a packet.
    pub fn transmit(&mut self, net: &mut Net, data: &[u8], now: i32) {
        assert!(data.len() <= MAX_MSGLEN, "Netchan_Transmit: length = {}", data.len());
        self.unsent_fragment_start = 0;
        // fragment large reliable messages
        if data.len() >= FRAGMENT_SIZE {
            self.unsent_fragments = true;
            self.unsent_buffer = data.to_vec();
            // only send the first fragment now
            self.transmit_next_fragment(net, now);
            return;
        }
        let mut send = self.header(self.outgoing_sequence as u32);
        self.outgoing_sequence += 1;
        send.write_data(data);
        net.send_packet(self.sock, send.bytes(), &self.remote_address);
        self.last_sent_time = now;
        self.last_sent_size = send.cursize;
    }

    /// `Netchan_Process`: false if the message should not be processed
    /// (out of order, a fragment, a bad checksum). On true, `msg` holds the
    /// whole message with `readcount`/`bit` past the header.
    pub fn process(&mut self, msg: &mut Msg) -> bool {
        msg.begin_reading_oob();
        let mut sequence = msg.read_long() as u32;
        // check for fragment information
        let fragmented = sequence & FRAGMENT_BIT != 0;
        sequence &= !FRAGMENT_BIT;
        let sequence = sequence as i32;
        // read the qport if we are a server
        if self.sock == NetSrc::Server {
            msg.read_short();
        }
        // 1.1.0's Netchan_Process reads no checksum (dec040)
        if !self.vanilla() {
            let checksum = msg.read_long();
            // UDP spoofing protection
            if gen_checksum(self.challenge, sequence) != checksum {
                return false;
            }
        }
        // read the fragment information
        let (fragment_start, fragment_length) = if fragmented { (msg.read_short(), msg.read_short()) } else { (0, 0) };
        // discard out of order or duplicated packets
        if sequence <= self.incoming_sequence {
            return false;
        }
        // dropped packets don't keep the message from being used
        self.dropped = sequence - (self.incoming_sequence + 1);
        if fragmented {
            // make sure we add the fragments in correct order
            if sequence != self.fragment_sequence {
                self.fragment_sequence = sequence;
                self.fragment_buffer.clear();
            }
            // if we missed a fragment, dump the message
            if fragment_start as usize != self.fragment_buffer.len() {
                return false;
            }
            // copy the fragment to the fragment buffer
            if fragment_length < 0 || msg.readcount + fragment_length as usize > msg.cursize || self.fragment_buffer.len() + fragment_length as usize > MAX_MSGLEN {
                return false;
            }
            self.fragment_buffer.extend_from_slice(&msg.data[msg.readcount..msg.readcount + fragment_length as usize]);
            // if this wasn't the last fragment, don't process anything
            if fragment_length as usize == FRAGMENT_SIZE {
                return false;
            }
            if self.fragment_buffer.len() > msg.maxsize() {
                return false;
            }
            // copy the full message over the partial fragment
            // make sure the sequence number is still there
            let fb = std::mem::take(&mut self.fragment_buffer);
            msg.data[..4].copy_from_slice(&sequence.to_le_bytes());
            msg.data[4..4 + fb.len()].copy_from_slice(&fb);
            msg.cursize = fb.len() + 4;
            msg.readcount = 4; // past the sequence number
            msg.bit = 32; // past the sequence number
            // clients were not acking fragmented messages
            self.incoming_sequence = sequence;
            return true;
        }
        // the message can now be read from the current message pointer
        self.incoming_sequence = sequence;
        true
    }
}

#[cfg(test)]
mod lan_tests {
    #[test]
    fn each_network_gets_its_own_broadcast() {
        use std::net::Ipv4Addr;
        let b = super::lan_broadcasts(&[Ipv4Addr::new(192, 168, 1, 23), Ipv4Addr::new(10, 0, 5, 7), Ipv4Addr::new(192, 168, 1, 40)]);
        assert_eq!(b, vec![[10, 0, 5, 255], [192, 168, 1, 255]]);
        // this machine's addresses: never loopback
        assert!(super::local_ipv4().iter().all(|a| !a.is_loopback()));
    }
}
