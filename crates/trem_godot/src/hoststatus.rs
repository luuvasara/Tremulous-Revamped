//! The host-time status of a hosted game (dec076): can players on the
//! internet and players on your network join, and if not, which steps are
//! left.
//!
//! The steps are listed in the order a player's packet passes them: the
//! router (port opening by PCP, NAT-PMP or UPnP; needed only by players on
//! the internet), then this PC's Windows Firewall (needed by everyone).
//! Each row has a state tag, [OK] [NO] [??] (plain ASCII, the same text in
//! the dialog and the console, in colour in the console: every font, code
//! page and screen reader shows it), the state in plain words and the official term
//! in brackets. Then the join addresses (every port with UDP), the network
//! type (natcheck.rs, the router side only) and a "What does this mean?"
//! link to the README's "Hosting and your network" section. The dialog and
//! the console print the same facts in the same order. The Windows network
//! profile names never reach the player.

use crate::firewall::{Prompt, Verdict, ANSWER_ALLOW, ANSWER_HOST, ANSWER_LAN};
use crate::natcheck::Nat;
use crate::pcp::{Outcome, PortResult, Proto};
use crate::upnp::Mapping;
use std::net::{Ipv4Addr, Ipv6Addr};

/// The help page for forwarding a port.
pub const HELP_URL: &str = "https://portforward.com";
/// The README section the "What does this mean?" link opens (it explains
/// the network types and the steps).
pub const GUIDE_URL: &str = "https://github.com/MeepingsCorner/Tremulous-Revamped#hosting-and-your-network";
/// The only addresses the dialog's links open.
pub const LINKS: [&str; 2] = [GUIDE_URL, HELP_URL];
/// How long the dialog waits for the router's answer (and the network
/// check) before it opens: running time (natcheck::FrameClock), so a map
/// load that freezes the frames does not use it up. Answers that come
/// later are printed as "Update: ..." lines (`updates`).
pub const UPNP_WAIT_SECS: f64 = 4.0;
/// How long the status waits for the firewall check: then it opens anyway,
/// with the firewall row "still checking", and the check's answer follows
/// (Windows' rule listing can take many seconds, or never finish).
pub const FW_WAIT_SECS: f64 = 8.0;

/// Step 1, as the player sees it.
#[derive(Clone, Debug, PartialEq)]
pub enum Fw {
    /// not Windows: there is no step 1
    NotApplicable,
    Allowed,
    /// a rule lets the home network (local subnet) in, nobody else
    LanOnly,
    /// no rule lets the game in
    NoRule,
    /// a rule blocks the game
    BlockRule,
    /// the check itself failed
    Unchecked,
    /// the check has not answered yet (`FW_WAIT_SECS` passed): the answer
    /// follows as an update
    Checking,
}

/// Step 2, as the player sees it.
#[derive(Clone, Debug, PartialEq)]
pub enum Router {
    /// net_upnp 0: the player turned it off
    Off,
    /// none of UPnP, PCP, NAT-PMP answered
    NoAnswer,
    /// the router said no, with its UPnP error code
    Refused(String),
    /// PCP / NAT-PMP said no: the protocol and its result code
    GatewayRefused(Proto, u16),
    Failed(String),
    /// PCP ADDRESS_MISMATCH: another router or the provider's CGNAT sits in front
    Behind,
    Forwarded {
        /// "PCP", "NAT-PMP" or "UPnP"
        via: String,
        ext: Option<String>,
        ext_ok: bool,
    },
    /// the player chose "Host for my home network only": the port was closed again
    Released,
}

/// Friends with IPv6: the server's global IPv6 address and port, and whether
/// the router opened its IPv6 firewall (PCP).
#[derive(Clone, Debug, PartialEq)]
pub struct V6 {
    pub addr: Ipv6Addr,
    pub port: u16,
    pub pinhole: Pinhole,
}

/// What the router said to opening its IPv6 firewall.
#[derive(Clone, Debug, PartialEq)]
pub enum Pinhole {
    /// opened, by this protocol
    Opened(&'static str),
    /// it answered no: the protocol and its result code
    Refused(Proto, u16),
    /// it did not answer
    NoAnswer,
    /// not asked (no IPv6 router, net_upnp 0, or no answer yet)
    NotAsked,
}

impl Fw {
    pub fn from_verdict(v: &Verdict) -> Fw {
        match v {
            Verdict::Allowed(_) => Fw::Allowed,
            Verdict::NoRule(_) => Fw::NoRule,
            Verdict::LanOnly(_) => Fw::LanOnly,
            Verdict::Blocked(_) => Fw::BlockRule,
            Verdict::Unknown(_) => Fw::Unchecked,
        }
    }
    /// A few words for hosting_status while the status is being collected.
    pub fn word(&self) -> &'static str {
        match self {
            Fw::NotApplicable => "not applicable",
            Fw::Allowed => "open",
            Fw::LanOnly => "open for your home network only",
            Fw::NoRule => "closed (no rule)",
            Fw::BlockRule => "closed (a rule blocks the game)",
            Fw::Unchecked => "not checked",
            Fw::Checking => "still checking",
        }
    }
    /// Players on your network are kept out too.
    pub fn lacks(&self) -> bool {
        matches!(self, Fw::NoRule | Fw::BlockRule)
    }
    /// Players on the internet are kept out.
    fn lacks_internet(&self) -> bool {
        self.lacks() || *self == Fw::LanOnly
    }
}

impl Router {
    /// From what the three protocols gave (`pcp::open_udp`): a protocol that
    /// opened the port, else what went wrong (UPnP's own error first).
    pub fn from_port(r: &PortResult) -> Router {
        if let Some(o) = &r.opened {
            let ext = o.external_ip();
            let ext_ok = ext.as_deref().is_none_or(|ip| !crate::upnp::is_private(ip));
            return Router::Forwarded { via: o.via().into(), ext, ext_ok };
        }
        if r.pcp == Outcome::Mismatch {
            return Router::Behind;
        }
        let upnp = r.upnp.as_ref().map(Router::from_result).unwrap_or(Router::NoAnswer);
        match (&r.pcp, upnp) {
            (_, u @ (Router::Refused(_) | Router::Failed(_))) => u,
            (Outcome::Refused(p, c), _) => Router::GatewayRefused(*p, *c),
            _ => Router::NoAnswer,
        }
    }

    /// From what UPnP alone returned (`upnp::map_at`).
    pub fn from_result(r: &Result<Mapping, String>) -> Router {
        match r {
            Ok(m) => {
                let ext = m.external_ip.clone();
                let ext_ok = ext.as_deref().is_none_or(|ip| !crate::upnp::is_private(ip));
                Router::Forwarded { via: "UPnP".into(), ext, ext_ok }
            }
            Err(e) if e.contains("no UPnP router answered") => Router::NoAnswer,
            Err(e) => match e.split_once("UPnP error ") {
                Some((_, rest)) => {
                    let code: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
                    if code.is_empty() {
                        Router::Failed(e.clone())
                    } else {
                        Router::Refused(code)
                    }
                }
                None => Router::Failed(e.clone()),
            },
        }
    }
    fn ok(&self) -> bool {
        matches!(self, Router::Forwarded { ext_ok: true, .. })
    }
}

/// Everything the status needs.
#[derive(Clone, Debug, PartialEq)]
pub struct Status {
    pub map: String,
    pub port: u16,
    pub fw: Fw,
    pub router: Router,
    /// this PC's addresses, the one facing the default route flagged as main
    pub addrs: Vec<(Ipv4Addr, bool)>,
    /// the network check (None: off, net_natCheck 0)
    pub nat: Option<Nat>,
    /// the IPv6 join address, when the server has a global one
    pub v6: Option<V6>,
    /// which STUN servers were asked (console only)
    pub asked: Option<String>,
}

/// The router facts the network check needs.
pub fn router_facts(r: &Router) -> crate::natcheck::RouterFacts {
    match r {
        Router::Forwarded { ext, ext_ok, .. } => crate::natcheck::RouterFacts { opened: true, external: ext.as_deref().and_then(|e| e.parse().ok()), behind: !ext_ok },
        Router::Behind => crate::natcheck::RouterFacts { opened: false, external: None, behind: true },
        _ => crate::natcheck::RouterFacts::default(),
    }
}

/// The IPv6 line's facts from the server's address and the PCP attempt.
pub fn v6_join(addr: Option<Ipv6Addr>, port: u16, r: Option<&PortResult>) -> Option<V6> {
    let pinhole = match r.and_then(|r| r.v6.as_ref()) {
        None => Pinhole::NotAsked,
        Some(Outcome::Mapped(l)) => Pinhole::Opened(l.proto.name()),
        Some(Outcome::Refused(p, c)) => Pinhole::Refused(*p, *c),
        Some(Outcome::Mismatch) => Pinhole::Refused(Proto::Pcp, crate::pcp::ADDRESS_MISMATCH),
        Some(Outcome::NoAnswer) => Pinhole::NoAnswer,
    };
    addr.map(|addr| V6 { addr, port, pinhole })
}

/// "This PC's firewall: ..." on its own, plain (a late answer).
impl Status {
    pub fn fw_line(&self) -> Option<String> {
        self.fw_step().map(|s| format!("This PC's firewall: {}{}", s.state, s.rest))
    }
}

