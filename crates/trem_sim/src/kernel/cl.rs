//! Kernel `cl` — the client engine of tremulous-legacy (dec033):
//! client/cl_main.c (the CA_* connection state machine: `CL_Connect_f`,
//! `CL_CheckForResend`, `CL_ConnectionlessPacket`, `CL_PacketEvent`,
//! `CL_Disconnect`, `CL_CheckTimeout`, `CL_CheckUserinfo`, `CL_Rcon_f`, the
//! server browser: `CL_LocalServers_f`, `CL_GlobalServers_f`,
//! `CL_ServersResponsePacket`, `CL_ServerInfoPacket`, `CL_Ping_f`,
//! `CL_GetPing*`, `CL_ServerStatus`), client/cl_parse.c (`CL_ParseGamestate`,
//! `CL_ParseSnapshot`, `CL_ParsePacketEntities`, `CL_ParseCommandString`,
//! `CL_ParseDownload`, `CL_SystemInfoChanged`), client/cl_input.c
//! (`CL_CreateNewCommands`, `CL_ReadyToSendPacket`, `CL_WritePacket` with
//! the keyed usercmd delta and the reliable commands), client/cl_cgame.c
//! (`CL_GetSnapshot`, `CL_GetServerCommand` with the bcs0/1/2 big
//! configstrings, `CL_SetCGameTime`, `CL_AdjustTimeDelta`,
//! `CL_FirstSnapshot`) and cl_net_chan.c; the pure / download half
//! (`CL_ParseServerInfo`, the sv_paks part of `CL_SystemInfoChanged`,
//! `CL_InitDownloads`, `CL_NextDownload` with com_downloadPrompt,
//! `CL_BeginDownload`, `CL_ParseDownload` into fs_homepath,
//! `CL_DownloadsComplete`, `CL_SendPureChecksums`; dec037). Demos are in
//! demo.rs.
//!
//! dec040: a connection speaks legacy's protocol 71 or vanilla Tremulous
//! 1.1.0's protocol 69 (`NetClient::protocol`, chosen by `cl_netProtocol`
//! or the connect flag; Auto asks the server's getinfo first): the 1.1
//! wire is v11net.rs, the game-data mapping v11.rs.
//!
//! The host (Godot TremGame or a test) owns a `Net` and calls `frame`;
//! cgview.rs is the cgame that reads snapshots and server commands.

use super::files::{self, PakSet};
use super::msg::{self, EntityState, Msg, GENTITYNUM_BITS, MAX_GENTITIES, MAX_MSGLEN};
use super::netchan::{self, Net, NetAdr, NetAdrType, NetSrc, Netchan};
const V11_MAX_RELIABLE_COMMANDS: usize = 64;
/// How far ahead of the expected block a download block is kept (a
/// server's window is MAX_DOWNLOAD_WINDOW, 48).
const MAX_DOWNLOAD_AHEAD: i32 = 64;
use super::sv::{clc, svc, MAX_RELIABLE_COMMANDS, MAX_SNAPSHOT_ENTITIES, PACKET_BACKUP, PACKET_MASK, PROTOCOL_VERSION, GAMENAME, SNAPFLAG_NOT_ACTIVE, MAX_MAP_AREA_BYTES, MAX_PACKET_USERCMDS};
use super::svgame::{self, cs};
use super::types::*;
use super::v11::{self, PlayerState11, PROTOCOL_VANILLA};
use super::v11net;

/// `CMD_BACKUP` / `CMD_MASK`.
pub const CMD_BACKUP: usize = 64;
pub const CMD_MASK: usize = CMD_BACKUP - 1;
/// `MAX_PARSE_ENTITIES`.
pub const MAX_PARSE_ENTITIES: usize = PACKET_BACKUP * MAX_SNAPSHOT_ENTITIES;
/// `RETRANSMIT_TIMEOUT`: time between connection packet retransmits.
pub const RETRANSMIT_TIMEOUT: i32 = 3000;
/// `RESET_TIME`.
const RESET_TIME: i32 = 500;
/// `MAX_PINGREQUESTS` / `MAX_OTHER_SERVERS` / `MAX_GLOBAL_SERVERS`.
pub const MAX_PINGREQUESTS: usize = 32;
pub const MAX_OTHER_SERVERS: usize = 128;
pub const MAX_GLOBAL_SERVERS: usize = 4096;
/// `MAX_SERVERSPERPACKET`.
const MAX_SERVERSPERPACKET: usize = 256;
/// `cl_netProtocol` (dec040): 0 asks the server first (Auto), 71 legacy
/// 1.3, 69 vanilla 1.1.0.
pub const NET_PROTOCOL_AUTO: i32 = 0;
/// How long Auto waits for the server's infoResponse before it falls back
/// to legacy (dec040).
pub const PROTOCOL_PROBE_TIME: i32 = 1000;

/// `connstate_t`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum ConnState {
    Uninitialized,
    /// not talking to a server
    #[default]
    Disconnected,
    /// not used any more, was checking cd key
    Authorizing,
    /// sending request packets to the server
    Connecting,
    /// sending challenge packets to the server
    Challenging,
    /// netchan_t established, getting gamestate
    Connected,
    /// only during cgame initialization, never during main loop
    Loading,
    /// got gamestate, waiting for first frame
    Primed,
    /// game views should be displayed
    Active,
    /// playing a cinematic or a static pic, not connected to a server
    Cinematic,
}

/// `clSnapshot_t`.
#[derive(Clone, Debug, Default)]
pub struct ClSnapshot {
    /// cleared if delta parsing was invalid
    pub valid: bool,
    /// rate delayed and dropped commands
    pub snap_flags: i32,
    /// server time the message is valid for (in msec)
    pub server_time: i32,
    /// copied from netchan->incoming_sequence
    pub message_num: i32,
    /// messageNum the delta is from
    pub delta_num: i32,
    /// time from when cmdNum-1 was sent to time packet was reeceived
    pub ping: i32,
    pub areamask: [u8; MAX_MAP_AREA_BYTES],
    /// complete information about the current player at this time
    pub ps: PlayerState,
    /// protocol 69: the playerState as the 1.1 server sent it (the delta
    /// base; `ps` is its legacy form for the cgame, dec040)
    pub ps11: PlayerState11,
    /// all of the entities that need to be presented
    pub num_entities: usize,
    /// at the time of this snapshot
    pub parse_entities_num: usize,
    /// execute all commands up to this before making the snapshot current
    pub server_command_num: i32,
}

/// `snapshot_t` (what CL_GetSnapshot hands the cgame).
#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub snap_flags: i32,
    pub ping: i32,
    pub server_time: i32,
    pub areamask: [u8; MAX_MAP_AREA_BYTES],
    pub ps: PlayerState,
    pub entities: Vec<EntityState>,
    pub server_command_sequence: i32,
}

/// `outPacket_t`.
#[derive(Clone, Copy, Debug, Default)]
pub struct OutPacket {
    /// cl.cmdNumber when packet was sent
    pub p_cmd_number: i32,
    /// usercmd->serverTime when packet was sent
    pub p_server_time: i32,
    /// cls.realtime when packet was sent
    pub p_realtime: i32,
}

/// `serverInfo_t` (the server browser's lists).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ServerInfo {
    pub adr: NetAdr,
    pub host_name: String,
    pub map_name: String,
    pub game: String,
    pub net_type: i32,
    pub game_type: i32,
    pub clients: i32,
    pub max_clients: i32,
    pub min_ping: i32,
    pub max_ping: i32,
    /// -1: no answer yet
    pub ping: i32,
    pub visible: bool,
    /// the master's featured-servers label (`CL_GSRFeaturedLabel`), "" otherwise
    pub label: String,
    /// the infoResponse's `protocol`: 71 legacy, 69 vanilla 1.1.0, 0 not
    /// known yet (dec040)
    pub protocol: i32,
}

/// `ping_t`.
#[derive(Clone, Debug, Default)]
pub struct Ping {
    pub adr: NetAdr,
    pub start: i32,
    pub time: i32,
    pub info: String,
}

/// `serverStatus_t` (one getstatus answer).
#[derive(Clone, Debug, Default)]
pub struct ServerStatus {
    pub adr: NetAdr,
    pub info: String,
    pub players: Vec<(i32, i32, String)>,
    pub time: i32,
}

/// The `AS_*` server list sources of the ui.
// q_shared.h: the client and the ui share these numbers (`trap_LAN_*`)
pub const AS_GLOBAL: i32 = 0;
/// "AS_MPLAYER is no longer used": the global list
pub const AS_MPLAYER: i32 = 1;
pub const AS_LOCAL: i32 = 2;
pub const AS_FAVORITES: i32 = 3;

/// The UDP download being received (`clc.download*`, cl_downloadName /
/// cl_downloadSize / cl_downloadCount / cl_downloadTime for the ui).
#[derive(Debug, Default)]
pub struct ClDownload {
    /// `clc.downloadName`: the local name, relative to fs_homepath
    pub local_name: String,
    /// `cl_downloadName`
    pub remote_name: String,
    pub block: i32,
    pub size: i32,
    pub count: i32,
    /// `cl_downloadTime` (cls.realtime at the start)
    pub time: i32,
    /// the data when there is no fs_homepath (tests)
    pub data: Vec<u8>,
    /// `clc.download`: the open "<local>.tmp" file
    pub file: Option<std::fs::File>,
    /// blocks that arrived ahead of `block` (a lost packet before them),
    /// by block counter; taken in order, and acknowledged in order, once the
    /// gap is filled, so a server still sees one "nextdl" per block in turn
    pub ahead: std::collections::BTreeMap<i32, Vec<u8>>,
}

/// `com_downloadPrompt` flags (q_shared.h `DLP_*`).
pub const DLP_TYPE_MASK: i32 = 0x0f;
/// don't download anything
pub const DLP_IGNORE: i32 = 0x01;
/// download via HTTP redirect
pub const DLP_CURL: i32 = 0x02;
/// download from server
pub const DLP_UDP: i32 = 0x04;
/// prompt needs to be shown
pub const DLP_SHOW: i32 = 0x10;
/// prompt has been processed by client
pub const DLP_PROMPTED: i32 = 0x20;
/// prompt is not being shown by UI VM
pub const DLP_STALE: i32 = 0x40;

/// The client (`clientConnection_t clc` + `clientActive_t cl` +
/// `clientStatic_t cls` subsets).
pub struct NetClient {
    pub state: ConnState,
    /// name of server from original connect (used by reconnect)
    pub servername: String,
    pub server_address: NetAdr,
    /// from the server to use for connecting
    pub challenge: i32,
    /// for connection retransmits
    pub connect_time: i32,
    pub connect_packet_count: i32,
    /// for display on connection dialog (a "print" OOB from the server)
    pub server_message: String,
    pub netchan: Netchan,
    /// `net_qport`
    pub qport: i32,
    pub reliable_sequence: i32,
    pub reliable_acknowledge: i32,
    pub reliable_commands: Vec<String>,
    pub server_message_sequence: i32,
    pub server_command_sequence: i32,
    pub last_executed_server_command: i32,
    pub server_commands: Vec<String>,
    pub last_packet_sent_time: i32,
    pub last_packet_time: i32,
    pub timeout_count: i32,
    pub checksum_feed: i32,
    pub client_num: i32,
    pub server_id: i32,
    /// `cl.gameState` (empty strings: unset)
    pub configstrings: Vec<String>,
    pub entity_baselines: Vec<EntityState>,
    /// a new gamestate arrived: the host (re)loads the map and the cgame
    pub gamestate_new: bool,
    pub snap: ClSnapshot,
    pub snapshots: Vec<ClSnapshot>,
    pub parse_entities: Vec<EntityState>,
    pub parse_entities_num: usize,
    pub new_snapshots: bool,
    pub cmds: Vec<UserCmd>,
    pub cmd_number: i32,
    pub out_packets: Vec<OutPacket>,
    /// may be paused during play
    pub server_time: i32,
    /// to prevent time from flowing bakcwards
    pub old_server_time: i32,
    /// to check tournament restarts
    pub old_frame_server_time: i32,
    /// cl.serverTime = cls.realtime + cl.serverTimeDelta
    pub server_time_delta: i32,
    /// set if any cgame frame has been forced to extrapolate
    pub extrapolated_snapshot: bool,
    /// `cls.realtime`
    pub realtime: i32,
    /// `Cvar_InfoString( CVAR_USERINFO )` as the host builds it
    pub userinfo: String,
    pub userinfo_modified: bool,
    /// the QKEY_FILE contents (2048 bytes) for cl_guid, and cl_guidServerUniq
    pub qkey: Option<Vec<u8>>,
    pub guid_server_uniq: bool,
    /// set when the server dropped us (ERR_SERVERDISCONNECT / ERR_DROP)
    pub error: Option<String>,
    /// `cl_reconnectArgs`
    pub reconnect_args: String,
    /// cvars: cl_maxpackets, cl_packetdup, cl_timeNudge, cl_timeout, cl_nodelta, cl_maxPing
    pub maxpackets: i32,
    pub packetdup: i32,
    pub time_nudge: i32,
    pub timeout: i32,
    pub nodelta: bool,
    pub max_ping: i32,
    /// `cl_lanForcePackets`
    pub lan_force_packets: bool,
    /// Com_Printf output for the host's console
    pub print_out: String,
    // ---- server browser
    pub local_servers: Vec<ServerInfo>,
    pub global_servers: Vec<ServerInfo>,
    /// `cls.favoriteServers` (the ui's favorites, kept in step by the host)
    pub favorite_servers: Vec<ServerInfo>,
    /// the visible flags of the local slots across a rescan
    local_visible: Vec<bool>,
    /// each local server's infoResponse without its challenge (the
    /// loopback / broadcast duplicate check, dec040)
    local_info_keys: Vec<String>,
    /// -1 while waiting for a master's answer
    pub num_global_servers: i32,
    /// `cls.numMasterPackets` / `cls.receivedMasterPackets`
    pub num_master_packets: i32,
    pub received_master_packets: u32,
    pub ping_update_source: i32,
    pub pinglist: Vec<Ping>,
    pub server_status: Option<ServerStatus>,
    pub master_names: Vec<String>,
    master_rx: Option<std::sync::mpsc::Receiver<(String, Option<NetAdr>)>>,
    // ---- rcon
    pub rcon_password: String,
    pub rcon_address: String,
    // ---- downloads and pure (dec037)
    pub download: Option<ClDownload>,
    /// `clc.downloadList`: "@remotename@localname@remotename@localname..."
    pub download_list: String,
    /// finished downloads without an fs_homepath: (local name, bytes)
    pub completed_downloads: Vec<(String, Vec<u8>)>,
    /// `clc.downloadRestart`: a pak arrived, FS_Restart before loading
    pub download_restart: bool,
    /// when "donedl" went out with no gamestate since (it is sent again
    /// while none comes: the server only resends a lost gamestate to a
    /// client on an older serverid, and a download keeps the same one)
    pub donedl_time: Option<i32>,
    /// `cl_allowDownload` (DLF_* flags, default 0)
    pub allow_download: i32,
    /// `com_downloadPrompt` / `com_downloadPromptText` (the ui's download_popmenu)
    pub download_prompt: i32,
    pub download_prompt_text: String,
    /// `clc.sv_allowDownload` / `clc.sv_dlURL` (CL_ParseServerInfo)
    pub sv_allow_download: i32,
    pub sv_dl_url: String,
    /// `cl_connectedToPureServer`
    pub connected_to_pure: bool,
    /// the client's search path (None: no pk3s; sheet maps, tests)
    pub fs: Option<PakSet>,
    /// `fs_homepath` (downloads go to <home>/<localName>)
    pub home_path: Option<std::path::PathBuf>,
    /// set by FS_Restart (a download arrived): the host rescans its pk3s
    pub fs_restarted: bool,
    // ---- demos (demo.rs)
    pub demo: super::demo::DemoState,
    rng: u32,
    /// the clc VoIP state (voip.rs; wt-b VoIP)
    pub voip: super::voip::ClVoip,
    // ---- protocol 69, vanilla 1.1.0 (dec040)
    /// the connection's protocol: 71 or 69
    pub protocol: i32,
    /// `cl_netProtocol`: NET_PROTOCOL_AUTO, 71 or 69
    pub net_protocol: i32,
    /// Auto: the realtime by which the server's infoResponse must be in
    probe_deadline: Option<i32>,
    /// the gamestate's configstrings as the 1.1 server sent them
    pub configstrings11: Vec<String>,
    /// the baselines and parse entities as the 1.1 server sent them (the
    /// delta bases; `entity_baselines` / `parse_entities` hold their
    /// legacy forms for the cgame)
    pub entity_baselines11: Vec<EntityState>,
    pub parse_entities11: Vec<EntityState>,
    /// legacy server commands a 1.1 command turned into beyond its first
    pending_server_commands: std::collections::VecDeque<Vec<String>>,
    /// both protocols were asked of the master: the answers are merged
    master_merge: bool,
}

