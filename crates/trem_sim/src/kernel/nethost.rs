//! Kernel `nethost` — the networked part of common.c's `Com_Frame`
//! (`NET_FlushPacketQueue`, `SV_Frame`, `CL_Frame` up to the cgame) and
//! the engine console commands that need no renderer: sv_ccmds.c's
//! heartbeat / serverinfo / systeminfo / killserver and cl_main.c's
//! localservers / globalservers / ping / serverstatus / rcon / reconnect,
//! plus `Info_Print` and the cvar plumbing of server and client (dec033).
//! The Godot host (game.rs) and the dedicated example both drive it; map
//! loading (`SV_SpawnServer`'s BSP, the client's `CL_InitCGame` world)
//! stays with them, asked for through `HostEvent`s. dec037 adds the
//! demo commands (record / stoprecord / demo), the download prompt frame,
//! ipv6 sockets with the multicast group of a running server
//! (`NET_JoinMulticast6` in SV_SpawnServer, `NET_LeaveMulticast6` in
//! SV_Shutdown) and the pure / download cvars.

use super::cgview::Cgame;
use super::cl::{ConnState, NetClient};
use super::files::PakSet;
use super::netchan::{self, Net, NetAdr, NetAdrType, NetSrc};
use super::sv::{Server, ServerCvars, MAX_MASTER_SERVERS};
use super::svgame;
use super::types::UserCmd;
use super::world::World;

/// What the host has to do after a frame.
#[derive(Clone, Debug, PartialEq)]
pub enum HostEvent {
    /// a gamestate arrived (`CL_ParseGamestate` -> `CL_InitCGame`): load
    /// the map's view World, then call `NetHost::view_loaded`
    LoadView { map: String },
    /// the server dropped us or the connection failed (`Com_Error(
    /// ERR_DROP / ERR_SERVERDISCONNECT )`): back to the menus with this text
    Dropped { reason: String },
    /// an rcon line with a good password (`SV_RemoteCommand`): run it on
    /// the server console, then `rcon_reply` the output
    Rcon { from: NetAdr, line: String },
    /// `FS_Restart` after a download: the host rescans its pk3 folders
    FsRestart,
    /// `CL_DemoCompleted`: back to the menu, then `CL_NextDemo` (nextdemo)
    DemoCompleted,
}

/// The host's network state: sockets, an optional server, the client and
/// its cgame.
pub struct NetHost {
    pub net: Net,
    pub server: Option<Server>,
    pub cl: NetClient,
    pub cg: Option<Cgame>,
    /// Com_Printf output of server, client and cgame for the host's console
    pub out: String,
    /// a LoadView is out and `view_loaded` has not been called yet
    load_pending: bool,
    /// `net_ip6` / `net_port6` for a server's ipv6 socket
    pub ip6: String,
    pub port6: u16,
    /// the server's pk3 search path for the next SV_SpawnServer
    pub server_fs: Option<PakSet>,
    /// a server reached over the loopback only (`net_enabled 0`: the
    /// single-player game recorded on the loopback, dec037)
    pub loopback_only: bool,
}

impl Default for NetHost {
    fn default() -> Self {
        Self::new()
    }
}

impl NetHost {
    pub fn new() -> Self {
        NetHost { net: Net::new(), server: None, cl: NetClient::new(0), cg: None, out: String::new(), load_pending: false, ip6: String::new(), port6: netchan::PORT_SERVER, server_fs: None, loopback_only: false }
    }

    /// `com_sv_running`.
    pub fn server_running(&self) -> bool {
        self.server.is_some()
    }

    /// The client is connected or connecting somewhere.
    pub fn client_connected(&self) -> bool {
        self.cl.state > ConnState::Disconnected
    }

