//! UPnP port mapping for a hosted game (port addition, dec046).
//!
//! A friend joining over the internet reaches the host's router, not the
//! host: the router must forward UDP 30720 to the hosting PC. Most home
//! routers do that on request (UPnP Internet Gateway Device): this finds
//! the router (SSDP), asks it to forward the server's port to this PC
//! (AddPortMapping) and for the address the friend should join
//! (GetExternalIPAddress), and takes the mapping back when the server
//! stops. The mapping is timed (two hours, like PCP) and renewed while the
//! server runs (pcp::keep_upnp), so a game that crashes or is killed leaves
//! nothing in the router for longer than that; only a router that takes
//! permanent mappings alone (error 725) gets one. std only, run on a thread
//! (discovery waits up to two seconds).

use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream, ToSocketAddrs, UdpSocket};
use std::time::{Duration, Instant};

/// A port the router forwards to this PC.
#[derive(Clone, Debug, PartialEq)]
pub struct Mapping {
    /// the WAN connection service's control URL and type
    pub control_url: String,
    pub service: String,
    pub port: u16,
    pub local_ip: Ipv4Addr,
    /// the router's internet address (what friends join), when it says
    pub external_ip: Option<String>,
    /// the lease asked for, in seconds (0: permanent, error 725 routers)
    pub lease: u32,
    pub desc: String,
}

/// UPnP error 725 OnlyPermanentLeasesSupported: the router takes lease 0 only.
const ONLY_PERMANENT: &str = "725";

const SERVICES: [&str; 3] = ["urn:schemas-upnp-org:service:WANIPConnection:2", "urn:schemas-upnp-org:service:WANIPConnection:1", "urn:schemas-upnp-org:service:WANPPPConnection:1"];

/// SSDP M-SEARCH: the description URLs of the gateways that answer.
pub fn discover(wait: Duration) -> Vec<String> {
    let Ok(s) = UdpSocket::bind(("0.0.0.0", 0)) else { return Vec::new() };
    let _ = s.set_read_timeout(Some(Duration::from_millis(200)));
    let _ = s.set_multicast_ttl_v4(2);
    for st in ["urn:schemas-upnp-org:device:InternetGatewayDevice:1", "urn:schemas-upnp-org:device:InternetGatewayDevice:2", "urn:schemas-upnp-org:service:WANIPConnection:1", "urn:schemas-upnp-org:service:WANPPPConnection:1"] {
        let msg = format!("M-SEARCH * HTTP/1.1\r\nHOST: 239.255.255.250:1900\r\nMAN: \"ssdp:discover\"\r\nMX: 2\r\nST: {st}\r\n\r\n");
        let _ = s.send_to(msg.as_bytes(), ("239.255.255.250", 1900));
    }
    let end = Instant::now() + wait;
    let mut out: Vec<String> = Vec::new();
    let mut buf = [0u8; 2048];
    while Instant::now() < end {
        if let Ok((n, _)) = s.recv_from(&mut buf) {
            let text = String::from_utf8_lossy(&buf[..n]);
            if let Some(loc) = header(&text, "location") {
                if !out.contains(&loc) {
                    out.push(loc);
                }
            }
        }
    }
    out
}

fn header(text: &str, name: &str) -> Option<String> {
    text.lines().find_map(|l| {
        let (k, v) = l.split_once(':')?;
        k.trim().eq_ignore_ascii_case(name).then(|| v.trim().to_string())
    })
}

/// http://host:port/path -> (host, port, path)
fn split_url(url: &str) -> Option<(String, u16, String)> {
    let rest = url.strip_prefix("http://")?;
    let (hostport, path) = match rest.find('/') {
        Some(i) => (&rest[..i], rest[i..].to_string()),
        None => (rest, "/".to_string()),
    };
    let (host, port) = match hostport.rsplit_once(':') {
        Some((h, p)) => (h.to_string(), p.parse().ok()?),
        None => (hostport.to_string(), 80),
    };
    Some((host, port, path))
}