/// The client engine's own reliable commands (cl_main.c / cl_parse.c:
/// disconnect, the pure checksums, downloads), which a 1.1.0 server's
/// engine reads as legacy's does; every other command is the game's and
/// goes through `v11::client_command_to_11` (dec040).
pub fn is_engine_command(cmd: &str) -> bool {
    let w = cmd.split_whitespace().next().unwrap_or("");
    matches!(w, "disconnect" | "cp" | "vdr" | "download" | "nextdl" | "stopdl" | "donedl")
}

/// `MAX_FEATLABEL_CHARS` (client.h).
pub const MAX_FEATLABEL_CHARS: usize = 32;

/// `QKEY_SIZE` (client.h).
pub const QKEY_SIZE: usize = 2048;

/// `Sys_Milliseconds`.
pub fn sys_milliseconds() -> i32 {
    use std::sync::OnceLock;
    static T0: OnceLock<std::time::Instant> = OnceLock::new();
    T0.get_or_init(std::time::Instant::now).elapsed().as_millis() as i32
}

/// "<dest>.tmp", the file a download is written to until it is verified.
fn download_tmp(dest: &std::path::Path) -> std::path::PathBuf {
    let mut t = dest.as_os_str().to_owned();
    t.push(".tmp");
    t.into()
}

#[cfg(test)]
mod download_name_tests {
    use super::*;
    use crate::kernel::files::PakSet;
    use std::io::Write;
    use std::path::{Path, PathBuf};

    fn scratch(tag: &str) -> PathBuf {
        let d = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/dlsec")
            .join(format!("{}_{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("home")).unwrap();
        d.canonicalize().unwrap()
    }

    /// Every file below `root`, relative, with / separators.
    fn files_under(root: &Path) -> Vec<String> {
        fn walk(d: &Path, root: &Path, out: &mut Vec<String>) {
            let Ok(rd) = std::fs::read_dir(d) else { return };
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, root, out);
                } else {
                    out.push(
                        p.strip_prefix(root)
                            .unwrap()
                            .to_string_lossy()
                            .replace('\\', "/"),
                    );
                }
            }
        }
        let mut v = Vec::new();
        walk(root, root, &mut v);
        v.sort();
        v
    }

    /// A stored pk3 and its checksum (what the server would reference).
    fn make_pk3(dir: &Path, payload: &[u8]) -> (Vec<u8>, i32) {
        let p = dir.join("src.zip");
        let f = std::fs::File::create(&p).unwrap();
        let mut z = zip::ZipWriter::new(f);
        z.start_file(
            "a.txt",
            zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored),
        )
        .unwrap();
        z.write_all(payload).unwrap();
        z.finish().unwrap();
        let sum = files::Pak::load(&p, "base").unwrap().checksum;
        (std::fs::read(&p).unwrap(), sum)
    }

    struct Run {
        cl: NetClient,
        list: Option<String>,
        root: PathBuf,
    }

    /// The server references `name` with checksum `sum`; the client (downloads
    /// on) runs the real path and the server answers the request with `body`
    /// (declared size `size`, then the zero-length end block).
    fn run(tag: &str, name: &str, sum: i32, size: i32, body: &[u8]) -> Run {
        let root = scratch(tag);
        let mut fs = PakSet::default();
        fs.pure_server_set_referenced_paks(&sum.to_string(), name);
        let list = fs.compare_paks(true, &|_| false);
        let mut cl = NetClient::new(1);
        cl.home_path = Some(root.join("home"));
        cl.allow_download = super::super::sv::DLF_ENABLE;
        cl.fs = Some(fs);
        let mut net = Net::new();
        if let Some(l) = list.clone() {
            cl.download = None;
            cl.download_list = l;
            cl.next_download();
            if cl.download.is_some() {
                let mut m = Msg::new(MAX_MSGLEN);
                m.write_short(0);
                m.write_long(size);
                m.write_short(body.len() as i32);
                m.write_data(body);
                m.begin_reading();
                cl.parse_download(&mut net, &mut m);
                let mut m = Msg::new(MAX_MSGLEN);
                m.write_short(1);
                m.write_short(0);
                m.begin_reading();
                cl.parse_download(&mut net, &mut m);
            }
        }
        Run { cl, list, root }
    }

    fn short(s: &str) -> String {
        let t: String = s.chars().take(70).collect();
        format!("{t:?}")
    }

    /// An invalid name must start nothing: no download list, no file anywhere.
    fn rejected(tag: &str, name: &str) -> Result<(), String> {
        let (body, sum) = (b"not-a-zip".to_vec(), 12345);
        let r = run(tag, name, sum, body.len() as i32, &body);
        let files = files_under(&r.root);
        if !files.is_empty() || r.list.is_some() {
            let l = r.list.as_deref().map(short);
            return Err(format!(
                "{tag}: name {} wrote {:?}, list {:?}, error {:?}",
                short(name),
                files.iter().map(|f| short(f)).collect::<Vec<_>>(),
                l,
                r.cl.error.as_deref().map(short)
            ));
        }
        Ok(())
    }

    fn assert_all_rejected(cases: &[(&str, &str)]) {
        let bad: Vec<String> = cases
            .iter()
            .filter_map(|(t, n)| rejected(t, n).err())
            .collect();
        assert!(
            bad.is_empty(),
            "{} of {} invalid names were accepted:\n{}",
            bad.len(),
            cases.len(),
            bad.join("\n")
        );
    }

    #[test]
    fn invalid_names_in_the_list_are_refused_at_use() {
        // a list handed straight to next_download, not built by compare_paks
        for (tag, list) in [
            ("fl1", "@a@b.txt"),
            ("fl2", "@/abs/x.pk3@/abs/x.pk3"),
            ("fl3", "@a.pk3@C:\\x.pk3"),
            ("fl4", "@x.pk3@../y.pk3"),
        ] {
            let root = scratch(tag);
            let mut cl = NetClient::new(1);
            cl.home_path = Some(root.join("home"));
            cl.allow_download = super::super::sv::DLF_ENABLE;
            cl.fs = Some(PakSet::default());
            cl.download_list = list.to_string();
            cl.next_download();
            assert!(cl.error.is_some(), "{list}: accepted");
            assert!(
                cl.download.is_none() && cl.download_list.is_empty(),
                "{list}: download started"
            );
            assert!(files_under(&root).is_empty());
        }
    }

    #[test]
    fn interrupted_download_is_deleted_on_disconnect() {
        let root = scratch("interrupt");
        let (zipbytes, sum) = make_pk3(&root, b"hello");
        let mut fs = PakSet::default();
        fs.pure_server_set_referenced_paks(&sum.to_string(), "slacker/part-1");
        let list = fs.compare_paks(true, &|_| false).unwrap();
        let mut cl = NetClient::new(1);
        cl.home_path = Some(root.join("home"));
        cl.allow_download = super::super::sv::DLF_ENABLE;
        cl.fs = Some(fs);
        cl.download_list = list;
        cl.next_download();
        let mut net = Net::new();
        let mut m = Msg::new(MAX_MSGLEN);
        m.write_short(0);
        m.write_long(zipbytes.len() as i32);
        m.write_short(10);
        m.write_data(&zipbytes[..10]);
        m.begin_reading();
        cl.parse_download(&mut net, &mut m);
        assert_eq!(
            files_under(&root.join("home")),
            vec!["slacker/part-1.pk3.tmp"]
        );
        cl.disconnect(&mut net);
        assert_eq!(files_under(&root.join("home")), Vec::<String>::new());
    }

    #[test]
    fn legit_names_still_download() {
        for (tag, name) in [
            ("legit1", "slacker/vms-1.1.0-bunker_220207-144309"),
            ("legit2", "gpp/map-arachnid2-1"),
            ("legit3", "base/data-x"),
        ] {
            let root0 = scratch(&format!("{tag}src"));
            let (zipbytes, sum) = make_pk3(&root0, b"hello");
            let r = run(tag, name, sum, zipbytes.len() as i32, &zipbytes);
            assert!(r.cl.error.is_none(), "{name}: {:?}", r.cl.error);
            assert_eq!(
                files_under(&r.root.join("home")),
                vec![format!("{name}.pk3")],
                "{name}"
            );
        }
    }

    #[test]
    fn absolute_path_is_rejected() {
        // the target lies in the test root but outside <root>/home
        let root = scratch("abs_target");
        let outside = root.join("outside").join("x");
        let name = outside.to_string_lossy().to_string();
        assert_all_rejected(&[("abs_target", &name)]);
        assert!(
            !outside.with_extension("pk3").exists(),
            "a file was written outside the download folder"
        );
    }

    #[cfg(unix)]
    #[test]
    fn absolute_path_example_is_rejected() {
        let r = rejected("abs_tmp", "/tmp/x");
        let written = Path::new("/tmp/x.pk3").exists();
        let _ = std::fs::remove_file("/tmp/x.pk3");
        assert!(!written, "wrote /tmp/x.pk3");
        r.unwrap();
    }

    #[test]
    fn windows_paths_are_rejected() {
        assert_all_rejected(&[
            ("win_drive", "C:\\x"),
            ("win_driverel", "C:x"),
            ("win_unc", "\\\\host\\share\\x"),
            ("win_lead_backslash", "\\x"),
            ("win_fwd_unc", "//host/share/x"),
            ("win_ads", "x:stream"),
            ("win_device", "con"),
        ]);
    }

    #[test]
    fn at_sign_is_rejected() {
        assert_all_rejected(&[("at1", "a@b.txt@c"), ("at2", "@b.txt@"), ("at3", "a@b")]);
    }

    #[test]
    fn traversal_forms_are_rejected() {
        assert_all_rejected(&[
            ("dd1", "../x"),
            ("dd2", "a/../../x"),
            ("dd3", "..\\x"),
            ("dd4", "a//b"),
            ("dd5", "./x"),
            ("dd6", "a/./b"),
            ("dd7", ".."),
            ("dd8", "a/.."),
            ("dd9", "a/"),
        ]);
    }

    #[test]
    fn only_pk3_names_are_accepted() {
        assert_all_rejected(&[("ext1", "a@b.txt@b"), ("ext2", "x@y.txt@z")]);
    }

    #[test]
    fn nul_and_control_characters_are_rejected() {
        assert_all_rejected(&[("nul", "a\0b"), ("ctl", "a\x07b"), ("del", "a\x7fb")]);
    }

    #[test]
    fn overlong_name_is_rejected() {
        assert_all_rejected(&[
            ("long", &"a".repeat(5000)),
            ("long2", &format!("{}/x", "d".repeat(300))),
        ]);
    }

    #[test]
    fn checksum_mismatch_leaves_nothing() {
        let root0 = scratch("mismatch_src");
        let (zipbytes, sum) = make_pk3(&root0, b"hello");
        let r = run(
            "mismatch",
            "slacker/pak-1",
            sum ^ 1,
            zipbytes.len() as i32,
            &zipbytes,
        );
        assert!(
            r.cl.error.is_some(),
            "a wrong checksum must drop the connection"
        );
        assert_eq!(files_under(&r.root.join("home")), Vec::<String>::new());
    }

    #[test]
    fn garbage_that_is_no_zip_leaves_nothing() {
        let r = run("garbage", "slacker/pak-2", 77, 7, b"not-a-zip");
        assert!(r.cl.error.is_some());
        assert_eq!(files_under(&r.root.join("home")), Vec::<String>::new());
    }
}

impl Default for NetClient {
    fn default() -> Self {
        Self::new(0)
    }
}

