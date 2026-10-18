//! The NAT type line of the host status (dec076): what kind of network the
//! hosting PC is behind, in the words consoles use (Open / Moderate /
//! Strict) plus CGNAT.
//!
//! Only when a network server starts (next to the port forwarding and the
//! firewall check), and only with `net_natCheck 1` (archived, the default;
//! 0 turns it off): one STUN Binding request (RFC 8489) from the server's
//! own game socket to each of two STUN servers run by different operators
//! on different addresses (`net_stunServers`). Each answer says which
//! address and port the router gave the game socket towards that server.
//! The same port both times: the router keeps one mapping for every
//! destination ("endpoint-independent mapping", RFC 4787). Different ports:
//! it makes a new one per destination ("endpoint-dependent", the old
//! "symmetric NAT"). The address equal to this PC's own: no NAT at all. The
//! only data sent is this PC's address and port, which every game server
//! it plays on also sees; the console says which servers were asked.
//!
//! One more request goes to the first server from a new socket of its own
//! (an ordinary outgoing connection, like the PC's other traffic). With two
//! internet connections (multi-WAN, policy routing) the router may send the
//! game's traffic out of one and the PC's other traffic out of the other:
//! the two answers then name different internet addresses and the network
//! type reads "Unclear" (rare). Players join at the game's address.
//! The same holds when the router opens the port on one internet address
//! while the game's own traffic leaves from another (the STUN answers):
//! "Unclear" too, and players join at the address the router opened. Two
//! different public addresses are never CGNAT: that needs a shared
//! (100.64.0.0/10) or private address on the router's internet side.
//!
//! Public STUN servers cannot tell the router's filtering apart (that needs
//! a server with two addresses, RFC 5780), so a mapping kept for every
//! destination with no port opened reads "Moderate": the honest lower bound.

use std::net::{IpAddr, Ipv4Addr, SocketAddr, ToSocketAddrs};
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};
use trem_sim::kernel::netchan::{Net, NetSrc};
use trem_sim::kernel::stun;

/// Two operators, two networks (Cloudflare: "free and unlimited").
pub const DEFAULT_SERVERS: &str = "stun.cloudflare.com:3478 stun.l.google.com:19302";
/// A second send to a server that has not answered after this long.
const RETRY: Duration = Duration::from_millis(500);
/// The check gives up this long after the first send.
const GIVE_UP: Duration = Duration::from_millis(1500);
/// ... and when the names have not resolved after this long.
const RESOLVE_WAIT: Duration = Duration::from_secs(3);

/// The longest frame a deadline counts in full (frame_clock).
pub const MAX_STEP: Duration = Duration::from_millis(250);

/// Time the game spent running frames: a longer frame (a map load freezes
/// the main loop for seconds) counts as `MAX_STEP`. The check's and the
/// status's deadlines use it, so a stall does not use them up before the
/// requests were sent or their answers read.
#[derive(Clone, Copy, Debug, Default)]
pub struct FrameClock {
    last: Option<Instant>,
    secs: f64,
}

impl FrameClock {
    /// One frame at `now`: the running time so far (0 on the first).
    pub fn tick(&mut self, now: Instant) -> f64 {
        if let Some(l) = self.last {
            self.secs += now.saturating_duration_since(l).min(MAX_STEP).as_secs_f64();
        }
        self.last = Some(now);
        self.secs
    }
}

/// What the network is, as the host status says it.
#[derive(Clone, Debug, PartialEq)]
pub enum Nat {
    /// this PC has a public address (no NAT)
    OpenPublic,
    /// the router opened the game's port
    OpenMapped,
    /// one mapping for every destination, no port opened
    Moderate,
    /// a new mapping (port) per destination
    Strict,
    /// the provider's shared address (or another router in front)
    Cgnat(String),
    /// not enough answers to tell
    Unknown(&'static str),
    /// the game's internet address is not the one the PC's other traffic
    /// uses (two internet connections: multi-WAN / policy routing)
    Unclear { game: Ipv4Addr, other: Ipv4Addr },
    /// the router opened the port on one public address, the game's own
    /// traffic leaves from another (two internet connections; players join
    /// at the router's)
    Split { router: Ipv4Addr, seen: Ipv4Addr },
}

/// What the router step found (pcp.rs / upnp.rs), for the classification.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RouterFacts {
    /// a protocol opened the port
    pub opened: bool,
    /// the router's internet address, when it said one
    pub external: Option<Ipv4Addr>,
    /// PCP ADDRESS_MISMATCH, or an external address that is private / 100.64/10
    pub behind: bool,
}