/// What a late answer changed in a printed status: the changed lines, as
/// "Update: ..." (players on the internet, the router, the network type, IPv6).
pub fn updates(old: &Status, new: &Status) -> Vec<String> {
    let pairs = [
        (Some(old.internet_row()), Some(new.internet_row())),
        (Some(old.lan_row()), Some(new.lan_row())),
        (Some(old.router_line()), Some(new.router_line())),
        (old.nat.as_ref().map(|n| n.line(old.port)), new.nat.as_ref().map(|n| n.line(new.port))),
        (old.v6_line(), new.v6_line()),
        (old.fw_line(), new.fw_line()),
    ];
    pairs
        .into_iter()
        .filter_map(|(o, n)| n.filter(|n| o.as_ref() != Some(n)))
        .map(|l| format!("Update: {l}"))
        .collect()
}


/// The addresses friends on the LAN join at: the main one first.
pub fn order_addrs(all: &[Ipv4Addr], main: Option<Ipv4Addr>) -> Vec<(Ipv4Addr, bool)> {
    let mut out: Vec<(Ipv4Addr, bool)> = Vec::new();
    for a in all {
        if a.is_loopback() || a.is_link_local() || out.iter().any(|o| o.0 == *a) {
            continue;
        }
        out.push((*a, Some(*a) == main));
    }
    // no default route known: a lone address is the main one
    if main.is_none() && out.len() == 1 {
        out[0].1 = true;
    }
    out.sort_by_key(|o| !o.1);
    out
}

/// Whether the report may be shown: the firewall answered, and the router
/// answered (or UPnP is off) and the network check is over (or off), or the
/// wait is over.
/// `waited_secs` counts running frames (a stalled frame counts little);
/// `wall_secs` is the clock on the wall: after `FW_WAIT_SECS` of it the report
/// is ready whatever has not answered (the firewall check runs on its own
/// thread, so a frozen map load does not hold it back; the row reads "still
/// checking").
pub fn ready(fw_done: bool, router_done: bool, upnp_on: bool, nat_done: bool, waited_secs: f64, wall_secs: f64) -> bool {
    (fw_done && (((router_done || !upnp_on) && nat_done) || waited_secs >= UPNP_WAIT_SECS)) || wall_secs >= FW_WAIT_SECS
}

/// A step's state: its tag (dialog, console, in-game page) and console colour.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Mark {
    Ok,
    No,
    /// not opened as far as the game can see (a port forwarded by hand
    /// cannot be seen), or not checked
    Maybe,
}

impl Mark {
    /// The one source of the state text, everywhere it is shown: plain
    /// ASCII (check and cross glyphs draw as empty boxes in fonts without
    /// them, e.g. Wine's Tahoma and the console font).
    pub fn tag(self) -> &'static str {
        match self {
            Mark::Ok => "[OK]",
            Mark::No => "[NO]",
            Mark::Maybe => "[??]",
        }
    }
    /// Q3 colour code: green, red, yellow.
    pub fn colour(self) -> &'static str {
        match self {
            Mark::Ok => "^2",
            Mark::No => "^1",
            Mark::Maybe => "^3",
        }
    }
}

/// One step row: the state, its word (coloured in the console), the rest of
/// the sentence, and the fix (where the player can do something).
#[derive(Clone, Debug, PartialEq)]
pub struct Step {
    pub mark: Mark,
    pub name: &'static str,
    pub state: String,
    pub rest: String,
    pub fix: Option<String>,
    /// the fix ends with the port-forwarding help link
    pub link: bool,
    /// the fix's last sentence (after the link)
    pub after: Option<String>,
}

/// Where the text goes: the dialog (links) or the console (colours).
#[derive(Clone, Copy, PartialEq)]
enum To {
    Dialog,
    Console,
}

/// The console's columns: tag at 2, name at 7, value at 28.
const COL: usize = 28;
/// The console's line width: Console.linewidth is 78, and a word that
/// ends on its last column moves to the next line, so 77.
const WIDTH: usize = 77;

/// `colour` from the start of `s` up to and including its first space
/// after `from`, then white: the console counts a colour code inside a word
/// as part of the word's length, so the codes sit at word starts.
fn paint(colour: &str, s: &str, from: usize) -> String {
    match s[from..].find(' ') {
        Some(i) => format!("{colour}{} ^7{}", &s[..from + i], &s[from + i + 1..]),
        None => format!("{colour}{s}^7"),
    }
}

/// Visible length (Q3 colour codes take no room).
fn vis_len(s: &str) -> usize {
    let mut n = 0;
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c == '^' && it.peek().is_some_and(|n| *n != '^') {
            it.next();
            continue;
        }
        n += 1;
    }
    n
}

/// Word wrap with a hanging indent of `indent` spaces.
fn wrap(line: &str, indent: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let lead = line.len() - line.trim_start().len();
    cur.push_str(&line[..lead]);
    let mut first = true;
    for w in line[lead..].split(' ') {
        let fits = vis_len(&cur) + 1 + vis_len(w) <= WIDTH;
        if !first && !fits {
            out.push(cur.trim_end().to_string());
            cur = " ".repeat(indent);
        } else if !first {
            cur.push(' ');
        }
        cur.push_str(w);
        first = false;
    }
    out.push(cur);
    out
}

fn steps_text(n: usize) -> String {
    if n == 1 {
        "1 step left".into()
    } else {
        format!("{n} steps left")
    }
}

impl Status {
    fn main_addr(&self) -> Option<String> {
        self.addrs
            .iter()
            .find(|a| a.1)
            .or(self.addrs.first())
            .map(|a| a.0.to_string())
    }

    /// Players on the internet can join as far as the router goes: it
    /// opened the port, or this PC needs none (a public address).
    fn inet_ok(&self) -> bool {
        self.router.ok() || self.nat == Some(Nat::OpenPublic)
    }

    fn to_this_pc(&self) -> String {
        match self.main_addr() {
            Some(a) => format!("this PC, {a}"),
            None => "this PC".into(),
        }
    }

    /// The router row (first on the path a player's packet takes).
    pub fn router_step(&self) -> Step {
        let p = self.port;
        let fwd = format!("forward UDP port {p} to {} (port forwarding)", self.to_this_pc());
        let cant_see = "If you already forwarded it, players can join; the game can't check this.".to_string();
        let behind_fix = format!("Fix: forward UDP port {p} on the outer router too, or ask your provider for a public IPv4 address.");
        let step = |mark, state: &str, rest: String, fix: Option<String>, link: bool, after: Option<String>| Step {
            mark,
            name: "Router",
            state: state.into(),
            rest,
            fix,
            link,
            after,
        };
        if self.nat == Some(Nat::OpenPublic) && !self.router.ok() {
            return step(Mark::Ok, "not needed", ": this PC has its own public address.".into(), None, false, None);
        }
        match &self.router {
            Router::Forwarded { via, ext_ok: true, .. } => step(
                Mark::Ok,
                "opened",
                format!(" UDP port {p} automatically for this game ({via}); it closes when you stop hosting."),
                None,
                false,
                None,
            ),
            Router::Forwarded { via, ext, ext_ok: false } => step(
                Mark::No,
                "opened",
                format!(
                    " UDP port {p} ({via}), but another router or your provider's shared address (CGNAT) is in front: {} can't be reached from the internet.",
                    ext.as_deref().unwrap_or("its address")
                ),
                Some(behind_fix),
                false,
                None,
            ),
            Router::Behind => step(
                Mark::No,
                "not opened",
                ": another router or your provider's shared address (CGNAT) is in front (PCP saw another address than this PC's).".into(),
                Some(behind_fix),
                false,
                None,
            ),
            Router::GatewayRefused(proto, c) => step(
                Mark::No,
                "refused",
                format!(" to open UDP port {p} ({}).", proto.refusal(*c)),
                Some(format!("Fix: {fwd}.")),
                true,
                Some(cant_see),
            ),
            Router::Refused(c) if c == "718" => step(
                Mark::No,
                "refused",
                format!(" to open UDP port {p} (UPnP error 718: that port is already forwarded to another device)."),
                Some(format!("Fix: {fwd}.")),
                true,
                Some(cant_see),
            ),
            Router::Refused(c) => step(
                Mark::No,
                "refused",
                format!(" to open UDP port {p} (UPnP error {c})."),
                Some(format!("Fix: {fwd}.")),
                true,
                Some(cant_see),
            ),
            Router::Failed(e) => step(
                Mark::No,
                "failed",
                format!(" to open UDP port {p} automatically (UPnP: {e})."),
                Some(format!("Fix: {fwd}.")),
                true,
                Some(cant_see),
            ),
            Router::NoAnswer => step(
                Mark::Maybe,
                "no answer",
                format!(": UDP port {p} was not opened automatically (no UPnP, PCP or NAT-PMP answer)."),
                Some(format!("Fix: turn on automatic port opening (UPnP) in the router, or {fwd}.")),
                true,
                Some(cant_see),
            ),
            Router::Off => step(
                Mark::Maybe,
                "not asked",
                format!(" (net_upnp 0): UDP port {p} is not opened automatically."),
                Some(format!("Fix: {fwd}, or set net_upnp 1.")),
                false,
                Some(cant_see),
            ),
            Router::Released => step(
                Mark::Maybe,
                "closed again",
                ": you host for your home network only (LAN).".into(),
                None,
                false,
                None,
            ),
        }
    }

