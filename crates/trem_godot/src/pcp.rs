//! PCP (RFC 6887) and NAT-PMP (RFC 6886) port mapping for a hosted game,
//! next to UPnP (upnp.rs; dec076).
//!
//! Both are one small UDP request to the default gateway's port 5351: "please
//! forward this UDP port to me". PCP goes first; a NAT-PMP-only router answers
//! it with "unsupported version" (or not at all), and then the NAT-PMP form is
//! sent. The mapping asks for the RFC's recommended two hours, is renewed
//! before it runs out, and deleted (lifetime 0) when the server stops. Every
//! answer carries the router's epoch (seconds since it started its mapping
//! table): an epoch that went back means the router restarted and forgot the
//! mapping, so it is made again. PCP's ADDRESS_MISMATCH, or an external
//! address that is private or in 100.64.0.0/10, means another router or the
//! provider's CGNAT sits in front. For IPv6 the same PCP MAP opens the
//! router's IPv6 firewall for the server's IPv6 socket.
//!
//! Design after Tailscale's net/portmapper (BSD-3-Clause; no code copied):
//! UPnP, PCP and NAT-PMP asked at the same time, 250 ms per gateway try,
//! 2 h lifetime, the epoch watch. std only: threads and blocking sockets,
//! like upnp.rs.

use crate::upnp;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV6, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// The gateway's PCP / NAT-PMP port.
pub const PORT: u16 = 5351;
/// "RFC recommended 2 hour map duration" (Tailscale portmapper; RFC 6886 3.3).
pub const LIFETIME: u32 = 7200;
/// The gateway is one hop away: 250 ms per try (Tailscale's portMapServiceTimeout).
pub const TRY_TIMEOUT: Duration = Duration::from_millis(250);
/// How often a mapping is renewed and the epoch checked (at most).
pub const RECHECK: Duration = Duration::from_secs(600);

const PCP_VERSION: u8 = 2;
const OP_MAP: u8 = 1;
const UDP: u8 = 17;
/// PCP result codes (RFC 6887 7.4)
pub const UNSUPP_VERSION: u16 = 1;
pub const ADDRESS_MISMATCH: u16 = 12;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Proto {
    Pcp,
    NatPmp,
}

impl Proto {
    pub fn name(self) -> &'static str {
        match self {
            Proto::Pcp => "PCP",
            Proto::NatPmp => "NAT-PMP",
        }
    }

    /// A refusal's result code in plain words (PCP: RFC 6887 7.4, NAT-PMP:
    /// RFC 6886 3.5); None for a code without one.
    pub fn reason(self, code: u16) -> Option<&'static str> {
        Some(match (self, code) {
            (Proto::Pcp, 2) | (Proto::NatPmp, 2) => "not allowed",
            (Proto::Pcp, 1 | 3 | 4 | 5 | 6) | (Proto::NatPmp, 1 | 5) => "the router did not understand the request",
            (Proto::Pcp, 7) | (Proto::NatPmp, 3) => "the router has no internet connection",
            (Proto::Pcp, 8) | (Proto::NatPmp, 4) => "the router has no mappings left",
            (Proto::Pcp, 9) => "not for UDP",
            (Proto::Pcp, 10) => "this PC has too many mappings",
            (Proto::Pcp, 11) => "that outside port is not available",
            (Proto::Pcp, 12) => "another router or the provider's CGNAT is in front",
            _ => return None,
        })
    }

    /// "PCP: not allowed", or "PCP error 99" for a code without words.
    pub fn refusal(self, code: u16) -> String {
        match self.reason(code) {
            Some(r) => format!("{}: {r}", self.name()),
            None => format!("{} error {code}", self.name()),
        }
    }
}

/// A port the gateway forwards to this PC.
#[derive(Clone, Debug, PartialEq)]
pub struct Lease {
    pub proto: Proto,
    pub gateway: SocketAddr,
    /// this PC's address towards the gateway (the PCP client address)
    pub local: IpAddr,
    pub internal_port: u16,
    /// the gateway's outside address and port (0.0.0.0 / :: when it did not say)
    pub external: SocketAddr,
    pub lifetime: u32,
    pub epoch: u32,
    pub at: Instant,
    nonce: [u8; 12],
}

/// What one PCP / NAT-PMP attempt gave.
#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    Mapped(Lease),
    /// PCP ADDRESS_MISMATCH: the gateway saw another address than ours, so
    /// another NAT sits between (CGNAT signal)
    Mismatch,
    /// the gateway said no: the protocol and its result code
    Refused(Proto, u16),
    NoAnswer,
}

impl Outcome {
    pub fn lease(&self) -> Option<&Lease> {
        match self {
            Outcome::Mapped(l) => Some(l),
            _ => None,
        }
    }
}

/// The PCP form of an address (IPv4 as ::ffff:a.b.c.d).
fn v6_form(ip: IpAddr) -> [u8; 16] {
    match ip {
        IpAddr::V4(a) => a.to_ipv6_mapped().octets(),
        IpAddr::V6(a) => a.octets(),
    }
}

fn from_v6_form(o: [u8; 16]) -> IpAddr {
    let a = Ipv6Addr::from(o);
    match a.to_ipv4_mapped() {
        Some(v4) => IpAddr::V4(v4),
        None => IpAddr::V6(a),
    }
}