/// Is `a` a provider-shared (100.64.0.0/10) or private address?
fn shared(a: &Ipv4Addr) -> bool {
    crate::upnp::is_private(&a.to_string())
}

/// 100.64.0.0/10, the range set aside for providers' shared NAT (RFC 6598).
fn cgn_range(a: &Ipv4Addr) -> bool {
    let o = a.octets();
    o[0] == 100 && (64..128).contains(&o[1])
}

/// The table of report.md H2. `local`: this PC's address facing the default
/// route; `mapped`: the answers of servers on different addresses, in order;
/// `other`: what the first server saw from a new socket (the PC's other
/// traffic). CGNAT only with evidence: an answer or the router's internet
/// address in 100.64.0.0/10, or a private router address while the servers
/// see a public one. Two public addresses are two internet connections.
pub fn classify(local: Option<Ipv4Addr>, mapped: &[SocketAddr], router: &RouterFacts, other: Option<SocketAddr>) -> Nat {
    let m4: Vec<(Ipv4Addr, u16)> = mapped
        .iter()
        .filter_map(|m| match m.ip() {
            IpAddr::V4(a) => Some((a, m.port())),
            _ => None,
        })
        .collect();
    if m4.iter().any(|m| Some(m.0) == local) {
        return Nat::OpenPublic;
    }
    if let Some(m) = m4.iter().find(|m| shared(&m.0)) {
        return Nat::Cgnat(m.0.to_string());
    }
    let ext = router.external.filter(|e| !e.is_unspecified());
    if router.behind {
        // PCP ADDRESS_MISMATCH, or the router's internet address is not public
        match ext {
            Some(e) if cgn_range(&e) => return Nat::Cgnat(e.to_string()),
            Some(e) if m4.first().is_some_and(|m| m.0 != e) => return Nat::Cgnat(e.to_string()),
            // no address to show it: what the answers say (below), never Open
            _ => {}
        }
    }
    // two internet connections: the game's address (the answers to the game
    // socket, else the router's) is not the one the PC's other traffic uses
    if let (Some(IpAddr::V4(o)), Some(game)) = (other.map(|o| o.ip()), m4.first().map(|m| m.0).or(ext)) {
        if o != game && ext != Some(o) {
            return Nat::Unclear { game, other: o };
        }
    }
    // the router opened the port on another public address than the one
    // the game's traffic leaves from (multi-WAN): not CGNAT (both public)
    if let (Some(e), Some(m), false) = (ext, m4.first(), router.behind) {
        if router.opened && e != m.0 {
            return Nat::Split { router: e, seen: m.0 };
        }
    }
    if router.opened && !router.behind {
        return Nat::OpenMapped;
    }
    match m4.as_slice() {
        [] => Nat::Unknown("no STUN server answered"),
        [_] => Nat::Unknown("only one STUN server answered"),
        [a, b, ..] if a.1 == b.1 => Nat::Moderate,
        _ => Nat::Strict,
    }
}

impl Nat {
    /// The network type in plain words, the official name in brackets
    /// (dec076). It describes the router side only: this PC's firewall is
    /// its own row of the status. `port`: the server's port.
    pub fn label(&self, port: u16) -> String {
        match self {
            Nat::OpenPublic => "Open - this PC has its own public address (no NAT)".into(),
            Nat::OpenMapped => format!("Open - the router opened UDP {port} for this game (port mapping)"),
            Nat::Moderate => "Moderate - port stays the same (cone NAT)".into(),
            Nat::Strict => "Strict - port changes per player (symmetric NAT)".into(),
            Nat::Cgnat(ip) if ip.is_empty() => "CGNAT - your provider shares one address (carrier-grade NAT)".into(),
            Nat::Cgnat(ip) => format!("CGNAT - your provider shares one address, {ip} (carrier-grade NAT)"),
            Nat::Unknown(why) => format!("Not known - {why}"),
            Nat::Unclear { .. } => "Unclear - the game's internet address differs from this PC's other traffic: two internet connections, rare (multi-WAN / policy routing)".into(),
            Nat::Split { .. } => "Unclear - the router opened the port on another internet address than the game's traffic uses: two internet connections, rare (multi-WAN / policy routing)".into(),
        }
    }

    /// The console's line under the network type, when it is Unclear (either kind).
    pub fn detail(&self, port: u16) -> Option<String> {
        match self {
            Nat::Unclear { game, other } => Some(format!("The game is seen at {game}, this PC's other traffic at {other}. Players join at the game's address, {game}:{port}.")),
            Nat::Split { router, seen } => Some(format!("The router opened UDP {port} on {router}; the STUN servers see the game's traffic at {seen}. Players on the internet join at {router}:{port}, the address the router opened.")),
            _ => None,
        }
    }