    /// `SV_SpawnServer` on a loaded World: a new server (opening the server
    /// socket on `ip`:`port`, the next free one of the ten after it) or the
    /// next map of the running one. With `listen`, the host's own client
    /// connects over the loopback (`CL_MapLoading` -> "connect localhost").
    pub fn spawn_server(&mut self, world: World, map: &str, cvars: ServerCvars, ip: &str, port: u16, listen: bool) {
        match self.server.as_mut() {
            Some(sv) => {
                sv.cvars = cvars;
                sv.change_map(world, map);
            }
            None => {
                if !self.net.is_open(NetSrc::Server) && !self.loopback_only {
                    // NET_OpenIP per net_enabled (ipv6, then ipv4)
                    let (ip6, port6) = (self.ip6.clone(), self.port6);
                    let log = self.net.open_ip(NetSrc::Server, ip, port, &ip6, port6);
                    self.out.push_str(&log);
                    if !self.net.is_open(NetSrc::Server) {
                        self.out.push_str("WARNING: no network socket: only the loopback client can join\n");
                    }
                    else if let Some(p) = self.net.port(NetSrc::Server) {
                        // dec045: where the players on this network join
                        for a in netchan::local_ipv4() {
                            self.out.push_str(&format!("LAN players can join at {a}:{p} (connect {a}:{p})\n"));
                        }
                        if cfg!(windows) {
                            self.out.push_str("If they cannot see this server: allow the game through Windows Firewall on Private networks, with this network set to Private\n");
                        }
                    }
                }
                self.server = Some(Server::spawn_fs(world, map, cvars, self.server_fs.clone()));
                // Join the ipv6 multicast group now that a map is running so
                // clients can scan for us on the local network.
                let log = self.net.join_multicast6(NetSrc::Server);
                self.out.push_str(&log);
            }
        }
        if let Some(sv) = self.server.as_mut() {
            self.out.push_str(&std::mem::take(&mut sv.console));
        }
        if listen && !(self.cl.state > ConnState::Disconnected && self.cl.server_address.kind == NetAdrType::Loopback) {
            self.cg = None;
            let _ = self.cl.connect(&mut self.net, "localhost");
            self.out.push_str(&std::mem::take(&mut self.cl.print_out));
        }
    }

    /// `SV_Shutdown`: tell the clients, free the server and its socket.
    pub fn shutdown_server(&mut self, message: &str) {
        if let Some(mut sv) = self.server.take() {
            sv.shutdown(&mut self.net, message);
            self.out.push_str("----- Server Shutdown -----\n");
            self.net.close(NetSrc::Server);
        }
    }

    /// `CL_Disconnect` (+ `SV_Shutdown` for a listen server, as
    /// `CL_Disconnect_f` does).
    pub fn disconnect(&mut self) {
        self.shutdown_server("Disconnected from server");
        self.cl.disconnect(&mut self.net);
        self.cg = None;
        self.load_pending = false;
    }

    /// `CL_Connect_f`: a server address ("localhost" for our own server).
    pub fn connect(&mut self, server: &str) -> Result<(), String> {
        self.connect_family(server, NetAdrType::Unspec)
    }

    /// `CL_Connect_f` with its "-4" / "-6" family option.
    pub fn connect_family(&mut self, server: &str, family: NetAdrType) -> Result<(), String> {
        self.connect_protocol(server, family, super::cl::NET_PROTOCOL_AUTO)
    }

    /// `CL_Connect_f` with a protocol (dec040): 71 or 69, or
    /// NET_PROTOCOL_AUTO for `cl_netProtocol`.
    pub fn connect_protocol(&mut self, server: &str, family: NetAdrType, protocol: i32) -> Result<(), String> {
        // a listen server goes away unless we connect to it
        if !server.eq_ignore_ascii_case("localhost") {
            self.shutdown_server("Server quit");
        }
        if !self.net.is_open(NetSrc::Client) && !server.eq_ignore_ascii_case("localhost") {
            self.net.open_client();
            if !self.net.is_open(NetSrc::Client) {
                return Err("NET_IPSocket: no socket".into());
            }
        }
        self.cg = None;
        self.load_pending = false;
        let r = self.cl.connect_protocol(&mut self.net, server, family, protocol);
        self.out.push_str(&std::mem::take(&mut self.cl.print_out));
        r
    }