/// A PCP MAP request (RFC 6887 7.1 + 11.1): UDP `port` to `port`.
pub fn pcp_map_request(local: IpAddr, port: u16, lifetime: u32, nonce: &[u8; 12], suggest: Option<SocketAddr>) -> Vec<u8> {
    let mut b = vec![0u8; 60];
    b[0] = PCP_VERSION;
    b[1] = OP_MAP;
    b[4..8].copy_from_slice(&lifetime.to_be_bytes());
    b[8..24].copy_from_slice(&v6_form(local));
    b[24..36].copy_from_slice(nonce);
    b[36] = UDP;
    b[40..42].copy_from_slice(&port.to_be_bytes());
    let (sp, sip) = match suggest {
        Some(s) => (s.port(), s.ip()),
        None => (port, if local.is_ipv4() { IpAddr::V4(Ipv4Addr::UNSPECIFIED) } else { IpAddr::V6(Ipv6Addr::UNSPECIFIED) }),
    };
    b[42..44].copy_from_slice(&sp.to_be_bytes());
    b[44..60].copy_from_slice(&v6_form(sip));
    b
}

/// A PCP answer: (result code, lifetime, epoch, external address). `None`
/// when it is not a PCP MAP answer to this request (nonce, protocol, port).
pub fn parse_pcp(d: &[u8], nonce: &[u8; 12], port: u16) -> Option<(u16, u32, u32, Option<SocketAddr>)> {
    if d.len() < 24 || d[0] != PCP_VERSION || d[1] != 0x80 | OP_MAP {
        return None;
    }
    let code = d[3] as u16;
    let lifetime = u32::from_be_bytes([d[4], d[5], d[6], d[7]]);
    let epoch = u32::from_be_bytes([d[8], d[9], d[10], d[11]]);
    if d.len() < 60 {
        // an error answer may come without the MAP payload
        return (code != 0).then_some((code, lifetime, epoch, None));
    }
    if d[24..36] != nonce[..] || d[36] != UDP || u16::from_be_bytes([d[40], d[41]]) != port {
        return None;
    }
    let ext_port = u16::from_be_bytes([d[42], d[43]]);
    let mut o = [0u8; 16];
    o.copy_from_slice(&d[44..60]);
    Some((code, lifetime, epoch, Some(SocketAddr::new(from_v6_form(o), ext_port))))
}

/// A NAT-PMP answer to a PCP request: version 0 and "unsupported version".
fn natpmp_unsupp_version(d: &[u8]) -> bool {
    d.len() >= 4 && d[0] == 0 && d[1] >= 128 && u16::from_be_bytes([d[2], d[3]]) == UNSUPP_VERSION
}

/// NAT-PMP's UDP mapping request (RFC 6886 3.3).
pub fn natpmp_map_request(port: u16, ext_port: u16, lifetime: u32) -> Vec<u8> {
    let mut b = vec![0u8, 1, 0, 0];
    b.extend_from_slice(&port.to_be_bytes());
    b.extend_from_slice(&ext_port.to_be_bytes());
    b.extend_from_slice(&lifetime.to_be_bytes());
    b
}

/// NAT-PMP mapping answer: (result, epoch, external port, lifetime).
pub fn parse_natpmp_map(d: &[u8], port: u16) -> Option<(u16, u32, u16, u32)> {
    if d.len() < 16 || d[0] != 0 || d[1] != 129 || u16::from_be_bytes([d[8], d[9]]) != port {
        return (d.len() >= 4 && d[0] == 0 && d[1] == 129).then(|| (u16::from_be_bytes([d[2], d[3]]), 0, 0, 0)).filter(|r| r.0 != 0);
    }
    Some((u16::from_be_bytes([d[2], d[3]]), u32::from_be_bytes([d[4], d[5], d[6], d[7]]), u16::from_be_bytes([d[10], d[11]]), u32::from_be_bytes([d[12], d[13], d[14], d[15]])))
}

/// NAT-PMP external address answer: (result, epoch, address).
pub fn parse_natpmp_addr(d: &[u8]) -> Option<(u16, u32, Ipv4Addr)> {
    if d.len() < 12 || d[0] != 0 || d[1] != 128 {
        return None;
    }
    Some((u16::from_be_bytes([d[2], d[3]]), u32::from_be_bytes([d[4], d[5], d[6], d[7]]), Ipv4Addr::new(d[8], d[9], d[10], d[11])))
}

/// RFC 6887 8.5 (and RFC 6886 3.6): did the gateway lose its mappings
/// (restart) between two answers? The epoch went back, or the two clocks
/// moved by too different amounts.
pub fn epoch_lost(prev_epoch: u32, prev_at: Instant, epoch: u32, now: Instant) -> bool {
    if epoch < prev_epoch {
        return true;
    }
    let client = now.saturating_duration_since(prev_at).as_secs() as i64;
    let server = (epoch - prev_epoch) as i64;
    client + 2 < server - server / 16 || server + 2 < client - client / 16
}

/// A random-enough nonce (std only: the hasher's random keys and the clock).
fn nonce() -> [u8; 12] {
    use std::hash::{BuildHasher, Hasher};
    let mut out = [0u8; 12];
    for (k, c) in out.chunks_mut(8).enumerate() {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0));
        h.write_usize(k);
        let v = h.finish().to_be_bytes();
        c.copy_from_slice(&v[..c.len()]);
    }
    out
}

/// Send `req` to `gw` and wait for an answer from it that `accept` takes:
/// `tries` sends, `wait` each.
fn exchange(s: &UdpSocket, gw: SocketAddr, req: &[u8], tries: u32, wait: Duration, accept: &mut dyn FnMut(&[u8]) -> bool) -> Option<Vec<u8>> {
    let mut buf = [0u8; 1100];
    for _ in 0..tries {
        s.send_to(req, gw).ok()?;
        let end = Instant::now() + wait;
        loop {
            let left = end.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            let _ = s.set_read_timeout(Some(left));
            match s.recv_from(&mut buf) {
                Ok((n, from)) if from.ip() == gw.ip() => {
                    if accept(&buf[..n]) {
                        return Some(buf[..n].to_vec());
                    }
                }
                Ok(_) => {}
                Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => break,
                Err(e) if e.kind() == std::io::ErrorKind::ConnectionReset => {}
                Err(_) => return None,
            }
        }
    }
    None
}