/// One HTTP/1.1 request (Connection: close): (status, body).
fn http(url: &str, method: &str, headers: &[(&str, String)], body: &str) -> Result<(u32, String), String> {
    let (host, port, path) = split_url(url).ok_or_else(|| format!("bad URL {url}"))?;
    let addr: SocketAddr = (host.as_str(), port).to_socket_addrs().map_err(|e| e.to_string())?.next().ok_or("no address")?;
    let mut s = TcpStream::connect_timeout(&addr, Duration::from_secs(3)).map_err(|e| e.to_string())?;
    let _ = s.set_read_timeout(Some(Duration::from_secs(4)));
    let mut req = format!("{method} {path} HTTP/1.1\r\nHost: {host}:{port}\r\nConnection: close\r\nContent-Length: {}\r\n", body.len());
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str("\r\n");
    req.push_str(body);
    s.write_all(req.as_bytes()).map_err(|e| e.to_string())?;
    let mut raw = Vec::new();
    let _ = s.read_to_end(&mut raw);
    let text = String::from_utf8_lossy(&raw).to_string();
    let (head, body) = text.split_once("\r\n\r\n").ok_or("bad HTTP answer")?;
    let status: u32 = head.split_whitespace().nth(1).and_then(|c| c.parse().ok()).ok_or("bad HTTP status")?;
    let chunked = header(head, "transfer-encoding").is_some_and(|v| v.eq_ignore_ascii_case("chunked"));
    Ok((status, if chunked { dechunk(body) } else { body.to_string() }))
}

fn dechunk(mut b: &str) -> String {
    let mut out = String::new();
    while let Some((size, rest)) = b.split_once("\r\n") {
        let n = usize::from_str_radix(size.trim().split(';').next().unwrap_or(""), 16).unwrap_or(0);
        if n == 0 || rest.len() < n {
            break;
        }
        out.push_str(&rest[..n]);
        b = rest[n..].trim_start_matches("\r\n");
    }
    out
}

fn tag<'a>(xml: &'a str, name: &str) -> Option<&'a str> {
    let open = format!("<{name}>");
    let i = xml.find(&open)? + open.len();
    let j = xml[i..].find(&format!("</{name}>"))? + i;
    Some(xml[i..j].trim())
}

/// The WAN connection service of a gateway description: (service type,
/// absolute control URL).
pub fn wan_service(location: &str, description: &str) -> Option<(String, String)> {
    let base = tag(description, "URLBase").map(str::to_string).unwrap_or_else(|| {
        let (h, p, _) = split_url(location).unwrap_or_default();
        format!("http://{h}:{p}")
    });
    for svc in SERVICES {
        // the <service> block whose serviceType is svc
        let mut rest = description;
        while let Some(i) = rest.find("<service>") {
            let block_end = rest[i..].find("</service>").map(|j| i + j).unwrap_or(rest.len());
            let block = &rest[i..block_end];
            if tag(block, "serviceType") == Some(svc) {
                let ctl = tag(block, "controlURL")?;
                let url = if ctl.starts_with("http://") {
                    ctl.to_string()
                } else {
                    format!("{}/{}", base.trim_end_matches('/'), ctl.trim_start_matches('/'))
                };
                return Some((svc.to_string(), url));
            }
            rest = &rest[block_end..];
        }
    }
    None
}

fn soap(control: &str, service: &str, action: &str, args: &[(&str, String)]) -> Result<(u32, String), String> {
    let mut body = format!("<?xml version=\"1.0\"?>\r\n<s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\" s:encodingStyle=\"http://schemas.xmlsoap.org/soap/encoding/\"><s:Body><u:{action} xmlns:u=\"{service}\">");
    for (k, v) in args {
        body.push_str(&format!("<{k}>{v}</{k}>"));
    }
    body.push_str(&format!("</u:{action}></s:Body></s:Envelope>"));
    http(control, "POST", &[("Content-Type", "text/xml; charset=\"utf-8\"".into()), ("SOAPAction", format!("\"{service}#{action}\""))], &body)
}

/// The local address this PC reaches `host` from.
fn local_ip_to(host: &str) -> Option<Ipv4Addr> {
    let s = UdpSocket::bind(("0.0.0.0", 0)).ok()?;
    s.connect((host, 1900)).ok()?;
    match s.local_addr().ok()?.ip() {
        std::net::IpAddr::V4(v) => Some(v),
        _ => None,
    }
}

