//! STUN Binding requests from the game socket (RFC 8489), for the host's
//! NAT check (dec076; the classification is trem_godot's natcheck.rs).
//!
//! A Binding request is a 20-byte header: type 0x0001, length 0, the magic
//! cookie 0x2112A442 and a 96-bit transaction id. The answer carries
//! XOR-MAPPED-ADDRESS: the address and port the server saw us from, which is
//! what the router made of the game socket. The answers arrive on the game
//! socket, so `Net::get_packet` takes out exactly the packets that answer a
//! transaction we sent (same server address, same transaction id, the
//! cookie) before the Q3 packet path. Q3 out-of-band packets start with
//! 0xffffffff (never a STUN type: its top two bits are 0); a netchan packet
//! has no fixed cookie, and the source and transaction id checks keep one
//! that happens to carry it in bytes 4-7 on the Q3 path.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

pub const MAGIC: u32 = 0x2112_A442;
pub const BINDING_REQUEST: u16 = 0x0001;
pub const BINDING_SUCCESS: u16 = 0x0101;
const ATTR_MAPPED_ADDRESS: u16 = 0x0001;
const ATTR_XOR_MAPPED_ADDRESS: u16 = 0x0020;
/// the default port of a STUN server (RFC 8489 section 18.6)
pub const PORT: u16 = 3478;

/// A Binding request with transaction id `txid` (no attributes).
pub fn binding_request(txid: &[u8; 12]) -> Vec<u8> {
    let mut d = Vec::with_capacity(20);
    d.extend_from_slice(&BINDING_REQUEST.to_be_bytes());
    d.extend_from_slice(&0u16.to_be_bytes());
    d.extend_from_slice(&MAGIC.to_be_bytes());
    d.extend_from_slice(txid);
    d
}

/// The header of a STUN message: its type and transaction id. None unless
/// the top two bits are 0, the cookie is in bytes 4-7 and the length field
/// matches the packet (a multiple of 4).
pub fn header(d: &[u8]) -> Option<(u16, [u8; 12])> {
    if d.len() < 20 || d[0] & 0xc0 != 0 {
        return None;
    }
    let len = u16::from_be_bytes([d[2], d[3]]) as usize;
    if len % 4 != 0 || len + 20 != d.len() || u32::from_be_bytes([d[4], d[5], d[6], d[7]]) != MAGIC {
        return None;
    }
    let mut id = [0u8; 12];
    id.copy_from_slice(&d[8..20]);
    Some((u16::from_be_bytes([d[0], d[1]]), id))
}

/// Does `d` from `from` answer one of our transactions (server, txid)?
pub fn answers(waiting: &[(SocketAddr, [u8; 12])], from: &SocketAddr, d: &[u8]) -> bool {
    match header(d) {
        Some((_, id)) => waiting.iter().any(|(a, t)| a == from && *t == id),
        None => false,
    }
}

/// The mapped address of a Binding success answer to `txid`:
/// XOR-MAPPED-ADDRESS, else the old MAPPED-ADDRESS (RFC 3489 servers).
pub fn mapped_address(d: &[u8], txid: &[u8; 12]) -> Option<SocketAddr> {
    let (ty, id) = header(d)?;
    if ty != BINDING_SUCCESS || id != *txid {
        return None;
    }
    let mut plain = None;
    let mut i = 20;
    while i + 4 <= d.len() {
        let at = u16::from_be_bytes([d[i], d[i + 1]]);
        let len = u16::from_be_bytes([d[i + 2], d[i + 3]]) as usize;
        let v = d.get(i + 4..i + 4 + len)?;
        match at {
            ATTR_XOR_MAPPED_ADDRESS => return address(v, Some(txid)),
            ATTR_MAPPED_ADDRESS => plain = address(v, None),
            _ => {}
        }
        // attributes are padded to 4 bytes
        i += 4 + len.div_ceil(4) * 4;
    }
    plain
}