    /// One frame of `msec`: queued packets, the server, the client and the
    /// cgame (into `view`), then the usercmd when there is one.
    pub fn frame(&mut self, msec: i32, view: Option<&mut World>, cmd: Option<UserCmd>) -> Vec<HostEvent> {
        let mut ev = Vec::new();
        self.net.flush_packet_queue();
        if self.cl.fs_restarted {
            self.cl.fs_restarted = false;
            ev.push(HostEvent::FsRestart);
        }
        if let Some(sv) = self.server.as_mut() {
            sv.frame(&mut self.net, msec);
            self.out.push_str(&std::mem::take(&mut sv.console));
            for (from, line) in std::mem::take(&mut sv.rcon_queue) {
                ev.push(HostEvent::Rcon { from, line });
            }
        }
        self.cl.frame(&mut self.net, msec);
        if self.cl.fs_restarted {
            self.cl.fs_restarted = false;
            ev.push(HostEvent::FsRestart);
        }
        if self.cl.demo.completed {
            self.cl.demo.completed = false;
            self.cg = None;
            self.load_pending = false;
            self.out.push_str(&std::mem::take(&mut self.cl.print_out));
            ev.push(HostEvent::DemoCompleted);
            return ev;
        }
        if let Some(reason) = self.cl.error.take() {
            self.cg = None;
            self.load_pending = false;
            ev.push(HostEvent::Dropped { reason });
        }
        if self.cl.gamestate_new && !self.load_pending {
            self.load_pending = true;
            self.cg = None;
            // (the serverinfo as the server sent it: a 1.1.0 one on protocol 69, dec040)
            let map = svgame::info_value(&self.cl.engine_cs(svgame::cs::SERVERINFO), "mapname");
            ev.push(HostEvent::LoadView { map });
        }
        if let (Some(cg), Some(v)) = (self.cg.as_mut(), view) {
            if self.cl.state == ConnState::Active {
                cg.frame(&mut self.cl, v);
                self.out.push_str(&std::mem::take(&mut cg.print_out));
            }
        }
        if let Some(cmd) = cmd {
            self.cl.create_cmd(cmd);
        }
        self.cl.send_cmd(&mut self.net);
        self.out.push_str(&std::mem::take(&mut self.cl.print_out));
        ev
    }

    /// Once per frame of the host's ui (not per network tick): `CL_Frame`'s
    /// download prompt check.
    pub fn ui_frame(&mut self) {
        self.cl.download_prompt_frame();
        self.out.push_str(&std::mem::take(&mut self.cl.print_out));
    }

    /// `CL_PlayDemo_f`: kill a local server ("sv_killserver 2"), disconnect,
    /// open the demo and read up to its gamestate; the next frame asks the
    /// host to load the map (LoadView).
    pub fn play_demo(&mut self, name: &str) -> Result<(), String> {
        self.shutdown_server("Server quit");
        self.cl.disconnect(&mut self.net);
        self.cg = None;
        self.load_pending = false;
        let r = self.cl.play_demo(name);
        self.out.push_str(&std::mem::take(&mut self.cl.print_out));
        r
    }

    /// The view World of the last LoadView is up: `CL_InitCGame` and
    /// `CL_DownloadsComplete`'s "we're ready".
    pub fn view_loaded(&mut self) {
        self.load_pending = false;
        self.cg = Some(Cgame::init(&self.cl));
        self.cl.cgame_loaded(&mut self.net);
    }

    /// wt-b VoIP: `CL_Frame`'s `CL_CaptureVoip` (after `CL_SendCmd`) with
    /// the host's sound capture and cvars.
    pub fn capture_voip(&mut self, host: &mut dyn super::voip::VoipHost) {
        self.cl.capture_voip(host);
        self.out.push_str(&std::mem::take(&mut self.cl.print_out));
    }

    /// The local player's client number in the view.
    pub fn client_num(&self) -> Option<usize> {
        (self.cl.state >= ConnState::Primed && self.cl.client_num >= 0 && (self.cl.client_num as usize) < super::world::MAX_CLIENTS).then_some(self.cl.client_num as usize)
    }