/// This PC's address towards `gw` (nothing is sent).
fn local_ip_to(gw: SocketAddr) -> Option<IpAddr> {
    let any: SocketAddr = if gw.is_ipv4() { "0.0.0.0:0".parse().unwrap() } else { "[::]:0".parse().unwrap() };
    let s = UdpSocket::bind(any).ok()?;
    s.connect(gw).ok()?;
    Some(s.local_addr().ok()?.ip())
}

/// A socket towards `gw` from `from` (default: the address the system
/// would use). The PCP client address must be the address the mapping is
/// for: for IPv6 the server's global address, not the link-local one a
/// link-local router would otherwise be reached from.
fn socket_to(gw: SocketAddr, from: Option<IpAddr>) -> Option<(UdpSocket, IpAddr)> {
    let local = match from {
        Some(a) => a,
        None => local_ip_to(gw)?,
    };
    let bind = match (local, gw) {
        (IpAddr::V6(a), SocketAddr::V6(g)) => SocketAddr::V6(SocketAddrV6::new(a, 0, 0, g.scope_id())),
        (a, _) => SocketAddr::new(a, 0),
    };
    Some((UdpSocket::bind(bind).ok()?, local))
}

/// Map UDP `port` at the gateway `gw`: PCP, then NAT-PMP (IPv4 only) when
/// the gateway does not speak PCP. `suggest`/`old_nonce`: renew a lease.
pub fn map(gw: SocketAddr, port: u16, lifetime: u32, renew: Option<&Lease>) -> Outcome {
    map_from(gw, renew.map(|l| l.local), port, lifetime, renew)
}

/// `map` with the client address given (`from`; None: the system's choice).
pub fn map_from(gw: SocketAddr, from: Option<IpAddr>, port: u16, lifetime: u32, renew: Option<&Lease>) -> Outcome {
    let Some((s, local)) = socket_to(gw, from) else { return Outcome::NoAnswer };
    let tries = 2;
    let natpmp_first = renew.is_some_and(|l| l.proto == Proto::NatPmp);
    let mut fall_back = gw.is_ipv4();
    if !natpmp_first {
        let n = renew.map(|l| l.nonce).unwrap_or_else(nonce);
        let suggest = renew.map(|l| l.external).filter(|e| !e.ip().is_unspecified());
        let req = pcp_map_request(local, port, lifetime, &n, suggest);
        let mut unsupp = false;
        let got = exchange(&s, gw, &req, tries, TRY_TIMEOUT, &mut |d| {
            if natpmp_unsupp_version(d) || (d.len() >= 4 && d[0] != PCP_VERSION && d[1] & 0x80 != 0) {
                unsupp = true;
                return true;
            }
            parse_pcp(d, &n, port).is_some()
        });
        match got.as_deref().and_then(|d| parse_pcp(d, &n, port)) {
            Some((0, life, epoch, ext)) => {
                return Outcome::Mapped(Lease {
                    proto: Proto::Pcp,
                    gateway: gw,
                    local,
                    internal_port: port,
                    external: ext.unwrap_or(SocketAddr::new(Ipv4Addr::UNSPECIFIED.into(), port)),
                    lifetime: life,
                    epoch,
                    at: Instant::now(),
                    nonce: n,
                })
            }
            Some((ADDRESS_MISMATCH, ..)) => return Outcome::Mismatch,
            Some((UNSUPP_VERSION, ..)) => {}
            Some((code, ..)) => return Outcome::Refused(Proto::Pcp, code),
            None if unsupp => {}
            // a gateway that ignores PCP may still speak NAT-PMP
            None => fall_back &= renew.is_none(),
        }
    }
    if !fall_back && !natpmp_first {
        return Outcome::NoAnswer;
    }
    if !gw.is_ipv4() {
        return Outcome::NoAnswer;
    }
    natpmp(&s, gw, local, port, lifetime, renew.map(|l| l.external.port()).unwrap_or(port))
}

fn natpmp(s: &UdpSocket, gw: SocketAddr, local: IpAddr, port: u16, lifetime: u32, ext_port: u16) -> Outcome {
    let req = natpmp_map_request(port, ext_port, lifetime);
    let Some(d) = exchange(s, gw, &req, 2, TRY_TIMEOUT, &mut |d| parse_natpmp_map(d, port).is_some()) else { return Outcome::NoAnswer };
    let Some((code, epoch, ext_port, life)) = parse_natpmp_map(&d, port) else { return Outcome::NoAnswer };
    if code != 0 {
        return Outcome::Refused(Proto::NatPmp, code);
    }
    let ext_ip = exchange(s, gw, &[0, 0], 2, TRY_TIMEOUT, &mut |d| parse_natpmp_addr(d).is_some()).and_then(|d| parse_natpmp_addr(&d)).filter(|a| a.0 == 0).map(|a| a.2).unwrap_or(Ipv4Addr::UNSPECIFIED);
    Outcome::Mapped(Lease { proto: Proto::NatPmp, gateway: gw, local, internal_port: port, external: SocketAddr::new(ext_ip.into(), ext_port), lifetime: life, epoch, at: Instant::now(), nonce: [0; 12] })
}

/// Take a mapping back (lifetime 0).
pub fn delete(l: &Lease) {
    let Some((s, _)) = socket_to(l.gateway, Some(l.local)) else { return };
    let req = match l.proto {
        Proto::Pcp => pcp_map_request(l.local, l.internal_port, 0, &l.nonce, None),
        // RFC 6886 3.4: lifetime 0 and suggested external port 0
        Proto::NatPmp => natpmp_map_request(l.internal_port, 0, 0),
    };
    let (n, port) = (l.nonce, l.internal_port);
    let _ = exchange(&s, l.gateway, &req, 1, TRY_TIMEOUT, &mut |d| parse_pcp(d, &n, port).is_some() || parse_natpmp_map(d, port).is_some());
}

