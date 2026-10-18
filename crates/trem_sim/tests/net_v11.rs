//! Vanilla Tremulous 1.1.0 (protocol 69) support (dec040, dec041).
//!
//! * `msg69_oracle_stream`: 1.1.0's msg.c deltas (entityState,
//!   playerState, keyed usercmds), its netchan headers and
//!   CL_Netchan_Encode / CL_Netchan_Decode, produced with the port; the
//!   SHA-256 of the hex lines must equal re/traces/msg69.sha256, the output
//!   of re/harness/msg69_ref.c (re/source11's msg.c, huffman.c, net_chan.c
//!   and cl_net_chan.c's coders compiled natively).
//! * round trips of the 1.1 deltas.
//! * end to end against a real 1.1.0 tremded (#[ignore]d: needs
//!   TREM11_DED / TREM11_BASE, and fails without them): handshake, gamestate, Active, snapshots with
//!   entities, a playerState that moves after `team humans` / `class
//!   rifle`, a clean disconnect in the server's log; Auto picks 69 for it.
//! * Auto picks 71 for the port's own server.
use sha2::{Digest, Sha256};
use std::net::UdpSocket;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use trem_sim::kernel::cl::{ConnState, NetClient, NET_PROTOCOL_AUTO};
use trem_sim::kernel::msg::*;
use trem_sim::kernel::netchan::*;
use trem_sim::kernel::types::UserCmd;
use trem_sim::kernel::v11::{self, PlayerState11};
use trem_sim::kernel::v11net::{self, ENTITY_STATE_FIELDS_11, PLAYER_STATE_FIELDS_11};

#[test]
fn protocol_number() {
    assert_eq!(v11::PROTOCOL_VANILLA, 69);
}

// ===================================================================== oracle

/// The generator of re/harness/msg69_ref.c (the one of net_ref.c / net.rs).
struct Lcg(u32);
impl Lcg {
    fn next(&mut self) -> u32 {
        self.0 = self.0.wrapping_mul(1103515245).wrapping_add(12345);
        self.0 >> 8
    }
    fn next2(&mut self) -> u32 {
        let a = self.next();
        let b = self.next();
        a ^ (b << 8)
    }
    fn word(&mut self, bits: i32) -> i32 {
        let r = self.next() % 10;
        if r < 4 {
            return 0;
        }
        if bits == 0 {
            let f = if r < 7 { ((self.next() % 8000) as i32 - 4000) as f32 } else { ((self.next() % 2000000) as i32 - 1000000) as f32 / 64.0 };
            return f.to_bits() as i32;
        }
        let ab = bits.unsigned_abs();
        let mut v = if r < 7 { self.next() % 256 } else { self.next2() };
        if ab < 32 {
            v &= (1u32 << ab) - 1;
        }
        v as i32
    }
}

fn set<T>(s: &mut T, off: usize, v: i32) {
    unsafe { std::ptr::write((s as *mut T as *mut u8).add(off) as *mut i32, v) }
}

fn gen_es(r: &mut Lcg) -> EntityState {
    let mut es = EntityState { number: (r.next() % (MAX_GENTITIES as u32 - 1)) as i32, ..Default::default() };
    for f in ENTITY_STATE_FIELDS_11.iter() {
        let v = r.word(f.bits);
        set(&mut es, f.offset, v);
    }
    es
}

fn gen_ps(r: &mut Lcg) -> PlayerState11 {
    let mut ps = PlayerState11::default();
    for f in PLAYER_STATE_FIELDS_11.iter() {
        let v = r.word(f.bits);
        set(&mut ps, f.offset, v);
    }
    for i in 0..16 {
        ps.stats[i] = r.word(16) as i16 as i32;
    }
    for i in 0..16 {
        ps.persistant[i] = r.word(16) as i16 as i32;
    }
    for i in 0..16 {
        ps.ammo[i] = r.word(16) as i16 as i32;
    }
    for i in 0..16 {
        ps.powerups[i] = r.word(32);
    }
    ps
}

fn gen_cmd(r: &mut Lcg, base: i32) -> UserCmd {
    let mut c = UserCmd { server_time: base + (r.next() % 600) as i32, ..Default::default() };
    c.angles[0] = (r.next() % 65536) as i32;
    c.angles[1] = (r.next() % 65536) as i32;
    c.angles[2] = if r.next() % 4 != 0 { 0 } else { (r.next() % 65536) as i32 };
    c.buttons = if r.next() % 4 != 0 { 0 } else { (r.next() % 65536) as i32 };
    c.weapon = (r.next() % 26) as u8;
    c.forwardmove = if r.next() % 3 != 0 { 127 } else { ((r.next() % 256) as i32 - 128) as i8 };
    c.rightmove = ((r.next() % 256) as i32 - 128) as i8;
    c.upmove = if r.next() % 2 != 0 { 0 } else { 127 };
    c
}

/// A command text of the generator: printable chars, '%' and U+00C0..
/// U+00FF (two UTF-8 bytes of 0x80+, as msg69_ref.c writes them).
fn gen_text(r: &mut Lcg) -> String {
    let n = r.next() % 40;
    (0..n)
        .map(|_| match r.next() % 8 {
            0 => '%',
            1 => char::from(0xc0u8 + (r.next() % 0x40) as u8),
            _ => char::from(b' ' + (r.next() % 95) as u8),
        })
        .collect()
}

fn hex(out: &mut String, tag: &str, d: &[u8]) {
    out.push_str(&format!("{tag} {} ", d.len()));
    for b in d {
        out.push_str(&format!("{b:02x}"));
    }
    out.push('\n');
}

/// The port's rendition of re/harness/msg69_ref.c.
fn oracle_text() -> String {
    let mut out = String::new();
    let mut r = Lcg(6969);
    // 1. keyed usercmds with Com_HashKey keys
    let mut m = Msg::new(MAX_MSGLEN);
    let mut a = UserCmd::default();
    for _ in 0..300 {
        let t = gen_text(&mut r);
        let key = (r.next2() as i32) ^ v11net::com_hash_key(t.as_bytes(), 32);
        let mut b = gen_cmd(&mut r, a.server_time);
        if r.next() % 4 == 0 {
            b = a;
            b.server_time += 16;
        }
        m.write_delta_usercmd_key(key, &a, &b);
        a = b;
    }
    hex(&mut out, "cmd", m.bytes());
    // 2. entities
    for _ in 0..40 {
        let mut m = Msg::new(MAX_MSGLEN);
        for _ in 0..20 {
            let a = gen_es(&mut r);
            let mut b = gen_es(&mut r);
            b.number = a.number;
            match r.next() % 4 {
                0 => m.write_delta_entity11(Some(&a), Some(&b), false),
                1 => m.write_delta_entity11(Some(&a), Some(&a), true),
                2 => m.write_delta_entity11(Some(&a), None, true),
                _ => m.write_delta_entity11(Some(&EntityState::default()), Some(&b), true),
            }
        }
        hex(&mut out, "ent", m.bytes());
    }
    // 3. playerstates
    for _ in 0..40 {
        let mut m = Msg::new(MAX_MSGLEN);
        let a = gen_ps(&mut r);
        let b = gen_ps(&mut r);
        if r.next() % 3 == 0 {
            m.write_delta_playerstate11(None, &b);
        } else {
            m.write_delta_playerstate11(Some(&a), &b);
        }
        hex(&mut out, "ps", m.bytes());
    }
    // 4. CL_Netchan_Encode of client messages, CL_Netchan_Decode of server ones
    let mut cmds = vec![String::new(); 64];
    for _ in 0..30 {
        for c in cmds.iter_mut() {
            *c = gen_text(&mut r);
        }
        let challenge = r.next2() as i32;
        // client message: serverId, messageAcknowledge, reliableAcknowledge, payload
        let mut m = Msg::new(MAX_MSGLEN);
        m.write_long(r.next2() as i32);
        m.write_long(r.next2() as i32);
        m.write_long((r.next() % 1000) as i32);
        let n = r.next() % 300;
        for _ in 0..n {
            m.write_byte((r.next() % 256) as i32);
        }
        v11net::cl_netchan_encode(&mut m, challenge, &cmds);
        hex(&mut out, "enc", m.bytes());
        // server message after Netchan_Process: sequence, then reliableAcknowledge, payload
        let seq = (r.next() % 100000) as i32;
        let mut body = Msg::new(MAX_MSGLEN);
        body.write_long((r.next() % 1000) as i32);
        let n = r.next() % 300;
        for _ in 0..n {
            body.write_byte((r.next() % 256) as i32);
        }
        let mut d = seq.to_le_bytes().to_vec();
        d.extend_from_slice(body.bytes());
        let mut m = Msg::from_bytes(&d);
        m.begin_reading_oob();
        m.read_long();
        v11net::cl_netchan_decode(&mut m, challenge, &cmds);
        hex(&mut out, "dec", m.bytes());
    }
    // 5. netchan without the challenge checksum
    let mut net = Net::new();
    net.capture = Some(Vec::new());
    let adr = NetAdr { kind: NetAdrType::Ip, ..Default::default() };
    let mut chan = Netchan::setup_protocol(NetSrc::Client, adr, 4242, -1234567, 69);
    for len in [0usize, 100, 1299, 1300, 2600, 4000] {
        let data: Vec<u8> = (0..len).map(|_| r.next() as u8).collect();
        chan.transmit(&mut net, &data, 0);
        while chan.unsent_fragments {
            chan.transmit_next_fragment(&mut net, 0);
        }
    }
    for (_, p) in net.capture.take().unwrap() {
        hex(&mut out, "pkt", &p);
    }
    out
}