    /// This PC's firewall row (second on the path); None when not Windows.
    pub fn fw_step(&self) -> Option<Step> {
        let (mark, state, rest, fix) = match self.fw {
            Fw::NotApplicable => return None,
            Fw::Allowed => (Mark::Ok, "open", " for the game (Windows Firewall rule).", None),
            Fw::LanOnly => (
                Mark::Maybe,
                "open for your home network only",
                " (Windows Firewall rule for the local subnet).",
                Some("Type firewall_allow to open it for players on the internet too (Windows asks for permission)."),
            ),
            Fw::NoRule => (
                Mark::No,
                "closed",
                " to the game (no Windows Firewall rule yet).",
                Some("Type firewall_allow to open it (Windows asks for permission)."),
            ),
            Fw::BlockRule => (
                Mark::No,
                "closed",
                " to the game (a Windows Firewall rule blocks it).",
                Some("Type firewall_allow to open it (Windows asks for permission)."),
            ),
            Fw::Unchecked => (Mark::Maybe, "not checked", ": Windows did not answer the check.", None),
            Fw::Checking => (Mark::Maybe, "still checking", ": Windows has not answered yet; this updates when it does.", None),
        };
        Some(Step {
            mark,
            name: "This PC's firewall",
            state: state.into(),
            rest: rest.into(),
            fix: fix.map(String::from),
            link: false,
            after: None,
        })
    }

    /// The steps left for players on the internet, in path order.
    fn internet_left(&self) -> Vec<Step> {
        let mut v = Vec::new();
        if !self.inet_ok() && self.router != Router::Released {
            v.push(self.router_step());
        }
        if self.fw.lacks_internet() {
            v.extend(self.fw_step());
        }
        v
    }

    fn group(&self, left: &[Step], to: To) -> String {
        if left.is_empty() {
            return match to {
                To::Dialog => "can join".into(),
                To::Console => "^2can join^7".into(),
            };
        }
        let names: Vec<String> = left
            .iter()
            .map(|s| match s.name {
                "Router" => "your router".to_string(),
                _ => "this PC's firewall".to_string(),
            })
            .collect();
        let mark = if left.iter().any(|s| s.mark == Mark::No) { Mark::No } else { Mark::Maybe };
        match to {
            To::Dialog => format!("{}: {}", steps_text(left.len()), names.join(", ")),
            To::Console => paint(mark.colour(), &format!("{}: {}", steps_text(left.len()), names.join(", ")), steps_text(left.len()).len()),
        }
    }

    fn internet_group(&self, to: To) -> String {
        if self.router == Router::Released {
            return "not this time: you host for your home network only (LAN)".into();
        }
        if self.fw == Fw::Unchecked && self.internet_left().is_empty() {
            return "not sure: this PC's firewall was not checked".into();
        }
        if self.fw == Fw::Checking && self.internet_left().is_empty() {
            return "not sure yet: this PC's firewall is still being checked".into();
        }
        self.group(&self.internet_left(), to)
    }

    fn lan_group(&self, to: To) -> String {
        match self.fw {
            Fw::Unchecked => "not sure: this PC's firewall was not checked".into(),
            Fw::Checking => "not sure yet: this PC's firewall is still being checked".into(),
            _ if self.fw.lacks() => self.group(&self.fw_step().into_iter().collect::<Vec<_>>(), to),
            _ => self.group(&[], to),
        }
    }

    /// The verdict on top.
    pub fn headline(&self) -> String {
        let n = self.internet_left().len();
        if self.fw.lacks() {
            format!("Players can't join yet: {}", steps_text(n))
        } else if self.router == Router::Released {
            "Players on your network can join".into()
        } else if n > 0 {
            format!("Internet players can't join yet: {}", steps_text(n))
        } else if self.fw == Fw::Checking {
            format!("Hosting {}: still checking this PC's firewall", self.map)
        } else if self.fw == Fw::Unchecked {
            format!("Hosting {}", self.map)
        } else {
            "Players can join".into()
        }
    }

    /// "Players on the internet: ..." (also an update line).
    pub fn internet_row(&self) -> String {
        format!("Players on the internet: {}", self.internet_group(To::Dialog))
    }

    pub fn lan_row(&self) -> String {
        format!("Players on your network: {}", self.lan_group(To::Dialog))
    }

    /// "Router: ..." on its own, plain (a late answer).
    pub fn router_line(&self) -> String {
        let s = self.router_step();
        format!("Router: {}{}", s.state, s.rest)
    }

    /// The public IPv4 join address, when it is known.
    fn public_v4(&self) -> Option<String> {
        match &self.router {
            Router::Forwarded { ext: Some(ip), ext_ok: true, .. } => Some(format!("{ip}:{}", self.port)),
            _ if self.nat == Some(Nat::OpenPublic) => self.main_addr().map(|a| format!("{a}:{}", self.port)),
            _ => None,
        }
    }

    /// The IPv6 join address and what the router said to it.
    fn v6_text(&self) -> Option<String> {
        let v = self.v6.as_ref()?;
        let at = v6_join_address(&v.addr, v.port);
        Some(match &v.pinhole {
            Pinhole::Opened(p) => format!("{at} (the router opened its IPv6 firewall, {p})"),
            Pinhole::Refused(p, c) => format!("{at}, if the router's IPv6 firewall lets players in (it refused to open it: {})", p.refusal(*c)),
            Pinhole::NoAnswer => format!("{at}, if the router's IPv6 firewall lets players in (no PCP answer)"),
            Pinhole::NotAsked => format!("{at}, if the router's IPv6 firewall lets players in"),
        })
    }

    /// "IPv6 (UDP): ..." (also an update line).
    pub fn v6_line(&self) -> Option<String> {
        self.v6_text().map(|t| format!("IPv6 (UDP): {t}"))
    }

    fn lan_addrs(&self) -> Option<String> {
        if self.addrs.is_empty() {
            return None;
        }
        let several = self.addrs.len() > 1;
        Some(
            self.addrs
                .iter()
                .map(|(a, m)| {
                    let tag = match (several, m) {
                        (false, _) => "",
                        (true, true) => " (main)",
                        (true, false) => " (other adapter)",
                    };
                    format!("{a}:{}{tag}", self.port)
                })
                .collect::<Vec<_>>()
                .join(", "),
        )
    }

    fn step_sentence(s: &Step, to: To, link: &str) -> String {
        let mut t = match to {
            To::Dialog => format!("{}{}", s.state, s.rest),
            To::Console => paint(s.mark.colour(), &format!("{}{}", s.state, s.rest), s.state.len()),
        };
        if s.mark == Mark::Ok && s.name == "Router" {
            // "...for this game (PCP); it closes..." reads as one sentence
            t = t.replace("; it closes", " - it closes");
        }
        if let Some(f) = &s.fix {
            t.push(' ');
            if s.link {
                // "... (port forwarding)." + link
                let f = f.trim_end_matches('.');
                match to {
                    To::Dialog => t.push_str(&format!("{f}. {link}.")),
                    To::Console => t.push_str(&format!("{f}: {link}.")),
                }
            } else {
                t.push_str(f);
            }
        }
        if let Some(a) = &s.after {
            t.push(' ');
            t.push_str(match to {
                To::Console => "If you already did, players can join.",
                To::Dialog => a,
            });
        }
        t
    }

    /// The console block (also the engine log): the same facts as the
    /// dialog, in columns, Q3 colour codes for the states.
    pub fn console(&self) -> Vec<String> {
        let mut l = vec![format!("Hosting {} on UDP {}. {}", self.map, self.port, self.headline())];
        l.push(format!("  Players on the internet:  {}", self.internet_group(To::Console)));
        l.push(format!("  Players on your network:  {}", self.lan_group(To::Console)));
        let rows = [Some(self.router_step()), self.fw_step()];
        for s in rows.into_iter().flatten() {
            let name = format!("{:<21}", s.name);
            l.extend(wrap(&format!("  {}{} ^7{name}{}", s.mark.colour(), s.mark.tag(), Status::step_sentence(&s, To::Console, HELP_URL)), COL));
        }
        let mut row = |k: &str, v: String| l.extend(wrap(&format!("       {k:<21}{v}"), COL));
        if let Some(a) = self.public_v4() {
            row("Internet (UDP)", format!("{a} (public IPv4)"));
        }
        if let Some(t) = self.v6_text() {
            row("IPv6 (UDP)", t);
        }
        if let Some(a) = self.lan_addrs() {
            row("LAN (UDP)", a);
        }
        if let Some(n) = &self.nat {
            row("Network type", n.label(self.port));
            if let Some(d) = n.detail(self.port) {
                row("", d);
            }
        }
        l.push("What does this mean? README, section \"Hosting and your network\".".into());
        if let Some(a) = &self.asked {
            l.extend(wrap(a, 2));
        }
        l
    }

    /// The window's text: the two groups, the steps, the addresses and the
    /// network type.
    fn dialog_content(&self) -> String {
        let guide = format!("<a href=\"{GUIDE_URL}\">What does this mean?</a>");
        let help = format!("<a href=\"{HELP_URL}\">How to forward a port</a>");
        let mut c = format!("{}\n{}\n\n", self.internet_row(), self.lan_row());
        // the firewall's fix is the dialog's own buttons (the console says firewall_allow)
        let fw = self.fw_step().map(|s| Step { fix: None, ..s });
        for s in [Some(self.router_step()), fw].into_iter().flatten() {
            c.push_str(&format!("{} {}: {}\n", s.mark.tag(), s.name, Status::step_sentence(&s, To::Dialog, &help)));
        }
        let mut info = Vec::new();
        match (self.public_v4(), self.v6.as_ref()) {
            (Some(a), Some(v)) => info.push(format!("Internet address (UDP): {a} (IPv6: {})", v6_join_address(&v.addr, v.port))),
            (Some(a), None) => info.push(format!("Internet address (UDP): {a}")),
            (None, Some(_)) => info.push(format!("Internet address (UDP): IPv6 {}", self.v6_text().unwrap_or_default())),
            (None, None) => {}
        }
        if let Some(n) = &self.nat {
            info.push(format!("Network type: {} {guide}", n.label(self.port)));
        }
        if !info.is_empty() {
            c.push('\n');
            for i in info {
                c.push_str(&format!("    {i}\n"));
            }
        }
        c.trim_end().to_string()
    }