/// What the renewal thread tells the game.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// the gateway had restarted (epoch) or moved the port: mapped again
    Remapped(Lease),
    /// the mapping could not be renewed
    Lost(Proto),
    /// the UPnP mapping could not be renewed
    UpnpLost,
}

/// The thread that renews a lease and deletes it when the server stops.
pub struct Keeper {
    stop: Sender<()>,
    handle: std::thread::JoinHandle<()>,
}

impl Keeper {
    /// Delete the mapping; `wait`: until it is sent (process exit).
    pub fn stop(self, wait: bool) {
        let _ = self.stop.send(());
        if wait {
            let _ = self.handle.join();
        }
    }
}

/// Renew `lease` every `every` (at most half its lifetime), remap after a
/// gateway restart, delete it when stopped.
pub fn keep(lease: Lease, every: Duration, events: Sender<Event>) -> Keeper {
    let (stop, rx): (Sender<()>, Receiver<()>) = std::sync::mpsc::channel();
    let handle = std::thread::spawn(move || {
        let mut l = lease;
        loop {
            let wait = every.min(Duration::from_secs((l.lifetime / 2).max(1) as u64));
            match rx.recv_timeout(wait) {
                Err(RecvTimeoutError::Timeout) => {}
                _ => {
                    delete(&l);
                    return;
                }
            }
            match map(l.gateway, l.internal_port, LIFETIME, Some(&l)) {
                Outcome::Mapped(n) => {
                    let restarted = epoch_lost(l.epoch, l.at, n.epoch, n.at);
                    let moved = n.external != l.external;
                    l = n;
                    if restarted || moved {
                        let _ = events.send(Event::Remapped(l.clone()));
                    }
                }
                _ => {
                    // the renewal failed: one fresh request, else report it
                    match map(l.gateway, l.internal_port, LIFETIME, None) {
                        Outcome::Mapped(n) => {
                            l = n;
                            let _ = events.send(Event::Remapped(l.clone()));
                        }
                        _ => {
                            let _ = events.send(Event::Lost(l.proto));
                            return;
                        }
                    }
                }
            }
        }
    });
    Keeper { stop, handle }
}

/// Renew a timed UPnP mapping (upnp.rs) the same way as a PCP lease, and
/// take it back when stopped. A permanent one (error 725 routers) is only
/// taken back.
pub fn keep_upnp(m: upnp::Mapping, every: Duration, events: Sender<Event>) -> Keeper {
    let (stop, rx): (Sender<()>, Receiver<()>) = std::sync::mpsc::channel();
    let handle = std::thread::spawn(move || {
        loop {
            let r = if m.lease == 0 {
                rx.recv().map_err(|_| RecvTimeoutError::Disconnected)
            } else {
                rx.recv_timeout(every.min(Duration::from_secs((m.lease / 2).max(1) as u64)))
            };
            if !matches!(r, Err(RecvTimeoutError::Timeout)) {
                upnp::unmap(&m);
                return;
            }
            // one retry, else report it; still taken back on stop
            if upnp::renew(&m).is_err() && upnp::renew(&m).is_err() {
                let _ = events.send(Event::UpnpLost);
                let _ = rx.recv();
                upnp::unmap(&m);
                return;
            }
        }
    });
    Keeper { stop, handle }
}

// ------------------------------------------------------------ gateways

/// `/proc/net/route`: the gateway of the default route.
pub fn parse_proc_route(text: &str) -> Option<Ipv4Addr> {
    text.lines().skip(1).find_map(|l| {
        let f: Vec<&str> = l.split_whitespace().collect();
        if f.len() < 8 || f[1] != "00000000" || f[7] != "00000000" {
            return None;
        }
        let g = u32::from_str_radix(f[2], 16).ok()?;
        (g != 0).then(|| Ipv4Addr::from(g.to_le_bytes()))
    })
}

/// `/proc/net/ipv6_route`: the next hop of the default route and its interface.
pub fn parse_proc_ipv6_route(text: &str) -> Option<(Ipv6Addr, String)> {
    text.lines().find_map(|l| {
        let f: Vec<&str> = l.split_whitespace().collect();
        if f.len() < 10 || f[0] != "00000000000000000000000000000000" || f[1] != "00" {
            return None;
        }
        let hop = u128::from_str_radix(f[4], 16).ok()?;
        (hop != 0).then(|| (Ipv6Addr::from(hop), f[9].to_string()))
    })
}

/// `route -n get default` (macOS, BSD): the "gateway:" line.
pub fn parse_route_get(text: &str) -> Option<String> {
    text.lines().find_map(|l| l.trim().strip_prefix("gateway:").map(|g| g.trim().to_string()))
}

#[cfg(unix)]
fn if_index(name: &str) -> u32 {
    extern "C" {
        fn if_nametoindex(name: *const std::ffi::c_char) -> u32;
    }
    let Ok(c) = std::ffi::CString::new(name) else { return 0 };
    // SAFETY: c is a valid nul-terminated string
    unsafe { if_nametoindex(c.as_ptr()) }
}

#[cfg(any(target_os = "macos", target_os = "freebsd", target_os = "openbsd", target_os = "netbsd"))]
fn route_get(args: &[&str]) -> Option<String> {
    let o = std::process::Command::new("route").args(args).output().ok()?;
    parse_route_get(&String::from_utf8_lossy(&o.stdout))
}

/// The IPv4 default gateway (where PCP / NAT-PMP requests go).
pub fn gateway_v4() -> Option<Ipv4Addr> {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        parse_proc_route(&std::fs::read_to_string("/proc/net/route").ok()?)
    }
    #[cfg(any(target_os = "macos", target_os = "freebsd", target_os = "openbsd", target_os = "netbsd"))]
    {
        route_get(&["-n", "get", "default"])?.parse().ok()
    }
    #[cfg(windows)]
    {
        win::gateway_v4()
    }
    #[cfg(not(any(windows, unix)))]
    {
        None
    }
}