    /// The status row: "Network type: ...".
    pub fn line(&self, port: u16) -> String {
        format!("Network type: {}", self.label(port))
    }
}

/// `host:port` (port 3478 when missing) to its IPv4 address.
pub fn resolve_v4(name: &str) -> Option<SocketAddr> {
    let with_port = if name.rsplit_once(':').is_some_and(|(_, p)| p.parse::<u16>().is_ok()) { name.to_string() } else { format!("{name}:{}", stun::PORT) };
    with_port.to_socket_addrs().ok()?.find(|a| a.is_ipv4())
}

/// One server of the check.
#[derive(Clone, Debug, PartialEq)]
pub struct Target {
    pub name: String,
    pub addr: Option<SocketAddr>,
    txid: [u8; 12],
    pub mapped: Option<SocketAddr>,
    sends: u32,
    /// on the same address as an earlier server: not asked
    same: bool,
}

/// The check of one hosted server: resolve (on a thread), send, collect.
pub struct Probe {
    resolving: Option<Receiver<Vec<(String, Option<SocketAddr>)>>>,
    clock: FrameClock,
    /// the frame clock at the first send
    sent_at: Option<f64>,
    pub targets: Vec<Target>,
    /// the one request from a new socket (to the first server), and its socket
    pub other: Option<Target>,
    other_sock: Option<std::net::UdpSocket>,
    /// where that socket binds (tests use another loopback address)
    pub other_bind: String,
    pub done: bool,
}

/// `net_stunServers` as a list (space or comma separated).
pub fn server_list(cvar: &str) -> Vec<String> {
    cvar.split(|c: char| c.is_whitespace() || c == ',').filter(|s| !s.is_empty()).map(str::to_string).collect()
}