    /// The window on every host (dec076, Windows): the question when the
    /// firewall keeps players on your network out (`prompt`), else the
    /// status with "Keep hosting" (and "Allow players on the internet too"
    /// for a home-network-only rule). `never`: net_hostWindow is 0
    /// (`hosting_status` opened it anyway): the checkbox starts ticked.
    pub fn status_prompt(&self, exe: &str, never: bool) -> Prompt {
        if let Some(p) = self.prompt(exe) {
            return Prompt { checked: never, ..p };
        }
        let mut buttons = Vec::new();
        if self.fw == Fw::LanOnly {
            buttons.push((ANSWER_ALLOW, "Allow players on the internet too\nReplaces the home-network-only rule with one for everyone (one Windows Firewall rule). Windows asks for permission.".to_string()));
        }
        buttons.push((ANSWER_HOST, "Keep hosting\nCloses this window; nothing is changed.".to_string()));
        Prompt {
            title: format!("Tremulous: hosting {}", self.map),
            main: self.headline(),
            content: self.dialog_content(),
            footer: "Type hosting_status in the console to check again at any time.".into(),
            details: "This window shows each time you host: who can join, and why. hosting_status (console) checks the router, the network (STUN) and this PC's firewall again and shows it at any time.\n\n\
                      \"Don't ask again when I host\" is net_hostWindow 0. To have the window back, untick it here (open it with hosting_status), or type net_hostWindow 1."
                .into(),
            verify: "Don't ask again when I host".into(),
            buttons,
            checked: never,
            warn: !self.internet_left().is_empty() || self.fw.lacks_internet(),
        }
    }

    /// The question: only when the player can fix something on this PC (the
    /// firewall keeps players on your network out too).
    pub fn prompt(&self, exe: &str) -> Option<Prompt> {
        if !self.fw.lacks() {
            return None;
        }
        let content = self.dialog_content();
        let block = self.fw == Fw::BlockRule;
        let allow_note = if block {
            "Removes the rule that blocks the game and opens this PC's firewall for it (one Windows Firewall rule). Windows asks for permission."
        } else {
            "Opens this PC's firewall for the game (one Windows Firewall rule). Windows asks for permission."
        };
        let lan_note = if self.router.ok() {
            "Opens this PC's firewall for your home network only (local subnet); the router port is closed again."
        } else {
            "Opens this PC's firewall for your home network only (local subnet); the router port stays closed."
        };
        let details = format!(
            "Allow adds one Windows Firewall rule, \"{}\": incoming UDP for {exe}, on the networks this PC is on now. The rule belongs to this exact file path: a copy of the game in another folder asks again, and the game's old rules for copies that no longer exist are removed in the same step. \
             Home network only adds the rule \"{}\" instead, for addresses of your local network only (remote address: local subnet), and the game closes the router port again.\n\n\
             Risk: small. While hosting, anyone who can reach your server can send it data, so a bug in the game could be abused. \
             Windows asks for administrator permission (UAC, it says \"Windows PowerShell\"), then PowerShell changes the rules in the background, in one step.\n\n\
             \"Don't ask again when I host\" is net_hostWindow 0; type firewall_allow in the console to open the firewall later, and hosting_status to see this window again (untick the box there to have it back when you host).",
            crate::firewall::RULE_NAME,
            crate::firewall::RULE_NAME_LAN
        );
        Some(Prompt {
            title: format!("Tremulous: hosting {}", self.map),
            main: self.headline(),
            content,
            footer: "Allow adds one Windows Firewall rule for this game only, used only while you host. Windows asks for permission first.".into(),
            details,
            verify: "Don't ask again when I host".into(),
            buttons: vec![
                (ANSWER_ALLOW, format!("Allow players to join\n{allow_note}")),
                (ANSWER_LAN, format!("Host for my home network only (LAN)\n{lan_note}")),
                (ANSWER_HOST, "Host anyway\nNobody else can join until the firewall is open.".into()),
            ],
            checked: false,
            warn: true,
        })
    }
}

/// hosting_status while a check runs: what is known and what is not.
/// `fw`: the answer, if it came; `fw_secs`: how long the check has run.
pub fn known_lines(fw: Option<String>, fw_secs: Option<f64>, router: Option<String>, network: Option<String>) -> Vec<String> {
    let fw = match (fw, fw_secs) {
        (Some(w), _) => w,
        (None, Some(s)) => format!("still checking ({s:.0} s so far)"),
        (None, None) => "still checking".into(),
    };
    vec![
        format!("This PC's firewall: {fw}"),
        format!("Router: {}", router.unwrap_or_else(|| "still checking".into())),
        format!("Network type: {}", network.unwrap_or_else(|| "still checking".into())),
    ]
}

/// The one console line for each way the window is skipped or fails
/// (also in the engine log): the player never has to guess why there is no
/// window.
pub mod say {
    /// net_hostWindow 0 and nobody asked for the window.
    pub fn window_off() -> String {
        "The hosting window is off (net_hostWindow 0). Type hosting_status to open it.".into()
    }
    /// Windows' own dialog could not be shown; Godot's plain one follows.
    pub fn taskdialog_failed(reason: &str) -> String {
        format!("The hosting window could not use Windows' own dialog ({reason}); showing the plain dialog instead.")
    }
    /// Neither dialog could be shown.
    pub fn godot_dialog_failed(err: &str) -> String {
        format!("The plain hosting dialog could not be shown either ({err}); hosting goes on without it. Type hosting_status to try again; the facts are in the status above.")
    }
    /// The firewall check has not answered when the status opens.
    pub fn still_checking(secs: f64) -> String {
        format!("The firewall check has not answered after {secs:.0} s (Windows can be slow); showing what is known. The window and the status update when it does.")
    }
    /// The check ended without a verdict (PowerShell missing, stopped after its limit, no network).
    pub fn check_failed(reason: &str) -> String {
        format!("This PC's firewall could not be checked: {reason}.")
    }
}

/// What `hosting_status` writes to user://hosting_status.txt: the settings
/// and states that decide whether the window opens, a few short lines (a
/// tester can read the file when the console font is too small).
#[derive(Clone, Debug, Default)]
pub struct DebugFacts {
    /// the net_hostWindow value ("1" when not set)
    pub host_window: String,
    /// the old net_firewallPrompt value, if the config still has one (ignored)
    pub old_prompt: Option<String>,
    pub windows: bool,
    pub hosting: bool,
    /// "answered: ..." / "running for N s" / "not started"
    pub firewall: String,
    pub router: String,
    pub network: String,
    /// a status is being collected (waits at most `FW_WAIT_SECS`)
    pub collecting: bool,
    /// "none open" / "Windows dialog open" / "plain dialog open"
    pub dialog: String,
    /// what happened to the dialog and the check, oldest first
    pub events: Vec<String>,
    /// the last status (console lines, colour codes removed)
    pub status: Vec<String>,
}

/// The file text: facts first, one per line, then what happened, then the status.
pub fn debug_text(f: &DebugFacts) -> String {
    let mut o = vec![
        "Tremulous hosting status (written by hosting_status)".to_string(),
        format!("net_hostWindow = {} (1 = the window opens when you host, 0 = off)", f.host_window),
    ];
    match &f.old_prompt {
        Some(v) => o.push(format!("net_firewallPrompt = {v} (old setting, ignored)")),
        None => o.push("net_firewallPrompt: not set (old setting, ignored anyway)".into()),
    }
    o.push(format!("Windows: {}   hosting a network game: {}", if f.windows { "yes" } else { "no" }, if f.hosting { "yes" } else { "no" }));
    o.push(format!("This PC's firewall check: {}", f.firewall));
    o.push(format!("Router: {}", f.router));
    o.push(format!("Network check: {}", f.network));
    o.push(format!("Status being collected: {}   Hosting window: {}", if f.collecting { "yes" } else { "no" }, f.dialog));
    o.push(String::new());
    o.push("What happened (oldest first):".into());
    if f.events.is_empty() {
        o.push("  nothing yet".into());
    }
    o.extend(f.events.iter().map(|e| format!("  {e}")));
    o.push(String::new());
    o.push("Last status:".into());
    if f.status.is_empty() {
        o.push("  none yet (host a network game)".into());
    }
    o.extend(f.status.iter().map(|l| format!("  {l}")));
    o.join("\n") + "\n"
}