    /// `SV_SendServerCommand`-free answer to an rcon line.
    pub fn rcon_reply(&mut self, to: &NetAdr, text: &str) {
        if let Some(sv) = self.server.as_mut() {
            sv.rcon_reply(&mut self.net, to, text);
        }
    }

    /// The engine commands this module owns; None for any other command.
    pub fn command(&mut self, argv: &[String]) -> Option<String> {
        let a = |i: usize| argv.get(i).map(|s| s.as_str()).unwrap_or("");
        let c = a(0).to_ascii_lowercase();
        let r = match c.as_str() {
            // sv_ccmds.c
            "heartbeat" => match self.server.as_mut() {
                Some(sv) => {
                    sv.force_heartbeat();
                    String::new()
                }
                None => "Server is not running.\n".into(),
            },
            "serverinfo" => match self.server.as_ref() {
                Some(sv) => format!("Server info settings:\n{}", info_print(&sv.serverinfo())),
                None => "Server is not running.\n".into(),
            },
            "systeminfo" => match self.server.as_ref() {
                Some(sv) => format!("System info settings:\n{}", info_print(&sv.systeminfo())),
                None => "Server is not running.\n".into(),
            },
            "killserver" => {
                self.shutdown_server("killserver");
                String::new()
            }
            // cl_main.c
            "localservers" => {
                self.cl.local_servers(&mut self.net);
                String::new()
            }
            "globalservers" => {
                if argv.len() < 2 {
                    "usage: globalservers <master# 0-4> [keywords]\n".into()
                } else {
                    let master = super::spawn::c_atoi(a(1)).max(0) as usize;
                    // the C passes Cmd_Argv(2) on; the ui sends "full empty"
                    // and both words matter to the master (without "empty"
                    // it leaves out every server with nobody on it). A
                    // numeric first word (ioq3's protocol argument) is
                    // dropped: the request carries PROTOCOL_VERSION (dec039)
                    let mut kw: Vec<&str> = argv[2..].iter().map(String::as_str).collect();
                    if kw.first().is_some_and(|w| !w.is_empty() && w.bytes().all(|b| b.is_ascii_digit())) {
                        kw.remove(0);
                    }
                    let kw = kw.join(" ");
                    match self.cl.global_servers(&mut self.net, master, &kw) {
                        Ok(()) => String::new(),
                        Err(e) => format!("{e}\n"),
                    }
                }
            }
            "ping" => {
                if argv.len() < 2 {
                    "usage: ping [-4|-6] server\n".into()
                } else {
                    let host = argv.last().unwrap();
                    let (r, mut adr) = netchan::string_to_adr(host, NetAdrType::Unspec);
                    if r == 0 {
                        "Bad address\n".into()
                    } else {
                        if r == 2 {
                            adr.port = netchan::PORT_SERVER;
                        }
                        self.cl.ping(&mut self.net, &adr);
                        String::new()
                    }
                }
            }
            "serverstatus" => {
                let to = if argv.len() > 1 { a(1).to_string() } else if self.cl.state >= ConnState::Connected { self.cl.servername.clone() } else { String::new() };
                if to.is_empty() {
                    "Not connected to a server.\nusage: serverstatus [-4|-6] server\n".into()
                } else if self.cl.request_server_status(&mut self.net, &to) {
                    String::new()
                } else {
                    "Bad address\n".into()
                }
            }
            "rcon" => match self.cl.rcon(&mut self.net, &argv[1..].join(" ")) {
                Ok(()) => String::new(),
                Err(e) => format!("{e}\n"),
            },
            // cl_main.c demos
            "record" => {
                if argv.len() > 2 {
                    "record <demoname>\n".into()
                } else {
                    self.cl.record(argv.get(1).map(|s| s.as_str()))
                }
            }
            "stoprecord" => self.cl.stop_record(),
            _ => return None,
        };
        let mut out = std::mem::take(&mut self.cl.print_out);
        out.push_str(&r);
        Some(out)
    }
}