/// The IPv6 default router (link-local, with its interface).
pub fn gateway_v6() -> Option<SocketAddrV6> {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        let (a, ifname) = parse_proc_ipv6_route(&std::fs::read_to_string("/proc/net/ipv6_route").ok()?)?;
        Some(SocketAddrV6::new(a, PORT, 0, if_index(&ifname)))
    }
    #[cfg(any(target_os = "macos", target_os = "freebsd", target_os = "openbsd", target_os = "netbsd"))]
    {
        let g = route_get(&["-n", "get", "-inet6", "default"])?;
        let (a, ifname) = g.split_once('%').map(|(a, i)| (a.to_string(), i.to_string())).unwrap_or((g, String::new()));
        Some(SocketAddrV6::new(a.parse().ok()?, PORT, 0, if_index(&ifname)))
    }
    #[cfg(windows)]
    {
        win::gateway_v6()
    }
    #[cfg(not(any(windows, unix)))]
    {
        None
    }
}

/// This PC's global IPv6 address towards the internet (nothing is sent):
/// what friends with IPv6 join.
pub fn global_v6() -> Option<Ipv6Addr> {
    let s = UdpSocket::bind("[::]:0").ok()?;
    s.connect("[2001:db8::1]:9").ok()?;
    match s.local_addr().ok()?.ip() {
        IpAddr::V6(a) if is_global_v6(&a) => Some(a),
        _ => None,
    }
}

/// Global unicast (2000::/3).
pub fn is_global_v6(a: &Ipv6Addr) -> bool {
    a.segments()[0] & 0xe000 == 0x2000
}

#[cfg(windows)]
mod win {
    //! iphlpapi's GetBestRoute / GetBestRoute2, loaded at run time.
    use std::ffi::c_void;
    use std::net::{Ipv4Addr, Ipv6Addr, SocketAddrV6};

    extern "system" {
        fn LoadLibraryW(name: *const u16) -> *mut c_void;
        fn GetProcAddress(module: *mut c_void, name: *const u8) -> *const c_void;
    }

    fn proc(name: &[u8]) -> *const c_void {
        let lib: Vec<u16> = "iphlpapi.dll".encode_utf16().chain(Some(0)).collect();
        // SAFETY: nul-terminated names; a null module or proc is checked by the callers
        unsafe {
            let m = LoadLibraryW(lib.as_ptr());
            if m.is_null() {
                return std::ptr::null();
            }
            GetProcAddress(m, name.as_ptr())
        }
    }

    pub fn gateway_v4() -> Option<Ipv4Addr> {
        let f = proc(b"GetBestRoute\0");
        if f.is_null() {
            return None;
        }
        type BestRoute = unsafe extern "system" fn(u32, u32, *mut [u32; 14]) -> u32;
        // SAFETY: GetBestRoute(DWORD dest, DWORD src, PMIB_IPFORWARDROW): 14 DWORDs
        let f: BestRoute = unsafe { std::mem::transmute(f) };
        let mut row = [0u32; 14];
        let dest = u32::from_ne_bytes([192, 0, 2, 1]);
        // SAFETY: row is a writable MIB_IPFORWARDROW
        if unsafe { f(dest, 0, &mut row) } != 0 {
            return None;
        }
        // dwForwardNextHop, in network order
        let g = Ipv4Addr::from(row[3].to_ne_bytes());
        (!g.is_unspecified()).then_some(g)
    }

    pub fn gateway_v6() -> Option<SocketAddrV6> {
        let get = proc(b"GetIpForwardTable2\0");
        let free = proc(b"FreeMibTable\0");
        if get.is_null() || free.is_null() {
            return None;
        }
        type Table2 = unsafe extern "system" fn(u16, *mut *mut u8) -> u32;
        type Free = unsafe extern "system" fn(*mut u8);
        // SAFETY: GetIpForwardTable2(ADDRESS_FAMILY, PMIB_IPFORWARD_TABLE2*) and FreeMibTable(PVOID)
        let (get, free): (Table2, Free) = unsafe { (std::mem::transmute(get), std::mem::transmute(free)) };
        let mut t: *mut u8 = std::ptr::null_mut();
        // AF_INET6 = 23
        // SAFETY: t receives a table the system allocated
        if unsafe { get(23, &mut t) } != 0 || t.is_null() {
            return None;
        }
        // MIB_IPFORWARD_TABLE2: ULONG NumEntries, then (at 8) MIB_IPFORWARD_ROW2[104 bytes]:
        // InterfaceIndex at 8, DestinationPrefix.PrefixLength at 40, NextHop
        // (SOCKADDR_INET) at 44, Metric at 84
        let mut best: Option<(u32, SocketAddrV6)> = None;
        // SAFETY: NumEntries rows follow the header, as the system wrote them
        unsafe {
            let n = u32::from_ne_bytes(std::slice::from_raw_parts(t, 4).try_into().unwrap()) as usize;
            for i in 0..n {
                let r = std::slice::from_raw_parts(t.add(8 + i * 104), 104);
                let u = |o: usize| u32::from_ne_bytes([r[o], r[o + 1], r[o + 2], r[o + 3]]);
                if r[40] != 0 || u16::from_ne_bytes([r[44], r[45]]) != 23 {
                    continue;
                }
                let mut a = [0u8; 16];
                a.copy_from_slice(&r[52..68]);
                let a = Ipv6Addr::from(a);
                if a.is_unspecified() {
                    continue;
                }
                let metric = u(84);
                if best.is_none_or(|b| metric < b.0) {
                    best = Some((metric, SocketAddrV6::new(a, super::PORT, 0, u(8))));
                }
            }
            free(t);
        }
        best.map(|b| b.1)
    }
}