impl NetClient {
    pub fn new(qport: i32) -> Self {
        // Netchan_Init( Com_Milliseconds() & 0xffff ): one per process in the
        // C; several clients of one process (tests) must still differ
        static SEQ: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);
        let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let qport = if qport == 0 { (sys_milliseconds() ^ std::process::id() as i32 ^ n.wrapping_mul(7919)) & 0xffff } else { qport & 0xffff };
        NetClient {
            state: ConnState::Disconnected,
            servername: String::new(),
            server_address: NetAdr::default(),
            challenge: 0,
            connect_time: 0,
            connect_packet_count: 0,
            server_message: String::new(),
            qkey: None,
            guid_server_uniq: true,
            netchan: Netchan::setup(NetSrc::Client, NetAdr::default(), qport, 0),
            qport,
            reliable_sequence: 0,
            reliable_acknowledge: 0,
            reliable_commands: vec![String::new(); MAX_RELIABLE_COMMANDS],
            server_message_sequence: 0,
            server_command_sequence: 0,
            last_executed_server_command: 0,
            server_commands: vec![String::new(); MAX_RELIABLE_COMMANDS],
            last_packet_sent_time: 0,
            last_packet_time: 0,
            timeout_count: 0,
            checksum_feed: 0,
            client_num: 0,
            server_id: 0,
            configstrings: vec![String::new(); cs::MAX_CONFIGSTRINGS],
            entity_baselines: vec![EntityState::default(); MAX_GENTITIES as usize],
            gamestate_new: false,
            snap: ClSnapshot::default(),
            snapshots: vec![ClSnapshot::default(); PACKET_BACKUP],
            parse_entities: vec![EntityState::default(); MAX_PARSE_ENTITIES],
            parse_entities_num: 0,
            new_snapshots: false,
            cmds: vec![UserCmd::default(); CMD_BACKUP],
            cmd_number: 0,
            out_packets: vec![OutPacket::default(); PACKET_BACKUP],
            server_time: 0,
            old_server_time: 0,
            old_frame_server_time: 0,
            server_time_delta: 0,
            extrapolated_snapshot: false,
            realtime: 0,
            userinfo: String::new(),
            userinfo_modified: false,
            error: None,
            reconnect_args: String::new(),
            maxpackets: 30,
            packetdup: 1,
            time_nudge: 0,
            timeout: 200,
            nodelta: false,
            max_ping: 800,
            lan_force_packets: true,
            print_out: String::new(),
            local_servers: Vec::new(),
            favorite_servers: Vec::new(),
            local_visible: Vec::new(),
            local_info_keys: Vec::new(),
            global_servers: Vec::new(),
            num_global_servers: 0,
            num_master_packets: 0,
            received_master_packets: 0,
            ping_update_source: AS_LOCAL,
            pinglist: vec![Ping::default(); MAX_PINGREQUESTS],
            server_status: None,
            master_names: vec![super::sv::MASTER_SERVER_NAME.into(), "master.ioquake3.org".into()],
            master_rx: None,
            rcon_password: String::new(),
            rcon_address: String::new(),
            download: None,
            download_list: String::new(),
            completed_downloads: Vec::new(),
            download_restart: false,
            donedl_time: None,
            allow_download: 0,
            download_prompt: 0,
            download_prompt_text: String::new(),
            sv_allow_download: 0,
            sv_dl_url: String::new(),
            connected_to_pure: false,
            fs: None,
            home_path: None,
            fs_restarted: false,
            demo: Default::default(),
            rng: (sys_milliseconds() as u32 ^ 0x5bd1_e995) | 1,
            voip: super::voip::ClVoip::default(),
            protocol: PROTOCOL_VERSION,
            net_protocol: NET_PROTOCOL_AUTO,
            probe_deadline: None,
            configstrings11: vec![String::new(); cs::MAX_CONFIGSTRINGS],
            entity_baselines11: vec![EntityState::default(); MAX_GENTITIES as usize],
            parse_entities11: vec![EntityState::default(); MAX_PARSE_ENTITIES],
            pending_server_commands: Default::default(),
            master_merge: false,
        }
    }

    /// The connection speaks Tremulous 1.1.0's protocol 69 (dec040).
    pub fn vanilla(&self) -> bool {
        self.protocol == PROTOCOL_VANILLA
    }

    /// A configstring as the server sent it (the engine's own: serverinfo,
    /// systeminfo): the 1.1 one on a protocol 69 connection.
    pub fn engine_cs(&self, index: usize) -> String {
        if self.vanilla() {
            self.configstrings11[index].clone()
        } else {
            self.configstrings[index].clone()
        }
    }

    fn rand(&mut self) -> i32 {
        self.rng = self.rng.wrapping_mul(1103515245).wrapping_add(12345);
        (self.rng >> 1) as i32
    }

    pub(crate) fn printf(&mut self, s: &str) {
        self.print_out.push_str(s);
    }

    /// `CL_ClearState`: forget the gamestate and snapshots.
    pub fn clear_state(&mut self) {
        self.snap = ClSnapshot::default();
        self.snapshots = vec![ClSnapshot::default(); PACKET_BACKUP];
        self.parse_entities_num = 0;
        self.new_snapshots = false;
        self.cmd_number = 0;
        self.cmds.iter_mut().for_each(|c| *c = UserCmd::default());
        self.out_packets.iter_mut().for_each(|p| *p = OutPacket::default());
        self.server_time = 0;
        self.old_server_time = 0;
        self.old_frame_server_time = 0;
        self.server_time_delta = 0;
        self.extrapolated_snapshot = false;
        self.configstrings.iter_mut().for_each(|s| s.clear());
        self.entity_baselines.iter_mut().for_each(|b| *b = EntityState::default());
        self.configstrings11.iter_mut().for_each(|s| s.clear());
        self.entity_baselines11.iter_mut().for_each(|b| *b = EntityState::default());
        self.pending_server_commands.clear();
        self.timeout_count = 0;
    }

    // ============================================================ commands

    /// `CL_AddReliableCommand`. Err when an unacknowledged command would be lost.
    pub fn add_reliable_command(&mut self, cmd: &str, is_disconnect: bool) -> Result<(), String> {
        let unacknowledged = self.reliable_sequence - self.reliable_acknowledge;
        // a 1.1.0 server keeps 64 commands (its MAX_RELIABLE_COMMANDS)
        let max = if self.vanilla() { V11_MAX_RELIABLE_COMMANDS } else { MAX_RELIABLE_COMMANDS } as i32;
        // if we would be losing an old command that hasn't been acknowledged,
        // we must drop the connection; leave one slot for the disconnect
        if (is_disconnect && unacknowledged > max) || (!is_disconnect && unacknowledged >= max) {
            self.error = Some("Client command overflow".into());
            return Err("Client command overflow".into());
        }
        // protocol 69: the game's commands as the 1.1 server knows them
        // (v11.rs); the engine's own commands go as they are (dec040)
        let cmd = if self.vanilla() && !is_engine_command(cmd) {
            match v11::client_command_to_11(cmd) {
                Some(c) => c,
                None => return Ok(()),
            }
        } else {
            cmd.to_string()
        };
        self.reliable_sequence += 1;
        self.reliable_commands[self.reliable_sequence as usize & (MAX_RELIABLE_COMMANDS - 1)] = cmd.chars().take(msg::MAX_STRING_CHARS - 1).collect();
        Ok(())
    }

    /// `CL_ForwardCommandToServer`: a console line the client does not
    /// handle goes to the server as a reliable command (key-up "-" commands
    /// are not forwarded).
    pub fn forward_command(&mut self, line: &str) -> Result<(), String> {
        let line = line.trim();
        if line.starts_with('-') || line.is_empty() {
            return Ok(());
        }
        if self.state != ConnState::Active {
            return Err(format!("Unknown command \"{}\"", line.split_whitespace().next().unwrap_or("")));
        }
        self.add_reliable_command(line, false)
    }

    /// `CL_Connect_f`: `server` is "host[:port]" or "localhost".
    pub fn connect(&mut self, net: &mut Net, server: &str) -> Result<(), String> {
        self.connect_family(net, server, NetAdrType::Unspec)
    }

    /// `CL_Connect_f` with "-4" / "-6" (`family` NA_IP / NA_IP6).
    pub fn connect_family(&mut self, net: &mut Net, server: &str, family: NetAdrType) -> Result<(), String> {
        self.connect_protocol(net, server, family, NET_PROTOCOL_AUTO)
    }

    /// `CL_Connect_f` with a protocol (dec040): 71 or 69 forced, else
    /// `cl_netProtocol`; Auto asks the server for its info first and takes
    /// its `protocol` (69: vanilla 1.1.0, anything else: legacy), legacy
    /// when no answer comes within PROTOCOL_PROBE_TIME.
    pub fn connect_protocol(&mut self, net: &mut Net, server: &str, family: NetAdrType, protocol: i32) -> Result<(), String> {
        let mut args = match family {
            NetAdrType::Ip => format!("-4 {server}"),
            NetAdrType::Ip6 => format!("-6 {server}"),
            _ => server.to_string(),
        };
        if protocol == PROTOCOL_VANILLA || protocol == PROTOCOL_VERSION {
            args.push_str(&format!(" -{protocol}"));
        }
        let protocol = if protocol == NET_PROTOCOL_AUTO { self.net_protocol } else { protocol };
        self.reconnect_args = args;
        self.server_message.clear();
        self.disconnect(net);
        self.servername = server.to_string();
        let (r, mut adr) = netchan::string_to_adr(server, family);
        if r == 0 {
            self.state = ConnState::Disconnected;
            return Err("Bad server address".into());
        }
        if adr.port == 0 && adr.kind != NetAdrType::Loopback {
            adr.port = netchan::PORT_SERVER;
        }
        self.server_address = adr;
        let server_string = adr.to_string_port();
        self.printf(&format!("{server} resolved to {server_string}\n"));
        // CL_UpdateGUID: cl_guid is the MD5 of the qkey (after the server
        // address with cl_guidServerUniq); "" without a 2048-byte qkey
        if let Some(q) = self.qkey.as_ref() {
            let guid = if q.len() != QKEY_SIZE { String::new() } else { super::md5::md5_file(q, QKEY_SIZE, if self.guid_server_uniq { server_string.as_bytes() } else { b"" }) };
            svgame::info_set(&mut self.userinfo, "cl_guid", &guid);
        }
        // the protocol of this connection (dec040): our own server over
        // the loopback is legacy
        self.probe_deadline = None;
        self.protocol = PROTOCOL_VERSION;
        if !netchan::is_local_address(&adr) {
            if protocol == PROTOCOL_VANILLA {
                self.protocol = PROTOCOL_VANILLA;
            } else if protocol != PROTOCOL_VERSION {
                // Auto: the server's getinfo answer decides
                self.probe_deadline = Some(self.realtime + PROTOCOL_PROBE_TIME);
                net.out_of_band_print(NetSrc::Client, &adr, "getinfo xxx");
            }
        }
        // if we aren't playing on a lan, we need to authenticate
        if netchan::is_local_address(&adr) {
            self.state = ConnState::Challenging;
        } else {
            self.state = ConnState::Connecting;
            // a client challenge number that ideally is mirrored back by the server
            self.challenge = ((self.rand() << 16) ^ self.rand()) ^ sys_milliseconds();
        }
        // CL_CheckForResend() will fire immediately
        self.connect_time = -99999;
        self.connect_packet_count = 0;
        Ok(())
    }

    /// `CL_Disconnect`: sends "disconnect" three times when connected.
    pub fn disconnect(&mut self, net: &mut Net) {
        if self.demo.recording {
            let s = self.stop_record();
            self.printf(&s);
        }
        // back to base only (FS_ConditionalRestart when fs_game is cleared)
        let home = self.home_path.clone();
        if let (Some(fs), Some(h)) = (self.fs.as_mut(), home.as_ref()) {
            if fs.set_games(h, &[]) {
                let feed = fs.feed;
                fs.restart(feed);
                self.fs_restarted = true;
            }
        }
        // the download's temp file is closed and deleted
        self.discard_download();
        self.download_list.clear();
        self.download_restart = false;
        let playing = self.demo.playing;
        self.demo.close();
        self.voip.demoplaying = false;
        // send a disconnect message to the server
        // send it a few times in case one is dropped
        if self.state >= ConnState::Connected && !playing {
            let _ = self.add_reliable_command("disconnect", true);
            self.write_packet(net);
            self.write_packet(net);
            self.write_packet(net);
        }
        self.clear_state();
        // wipe the client connection
        self.state = ConnState::Disconnected;
        self.reliable_sequence = 0;
        self.reliable_acknowledge = 0;
        // CL_Disconnect memsets clc: the command rings too. A protocol-69
        // server XORs its messages with the acknowledged command's text, so a
        // stale "nextdl 126" left in slot 0 garbled the next server's gamestate
        // ("configstring > MAX_CONFIGSTRINGS" on a reconnect)
        self.reliable_commands.iter_mut().for_each(String::clear);
        self.server_commands.iter_mut().for_each(String::clear);
        self.server_command_sequence = 0;
        self.last_executed_server_command = 0;
        self.server_message_sequence = 0;
        self.gamestate_new = false;
        self.voip_disconnect(); // wt-b VoIP
        // the next connection (or demo) picks its protocol anew (dec040)
        self.protocol = PROTOCOL_VERSION;
        self.probe_deadline = None;
    }

    /// `CL_Rcon_f`.
    pub fn rcon(&mut self, net: &mut Net, command: &str) -> Result<(), String> {
        if self.rcon_password.is_empty() {
            return Err("You must set 'rconpassword' before\nissuing an rcon command.".into());
        }
        let to = if self.state >= ConnState::Connected {
            self.netchan.remote_address
        } else {
            if self.rcon_address.is_empty() {
                return Err("You must either be connected,\nor set the 'rconAddress' cvar\nto issue rcon commands".into());
            }
            let (_, mut a) = netchan::string_to_adr(&self.rcon_address, NetAdrType::Unspec);
            if a.port == 0 {
                a.port = netchan::PORT_SERVER;
            }
            a
        };
        let mut d = vec![0xffu8; 4];
        d.extend_from_slice(format!("rcon {} {}", self.rcon_password, command).as_bytes());
        d.push(0);
        net.send_packet(NetSrc::Client, &d, &to);
        Ok(())
    }

    // ============================================================ the frame

    /// `CL_Frame`'s download prompt check, once per frame of the ui (the
    /// host's frame, not every network tick): the ui's answer starts the
    /// downloads; a prompt no ui showed within a frame is ignored.
    pub fn download_prompt_frame(&mut self) {
        // We may have a download prompt ready
        if self.download_prompt & DLP_TYPE_MASK != 0 && self.download_prompt & DLP_PROMPTED == 0 {
            self.printf(&format!("Download prompt returned {}\n", self.download_prompt));
            self.next_download();
        } else if self.download_prompt & DLP_SHOW != 0 {
            // If the UI VM does not support the download prompt, we need to catch
            // the prompt here and replicate regular behavior.
            // One frame will always run between requesting and showing the prompt.
            if self.download_prompt & DLP_STALE != 0 {
                self.printf("WARNING: UI VM does not support download prompt\n");
                self.download_prompt = DLP_IGNORE;
                self.next_download();
            } else {
                self.download_prompt |= DLP_STALE;
            }
        }
    }

    /// One client frame up to the cgame: packets, userinfo, timeout,
    /// connection resends and the cgame time (`CL_Frame` without the
    /// command creation, which the host does with `create_cmd` + `send_cmd`).
    pub fn frame(&mut self, net: &mut Net, msec: i32) {
        self.realtime += msec;
        self.auto_record_frame();
        while let Some((from, data)) = net.get_packet(NetSrc::Client) {
            let mut m = Msg::from_bytes(&data);
            self.packet_event(net, &from, &mut m);
        }
        self.receive_masters(net);
        self.check_userinfo();
        self.check_timeout(net);
        self.check_for_resend(net);
        self.check_donedl_resend();
        self.set_cgame_time();
    }

    /// No gamestate a second after "donedl": it (or the gamestate) was
    /// lost, so ask again; a server answers every "donedl" from a client not
    /// yet active with the gamestate (SV_DoneDownload_f).
    fn check_donedl_resend(&mut self) {
        let Some(t) = self.donedl_time else { return };
        if self.state != ConnState::Connected {
            self.donedl_time = None;
            return;
        }
        if self.realtime - t >= 1000 {
            let _ = self.add_reliable_command("donedl", false);
            self.last_packet_sent_time = self.realtime - 1000;
            self.donedl_time = Some(self.realtime);
        }
    }

    /// `CL_CheckUserinfo`.
    fn check_userinfo(&mut self) {
        // don't add reliable commands when not yet connected
        if self.state < ConnState::Connected {
            return;
        }
        if self.userinfo_modified {
            self.userinfo_modified = false;
            let ui = self.userinfo.clone();
            let _ = self.add_reliable_command(&format!("userinfo \"{ui}\""), false);
        }
    }

    /// `CL_CheckTimeout`.
    fn check_timeout(&mut self, net: &mut Net) {
        if self.state >= ConnState::Connected && self.state != ConnState::Cinematic && self.realtime - self.last_packet_time > self.timeout * 1000 {
            self.timeout_count += 1;
            if self.timeout_count > 5 {
                self.printf("\nServer connection timed out.\n");
                self.error = Some("Server connection timed out.".into());
                self.disconnect(net);
            }
        } else {
            self.timeout_count = 0;
        }
    }

    /// `CL_CheckForResend`.
    fn check_for_resend(&mut self, net: &mut Net) {
        // don't send anything if playing back a demo
        if self.demo.playing {
            return;
        }
        // resend if we haven't gotten a reply yet
        if self.state != ConnState::Connecting && self.state != ConnState::Challenging {
            return;
        }
        // Auto: waiting for the server's protocol (dec040)
        if let Some(deadline) = self.probe_deadline {
            if self.realtime < deadline {
                return;
            }
            self.probe_deadline = None;
            self.protocol = PROTOCOL_VERSION;
            self.printf("No info from the server: trying protocol 71.\n");
            self.connect_time = -99999;
        }
        if self.realtime - self.connect_time < RETRANSMIT_TIMEOUT {
            return;
        }
        self.connect_time = self.realtime; // for retransmit requests
        self.connect_packet_count += 1;
        match self.state {
            ConnState::Connecting => {
                // requesting a challenge, with the client challenge and the
                // gamename; 1.1.0's CL_CheckForResend sends neither (dec040)
                let data = if self.vanilla() { "getchallenge".to_string() } else { format!("getchallenge {} {GAMENAME}", self.challenge) };
                net.out_of_band_print(NetSrc::Client, &self.server_address, &data);
            }
            _ => {
                // sending back the challenge
                let mut info = self.userinfo.clone();
                svgame::info_set(&mut info, "protocol", &self.protocol.to_string());
                svgame::info_set(&mut info, "qport", &self.qport.to_string());
                svgame::info_set(&mut info, "challenge", &self.challenge.to_string());
                let data = format!("connect \"{info}\"");
                net.out_of_band_data(NetSrc::Client, &self.server_address, data.as_bytes());
                // the most current userinfo has been sent, so watch for any
                // newer changes to userinfo variables
                self.userinfo_modified = false;
            }
        }
    }

    /// `CL_PacketEvent`.
    pub fn packet_event(&mut self, net: &mut Net, from: &NetAdr, m: &mut Msg) {
        self.last_packet_time = self.realtime;
        if m.cursize >= 4 && m.data[..4] == [0xff; 4] {
            self.connectionless_packet(net, from, m);
            return;
        }
        if self.state < ConnState::Connected {
            return; // can't be a valid sequenced packet
        }
        if m.cursize < 4 {
            self.printf(&format!("{}: Runt packet\n", from.to_string_port()));
            return;
        }
        // packet from server
        if !netchan::compare_adr(from, &self.netchan.remote_address) {
            return;
        }
        if !self.netchan.process(m) {
            return; // out of order, duplicated, etc
        }
        // 1.1.0's CL_Netchan_Process decodes the message (dec040)
        if self.vanilla() {
            v11net::cl_netchan_decode(m, self.challenge, &self.reliable_commands);
        }
        // the header is different lengths for reliable and unreliable messages
        let header_bytes = m.readcount;
        // track the last message received so it can be returned in
        // client messages, allowing the server to detect a dropped gamestate
        self.server_message_sequence = i32::from_le_bytes([m.data[0], m.data[1], m.data[2], m.data[3]]);
        self.last_packet_time = self.realtime;
        self.parse_server_message(net, m);
        // we don't know if it is ok to save a demo message until
        // after we have parsed the frame
        if self.demo.recording && !self.demo.waiting {
            self.write_demo_message(m, header_bytes);
        }
    }

    /// `CL_ConnectionlessPacket`.
    fn connectionless_packet(&mut self, net: &mut Net, from: &NetAdr, m: &mut Msg) {
        m.begin_reading_oob();
        m.read_long(); // skip the -1
        let s = m.read_string_line();
        let argv = super::svcmds::tokenize(&s);
        let c = argv.first().cloned().unwrap_or_default();
        let arg = |i: usize| argv.get(i).cloned().unwrap_or_default();
        match c.to_lowercase().as_str() {
            "challengeresponse" => {
                if self.state != ConnState::Connecting {
                    return; // Unwanted challenge response received. Ignored.
                }
                // 1.1.0's challengeResponse carries only the server's
                // challenge (dec040)
                if !self.vanilla() {
                    let c2 = arg(2);
                    let ver = arg(3);
                    if !ver.is_empty() && super::spawn::c_atoi(&ver) != PROTOCOL_VERSION {
                        self.printf(&format!("^3Warning: Server reports protocol version {ver}, we have {PROTOCOL_VERSION}. Trying anyways.\n"));
                    }
                    if c2.is_empty() || super::spawn::c_atoi(&c2) != self.challenge {
                        self.printf("Bad challenge for challengeResponse. Ignored.\n");
                        return;
                    }
                }
                // start sending challenge response instead of challenge request packets
                self.challenge = super::spawn::c_atoi(&arg(1));
                self.state = ConnState::Challenging;
                self.connect_packet_count = 0;
                self.connect_time = -99999;
                // take this address as the new server address.  This allows
                // a server proxy to hand off connections to multiple servers
                self.server_address = *from;
            }
            "connectresponse" => {
                if self.state >= ConnState::Connected {
                    return; // Dup connect received. Ignored.
                }
                if self.state != ConnState::Challenging {
                    return; // connectResponse packet while not connecting. Ignored.
                }
                if self.vanilla() {
                    // 1.1.0: NET_CompareBaseAdr, no challenge in the packet
                    if !netchan::compare_base_adr(from, &self.server_address) {
                        self.printf("connectResponse from a different address.  Ignored.\n");
                        return;
                    }
                } else {
                    if !netchan::compare_adr(from, &self.server_address) {
                        return; // connectResponse from wrong address. Ignored.
                    }
                    let c1 = arg(1);
                    if c1.is_empty() || super::spawn::c_atoi(&c1) != self.challenge {
                        self.printf("ConnectResponse with bad challenge received. Ignored.\n");
                        return;
                    }
                }
                self.netchan = Netchan::setup_protocol(NetSrc::Client, *from, self.qport, self.challenge, self.protocol);
                self.state = ConnState::Connected;
                self.last_packet_sent_time = -9999; // send first packet immediately
                self.last_packet_time = self.realtime;
            }
            // server responding to an info broadcast
            "inforesponse" => {
                // Auto: the server we connect to tells its protocol (dec040)
                if self.probe_deadline.is_some() && self.state == ConnState::Connecting && netchan::compare_adr(from, &self.server_address) {
                    let info = m.read_string();
                    let p = super::spawn::c_atoi(&svgame::info_value(&info, "protocol"));
                    self.protocol = if p == PROTOCOL_VANILLA { PROTOCOL_VANILLA } else { PROTOCOL_VERSION };
                    self.probe_deadline = None;
                    self.connect_time = -99999;
                    self.printf(&format!("Server protocol {p}: connecting with protocol {}\n", self.protocol));
                    return;
                }
                self.server_info_packet(from, m)
            }
            // server responding to a get playerlist
            "statusresponse" => self.server_status_response(from, m),
            // echo request from server
            "echo" => net.out_of_band_print(NetSrc::Client, from, &arg(1)),
            "print" => {
                let s = m.read_string();
                self.server_message = s.trim_end_matches('\n').to_string();
                self.printf(&s);
            }
            _ => {
                if c.starts_with("getserversResponse") {
                    self.servers_response_packet(from, m, false);
                } else if c.starts_with("getserversExtResponse") {
                    self.servers_response_packet(from, m, true);
                }
            }
        }
    }

    // ============================================================ parsing

    /// `CL_ParseServerMessage`.
    pub(crate) fn parse_server_message(&mut self, net: &mut Net, m: &mut Msg) {
        m.bitstream();
        // get the reliable sequence acknowledge number
        self.reliable_acknowledge = m.read_long();
        if self.reliable_acknowledge < self.reliable_sequence - MAX_RELIABLE_COMMANDS as i32 {
            self.reliable_acknowledge = self.reliable_sequence;
        }
        // parse the message
        loop {
            if m.readcount > m.cursize {
                self.drop_error("CL_ParseServerMessage: read past end of server message");
                break;
            }
            let cmd = m.read_byte();
            if cmd == svc::EOF {
                break;
            }
            match cmd {
                svc::NOP => {}
                svc::SERVER_COMMAND => self.parse_command_string(m),
                svc::GAMESTATE => self.parse_gamestate(m),
                svc::SNAPSHOT => self.parse_snapshot(m),
                svc::DOWNLOAD => self.parse_download(net, m),
                // wt-b VoIP: CL_ParseVoip (voip.rs); 1.1.0 has no VoIP
                svc::VOIP_SPEEX if !self.vanilla() => self.parse_voip(m, true),
                svc::VOIP_OPUS if !self.vanilla() => {
                    let ignore = !self.voip.enabled;
                    self.parse_voip(m, ignore)
                }
                _ => {
                    self.printf(&format!("CL_ParseServerMessage: unknown op {cmd} at byte {} of {}\n", m.readcount, m.cursize));
                    self.drop_error("CL_ParseServerMessage: Illegible server message");
                    break;
                }
            }
        }
    }

    /// `Com_Error( ERR_DROP, ... )` on the client: the connection ends.
    pub(crate) fn drop_error(&mut self, s: &str) {
        self.printf(&format!("{s}\n"));
        // the first reason stays: the rest of the message is skipped, not read as the next command
        if self.error.is_none() {
            self.error = Some(s.to_string());
        }
    }

    /// `CL_ParseCommandString`: saved until the cgame asks for them.
    fn parse_command_string(&mut self, m: &mut Msg) {
        let seq = m.read_long();
        let s = if self.vanilla() { m.read_string11() } else { m.read_string() };
        // see if we have already executed stored it off
        if self.server_command_sequence >= seq {
            return;
        }
        self.server_command_sequence = seq;
        self.server_commands[seq as usize & (MAX_RELIABLE_COMMANDS - 1)] = s;
    }

    /// `CL_ParseGamestate`.
    fn parse_gamestate(&mut self, m: &mut Msg) {
        self.connect_packet_count = 0;
        self.donedl_time = None;
        // wipe local client state
        self.clear_state();
        // a gamestate always marks a server command sequence
        self.server_command_sequence = m.read_long();
        // parse all the configstrings and baselines
        loop {
            let cmd = m.read_byte();
            if cmd == svc::EOF {
                break;
            }
            if cmd == svc::CONFIGSTRING {
                let i = m.read_short();
                if i < 0 || i as usize >= cs::MAX_CONFIGSTRINGS {
                    self.drop_error("configstring > MAX_CONFIGSTRINGS");
                    return;
                }
                if self.vanilla() {
                    // the 1.1 configstring and its legacy counterpart (v11.rs)
                    let s = m.read_big_string11();
                    for (li, ls) in v11::configstrings_from_11(i as usize, &s) {
                        if li < cs::MAX_CONFIGSTRINGS {
                            self.configstrings[li] = ls;
                        }
                    }
                    self.configstrings11[i as usize] = s;
                } else {
                    self.configstrings[i as usize] = m.read_big_string();
                }
            } else if cmd == svc::BASELINE {
                let newnum = m.read_bits(GENTITYNUM_BITS);
                if newnum < 0 || newnum >= MAX_GENTITIES {
                    self.drop_error(&format!("Baseline number out of range: {newnum}"));
                    return;
                }
                let null = EntityState::default();
                if self.vanilla() {
                    let b = m.read_delta_entity11(&null, newnum);
                    self.entity_baselines11[newnum as usize] = b;
                    self.entity_baselines[newnum as usize] = v11::es_from_11(&b);
                } else {
                    self.entity_baselines[newnum as usize] = m.read_delta_entity(&null, newnum);
                }
            } else {
                self.drop_error("CL_ParseGamestate: bad command byte");
                return;
            }
        }
        self.client_num = m.read_long();
        // this game version keeps MAX_CLIENTS players: refuse a server (and a player number) beyond that
        if let Some(why) = self.client_limit_error() {
            self.client_num = -1;
            self.drop_error(&why);
            return;
        }
        // read the checksum feed
        self.checksum_feed = m.read_long();
        // parse useful values out of CS_SERVERINFO
        self.parse_server_info();
        // parse serverId and other cvars
        self.system_info_changed();
        // stop recording now so the demo won't have an unnecessary level load at the end.
        if self.demo.auto_record && self.demo.recording {
            let s = self.stop_record();
            self.printf(&s);
        }
        let feed = self.checksum_feed;
        // FS_ConditionalRestart's fs_game switch: a mod server's folder joins
        // the search path (its downloaded paks live in "<fs_game>/")
        let game = svgame::info_value(&self.engine_cs(cs::SYSTEMINFO), "fs_game");
        let home = self.home_path.clone();
        if let Some(fs) = self.fs.as_mut() {
            // lowest priority first: the other referenced folders, then fs_game
            let mut games: Vec<String> = fs.referenced_games().into_iter().filter(|g| !g.eq_ignore_ascii_case(&game)).collect();
            games.push(game.clone());
            if home.as_ref().is_some_and(|h| fs.set_games(h, &games)) {
                fs.restart(feed);
                self.fs_restarted = true;
            } else {
                fs.conditional_restart(feed);
            }
        }
        // This used to call CL_StartHunkUsers, but now we enter the download state before loading the
        // cgame
        self.init_downloads();
    }

    /// A readable reason when the gamestate asks for more players than this game
    /// version holds (sv_maxclients, or the player number the server gave us).
    pub fn client_limit_error(&self) -> Option<String> {
        let limit = super::world::MAX_CLIENTS as i32;
        let si = self.engine_cs(cs::SERVERINFO);
        let max = super::spawn::c_atoi(&svgame::info_value(&si, "sv_maxclients"));
        if max > limit {
            return Some(format!("This server allows more players ({max}) than this game version supports ({limit})."));
        }
        if self.client_num < 0 || self.client_num >= limit {
            return Some(format!("The server gave this client the player number {}, outside the {limit} this game version supports.", self.client_num));
        }
        None
    }

    /// `CL_ParseServerInfo`.
    fn parse_server_info(&mut self) {
        let si = self.engine_cs(cs::SERVERINFO);
        self.sv_allow_download = super::spawn::c_atoi(&svgame::info_value(&si, "sv_allowDownload"));
        self.sv_dl_url = svgame::info_value(&si, "sv_dlURL");
    }

    /// `CL_SystemInfoChanged` (the parts this client uses): the serverId,
    /// and the pure server's pak lists (`FS_PureServerSetLoadedPaks`,
    /// `FS_PureServerSetReferencedPaks`).
    pub(crate) fn system_info_changed(&mut self) {
        let si = self.engine_cs(cs::SYSTEMINFO);
        self.server_id = super::spawn::c_atoi(&svgame::info_value(&si, "sv_serverid"));
        // don't set any vars when playing a demo
        if self.demo.playing {
            return;
        }
        let v = |k: &str| svgame::info_value(&si, k);
        if let Some(fs) = self.fs.as_mut() {
            // check pure server string
            fs.pure_server_set_loaded_paks(&v("sv_paks"), &v("sv_pakNames"));
            fs.pure_server_set_referenced_paks(&v("sv_referencedPaks"), &v("sv_referencedPakNames"));
        }
        self.connected_to_pure = super::spawn::c_atoi(&v("sv_pure")) != 0;
        self.voip_system_info_changed(&si); // wt-b VoIP
    }

    /// `FS_SV_FileExists` in fs_homepath.
    fn home_file_exists(&self, name: &str) -> bool {
        self.home_path.as_ref().is_some_and(|h| h.join(name).exists())
    }

    /// `CL_InitDownloads`: after receiving a valid game state, compare the
    /// referenced paks and determine if we need to download them.
    fn init_downloads(&mut self) {
        let need = match self.fs.as_ref() {
            Some(fs) if !self.demo.playing => fs.compare_paks(true, &|n| self.home_file_exists(n)),
            _ => None,
        };
        if let Some(list) = need {
            self.printf(&format!("Need paks: {list}\n"));
            self.download_prompt = 0;
            if !list.is_empty() {
                // if autodownloading is not enabled on the server
                self.state = ConnState::Connected;
                self.discard_download();
                self.download_list = list;
                self.next_download();
                return;
            }
        }
        self.downloads_complete();
    }

    /// `CL_DownloadsComplete`: FS_Restart and "donedl" after a download
    /// (the server sends the gamestate again), else CA_LOADING: the host
    /// loads the map and calls `cgame_loaded`.
    fn downloads_complete(&mut self) {
        self.printf("Downloads complete\n");
        // if we downloaded files we need to restart the file system
        if self.download_restart {
            self.download_restart = false;
            // We possibly downloaded a pak, restart the file system to load it
            let feed = self.checksum_feed;
            if let Some(fs) = self.fs.as_mut() {
                fs.restart(feed);
            }
            self.fs_restarted = true;
            // inform the server so we get new gamestate info
            let _ = self.add_reliable_command("donedl", false);
            // send it with the next frame rather than within the once a
            // second CL_ReadyToSendPacket allows without a gamestate
            self.last_packet_sent_time = self.realtime - 1000;
            self.donedl_time = Some(self.realtime);
            // by sending the donedl command we request a new gamestate
            // so we don't want to load stuff yet
            return;
        }
        // let the client game init and load data
        self.state = ConnState::Loading;
        self.gamestate_new = true;
    }

    /// The host has loaded the map (`CL_InitCGame`, which also reloads the
    /// ui): the modules' and the map's paks are referenced, the pure
    /// checksums go out (`CL_SendPureChecksums`), CA_PRIMED, and a few
    /// packets so the server knows.
    pub fn cgame_loaded(&mut self, net: &mut Net) {
        self.gamestate_new = false;
        self.state = ConnState::Primed;
        let map = svgame::info_value(&self.engine_cs(cs::SERVERINFO), "mapname");
        let mut empty = PakSet { feed: self.checksum_feed, ..Default::default() };
        let fs = self.fs.as_mut().unwrap_or(&mut empty);
        fs.reference_module(&files::UI_MODULE, files::FS_UI_REF);
        fs.reference_module(&files::CGAME_MODULE, files::FS_CGAME_REF);
        fs.reference(&format!("maps/{map}.bsp"));
        // if we are pure we need to send back a command with our referenced pk3 checksums
        let cp = format!("cp {} {}", self.server_id, fs.referenced_pak_pure_checksums());
        if !self.demo.playing {
            let _ = self.add_reliable_command(&cp, false);
            self.write_packet(net);
            self.write_packet(net);
            self.write_packet(net);
        }
    }

    /// `CL_DeltaEntity`.
    fn delta_entity(&mut self, m: &mut Msg, frame: &mut ClSnapshot, newnum: i32, old: &EntityState, unchanged: bool) {
        let state = if unchanged {
            *old
        } else if self.vanilla() {
            m.read_delta_entity11(old, newnum)
        } else {
            m.read_delta_entity(old, newnum)
        };
        if state.number == MAX_GENTITIES - 1 {
            return; // entity was delta removed
        }
        let k = self.parse_entities_num & (MAX_PARSE_ENTITIES - 1);
        if self.vanilla() {
            // keep the 1.1 state as the delta base, the legacy one for the cgame
            self.parse_entities11[k] = state;
            self.parse_entities[k] = v11::es_from_11(&state);
        } else {
            self.parse_entities[k] = state;
        }
        self.parse_entities_num += 1;
        frame.num_entities += 1;
    }

    /// `CL_ParsePacketEntities`.
    fn parse_packet_entities(&mut self, m: &mut Msg, oldframe: Option<&ClSnapshot>, newframe: &mut ClSnapshot) {
        newframe.parse_entities_num = self.parse_entities_num;
        newframe.num_entities = 0;
        // delta from the entities present in oldframe
        let mut oldindex = 0usize;
        let base = if self.vanilla() { &self.parse_entities11 } else { &self.parse_entities };
        let old_ents: Vec<EntityState> = match oldframe {
            Some(f) => (0..f.num_entities).map(|k_| base[(f.parse_entities_num + k_) & (MAX_PARSE_ENTITIES - 1)]).collect(),
            None => Vec::new(),
        };
        let oldnum = |oi: usize| old_ents.get(oi).map(|e| e.number).unwrap_or(99999);
        loop {
            // read the entity index number
            let newnum = m.read_bits(GENTITYNUM_BITS);
            if newnum == MAX_GENTITIES - 1 {
                break;
            }
            if m.readcount > m.cursize {
                self.drop_error("CL_ParsePacketEntities: end of message");
                return;
            }
            while oldnum(oldindex) < newnum {
                // one or more entities from the old packet are unchanged
                let o = old_ents[oldindex];
                self.delta_entity(m, newframe, o.number, &o, true);
                oldindex += 1;
            }
            if oldnum(oldindex) == newnum {
                // delta from previous state
                let o = old_ents[oldindex];
                self.delta_entity(m, newframe, newnum, &o, false);
                oldindex += 1;
                continue;
            }
            if oldnum(oldindex) > newnum {
                // delta from baseline
                let b = if self.vanilla() { self.entity_baselines11[newnum as usize] } else { self.entity_baselines[newnum as usize] };
                self.delta_entity(m, newframe, newnum, &b, false);
                continue;
            }
        }
        // any remaining entities in the old frame are copied over
        while oldnum(oldindex) != 99999 {
            // one or more entities from the old packet are unchanged
            let o = old_ents[oldindex];
            self.delta_entity(m, newframe, o.number, &o, true);
            oldindex += 1;
        }
    }

    /// `CL_ParseSnapshot`.
    fn parse_snapshot(&mut self, m: &mut Msg) {
        let mut new_snap = ClSnapshot { server_command_num: self.server_command_sequence, ..Default::default() };
        new_snap.server_time = m.read_long();
        new_snap.message_num = self.server_message_sequence;
        let delta_num = m.read_byte();
        new_snap.delta_num = if delta_num == 0 { -1 } else { new_snap.message_num - delta_num };
        new_snap.snap_flags = m.read_byte();
        // If the frame is delta compressed from data that we
        // no longer have available, we must suck up the rest of
        // the frame, but not use it, then ask for a non-compressed message
        let old = if new_snap.delta_num <= 0 {
            new_snap.valid = true; // uncompressed frame
            self.demo.waiting = false; // we can start recording now
            None
        } else {
            let o = self.snapshots[new_snap.delta_num as usize & PACKET_MASK].clone();
            if !o.valid {
                // should never happen
                self.printf("Delta from invalid frame (not supposed to happen!).\n");
            } else if o.message_num != new_snap.delta_num {
                // The frame that the server did the delta from
                // is too old, so we can't reconstruct it properly.
            } else if self.parse_entities_num - o.parse_entities_num > MAX_PARSE_ENTITIES - MAX_SNAPSHOT_ENTITIES {
                // Delta parseEntitiesNum too old.
            } else {
                new_snap.valid = true; // valid delta parse
            }
            Some(o)
        };
        // read areamask
        let len = m.read_byte();
        if len < 0 || len as usize > MAX_MAP_AREA_BYTES {
            self.drop_error(&format!("CL_ParseSnapshot: Invalid size {len} for areamask"));
            return;
        }
        let a = m.read_data(len as usize);
        new_snap.areamask[..a.len()].copy_from_slice(&a);
        // read playerinfo
        if self.vanilla() {
            // the 1.1 playerState is the delta base; the cgame gets its legacy form
            new_snap.ps11 = m.read_delta_playerstate11(old.as_ref().map(|o| &o.ps11));
            new_snap.ps = v11::ps_from_11(&new_snap.ps11);
        } else {
            new_snap.ps = m.read_delta_playerstate(old.as_ref().map(|o| &o.ps));
        }
        // read packet entities
        self.parse_packet_entities(m, old.as_ref(), &mut new_snap);
        // if not valid, dump the entire thing now that it has
        // been properly read
        if !new_snap.valid {
            return;
        }
        // clear the valid flags of any snapshots between the last
        // received and this one, so if there was a dropped packet
        // it won't look like something valid to delta from next time
        let mut old_message_num = self.snap.message_num + 1;
        if new_snap.message_num - old_message_num >= PACKET_BACKUP as i32 {
            old_message_num = new_snap.message_num - (PACKET_BACKUP as i32 - 1);
        }
        while old_message_num < new_snap.message_num {
            self.snapshots[old_message_num as usize & PACKET_MASK].valid = false;
            old_message_num += 1;
        }
        // copy to the current good spot
        self.snap = new_snap;
        self.snap.ping = 999;
        // calculate ping time
        for i in 0..PACKET_BACKUP {
            let pn = (self.netchan.outgoing_sequence - 1 - i as i32) as usize & PACKET_MASK;
            if self.snap.ps.command_time >= self.out_packets[pn].p_server_time {
                self.snap.ping = self.realtime - self.out_packets[pn].p_realtime;
                break;
            }
        }
        // save the frame off in the backup array for later delta comparisons
        self.snapshots[self.snap.message_num as usize & PACKET_MASK] = self.snap.clone();
        self.new_snapshots = true;
    }

    /// `CL_ParseDownload`: a block of the requested file; block zero
    /// holds the size (or -1 and the server's refusal). The data goes to
    /// "<fs_homepath>/<localName>.tmp", renamed at the zero-length block.
    fn parse_download(&mut self, net: &mut Net, m: &mut Msg) {
        let Some(mut d) = self.download.take() else {
            // a block resent before the server saw the last ack can come in
            // after the download is done; read past it (the C client did not,
            // and misread the rest of the message as an illegible one)
            if m.read_short() == 0 && m.read_long() < 0 {
                m.read_string();
            } else {
                let size = m.read_short();
                if (0..=MAX_MSGLEN as i32).contains(&size) {
                    m.read_data(size as usize);
                }
            }
            self.printf("Server sending download, but no download was requested\n");
            let _ = self.add_reliable_command("stopdl", false);
            return;
        };
        // read the data
        let block = m.read_short() as u16 as i32;
        // block zero always carries the file size (the server writes it
        // whenever downloadXmitBlock is 0) and is resent until acknowledged;
        // ioq3's `!block && !clc.downloadBlock` skipped the size of a resent
        // block 0 and misread the rest. Tremulous 1.1.0's client tests `!block`.
        if block == 0 && d.block < 0x10000 {
            // block zero is special, contains file size
            d.size = m.read_long();
            if d.size < 0 {
                let e = m.read_string();
                self.abort_download(d, &e);
                return;
            }
        }
        let size = m.read_short();
        if size < 0 || size as usize > MAX_MSGLEN {
            self.abort_download(
                d,
                &format!("CL_ParseDownload: Invalid size {size} for download chunk"),
            );
            return;
        }
        let data = m.read_data(size as usize);
        if (d.block & 0xffff) != block {
            // CL_ParseDownload: Expected block d.block, got block. The C
            // client drops it; keep one within the server's window instead,
            // so the resend after a loss only has to fill the gap
            let ahead = (block - (d.block & 0xffff)) & 0xffff;
            if d.block >= 0 && (1..MAX_DOWNLOAD_AHEAD).contains(&ahead) {
                d.ahead.entry(d.block + ahead).or_insert(data);
            }
            self.download = Some(d);
            self.take_download_blocks_ahead(net);
            return;
        }
        self.download = Some(d);
        self.take_download_block(net, data);
        self.take_download_blocks_ahead(net);
    }

    /// The blocks kept from ahead of a gap, now next in turn, while there is
    /// room for their reliable "nextdl" (an overflow would drop the
    /// connection); the rest wait for the next download packet.
    fn take_download_blocks_ahead(&mut self, net: &mut Net) {
        loop {
            let max = if self.vanilla() { V11_MAX_RELIABLE_COMMANDS } else { MAX_RELIABLE_COMMANDS } as i32;
            if self.reliable_sequence - self.reliable_acknowledge >= max - 8 {
                return;
            }
            let Some(d) = self.download.as_mut() else { return };
            let block = d.block;
            let Some(data) = d.ahead.remove(&block) else { return };
            self.take_download_block(net, data);
        }
    }

    /// The next block of the download, in turn: written, acknowledged, and
    /// at the zero-length block the file is done.
    fn take_download_block(&mut self, net: &mut Net, data: Vec<u8>) {
        let Some(mut d) = self.download.take() else { return };
        let size = data.len() as i32;
        // the data goes to "<home>/<local>.tmp"; the name was checked when the list was
        // built, and again here, at the one place a path is made from it
        let dest = match self.home_path.as_deref() {
            Some(h) => match files::download_dest(h, &d.local_name) {
                Some(p) => Some(p),
                None => {
                    let msg = format!(
                        "Refusing to download to an unsafe name ({} bytes of name)",
                        d.local_name.len()
                    );
                    self.abort_download(d, &msg);
                    return;
                }
            },
            None => None,
        };
        let tmp = dest.as_ref().map(|p| download_tmp(p));
        // open the file if not opened yet
        if d.file.is_none() {
            if let Some(t) = tmp.as_ref() {
                if let Some(dir) = t.parent() {
                    let _ = std::fs::create_dir_all(dir);
                }
                match std::fs::File::create(t) {
                    Ok(f) => d.file = Some(f),
                    Err(_) => {
                        self.printf(&format!("Could not create {}.tmp\n", d.local_name));
                        self.discard_partial(&d);
                        let _ = self.add_reliable_command("stopdl", false);
                        self.next_download();
                        return;
                    }
                }
            }
        }
        if size > 0 {
            match d.file.as_mut() {
                Some(f) => {
                    use std::io::Write;
                    if f.write_all(&data).is_err() {
                        let msg = format!(
                            "Download of {} failed: could not write the file",
                            d.local_name
                        );
                        self.abort_download(d, &msg);
                        return;
                    }
                }
                None => d.data.extend_from_slice(&data),
            }
        }
        let _ = self.add_reliable_command(&format!("nextdl {}", d.block), false);
        d.block += 1;
        d.count += size;
        if size == 0 {
            // A zero length block means EOF
            if d.file.take().is_some() {
                if let (Some(t), Some(dest)) = (tmp, dest) {
                    // the pak must be one the server referenced, checked BEFORE it takes its
                    // place under the final name
                    if let Some(fs) = self.fs.as_ref() {
                        if !fs.compare_zip_checksum(&t) {
                            let msg = format!("Incorrect checksum for file: {}", d.local_name);
                            let _ = std::fs::remove_file(&t);
                            self.abort_download(d, &msg);
                            return;
                        }
                    }
                    // rename the file
                    if std::fs::rename(&t, &dest).is_err() {
                        let _ = std::fs::remove_file(&t);
                        let msg = format!(
                            "Download of {} failed: could not move the file into place",
                            d.local_name
                        );
                        self.abort_download(d, &msg);
                        return;
                    }
                }
            } else {
                self.completed_downloads.push((d.local_name.clone(), std::mem::take(&mut d.data)));
            }
            self.printf(&format!("Downloaded {} ({} bytes)\n", d.local_name, d.count));
            // the finished download's name stays for CL_NextDownload's checksum check
            let finished = d.local_name.clone();
            self.download = Some(ClDownload { local_name: finished, block: -1, ..Default::default() });
            // send intentions now
            // We need this because without it, we would hold the last nextdl and then start
            // loading right away.  If we take a while to load, the server is happily trying
            // to send us that last block over and over.
            // Write it twice to help make sure we acknowledge the download
            self.write_packet(net);
            self.write_packet(net);
            // get another file if needed
            self.next_download();
            return;
        }
        self.download = Some(d);
    }

    /// Deletes the partial file of a download in progress ("<local>.tmp").
    fn discard_partial(&mut self, d: &ClDownload) {
        if d.local_name.is_empty() || d.block < 0 {
            return;
        }
        if let Some(dest) = self
            .home_path
            .as_deref()
            .and_then(|h| files::download_dest(h, &d.local_name))
        {
            let _ = std::fs::remove_file(download_tmp(&dest));
        }
    }

    /// The download in progress is abandoned: its partial file is deleted.
    fn discard_download(&mut self) {
        if let Some(mut d) = self.download.take() {
            d.file = None;
            self.discard_partial(&d);
        }
    }

    /// A download failed or was refused: close and delete the partial file,
    /// forget the rest of the list and drop the connection with `msg`.
    fn abort_download(&mut self, mut d: ClDownload, msg: &str) {
        d.file = None;
        self.discard_partial(&d);
        self.download_list.clear();
        self.drop_error(msg);
    }

    /// `CL_BeginDownload`: requests a file to download from the server.
    pub fn begin_download(&mut self, local_name: &str, remote_name: &str) {
        self.download = Some(ClDownload { local_name: local_name.to_string(), remote_name: remote_name.to_string(), time: self.realtime, ..Default::default() });
        let _ = self.add_reliable_command(&format!("download {remote_name}"), false);
    }

    /// `CL_NextDownload`: a download completed or failed; the next file of
    /// the list (after the download prompt, cl_allowDownload), or
    /// `CL_DownloadsComplete`. This port has no cURL (a USE_CURL-less build).
    pub fn next_download(&mut self) {
        // A download has finished, check whether this matches a referenced checksum
        if let Some(d) = self.download.take() {
            if !d.local_name.is_empty() && d.block == -1 {
                if let (Some(fs), Some(h)) = (self.fs.as_ref(), self.home_path.as_ref()) {
                    if let Some(p) = files::download_dest(h, &d.local_name) {
                        if !fs.compare_zip_checksum(&p) {
                            let _ = std::fs::remove_file(&p);
                            self.download_list.clear();
                            self.drop_error(&format!(
                                "Incorrect checksum for file: {}",
                                d.local_name
                            ));
                            return;
                        }
                    }
                }
            }
        }
        // We are looking to start a download here
        if !self.download_list.is_empty() {
            // Prompt if we do not allow automatic downloads
            let prompt = self.download_prompt;
            if prompt & DLP_TYPE_MASK == 0 && self.allow_download & super::sv::DLF_ENABLE == 0 {
                // Make a pretty version of the download list
                let names: Vec<&str> = self.download_list.trim_start_matches('@').split('@').step_by(2).collect();
                let max_list = 12;
                let mut files_s = names.iter().take(max_list).copied().collect::<Vec<_>>().join(", ");
                let others = names.len().saturating_sub(max_list);
                if others > 0 {
                    files_s.push_str(&format!("({others} other file{})\n", if others > 1 { "s" } else { "" }));
                }
                // Set the pure message
                let pure_msg = if self.connected_to_pure {
                    if self.sv_allow_download & super::sv::DLF_ENABLE == 0 || (self.sv_allow_download & super::sv::DLF_NO_UDP != 0 && self.sv_allow_download & super::sv::DLF_NO_REDIRECT != 0) {
                        "You are missing files required by the server. The server does not allow downloading. You must install these files manually:"
                    } else {
                        "You are missing files required by the server. You must download these files or disconnect:"
                    }
                } else {
                    "You are missing optional files provided by the server. You may not need them to play but can choose to download them anyway:"
                };
                self.download_prompt_text = format!("{pure_msg}\n\n{files_s}\n");
                self.download_prompt = DLP_SHOW;
                return;
            }
            if prompt & DLP_PROMPTED == 0 {
                self.download_prompt = prompt | DLP_PROMPTED;
            }
            let prompt = prompt & DLP_TYPE_MASK;
            // format is:
            //  @remotename@localname@remotename@localname, etc.
            let s = self.download_list.strip_prefix('@').unwrap_or(&self.download_list).to_string();
            let Some((remote, rest)) = s.split_once('@') else {
                self.downloads_complete();
                return;
            };
            let (local, rest) = match rest.split_once('@') {
                Some((l, r)) => (l.to_string(), format!("@{r}")),
                None => (rest.to_string(), String::new()),
            };
            let remote = remote.to_string();
            // both names must be plain "<dir>/<name>.pk3" (the list is built from the
            // server's names)
            if !files::safe_download_name(&local) || !files::safe_download_name(&remote) {
                self.download_list.clear();
                self.drop_error("Refusing to download a file with an unsafe name");
                return;
            }
            self.printf(&format!("Trying UDP download: {local}; {remote}\n"));
            if (self.allow_download & super::sv::DLF_ENABLE == 0 || self.allow_download & super::sv::DLF_NO_UDP != 0) && prompt != DLP_UDP {
                if self.connected_to_pure {
                    self.drop_error(&format!("Automatic downloads are disabled on your client (cl_allowDownload is {}). You can enable automatic downloads in the Options menu.", self.allow_download));
                    return;
                }
                self.printf("WARNING: UDP downloads are disabled.\n");
                self.download_list.clear();
                self.downloads_complete();
                return;
            }
            self.begin_download(&local, &remote);
            self.download_restart = true;
            // move over the rest
            self.download_list = rest;
            return;
        }
        self.downloads_complete();
    }

    // ============================================================ outgoing

    /// `CL_CreateNewCommands` with the host's `CL_CreateCmd` result: stamped
    /// with cl.serverTime.
    pub fn create_cmd(&mut self, mut cmd: UserCmd) {
        // no need to create usercmds until we have a gamestate
        if self.state < ConnState::Primed {
            return;
        }
        cmd.server_time = self.server_time;
        if self.vanilla() {
            // 1.1 sprints with the "boost" command, not a button (v11.rs)
            let prev = self.cmds[self.cmd_number as usize & CMD_MASK].buttons;
            if let Some(c) = v11::boost_command(prev, cmd.buttons) {
                let _ = self.add_reliable_command(c, false);
            }
        }
        self.cmd_number += 1;
        self.cmds[self.cmd_number as usize & CMD_MASK] = cmd;
    }

    /// `CL_ReadyToSendPacket`.
    fn ready_to_send_packet(&self) -> bool {
        // if we don't have a valid gamestate yet, only send
        // one packet a second
        if self.state != ConnState::Active && self.state != ConnState::Primed && self.download.as_ref().map_or(true, |d| d.local_name.is_empty()) && self.realtime - self.last_packet_sent_time < 1000 {
            return false;
        }
        // send every frame for loopbacks
        if self.netchan.remote_address.kind == NetAdrType::Loopback {
            return true;
        }
        // send every frame while downloading (port: the original held 50 ms
        // between packets), so the "nextdl" acks that pace the server's
        // download window go out at once
        if self.download.as_ref().is_some_and(|d| !d.local_name.is_empty() && d.block >= 0) {
            return true;
        }
        // send every frame for LAN
        if self.lan_force_packets && netchan::is_lan_address(&self.netchan.remote_address) {
            return true;
        }
        // check for exceeding cl_maxpackets
        let maxp = self.maxpackets.clamp(15, 125);
        let old = (self.netchan.outgoing_sequence - 1) as usize & PACKET_MASK;
        let delta = self.realtime - self.out_packets[old].p_realtime;
        delta >= 1000 / maxp
    }

    /// `CL_SendCmd`.
    pub fn send_cmd(&mut self, net: &mut Net) {
        // don't send any message if not connected
        if self.state < ConnState::Connected {
            return;
        }
        // don't send anything if playing back a demo
        if self.demo.playing {
            return;
        }
        // don't send a packet if the last packet was sent too recently
        if !self.ready_to_send_packet() {
            return;
        }
        self.write_packet(net);
    }

    /// `CL_WritePacket`.
    pub fn write_packet(&mut self, net: &mut Net) {
        // don't send anything if playing back a demo
        if self.demo.playing {
            return;
        }
        let mut buf = Msg::new(MAX_MSGLEN);
        buf.bitstream();
        // write the current serverId so the server
        // can tell if this is from the current gameState
        buf.write_long(self.server_id);
        // write the last message we received, which can
        // be used for delta compression, and is also used
        // to tell if we dropped a gamestate
        buf.write_long(self.server_message_sequence);
        // write the last reliable message we received
        buf.write_long(self.server_command_sequence);
        // write any unacknowledged clientCommands
        for i in self.reliable_acknowledge + 1..=self.reliable_sequence {
            buf.write_byte(clc::CLIENT_COMMAND);
            buf.write_long(i);
            buf.write_string(&self.reliable_commands[i as usize & (MAX_RELIABLE_COMMANDS - 1)]);
        }
        // we want to send all the usercmds that were generated in the last
        // few packet, so even if a couple packets are dropped in a row,
        // all the cmds will make it to the server
        let dup = self.packetdup.clamp(0, 5);
        let old_packet = (self.netchan.outgoing_sequence - 1 - dup) as usize & PACKET_MASK;
        let mut count = self.cmd_number - self.out_packets[old_packet].p_cmd_number;
        if count > MAX_PACKET_USERCMDS {
            count = MAX_PACKET_USERCMDS;
        }
        if !self.vanilla() {
            self.write_voip(&mut buf); // wt-b VoIP: the clc_voipOpus block
        }
        let mut oldcmd = UserCmd::default();
        if count >= 1 && self.state >= ConnState::Primed {
            // begin a client move command
            if self.nodelta || !self.snap.valid || self.demo.waiting || self.server_message_sequence != self.snap.message_num {
                buf.write_byte(clc::MOVE_NO_DELTA);
            } else {
                buf.write_byte(clc::MOVE);
            }
            // write the command count
            buf.write_byte(count);
            // use the checksum feed in the key
            let mut key = self.checksum_feed;
            // also use the message acknowledge
            key ^= self.server_message_sequence;
            // also use the last acknowledged server command in the key
            let last = &self.server_commands[self.server_command_sequence as usize & (MAX_RELIABLE_COMMANDS - 1)];
            key ^= if self.vanilla() { v11net::com_hash_key(last.as_bytes(), 32) } else { msg::hash_key(last, 32) };
            // write all the commands, including the predicted command
            for i in 0..count {
                let j = (self.cmd_number - count + i + 1) as usize & CMD_MASK;
                // protocol 69: the usercmd as the 1.1 server reads it (v11.rs)
                let cmd = if self.vanilla() { v11::usercmd_to_11(&self.cmds[j]) } else { self.cmds[j] };
                buf.write_delta_usercmd_key(key, &oldcmd, &cmd);
                oldcmd = cmd;
            }
        }
        // deliver the message
        let pn = self.netchan.outgoing_sequence as usize & PACKET_MASK;
        self.out_packets[pn] = OutPacket { p_realtime: self.realtime, p_server_time: oldcmd.server_time, p_cmd_number: self.cmd_number };
        self.last_packet_sent_time = self.realtime;
        // CL_Netchan_Transmit
        buf.write_byte(clc::EOF);
        // 1.1.0's CL_Netchan_Encode (dec040)
        if self.vanilla() {
            v11net::cl_netchan_encode(&mut buf, self.challenge, &self.server_commands);
        }
        let t = self.realtime;
        self.netchan.transmit(net, buf.bytes(), t);
        // Transmit all fragments without delay
        while self.netchan.unsent_fragments {
            self.netchan.transmit_next_fragment(net, t);
        }
    }

    // ============================================================ cgame time

    /// `CL_SetCGameTime`.
    pub fn set_cgame_time(&mut self) {
        // getting a valid frame message ends the connection process
        if self.state != ConnState::Active {
            if self.state != ConnState::Primed {
                return;
            }
            if self.demo.playing {
                // we shouldn't get the first snapshot on the same frame
                // as the gamestate, because it causes a bad time skip
                if !self.demo.first_frame_skipped {
                    self.demo.first_frame_skipped = true;
                    return;
                }
                self.read_demo_message();
            }
            if self.new_snapshots {
                self.new_snapshots = false;
                self.first_snapshot();
            }
            if self.state != ConnState::Active {
                return;
            }
        }
        // if we have gotten to this point, cl.snap is guaranteed to be valid
        if !self.snap.valid {
            return;
        }
        if self.snap.server_time < self.old_frame_server_time {
            // a map_restart or new map: the times start over
            self.old_frame_server_time = self.snap.server_time;
            self.old_server_time = self.snap.server_time;
            self.server_time_delta = self.snap.server_time - self.realtime;
        }
        self.old_frame_server_time = self.snap.server_time;
        // get our current view of time
        if self.demo.playing && self.demo.freeze {
            // cl_freezeDemo is used to lock a demo in place for single frame advances
            if self.new_snapshots {
                self.adjust_time_delta();
            }
            self.demo_time_frame();
            return;
        }
        // cl_timeNudge is a user adjustable
        // cvar that allows more or less latency to be added in the interest
        // of better smoothness or better responsiveness
        let tn = self.time_nudge.clamp(-30, 30);
        self.server_time = self.realtime + self.server_time_delta - tn;
        // guarantee that time will never flow backwards, even if
        // serverTimeDelta made an adjustment or cl_timeNudge was changed
        if self.server_time < self.old_server_time {
            self.server_time = self.old_server_time;
        }
        self.old_server_time = self.server_time;
        // note if we are almost past the latest frame (without timeNudge),
        // so we will try and adjust back a bit when the next snapshot arrives
        if self.realtime + self.server_time_delta >= self.snap.server_time - 5 {
            self.extrapolated_snapshot = true;
        }
        // if we have gotten new snapshots, drift serverTimeDelta
        // don't do this every frame, or a period of packet loss would
        // make a huge adjustment
        if self.new_snapshots {
            self.adjust_time_delta();
        }
        if self.demo.playing {
            self.demo_time_frame();
        }
    }

    /// `CL_AdjustTimeDelta`.
    fn adjust_time_delta(&mut self) {
        self.new_snapshots = false;
        // the delta never drifts when replaying a demo
        if self.demo.playing {
            return;
        }
        let new_delta = self.snap.server_time - self.realtime;
        let delta_delta = (new_delta - self.server_time_delta).abs();
        if delta_delta > RESET_TIME {
            self.server_time_delta = new_delta;
            self.old_server_time = self.snap.server_time;
            self.server_time = self.snap.server_time;
        } else if delta_delta > 100 {
            // fast adjust, cut the difference in half
            self.server_time_delta = (self.server_time_delta + new_delta) >> 1;
        } else {
            // slow drift adjust, only move 1 or 2 msec
            // if any of the frames between this and the previous snapshot
            // had to be extrapolated, nudge our sense of time back a little
            if self.extrapolated_snapshot {
                self.extrapolated_snapshot = false;
                self.server_time_delta -= 2;
            } else {
                // otherwise, move our sense of time forward to minimize total latency
                self.server_time_delta += 1;
            }
        }
    }

    /// `CL_FirstSnapshot`.
    fn first_snapshot(&mut self) {
        // ignore snapshots that don't have entities
        if self.snap.snap_flags & SNAPFLAG_NOT_ACTIVE != 0 {
            return;
        }
        self.state = ConnState::Active;
        // set the timedelta so we are exactly on this first frame
        self.server_time_delta = self.snap.server_time - self.realtime;
        self.old_server_time = self.snap.server_time;
        self.demo.time_demo_base_time = self.snap.server_time;
        self.voip_first_snapshot(); // wt-b VoIP
    }

    /// `CL_GetSnapshot`.
    pub fn get_snapshot(&self, number: i32) -> Option<Snapshot> {
        if number > self.snap.message_num {
            return None;
        }
        // if the frame has fallen out of the circular buffer, we can't return it
        if self.snap.message_num - number >= PACKET_BACKUP as i32 {
            return None;
        }
        // if the frame is not valid, we can't return it
        let s = &self.snapshots[number as usize & PACKET_MASK];
        if !s.valid || s.message_num != number {
            return None;
        }
        // if the entities in the frame have fallen out of their
        // circular buffer, we can't return it
        if self.parse_entities_num - s.parse_entities_num >= MAX_PARSE_ENTITIES {
            return None;
        }
        let count = s.num_entities.min(MAX_SNAPSHOT_ENTITIES);
        Some(Snapshot {
            snap_flags: s.snap_flags,
            ping: s.ping,
            server_time: s.server_time,
            areamask: s.areamask,
            ps: s.ps,
            entities: (0..count).map(|i| self.parse_entities[(s.parse_entities_num + i) & (MAX_PARSE_ENTITIES - 1)]).collect(),
            server_command_sequence: s.server_command_num,
        })
    }

    /// `CL_GetServerCommand`: the command's arguments, None when it is for
    /// the engine (bcs0/bcs1, disconnect) or lost. `cs` updates the
    /// gamestate's configstring before the cgame sees it.
    pub fn get_server_command(&mut self, number: i32, big: &mut String) -> Option<Vec<String>> {
        // if we have irretrievably lost a reliable command, drop the connection
        if number <= self.server_command_sequence - MAX_RELIABLE_COMMANDS as i32 {
            // when a demo record was started after the client got a whole bunch of
            // reliable commands then the client never got those first reliable commands
            if self.demo.playing {
                return None;
            }
            self.drop_error("CL_GetServerCommand: a reliable command was cycled out");
            return None;
        }
        if number > self.server_command_sequence {
            self.drop_error("CL_GetServerCommand: requested a command not received");
            return None;
        }
        let mut s = self.server_commands[number as usize & (MAX_RELIABLE_COMMANDS - 1)].clone();
        self.last_executed_server_command = number;
        loop {
            let argv = super::svcmds::tokenize(&s);
            let cmd = argv.first().cloned().unwrap_or_default();
            match cmd.as_str() {
                "disconnect" => {
                    // allow server to indicate why they were disconnected
                    let why = argv.get(1).map(|r| format!("Server disconnected - {r}")).unwrap_or_else(|| "Server disconnected".into());
                    // Com_Error ends the frame at the first one
                    if self.error.is_none() {
                        self.error = Some(why);
                    }
                    return None;
                }
                "bcs0" => {
                    *big = format!("cs {} \"{}", argv.get(1).cloned().unwrap_or_default(), argv.get(2).cloned().unwrap_or_default());
                    return None;
                }
                "bcs1" => {
                    big.push_str(argv.get(2).map(|x| x.as_str()).unwrap_or(""));
                    return None;
                }
                "bcs2" => {
                    big.push_str(argv.get(2).map(|x| x.as_str()).unwrap_or(""));
                    big.push('"');
                    s = std::mem::take(big);
                    continue;
                }
                _ => break,
            }
        }
        if !self.vanilla() {
            return self.server_command_text(&s);
        }
        // protocol 69 (dec040): a configstring goes through
        // configstring_from_11 like the gamestate's, every other command
        // through server_command_from_11 (v11.rs); a command that becomes
        // several legacy ones queues the rest for
        // `take_pending_server_command`
        let argv = super::svcmds::tokenize(&s);
        if argv.first().map(|c| c.as_str()) == Some("cs") {
            // CL_ConfigstringModified
            let index = super::spawn::c_atoi(argv.get(1).map(|x| x.as_str()).unwrap_or("-1"));
            if index < 0 || index as usize >= cs::MAX_CONFIGSTRINGS {
                return None;
            }
            let text = argv.get(2).cloned().unwrap_or_default();
            self.configstrings11[index as usize] = text.clone();
            if index as usize == cs::SYSTEMINFO {
                // parse serverId and other cvars
                self.system_info_changed();
            }
            // CS_STAGES also sets legacy's CS_ALIEN_STAGES / CS_HUMAN_STAGES
            let mut set: std::collections::VecDeque<Vec<String>> = std::collections::VecDeque::new();
            for (li, ls) in v11::configstrings_from_11(index as usize, &text) {
                if li < cs::MAX_CONFIGSTRINGS {
                    self.configstrings[li] = ls.clone();
                    set.push_back(vec!["cs".into(), li.to_string(), ls]);
                }
            }
            let first = set.pop_front()?;
            self.pending_server_commands.extend(set);
            return Some(first);
        }
        let mut out: std::collections::VecDeque<Vec<String>> = v11::server_command_from_11(&s).iter().filter_map(|t| self.server_command_text(t)).collect();
        let first = out.pop_front();
        self.pending_server_commands.extend(out);
        first
    }

    /// A legacy command a 1.1 server command turned into beyond the first
    /// (`get_server_command` queues them; the cgame runs them right after
    /// it, dec040).
    pub fn take_pending_server_command(&mut self) -> Option<Vec<String>> {
        self.pending_server_commands.pop_front()
    }

    /// The legacy server command text `s` for the cgame: `cs` updates the
    /// gamestate first, `map_restart` resets the commands.
    fn server_command_text(&mut self, s: &str) -> Option<Vec<String>> {
        let argv = super::svcmds::tokenize(s);
        let cmd = argv.first().cloned().unwrap_or_default();
        match cmd.as_str() {
            "cs" => {
                // CL_ConfigstringModified
                let index = super::spawn::c_atoi(argv.get(1).map(|x| x.as_str()).unwrap_or("-1"));
                if index >= 0 && (index as usize) < cs::MAX_CONFIGSTRINGS {
                    self.configstrings[index as usize] = argv.get(2).cloned().unwrap_or_default();
                    if index as usize == cs::SYSTEMINFO && !self.vanilla() {
                        // parse serverId and other cvars
                        self.system_info_changed();
                    }
                }
                Some(argv)
            }
            "map_restart" => {
                // reset the command buffer so we don't run the old map's commands
                self.cmds.iter_mut().for_each(|c| *c = UserCmd::default());
                Some(argv)
            }
            _ => Some(argv),
        }
    }

    // ============================================================ browser

    /// `CL_LocalServers_f`: "getinfo xxx" broadcast on the four server ports,
    /// twice; the loopback-interface copies make same-machine servers show
    /// up where broadcasts are not looped back (dec033).
    pub fn local_servers(&mut self, net: &mut Net) {
        self.printf("Scanning for servers on the local network...\n");
        // reset the list, waiting for response
        self.ping_update_source = AS_LOCAL;
        self.local_visible = self.local_servers.iter().map(|s| s.visible).collect();
        self.local_servers.clear();
        net.open_client();
        let message = b"\xff\xff\xff\xffgetinfo xxx";
        // each network's own broadcast too (dec045: Windows sends the
        // all-ones broadcast out of one adapter only)
        let nets = if net.capture.is_some() { Vec::new() } else { netchan::lan_broadcasts(&netchan::local_ipv4()) };
        for _ in 0..2 {
            for j in 0..netchan::NUM_SERVER_PORTS {
                let port = netchan::PORT_SERVER + j;
                let to = NetAdr { kind: NetAdrType::Broadcast, port, ..Default::default() };
                net.send_packet(NetSrc::Client, message, &to);
                let to = NetAdr { kind: NetAdrType::Multicast6, port, ..Default::default() };
                net.send_packet(NetSrc::Client, message, &to);
                for b in &nets {
                    let to = NetAdr { kind: NetAdrType::Ip, ip: *b, port, ..Default::default() };
                    net.send_packet(NetSrc::Client, message, &to);
                }
                // the loopback interfaces (dec033): same-machine servers
                let lo = NetAdr { kind: NetAdrType::Ip, ip: [127, 0, 0, 1], port, ..Default::default() };
                net.send_packet(NetSrc::Client, message, &lo);
                if net.is_open6(NetSrc::Client) {
                    let lo6 = NetAdr { kind: NetAdrType::Ip6, ip6: std::net::Ipv6Addr::LOCALHOST.octets(), port, ..Default::default() };
                    net.send_packet(NetSrc::Client, message, &lo6);
                }
            }
        }
    }

    /// `CL_GlobalServers_f`: resolve master `n` on a background thread,
    /// then "getservers 71 full empty" (dec039). An unresolvable or
    /// silent master leaves the list empty.
    pub fn global_servers(&mut self, net: &mut Net, master: usize, keywords: &str) -> Result<(), String> {
        let Some(name) = self.master_names.get(master).cloned().filter(|n| !n.is_empty()) else {
            return Err("CL_GlobalServers_f: Error: No master server address given.".into());
        };
        self.printf(&format!("Requesting servers from master {name}...\n"));
        self.num_global_servers = -1;
        self.global_servers.clear();
        self.ping_update_source = AS_GLOBAL;
        net.open_client();
        let (tx, rx) = std::sync::mpsc::channel();
        let kw = keywords.to_string();
        std::thread::spawn(move || {
            let (r, mut a) = netchan::string_to_adr(&name, NetAdrType::Unspec);
            if r == 2 {
                a.port = netchan::PORT_MASTER;
            }
            let _ = tx.send((kw, (r != 0).then_some(a)));
        });
        self.master_rx = Some(rx);
        Ok(())
    }

    fn receive_masters(&mut self, net: &mut Net) {
        let Some(rx) = self.master_rx.as_ref() else { return };
        if let Ok((kw, a)) = rx.try_recv() {
            self.master_rx = None;
            match a {
                Some(a) => {
                    // The C sends "getservers <com_gamename> <argv 2..>", but
                    // the live master (master.tremulous.net, darklegion's
                    // tremulous-master) reads the first word after
                    // getservers as the protocol and answers nothing to
                    // "Tremulous": the request carries PROTOCOL_VERSION
                    // instead (dec039). IPv6 masters get the extended query
                    // with the game name first, which both that master and
                    // dpmaster parse.
                    // Both protocols are asked for, legacy's and vanilla
                    // 1.1.0's, and the answers merged; each server's own
                    // infoResponse tells which it is (dec040).
                    self.master_merge = true;
                    for protocol in [PROTOCOL_VERSION, PROTOCOL_VANILLA] {
                        let cmd = if matches!(a.kind, NetAdrType::Ip6 | NetAdrType::Multicast6) {
                            let v6only = if net.enabled & netchan::NET_ENABLEV4 == 0 { " ipv6" } else { "" };
                            format!("getserversExt {GAMENAME} {protocol} {kw}{v6only}")
                        } else {
                            format!("getservers {protocol} {kw}")
                        };
                        net.out_of_band_print(NetSrc::Client, &a, cmd.trim_end());
                    }
                }
                None => {
                    self.printf("CL_GlobalServers_f: Error: could not resolve address of master\n");
                    self.num_global_servers = 0;
                }
            }
        }
    }

    /// `CL_GSRSequenceInformation`: "\\0<index>\\0<count>\\0" after the
    /// header; (index, position after it), or None when absent or nonsensical.
    fn gsr_sequence_information(&mut self, b: &[u8], p: usize) -> Option<(i32, usize)> {
        let field = |from: usize| -> Option<(i32, usize)> {
            let mut e = from;
            while e < b.len() && b[e].is_ascii_digit() {
                e += 1;
            }
            (e < b.len() && b[e] == 0).then(|| (std::str::from_utf8(&b[from..e]).ok().and_then(|x| x.parse().ok()).unwrap_or(0), e + 1))
        };
        if b.get(p) != Some(&0) {
            return None;
        }
        let (ind, q) = field(p + 1)?;
        let (num, r) = field(q)?;
        if num <= 0 || ind <= 0 || ind > num || ind > 31 {
            return None; // nonsensical response
        }
        // with the two protocols' answers merged (dec040) the counts of
        // the two lists differ: nothing is thrown away
        if self.num_master_packets > 0 && num != self.num_master_packets && !self.master_merge {
            // assume we sent two getservers and somehow they changed in
            // between: only use the results that arrive later
            self.received_master_packets = 0;
            self.global_servers.clear();
        }
        self.num_master_packets = num;
        Some((ind, r))
    }

    /// `CL_ServersResponsePacket`.
    fn servers_response_packet(&mut self, _from: &NetAdr, m: &Msg, extended: bool) {
        if self.num_global_servers == -1 {
            // state to detect lack of servers or lack of response
            self.num_global_servers = 0;
            self.num_master_packets = 0;
            self.received_master_packets = 0;
        }
        let b = m.bytes();
        let mut p = 4usize;
        // advance to initial token
        while p < b.len() && b[p] != 0 && b[p] != b'\\' && b[p] != b'/' {
            p += 1;
        }
        let mut label = String::new();
        if p < b.len() && b[p] == 0 {
            if let Some((ind, rest)) = self.gsr_sequence_information(b, p) {
                // this denotes the start of new-syntax stuff
                // have we already received this packet?
                if self.received_master_packets & (1 << (ind - 1)) != 0 && !self.master_merge {
                    return;
                }
                self.received_master_packets |= 1 << (ind - 1);
                p = rest;
                // CL_GSRFeaturedLabel: up to the field break or the server list
                let start = p;
                while p < b.len() && b[p] != 0 && b[p] != b'\\' && b[p] != b'/' {
                    p += 1;
                }
                label = String::from_utf8_lossy(&b[start..p.min(start + MAX_FEATLABEL_CHARS - 1)]).into_owned();
            }
            // now skip to the server list
            while p < b.len() && b[p] != b'\\' && b[p] != b'/' {
                p += 1;
            }
        }
        let mut found = Vec::new();
        while p + 1 < b.len() {
            let mut a = NetAdr::default();
            if b[p] == b'\\' {
                // IPv4 address
                p += 1;
                if b.len() - p < 4 + 2 + 1 {
                    break;
                }
                a.kind = NetAdrType::Ip;
                a.ip.copy_from_slice(&b[p..p + 4]);
                p += 4;
            } else if extended && b[p] == b'/' {
                // IPv6 address, if it's an extended response
                p += 1;
                if b.len() - p < 16 + 2 + 1 {
                    break;
                }
                a.kind = NetAdrType::Ip6;
                a.ip6.copy_from_slice(&b[p..p + 16]);
                p += 16;
            } else {
                // syntax error!
                break;
            }
            // parse out port
            a.port = ((b[p] as u16) << 8) | b[p + 1] as u16;
            p += 2;
            // syntax check
            if p >= b.len() || (b[p] != b'\\' && b[p] != b'/') {
                break;
            }
            found.push(a);
            if found.len() >= MAX_SERVERSPERPACKET {
                break;
            }
        }
        for a in &found {
            if self.global_servers.len() >= MAX_GLOBAL_SERVERS {
                break;
            }
            // the second protocol's list repeats the servers that run both
            if self.master_merge && self.global_servers.iter().any(|s| netchan::compare_adr(&s.adr, a)) {
                continue;
            }
            self.global_servers.push(ServerInfo { adr: *a, ping: -1, visible: true, label: label.clone(), ..Default::default() });
        }
        self.num_global_servers = self.global_servers.len() as i32;
        self.printf(&format!("{} servers parsed (total {})\n", found.len(), self.num_global_servers));
    }

    /// `CL_SetServerInfo` for every list holding `from`.
    fn set_server_info_by_address(&mut self, from: &NetAdr, info: Option<&str>, ping: i32) {
        for s in self.local_servers.iter_mut().chain(self.global_servers.iter_mut()).chain(self.favorite_servers.iter_mut()) {
            if netchan::compare_adr(from, &s.adr) {
                if let Some(info) = info {
                    s.clients = super::spawn::c_atoi(&svgame::info_value(info, "clients"));
                    s.host_name = svgame::info_value(info, "hostname");
                    s.map_name = svgame::info_value(info, "mapname");
                    s.max_clients = super::spawn::c_atoi(&svgame::info_value(info, "sv_maxclients"));
                    let g = svgame::info_value(info, "game");
                    s.game = if g.is_empty() { "base".into() } else { g };
                    s.game_type = super::spawn::c_atoi(&svgame::info_value(info, "gametype"));
                    s.net_type = super::spawn::c_atoi(&svgame::info_value(info, "nettype"));
                    s.min_ping = super::spawn::c_atoi(&svgame::info_value(info, "minping"));
                    s.max_ping = super::spawn::c_atoi(&svgame::info_value(info, "maxping"));
                    s.protocol = super::spawn::c_atoi(&svgame::info_value(info, "protocol"));
                }
                s.ping = ping;
            }
        }
    }

    /// `CL_ServerInfoPacket`.
    fn server_info_packet(&mut self, from: &NetAdr, m: &mut Msg) {
        let info = m.read_string();
        let protocol = super::spawn::c_atoi(&svgame::info_value(&info, "protocol"));
        // a vanilla 1.1.0 server's info has no gamename (dec040)
        if protocol != PROTOCOL_VANILLA {
            // if this isn't the correct gamename, ignore it
            if svgame::info_value(&info, "gamename") != GAMENAME {
                return;
            }
            // if this isn't the correct protocol version, ignore it
            if protocol != PROTOCOL_VERSION {
                return;
            }
        }
        // iterate servers waiting for ping response
        let now = sys_milliseconds();
        for i in 0..MAX_PINGREQUESTS {
            let p = &self.pinglist[i];
            if p.adr.port != 0 && p.time == 0 && netchan::compare_adr(from, &p.adr) {
                // calc ping time
                let t = (now - p.start).max(1);
                let ty = match from.kind {
                    NetAdrType::Broadcast | NetAdrType::Ip => 1,
                    NetAdrType::Ip6 => 2,
                    _ => 0,
                };
                let mut inf = info.clone();
                svgame::info_set(&mut inf, "nettype", &ty.to_string());
                self.pinglist[i].time = t;
                self.pinglist[i].info = inf;
                self.set_server_info_by_address(from, Some(&info), t);
                return;
            }
        }
        // if not just sent a local broadcast or pinging local servers
        if self.ping_update_source != AS_LOCAL {
            return;
        }
        // avoid duplicate
        if self.local_servers.iter().any(|s| netchan::compare_adr(from, &s.adr)) {
            return;
        }
        // the port's scan also asks the loopback (dec033), and a server on
        // this machine answers that and the broadcast: one server, two
        // addresses. The same port with the same info (challenge aside) is
        // the same server; the loopback copy gives way (dec040)
        let key = |info: &str| {
            let mut k = info.to_string();
            svgame::info_set(&mut k, "challenge", "");
            k
        };
        let me = key(&info);
        let is_lo = |a: &NetAdr| a.kind == NetAdrType::Ip && a.ip[0] == 127;
        if let Some(j) = (0..self.local_servers.len()).find(|&j| self.local_servers[j].adr.port == from.port && self.local_info_keys.get(j) == Some(&me) && (is_lo(from) || is_lo(&self.local_servers[j].adr))) {
            if is_lo(&self.local_servers[j].adr) && !is_lo(from) {
                self.local_servers[j].adr = *from;
            }
            return;
        }
        if self.local_servers.len() >= MAX_OTHER_SERVERS {
            return;
        }
        // add this to the list (CL_InitServerInfo: ping -1, the ui pings it);
        // the slot keeps the visible flag CL_LocalServers_f preserved
        let n = self.local_servers.len();
        let visible = self.local_visible.get(n).copied().unwrap_or(false);
        self.local_servers.push(ServerInfo { adr: *from, ping: -1, visible, protocol, ..Default::default() });
        self.local_info_keys.truncate(n);
        self.local_info_keys.push(me);
    }

    /// `CL_ServerStatusResponse`.
    fn server_status_response(&mut self, from: &NetAdr, m: &mut Msg) {
        let info = m.read_string_line();
        let mut players = Vec::new();
        loop {
            let l = m.read_string_line();
            if l.is_empty() {
                break;
            }
            let a = super::svcmds::tokenize(&l);
            if a.len() >= 3 {
                players.push((super::spawn::c_atoi(&a[0]), super::spawn::c_atoi(&a[1]), a[2].clone()));
            }
        }
        self.server_status = Some(ServerStatus { adr: *from, info, players, time: self.realtime });
    }

    /// `CL_ServerStatus`: "getstatus" to an address.
    pub fn request_server_status(&mut self, net: &mut Net, server: &str) -> bool {
        let (r, mut a) = netchan::string_to_adr(server, NetAdrType::Unspec);
        if r == 0 {
            return false;
        }
        if r == 2 {
            a.port = netchan::PORT_SERVER;
        }
        net.open_client();
        net.out_of_band_print(NetSrc::Client, &a, "getstatus");
        true
    }

    /// `CL_Ping_f` / `LAN_UpdateVisiblePings`' ping of one address.
    pub fn ping(&mut self, net: &mut Net, adr: &NetAdr) {
        // CL_GetFreePing
        let now = sys_milliseconds();
        let mut slot = None;
        for (i, p) in self.pinglist.iter().enumerate() {
            // find free ping slot
            if p.adr.port != 0 {
                if p.time == 0 {
                    if now - p.start < 500 {
                        // still waiting for response
                        continue;
                    }
                } else if p.time < 500 {
                    // results have not been queried
                    continue;
                }
            }
            slot = Some(i);
            break;
        }
        let i = slot.unwrap_or_else(|| {
            // use oldest entry
            (0..MAX_PINGREQUESTS).max_by_key(|&i| now - self.pinglist[i].start).unwrap_or(0)
        });
        self.pinglist[i] = Ping { adr: *adr, start: now, time: 0, info: String::new() };
        self.set_server_info_by_address(adr, None, 0);
        net.open_client();
        net.out_of_band_print(NetSrc::Client, adr, "getinfo xxx");
    }

    /// `CL_GetPing`: (address, ping ms; 0 while waiting, the elapsed time
    /// once past cl_maxPing); the server's list entry gets the result.
    pub fn get_ping(&mut self, n: usize) -> Option<(NetAdr, i32)> {
        let p = self.pinglist.get(n).filter(|p| p.adr.port != 0)?.clone();
        let mut time = p.time;
        if time == 0 {
            // check for timeout
            time = sys_milliseconds() - p.start;
            if time < self.max_ping.max(100) {
                // not timed out yet
                time = 0;
            }
        }
        self.set_server_info_by_address(&p.adr, if p.info.is_empty() { None } else { Some(&p.info) }, p.time);
        Some((p.adr, time))
    }

    /// `CL_UpdateVisiblePings_f`: ping the visible servers of a list that
    /// have no ping yet; true while answers are outstanding.
    pub fn update_visible_pings(&mut self, net: &mut Net, source: i32) -> bool {
        if !(0..=AS_FAVORITES).contains(&source) {
            return false;
        }
        self.ping_update_source = source;
        let mut status = false;
        // CL_GetPingQueueCount
        let mut slots = self.pinglist.iter().filter(|p| p.adr.port != 0).count();
        if slots < MAX_PINGREQUESTS {
            let list: Vec<(NetAdr, bool, i32)> = match source {
                AS_LOCAL => &self.local_servers,
                AS_GLOBAL | AS_MPLAYER => &self.global_servers,
                _ => &self.favorite_servers,
            }
            .iter()
            .map(|s| (s.adr, s.visible, s.ping))
            .collect();
            net.open_client();
            for (adr, visible, ping) in list {
                if !visible || ping != -1 {
                    continue;
                }
                if slots >= MAX_PINGREQUESTS {
                    break;
                }
                if self.pinglist.iter().any(|p| p.adr.port != 0 && netchan::compare_adr(&p.adr, &adr)) {
                    continue; // already on the list
                }
                status = true;
                if let Some(j) = self.pinglist.iter().position(|p| p.adr.port == 0) {
                    self.pinglist[j] = Ping { adr, start: sys_milliseconds(), time: 0, info: String::new() };
                    net.out_of_band_print(NetSrc::Client, &adr, "getinfo xxx");
                    slots += 1;
                }
            }
        }
        if slots > 0 {
            status = true;
        }
        for i in 0..MAX_PINGREQUESTS {
            if self.pinglist[i].adr.port == 0 {
                continue;
            }
            if let Some((_, t)) = self.get_ping(i) {
                if t != 0 {
                    self.clear_ping(i);
                    status = true;
                }
            }
        }
        status
    }

    /// `CL_ClearPing`.
    pub fn clear_ping(&mut self, n: usize) {
        if let Some(p) = self.pinglist.get_mut(n) {
            p.adr.port = 0;
        }
    }
}