#[test]
fn msg69_oracle_stream() {
    let text = oracle_text();
    if let Ok(p) = std::env::var("MSG69_REF_OUT") {
        std::fs::write(p, &text).unwrap();
    }
    let got = format!("{:x}", Sha256::digest(text.as_bytes()));
    let want = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../re/traces/msg69.sha256")).unwrap();
    assert_eq!(got, want.split_whitespace().next().unwrap(), "1.1 msg/netchan stream differs from re/harness/msg69_ref (MSG69_REF_OUT=path to dump)");
}

#[test]
fn deltas_round_trip() {
    let mut r = Lcg(42);
    for _ in 0..200 {
        let a = gen_es(&mut r);
        let mut b = gen_es(&mut r);
        b.number = a.number;
        let mut m = Msg::new(MAX_MSGLEN);
        m.write_delta_entity11(Some(&a), Some(&b), true);
        let mut rd = Msg::from_bytes(m.bytes());
        rd.begin_reading();
        let n = rd.read_bits(GENTITYNUM_BITS);
        assert_eq!(n, a.number);
        let got = rd.read_delta_entity11(&a, n);
        assert_eq!(got.misc, b.misc, "powerups ride in misc");
        assert_eq!(got.weapon_anim, a.weapon_anim, "1.1 sends no weaponAnim");
        let mut want = b;
        want.weapon_anim = a.weapon_anim;
        assert_eq!(got, want);
        let pa = gen_ps(&mut r);
        let pb = gen_ps(&mut r);
        let mut m = Msg::new(MAX_MSGLEN);
        m.write_delta_playerstate11(Some(&pa), &pb);
        let mut rd = Msg::from_bytes(m.bytes());
        rd.begin_reading();
        let got = rd.read_delta_playerstate11(Some(&pa));
        // signed 16 bit fields come back sign-extended, the others as sent
        let mut want = pb;
        for f in PLAYER_STATE_FIELDS_11.iter() {
            let v = unsafe { std::ptr::read((&pb as *const PlayerState11 as *const u8).add(f.offset) as *const i32) };
            let old = unsafe { std::ptr::read((&pa as *const PlayerState11 as *const u8).add(f.offset) as *const i32) };
            if old != v && f.bits < 0 && v & (1 << (-f.bits - 1)) != 0 {
                set(&mut want, f.offset, v | (-1i32 << -f.bits));
            }
        }
        for i in 0..16 {
            // (the generator's shorts are already sign-extended)
            want.stats[i] = pb.stats[i] as i16 as i32;
            want.persistant[i] = pb.persistant[i] as i16 as i32;
            want.ammo[i] = pb.ammo[i] as i16 as i32;
        }
        assert_eq!(got, want);
    }
}

#[test]
fn netchan_coders_round_trip() {
    // CL_Netchan_Encode is its own inverse with the same key material
    let cmds: Vec<String> = (0..64).map(|i| format!("cmd {i} 100%")).collect();
    let mut m = Msg::new(MAX_MSGLEN);
    m.write_long(7);
    m.write_long(8);
    m.write_long(9);
    m.write_string("hello there");
    let plain = m.bytes().to_vec();
    v11net::cl_netchan_encode(&mut m, 12345, &cmds);
    assert_ne!(m.bytes(), &plain[..]);
    // the header longs are left alone (12 bytes or fewer of Huffman output)
    let mut d = m.bytes().to_vec();
    let mut again = Msg::from_bytes(&d);
    v11net::cl_netchan_encode(&mut again, 12345, &cmds);
    d.copy_from_slice(again.bytes());
    assert_eq!(d, plain);
}

#[test]
fn hash_keys_differ_from_legacy_on_high_chars() {
    assert_eq!(v11net::com_hash_key(b"print \"hi\"", 32), hash_key("print \"hi\"", 32));
    assert_ne!(v11net::com_hash_key(b"\xe9t\xe9", 32), hash_key("\u{e9}t\u{e9}", 32));
}

// ===================================================================== end to end

/// The real 1.1.0 dedicated server: TREM11_DED (the tremded binary of a 1.1.0
/// build), its fs_basepath TREM11_BASE (base/ holding data-1.1.0.pk3, the maps
/// and the 1.1 game). The four tests that need it are #[ignore]d; run them with
/// --ignored. With the variables unset or wrong they FAIL and say so: they are
/// never skipped quietly.
fn ded_paths() -> Option<(PathBuf, PathBuf)> {
    let var = |k: &str| std::env::var(k).unwrap_or_else(|_| panic!("set {k}: these tests need a real Tremulous 1.1.0 tremded (TREM11_DED=<tremded binary>, TREM11_BASE=<fs_basepath holding base/>)"));
    let (bin, base) = (PathBuf::from(var("TREM11_DED")), PathBuf::from(var("TREM11_BASE")));
    assert!(bin.exists(), "TREM11_DED={} does not exist", bin.display());
    assert!(base.join("base").exists(), "TREM11_BASE={} has no base/ folder", base.display());
    Some((bin, base))
}

struct Ded {
    child: Child,
    log: PathBuf,
    port: u16,
}

impl Drop for Ded {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Ded {
    fn start(port: u16, pure: bool) -> Option<Ded> {
        Self::start_with(port, pure, &[])
    }