// ------------------------------------------------------------ all three at once

/// Which protocol opened the port.
#[derive(Clone, Debug, PartialEq)]
pub enum Opened {
    Lease(Lease),
    Upnp(upnp::Mapping),
}

impl Opened {
    pub fn via(&self) -> &'static str {
        match self {
            Opened::Lease(l) => l.proto.name(),
            Opened::Upnp(_) => "UPnP",
        }
    }
    /// The router's internet address, when it said one.
    pub fn external_ip(&self) -> Option<String> {
        match self {
            Opened::Lease(l) => (!l.external.ip().is_unspecified()).then(|| l.external.ip().to_string()),
            Opened::Upnp(m) => m.external_ip.clone(),
        }
    }
}

/// What the three protocols gave for one hosted server.
#[derive(Clone, Debug, PartialEq)]
pub struct PortResult {
    pub opened: Option<Opened>,
    /// the IPv4 PCP / NAT-PMP attempt
    pub pcp: Outcome,
    /// UPnP's answer; None when it was not needed (PCP / NAT-PMP mapped first)
    pub upnp: Option<Result<upnp::Mapping, String>>,
    /// the IPv6 firewall pinhole (PCP), when the server has a global IPv6 address
    pub v6: Option<Outcome>,
}

/// Ask UPnP, PCP and NAT-PMP at once for UDP `port` (and PCP for the IPv6
/// socket's `v6` address and port). `upnp_map` is UPnP's discovery + mapping;
/// it skips the mapping when `done` is set (PCP / NAT-PMP mapped already).
pub fn open_with(gw4: Option<SocketAddr>, port: u16, gw6: Option<SocketAddr>, v6: Option<(Ipv6Addr, u16)>, upnp_map: Box<dyn FnOnce(Arc<AtomicBool>) -> Option<Result<upnp::Mapping, String>> + Send>) -> PortResult {
    let done = Arc::new(AtomicBool::new(false));
    let d2 = done.clone();
    let u = std::thread::spawn(move || upnp_map(d2));
    let v6 = match (gw6, v6) {
        (Some(g), Some((a, p))) => Some(std::thread::spawn(move || map_from(g, Some(IpAddr::V6(a)), p, LIFETIME, None))),
        _ => None,
    };
    let pcp = match gw4 {
        Some(g) => map(g, port, LIFETIME, None),
        None => Outcome::NoAnswer,
    };
    if pcp.lease().is_some() {
        done.store(true, Ordering::SeqCst);
    }
    let upnp = u.join().unwrap_or(None);
    let v6 = v6.map(|h| h.join().unwrap_or(Outcome::NoAnswer));
    let opened = match (&pcp, &upnp) {
        (Outcome::Mapped(l), _) => Some(Opened::Lease(l.clone())),
        (_, Some(Ok(m))) => Some(Opened::Upnp(m.clone())),
        _ => None,
    };
    PortResult { opened, pcp, upnp, v6 }
}