/// `CL_Connect_f`'s arguments: `[-4|-6] server [-1|-69|-71]` (dec040):
/// a last "-1" (GrangerHub's flag for 1.1.0 servers) or "-69" connects
/// with Tremulous 1.1.0's protocol 69, "-71" with legacy's; without one
/// `cl_netProtocol` decides. (family, server, protocol or
/// NET_PROTOCOL_AUTO); None without a server.
pub fn parse_connect_args(args: &[String]) -> Option<(NetAdrType, String, i32)> {
    let mut args: Vec<&str> = args.iter().map(String::as_str).collect();
    let protocol = match args.last().copied() {
        Some("-1") | Some("-69") if args.len() > 1 => super::v11::PROTOCOL_VANILLA,
        Some("-71") if args.len() > 1 => super::sv::PROTOCOL_VERSION,
        _ => super::cl::NET_PROTOCOL_AUTO,
    };
    if protocol != super::cl::NET_PROTOCOL_AUTO {
        args.pop();
    }
    let (family, server) = match args.as_slice() {
        [f, s, ..] if *f == "-4" => (NetAdrType::Ip, s.to_string()),
        [f, s, ..] if *f == "-6" => (NetAdrType::Ip6, s.to_string()),
        [.., s] => (NetAdrType::Unspec, s.to_string()),
        [] => return None,
    };
    Some((family, server, protocol))
}

/// `Info_Print`.
pub fn info_print(s: &str) -> String {
    let mut out = String::new();
    let mut it = s.strip_prefix('\\').unwrap_or(s).split('\\');
    while let Some(k) = it.next() {
        if k.is_empty() && out.is_empty() && s.len() <= 1 {
            break;
        }
        out.push_str(&format!("{k:<20} "));
        match it.next() {
            Some(v) => out.push_str(&format!("{v}\n")),
            None => {
                out.push_str("MISSING VALUE\n");
                break;
            }
        }
    }
    out
}

/// The server cvars (sv_init.c / sv_main.c / common.c names) from the
/// host's cvar store; unset ones keep the defaults.
pub fn server_cvars(get: &dyn Fn(&str) -> Option<String>) -> ServerCvars {
    let mut c = ServerCvars::default();
    let int = |n: &str, d: i32| get(n).map(|v| super::spawn::c_atoi(&v)).unwrap_or(d);
    if let Some(v) = get("sv_hostname") {
        c.hostname = v;
    }
    c.maxclients = int("sv_maxclients", c.maxclients);
    c.private_clients = int("sv_privateClients", c.private_clients);
    c.private_password = get("sv_privatePassword").unwrap_or_default();
    c.rcon_password = get("rconPassword").unwrap_or_default();
    c.fps = int("sv_fps", c.fps);
    c.timeout = int("sv_timeout", c.timeout);
    c.zombietime = int("sv_zombietime", c.zombietime);
    c.reconnect_limit = int("sv_reconnectlimit", c.reconnect_limit);
    c.allow_download = int("sv_allowDownload", c.allow_download);
    c.pure = int("sv_pure", c.pure);
    c.dl_url = get("sv_dlURL").unwrap_or_default();
    c.min_rate = int("sv_minRate", c.min_rate);
    c.max_rate = int("sv_maxRate", c.max_rate);
    c.min_ping = int("sv_minPing", c.min_ping);
    c.max_ping = int("sv_maxPing", c.max_ping);
    c.lan_force_rate = int("sv_lanForceRate", 1) != 0;
    c.pad_packets = int("sv_padPackets", c.pad_packets);
    c.dedicated = int("dedicated", 0);
    for i in 0..MAX_MASTER_SERVERS {
        if let Some(v) = get(&format!("sv_master{}", i + 1)) {
            c.masters[i] = v;
        }
    }
    c.motd = get("g_motd").unwrap_or_default();
    c.voip = int("sv_voip", c.voip).clamp(0, 1); // wt-b VoIP
    for n in ["timelimit", "g_needpass"] {
        if let Some(v) = get(n) {
            c.serverinfo_extra.push((n.to_string(), v));
        }
    }
    c
}