    fn start_with(port: u16, pure: bool, extra: &[&str]) -> Option<Ded> {
        let (bin, base) = ded_paths()?;
        let log = std::env::temp_dir().join(format!("trem11-ded-{port}.log"));
        let f = std::fs::File::create(&log).unwrap();
        let home = Self::home(port);
        let _ = std::fs::remove_dir_all(&home);
        if pure {
            // 1.1.0's SV_VerifyPaks_f expects the cgame and ui the server
            // finds in a pak (vm/cgame.qvm, vm/ui.qvm; vms-1.1.0.pk3 in a
            // full install); this server runs its game from the dll, so a
            // stand-in pak with those two names will do
            std::fs::create_dir_all(home.join("base")).unwrap();
            let mut z = zip::ZipWriter::new(std::fs::File::create(home.join("base/vms-standin.pk3")).unwrap());
            for n in ["vm/cgame.qvm", "vm/ui.qvm"] {
                z.start_file(n, zip::write::SimpleFileOptions::default()).unwrap();
                std::io::Write::write_all(&mut z, format!("stand-in {n}").as_bytes()).unwrap();
            }
            z.finish().unwrap();
        }
        let child = Command::new(bin)
            .args(["+set", "fs_basepath"])
            .arg(&base)
            .args(["+set", "fs_homepath"])
            .arg(&home)
            .args(extra)
            // dedicated 1 lets the 1.1.0 client exe (Windows has no tremded)
            // serve; its console goes to a window there, so also log to
            // fs_homepath/base/qconsole.log
            .args(["+set", "dedicated", "1", "+set", "logfile", "2"])
            .args(["+set", "vm_game", "0", "+set", "sv_pure", if pure { "1" } else { "0" }, "+set", "net_port", &port.to_string(), "+set", "net_ip", "127.0.0.1", "+set", "sv_hostname", "vanilla", "+map", "atcs"])
            .stdin(Stdio::null())
            .stdout(f.try_clone().unwrap())
            .stderr(f)
            .spawn()
            .ok()?;
        let d = Ded { child, log, port };
        // wait for its infoResponse
        let s = UdpSocket::bind("127.0.0.1:0").unwrap();
        s.set_read_timeout(Some(Duration::from_millis(200))).unwrap();
        for _ in 0..100 {
            let _ = s.send_to(b"\xff\xff\xff\xffgetinfo xxx", ("127.0.0.1", port));
            let mut buf = [0u8; 1400];
            match s.recv_from(&mut buf) {
                Ok((n, _)) if String::from_utf8_lossy(&buf[..n]).contains("infoResponse") => return Some(d),
                Ok(_) => {}
                // Windows reports the ICMP port-unreachable at once
                // (WSAECONNRESET) instead of timing out: wait out the boot
                Err(_) => std::thread::sleep(Duration::from_millis(200)),
            }
        }
        panic!("the 1.1.0 server on {port} did not answer: {}", std::fs::read_to_string(&d.log).unwrap_or_default());
    }

    fn home(port: u16) -> PathBuf {
        std::env::temp_dir().join(format!("trem11-home-{port}"))
    }

    fn log(&self) -> String {
        let mut s = std::fs::read_to_string(&self.log).unwrap_or_default();
        s.push_str(&std::fs::read_to_string(Self::home(self.port).join("base/qconsole.log")).unwrap_or_default());
        s
    }
}

/// A client stepping in real time; its server commands are run through
/// `get_server_command` like the cgame does.
struct Peer {
    cl: NetClient,
    net: Net,
    last: Instant,
    executed: i32,
    big: String,
    commands: Vec<Vec<String>>,
}

impl Peer {
    fn new() -> Peer {
        let mut net = Net::new();
        net.open(NetSrc::Client, "127.0.0.1", 0).unwrap();
        let mut cl = NetClient::new(0);
        cl.userinfo = "\\name\\PortProbe\\rate\\25000\\snaps\\20".into();
        Peer { cl, net, last: Instant::now(), executed: 0, big: String::new(), commands: Vec::new() }
    }

    fn step(&mut self, cmd: Option<UserCmd>) {
        std::thread::sleep(Duration::from_millis(8));
        let now = Instant::now();
        let msec = (now - self.last).as_millis() as i32;
        if msec == 0 {
            return;
        }
        self.last = now;
        self.cl.frame(&mut self.net, msec);
        if self.cl.gamestate_new {
            self.cl.cgame_loaded(&mut self.net);
        }
        if self.cl.state == ConnState::Active {
            while self.executed < self.cl.server_command_sequence {
                self.executed += 1;
                if let Some(a) = self.cl.get_server_command(self.executed, &mut self.big) {
                    self.commands.push(a);
                }
                while let Some(a) = self.cl.take_pending_server_command() {
                    self.commands.push(a);
                }
            }
        }
        if let Some(c) = cmd {
            self.cl.create_cmd(c);
        }
        self.cl.send_cmd(&mut self.net);
    }