impl Probe {
    pub fn start(servers: Vec<String>, resolve: fn(&str) -> Option<SocketAddr>) -> Probe {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(servers.iter().map(|n| (n.clone(), resolve(n))).collect());
        });
        Probe { resolving: Some(rx), clock: FrameClock::default(), sent_at: None, targets: Vec::new(), other: None, other_sock: None, other_bind: "0.0.0.0:0".into(), done: false }
    }

    /// One frame: send once the names are known, read the answers, retry
    /// once, give up. True once the check is over.
    pub fn poll(&mut self, net: &mut Net) -> bool {
        self.poll_at(net, Instant::now())
    }

    /// `poll` at `now` (tests simulate a stalled frame with it).
    pub fn poll_at(&mut self, net: &mut Net, now: Instant) -> bool {
        if self.done {
            return true;
        }
        let t = self.clock.tick(now);
        if let Some(rx) = self.resolving.as_ref() {
            match rx.try_recv() {
                Ok(list) => {
                    self.resolving = None;
                    let mut seen: Vec<IpAddr> = Vec::new();
                    for (k, (name, addr)) in list.into_iter().enumerate() {
                        // servers on the same address cannot tell the mappings apart
                        let same = addr.is_some_and(|a| seen.contains(&a.ip()));
                        let addr = addr.filter(|_| !same);
                        if let Some(a) = addr {
                            seen.push(a.ip());
                        }
                        let mut txid = [0u8; 12];
                        let r = std::collections::hash_map::RandomState::new();
                        let h = std::hash::BuildHasher::hash_one(&r, (k, Instant::now()));
                        txid[..8].copy_from_slice(&h.to_be_bytes());
                        txid[8..].copy_from_slice(&(k as u32).to_be_bytes());
                        self.targets.push(Target { name, addr, txid, mapped: None, sends: 0, same });
                    }
                    // one request from a new socket to the first server
                    if let Some(first) = self.targets.iter().find(|t| t.addr.is_some()) {
                        let mut o = first.clone();
                        o.txid[8..].copy_from_slice(&u32::MAX.to_be_bytes());
                        let sock = std::net::UdpSocket::bind(self.other_bind.as_str()).ok().filter(|s| s.set_nonblocking(true).is_ok());
                        if let Some(s) = &sock {
                            if s.send_to(&stun::binding_request(&o.txid), o.addr.unwrap()).is_ok() {
                                o.sends = 1;
                            }
                        }
                        if o.sends == 1 {
                            self.other = Some(o);
                            self.other_sock = sock;
                        }
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Empty) if t < RESOLVE_WAIT.as_secs_f64() => return false,
                Err(_) => {
                    self.resolving = None;
                    return self.finish(net);
                }
            }
        }
        for (from, d) in net.take_stun_replies() {
            if let Some(t) = self.targets.iter_mut().find(|t| t.addr == Some(from)) {
                if let Some(m) = stun::mapped_address(&d, &t.txid) {
                    t.mapped = Some(m);
                }
            }
        }
        if let (Some(o), Some(s)) = (self.other.as_mut(), self.other_sock.as_ref()) {
            let mut b = [0u8; 600];
            while let Ok((n, from)) = s.recv_from(&mut b) {
                if Some(from) == o.addr {
                    if let Some(m) = stun::mapped_address(&b[..n], &o.txid) {
                        o.mapped = Some(m);
                    }
                }
            }
        }
        let since = t - *self.sent_at.get_or_insert(t);
        for t in self.targets.iter_mut() {
            let Some(a) = t.addr else { continue };
            let due = match t.sends {
                0 => true,
                1 => t.mapped.is_none() && since >= RETRY.as_secs_f64(),
                _ => false,
            };
            if due {
                net.stun_send(NetSrc::Server, a, t.txid);
                t.sends += 1;
            }
        }
        let all = self.targets.iter().all(|t| t.addr.is_none() || t.mapped.is_some()) && self.other.as_ref().is_none_or(|o| o.mapped.is_some());
        if all || since >= GIVE_UP.as_secs_f64() {
            return self.finish(net);
        }
        false
    }

    fn finish(&mut self, net: &mut Net) -> bool {
        net.stun_done();
        self.other_sock = None;
        self.done = true;
        true
    }

    /// The answers, in the order of the list.
    pub fn mapped(&self) -> Vec<SocketAddr> {
        self.targets.iter().filter_map(|t| t.mapped).collect()
    }

    /// What the first server saw from the new socket (the PC's other traffic).
    pub fn other_mapped(&self) -> Option<SocketAddr> {
        self.other.as_ref().and_then(|o| o.mapped)
    }

    /// "Network check: asked a (1.2.3.4), b (no address)."
    pub fn asked_line(&self) -> String {
        let l: Vec<String> = self
            .targets
            .iter()
            .map(|t| match (t.addr, t.mapped) {
                (Some(a), Some(_)) => format!("{} ({})", t.name, a.ip()),
                (Some(a), None) => format!("{} ({}, no answer)", t.name, a.ip()),
                (None, _) if t.same => format!("{} (same address as another, not asked)", t.name),
                (None, _) => format!("{} (no address)", t.name),
            })
            .collect();
        let other = match &self.other {
            Some(o) if o.mapped.is_some() => format!(", and {} once more from a separate socket", o.name),
            Some(o) => format!(", and {} once more from a separate socket (no answer)", o.name),
            None => String::new(),
        };
        format!("Network check (net_natCheck 0 turns it off): asked the STUN servers {}{other}.", l.join(", "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::UdpSocket;

    fn sa(s: &str) -> SocketAddr {
        s.parse().unwrap()
    }

    #[test]
    fn the_classification_table() {
        let none = RouterFacts::default();
        let local = Some(Ipv4Addr::new(192, 168, 1, 20));
        // EIM: the same port to both servers, nothing opened
        assert_eq!(classify(local, &[sa("203.0.113.7:30720"), sa("203.0.113.7:30720")], &none, None), Nat::Moderate);
        // EDM: a new port per destination
        assert_eq!(classify(local, &[sa("203.0.113.7:51001"), sa("203.0.113.7:51002")], &none, None), Nat::Strict);
        // a public address on the PC
        assert_eq!(classify(Some(Ipv4Addr::new(203, 0, 113, 7)), &[sa("203.0.113.7:30720")], &none, None), Nat::OpenPublic);
        // the router opened the port: Open even with a strict mapping
        let opened = RouterFacts { opened: true, external: Some(Ipv4Addr::new(203, 0, 113, 7)), behind: false };
        assert_eq!(classify(local, &[sa("203.0.113.7:51001"), sa("203.0.113.7:51002")], &opened, None), Nat::OpenMapped);
        assert_eq!(classify(local, &[], &opened, None), Nat::OpenMapped);
        // CGNAT: the internet sees a shared address
        assert_eq!(classify(local, &[sa("100.72.3.4:30720"), sa("100.72.3.4:30720")], &none, None), Nat::Cgnat("100.72.3.4".into()));
        // ... or PCP said ADDRESS_MISMATCH / the router's address is private
        let behind = RouterFacts { opened: false, external: Some(Ipv4Addr::new(10, 1, 2, 3)), behind: true };
        assert_eq!(classify(local, &[sa("203.0.113.7:30720"), sa("203.0.113.7:30720")], &behind, None), Nat::Cgnat("10.1.2.3".into()));
        // a public router address the servers do not see is two internet connections, not CGNAT
        let other = RouterFacts { opened: true, external: Some(Ipv4Addr::new(198, 51, 100, 1)), behind: false };
        assert_eq!(classify(local, &[sa("203.0.113.7:30720")], &other, None), Nat::Split { router: Ipv4Addr::new(198, 51, 100, 1), seen: Ipv4Addr::new(203, 0, 113, 7) });
        // two internet connections: the separate socket leaves by another address
        let wan2 = Some(sa("198.51.100.9:61000"));
        let game = Ipv4Addr::new(203, 0, 113, 7);
        let other_ip = Ipv4Addr::new(198, 51, 100, 9);
        assert_eq!(classify(local, &[sa("203.0.113.7:30720"), sa("203.0.113.7:30720")], &none, wan2), Nat::Unclear { game, other: other_ip });
        assert_eq!(classify(local, &[sa("203.0.113.7:30720")], &opened, wan2), Nat::Unclear { game, other: other_ip });
        assert_eq!(classify(local, &[], &opened, wan2), Nat::Unclear { game, other: other_ip });
        // ... the same address: unchanged
        let same = Some(sa("203.0.113.7:61000"));
        assert_eq!(classify(local, &[sa("203.0.113.7:30720"), sa("203.0.113.7:30720")], &none, same), Nat::Moderate);
        assert_eq!(classify(local, &[sa("203.0.113.7:51001"), sa("203.0.113.7:51002")], &opened, same), Nat::OpenMapped);
        // ... equal to the router's address: the router's and the game's addresses differ
        let at_router = RouterFacts { opened: true, external: Some(other_ip), behind: false };
        assert_eq!(classify(local, &[sa("203.0.113.7:30720")], &at_router, wan2), Nat::Split { router: other_ip, seen: game });
        // ... CGNAT and a public PC address stay what they were
        assert_eq!(classify(local, &[sa("100.72.3.4:30720"), sa("100.72.3.4:30720")], &none, wan2), Nat::Cgnat("100.72.3.4".into()));
        assert_eq!(classify(Some(game), &[sa("203.0.113.7:30720")], &none, wan2), Nat::OpenPublic);
        // not enough answers
        assert_eq!(classify(local, &[], &none, None), Nat::Unknown("no STUN server answered"));
        assert_eq!(classify(local, &[], &none, wan2), Nat::Unknown("no STUN server answered"));
        assert_eq!(classify(local, &[sa("203.0.113.7:30720")], &none, None), Nat::Unknown("only one STUN server answered"));
    }

    /// Seen in a Windows 11 test and reproduced on Linux and Wine on the same
    /// network (the addresses here are documentation ones): a router with two
    /// internet connections. PCP opened the port on the default-route
    /// connection, 203.0.113.172; the game's STUN traffic (both servers, and
    /// the separate socket) leaves by the other one, 198.51.100.201. Every
    /// address public: the old table said "CGNAT ... 203.0.113.172".
    #[test]
    fn two_public_addresses_are_not_cgnat() {
        let local = Some(Ipv4Addr::new(192, 168, 1, 20));
        let pcp = RouterFacts { opened: true, external: Some(Ipv4Addr::new(203, 0, 113, 172)), behind: false };
        let mapped = [sa("198.51.100.201:30771"), sa("198.51.100.201:30771")];
        let n = classify(local, &mapped, &pcp, Some(sa("198.51.100.201:38348")));
        assert_eq!(n, Nat::Split { router: Ipv4Addr::new(203, 0, 113, 172), seen: Ipv4Addr::new(198, 51, 100, 201) });
        assert!(n.line(30771).starts_with("Network type: Unclear - the router opened the port on another internet address"), "{}", n.line(30771));
        assert!(!n.label(30771).to_ascii_lowercase().contains("cgnat") && !n.label(30771).contains("carrier"));
        assert_eq!(
            n.detail(30771).unwrap(),
            "The router opened UDP 30771 on 203.0.113.172; the STUN servers see the game's traffic at 198.51.100.201. Players on the internet join at 203.0.113.172:30771, the address the router opened."
        );
        // the same answers without the separate socket's (Wine run: 59752), and with one server only
        assert_eq!(classify(local, &mapped[..1], &pcp, None), n);
        // CGNAT only with evidence: the router's internet side in 100.64.0.0/10 ...
        let shared_wan = RouterFacts { opened: true, external: Some(Ipv4Addr::new(100, 72, 0, 5)), behind: true };
        assert_eq!(classify(local, &mapped, &shared_wan, None), Nat::Cgnat("100.72.0.5".into()));
        assert_eq!(classify(local, &[], &shared_wan, None), Nat::Cgnat("100.72.0.5".into()));
        // ... or a private one while the servers see a public address
        let private_wan = RouterFacts { opened: true, external: Some(Ipv4Addr::new(10, 0, 0, 2)), behind: true };
        assert_eq!(classify(local, &mapped, &private_wan, None), Nat::Cgnat("10.0.0.2".into()));
        // a private router address and no answer: not known, and never Open
        assert_eq!(classify(local, &[], &private_wan, None), Nat::Unknown("no STUN server answered"));
        // PCP ADDRESS_MISMATCH alone names no address: the answers decide (the Router row says what is in front)
        let mismatch = RouterFacts { opened: false, external: None, behind: true };
        assert_eq!(classify(local, &mapped, &mismatch, None), Nat::Moderate);
        assert_eq!(classify(local, &[sa("198.51.100.201:51001"), sa("198.51.100.201:51002")], &mismatch, None), Nat::Strict);
        // the router's address seen by the servers too: Open, as before
        let same = RouterFacts { opened: true, external: Some(Ipv4Addr::new(198, 51, 100, 201)), behind: false };
        assert_eq!(classify(local, &mapped, &same, Some(sa("198.51.100.201:38348"))), Nat::OpenMapped);
    }

    #[test]
    fn the_lines() {
        assert_eq!(Nat::Moderate.line(30720), "Network type: Moderate - port stays the same (cone NAT)");
        assert_eq!(Nat::Strict.label(30720), "Strict - port changes per player (symmetric NAT)");
        assert_eq!(Nat::OpenMapped.label(30720), "Open - the router opened UDP 30720 for this game (port mapping)");
        assert!(Nat::OpenPublic.label(1).ends_with("its own public address (no NAT)"));
        assert!(Nat::Cgnat("100.72.3.4".into()).label(1).contains("shares one address, 100.72.3.4 (carrier-grade NAT)"));
        assert_eq!(Nat::Unknown("no STUN server answered").label(1), "Not known - no STUN server answered");
        let unclear = Nat::Unclear { game: Ipv4Addr::new(203, 0, 113, 7), other: Ipv4Addr::new(198, 51, 100, 9) };
        assert_eq!(unclear.line(30720), "Network type: Unclear - the game's internet address differs from this PC's other traffic: two internet connections, rare (multi-WAN / policy routing)");
        assert_eq!(unclear.detail(30720).unwrap(), "The game is seen at 203.0.113.7, this PC's other traffic at 198.51.100.9. Players join at the game's address, 203.0.113.7:30720.");
        assert_eq!(Nat::Moderate.detail(30720), None);
        let split = Nat::Split { router: Ipv4Addr::new(198, 51, 100, 1), seen: Ipv4Addr::new(203, 0, 113, 7) };
        for n in [Nat::OpenPublic, Nat::OpenMapped, Nat::Moderate, Nat::Strict, Nat::Cgnat(String::new()), Nat::Unknown("x"), unclear, split] {
            let l = n.label(30720).to_ascii_lowercase();
            assert!(!l.contains("friend") && !l.contains("router + pc"), "{l}");
            if matches!(n, Nat::Unclear { .. } | Nat::Split { .. }) {
                assert!(!l.contains("cgnat") && !l.contains("double nat") && !l.contains("carrier"), "{l}");
            }
        }
        assert_eq!(server_list("stun.cloudflare.com:3478  stun.l.google.com:19302,x"), vec!["stun.cloudflare.com:3478", "stun.l.google.com:19302", "x"]);
        assert_eq!(server_list(DEFAULT_SERVERS).len(), 2);
    }

    /// A fake STUN server on `ip` that answers with `map(seen source)`.
    fn fake_stun(ip: &str, map: fn(SocketAddr) -> SocketAddr) -> SocketAddr {
        let s = UdpSocket::bind((ip, 0)).unwrap();
        let a = s.local_addr().unwrap();
        std::thread::spawn(move || {
            let _ = s.set_read_timeout(Some(Duration::from_secs(10)));
            let mut b = [0u8; 600];
            while let Ok((n, from)) = s.recv_from(&mut b) {
                if let Some((stun::BINDING_REQUEST, id)) = stun::header(&b[..n]) {
                    let _ = s.send_to(&stun::binding_success(&id, map(from)), from);
                }
            }
        });
        a
    }

    fn run(servers: [SocketAddr; 2]) -> (Probe, Net) {
        let mut net = Net::new();
        net.open(NetSrc::Server, "127.0.0.1", 0).unwrap();
        // the "names" are the addresses themselves
        let mut p = Probe::start(servers.iter().map(|a| a.to_string()).collect(), |n| n.parse().ok());
        let t = Instant::now();
        while !p.poll(&mut net) {
            while net.get_packet(NetSrc::Server).is_some() {}
            std::thread::sleep(Duration::from_millis(5));
            assert!(t.elapsed() < Duration::from_secs(5));
        }
        (p, net)
    }

    #[test]
    fn two_fake_servers_endpoint_independent_and_dependent() {
        // a NAT shim: the router keeps one outside port (EIM) ...
        let a = fake_stun("127.0.0.1", |_| sa("203.0.113.7:40000"));
        let b = fake_stun("127.0.0.2", |_| sa("203.0.113.7:40000"));
        let (p, _) = run([a, b]);
        assert_eq!(p.mapped().len(), 2);
        assert_eq!(classify(Some(Ipv4Addr::new(192, 168, 1, 20)), &p.mapped(), &RouterFacts::default(), None), Nat::Moderate);
        assert!(p.asked_line().contains("127.0.0.1:") && p.asked_line().contains("(127.0.0.2)"), "{}", p.asked_line());
        // ... or a new one per destination (EDM)
        let a = fake_stun("127.0.0.1", |_| sa("203.0.113.7:40001"));
        let b = fake_stun("127.0.0.2", |_| sa("203.0.113.7:40002"));
        let (p, _) = run([a, b]);
        assert_eq!(classify(Some(Ipv4Addr::new(192, 168, 1, 20)), &p.mapped(), &RouterFacts::default(), None), Nat::Strict);
        // no NAT: the servers see the game socket itself
        let a = fake_stun("127.0.0.1", |f| f);
        let b = fake_stun("127.0.0.2", |f| f);
        let (p, _) = run([a, b]);
        assert_eq!(p.mapped()[0].port(), p.mapped()[1].port());
        assert_eq!(classify(Some(Ipv4Addr::LOCALHOST), &p.mapped(), &RouterFacts::default(), None), Nat::OpenPublic);
    }

    #[test]
    fn a_separate_socket_seen_at_another_address_is_unclear() {
        // the fake router sends the game socket (127.0.0.1) out of one
        // connection and the separate socket (bound to 127.0.0.3) out of another
        fn wan(f: SocketAddr) -> SocketAddr {
            if f.ip() == IpAddr::V4(Ipv4Addr::new(127, 0, 0, 3)) { sa("198.51.100.9:61000") } else { sa("203.0.113.7:40000") }
        }
        let a = fake_stun("127.0.0.1", wan);
        let b = fake_stun("127.0.0.2", wan);
        let mut net = Net::new();
        net.open(NetSrc::Server, "127.0.0.1", 0).unwrap();
        let mut p = Probe::start(vec![a.to_string(), b.to_string()], |n| n.parse().ok());
        p.other_bind = "127.0.0.3:0".into();
        let t = Instant::now();
        while !p.poll(&mut net) {
            while net.get_packet(NetSrc::Server).is_some() {}
            std::thread::sleep(Duration::from_millis(5));
            assert!(t.elapsed() < Duration::from_secs(5));
        }
        assert!(t.elapsed() < GIVE_UP, "all three answered: no waiting");
        assert_eq!(p.other_mapped(), Some(sa("198.51.100.9:61000")));
        assert_eq!(net.packets_sent, 2, "the game socket sends two, the separate socket one");
        assert!(p.asked_line().ends_with(&format!(", and {a} once more from a separate socket.")), "{}", p.asked_line());
        let local = Some(Ipv4Addr::new(192, 168, 1, 20));
        let n = classify(local, &p.mapped(), &RouterFacts::default(), p.other_mapped());
        assert_eq!(n, Nat::Unclear { game: Ipv4Addr::new(203, 0, 113, 7), other: Ipv4Addr::new(198, 51, 100, 9) });
        // one connection: the same address both ways
        let a = fake_stun("127.0.0.1", |_| sa("203.0.113.7:40000"));
        let b = fake_stun("127.0.0.2", |_| sa("203.0.113.7:40000"));
        let (p, _) = run([a, b]);
        assert_eq!(p.other_mapped().map(|m| m.ip()), Some(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 7))));
        assert_eq!(classify(local, &p.mapped(), &RouterFacts::default(), p.other_mapped()), Nat::Moderate);
    }

    #[test]
    fn a_silent_server_is_retried_once_then_given_up() {
        let a = fake_stun("127.0.0.1", |_| sa("203.0.113.7:40000"));
        let silent = UdpSocket::bind("127.0.0.2:0").unwrap();
        let t = Instant::now();
        let (p, mut net) = run([a, silent.local_addr().unwrap()]);
        assert!(t.elapsed() >= GIVE_UP && t.elapsed() < GIVE_UP + Duration::from_millis(500), "{:?}", t.elapsed());
        assert_eq!(p.targets[1].sends, 2, "one retry");
        assert_eq!(p.targets[0].sends, 1, "the answered one is not asked again");
        assert_eq!(classify(None, &p.mapped(), &RouterFacts::default(), None), Nat::Unknown("only one STUN server answered"));
        assert!(p.asked_line().contains("no answer"));
        // the check is over: a late answer takes the Q3 path (and is ignored there)
        let mut b = [0u8; 64];
        silent.set_read_timeout(Some(Duration::from_millis(200))).unwrap();
        let (n, from) = silent.recv_from(&mut b).unwrap();
        let (_, id) = stun::header(&b[..n]).unwrap();
        silent.send_to(&stun::binding_success(&id, sa("203.0.113.7:1")), from).unwrap();
        std::thread::sleep(Duration::from_millis(50));
        assert!(net.get_packet(NetSrc::Server).is_some());
    }

    #[test]
    fn a_stalled_frame_does_not_use_up_the_wait() {
        // a map load freezes the main loop: the requests go out, then no
        // frame for 6 s; the answers wait in the socket until the next one
        let a = fake_stun("127.0.0.1", |_| sa("203.0.113.7:40000"));
        let b = fake_stun("127.0.0.2", |_| sa("203.0.113.7:40000"));
        let mut net = Net::new();
        net.open(NetSrc::Server, "127.0.0.1", 0).unwrap();
        let mut p = Probe::start(vec![a.to_string(), b.to_string()], |n| n.parse().ok());
        let mut stall = Duration::ZERO;
        let t0 = Instant::now();
        while net.packets_sent < 2 {
            assert!(!p.poll_at(&mut net, Instant::now() + stall));
            std::thread::sleep(Duration::from_millis(5));
            assert!(t0.elapsed() < Duration::from_secs(5));
        }
        std::thread::sleep(Duration::from_millis(50));
        stall += Duration::from_secs(6);
        // the first frame after the stall: the answers are not read yet (the
        // server frame reads the socket after this poll); not given up
        assert!(!p.poll_at(&mut net, Instant::now() + stall), "a stall is one short frame");
        while !p.poll_at(&mut net, Instant::now() + stall) {
            while net.get_packet(NetSrc::Server).is_some() {}
            std::thread::sleep(Duration::from_millis(5));
            assert!(t0.elapsed() < Duration::from_secs(5));
        }
        assert_eq!(p.mapped().len(), 2, "{}", p.asked_line());
        assert_eq!(p.targets[0].sends, 1, "no retry needed");
        // the clock itself
        let mut c = FrameClock::default();
        let t = Instant::now();
        assert_eq!(c.tick(t), 0.0);
        assert!((c.tick(t + Duration::from_millis(16)) - 0.016).abs() < 1e-9);
        assert!((c.tick(t + Duration::from_millis(6016)) - 0.266).abs() < 1e-9);
    }

    #[test]
    fn servers_on_one_address_count_once() {
        let a = fake_stun("127.0.0.1", |_| sa("203.0.113.7:40000"));
        let b = fake_stun("127.0.0.1", |_| sa("203.0.113.7:40000"));
        let (p, _) = run([a, b]);
        assert_eq!(p.mapped().len(), 1);
        assert!(p.asked_line().contains("(same address as another, not asked)"));
    }

    #[test]
    fn unresolvable_names() {
        let mut net = Net::new();
        net.open(NetSrc::Server, "127.0.0.1", 0).unwrap();
        let mut p = Probe::start(vec!["a.invalid".into(), "b.invalid".into()], |_| None);
        let t = Instant::now();
        while !p.poll(&mut net) {
            std::thread::sleep(Duration::from_millis(5));
            assert!(t.elapsed() < Duration::from_secs(5));
        }
        assert!(p.mapped().is_empty());
        assert_eq!(net.packets_sent, 0);
    }
}