/// The real thing: this PC's gateways and UPnP discovery.
pub fn open_udp(port: u16, v6: Option<(Ipv6Addr, u16)>, desc: &'static str) -> PortResult {
    let gw4 = gateway_v4().map(|g| SocketAddr::new(g.into(), PORT));
    let gw6 = v6.and_then(|_| gateway_v6()).map(SocketAddr::V6);
    open_with(
        gw4,
        port,
        gw6,
        v6,
        Box::new(move |done: Arc<AtomicBool>| {
            let gws = upnp::discover(Duration::from_millis(2000));
            if done.load(Ordering::SeqCst) {
                return None;
            }
            if gws.is_empty() {
                return Some(Err("no UPnP router answered (UPnP off, or not a home router)".into()));
            }
            let mut last = String::new();
            for g in gws {
                if done.load(Ordering::SeqCst) {
                    return None;
                }
                match upnp::map_at(&g, port, desc) {
                    Ok(m) => return Some(Ok(m)),
                    Err(e) => last = e,
                }
            }
            Some(Err(last))
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// What a fake gateway does with a request.
    #[derive(Clone, Copy, PartialEq)]
    enum Kind {
        /// PCP server; maps to 203.0.113.7
        Pcp,
        /// NAT-PMP only: answers PCP with "unsupported version"
        NatPmp,
        /// NAT-PMP only and silent to PCP
        NatPmpSilent,
        /// PCP behind another NAT: ADDRESS_MISMATCH
        Mismatch,
        /// PCP, but its outside address is a CGNAT one
        PcpCgnat,
        Silent,
    }

    struct Fake {
        addr: SocketAddr,
        /// every request seen (raw)
        log: Arc<Mutex<Vec<Vec<u8>>>>,
        epoch: Arc<Mutex<u32>>,
    }

    fn fake(kind: Kind) -> Fake {
        let s = UdpSocket::bind("127.0.0.1:0").unwrap();
        let addr = s.local_addr().unwrap();
        let log = Arc::new(Mutex::new(Vec::new()));
        let epoch = Arc::new(Mutex::new(1000u32));
        let (l2, e2) = (log.clone(), epoch.clone());
        std::thread::spawn(move || {
            let _ = s.set_read_timeout(Some(Duration::from_secs(20)));
            let mut b = [0u8; 1100];
            while let Ok((n, from)) = s.recv_from(&mut b) {
                let d = b[..n].to_vec();
                l2.lock().unwrap().push(d.clone());
                let ep = *e2.lock().unwrap();
                let pcp = d.len() >= 60 && d[0] == 2;
                let answer: Option<Vec<u8>> = match kind {
                    Kind::Silent => None,
                    Kind::Pcp | Kind::Mismatch | Kind::PcpCgnat if pcp => {
                        let mut r = d.clone();
                        r[1] = 0x81;
                        r[2] = 0;
                        // the client address must be where the request came from
                        let client = from_v6_form(d[8..24].try_into().unwrap());
                        r[3] = if kind == Kind::Mismatch || client != from.ip() { 12 } else { 0 };
                        let life = u32::from_be_bytes([d[4], d[5], d[6], d[7]]);
                        r[4..8].copy_from_slice(&life.min(3600).to_be_bytes());
                        r[8..12].copy_from_slice(&ep.to_be_bytes());
                        r[12..24].fill(0);
                        let ext: IpAddr = if kind == Kind::PcpCgnat { "100.72.1.2".parse().unwrap() } else { "203.0.113.7".parse().unwrap() };
                        r[44..60].copy_from_slice(&v6_form(ext));
                        Some(r)
                    }
                    Kind::NatPmp if pcp => Some(vec![0, 0x81, 0, 1, 0, 0, 0, 0]),
                    Kind::NatPmp | Kind::NatPmpSilent if d.len() == 2 && d[..2] == [0, 0] => {
                        let mut r = vec![0, 128, 0, 0];
                        r.extend_from_slice(&ep.to_be_bytes());
                        r.extend_from_slice(&[198, 51, 100, 4]);
                        Some(r)
                    }
                    Kind::NatPmp | Kind::NatPmpSilent if d.len() == 12 && d[0] == 0 && d[1] == 1 => {
                        let mut r = vec![0, 129, 0, 0];
                        r.extend_from_slice(&ep.to_be_bytes());
                        r.extend_from_slice(&d[4..6]);
                        let ext = if u32::from_be_bytes([d[8], d[9], d[10], d[11]]) == 0 { [0, 0] } else { [0x9c, 0x41] };
                        r.extend_from_slice(&ext);
                        r.extend_from_slice(&d[8..12]);
                        Some(r)
                    }
                    _ => None,
                };
                if let Some(a) = answer {
                    let _ = s.send_to(&a, from);
                }
            }
        });
        Fake { addr, log, epoch }
    }

    #[test]
    fn pcp_maps_the_port_for_two_hours() {
        let g = fake(Kind::Pcp);
        let Outcome::Mapped(l) = map(g.addr, 30720, LIFETIME, None) else { panic!() };
        assert_eq!(l.proto, Proto::Pcp);
        assert_eq!(l.external, "203.0.113.7:30720".parse().unwrap());
        assert_eq!(l.local, "127.0.0.1".parse::<IpAddr>().unwrap());
        let req = g.log.lock().unwrap()[0].clone();
        assert_eq!(req.len(), 60);
        assert_eq!((req[0], req[1], req[36]), (2, 1, 17), "PCP version 2, MAP, UDP");
        assert_eq!(u32::from_be_bytes([req[4], req[5], req[6], req[7]]), 7200);
        assert_eq!(u16::from_be_bytes([req[40], req[41]]), 30720);
        assert_eq!(&req[8..24], &v6_form("127.0.0.1".parse().unwrap()));
        // the gateway granted 3600 s: kept as it said
        assert_eq!(l.lifetime, 3600);
    }

    #[test]
    fn a_nat_pmp_router_gets_the_nat_pmp_request() {
        for kind in [Kind::NatPmp, Kind::NatPmpSilent] {
            let g = fake(kind);
            let Outcome::Mapped(l) = map(g.addr, 30720, LIFETIME, None) else { panic!() };
            assert_eq!(l.proto, Proto::NatPmp);
            assert_eq!(l.external, "198.51.100.4:40001".parse().unwrap());
            let log = g.log.lock().unwrap().clone();
            assert_eq!(log[0][0], 2, "PCP first");
            let m = log.iter().find(|d| d.len() == 12).expect("a NAT-PMP mapping request");
            assert_eq!(m, &natpmp_map_request(30720, 30720, 7200));
        }
    }

    #[test]
    fn mismatch_and_cgnat_addresses_are_the_cgnat_signal() {
        let g = fake(Kind::Mismatch);
        assert_eq!(map(g.addr, 30720, LIFETIME, None), Outcome::Mismatch);
        let g = fake(Kind::PcpCgnat);
        let r = open_with(Some(g.addr), 30720, None, None, Box::new(|_| None));
        assert_eq!(r.opened.as_ref().and_then(|o| o.external_ip()).as_deref(), Some("100.72.1.2"));
        assert!(upnp::is_private("100.72.1.2"));
    }

    #[test]
    fn a_silent_gateway_times_out_quickly() {
        let g = fake(Kind::Silent);
        let t = Instant::now();
        assert_eq!(map(g.addr, 30720, LIFETIME, None), Outcome::NoAnswer);
        // PCP twice and NAT-PMP twice, 250 ms each
        assert!(t.elapsed() < Duration::from_millis(1500), "{:?}", t.elapsed());
        assert_eq!(g.log.lock().unwrap().len(), 4);
    }

    #[test]
    fn the_epoch_rule() {
        let t0 = Instant::now();
        let t = t0 + Duration::from_secs(600);
        assert!(!epoch_lost(1000, t0, 1600, t));
        assert!(!epoch_lost(1000, t0, 1590, t), "small drift");
        assert!(epoch_lost(1000, t0, 20, t), "went back: restarted");
        assert!(epoch_lost(1000, t0, 1100, t), "500 s missing: restarted and running since");
        assert!(epoch_lost(1000, t0, 5000, t), "too far ahead");
    }

    #[test]
    fn renewal_remaps_after_a_router_restart_and_stop_deletes() {
        let g = fake(Kind::Pcp);
        let Outcome::Mapped(l) = map(g.addr, 30720, LIFETIME, None) else { panic!() };
        let (tx, rx) = std::sync::mpsc::channel();
        let k = keep(l.clone(), Duration::from_millis(300), tx);
        // the router restarts: its epoch starts again from 0
        *g.epoch.lock().unwrap() = 3;
        match rx.recv_timeout(Duration::from_secs(3)).unwrap() {
            Event::Remapped(n) => assert_eq!(n.external, l.external),
            e => panic!("{e:?}"),
        }
        // a renewal keeps the nonce (the same mapping)
        let log = g.log.lock().unwrap().clone();
        assert!(log.len() >= 2 && log[1][24..36] == log[0][24..36]);
        k.stop(true);
        let log = g.log.lock().unwrap().clone();
        let last = log.last().unwrap();
        assert_eq!(u32::from_be_bytes([last[4], last[5], last[6], last[7]]), 0, "deleted with lifetime 0");
    }

    #[test]
    fn refusals_in_plain_words() {
        assert_eq!(Proto::Pcp.refusal(2), "PCP: not allowed");
        assert_eq!(Proto::NatPmp.refusal(2), "NAT-PMP: not allowed");
        assert_eq!(Proto::NatPmp.refusal(3), "NAT-PMP: the router has no internet connection");
        assert_eq!(Proto::Pcp.refusal(8), "PCP: the router has no mappings left");
        assert_eq!(Proto::Pcp.refusal(99), "PCP error 99");
        assert_eq!(Proto::NatPmp.reason(9), None);
    }

    #[test]
    fn nat_pmp_delete_is_lifetime_zero_port_zero() {
        let g = fake(Kind::NatPmp);
        let Outcome::Mapped(l) = map(g.addr, 30720, LIFETIME, None) else { panic!() };
        delete(&l);
        let log = g.log.lock().unwrap().clone();
        assert_eq!(log.last().unwrap(), &natpmp_map_request(30720, 0, 0));
    }

    #[test]
    fn pcp_first_and_upnp_skips_its_mapping() {
        let g = fake(Kind::Pcp);
        let r = open_with(
            Some(g.addr),
            30720,
            None,
            None,
            Box::new(|done: Arc<AtomicBool>| {
                // UPnP's discovery takes longer than PCP
                std::thread::sleep(Duration::from_millis(400));
                (!done.load(Ordering::SeqCst)).then(|| Err("mapped by UPnP".to_string()))
            }),
        );
        assert_eq!(r.opened.as_ref().map(|o| o.via()), Some("PCP"));
        assert_eq!(r.upnp, None, "no second mapping");
        // nothing on PCP: UPnP's answer stands
        let s = fake(Kind::Silent);
        let m = upnp::Mapping { control_url: String::new(), service: String::new(), port: 30720, local_ip: Ipv4Addr::LOCALHOST, external_ip: Some("203.0.113.9".into()), lease: LIFETIME, desc: String::new() };
        let m2 = m.clone();
        let r = open_with(Some(s.addr), 30720, None, None, Box::new(move |_| Some(Ok(m2))));
        assert_eq!(r.opened, Some(Opened::Upnp(m)));
        assert_eq!(r.pcp, Outcome::NoAnswer);
    }

    #[test]
    fn ipv6_pinhole_by_pcp() {
        let Ok(s) = UdpSocket::bind("[::1]:0") else {
            eprintln!("no ipv6 loopback: skipping");
            return;
        };
        drop(s);
        // a PCP server on ::1
        let srv = UdpSocket::bind("[::1]:0").unwrap();
        let a = srv.local_addr().unwrap();
        std::thread::spawn(move || {
            let mut b = [0u8; 200];
            if let Ok((n, from)) = srv.recv_from(&mut b) {
                let mut r = b[..n].to_vec();
                r[1] = 0x81;
                r[3] = 0;
                r[8..12].copy_from_slice(&5u32.to_be_bytes());
                // a firewall keeps address and port
                let _ = srv.send_to(&r, from);
            }
        });
        let r = open_with(None, 30720, Some(a), Some(("::1".parse().unwrap(), 30721)), Box::new(|_| None));
        let Some(Outcome::Mapped(l)) = r.v6 else { panic!("{:?}", r.v6) };
        assert_eq!(l.internal_port, 30721);
        assert_eq!(l.local, "::1".parse::<IpAddr>().unwrap());
        assert!(r.opened.is_none(), "the ipv4 port is a separate matter");
    }

    #[test]
    fn gateway_tables() {
        let route = "Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT\n\
                     eth0\t0001A8C0\t00000000\t0001\t0\t0\t0\t00FFFFFF\t0\t0\t0\n\
                     eth0\t00000000\t0101A8C0\t0003\t0\t0\t100\t00000000\t0\t0\t0\n";
        assert_eq!(parse_proc_route(route), Some(Ipv4Addr::new(192, 168, 1, 1)));
        assert_eq!(parse_proc_route("Iface\n"), None);
        let r6 = "20010db8000000000000000000000000 40 00000000000000000000000000000000 00 00000000000000000000000000000000 00000100 00000001 00000000 00000001     eth0\n\
                  00000000000000000000000000000000 00 00000000000000000000000000000000 00 fe800000000000000000000000000001 00000400 00000001 00000000 00000003     eth0\n";
        assert_eq!(parse_proc_ipv6_route(r6), Some(("fe80::1".parse().unwrap(), "eth0".into())));
        assert_eq!(parse_route_get("   route to: default\ndestination: default\n    gateway: 10.0.1.1\n  interface: en0\n").as_deref(), Some("10.0.1.1"));
        assert!(is_global_v6(&"2a02:1:2::5".parse().unwrap()));
        assert!(!is_global_v6(&"fe80::1".parse().unwrap()) && !is_global_v6(&"fd00::1".parse().unwrap()) && !is_global_v6(&"::1".parse().unwrap()));
    }
}