/// The address of this PC that faces the default route (what a UDP socket
/// "connected" towards the internet would use; nothing is sent).
pub fn default_route_ip() -> Option<Ipv4Addr> {
    local_ip_to("192.0.2.1")
}

/// AddPortMapping: Ok, or the router's (error code, description).
fn add(control_url: &str, service: &str, port: u16, local_ip: Ipv4Addr, desc: &str, lease: u32) -> Result<Result<(), (String, String)>, String> {
    let (st, body) = soap(control_url, service, "AddPortMapping", &[
        ("NewRemoteHost", String::new()),
        ("NewExternalPort", port.to_string()),
        ("NewProtocol", "UDP".into()),
        ("NewInternalPort", port.to_string()),
        ("NewInternalClient", local_ip.to_string()),
        ("NewEnabled", "1".into()),
        ("NewPortMappingDescription", desc.into()),
        ("NewLeaseDuration", lease.to_string()),
    ])?;
    if st == 200 {
        return Ok(Ok(()));
    }
    Ok(Err((tag(&body, "errorCode").unwrap_or("?").to_string(), tag(&body, "errorDescription").unwrap_or("").to_string())))
}

/// Ask the gateway at `location` to forward UDP `port` to this PC.
pub fn map_at(location: &str, port: u16, desc: &str) -> Result<Mapping, String> {
    let (st, description) = http(location, "GET", &[], "")?;
    if st != 200 {
        return Err(format!("the router's description answered HTTP {st}"));
    }
    let (service, control_url) = wan_service(location, &description).ok_or("the router has no WAN connection service")?;
    let (host, _, _) = split_url(&control_url).ok_or("bad control URL")?;
    let local_ip = local_ip_to(&host).ok_or("no local address towards the router")?;
    // a timed lease, renewed while the server runs: a crashed game's mapping
    // runs out by itself. Lease 0 (permanent) only for a router that says it
    // takes nothing else.
    let mut lease = crate::pcp::LIFETIME;
    let mut r = add(&control_url, &service, port, local_ip, desc, lease)?;
    if matches!(&r, Err((code, _)) if code == ONLY_PERMANENT) {
        lease = 0;
        r = add(&control_url, &service, port, local_ip, desc, lease)?;
    }
    if let Err((code, what)) = r {
        return Err(format!("the router refused the port mapping (UPnP error {code} {what})"));
    }
    let external_ip = soap(&control_url, &service, "GetExternalIPAddress", &[]).ok().and_then(|(s, b)| (s == 200).then(|| tag(&b, "NewExternalIPAddress").map(str::to_string)).flatten()).filter(|ip| !ip.is_empty());
    Ok(Mapping { control_url, service, port, local_ip, external_ip, lease, desc: desc.to_string() })
}

/// Renew a timed mapping (the same AddPortMapping again; the router
/// restarts its lease). Err: the router no longer takes it.
pub fn renew(m: &Mapping) -> Result<(), String> {
    match add(&m.control_url, &m.service, m.port, m.local_ip, &m.desc, m.lease)? {
        Ok(()) => Ok(()),
        Err((code, what)) => Err(format!("UPnP error {code} {what}")),
    }
}

/// Take a mapping back.
pub fn unmap(m: &Mapping) {
    let _ = soap(&m.control_url, &m.service, "DeletePortMapping", &[("NewRemoteHost", String::new()), ("NewExternalPort", m.port.to_string()), ("NewProtocol", "UDP".into())]);
}