/// A (XOR-)MAPPED-ADDRESS value: 0, family (1 ipv4, 2 ipv6), port, address;
/// XOR-ed with the cookie (and the transaction id for ipv6) when `xor`.
fn address(v: &[u8], xor: Option<&[u8; 12]>) -> Option<SocketAddr> {
    if v.len() < 4 {
        return None;
    }
    let mut key = [0u8; 16];
    if let Some(t) = xor {
        key[..4].copy_from_slice(&MAGIC.to_be_bytes());
        key[4..].copy_from_slice(t);
    }
    let port = u16::from_be_bytes([v[2] ^ key[0], v[3] ^ key[1]]);
    match v[1] {
        1 if v.len() >= 8 => {
            let o: Vec<u8> = (0..4).map(|k| v[4 + k] ^ key[k]).collect();
            Some(SocketAddr::new(IpAddr::V4(Ipv4Addr::new(o[0], o[1], o[2], o[3])), port))
        }
        2 if v.len() >= 20 => {
            let mut o = [0u8; 16];
            for k in 0..16 {
                o[k] = v[4 + k] ^ key[k];
            }
            Some(SocketAddr::new(IpAddr::V6(Ipv6Addr::from(o)), port))
        }
        _ => None,
    }
}

/// A Binding success answer (what a STUN server sends; the tests' fake servers).
pub fn binding_success(txid: &[u8; 12], mapped: SocketAddr) -> Vec<u8> {
    let mut key = [0u8; 16];
    key[..4].copy_from_slice(&MAGIC.to_be_bytes());
    key[4..].copy_from_slice(txid);
    let mut v = vec![0u8, if mapped.is_ipv4() { 1 } else { 2 }];
    let p = mapped.port().to_be_bytes();
    v.extend_from_slice(&[p[0] ^ key[0], p[1] ^ key[1]]);
    let o: Vec<u8> = match mapped.ip() {
        IpAddr::V4(a) => a.octets().to_vec(),
        IpAddr::V6(a) => a.octets().to_vec(),
    };
    v.extend(o.iter().enumerate().map(|(k, b)| b ^ key[k]));
    let mut d = Vec::new();
    d.extend_from_slice(&BINDING_SUCCESS.to_be_bytes());
    d.extend_from_slice(&((4 + v.len()) as u16).to_be_bytes());
    d.extend_from_slice(&MAGIC.to_be_bytes());
    d.extend_from_slice(txid);
    d.extend_from_slice(&ATTR_XOR_MAPPED_ADDRESS.to_be_bytes());
    d.extend_from_slice(&(v.len() as u16).to_be_bytes());
    d.extend_from_slice(&v);
    d
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::netchan::{Net, NetAdr, NetSrc};
    use std::net::UdpSocket;

    const T: [u8; 12] = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12];

    #[test]
    fn the_request_is_the_rfc_8489_header() {
        let r = binding_request(&T);
        assert_eq!(r.len(), 20);
        assert_eq!(&r[..8], &[0x00, 0x01, 0x00, 0x00, 0x21, 0x12, 0xa4, 0x42]);
        assert_eq!(&r[8..], &T);
        assert_eq!(header(&r), Some((BINDING_REQUEST, T)));
    }

    #[test]
    fn xor_mapped_address_both_families() {
        for m in ["203.0.113.7:40001", "[2001:db8::7]:40002"] {
            let m: SocketAddr = m.parse().unwrap();
            let d = binding_success(&T, m);
            assert_eq!(mapped_address(&d, &T), Some(m));
            // another transaction's answer is not ours
            assert_eq!(mapped_address(&d, &[0; 12]), None);
        }
        // the sample IPv4 response of RFC 5769 section 2.2: 192.0.2.1:32853
        let mut d = vec![0x01, 0x01, 0x00, 0x0c, 0x21, 0x12, 0xa4, 0x42];
        let id = [0xb7, 0xe7, 0xa7, 0x01, 0xbc, 0x34, 0xd6, 0x86, 0xfa, 0x87, 0xdf, 0xae];
        d.extend_from_slice(&id);
        d.extend_from_slice(&[0x00, 0x20, 0x00, 0x08, 0x00, 0x01, 0xa1, 0x47, 0xe1, 0x12, 0xa6, 0x43]);
        assert_eq!(mapped_address(&d, &id), Some("192.0.2.1:32853".parse().unwrap()));
        // an old server's MAPPED-ADDRESS, after an unknown attribute
        let mut d = vec![0x01, 0x01, 0x00, 0x14, 0x21, 0x12, 0xa4, 0x42];
        d.extend_from_slice(&T);
        d.extend_from_slice(&[0x80, 0x22, 0x00, 0x01, b'x', 0, 0, 0]);
        d.extend_from_slice(&[0x00, 0x01, 0x00, 0x08, 0x00, 0x01, 0x9c, 0x41, 198, 51, 100, 9]);
        assert_eq!(mapped_address(&d, &T), Some("198.51.100.9:40001".parse().unwrap()));
    }

    #[test]
    fn q3_packets_are_never_taken_for_stun() {
        let srv: SocketAddr = "192.0.2.50:3478".parse().unwrap();
        let w = vec![(srv, T)];
        // an out-of-band packet
        let mut oob = vec![0xff; 4];
        oob.extend_from_slice(b"getinfo xxxxxxxxxxxxxxxxxxxx");
        assert!(header(&oob).is_none() && !answers(&w, &srv, &oob));
        // a netchan packet that carries the cookie in bytes 4-7 and a fitting length
        let mut nc = vec![0x00, 0x00, 0x00, 0x04, 0x21, 0x12, 0xa4, 0x42];
        nc.extend_from_slice(&[9; 12]);
        nc.extend_from_slice(&[0; 4]);
        assert!(header(&nc).is_some(), "shaped like STUN");
        assert!(!answers(&w, &srv, &nc), "but no transaction of ours");
        // our answer from another address is not taken either
        let ok = binding_success(&T, "203.0.113.7:1".parse().unwrap());
        assert!(answers(&w, &srv, &ok));
        assert!(!answers(&w, &"192.0.2.51:3478".parse().unwrap(), &ok));
        assert!(!answers(&[], &srv, &ok));
    }

    #[test]
    fn answers_leave_the_q3_path_and_q3_packets_stay_on_it() {
        let mut net = Net::new();
        let port = net.open(NetSrc::Server, "127.0.0.1", 0).unwrap();
        let fake = UdpSocket::bind("127.0.0.1:0").unwrap();
        fake.set_read_timeout(Some(std::time::Duration::from_secs(2))).unwrap();
        let fa = fake.local_addr().unwrap();
        net.stun_send(NetSrc::Server, fa, T);
        let mut b = [0u8; 64];
        let (n, from) = fake.recv_from(&mut b).unwrap();
        assert_eq!(from.port(), port, "sent from the game socket");
        assert_eq!(header(&b[..n]), Some((BINDING_REQUEST, T)));
        // the answer, then an OOB packet from the same address, then a stray STUN-shaped one
        fake.send_to(&binding_success(&T, "203.0.113.7:40001".parse().unwrap()), from).unwrap();
        fake.send_to(b"\xff\xff\xff\xffgetstatus", from).unwrap();
        fake.send_to(&binding_success(&[7; 12], "203.0.113.7:1".parse().unwrap()), from).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(50));
        let mut q3 = Vec::new();
        while let Some((a, d)) = net.get_packet(NetSrc::Server) {
            q3.push((a, d));
        }
        assert_eq!(q3.len(), 2, "the OOB packet and the stray one: {q3:?}");
        assert_eq!(q3[0].0, NetAdr::from_socket(&fa));
        assert_eq!(&q3[0].1, b"\xff\xff\xff\xffgetstatus");
        let r = net.take_stun_replies();
        assert_eq!(r.len(), 1);
        assert_eq!(mapped_address(&r[0].1, &T), Some("203.0.113.7:40001".parse().unwrap()));
        // once the check is over nothing more is taken
        net.stun_done();
        fake.send_to(&binding_success(&T, "203.0.113.7:40001".parse().unwrap()), from).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert!(net.get_packet(NetSrc::Server).is_some());
    }
}