/// `[addr]:port`, the form `connect` takes.
pub fn v6_join_address(a: &Ipv6Addr, port: u16) -> String {
    format!("[{a}]:{port}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::firewall::ANSWER_NEVER;
    use crate::pcp::Opened;

    const EXE: &str = r"C:\Games\Tremulous-Revamped\Tremulous.exe";

    fn st(fw: Fw, router: Router) -> Status {
        Status {
            map: "atcs".into(),
            port: 30720,
            fw,
            router,
            addrs: order_addrs(
                &[
                    "10.20.30.4".parse().unwrap(),
                    "10.0.0.5".parse().unwrap(),
                    "127.0.0.1".parse().unwrap(),
                ],
                Some("10.0.0.5".parse().unwrap()),
            ),
            nat: None,
            v6: None,
            asked: None,
        }
    }
    fn fwd() -> Router {
        Router::Forwarded {
            via: "UPnP".into(),
            ext: Some("203.0.113.7".into()),
            ext_ok: true,
        }
    }
    fn pcp_fwd() -> Router {
        Router::Forwarded { via: "PCP".into(), ext: Some("203.0.113.7".into()), ext_ok: true }
    }
    fn v6(pinhole: Pinhole) -> Option<V6> {
        Some(V6 { addr: "2001:db8:77::5".parse().unwrap(), port: 30720, pinhole })
    }
    /// The mockup's s1: the router opened the port (PCP), the firewall has no rule.
    fn s1() -> Status {
        let mut s = st(Fw::NoRule, pcp_fwd());
        s.addrs = order_addrs(&["192.0.2.10".parse().unwrap()], Some("192.0.2.10".parse().unwrap()));
        s.nat = Some(Nat::OpenMapped);
        s.v6 = v6(Pinhole::Opened("PCP"));
        s.asked = Some("Network check (net_natCheck 0 turns it off): asked the STUN servers stun-a.test (192.0.2.20), stun-b.test (198.51.100.20).".into());
        s
    }
    /// The mockup's s2: the router did not answer, the firewall is open.
    fn s2() -> Status {
        let mut s = st(Fw::Allowed, Router::NoAnswer);
        s.addrs = order_addrs(&["192.0.2.10".parse().unwrap()], Some("192.0.2.10".parse().unwrap()));
        s.nat = Some(Nat::Moderate);
        s
    }
    fn plain(l: &[String]) -> Vec<String> {
        l.iter().map(|x| crate::firewall::strip_links(&strip(x))).collect()
    }
    fn strip(s: &str) -> String {
        let mut out = String::new();
        let mut it = s.chars().peekable();
        while let Some(c) = it.next() {
            if c == '^' && it.peek().is_some_and(|n| *n != '^') {
                it.next();
                continue;
            }
            out.push(c);
        }
        out
    }
    /// The console as one text, its wrapped lines joined again.
    fn flat(s: &Status) -> String {
        let t = plain(&s.console()).join("\n");
        let mut out = String::new();
        for (i, l) in t.split('\n').enumerate() {
            let t = l.trim_start();
            let cont = l.starts_with(&" ".repeat(COL)) || (l.starts_with("  ") && !l.starts_with("   ") && !t.starts_with("Players") && !t.starts_with('['));
            if i > 0 {
                out.push(if cont { ' ' } else { '\n' });
            }
            out.push_str(if cont { t } else { l });
        }
        out
    }
    fn all_text(s: &Status) -> String {
        let mut t = flat(s);
        if let Some(p) = s.prompt(EXE) {
            t.push_str(&format!("\n{}\n{}\n{}\n{}\n{}\n{}", p.title, p.main, p.content, p.footer, p.details, p.verify));
            for b in &p.buttons {
                t.push_str(&format!("\n{}", b.1));
            }
            t.push_str(&p.plain());
        }
        t
    }
    fn all_fws() -> Vec<Fw> {
        vec![Fw::NotApplicable, Fw::Allowed, Fw::LanOnly, Fw::NoRule, Fw::BlockRule, Fw::Unchecked, Fw::Checking]
    }
    fn all_routers() -> Vec<Router> {
        vec![
            Router::Off,
            Router::NoAnswer,
            Router::Refused("718".into()),
            Router::Refused("501".into()),
            Router::Failed("x".into()),
            fwd(),
            Router::Forwarded { via: "NAT-PMP".into(), ext: Some("10.0.0.1".into()), ext_ok: false },
            Router::Forwarded { via: "PCP".into(), ext: None, ext_ok: true },
            Router::Behind,
            Router::GatewayRefused(Proto::Pcp, 2),
            Router::Released,
        ]
    }

    #[test]
    fn s1_dialog_router_open_firewall_closed() {
        let p = s1().prompt(EXE).unwrap();
        assert_eq!(p.title, "Tremulous: hosting atcs");
        assert_eq!(p.main, "Players can't join yet: 1 step left");
        let c: Vec<&str> = p.content.lines().collect();
        assert_eq!(c[0], "Players on the internet: 1 step left: this PC's firewall");
        assert_eq!(c[1], "Players on your network: 1 step left: this PC's firewall");
        assert_eq!(c[2], "");
        assert_eq!(c[3], "[OK] Router: opened UDP port 30720 automatically for this game (PCP) - it closes when you stop hosting.");
        assert_eq!(c[4], "[NO] This PC's firewall: closed to the game (no Windows Firewall rule yet).");
        assert_eq!(c[6], "    Internet address (UDP): 203.0.113.7:30720 (IPv6: [2001:db8:77::5]:30720)");
        assert_eq!(c[7], format!("    Network type: Open - the router opened UDP 30720 for this game (port mapping) <a href=\"{GUIDE_URL}\">What does this mean?</a>"));
        // the network type describes the router only: no "router + PC" next to a closed firewall
        assert!(!p.content.contains("router + PC"));
        let labels: Vec<&str> = p.buttons.iter().map(|b| b.1.split('\n').next().unwrap()).collect();
        assert_eq!(labels, ["Allow players to join", "Host for my home network only (LAN)", "Host anyway"]);
        assert_eq!(p.buttons.iter().map(|b| b.0).collect::<Vec<_>>(), [ANSWER_ALLOW, ANSWER_LAN, ANSWER_HOST]);
        assert!(p.buttons[0].1.ends_with("\nOpens this PC's firewall for the game (one Windows Firewall rule). Windows asks for permission."));
        assert!(p.buttons[1].1.ends_with("(local subnet); the router port is closed again."));
        assert!(p.buttons[2].1.ends_with("\nNobody else can join until the firewall is open."));
        assert_eq!(p.verify, "Don't ask again when I host");
        assert_eq!(p.footer, "Allow adds one Windows Firewall rule for this game only, used only while you host. Windows asks for permission first.");
        for k in [EXE, "incoming UDP", "\"Tremulous Revamped\"", "\"Tremulous Revamped (LAN)\"", "local subnet", "Risk: small", "UAC", "PowerShell", "net_hostWindow 0", "no longer exist"] {
            assert!(p.details.contains(k), "{k}");
        }
        assert_eq!(p.plain_buttons().last().unwrap().0, ANSWER_NEVER);
    }

    #[test]
    fn s1_console_columns_and_colours() {
        let c = s1().console();
        assert_eq!(c[0], "Hosting atcs on UDP 30720. Players can't join yet: 1 step left");
        assert_eq!(c[1], "  Players on the internet:  ^11 step left: ^7this PC's firewall");
        assert_eq!(c[2], "  Players on your network:  ^11 step left: ^7this PC's firewall");
        assert_eq!(c[3], "  ^2[OK] ^7Router               ^2opened ^7UDP port 30720 automatically for this game");
        assert_eq!(c[4], format!("{}(PCP) - it closes when you stop hosting.", " ".repeat(COL)));
        assert!(c[5].starts_with("  ^1[NO] ^7This PC's firewall   ^1closed ^7to the game (no Windows Firewall rule"), "{}", c[5]);
        let p = plain(&c);
        let at = |k: &str| p.iter().position(|l| l.starts_with(k)).unwrap_or_else(|| panic!("{k}: {p:#?}"));
        assert_eq!(p[at("       Internet (UDP)")], "       Internet (UDP)       203.0.113.7:30720 (public IPv4)");
        assert!(p[at("       IPv6 (UDP)")].starts_with("       IPv6 (UDP)           [2001:db8:77::5]:30720"));
        assert_eq!(p[at("       LAN (UDP)")], "       LAN (UDP)            192.0.2.10:30720");
        assert_eq!(p[at("       Network type")], "       Network type         Open - the router opened UDP 30720 for this game");
        assert!(p.contains(&"What does this mean? README, section \"Hosting and your network\".".to_string()));
        assert!(p.iter().any(|l| l.starts_with("Network check (net_natCheck 0 turns it off)")));
        // the console's own wrap: a line's words fit in 78 columns, colour codes not counted
        for l in &p {
            assert!(l.chars().count() <= WIDTH, "{l}");
        }
        for l in &c {
            for w in l.split(' ') {
                assert!(w.len() < 2 || !w[1..].contains('^'), "a colour code inside a word: {w}");
            }
        }
    }

    #[test]
    fn s2_router_no_answer_firewall_open_console_only() {
        let s = s2();
        assert!(s.prompt(EXE).is_none(), "no dialog when the firewall already allows");
        let c = s.console();
        assert_eq!(c[0], "Hosting atcs on UDP 30720. Internet players can't join yet: 1 step left");
        assert_eq!(c[1], "  Players on the internet:  ^31 step left: ^7your router");
        assert_eq!(c[2], "  Players on your network:  ^2can join^7");
        let p = flat(&s);
        assert!(p.contains("[??] Router               no answer: UDP port 30720 was not opened automatically (no UPnP, PCP or NAT-PMP answer)."), "{p}");
        assert!(p.contains("Fix: turn on automatic port opening (UPnP) in the router, or forward UDP port 30720 to this PC, 192.0.2.10 (port forwarding): https://portforward.com. If you already did, players can join."), "{p}");
        assert!(p.contains("[OK] This PC's firewall   open for the game (Windows Firewall rule)."));
        assert!(p.contains("       Network type         Moderate - port stays the same (cone NAT)"));
        assert!(!p.contains("Internet (UDP)"), "no public address known: {p}");
        // the dialog's text for the same router row (when the firewall also lacks)
        let mut d = s.clone();
        d.fw = Fw::NoRule;
        let pr = d.prompt(EXE).unwrap();
        assert_eq!(pr.main, "Players can't join yet: 2 steps left");
        assert!(pr.content.starts_with("Players on the internet: 2 steps left: your router, this PC's firewall\nPlayers on your network: 1 step left: this PC's firewall"));
        assert!(pr.content.contains("[??] Router: no answer: UDP port 30720 was not opened automatically (no UPnP, PCP or NAT-PMP answer). Fix: turn on automatic port opening (UPnP) in the router, or forward UDP port 30720 to this PC, 192.0.2.10 (port forwarding). <a href=\"https://portforward.com\">How to forward a port</a>. If you already forwarded it, players can join; the game can't check this."), "{}", pr.content);
        assert!(pr.buttons[1].1.ends_with("the router port stays closed."));
    }

    #[test]
    fn firewall_wait_is_bounded() {
        // the firewall answered, router and network too: at once
        assert!(ready(true, true, true, true, 0.0, 0.0));
        // the firewall has not answered: waits, then opens anyway at FW_WAIT_SECS of wall time
        assert!(!ready(false, true, true, true, 0.0, 0.0));
        assert!(!ready(false, true, true, true, 7.9, 7.9));
        assert!(ready(false, true, true, true, 0.0, FW_WAIT_SECS));
        assert!(ready(false, false, true, false, 1.0, 9.0));
        // slow frames (the frame clock hardly moves): the wall clock still bounds it
        assert!(ready(false, true, true, true, 3.0, 8.5));
        // the router is slow too: still the old 4 s rule when the firewall answered
        assert!(!ready(true, false, true, true, 3.9, 3.9));
        assert!(ready(true, false, true, true, UPNP_WAIT_SECS, UPNP_WAIT_SECS));
    }

    #[test]
    fn firewall_still_checking_row_and_update() {
        let s = st(Fw::Checking, fwd());
        let row = s.fw_step().unwrap();
        assert_eq!((row.mark, row.state.as_str()), (Mark::Maybe, "still checking"));
        assert_eq!(s.headline(), "Hosting atcs: still checking this PC's firewall");
        assert_eq!(s.lan_row(), "Players on your network: not sure yet: this PC's firewall is still being checked");
        assert_eq!(s.internet_row(), "Players on the internet: not sure yet: this PC's firewall is still being checked");
        let c = s.console().join("\n");
        assert!(c.contains("[??] ") && c.contains("still checking"), "{c}");
        let content = s.status_prompt(EXE, false).content;
        assert!(content.contains("[??] This PC's firewall: still checking"), "{content}");
        // no firewall question while it is not known (nothing to fix yet)
        assert!(s.prompt(EXE).is_none());
        // the answer comes: the changed lines
        let done = Status { fw: Fw::NoRule, ..s.clone() };
        let u = updates(&s, &done).join("\n");
        assert!(u.contains("Update: This PC's firewall: closed to the game"), "{u}");
        let ok = Status { fw: Fw::Allowed, ..s.clone() };
        assert!(updates(&s, &ok).iter().any(|l| l.starts_with("Update: This PC's firewall: open for the game")));
        // nothing changed: no update
        assert!(updates(&s, &s).is_empty());
    }

    #[test]
    fn hosting_status_while_checking_says_what_is_known() {
        let l = known_lines(None, Some(5.4), Some("answered".into()), None);
        assert_eq!(l[0], "This PC's firewall: still checking (5 s so far)");
        assert_eq!(l[1], "Router: answered");
        assert_eq!(l[2], "Network type: still checking");
        let l = known_lines(Some(Fw::NoRule.word().to_string()), None, None, Some("off (net_natCheck 0)".into()));
        assert_eq!(l[0], "This PC's firewall: closed (no rule)");
        for fw in all_fws() {
            assert!(!fw.word().is_empty());
        }
    }

    #[test]
    fn skip_lines_say_why() {
        assert_eq!(say::window_off(), "The hosting window is off (net_hostWindow 0). Type hosting_status to open it.");
        assert!(say::taskdialog_failed("TaskDialogIndirect failed, HRESULT 0x80070057").contains("0x80070057"));
        assert!(say::taskdialog_failed("x").contains("plain dialog"));
        assert!(say::godot_dialog_failed("ERR_UNAVAILABLE").contains("ERR_UNAVAILABLE"));
        assert!(say::still_checking(8.0).contains("8 s") && say::still_checking(8.0).contains("update"));
        assert!(say::check_failed("timed out after 60 s").contains("60 s"));
    }

    #[test]
    fn debug_file_has_the_facts() {
        let f = DebugFacts {
            host_window: "1".into(),
            old_prompt: Some("0".into()),
            windows: true,
            hosting: true,
            firewall: "running for 9 s".into(),
            router: "no answer".into(),
            network: "waiting".into(),
            collecting: false,
            dialog: "Windows dialog open".into(),
            events: vec!["+0.0 s hosting atcs".into(), "+8.0 s firewall check still running".into()],
            status: vec!["Hosting atcs on UDP 30720.".into()],
        };
        let t = debug_text(&f);
        assert!(t.contains("net_hostWindow = 1"), "{t}");
        assert!(t.contains("net_firewallPrompt = 0 (old setting, ignored)"));
        assert!(t.contains("This PC's firewall check: running for 9 s"));
        assert!(t.contains("  +8.0 s firewall check still running"));
        assert!(t.contains("  Hosting atcs on UDP 30720."));
        assert!(t.ends_with('\n') && t.lines().count() < 25);
        let none = debug_text(&DebugFacts { host_window: "0".into(), ..Default::default() });
        assert!(none.contains("net_hostWindow = 0") && none.contains("not set") && none.contains("nothing yet") && none.contains("none yet"));
    }

    #[test]
    fn every_firewall_state() {
        let row = |fw: Fw| st(fw, fwd()).fw_step().map(|s| (s.mark, format!("{}{}", s.state, s.rest)));
        assert_eq!(row(Fw::NotApplicable), None);
        assert_eq!(row(Fw::Allowed), Some((Mark::Ok, "open for the game (Windows Firewall rule).".into())));
        assert_eq!(row(Fw::NoRule), Some((Mark::No, "closed to the game (no Windows Firewall rule yet).".into())));
        assert_eq!(row(Fw::BlockRule), Some((Mark::No, "closed to the game (a Windows Firewall rule blocks it).".into())));
        assert_eq!(row(Fw::LanOnly), Some((Mark::Maybe, "open for your home network only (Windows Firewall rule for the local subnet).".into())));
        assert_eq!(row(Fw::Unchecked), Some((Mark::Maybe, "not checked: Windows did not answer the check.".into())));
        // a block rule: Allow says it removes it
        let p = st(Fw::BlockRule, fwd()).prompt(EXE).unwrap();
        assert!(p.buttons[0].1.contains("Removes the rule that blocks the game"));
        // LAN only: players on your network join, internet players need the firewall; no dialog
        let s = st(Fw::LanOnly, fwd());
        assert_eq!(s.lan_row(), "Players on your network: can join");
        assert_eq!(s.internet_row(), "Players on the internet: 1 step left: this PC's firewall");
        assert_eq!(s.headline(), "Internet players can't join yet: 1 step left");
        assert!(s.prompt(EXE).is_none());
        assert!(flat(&s).contains("firewall_allow to open it for players on the internet too"), "{}", flat(&s));
        // unchecked
        let s = st(Fw::Unchecked, fwd());
        assert_eq!(s.headline(), "Hosting atcs");
        assert_eq!(s.lan_row(), "Players on your network: not sure: this PC's firewall was not checked");
        assert_eq!(s.internet_row(), "Players on the internet: not sure: this PC's firewall was not checked");
        assert!(s.prompt(EXE).is_none());
        // all fine
        let s = st(Fw::Allowed, fwd());
        assert_eq!(s.headline(), "Players can join");
        assert_eq!(s.internet_row(), "Players on the internet: can join");
        assert!(s.prompt(EXE).is_none());
        // not Windows: only the router row
        let s = st(Fw::NotApplicable, Router::Refused("501".into()));
        assert_eq!(s.headline(), "Internet players can't join yet: 1 step left");
        assert_eq!(s.lan_row(), "Players on your network: can join");
        assert!(!plain(&s.console()).join("\n").contains("firewall"));
    }

    #[test]
    fn every_router_state() {
        let line = |r: Router| st(Fw::Allowed, r).router_line();
        assert_eq!(line(fwd()), "Router: opened UDP port 30720 automatically for this game (UPnP); it closes when you stop hosting.");
        assert_eq!(line(Router::Refused("718".into())), "Router: refused to open UDP port 30720 (UPnP error 718: that port is already forwarded to another device).");
        assert_eq!(line(Router::Refused("501".into())), "Router: refused to open UDP port 30720 (UPnP error 501).");
        assert_eq!(line(Router::GatewayRefused(Proto::Pcp, 2)), "Router: refused to open UDP port 30720 (PCP: not allowed).");
        assert!(line(Router::GatewayRefused(Proto::NatPmp, 77)).contains("(NAT-PMP error 77)"));
        assert_eq!(line(Router::Behind), "Router: not opened: another router or your provider's shared address (CGNAT) is in front (PCP saw another address than this PC's).");
        assert!(line(Router::Forwarded { via: "PCP".into(), ext: Some("10.1.2.3".into()), ext_ok: false }).contains("opened UDP port 30720 (PCP), but another router or your provider's shared address (CGNAT) is in front: 10.1.2.3 can't be reached from the internet."));
        assert_eq!(line(Router::Off), "Router: not asked (net_upnp 0): UDP port 30720 is not opened automatically.");
        assert_eq!(line(Router::Released), "Router: closed again: you host for your home network only (LAN).");
        assert!(line(Router::Failed("timeout".into())).contains("(UPnP: timeout)"));
        // marks: refusals are a sure no, no answer is "not as far as the game can see"
        assert_eq!(st(Fw::Allowed, Router::Refused("718".into())).router_step().mark, Mark::No);
        assert_eq!(st(Fw::Allowed, Router::NoAnswer).router_step().mark, Mark::Maybe);
        // the link only where forwarding by hand is the fix (not for net_upnp 0, not when fine)
        assert!(st(Fw::Allowed, Router::Refused("718".into())).router_step().link);
        assert!(!st(Fw::Allowed, Router::Off).router_step().link);
        assert!(!st(Fw::Allowed, fwd()).router_step().link);
        assert!(!plain(&st(Fw::Allowed, Router::Off).console()).join("\n").contains("portforward"));
        assert!(!plain(&st(Fw::Allowed, fwd()).console()).join("\n").contains("portforward"));
        // home network only: nobody on the internet this time, no router step counted
        let s = st(Fw::Allowed, Router::Released);
        assert_eq!(s.headline(), "Players on your network can join");
        assert_eq!(s.internet_row(), "Players on the internet: not this time: you host for your home network only (LAN)");
        // a public address: the router is not needed
        let mut s = st(Fw::Allowed, Router::NoAnswer);
        s.nat = Some(Nat::OpenPublic);
        assert_eq!(s.headline(), "Players can join");
        assert_eq!(s.router_line(), "Router: not needed: this PC has its own public address.");
        assert!(flat(&s).contains("Internet (UDP)       10.0.0.5:30720 (public IPv4)"));
        // the forward hint without an address
        let mut s = st(Fw::Allowed, Router::NoAnswer);
        s.addrs.clear();
        assert!(s.router_step().fix.unwrap().contains("forward UDP port 30720 to this PC (port forwarding)"));
        assert!(!plain(&s.console()).join("\n").contains("LAN (UDP)"));
    }

    #[test]
    fn several_adapters_main_first() {
        let p = flat(&st(Fw::Allowed, fwd()));
        assert!(p.contains("LAN (UDP)            10.0.0.5:30720 (main), 10.20.30.4:30720 (other adapter)"), "{p}");
    }

    #[test]
    fn two_internet_connections_read_unclear() {
        let mut s = s1();
        s.nat = Some(Nat::Unclear { game: "203.0.113.7".parse().unwrap(), other: "198.51.100.9".parse().unwrap() });
        let c = plain(&s.console()).join(" ").split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(c.contains("Network type Unclear - the game's internet address differs from this PC's other traffic"), "{c}");
        assert!(c.contains("The game is seen at 203.0.113.7, this PC's other traffic at 198.51.100.9."), "{c}");
        assert!(c.contains("Players join at the game's address, 203.0.113.7:30720."), "{c}");
        let p = s.prompt("C:\\Games\\T\\tremulous.exe").unwrap();
        assert!(p.content.contains("Network type: Unclear - the game's internet address differs from this PC's other traffic: two internet connections, rare (multi-WAN / policy routing)"), "{}", p.content);
        assert!(!c.to_ascii_lowercase().contains("double nat"));
    }

    #[test]
    fn wording_rules_in_every_state() {
        for f in all_fws() {
            for r in all_routers() {
                for nat in [None, Some(Nat::Moderate), Some(Nat::OpenMapped)] {
                    let mut s = st(f.clone(), r.clone());
                    s.nat = nat;
                    s.v6 = v6(Pinhole::NoAnswer);
                    let t = all_text(&s);
                    let low = t.to_ascii_lowercase();
                    // the Windows profile names never reach the player; players, not friends
                    for w in ["public network", "private network", "domain", "friend", "router + pc"] {
                        assert!(!low.contains(w), "{w} in {f:?} {r:?}: {t}");
                    }
                    // every port says UDP (an address's ":30720" is one)
                    for (i, _) in t.match_indices("30720") {
                        let before = &t[..i];
                        assert!(before.ends_with(':') || before.ends_with("UDP ") || before.ends_with("UDP port "), "bare port in {f:?} {r:?}: ...{}", &before[before.len().saturating_sub(40)..]);
                    }
                }
            }
        }
    }

    #[test]
    fn router_from_upnp_results() {
        let e = |s: &str| Router::from_result(&Err(s.to_string()));
        assert_eq!(
            e("no UPnP router answered (UPnP off, or not a home router)"),
            Router::NoAnswer
        );
        assert_eq!(
            e("the router refused the port mapping (UPnP error 718 ConflictInMappingEntry)"),
            Router::Refused("718".into())
        );
        assert!(matches!(
            e("the router's description answered HTTP 404"),
            Router::Failed(_)
        ));
        let m = Mapping {
            control_url: String::new(),
            service: String::new(),
            port: 30720,
            local_ip: "10.0.0.5".parse().unwrap(),
            external_ip: Some("100.72.0.5".into()),
            lease: 7200,
            desc: String::new(),
        };
        assert_eq!(
            Router::from_result(&Ok(m.clone())),
            Router::Forwarded {
                via: "UPnP".into(),
                ext: Some("100.72.0.5".into()),
                ext_ok: false
            }
        );
        let m = Mapping {
            external_ip: Some("203.0.113.7".into()),
            ..m
        };
        assert_eq!(Router::from_result(&Ok(m)), fwd());
    }

    #[test]
    fn addresses_main_first_and_filtered() {
        let a = order_addrs(
            &[
                "10.20.30.4".parse().unwrap(),
                "127.0.0.1".parse().unwrap(),
                "169.254.1.1".parse().unwrap(),
                "10.0.0.5".parse().unwrap(),
                "10.0.0.5".parse().unwrap(),
            ],
            Some("10.0.0.5".parse().unwrap()),
        );
        assert_eq!(
            a,
            vec![
                ("10.0.0.5".parse().unwrap(), true),
                ("10.20.30.4".parse().unwrap(), false)
            ]
        );
        // no default route: a lone address is main, several are all "other adapter"
        assert_eq!(
            order_addrs(&["10.0.0.5".parse().unwrap()], None),
            vec![("10.0.0.5".parse().unwrap(), true)]
        );
        assert!(order_addrs(
            &["10.0.0.5".parse().unwrap(), "10.0.0.6".parse().unwrap()],
            None
        )
        .iter()
        .all(|x| !x.1));
    }

    #[test]
    fn the_upnp_wait_is_four_seconds() {
        // firewall not answered: waits (but not past FW_WAIT_SECS of wall time, see firewall_wait_is_bounded)
        assert!(!ready(false, true, true, true, 4.0, 4.0));
        assert!(!ready(false, true, true, true, 7.0, 7.0));
        // router answered, or UPnP off: ready at once
        assert!(ready(true, true, true, true, 0.0, 0.0));
        assert!(ready(true, false, false, true, 0.0, 0.0));
        // router silent: waits, then gives up at 4 s of running frames
        assert!(!ready(true, false, true, true, 0.0, 0.0));
        assert!(!ready(true, false, true, true, 3.9, 3.9));
        assert!(ready(true, false, true, true, 4.0, 4.0));
        // the network check not over yet: waits for it too, as long
        assert!(!ready(true, true, true, false, 1.0, 1.0));
        assert!(ready(true, true, true, false, 4.0, 4.0));
        // a stalled map load: wall time runs on, the frame clock hardly does; the router is still given its 4 s of frames
        assert!(!ready(true, false, true, true, 0.5, 6.0));
    }

    #[test]
    fn a_stalled_map_load_then_late_answers() {
        use crate::natcheck::FrameClock;
        use std::time::{Duration, Instant};
        // live test: the report is made at spawn, then the map load freezes
        // the frames for 6 s; the STUN requests go out on the first frame after
        let mut c = FrameClock::default();
        let t = Instant::now();
        let ms = Duration::from_millis;
        assert!(!ready(true, true, true, false, c.tick(t), 0.0));
        assert!(!ready(true, true, true, false, c.tick(t + ms(16)), 0.0));
        let w = c.tick(t + ms(6016));
        assert!(w < 0.3 && !ready(true, true, true, false, w, 0.0), "the stall counts as one short frame: {w}");
        // the answers 4 ms later: ready with them
        assert!(ready(true, true, true, true, c.tick(t + ms(6020)), 0.0));
        // frames that do run still end the wait at 4 s
        let mut c = FrameClock::default();
        let mut w = 0.0;
        for i in 0..=260u64 {
            w = c.tick(t + ms(16 * i));
        }
        assert!(ready(true, false, true, false, w, 0.0), "{w}");
        // an answer after the report: the changed lines as updates
        let old = st(Fw::Allowed, Router::NoAnswer);
        let mut new = old.clone();
        assert!(updates(&old, &new).is_empty());
        new.nat = Some(Nat::Moderate);
        assert_eq!(updates(&old, &new), vec!["Update: Network type: Moderate - port stays the same (cone NAT)".to_string()]);
        let old = new.clone();
        new.router = fwd();
        new.nat = Some(Nat::OpenMapped);
        let u = updates(&old, &new);
        assert_eq!(u[0], "Update: Players on the internet: can join");
        assert!(u[1].starts_with("Update: Router: opened UDP port 30720 automatically for this game (UPnP)"), "{u:?}");
        assert!(u[2].starts_with("Update: Network type: Open - the router opened UDP 30720"), "{u:?}");
        assert_eq!(u.len(), 3);
        // the dialog's text follows (the open dialog is redrawn with it)
        let mut a = st(Fw::NoRule, Router::NoAnswer);
        let before = a.prompt("x.exe").unwrap().content;
        a.nat = Some(Nat::Strict);
        assert!(!before.contains("Network type") && a.prompt("x.exe").unwrap().content.contains("Network type: Strict - port changes per player (symmetric NAT)"));
    }

    fn pr(pcp: Outcome, upnp: Option<Result<Mapping, String>>) -> PortResult {
        let opened = match (&pcp, &upnp) {
            (Outcome::Mapped(l), _) => Some(Opened::Lease(l.clone())),
            (_, Some(Ok(m))) => Some(Opened::Upnp(m.clone())),
            _ => None,
        };
        PortResult { opened, pcp, upnp, v6: None }
    }

    #[test]
    fn one_router_result_from_three_protocols() {
        let no = || Some(Err("no UPnP router answered (UPnP off, or not a home router)".to_string()));
        assert_eq!(Router::from_port(&pr(Outcome::NoAnswer, no())), Router::NoAnswer);
        assert_eq!(Router::from_port(&pr(Outcome::Mismatch, no())), Router::Behind);
        assert_eq!(Router::from_port(&pr(Outcome::Refused(crate::pcp::Proto::Pcp, 2), no())), Router::GatewayRefused(Proto::Pcp, 2));
        // UPnP's own refusal is the more useful one
        let r718 = Some(Err("the router refused the port mapping (UPnP error 718 ConflictInMappingEntry)".to_string()));
        assert_eq!(Router::from_port(&pr(Outcome::Refused(crate::pcp::Proto::NatPmp, 2), r718)), Router::Refused("718".into()));
        let m = Mapping { control_url: String::new(), service: String::new(), port: 30720, local_ip: "10.0.0.5".parse().unwrap(), external_ip: Some("203.0.113.7".into()), lease: 7200, desc: String::new() };
        assert_eq!(Router::from_port(&pr(Outcome::NoAnswer, Some(Ok(m)))), fwd());
    }

    #[test]
    fn ipv6_rows_and_network_facts() {
        let mut s = st(Fw::Allowed, Router::NoAnswer);
        s.v6 = v6(Pinhole::Opened("PCP"));
        assert_eq!(s.v6_line().unwrap(), "IPv6 (UDP): [2001:db8:77::5]:30720 (the router opened its IPv6 firewall, PCP)");
        s.v6 = v6(Pinhole::Refused(Proto::Pcp, 2));
        assert_eq!(s.v6_line().unwrap(), "IPv6 (UDP): [2001:db8:77::5]:30720, if the router's IPv6 firewall lets players in (it refused to open it: PCP: not allowed)");
        s.v6 = v6(Pinhole::NotAsked);
        assert_eq!(s.v6_line().unwrap(), "IPv6 (UDP): [2001:db8:77::5]:30720, if the router's IPv6 firewall lets players in");
        // the dialog: IPv6 only, no public IPv4 known
        s.fw = Fw::NoRule;
        s.v6 = v6(Pinhole::NoAnswer);
        assert!(s.prompt(EXE).unwrap().content.contains("    Internet address (UDP): IPv6 [2001:db8:77::5]:30720, if the router's IPv6 firewall lets players in (no PCP answer)"));
        assert_eq!(v6_join_address(&"::1".parse().unwrap(), 27960), "[::1]:27960");
        assert_eq!(v6_join(None, 1, None), None);
        let a: Ipv6Addr = "2a02:1:2::5".parse().unwrap();
        let mut r = pr(Outcome::NoAnswer, None);
        r.v6 = Some(Outcome::Refused(Proto::Pcp, 2));
        assert_eq!(v6_join(Some(a), 30720, Some(&r)).unwrap().pinhole, Pinhole::Refused(Proto::Pcp, 2));
        r.v6 = Some(Outcome::NoAnswer);
        assert!(v6_join(Some(a), 30720, Some(&r)).unwrap().pinhole == Pinhole::NoAnswer);
        assert_eq!(v6_join(Some(a), 30720, None).unwrap().pinhole, Pinhole::NotAsked);
        // CGNAT facts for the network check
        assert!(router_facts(&Router::Behind).behind);
        assert!(router_facts(&Router::Forwarded { via: "UPnP".into(), ext: Some("100.72.0.5".into()), ext_ok: false }).behind);
        assert!(router_facts(&fwd()).opened && router_facts(&fwd()).external == Some("203.0.113.7".parse().unwrap()));
        assert_eq!(router_facts(&Router::Released), crate::natcheck::RouterFacts::default());
    }

    #[test]
    fn the_links_are_the_allow_list() {
        let p = s1().prompt(EXE).unwrap();
        let mut d = s2();
        d.fw = Fw::NoRule;
        let q = d.prompt(EXE).unwrap();
        for c in [&p.content, &q.content] {
            for (i, _) in c.match_indices("<a href=\"") {
                let href = &c[i + 9..i + 9 + c[i + 9..].find('"').unwrap()];
                assert!(LINKS.contains(&href), "{href}");
            }
        }
        assert!(q.content.contains(HELP_URL) && q.content.contains(GUIDE_URL));
        assert_eq!(GUIDE_URL, "https://github.com/MeepingsCorner/Tremulous-Revamped#hosting-and-your-network");
    }

    #[test]
    fn state_text_is_ascii() {
        // every font, code page and screen reader: no glyphs, no em dashes
        let nats = [Nat::OpenPublic, Nat::OpenMapped, Nat::Moderate, Nat::Strict, Nat::Cgnat(String::new()), Nat::Cgnat("100.64.0.1".into()), Nat::Unknown("no STUN server answered")];
        for base in [s1(), s2()] {
            for n in nats.iter().cloned().map(Some).chain([None]) {
                let mut s = base.clone();
                s.nat = n;
                for l in s.console() {
                    assert!(l.is_ascii(), "{l}");
                }
                if let Some(p) = s.prompt(EXE) {
                    for t in [&p.title, &p.main, &p.content, &p.footer, &p.details, &p.verify] {
                        assert!(t.is_ascii(), "{t}");
                    }
                    assert!(p.buttons.iter().all(|b| b.1.is_ascii()));
                }
            }
        }
    }

    /// The window on every host, also when the
    /// firewall already lets players in, and the way to have it back.
    #[test]
    fn the_status_window_when_nothing_is_lacking() {
        let s = st(Fw::Allowed, pcp_fwd());
        assert!(s.prompt(EXE).is_none(), "the firewall question itself stays for a lacking rule");
        let p = s.status_prompt(EXE, false);
        assert_eq!(p.main, "Players can join");
        assert!(!p.warn && !p.checked);
        assert_eq!(p.buttons, vec![(ANSWER_HOST, "Keep hosting\nCloses this window; nothing is changed.".to_string())]);
        assert_eq!(p.verify, "Don't ask again when I host");
        assert!(p.content.starts_with("Players on the internet: can join\nPlayers on your network: can join\n\n[OK] Router: opened UDP port 30720"), "{}", p.content);
        assert!(p.content.contains("\n[OK] This PC's firewall: open for the game"), "{}", p.content);
        assert!(p.footer.contains("hosting_status") && p.details.contains("net_hostWindow 1") && p.details.contains("untick"));
        // opened by hosting_status with net_hostWindow 0: the box starts ticked
        assert!(s.status_prompt(EXE, true).checked);
        // a lacking rule: the question with its buttons, the box as asked
        let q = st(Fw::NoRule, pcp_fwd()).status_prompt(EXE, true);
        assert!(q.warn && q.checked && q.buttons.len() == 3);
        assert_eq!(Prompt { checked: false, ..q }, st(Fw::NoRule, pcp_fwd()).prompt(EXE).unwrap());
        assert!(st(Fw::NoRule, pcp_fwd()).prompt(EXE).unwrap().details.contains("hosting_status to see this window again"));
        // home network only: a way to everyone, warning icon
        let l = st(Fw::LanOnly, pcp_fwd()).status_prompt(EXE, false);
        assert!(l.warn && l.buttons[0].0 == ANSWER_ALLOW && l.buttons[0].1.starts_with("Allow players on the internet too\n"));
        // the router did not answer: a step may be left, warning icon
        assert!(st(Fw::Allowed, Router::NoAnswer).status_prompt(EXE, false).warn);
        for t in [&p.title, &p.main, &p.content, &p.footer, &p.details, &p.verify] {
            assert!(t.is_ascii() && !t.to_ascii_lowercase().contains("friend"), "{t}");
        }
    }

    /// The router opened the port on one public address, the game's traffic
    /// leaves by another (a network with two internet connections): Unclear, players join
    /// at the router's address, no CGNAT anywhere.
    #[test]
    fn two_internet_connections_in_the_status() {
        let mut s = st(Fw::Allowed, Router::Forwarded { via: "PCP".into(), ext: Some("203.0.113.172".into()), ext_ok: true });
        s.nat = Some(Nat::Split { router: "203.0.113.172".parse().unwrap(), seen: "198.51.100.201".parse().unwrap() });
        let c = plain(&s.console()).join(" ").split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(c.contains("Internet (UDP) 203.0.113.172:30720"), "{c}");
        assert!(c.contains("Network type Unclear - the router opened the port on another internet address than the game's traffic uses"), "{c}");
        assert!(c.contains("Players on the internet join at 203.0.113.172:30720, the address the router opened."), "{c}");
        let lc = c.to_ascii_lowercase();
        assert!(!lc.contains("cgnat") && !lc.contains("carrier"), "{c}");
        let p = s.status_prompt(EXE, false);
        assert!(p.content.contains("Network type: Unclear - the router opened the port on another internet address"), "{}", p.content);
    }

    #[test]
    fn tags_are_ascii() {
        assert_eq!([Mark::Ok.tag(), Mark::No.tag(), Mark::Maybe.tag()], ["[OK]", "[NO]", "[??]"]);
        assert_eq!(wrap("ab cd", 4), vec!["ab cd".to_string()]);
        assert_eq!(vis_len("^1red^7"), 3);
        assert_eq!(paint("^1", "closed to the game", 6), "^1closed ^7to the game");
        assert_eq!(paint("^3", "no answer: UDP", 9), "^3no answer: ^7UDP");
        assert_eq!(paint("^2", "can join", 8), "^2can join^7");
    }
}