#[cfg(test)]
mod server_value_bounds_tests {
    use super::*;

    fn gamestate(maxclients: &str, client_num: i32) -> Msg {
        let mut m = Msg::new(MAX_MSGLEN);
        m.write_long(1);
        m.write_byte(svc::CONFIGSTRING as i32);
        m.write_short(cs::SERVERINFO as i32);
        m.write_big_string(&format!("\\sv_maxclients\\{maxclients}\\mapname\\atcs"));
        m.write_byte(svc::EOF as i32);
        m.write_long(client_num);
        m.write_long(0);
        m.begin_reading();
        m
    }

    fn run(maxclients: &str, client_num: i32) -> NetClient {
        let mut cl = NetClient::new(1);
        let mut m = gamestate(maxclients, client_num);
        cl.parse_gamestate(&mut m);
        cl
    }

    #[test]
    fn a_server_with_more_slots_than_supported_is_refused_with_a_message() {
        let cl = run("128", 3);
        assert_eq!(cl.error.as_deref(), Some("This server allows more players (128) than this game version supports (64)."));
    }

    #[test]
    fn a_player_number_out_of_range_is_refused() {
        for n in [64, 127, 1023, -1, i32::MIN, i32::MAX] {
            let cl = run("64", n);
            assert!(cl.error.is_some(), "client number {n} accepted");
        }
    }

    #[test]
    fn the_supported_range_still_connects() {
        for (max, n) in [("64", 0), ("64", 63), ("8", 7), ("", 0)] {
            let cl = run(max, n);
            assert!(cl.error.is_none(), "sv_maxclients {max:?}, client {n}: {:?}", cl.error);
            assert_eq!(cl.client_num, n);
        }
    }

    #[test]
    fn the_first_error_of_a_server_message_is_the_one_reported() {
        let mut cl = NetClient::new(1);
        let mut net = Net::new();
        let mut m = Msg::new(MAX_MSGLEN);
        m.write_long(0);
        m.write_byte(svc::GAMESTATE as i32);
        m.write_long(1);
        m.write_byte(svc::CONFIGSTRING as i32);
        m.write_short(cs::SERVERINFO as i32);
        m.write_big_string("\\sv_maxclients\\128");
        m.write_byte(svc::EOF as i32);
        m.write_long(5);
        m.write_long(0);
        m.write_byte(svc::EOF as i32);
        m.begin_reading();
        cl.parse_server_message(&mut net, &mut m);
        assert_eq!(cl.error.as_deref(), Some("This server allows more players (128) than this game version supports (64)."));
    }
}