/// The net_ip.c cvars into the sockets: `net_enabled` (the port's 0, its
/// single-process default, opens what the client default 3 would),
/// `net_ip6` / `net_port6`, `net_mcast6addr` / `net_mcast6iface`.
/// A dedicated server defaults to ipv4 only, as the C's DEDICATED build
/// ("I want server owners to explicitly turn on ipv6 support").
pub fn apply_net_cvars(nh: &mut NetHost, get: &dyn Fn(&str) -> Option<String>, dedicated: bool) {
    let int = |n: &str, d: i32| get(n).map(|v| super::spawn::c_atoi(&v)).unwrap_or(d);
    let mut enabled = int("net_enabled", if dedicated { netchan::NET_ENABLEV4 } else { 0 });
    if enabled & (netchan::NET_ENABLEV4 | netchan::NET_ENABLEV6) == 0 {
        enabled |= netchan::NET_ENABLEV4 | netchan::NET_ENABLEV6;
    }
    nh.net.enabled = enabled;
    netchan::set_net_enabled(enabled);
    nh.ip6 = get("net_ip6").unwrap_or_default();
    nh.port6 = get("net_port6").map(|p| super::spawn::c_atoi(&p) as u16).filter(|p| *p != 0).unwrap_or(netchan::PORT_SERVER);
    nh.net.mcast6addr = get("net_mcast6addr").unwrap_or_else(|| netchan::NET_MULTICAST_IP6.into());
    nh.net.mcast6iface = get("net_mcast6iface").unwrap_or_default();
}

/// The client cvars the network client reads (cl_main.c names).
pub fn apply_client_cvars(cl: &mut NetClient, get: &dyn Fn(&str) -> Option<String>) {
    let int = |n: &str, d: i32| get(n).map(|v| super::spawn::c_atoi(&v)).unwrap_or(d);
    cl.maxpackets = int("cl_maxpackets", cl.maxpackets);
    cl.packetdup = int("cl_packetdup", cl.packetdup);
    cl.time_nudge = int("cl_timeNudge", cl.time_nudge);
    cl.timeout = int("cl_timeout", cl.timeout);
    cl.nodelta = int("cl_nodelta", 0) != 0;
    cl.max_ping = int("cl_maxPing", cl.max_ping);
    cl.lan_force_packets = int("cl_lanForcePackets", 1) != 0;
    cl.guid_server_uniq = int("cl_guidServerUniq", 1) != 0;
    cl.rcon_password = get("rconPassword").unwrap_or_default();
    // downloads and demos (dec037)
    cl.allow_download = int("cl_allowDownload", 0);
    cl.demo.auto_record = int("cl_autoRecordDemo", 0) != 0;
    cl.demo.timedemo = int("timedemo", 0) != 0;
    cl.demo.freeze = int("cl_freezeDemo", 0) != 0;
    cl.demo.timedemo_log = get("cl_timedemoLog").unwrap_or_default();
    cl.rcon_address = get("rconAddress").unwrap_or_default();
    // the network mode (dec040): 0 Auto, 71 legacy 1.3, 69 vanilla 1.1.0
    cl.net_protocol = match int("cl_netProtocol", cl.net_protocol) {
        p @ (super::v11::PROTOCOL_VANILLA | super::sv::PROTOCOL_VERSION) => p,
        _ => super::cl::NET_PROTOCOL_AUTO,
    };
    for i in 0..MAX_MASTER_SERVERS {
        let v = get(&format!("sv_master{}", i + 1));
        if let Some(v) = v {
            if cl.master_names.len() <= i {
                cl.master_names.resize(i + 1, String::new());
            }
            cl.master_names[i] = v;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn info_print_pads_keys() {
        assert_eq!(info_print("\\a\\1\\hostname\\x"), format!("{:<20} 1\n{:<20} x\n", "a", "hostname"));
        assert_eq!(info_print("\\a"), format!("{:<20} MISSING VALUE\n", "a"));
    }
}