/// Whether an IPv4 address is a private / carrier-grade NAT one: a router
/// that reports one is itself behind another router (or the ISP's CGNAT),
/// and the port mapping does not reach the internet.
pub fn is_private(ip: &str) -> bool {
    match ip.parse::<Ipv4Addr>() {
        Ok(a) => {
            let o = a.octets();
            a.is_private() || a.is_loopback() || a.is_link_local() || (o[0] == 100 && (64..128).contains(&o[1]))
        }
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    const DESC: &str = r#"<?xml version="1.0"?><root><device><deviceList><device><serviceList>
<service><serviceType>urn:schemas-upnp-org:service:WANCommonInterfaceConfig:1</serviceType><controlURL>/ctl/CmnIfCfg</controlURL></service>
<service><serviceType>urn:schemas-upnp-org:service:WANIPConnection:1</serviceType><controlURL>/ctl/IPConn</controlURL></service>
</serviceList></device></deviceList></device></root>"#;

    /// What the fake router does with AddPortMapping.
    #[derive(Clone, Copy, PartialEq)]
    pub(crate) enum Igd {
        /// takes any lease
        Takes,
        /// error 725 OnlyPermanentLeasesSupported for a timed lease
        OnlyPermanent,
        /// error 718 ConflictInMappingEntry for every request
        Conflict,
    }

    /// A fake router: serves the description, records the SOAP actions; its
    /// behaviour can change while it runs.
    pub(crate) fn fake_igd(mode: Igd) -> (String, Arc<Mutex<Vec<String>>>, Arc<Mutex<Igd>>) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let log = Arc::new(Mutex::new(Vec::new()));
        let log2 = log.clone();
        let mode = Arc::new(Mutex::new(mode));
        let mode2 = mode.clone();
        std::thread::spawn(move || {
            for s in l.incoming().flatten().take(64) {
                let mut s = s;
                let _ = s.set_read_timeout(Some(Duration::from_millis(500)));
                let mut buf = vec![0u8; 8192];
                let mut got = Vec::new();
                while let Ok(n) = s.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                    got.extend_from_slice(&buf[..n]);
                    let t = String::from_utf8_lossy(&got);
                    if let Some((h, b)) = t.split_once("\r\n\r\n") {
                        let len: usize = header(h, "content-length").and_then(|v| v.parse().ok()).unwrap_or(0);
                        if b.len() >= len {
                            break;
                        }
                    }
                }
                let t = String::from_utf8_lossy(&got).to_string();
                let (code, body) = if t.starts_with("GET") {
                    (200, DESC.to_string())
                } else {
                    let action = header(&t, "soapaction").unwrap_or_default();
                    log2.lock().unwrap().push(format!("{action} {}", t.split_once("\r\n\r\n").map(|x| x.1).unwrap_or("")));
                    let m = *mode2.lock().unwrap();
                    if action.contains("AddPortMapping") && m == Igd::OnlyPermanent && !t.contains("<NewLeaseDuration>0<") {
                        (500, "<errorCode>725</errorCode><errorDescription>OnlyPermanentLeasesSupported</errorDescription>".to_string())
                    } else if action.contains("AddPortMapping") && m == Igd::Conflict {
                        (500, "<errorCode>718</errorCode><errorDescription>ConflictInMappingEntry</errorDescription>".to_string())
                    } else if action.contains("GetExternalIPAddress") {
                        (200, "<NewExternalIPAddress>203.0.113.17</NewExternalIPAddress>".to_string())
                    } else {
                        (200, "<ok/>".to_string())
                    }
                };
                let _ = s.write_all(format!("HTTP/1.1 {code} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes());
            }
        });
        (format!("http://127.0.0.1:{port}/rootDesc.xml"), log, mode)
    }

    fn fake_router(mode: Igd) -> (String, Arc<Mutex<Vec<String>>>) {
        let (loc, log, _) = fake_igd(mode);
        (loc, log)
    }

    #[test]
    fn the_router_forwards_the_port_and_names_the_address() {
        let (loc, log) = fake_router(Igd::Takes);
        let m = map_at(&loc, 30720, "Tremulous").unwrap();
        assert_eq!(m.service, "urn:schemas-upnp-org:service:WANIPConnection:1");
        assert!(m.control_url.ends_with("/ctl/IPConn"), "{}", m.control_url);
        assert_eq!(m.external_ip.as_deref(), Some("203.0.113.17"));
        let l = log.lock().unwrap().clone();
        assert!(l[0].contains("AddPortMapping") && l[0].contains("<NewExternalPort>30720<") && l[0].contains("<NewProtocol>UDP<") && l[0].contains("<NewInternalClient>127.0.0.1<"), "{l:?}");
        unmap(&m);
        std::thread::sleep(Duration::from_millis(100));
        assert!(log.lock().unwrap().iter().any(|a| a.contains("DeletePortMapping")));
    }

    #[test]
    fn the_lease_is_two_hours_not_permanent() {
        let (loc, log) = fake_router(Igd::Takes);
        let m = map_at(&loc, 30720, "Tremulous").unwrap();
        assert_eq!(m.lease, 7200);
        let l = log.lock().unwrap().clone();
        let adds: Vec<&String> = l.iter().filter(|a| a.contains("AddPortMapping")).collect();
        assert_eq!(adds.len(), 1, "{l:?}");
        assert!(adds[0].contains("<NewLeaseDuration>7200<"), "{l:?}");
        // a renewal is the same request again
        renew(&m).unwrap();
        let l = log.lock().unwrap().clone();
        assert_eq!(l.iter().filter(|a| a.contains("AddPortMapping") && a.contains("<NewLeaseDuration>7200<") && a.contains("<NewPortMappingDescription>Tremulous<")).count(), 2, "{l:?}");
    }

    #[test]
    fn only_a_router_that_takes_permanent_leases_alone_gets_one() {
        let (loc, log) = fake_router(Igd::OnlyPermanent);
        let m = map_at(&loc, 30720, "Tremulous").unwrap();
        assert_eq!(m.lease, 0);
        let l = log.lock().unwrap().clone();
        let adds: Vec<&String> = l.iter().filter(|a| a.contains("AddPortMapping")).collect();
        assert!(adds.len() == 2 && adds[0].contains("<NewLeaseDuration>7200<") && adds[1].contains("<NewLeaseDuration>0<"), "{l:?}");
        // any other refusal: no permanent retry
        let (loc, log) = fake_router(Igd::Conflict);
        let e = map_at(&loc, 30720, "Tremulous").unwrap_err();
        assert!(e.contains("UPnP error 718"), "{e}");
        let l = log.lock().unwrap().clone();
        assert!(!l.iter().any(|a| a.contains("<NewLeaseDuration>0<")), "{l:?}");
    }

    #[test]
    fn the_keeper_renews_and_takes_the_mapping_back() {
        let (loc, log, _) = fake_igd(Igd::Takes);
        let m = map_at(&loc, 30720, "Tremulous").unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let k = crate::pcp::keep_upnp(m, Duration::from_millis(200), tx);
        std::thread::sleep(Duration::from_millis(700));
        let adds = log.lock().unwrap().iter().filter(|a| a.contains("AddPortMapping") && a.contains("<NewLeaseDuration>7200<")).count();
        assert!(adds >= 3, "first mapping + renewals: {adds}");
        k.stop(true);
        assert!(log.lock().unwrap().last().unwrap().contains("DeletePortMapping"));
        assert!(rx.try_recv().is_err(), "no news while it renews");
    }

    #[test]
    fn a_renewal_the_router_refuses_is_reported_and_still_deleted_on_stop() {
        let (loc, log, mode) = fake_igd(Igd::Takes);
        let m = map_at(&loc, 30720, "Tremulous").unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let k = crate::pcp::keep_upnp(m, Duration::from_millis(150), tx);
        *mode.lock().unwrap() = Igd::Conflict;
        assert_eq!(rx.recv_timeout(Duration::from_secs(3)).unwrap(), crate::pcp::Event::UpnpLost);
        k.stop(true);
        assert!(log.lock().unwrap().last().unwrap().contains("DeletePortMapping"));
    }

    #[test]
    fn a_permanent_mapping_is_not_renewed_but_taken_back() {
        let (loc, log, _) = fake_igd(Igd::OnlyPermanent);
        let m = map_at(&loc, 30720, "Tremulous").unwrap();
        let (tx, _rx) = std::sync::mpsc::channel();
        let k = crate::pcp::keep_upnp(m, Duration::from_millis(100), tx);
        std::thread::sleep(Duration::from_millis(400));
        assert_eq!(log.lock().unwrap().iter().filter(|a| a.contains("AddPortMapping")).count(), 2, "7200 refused, 0 taken, nothing after");
        // the game ends without stop(): the dropped channel takes it back too
        drop(k);
        std::thread::sleep(Duration::from_millis(300));
        assert!(log.lock().unwrap().last().unwrap().contains("DeletePortMapping"));
    }

    #[test]
    fn chunked_answers_and_nat_addresses() {
        assert_eq!(dechunk("5\r\nhello\r\n6\r\n world\r\n0\r\n\r\n"), "hello world");
        assert!(is_private("192.168.0.1") && is_private("100.72.1.1") && is_private("10.1.2.3"));
        assert!(!is_private("203.0.113.17"));
    }
}