    fn run_until(&mut self, secs: f32, cmd: UserCmd, mut done: impl FnMut(&NetClient) -> bool) -> bool {
        let t0 = Instant::now();
        while t0.elapsed().as_secs_f32() < secs {
            self.step(Some(cmd));
            if let Some(e) = self.cl.error.as_ref() {
                panic!("dropped: {e}\n{}", self.cl.print_out);
            }
            if done(&self.cl) {
                return true;
            }
        }
        false
    }
}

#[test]
#[ignore = "needs a real 1.1.0 tremded: set TREM11_DED and TREM11_BASE; run with --ignored"]
fn joins_a_vanilla_server_and_moves() {
    let ded = Ded::start(30791, false).unwrap();
    let mut p = Peer::new();
    p.cl.connect_protocol(&mut p.net, &format!("127.0.0.1:{}", ded.port), NetAdrType::Unspec, v11::PROTOCOL_VANILLA).unwrap();
    assert!(p.cl.vanilla());
    assert!(p.run_until(15.0, UserCmd::default(), |c| c.state == ConnState::Active), "no Active state: {:?}\n{}\n{}", p.cl.state, p.cl.print_out, ded.log());
    assert!(p.cl.netchan.protocol == 69);
    let si = &p.cl.configstrings11[0];
    assert!(si.contains("\\protocol\\69") && si.contains("\\mapname\\atcs"), "serverinfo {si}");
    assert!(p.run_until(3.0, UserCmd::default(), |c| c.snap.num_entities > 0), "snapshots without entities");
    // join the humans and spawn with a rifle (1.1.0's g_cmds.c: "team humans", "class rifle")
    p.cl.add_reliable_command("team humans", false).unwrap();
    assert!(p.run_until(5.0, UserCmd::default(), |c| c.snap.ps11.stats[8] == 2), "not on the humans: pteam {}", p.cl.snap.ps11.stats[8]);
    p.cl.add_reliable_command("class rifle", false).unwrap();
    let spawned = p.run_until(20.0, UserCmd::default(), |c| c.snap.ps11.pm_type == 0 && c.snap.ps11.stats[0] > 0);
    assert!(spawned, "never spawned: pm_type {} health {}\n{}", p.cl.snap.ps11.pm_type, p.cl.snap.ps11.stats[0], ded.log());
    let start = p.cl.snap.ps11.origin;
    let fwd = UserCmd { forwardmove: 127, ..Default::default() };
    let moved = p.run_until(4.0, fwd, |c| {
        let o = c.snap.ps11.origin;
        ((o[0] - start[0]).powi(2) + (o[1] - start[1]).powi(2)).sqrt() > 64.0
    });
    assert!(moved, "the playerState did not move: {start:?} -> {:?}", p.cl.snap.ps11.origin);
    eprintln!("spawned at {start:?}, walked to {:?}; {} entities in the snapshot; {} server commands", p.cl.snap.ps11.origin, p.cl.snap.num_entities, p.commands.len());
    // the legacy form the cgame reads follows it
    assert_eq!(p.cl.snap.ps.origin, p.cl.snap.ps11.origin);
    assert!(p.cl.get_snapshot(p.cl.snap.message_num).is_some_and(|s| !s.entities.is_empty()));
    assert!(!p.commands.is_empty(), "server commands reached the cgame");
    // a clean disconnect
    p.cl.disconnect(&mut p.net);
    std::thread::sleep(Duration::from_millis(600));
    let log = ded.log();
    assert!(log.contains("ClientDisconnect: 0"), "no disconnect in the server log:\n{log}");
    eprintln!("--- server log ---\n{log}");
}

#[test]
#[ignore = "needs a real 1.1.0 tremded: set TREM11_DED and TREM11_BASE; run with --ignored"]
fn auto_picks_vanilla_for_a_vanilla_server() {
    let ded = Ded::start(30792, false).unwrap();
    let mut p = Peer::new();
    assert_eq!(p.cl.net_protocol, NET_PROTOCOL_AUTO);
    p.cl.connect(&mut p.net, &format!("127.0.0.1:{}", ded.port)).unwrap();
    assert!(!p.cl.vanilla(), "Auto starts by asking");
    assert!(p.run_until(15.0, UserCmd::default(), |c| c.state == ConnState::Active), "Auto did not connect: {}", p.cl.print_out);
    assert!(p.cl.vanilla(), "Auto picked 69");
    p.cl.disconnect(&mut p.net);
}

#[test]
fn auto_picks_legacy_for_a_port_server() {
    use trem_sim::gen::maps::MapId;
    use trem_sim::kernel::sv::{Server, ServerCvars};
    use trem_sim::kernel::world::World;
    let mut snet = Net::new();
    let sport = snet.open(NetSrc::Server, "127.0.0.1", 0).unwrap();
    let mut sv = Server::spawn(World::new(MapId::Arena), "arena", ServerCvars::default());
    let mut cnet = Net::new();
    cnet.open(NetSrc::Client, "127.0.0.1", 0).unwrap();
    let mut cl = NetClient::new(0);
    cl.userinfo = "\\name\\Auto\\cl_guid\\0123456789ABCDEF0123456789ABCDEF".into();
    cl.connect(&mut cnet, &format!("127.0.0.1:{sport}")).unwrap();
    for _ in 0..600 {
        sv.frame(&mut snet, 16);
        cl.frame(&mut cnet, 16);
        if cl.gamestate_new {
            cl.cgame_loaded(&mut cnet);
        }
        cl.create_cmd(UserCmd::default());
        cl.send_cmd(&mut cnet);
        std::thread::sleep(Duration::from_micros(300));
        if cl.state == ConnState::Active {
            break;
        }
    }
    assert_eq!(cl.state, ConnState::Active, "{}\n{}", cl.print_out, sv.console);
    assert_eq!(cl.protocol, 71);
    assert!(cl.print_out.contains("Server protocol 71"), "{}", cl.print_out);
}

#[test]
fn auto_falls_back_to_legacy_without_an_answer() {
    // a port nobody answers on: no getinfo reply within PROTOCOL_PROBE_TIME
    let quiet = UdpSocket::bind("127.0.0.1:0").unwrap();
    let mut net = Net::new();
    net.open(NetSrc::Client, "127.0.0.1", 0).unwrap();
    let mut cl = NetClient::new(0);
    cl.connect(&mut net, &quiet.local_addr().unwrap().to_string()).unwrap();
    quiet.set_read_timeout(Some(Duration::from_millis(300))).unwrap();
    let mut buf = [0u8; 1400];
    let (n, _) = quiet.recv_from(&mut buf).unwrap();
    assert_eq!(&buf[4..n], b"getinfo xxx");
    for _ in 0..70 {
        cl.frame(&mut net, 16);
    }
    assert_eq!(cl.protocol, 71);
    let (n, _) = quiet.recv_from(&mut buf).unwrap();
    let text = String::from_utf8_lossy(&buf[4..n]).to_string();
    assert!(text.starts_with("getchallenge ") && text.ends_with(" Tremulous"), "{text}");
}

#[test]
fn connect_flags() {
    use trem_sim::kernel::nethost::parse_connect_args;
    let a = |v: &[&str]| parse_connect_args(&v.iter().map(|s| s.to_string()).collect::<Vec<_>>());
    assert_eq!(a(&["1.2.3.4"]), Some((NetAdrType::Unspec, "1.2.3.4".into(), 0)));
    assert_eq!(a(&["1.2.3.4", "-1"]), Some((NetAdrType::Unspec, "1.2.3.4".into(), 69)));
    assert_eq!(a(&["-4", "h", "-69"]), Some((NetAdrType::Ip, "h".into(), 69)));
    assert_eq!(a(&["h", "-71"]), Some((NetAdrType::Unspec, "h".into(), 71)));
    assert_eq!(a(&[]), None);
}

#[test]
#[ignore = "needs a real 1.1.0 tremded: set TREM11_DED and TREM11_BASE; run with --ignored"]
fn joins_a_pure_vanilla_server() {
    let ded = Ded::start(30793, true).unwrap();
    let (_, base) = ded_paths().unwrap();
    let mut p = Peer::new();
    // the client's search path holds the 1.1 paks the server runs on
    p.cl.fs = Some(trem_sim::kernel::files::PakSet::new(&[(base.join("base"), "base".into()), (Ded::home(ded.port).join("base"), "base".into())]));
    p.cl.connect_protocol(&mut p.net, &format!("127.0.0.1:{}", ded.port), NetAdrType::Unspec, v11::PROTOCOL_VANILLA).unwrap();
    assert!(p.run_until(15.0, UserCmd::default(), |c| c.state == ConnState::Active), "no Active state: {:?}\n{}\n{}", p.cl.state, p.cl.print_out, ded.log());
    assert!(p.cl.connected_to_pure, "sv_pure 1 in the systeminfo");
    // SV_VerifyPaks_f would drop an unpure client within a few seconds
    p.run_until(3.0, UserCmd::default(), |_| false);
    assert_eq!(p.cl.state, ConnState::Active);
    let log = ded.log();
    assert!(!log.contains("Unpure client"), "{log}");
    p.cl.disconnect(&mut p.net);
}

#[test]
#[ignore = "needs a real 1.1.0 tremded: set TREM11_DED and TREM11_BASE; run with --ignored"]
fn downloads_a_pak_from_a_vanilla_server() {
    let ded = Ded::start_with(30794, true, &["+set", "sv_allowDownload", "1"]).unwrap();
    let (_, base) = ded_paths().unwrap();
    let home = std::env::temp_dir().join("trem11-client-home-30794");
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(home.join("base")).unwrap();
    let mut p = Peer::new();
    // the client lacks the server's vms pak: CL_InitDownloads fetches it
    p.cl.fs = Some(trem_sim::kernel::files::PakSet::new(&[(base.join("base"), "base".into()), (home.join("base"), "base".into())]));
    p.cl.home_path = Some(home.clone());
    p.cl.allow_download = 1;
    p.cl.connect_protocol(&mut p.net, &format!("127.0.0.1:{}", ded.port), NetAdrType::Unspec, v11::PROTOCOL_VANILLA).unwrap();
    assert!(p.run_until(20.0, UserCmd::default(), |c| c.state == ConnState::Active), "no Active state: {:?}\n{}\n{}", p.cl.state, p.cl.print_out, ded.log());
    let got = std::fs::read(home.join("base/vms-standin.pk3")).expect("the pak was downloaded");
    let want = std::fs::read(Ded::home(ded.port).join("base/vms-standin.pk3")).unwrap();
    assert_eq!(got, want);
    assert!(p.cl.print_out.contains("Downloaded base/vms-standin.pk3"), "{}", p.cl.print_out);
    p.cl.disconnect(&mut p.net);
}

fn root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The 1.1.0 movement (kernel::pmove11) reproduces the ORIGINAL 1.1.0
/// Pmove's golden trace (re/harness/pmove11_ref over scenarios11.tsv) byte
/// for byte: re/traces/arena11.sha256.
#[test]
fn game_pmove11_golden_trace_matches_c_oracle() {
    let root = root();
    let want = std::fs::read_to_string(root.join("re/traces/arena11.sha256")).expect("re/traces/arena11.sha256");
    let want = want.split_whitespace().next().unwrap().to_string();
    let trace = trem_sim::parity::dump_scenarios11(trem_sim::gen::maps::MapId::Arena, None);
    let got = format!("{:x}", Sha256::digest(trace.as_bytes()));
    if got != want {
        let _ = std::fs::create_dir_all(root.join("re/traces/port"));
        let _ = std::fs::write(root.join("re/traces/port/arena11.tsv"), &trace);
        panic!("1.1 golden trace diverged (port {got} != reference {want}); run `sheetty parity re/traces/ref/arena11.tsv re/traces/port/arena11.tsv --first-divergence`");
    }
}

use trem_sim::gen::buildables::BuildableId;
use trem_sim::gen::buildables11::Buildable11Id;
use trem_sim::gen::classes11::Class11Id;
use trem_sim::gen::enums::{self, Event, Event11, MeansOfDeath, Stat, Stat11, Pers, Pers11, Team};
use trem_sim::gen::upgrades11::Upgrade11Id;
use trem_sim::gen::weapons::WeaponId;
use trem_sim::gen::weapons11::Weapon11Id;
use trem_sim::kernel::svgame;

/// re/traces/v11/consts.tsv: the 1.1.0 headers' defines and enum members as
/// the C compiler evaluates them (re/harness/consts11).
fn consts11() -> std::collections::HashMap<String, (String, String)> {
    let t = std::fs::read_to_string(root().join("re/traces/v11/consts.tsv")).expect("re/traces/v11/consts.tsv");
    t.lines().filter_map(|l| {
        let c: Vec<&str> = l.split('\t').collect();
        (c.len() >= 3).then(|| (c[0].to_string(), (c[1].to_string(), c[2].to_string())))
    }).collect()
}
fn c11(m: &std::collections::HashMap<String, (String, String)>, name: &str) -> i64 {
    m.get(name).unwrap_or_else(|| panic!("{name} not in consts.tsv")).1.parse().unwrap()
}

#[test]
fn game_tables_number_like_the_11_headers() {
    let m = consts11();
    for w in Weapon11Id::ALL {
        assert_eq!(w.num() as i64, c11(&m, w.cold().c_sym), "{}", w.cold().c_sym);
    }
    assert_eq!(Weapon11Id::COUNT as i64 + 1, c11(&m, "WP_NUM_WEAPONS"));
    for c in Class11Id::ALL {
        assert_eq!(c.num() as i64, c11(&m, c.cold().c_sym), "{}", c.cold().c_sym);
    }
    assert_eq!(Class11Id::COUNT as i64, c11(&m, "PCL_NUM_CLASSES"));
    for u in Upgrade11Id::ALL {
        assert_eq!(u.num() as i64, c11(&m, u.cold().c_sym), "{}", u.cold().c_sym);
    }
    assert_eq!(Upgrade11Id::COUNT as i64 + 1, c11(&m, "UP_NUM_UPGRADES"));
    for b in Buildable11Id::ALL {
        assert_eq!(b.num() as i64, c11(&m, b.cold().c_sym), "{}", b.cold().c_sym);
    }
    assert_eq!(Buildable11Id::COUNT as i64 + 1, c11(&m, "BA_NUM_BUILDABLES"));
}

#[test]
fn game_enum_domains_number_like_the_11_headers() {
    let m = consts11();
    let check = |prefix: &str, ty: &str, all: &[(&str, i64)]| {
        let mut n = 0;
        for (sym, (kind, v)) in &m {
            if kind != &format!("enum:{ty}") {
                continue;
            }
            let camel: String = sym.strip_prefix(prefix).unwrap().split('_').map(|p| {
                let mut c = p.chars();
                c.next().map(|f| f.to_ascii_uppercase().to_string() + &c.as_str().to_ascii_lowercase()).unwrap_or_default()
            }).collect();
            let (_, val) = all.iter().find(|(n, _)| *n == camel).unwrap_or_else(|| panic!("{sym} missing from {ty}"));
            assert_eq!(*val, v.parse::<i64>().unwrap(), "{sym}");
            n += 1;
        }
        assert_eq!(n, all.len(), "{ty}");
    };
    check("EV_", "entity_event_t", &Event11::ALL.map(|e| (e.name(), e as i64)));
    check("MOD_", "meansOfDeath_t", &enums::MeansOfDeath11::ALL.map(|e| (e.name(), e as i64)));
    check("ET_", "entityType_t", &enums::EntityType11::ALL.map(|e| (e.name(), e as i64)));
    check("STAT_", "statIndex_t", &Stat11::ALL.map(|e| (e.name(), e as i64)));
    check("PERS_", "persEnum_t", &Pers11::ALL.map(|e| (e.name(), e as i64)));
    check("MN_", "dynMenu_t", &enums::Menu11::ALL.map(|e| (e.name(), e as i64)));
    check("PM_", "pmtype_t", &enums::PmType11::ALL.map(|e| (e.name(), e as i64)));
    for (name, v) in enums::ability11::ALL {
        assert_eq!(*v as i64, c11(&m, &format!("SCA_{}", name.to_ascii_uppercase())), "{name}");
    }
    // the shared enums the 1.1 adapter passes through unchanged
    for (sym, legacy) in [("TORSO_STAND2", enums::PlayerAnim::TorsoStand2 as i64), ("MAX_PLAYER_TOTALANIMATIONS", enums::PlayerAnim::MaxPlayerTotalanimations as i64), ("NSPA_WALKBACK", enums::NonSegAnim::Walkback as i64), ("BANIM_DESTROYED", enums::BuildableAnim::Destroyed as i64), ("WEAPON_RELOADING", enums::WeaponState::Reloading as i64), ("WPM_NOTFIRING", enums::WeaponMode::Notfiring as i64), ("PTE_HUMANS", Team::Humans as i64), ("PTE_ALIENS", Team::Aliens as i64)] {
        assert_eq!(c11(&m, sym), legacy, "{sym}");
    }
}

#[test]
fn game_numbering_maps_to_legacy() {
    // weapons: WP_HBUILD2 (26) is the presentation-only ackit, WP_HBUILD the ckit
    assert_eq!(v11::weapon_from_11(Weapon11Id::Ackit as i32), WeaponId::Ackit as i32);
    assert_eq!(v11::weapon_from_11(27), WeaponId::Ckit as i32);
    assert_eq!(v11::weapon_to_11(WeaponId::Ckit as i32), 27);
    for w in Weapon11Id::ALL {
        assert_eq!(v11::weapon_to_11(v11::weapon_from_11(w as i32)), w as i32);
        assert_eq!(w.def().legacy.id(), w.id());
    }
    // buildables: the hovel (8) moves to the presentation row, the humans' down one
    assert_eq!(v11::buildable_from_11(Buildable11Id::Hovel as i32), BuildableId::Hovel as i32);
    assert_eq!(v11::buildable_from_11(9), BuildableId::Telenode as i32);
    assert_eq!(v11::buildable_from_11(16), BuildableId::Repeater as i32);
    for b in Buildable11Id::ALL {
        assert_eq!(v11::buildable_to_11(v11::buildable_from_11(b as i32)), b as i32);
    }
    // presentation-only rows are in no legacy stage
    assert_eq!(BuildableId::Hovel.def().stages, 0);
    assert_eq!(WeaponId::Ackit.def().stages, 0);
    // events, means of death, entity types
    assert_eq!(v11::event_num_from_11(Event11::MissileHit as i32 | 0x100), Event::MissileHit as i32 | 0x100);
    assert_eq!(Event11::MissileHit as i32 + 1, Event::MissileHit as i32);
    assert_eq!(v11::event_from_11(Event11::Lev4ChargeStart), Event::Lev4TrampleStart);
    assert_eq!(v11::event_from_11(Event11::RptuseSound), Event::RptuseSound);
    for e in Event11::ALL {
        assert_eq!(v11::event_num_to_11(v11::event_num_from_11(e as i32)), e as i32);
        assert_eq!(v11::event_from_11(e).name().to_ascii_lowercase().replace("trample", "charge"), e.name().to_ascii_lowercase());
    }
    assert_eq!(v11::mod_from_11(enums::MeansOfDeath11::Level4Charge as i32), MeansOfDeath::Level4Trample as i32);
    assert_eq!(v11::mod_from_11(enums::MeansOfDeath11::Overmind as i32), MeansOfDeath::Overmind as i32);
    assert_eq!(v11::etype_from_11(enums::EntityType11::Buildable as i32), svgame::ET_BUILDABLE);
    assert_eq!(v11::etype_from_11(enums::EntityType11::Missile as i32), svgame::ET_MISSILE);
    assert_eq!(v11::etype_from_11(enums::EntityType11::Lev2ZapChain as i32), svgame::ET_LEV2_ZAP_CHAIN);
    assert_eq!(v11::etype_from_11(enums::EntityType11::Events as i32 + Event11::Jump as i32), svgame::ET_EVENTS + Event::Jump as i32);
    // pm_flags: PMF_FOLLOW 4096 -> 2048, PMF_TIME_WATERJUMP 256 -> 128
    use trem_sim::gen::{constants as k, constants11 as k11};
    for (a, b) in [(k11::PMF_FOLLOW, k::PMF_FOLLOW), (k11::PMF_TIME_WATERJUMP, k::PMF_TIME_WATERJUMP), (k11::PMF_CHARGE, k::PMF_CHARGE), (k11::PMF_WEAPON_SWITCH, k::PMF_WEAPON_SWITCH), (k11::PMF_DUCKED, k::PMF_DUCKED), (k11::PMF_RESPAWNED, k::PMF_RESPAWNED)] {
        assert_eq!(v11::pm_flags_from_11(a), b);
        assert_eq!(v11::pm_flags_to_11(b), a);
    }
    assert_eq!(v11::eflags_from_11(k11::EF_TALK | k11::EF_FIRING), k::EF_FIRING);
    assert_eq!(v11::eflags_from_11(k11::EF_BLOBLOCKED), k::EF_BLOBLOCKED);
    assert_eq!(v11::stat_state_from_11(k11::SS_WALLCLIMBING | k11::SS_WALLCLIMBINGCEILING | k11::SS_SPEEDBOOST), (k::SS_WALLCLIMBING | k::SS_SPEEDBOOST, k::EF_WALLCLIMBCEILING));
}

/// A 1.1 human the way 1.1's ClientSpawn makes one (blaster + rifle).
fn human11() -> v11::PlayerState11 {
    use trem_sim::kernel::bg11;
    let mut p = v11::PlayerState11 { client_num: 3, pm_type: 0, gravity: 800, speed: 320, viewheight: 26, ground_entity_num: 1023, grapple_point: [0.0, 0.0, 1.0], ..Default::default() };
    p.stats[Stat11::Pclass as usize] = Class11Id::HumanBase as i32;
    p.stats[Stat11::Pteam as usize] = 2;
    p.stats[Stat11::Health as usize] = 100;
    p.stats[Stat11::MaxHealth as usize] = 100;
    p.stats[Stat11::Stamina as usize] = 1000;
    bg11::add_weapon_to_inventory(Weapon11Id::Blaster as i32, &mut p.stats);
    bg11::add_weapon_to_inventory(Weapon11Id::Rifle as i32, &mut p.stats);
    bg11::add_upgrade_to_inventory(Upgrade11Id::Larmour as i32, &mut p.stats);
    p.weapon = Weapon11Id::Rifle as i32;
    bg11::pack_ammo_array(p.weapon, &mut p.ammo, &mut p.powerups, 27, 5);
    p.persistant[Pers11::Credit as usize] = 345;
    p.persistant[Pers11::Newweapon as usize] = Weapon11Id::Ckit as i32;
    p.stats[Stat11::State as usize] = trem_sim::gen::constants11::SS_SPEEDBOOST;
    p.e_flags = trem_sim::gen::constants11::EF_TALK;
    p.origin = [10.0, 20.0, 30.0];
    p
}

#[test]
fn game_playerstate_maps_and_round_trips() {
    let p = human11();
    let s = v11::ps_from_11(&p);
    assert_eq!(s.stats[Stat::Team as usize], Team::Humans as i32);
    assert_eq!(s.stats[Stat::Weapon as usize], WeaponId::Rifle as i32);
    assert_eq!(s.weapon, WeaponId::Rifle as i32);
    assert_eq!((s.ammo, s.clips), (27, 5));
    assert_eq!(s.persistant[Pers::Credit as usize], 345);
    assert_eq!(s.persistant[Pers::Newweapon as usize], WeaponId::Ckit as i32);
    assert!(trem_sim::kernel::bg::inventory_contains_upgrade(trem_sim::gen::upgrades::UpgradeId::Larmour as i32, &s.stats));
    assert_eq!(s.stats[Stat::State as usize], trem_sim::gen::constants::SS_SPEEDBOOST);
    assert_eq!(s.e_flags & trem_sim::gen::constants::EF_POISONCLOUDED, 0, "1.1 EF_TALK is not legacy EF_POISONCLOUDED");
    let back = v11::ps_to_11(&s);
    assert_eq!(back, p);
    assert_eq!(v11::ps_from_11(&back), s);
    // aliens: 1.1 counts frags, legacy credits
    let mut a = p;
    a.stats[Stat11::Pteam as usize] = 1;
    a.persistant[Pers11::Credit as usize] = 3;
    assert_eq!(v11::ps_from_11(&a).persistant[Pers::Credit as usize], 3 * trem_sim::gen::constants::ALIEN_CREDITS_PER_KILL);
    assert_eq!(v11::ps_to_11(&v11::ps_from_11(&a)), a);
}

/// Predicting through the legacy playerState (ps_to_11, pmove11,
/// ps_from_11) is predicting on the 1.1 one: the 1.1 golden scenarios agree
/// tick by tick.
#[test]
fn game_prediction_through_legacy_playerstate_is_exact() {
    use trem_sim::kernel::pmove11::Pmove11;
    use trem_sim::kernel::rng::Rng;
    struct W(trem_sim::kernel::cm::ClipMap);
    impl trem_sim::kernel::types::PmWorld for W {
        fn trace(&self, s: &[f32; 3], mins: Option<&[f32; 3]>, maxs: Option<&[f32; 3]>, e: &[f32; 3], _p: i32, mask: i32) -> trem_sim::kernel::cm::Trace {
            let mut t = self.0.box_trace(s, e, mins, maxs, mask);
            t.entity_num = if t.fraction != 1.0 { 1022 } else { 1023 };
            t
        }
        fn point_contents(&self, p: &[f32; 3], _p: i32) -> i32 {
            self.0.point_contents(p)
        }
    }
    let w = W(trem_sim::kernel::cm::from_sheet(trem_sim::gen::maps::MapId::Arena));
    let mut direct = human11();
    direct.e_flags = 0;
    direct.stats[Stat11::State as usize] = 0;
    let mut legacy = v11::ps_from_11(&direct);
    let (mut r1, mut r2) = (Rng::default(), Rng::default());
    let mut cmd = UserCmd::default();
    for t in 0..200 {
        cmd.server_time += 16;
        cmd.forwardmove = if t < 120 { 127 } else { 0 };
        cmd.upmove = if (40..60).contains(&t) { 127 } else { 0 };
        cmd.buttons = if (60..140).contains(&t) { 1 } else { 0 };
        cmd.angles[1] += 37;
        Pmove11::new(&mut direct, cmd, &w, &mut r1).pmove();
        let mut p = v11::ps_to_11(&legacy);
        Pmove11::new(&mut p, cmd, &w, &mut r2).pmove();
        legacy = v11::ps_from_11(&p);
        assert_eq!(legacy, v11::ps_from_11(&direct), "tick {t}");
    }
}

#[test]
fn game_entities_map_to_legacy() {
    use trem_sim::gen::constants as k;
    use trem_sim::gen::enums::EntityType11 as T;
    // a spawned, powered telenode at 15/31 health
    let mut e = EntityState { number: 70, e_type: T::Buildable as i32, modelindex: Buildable11Id::Telenode as i32, modelindex2: 2, generic1: 15 | 0x20 | 0x40, ..Default::default() };
    let s = v11::es_from_11(&e);
    assert_eq!(s.e_type, svgame::ET_BUILDABLE);
    assert_eq!(s.modelindex, BuildableId::Telenode as i32);
    assert_eq!(s.e_flags & (k::EF_B_SPAWNED | k::EF_B_POWERED), k::EF_B_SPAWNED | k::EF_B_POWERED);
    assert_eq!(s.generic1, (15.0f32 / 31.0 * BuildableId::Telenode.def().health as f32).ceil() as i32);
    // the hovel
    e.modelindex = Buildable11Id::Hovel as i32;
    assert_eq!(v11::es_from_11(&e).modelindex, BuildableId::Hovel as i32);
    // a player: weapon and class renumbered, items kept
    let p = EntityState { e_type: T::Player as i32, weapon: Weapon11Id::Ackit as i32, misc: 2 | (Class11Id::HumanBase as i32) << 8, modelindex: 1 << 5, ..Default::default() };
    let s = v11::es_from_11(&p);
    assert_eq!(s.weapon, WeaponId::Ackit as i32);
    assert_eq!(s.misc, 2 | (trem_sim::gen::classes::ClassId::HumanBase as i32) << 8);
    assert_eq!(s.modelindex, 1 << trem_sim::gen::upgrades::UpgradeId::Jetpack as i32);
    // a corpse: 1.1's class in clientNum
    let c = EntityState { e_type: T::Corpse as i32, client_num: Class11Id::Level3 as i32, ..Default::default() };
    let s = v11::es_from_11(&c);
    assert_eq!((s.e_type, s.generic1, s.modelindex2), (svgame::ET_CORPSE, trem_sim::gen::classes::ClassId::Level3 as i32, Team::Aliens as i32));
    // the obituary temp entity: victim 4, killed by client 7 with a dragoon pounce
    let o = EntityState { number: 300, e_type: T::Events as i32 + Event11::Obituary as i32, other_entity_num: 4, other_entity_num2: 7, event_parm: enums::MeansOfDeath11::Level3Pounce as i32, ..Default::default() };
    let s = v11::es_from_11(&o);
    let g = svgame::event_from_state(&s).expect("event");
    assert_eq!((g.event, g.weapon, g.other, g.parm), (Event::Obituary, 4, 7, MeansOfDeath::Level3Pounce as i32));
    // a missile hitting client 5: the event on the missile entity itself
    let m = EntityState { number: 301, e_type: T::General as i32, event: Event11::MissileHit as i32 | 0x200, weapon: Weapon11Id::Flamer as i32, other_entity_num: 5, event_parm: 12, ..Default::default() };
    let s = v11::es_from_11(&m);
    let g = v11::entity_event_from_11(&s, 301, &[1.0, 2.0, 3.0]).expect("event");
    assert_eq!((g.event, g.weapon, g.other, g.parm, g.entity, g.origin), (Event::MissileHit, WeaponId::Flamer as i32, 5, 12, 301, [1.0, 2.0, 3.0]));
    // a predictable event of client 2 (EF_PLAYER_EVENT temp entity)
    let j = EntityState { number: 302, e_type: T::Events as i32 + Event11::FireWeapon as i32, e_flags: 4, other_entity_num: 2, weapon: Weapon11Id::Rifle as i32, ..Default::default() };
    let g = svgame::event_from_state(&v11::es_from_11(&j)).unwrap();
    assert_eq!((g.event, g.entity, g.weapon, g.predictable), (Event::FireWeapon, 2, WeaponId::Rifle as i32, true));
}

#[test]
fn game_configstrings_map_to_legacy() {
    use svgame::cs;
    use v11::cs11;
    assert_eq!(v11::configstring_from_11(cs11::PLAYERS + 5, "n\\bob\\t\\2"), Some((cs::PLAYERS + 5, "n\\bob\\t\\2".to_string())));
    assert_eq!(v11::configstring_from_11(cs11::MODELS + 1, "m").unwrap().0, cs::MODELS + 1);
    assert_eq!(v11::configstring_from_11(cs11::SOUNDS + 9, "s").unwrap().0, cs::SOUNDS + 9);
    assert_eq!(v11::configstring_from_11(cs11::LOCATIONS + 2, "l").unwrap().0, cs::LOCATIONS + 2);
    assert_eq!(v11::configstring_from_11(cs11::PRECACHES + 2, "p"), None);
    assert_eq!(v11::configstring_from_11(cs11::VOTE_STRING, "kick x").unwrap().0, cs::VOTE_STRING);
    assert_eq!(v11::configstring_from_11(cs11::TEAMVOTE_TIME, "1").unwrap().0, cs::VOTE_TIME + Team::Humans as usize);
    assert_eq!(v11::configstring_from_11(cs11::TEAMVOTE_NO + 1, "1").unwrap().0, cs::VOTE_NO + Team::Aliens as usize);
    assert_eq!(v11::configstring_from_11(cs11::GAME_VERSION, "base").unwrap().0, cs::GAME_VERSION);
    let ready = v11::configstring_from_11(cs11::CLIENTS_READY, "5").unwrap();
    assert_eq!(ready.0, cs::CLIENTS_READY);
    assert!(svgame::client_list_contains(&ready.1, 0) && svgame::client_list_contains(&ready.1, 2) && !svgame::client_list_contains(&ready.1, 1));
    let st = v11::configstrings_from_11(cs11::STAGES, "1 2 30 40 50 -1");
    assert!(st.contains(&(v11::CS11_STAGES, "1 2 30 40 50 -1".to_string())));
    assert!(st.contains(&(cs::ALIEN_STAGES, "1 30 50".to_string())));
    assert!(st.contains(&(cs::HUMAN_STAGES, "2 40 -1".to_string())));
    assert_eq!(v11::configstring_from_11(cs11::BUILDPOINTS, "1 2 3 4 5").unwrap().0, v11::CS11_BUILDPOINTS);
    assert!(v11::CS11_SCORES2 < cs::MAX_CONFIGSTRINGS);
}

#[test]
fn game_server_commands_map_to_legacy() {
    assert_eq!(v11::server_command_from_11("chat \"bob^7: hi\x19\""), vec!["chat -1 0 \"bob^7: hi\"".to_string()]);
    assert_eq!(v11::server_command_from_11("tchat \"(bob^7): rush\""), vec!["chat -1 1 \"(bob^7): rush\"".to_string()]);
    assert_eq!(v11::server_command_from_11("cp \"hello\""), vec!["cp \"hello\"".to_string()]);
    let s = v11::server_command_from_11(&format!("scores 2 7 9 0 10 50 3 {} 6 4 -2 999 1 {} 0", Weapon11Id::Ackit as i32, Weapon11Id::Level3 as i32));
    assert_eq!(s, vec![format!("scores 7 9 0 10 50 3 {} 6 4 -2 999 1 {} 0", WeaponId::Ackit as i32, WeaponId::Level3 as i32)]);
    let t = v11::server_command_from_11(&format!("tinfo 1 3 0 80 0 {} {}", Weapon11Id::Ckit as i32, 2 | 11 << 8));
    assert_eq!(t, vec![format!("tinfo 3 0 80 {} 0", WeaponId::Ckit as i32)]);
    let t = v11::server_command_from_11(&format!("tinfo 1 4 2 50 0 1 {}", 1 | (Class11Id::Level2 as i32) << 8));
    assert_eq!(t, vec![format!("tinfo 4 2 50 {}", trem_sim::gen::classes::ClassId::Level2 as i32)]);
    assert_eq!(v11::server_command_from_11(&format!("servermenu {}", enums::Menu11::HNofunds as i32)), vec!["servermenu 0 \"You don't have enough credits\" 0".to_string()]);
    assert_eq!(v11::server_command_from_11(&format!("servermenu {}", enums::Menu11::ABuild as i32)), vec![format!("servermenu 0 \"{}\" 1", trem_sim::kernel::weapon::BUILD_MENU_NOTICE)]);
    let w = v11::server_command_from_11("print \"Humans win\n\"");
    assert_eq!(w[1], format!("cs {} \"Humans Win\"", svgame::cs::WINNER));
    assert_eq!(v11::server_command_from_11(&format!("cs {} \"1 2 3 4 5 6\"", v11::cs11::STAGES)).len(), 3);
    assert!(v11::server_command_from_11("ptrcrequest").is_empty());
}

#[test]
fn game_client_commands_and_usercmds_map_to_11() {
    use trem_sim::gen::{constants as k, constants11 as k11};
    assert_eq!(v11::client_command_to_11("build hovel").as_deref(), Some("build hovel"));
    assert_eq!(v11::client_command_to_11("class ackit").as_deref(), Some("class ackit"));
    assert_eq!(v11::client_command_to_11("say_team hi").as_deref(), Some("say_team hi"));
    assert_eq!(v11::client_command_to_11("team spectators").as_deref(), Some("team spectate"));
    assert_eq!(v11::client_command_to_11("deconstruct marked").as_deref(), Some("deconstruct"));
    assert_eq!(v11::client_command_to_11("vsay_team taunt"), None);
    let c = UserCmd { buttons: k::BUTTON_ATTACK | k::BUTTON_SPRINT | k::BUTTON_DODGE | k::BUTTON_USE_EVOLVE, weapon: WeaponId::Ckit as u8, ..Default::default() };
    let u = v11::usercmd_to_11(&c);
    assert_eq!(u.buttons, k11::BUTTON_ATTACK | k11::BUTTON_GETFLAG);
    assert_eq!(u.weapon, Weapon11Id::Ckit as u8);
    let c = UserCmd { weapon: 32 + trem_sim::gen::upgrades::UpgradeId::Jetpack as u8, ..Default::default() };
    assert_eq!(v11::usercmd_to_11(&c).weapon, 32 + Upgrade11Id::Jetpack as u8);
}

/// The cgame of a protocol 69 game predicts with 1.1's movement: a 1.1
/// server running 1.1's Pmove on the commands it receives 64 ms late and
/// snapshotting every 48 ms never disagrees with the client's prediction
/// (no `Prediction miss`; the legacy prediction misses, the rubber banding
/// of a legacy client on a 1.1 server).
#[test]
fn game_cgame_predicts_11_movement_without_misses() {
    use trem_sim::kernel::cgview::Cgame;
    use trem_sim::kernel::cl::{ClSnapshot, NetClient, CMD_MASK};
    use trem_sim::kernel::pmove11::Pmove11;
    use trem_sim::kernel::rng::Rng;
    use trem_sim::kernel::sv::PACKET_MASK;
    use trem_sim::kernel::world::World;
    struct Sw(trem_sim::kernel::cm::ClipMap);
    impl trem_sim::kernel::types::PmWorld for Sw {
        fn trace(&self, s: &[f32; 3], mins: Option<&[f32; 3]>, maxs: Option<&[f32; 3]>, e: &[f32; 3], _p: i32, mask: i32) -> trem_sim::kernel::cm::Trace {
            let mut t = self.0.box_trace(s, e, mins, maxs, mask);
            t.entity_num = if t.fraction != 1.0 { 1022 } else { 1023 };
            t
        }
        fn point_contents(&self, p: &[f32; 3], _p: i32) -> i32 {
            self.0.point_contents(p)
        }
    }
    let run = |vanilla: bool| -> usize {
        let server_world = Sw(trem_sim::kernel::cm::from_sheet(trem_sim::gen::maps::MapId::Arena));
        let mut rng = Rng::default();
        let mut ps11 = human11();
        ps11.client_num = 0;
        ps11.e_flags = 0;
        ps11.stats[Stat11::State as usize] = 0;
        ps11.origin = [-1100.0, -300.0, 40.0];
        let mut cl = NetClient::new(1);
        cl.client_num = 0;
        let mut w = World::new(trem_sim::gen::maps::MapId::Arena);
        let mut cg = Cgame::new(0);
        cg.vanilla = vanilla;
        let mut n = 0;
        let mut uplink = std::collections::VecDeque::new();
        for frame in 1..=400 {
            let t = frame * 16;
            let mut cmd = UserCmd { server_time: t, ..Default::default() };
            // run in circles, jumping now and then
            cmd.forwardmove = 127;
            cmd.rightmove = if frame % 70 < 20 { 64 } else { 0 };
            cmd.upmove = if frame < 100 && frame % 30 < 3 { 127 } else { 0 };
            cmd.angles[1] = frame * 200;
            cl.cmd_number += 1;
            cl.cmds[cl.cmd_number as usize & CMD_MASK] = cmd;
            // the "boost" command's SS_SPEEDBOOST from the start (1.1 sprints
            // by command, legacy by BUTTON_SPRINT; a server-side change of
            // state is a prediction miss in any client)
            if frame == 1 {
                ps11.stats[Stat11::State as usize] |= trem_sim::gen::constants11::SS_SPEEDBOOST;
            }
            uplink.push_back(cmd);
            if uplink.len() > 4 {
                let c = uplink.pop_front().unwrap();
                Pmove11::new(&mut ps11, c, &server_world, &mut rng).pmove();
            }
            if t % 48 == 0 && ps11.command_time > 0 {
                n += 1;
                let s = ClSnapshot { valid: true, message_num: n, server_time: ps11.command_time, ps: v11::ps_from_11(&ps11), parse_entities_num: cl.parse_entities_num, ..Default::default() };
                cl.snapshots[n as usize & PACKET_MASK] = s.clone();
                cl.snap = s;
            }
            cl.server_time = t;
            cg.frame(&mut cl, &mut w);
        }
        assert!(cg.valid_pps);
        assert!(n > 100);
        cg.misses.len()
    };
    assert_eq!(run(true), 0, "1.1 prediction against a 1.1 server");
    let legacy_misses = run(false);
    eprintln!("legacy prediction misses: {legacy_misses}");
    assert!(legacy_misses > 50, "the legacy prediction misses against a 1.1 server");
}
