//! `TremGame`: the Godot node that owns a `trem_sim::World`, feeds it the
//! local player's input as `UserCmd`s at a fixed 16 ms tick, and projects
//! the simulation state onto Godot nodes every rendered frame. Godot never
//! owns game state (dec001); everything here is presentation glue.

use crate::geom::{self, basis_from_angles, hex_color, q2g, q2g_dir, SCALE};
use godot::classes::base_material_3d::{BillboardMode, CullMode, Feature, Flags as MatFlags, TextureParam, Transparency};
use godot::classes::image::Format as ImageFormat;
use godot::classes::input::MouseMode;
use godot::classes::light_3d::Param as LightParam;
use godot::classes::mesh::PrimitiveType;
use godot::classes::{
    BoxMesh, Camera3D, Image, ImageTexture, CapsuleMesh, CylinderMesh, DirectionalLight3D, Environment, INode3D, Input, InputEvent, InputEventKey, InputEventMouseButton, InputEventMouseMotion, Label3D, MeshInstance3D, Node3D,
    OmniLight3D, SphereMesh, StandardMaterial3D, SurfaceTool, WorldEnvironment,
};
use godot::global::Key;
use godot::prelude::*;
use std::collections::{HashMap, HashSet};
use crate::actors::{class_model, map_torso_to_weapon_frame, Actor, GhostModel, MissileVis, SoundReq, ViewHand};
use crate::audio::Audio;
use crate::models::Library;
use trem_sim::gen::brushes::BRUSHES;
use trem_sim::gen::buildables::{BuildableId, BUILDABLES};
use trem_sim::gen::classes::{ClassId, CLASSES};
use trem_sim::gen::constants as k;
use trem_sim::gen::enums::{Event, MeansOfDeath, SpectatorState, Stat, Team};
use trem_sim::gen::maps::MapId;
use trem_sim::gen::materials::MATERIALS;
use trem_sim::gen::upgrades::{UpgradeId, UPGRADES};
use trem_sim::gen::weapons::{WeaponId, WEAPONS};
use trem_sim::kernel::qmath::{angle_vectors, ma};
use trem_sim::kernel::world::{EType, GameEvent, World, MAX_CLIENTS};
use trem_sim::kernel::{bg, bot, client};
use trem_sim::kernel::{admin, session, svcmds};

const TICK_MS: i32 = 16;

// wt-a (dec037): downloads, pure, demos, ipv6 cvars, headless dedicated
#[path = "netext.rs"]
mod netext;

struct Visual {
    key: String,
    node: Gd<Node3D>,
}

struct Fx {
    node: Gd<MeshInstance3D>,
    life: f64,
    max: f64,
    grow: f32,
}

/// The loading screen's facts (`CG_DrawLoadingScreen` and the CG_LOAD_*
/// ownerdraws of ui/loading.menu): CS_SERVERINFO mapname / sv_hostname,
/// CS_MESSAGE, CS_MOTD and cg.mediaFraction / buildablesFraction /
/// charModelFraction.
#[derive(Clone, Debug, Default)]
pub struct LoadInfo {
    pub mapname: String,
    pub message: String,
    pub hostname: String,
    pub motd: String,
    pub media: f32,
    pub buildables: f32,
    pub char_models: f32,
}

/// A map being loaded across frames, so the loading menu draws between the
/// phases of `SV_SpawnServer` / `G_InitGame` and `CG_Init` (each phase is
/// a step; `step_loading` runs steps for a few ms per frame).
struct MapLoad {
    paths: Vec<String>,
    name: String,
    set: crate::assets::Pk3Set,
    bsp: trem_sim::kernel::bsp::Bsp,
    step: usize,
    info: LoadInfo,
    world: Option<World>,
    sessions: Option<Vec<Option<String>>>,
    render: Option<crate::bspview::BspRender>,
    meshes: Option<Vec<Gd<MeshInstance3D>>>,
    brush: crate::mapents::BrushMeshes,
    sky: Option<crate::bspview::SkyDef>,
    /// the 4K pack's sky faces (hdtex.rs)
    sky_hd: Option<Vec<Gd<godot::classes::Texture2D>>>,
    grid: Option<crate::bspview::LightGrid>,
    shadow_dirs: Option<crate::lamps::ShadowDirs>,
    /// the world's triangles for the ray-traced effects (gfx.rs / rt.rs)
    rt_geom: Option<crate::bspview::RtGeometry>,
    /// the world's triangles for the acoustic simulation (fmodaudio.rs)
    acoustic: Option<crate::fmodaudio::AcousticMesh>,
    /// the map's fog brushes (gfx.rs fog volumes)
    fogs: Vec<crate::bspview::FogBox>,
    /// console lines to run once the map is up (rotation post commands)
    then: Vec<String>,
    /// devmap
    cheats: bool,
    /// single process, listen server or a remote server's client (dec033)
    mode: LoadMode,
}

/// What a map load is for (dec033).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LoadMode {
    /// the single-process game: the World is ticked here
    Single,
    /// `SV_SpawnServer` of a listen server: the World goes to the server,
    /// the presentation reads a view World the loopback client fills
    Listen,
    /// `CL_InitCGame` for a server elsewhere: only the view World
    Client,
}

/// `CG_RegisterSounds`' fixed list (the cgs.media sounds).
const CG_MEDIA_SOUNDS: &[&str] = &[
    "sound/announcements/overmindevolved.wav", "sound/announcements/reinforcement.wav", "sound/announcements/overmindattack.wav",
    "sound/announcements/overminddying.wav", "sound/announcements/overmindspawns.wav", "sound/player/level1/grab.wav",
    "sound/player/level4/charge_prepare.wav", "sound/player/level4/charge_start.wav", "sound/weapons/tracer.wav", "sound/weapons/change.wav",
    "sound/buildables/mgturret/spinup.wav", "sound/weapons/click.wav", "sound/misc/talk.wav", "sound/misc/alien_talk.wav", "sound/misc/human_talk.wav",
    "sound/player/land1.wav", "sound/player/watr_in.wav", "sound/player/watr_out.wav", "sound/player/watr_un.wav", "sound/misc/disconnect.wav",
    "sound/upgrades/jetpack/low.wav", "sound/upgrades/jetpack/idle.wav", "sound/upgrades/jetpack/hi.wav", "sound/upgrades/medkit/medkit.wav",
    "sound/player/alienevolve.wav", "sound/buildables/alien/explosion.wav", "sound/buildables/alien/damage.wav", "sound/buildables/alien/prebuild.wav",
    "sound/buildables/human/explosion.wav", "sound/buildables/human/prebuild.wav", "sound/misc/hard_bounce1.wav", "sound/misc/hard_bounce2.wav",
    "sound/buildables/repeater/use.wav", "sound/buildables/human/repair.wav", "sound/buildables/human/repaired.wav",
    "models/weapons/lcannon/warning.wav", "models/weapons/lcannon/warning2.wav",
];

/// Loading steps before the per-buildable and per-class ones.
const LOAD_FIXED_STEPS: usize = 8;

enum CurrentMap {
    Sheet(MapId),
    Bsp(Vec<String>, String),
}

/// What the host-time status waits for (dec076).
struct HostReport {
    map: String,
    port: u16,
    /// the wait (4 s) counts running frames: a map load that freezes them
    /// does not use it up before the network check could send
    clock: crate::natcheck::FrameClock,
    /// when the report was started (the firewall check's bound is wall time)
    since: std::time::Instant,
    fw: Option<(crate::hoststatus::Fw, crate::firewall::Verdict)>,
    router: Option<crate::hoststatus::Router>,
    upnp_on: bool,
    /// the network check runs (net_natCheck, dec076)
    nat_on: bool,
    ask: bool,
    /// `hosting_status`: show the window even with net_hostWindow 0
    force: bool,
}

#[derive(GodotClass)]
#[class(base=Node3D)]
pub struct TremGame {
    base: Base<Node3D>,
    current: CurrentMap,
    world: Option<World>,
    me: Option<usize>,
    acc_ms: f64,
    /// the disconnected client's CL_Frame clock (the server browser)
    browser_acc_ms: f64,
    /// Set false by the HUD while a menu or the console is open (the key
    /// catcher: `Key_SetCatcher( KEYCATCH_UI )`).
    #[var]
    input_enabled: bool,
    /// cg_thirdPerson (kept in step with the client cvar).
    #[var]
    third_person: bool,
    /// The client: binds, cvars, command buffer, cl_input.c, cg.weaponSelect
    /// (clientcmd.rs).
    client: crate::clientcmd::Client,
    /// Files for `exec` (user folder, then the pk3s).
    client_fs: Option<crate::hostfs::HostFs>,
    /// usercmd button bits pressed and released in frames no tick consumed
    pending_buttons: i32,
    /// the buttons of this frame's cmd still held (later ticks of the frame)
    held_buttons: i32,
    /// (clientNum, PERS_SPAWN_COUNT) of the last snapshot (`CG_Respawn`)
    last_spawn: Option<(i32, i32)>,
    /// cl.viewangles for a new World
    reset_view: Option<[f32; 3]>,
    #[export]
    bots_per_team: i32,
    /// g_initialMapRotation: rotation (from maprotation.cfg) to follow after
    /// the first map; empty = no rotation.
    #[export]
    initial_map_rotation: GString,
    /// Start at the original main menu (`UIMENU_MAIN`, no map) like the
    /// game does without startup commands. Honoured when main.tscn is the
    /// running scene or the command line has `--main-menu`; a scene that
    /// instances main.tscn (the tests) starts on the map as before, and
    /// `+map <name>` / `+devmap <name>` on the command line load that map.
    #[export]
    start_in_main_menu: bool,
    /// g_maprotation.c state (the C keeps it in cvars across map changes).
    rotations: trem_sim::kernel::maprotation::MapRotations,
    /// cvars set by rotation command sections / the console, re-applied to
    /// every new World (cvars outlive maps in the original).
    cvars: Vec<(String, String)>,
    /// a host console command ran from the game's EXEC_APPEND buffer
    exec_ran: bool,
    last_evolve_attempt: i32,
    last_build_attempt: i32,
    camera: Option<Gd<Camera3D>>,
    /// The 3D view (trap_R_RenderScene into cg.refdef's window): the camera
    /// renders the world into this SubViewport, shown by `view_sprite` over
    /// the `CG_CalcVrect` rectangle (see `sync_view_rect`).
    view_vp: Option<Gd<godot::classes::SubViewport>>,
    /// enhanced (port_vision, G): the human's flashlight / the alien's
    /// night vision is on; the light and the overlay that draw them
    vision_on: bool,
    vision_toggle: bool,
    flashlight: Option<Gd<godot::classes::SpotLight3D>>,
    night_vision: Option<Gd<godot::classes::ColorRect>>,
    view_sprite: Option<Gd<godot::classes::Sprite2D>>,
    /// cg.refdef x, y, width, height (pixels) and fov_x, fov_y of the last frame
    vrect: (i32, i32, i32, i32),
    view_fov: (f32, f32),
    viewmodel: Option<Gd<MeshInstance3D>>,
    ghost: Option<Gd<MeshInstance3D>>,
    visuals: HashMap<usize, Visual>,
    fx: Vec<Fx>,
    mats: HashMap<String, Gd<StandardMaterial3D>>,
    feed: Vec<(f64, String)>,
    /// feed lines dropped so far (dropped + feed.len() = lines ever added)
    feed_dropped: u64,
    clock: f64,
    /// Original Tremulous data (models, sounds) when a data pk3 is on the search path.
    lib: Option<Library>,
    pk3_paths: Vec<String>,
    actors: HashMap<usize, Actor>,
    failed_actors: HashSet<String>,
    /// enhanced mode: corpses drawn as ragdolls (ragdoll.rs); their
    /// skeletons (res://data/rigs.txt), the map collider they land on, and
    /// the push of each victim's killing hit (Godot space, m/s; when)
    ragdolls: Vec<crate::ragdoll::Ragdoll>,
    rigs: Option<std::rc::Rc<HashMap<String, crate::ragdoll::Rig>>>,
    ragdoll_world: Option<Gd<godot::classes::StaticBody3D>>,
    /// what ragdolls collide with, built only while they are on (enhanced):
    /// the map's solid brushes, the movers' (riding with them) and the
    /// structures' boxes; the hulls are kept from the map load
    ragdoll_brushes: Option<Gd<godot::classes::StaticBody3D>>,
    ragdoll_movers: HashMap<usize, Gd<godot::classes::AnimatableBody3D>>,
    ragdoll_solids: HashMap<usize, (String, Gd<godot::classes::StaticBody3D>)>,
    world_hulls: std::rc::Rc<Vec<Vec<Vector3>>>,
    /// the map's drawn opaque triangles (the acoustic mesh's, which FMOD
    /// takes): the ragdolls' surface collider
    world_tris: std::rc::Rc<(Vec<[f32; 3]>, Vec<[i32; 3]>)>,
    mover_hulls: std::rc::Rc<HashMap<i32, Vec<Vec<Vector3>>>>,
    death_push: HashMap<usize, (Vector3, f64)>,
    /// each living player's last velocity (Godot space, m/s): a ragdoll
    /// carries on with the momentum its body had
    alive_vel: HashMap<usize, Vector3>,
    hand: Option<ViewHand>,
    audio: Option<Audio>,
    lightgrid: Option<crate::bspview::LightGrid>,
    /// the map's world-fixed shadow directions (lamps.rs)
    shadow_dirs: Option<crate::lamps::ShadowDirs>,
    /// the router's forwarding of the hosted server's port (UPnP, PCP,
    /// NAT-PMP; upnp.rs / pcp.rs, dec046 / dec076): the request on its
    /// thread
    upnp_pending: Option<std::sync::mpsc::Receiver<crate::pcp::PortResult>>,
    /// the PCP / NAT-PMP leases and the UPnP mapping being renewed (the
    /// port, the IPv6 pinhole), taken back when the server stops; their news
    port_keepers: Vec<crate::pcp::Keeper>,
    port_events: Option<std::sync::mpsc::Receiver<crate::pcp::Event>>,
    port_last: Option<crate::pcp::PortResult>,
    /// dec076: the network check (STUN from the game socket) and its answers
    nat_probe: Option<crate::natcheck::Probe>,
    nat_answers: Option<(Vec<std::net::SocketAddr>, Option<std::net::SocketAddr>, String)>,
    /// dec047: the Windows Firewall check of a hosted game, and the network categories it found unallowed
    firewall_pending: Option<std::sync::mpsc::Receiver<crate::firewall::Verdict>>,
    firewall_missing: Vec<String>,
    /// dec054: the startup check ran; the window was shown for this hosted
    /// server (reset when it stops: the next host shows it again); the
    /// player's dialog answer; the elevated fix running on its thread
    firewall_started: bool,
    firewall_asked: bool,
    firewall_choice: Option<std::sync::mpsc::Receiver<Result<(i64, bool), String>>>,
    /// the open dialog is Godot's plain one (the native one failed)
    dialog_godot: bool,
    /// when the firewall check was started (its answer time, hosting_status)
    fw_started: Option<std::time::Instant>,
    /// the check's answer after the status was shown without it (dec076)
    fw_late: Option<(crate::hoststatus::Fw, crate::firewall::Verdict)>,
    /// what the check last said, for hosting_status.txt
    fw_last: Option<String>,
    /// hosting started at / what happened since, for hosting_status.txt
    host_t0: Option<std::time::Instant>,
    host_events: Vec<String>,
    /// dec076: "Host for my home network only": the router port stays closed until the server stops
    host_lan_only: bool,
    /// dec076: the status being collected for a hosted game (it may ask), not for startup
    host_report: Option<HostReport>,
    /// the last router answer and the last printed status (a late answer updates it)
    router_last: Option<crate::hoststatus::Router>,
    last_status: Option<crate::hoststatus::Status>,
    /// the question being shown natively, kept for Godot's dialog if the native one fails
    firewall_prompt: Option<crate::firewall::Prompt>,
    /// the open native dialog's new text (a late answer, dec076)
    firewall_refresh: Option<crate::taskdialog::Refresh>,
    firewall_fixing: Option<std::sync::mpsc::Receiver<Result<(), String>>>,
    missiles: HashMap<usize, MissileVis>,
    flashes: Vec<(Gd<godot::classes::OmniLight3D>, f64)>,
    mapents: crate::mapents::MapEnts,
    brush_meshes: crate::mapents::BrushMeshes,
    /// cgame particle / trail systems (original data only).
    effects: Option<crate::effects::Effects>,
    /// last camera (Quake origin, view angles) for camera-facing effects.
    cam_q: ([f32; 3], [f32; 3]),
    /// cgame view state (cg_view.c / cg_playerstate.c, view.rs).
    cgv: crate::view::ViewState,
    /// `CG_GhostBuildable` model (original data only; else the `ghost` box).
    ghost_model: Option<GhostModel>,
    /// a map loading across frames (the loading screen is up)
    loading: Option<Box<MapLoad>>,
    /// `CG_PlayerFloatSprite` sprites by client
    float_sprites: HashMap<usize, Gd<MeshInstance3D>>,
    /// The network host (dec033): sockets, a listen server, the network
    /// client and its cgame. Created by the first network command or
    /// networked map; the single-process game never touches it.
    nethost: Option<Box<trem_sim::kernel::nethost::NetHost>>,
    /// a listen-server load built the view World of this map; the loopback
    /// client's gamestate binds it
    view_map: Option<String>,
    /// why the last connection ended (`Com_Error` text for the ui)
    net_error: Option<String>,
    /// `UI_DrawConnectScreen`'s facts while connecting
    connect_screen: Option<LoadInfo>,
    /// this frame's usercmd for the network client
    net_cmd: Option<trem_sim::kernel::types::UserCmd>,
    /// the favorites the network client was last given
    lan_fav_addrs: Vec<String>,
    // wt-misc: userinfo/name, console, gamepad, cgame dev commands, warmup (misc.rs)
    misc: crate::misc::Misc,
    // wt-a (dec037): downloads, demos, dedicated (netext.rs)
    netext: netext::NetExt,
    // wt-b: VoIP sound, the video capture, joystick keys for the ui / console catchers
    wtb: WtB,
    /// the graphics mode (gfx.rs): vanilla / enhanced, ray-traced effects
    gfx: crate::gfx::GfxRuntime,
    /// the FMOD + Steam Audio sound path (fmodaudio.rs); None: Godot's audio
    fmod: Option<crate::fmodaudio::FmodAudio>,
    fmod_tried: bool,
    /// the next map's acoustic geometry
    fmod_scene: Option<crate::fmodaudio::AcousticMesh>,
    /// FMOD voices that follow an entity (S_UpdateEntityPosition): (voice, entity)
    fmod_attached: Vec<(i32, usize)>,
    /// positioned FMOD voices (not the player's own): (voice, Quake origin),
    /// for the wall thickness between them and the listener
    fmod_voices: Vec<(i32, [f32; 3])>,
    fmod_frames: u32,
    /// tests: the listener stays where it is (the camera may move)
    fmod_listener_locked: bool,
    /// tests: the blocked point with the strongest path, the farthest
    /// visible point (fmod_path_scan)
    fmod_best_path: [f32; 6],
}

#[godot_api]
impl INode3D for TremGame {
    fn init(base: Base<Node3D>) -> Self {
        TremGame {
            base,
            current: CurrentMap::Sheet(MapId::Arena),
            world: None,
            me: None,
            acc_ms: 0.0,
            browser_acc_ms: 0.0,
            input_enabled: true,
            third_person: false,
            client: crate::clientcmd::Client::new(),
            client_fs: None,
            pending_buttons: 0,
            held_buttons: 0,
            last_spawn: None,
            reset_view: None,
            bots_per_team: 3,
            initial_map_rotation: GString::new(),
            start_in_main_menu: true,
            rotations: Default::default(),
            cvars: Vec::new(),
            exec_ran: false,
            last_evolve_attempt: 0,
            last_build_attempt: 0,
            camera: None,
            view_vp: None,
            vision_on: false,
            vision_toggle: false,
            flashlight: None,
            night_vision: None,
            view_sprite: None,
            vrect: (0, 0, 0, 0),
            view_fov: (90.0, 73.74),
            viewmodel: None,
            ghost: None,
            visuals: HashMap::new(),
            fx: Vec::new(),
            mats: HashMap::new(),
            feed: Vec::new(),
            feed_dropped: 0,
            clock: 0.0,
            lib: None,
            pk3_paths: Vec::new(),
            actors: HashMap::new(),
            failed_actors: HashSet::new(),
            ragdolls: Vec::new(),
            rigs: None,
            ragdoll_world: None,
            ragdoll_brushes: None,
            ragdoll_movers: HashMap::new(),
            ragdoll_solids: HashMap::new(),
            world_hulls: Default::default(),
            world_tris: Default::default(),
            mover_hulls: Default::default(),
            death_push: HashMap::new(),
            alive_vel: HashMap::new(),
            hand: None,
            audio: None,
            lightgrid: None,
            shadow_dirs: None,
            upnp_pending: None,
            port_keepers: Vec::new(),
            port_events: None,
            port_last: None,
            nat_probe: None,
            nat_answers: None,
            firewall_pending: None,
            firewall_missing: Vec::new(),
            firewall_started: false,
            firewall_asked: false,
            firewall_choice: None,
            dialog_godot: false,
            fw_started: None,
            fw_late: None,
            fw_last: None,
            host_t0: None,
            host_events: Vec::new(),
            host_lan_only: false,
            host_report: None,
            router_last: None,
            last_status: None,
            firewall_prompt: None,
            firewall_refresh: None,
            firewall_fixing: None,
            missiles: HashMap::new(),
            flashes: Vec::new(),
            mapents: Default::default(),
            brush_meshes: HashMap::new(),
            effects: None,
            cam_q: ([0.0; 3], [0.0; 3]),
            cgv: Default::default(),
            ghost_model: None,
            loading: None,
            float_sprites: HashMap::new(),
            nethost: None,
            view_map: None,
            net_error: None,
            connect_screen: None,
            net_cmd: None,
            lan_fav_addrs: Vec::new(),
            misc: Default::default(),
            netext: Default::default(),
            wtb: Default::default(),
            gfx: Default::default(),
            fmod: None,
            fmod_tried: false,
            fmod_scene: None,
            fmod_attached: Vec::new(),
            fmod_voices: Vec::new(),
            fmod_frames: 0,
            fmod_best_path: [0.0; 6],
            fmod_listener_locked: false,
        }
    }

    /// Thread-local engine objects (the detail normal map, particle
    /// materials) must go before the engine does: dropping a Gd after the
    /// engine shut down panics.
    fn exit_tree(&mut self) {
        self.upnp_release(true);
        crate::gfx::release_statics();
        crate::hdtex::release();
    }

    fn ready(&mut self) {
        // the GPU's abilities (the graphics modes' global shader uniform
        // trem_gfx is declared in project.godot)
        let caps = crate::gfx::GpuCaps::detect();
        godot_print!("trem: {}", caps.describe());
        self.gfx.caps = Some(caps);
        self.pk3_paths = find_pk3s();
        self.lib = Library::open(&self.pk3_paths);
        match &self.lib {
            Some(_) => {
                let mut dirs: Vec<String> = Vec::new();
                for p in &self.pk3_paths {
                    let d = p.rsplit_once('/').map(|x| x.0.to_string()).unwrap_or_default();
                    if !dirs.contains(&d) {
                        dirs.push(d);
                    }
                }
                godot_print!("trem: Tremulous game data found in {} pk3 file(s) ({}): using original models and sounds", self.pk3_paths.len(), dirs.join(", "))
            }
            None => godot_print!("trem: no Tremulous data pk3 found (user://base, <exe>/base, res://base, --base=DIR): using placeholder shapes"),
        }
        self.init_map_rotations();
        // Com_ExecuteCfg: default.cfg (or the built-in copy), the port's
        // additions, autogen.cfg, autoexec.cfg
        self.client_fs = Some(crate::hostfs::HostFs::new(&self.pk3_paths));
        self.with_client(|c, g| c.startup(g));
        crate::gfx::register_cvars(&mut self.client.cvars);
        crate::fmodaudio::register_cvars(&mut self.client.cvars);
        // the 4K pack is needed before the first map's textures load
        {
            let on = self.client.cvars.integer("r_hdTextures") != 0;
            let path = self.client.cvars.find("r_hdPath").map(|v| v.string.clone()).unwrap_or_default();
            let max = self.client.cvars.integer("r_hdMaxSize").clamp(256, 8192) as usize;
            crate::hdtex::configure(on, &path, max);
            godot_print!("trem: 4K texture pack {}", if crate::hdtex::active() { "found" } else { "not installed (tests/hd_build.tscn makes it)" });
        }
        // Com_StartupVariable / Com_AddStartupCommands: +set and the other
        // "+" commands of the command line (+map / +devmap below)
        let startup = startup_commands();
        if !startup.is_empty() {
            for l in &startup {
                self.client.add_text(&format!("{l}\n"));
            }
            self.with_client(|c, g| c.execute(g));
            for l in self.client.take_output().lines() {
                godot_print!("{l}");
            }
            // wt-a (dec037): `+set dedicated 1|2`: a dedicated server
            if self.dedicated_wanted() {
                self.dedicated_start();
                if let Some((cmd, map)) = startup_map() {
                    let paths = self.pk3_paths.clone();
                    let err = self.begin_load(&paths, &map, Vec::new(), cmd == "devmap");
                    if !err.is_empty() {
                        godot_error!("{err}");
                    }
                }
                return;
            }
            if self.net_active() || self.loading.is_some() {
                return;
            }
        }
        // Com_AddStartupCommands: +map / +devmap; else the main menu (the
        // TremMenus ui runs it) when the original data is there
        let paths = self.pk3_paths.clone();
        if let Some((cmd, map)) = startup_map() {
            if self.lib.is_some() && self.begin_load(&paths, &map, Vec::new(), cmd == "devmap").is_empty() {
                return;
            }
        } else if self.lib.is_some() && self.start_in_main_menu && self.main_menu_scene() {
            return;
        }
        // with the original data, start on ATCS like a fresh Tremulous 1.1 server; else the sheet arena
        if self.lib.is_some() && self.begin_load(&paths, "atcs", Vec::new(), false).is_empty() {
            return;
        }
        self.start_map(MapId::Arena);
    }

    fn process(&mut self, delta: f64) {
        if self.netext.dedicated {
            self.dedicated_process(delta); // wt-a (dec037)
            return;
        }
        let delta = self.wtb_frame(delta); // wt-b: CL_Frame's fixed avi frame time, VoIP idle
        self.firewall_frame(); // dec054: the startup check and its dialog
        if self.loading.is_some() {
            self.step_loading();
            return;
        }
        self.fmod_frame();
        if let Some(cam) = self.camera.as_ref().filter(|c| c.is_inside_tree()).map(|c| c.get_global_position()) {
            let mut tree = self.base().get_tree();
            self.gfx.shadow_budget(&mut tree, cam);
            self.sync_shadow_sun(cam, delta as f32);
        }
        if self.net_active() {
            self.net_process(delta);
            return;
        }
        // Com_Frame -> CL_Frame runs disconnected too: the server browser's
        // answers (infoResponse, getserversResponse) are read there; without
        // it the lists never fill (dec040)
        if let Some(nh) = self.nethost.as_mut() {
            self.browser_acc_ms += delta * 1000.0;
            let mut steps = 0;
            while self.browser_acc_ms >= TICK_MS as f64 && steps < 8 {
                self.browser_acc_ms -= TICK_MS as f64;
                steps += 1;
                nh.frame(TICK_MS, None, None);
            }
            let out = std::mem::take(&mut nh.out);
            for l in out.lines().filter(|l| !l.is_empty()) {
                godot_print!("{l}");
            }
        }
        if self.world.is_none() {
            // disconnected (the main menu): the client still runs its
            // command buffer and writes autogen.cfg
            self.client_frame();
            return;
        }
        self.clock += delta;
        self.client_frame();
        self.write_cmd();
        self.acc_ms += delta * 1000.0;
        let mut steps = 0;
        while self.acc_ms >= TICK_MS as f64 && steps < 8 {
            self.acc_ms -= TICK_MS as f64;
            steps += 1;
            let w = self.world.as_mut().unwrap();
            w.real_time = real_time();
            w.frame(TICK_MS);
            let evs = w.drain_events();
            self.after_tick();
            self.transition_player_state();
            for e in evs {
                self.on_event(&e);
            }
            // "chat" / "print" server commands addressed to us
            if let (Some(w), Some(me)) = (self.world.as_mut(), self.me) {
                let team = w.clients[me].team;
                for (_, tm, cl, line) in std::mem::take(&mut w.prints) {
                    if tm.is_none_or(|x| x == team) && cl.is_none_or(|x| x == me) {
                        self.feed.push((self.clock, strip_colors(&line)));
                    }
                }
                for (_, to, _, mode, line) in std::mem::take(&mut w.chat) {
                    if to == me {
                        let tag = match mode {
                            trem_sim::kernel::cmds::SayMode::All => "",
                            trem_sim::kernel::cmds::SayMode::Team => "(team) ",
                            trem_sim::kernel::cmds::SayMode::Area => "(area) ",
                            // the line carries [ADMIN]/[PLAYER], or is raw console text
                            trem_sim::kernel::cmds::SayMode::Admins | trem_sim::kernel::cmds::SayMode::AdminsPublic | trem_sim::kernel::cmds::SayMode::Raw => "",
                            _ => "(private) ",
                        };
                        self.feed.push((self.clock, format!("{tag}{}", strip_colors(&line))));
                        self.misc.chat_sounds.push(mode); // wt-misc: CG_Say's talk sound
                    }
                }
            }
            // trap_SendConsoleCommand( EXEC_APPEND ) from votes / admin commands
            if self.run_exec() {
                return;
            }
            // ExitLevel -> map_restart
            if self.world.as_ref().is_some_and(|w| w.level.exited) {
                // g_nextMap / a passed "map" vote, else map_restart
                // (ExitLevel: g_nextMap, else G_AdvanceMapRotation, else map_restart)
                let mut next = self.world.as_ref().and_then(|w| w.level.next_map.clone());
                let mut post = String::new();
                if next.is_none() && self.rotations.active() {
                    if let Some(ch) = self.advance_rotation() {
                        next = Some(ch.map);
                        post = ch.post_command;
                    }
                }
                let paths = match &self.current {
                    CurrentMap::Bsp(p, _) => p.clone(),
                    _ => self.pk3_paths.clone(),
                };
                let then = if post.is_empty() { Vec::new() } else { vec![post.clone()] };
                if next.is_none() || !self.begin_load(&paths, next.as_deref().unwrap(), then, false).is_empty() {
                    self.restart();
                    if !post.is_empty() {
                        self.server_command(&post);
                    }
                }
                return;
            }
        }
        if steps == 8 {
            self.acc_ms = 0.0;
        }
        let t0 = std::time::Instant::now();
        self.sync_visuals();
        let t1 = t0.elapsed().as_millis();
        self.sync_camera();
        self.sync_vision();
        self.misc_frame(); // wt-misc: cgame rename prints, vote beep, dev commands
        self.update_fx(delta);
        let t2 = t0.elapsed().as_millis();
        self.update_effects();
        let t3 = t0.elapsed().as_millis();
        if let Some(path) = std::env::var_os("TREM_FRAME_LOG") {
            use std::io::Write;
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
                let n = self.base().get_child_count();
                let _ = writeln!(f, "frame clock={:.3} delta={:.3} rust={t3}ms children={n} actors={} visuals={} missiles={}", self.clock, delta, self.actors.len(), self.visuals.len(), self.missiles.len());
            }
        }
        if t3 > 150 && std::env::var_os("TREM_FX_DEBUG").is_some() {
            godot_print!("slow presentation frame: visuals {t1} ms, camera+fx {} ms, effects {} ms", t2 - t1, t3 - t2);
        }
    }

    /// sdl_input.c's events into the client: `CL_KeyEvent` for keys and
    /// mouse buttons (the bound commands run at once), `CL_MouseEvent` for
    /// motion while the mouse is captured. The HUD (hud.gd) handles Escape
    /// and the click that recaptures the mouse before this.
    fn unhandled_input(&mut self, event: Gd<InputEvent>) {
        if let Ok(m) = event.clone().try_cast::<InputEventMouseMotion>() {
            if self.input_enabled && Input::singleton().get_mouse_mode() == MouseMode::CAPTURED {
                let r = m.get_relative();
                self.client.mouse_event(r.x, r.y);
            }
            return;
        }
        let (key, down) = if let Ok(k) = event.clone().try_cast::<InputEventKey>() {
            // SDL key repeats are ignored without a key catcher
            if k.is_echo() {
                return;
            }
            let code = if k.get_keycode() != Key::NONE { k.get_keycode() } else { k.get_physical_keycode() };
            (crate::keys::keynum_from_godot_key(code), k.is_pressed())
        } else if let Ok(b) = event.clone().try_cast::<InputEventMouseButton>() {
            (crate::keys::keynum_from_godot_mouse(b.get_button_index()), b.is_pressed())
        } else {
            return;
        };
        if key <= 0 {
            return;
        }
        self.key_event_num(key, down);
    }
}

fn put(d: &mut VarDictionary, key: &str, v: impl ToGodot) {
    d.set(&key.to_variant(), &v.to_variant());
}

/// Q_CleanStr: drop ^N colour escapes.
fn strip_colors(s: &str) -> String {
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

fn class_name(c: i32) -> String {
    ClassId::from_num(c as u8).map(|c| c.cold().display_name.to_string()).unwrap_or_default()
}
fn weapon_name(w: i32) -> String {
    WeaponId::from_num(w as u8).filter(|_| w > 0).map(|w| w.cold().display_name.to_string()).unwrap_or_default()
}
fn team_name(t: Team) -> &'static str {
    match t {
        Team::Aliens => "Aliens",
        Team::Humans => "Humans",
        Team::None => "Spectators",
    }
}
fn team_from(s: &str) -> Team {
    match s {
        "aliens" | "a" => Team::Aliens,
        "humans" | "h" => Team::Humans,
        _ => Team::None,
    }
}

#[godot_api]
impl TremGame {
    /// (Re)start a match on the arena with `bots_per_team` bots per side.
    #[func]
    fn restart(&mut self) {
        match &self.current {
            CurrentMap::Sheet(m) => {
                let m = *m;
                self.start_map(m)
            }
            CurrentMap::Bsp(paths, name) => {
                let (p, n) = (paths.clone(), name.clone());
                let err = self.load_bsp_inner(&p, &n);
                if !err.is_empty() {
                    godot_error!("{err}");
                    self.start_map(MapId::Arena);
                }
            }
        }
    }

    /// Rotations from maprotation.cfg.
    #[func]
    fn map_rotations(&self) -> PackedStringArray {
        let mut out = PackedStringArray::new();
        for r in &self.rotations.rotations {
            out.push(r.name.as_str());
        }
        out
    }

    /// `maprotation <name>` and go straight to its first map.
    #[func]
    fn start_map_rotation(&mut self, name: GString) -> bool {
        self.rotations.clear_stack();
        if !self.rotations.begin(&name.to_string()) {
            return false;
        }
        let Some(ch) = self.advance_rotation() else { return false };
        let paths = self.pk3_paths.clone();
        let then = if ch.post_command.is_empty() { Vec::new() } else { vec![ch.post_command.clone()] };
        self.begin_load(&paths, &ch.map, then, false).is_empty()
    }

    /// Server console (`set timelimit 30`, `g_cheats 1`, `maprotation r`,
    /// the game's svcmds and admin commands). Returns the console output.
    #[func]
    fn console(&mut self, line: GString) -> GString {
        self.finish_loading();
        let out = self.console_line(&line.to_string());
        let more = self.run_exec_output();
        GString::from(format!("{out}{more}").trim_end())
    }

    /// Is this command for the server console rather than the client
    /// command path (svcmds, map / rotation / cvar commands)?
    #[func]
    fn is_server_command(&self, word: GString) -> bool {
        self.is_server_word(&word.to_string())
    }

    /// `port_gfx <arg>` (gfx.rs) for scripts: returns the mode description.
    #[func]
    fn gfx(&mut self, arg: GString) -> GString {
        self.port_gfx(&arg.to_string());
        GString::from(self.gfx.describe().trim_end())
    }

    /// The view origin (Quake units) and yaw, as `viewpos` prints them.
    #[func]
    fn view_pos(&self) -> Vector4 {
        let (o, a) = self.cam_q;
        Vector4::new(o[0], o[1], o[2], a[1])
    }

    /// g_cheats (devmap): enables give/god/noclip/notarget/setviewpos/destroy.
    #[func]
    fn set_cheats(&mut self, on: bool) {
        self.finish_loading();
        if let Some(w) = self.game_world_mut() {
            w.level.cheats = on;
        }
    }

    /// Test hook: spawn particle system `name` at a Godot-space point.
    #[func]
    fn spawn_fx_named(&mut self, name: GString, at: Vector3) -> bool {
        self.finish_loading();
        let now = (self.clock * 1000.0) as i32;
        let q = [at.x / crate::geom::SCALE, -at.z / crate::geom::SCALE, at.y / crate::geom::SCALE];
        self.effects.as_mut().is_some_and(|fx| fx.spawn_named(&name.to_string(), q, now))
    }

    /// The pk3 search path found at startup.
    #[func]
    fn pk3_paths(&self) -> PackedStringArray {
        let mut out = PackedStringArray::new();
        for p in &self.pk3_paths {
            out.push(p.as_str());
        }
        out
    }

    /// True when original Tremulous data (models, sounds) is in use.
    #[func]
    fn has_game_data(&self) -> bool {
        self.lib.is_some()
    }

    /// True while a map is loaded (`clc.state == CA_ACTIVE`); false at the
    /// main menu.
    #[func]
    pub(crate) fn has_world(&self) -> bool {
        // a networked view is up once the cgame runs (CA_ACTIVE)
        self.world.is_some() && (!self.net_active() || self.me.is_some())
    }

    /// `CL_Disconnect` + `SV_Shutdown`: drop the map and its scene (the
    /// ui then shows the main menu).
    #[func]
    pub(crate) fn shutdown_map(&mut self) {
        // (G_WriteSessionData is for map changes; a shutdown drops the sessions)
        // CL_Disconnect + SV_Shutdown of the network host (dec033)
        self.upnp_release(false);
        if let Some(nh) = self.nethost.as_mut() {
            nh.disconnect();
            for l in std::mem::take(&mut nh.out).lines() {
                godot_print!("{l}");
            }
        }
        self.view_map = None;
        self.connect_screen = None;
        self.net_cmd = None;
        if self.loading.as_ref().is_some_and(|l| l.mode != LoadMode::Single) {
            self.loading = None;
        }
        let children = self.base().get_children();
        for mut ch in children.iter_shared() {
            ch.queue_free();
        }
        self.world = None;
        self.me = None;
        self.visuals.clear();
        self.fx.clear();
        self.camera = None;
        self.viewmodel = None;
        self.ghost = None;
        self.ghost_model = None;
        self.cgv.reset();
        self.actors.clear();
        self.free_ragdolls();
        self.death_push.clear();
        self.alive_vel.clear();
        self.missiles.clear();
        self.flashes.clear();
        self.hand = None;
        self.audio = None;
        self.mapents = Default::default();
        self.brush_meshes.clear();
        self.effects = None;
        self.lightgrid = None;
        self.shadow_dirs = None;
        self.feed.clear();
    }

    /// Back to the built-in sheet arena.
    #[func]
    fn load_arena(&mut self) {
        self.start_map(MapId::Arena);
    }

    /// Map names (maps/<name>.bsp) found in a set of pk3 files.
    #[func]
    fn list_maps(&self, pk3_paths: PackedStringArray) -> PackedStringArray {
        let paths: Vec<String> = pk3_paths.as_slice().iter().map(|s| s.to_string()).collect();
        let set = crate::assets::Pk3Set::open(&paths);
        let mut out = PackedStringArray::new();
        for m in set.maps() {
            out.push(m.as_str());
        }
        out
    }

    /// Load maps/<name>.bsp from the pk3 search path (later pk3s override).
    /// Returns "" or an error.
    #[func]
    fn load_bsp(&mut self, pk3_paths: PackedStringArray, name: GString) -> GString {
        let paths: Vec<String> = pk3_paths.as_slice().iter().map(|s| s.to_string()).collect();
        GString::from(self.load_bsp_inner(&paths, &name.to_string()).as_str())
    }

    /// Load maps/<name>.bsp across frames with the loading screen up
    /// (`CG_DrawLoadingScreen`). Returns "" or an error (the BSP is read
    /// and parsed at once; the rest follows over the next frames).
    #[func]
    fn load_map(&mut self, pk3_paths: PackedStringArray, name: GString) -> GString {
        let paths: Vec<String> = pk3_paths.as_slice().iter().map(|s| s.to_string()).collect();
        GString::from(self.begin_load(&paths, &name.to_string(), Vec::new(), false).as_str())
    }

    /// Turn the view by `yaw` degrees (cl.viewangles, as mouse motion
    /// would; tests).
    #[func]
    fn turn_view(&mut self, yaw: f32) {
        self.client.input.viewangles[1] += yaw;
    }

    /// The network session in one line (tests, debugging): the client's
    /// state and slot, the server's map and clients ("" without one).
    #[func]
    fn net_status(&self) -> GString {
        let Some(nh) = self.nethost.as_ref() else { return GString::new() };
        let mut s = format!("cl={:?} num={} server={}", nh.cl.state, nh.cl.client_num, nh.cl.servername);
        if let Some(sv) = nh.server.as_ref() {
            s.push_str(&format!(" sv_map={} sv_active={} sv_port={}", sv.map_name, sv.active_clients(), nh.net.ports[trem_sim::kernel::netchan::NetSrc::Server as usize]));
        }
        if let (Some(w), Some(me)) = (self.world.as_ref(), self.me) {
            let ps = &w.clients[me].ps;
            let others: Vec<String> = (0..MAX_CLIENTS).filter(|&i| i != me && w.clients[i].connected).map(|i| format!("{}@{:.0},{:.0},{:.0}", w.clients[i].name, w.ents[i].origin[0], w.ents[i].origin[1], w.ents[i].origin[2])).collect();
            s.push_str(&format!(" me={} origin={:.0},{:.0},{:.0} hp={} ping={} weapon={} ammo={} clips={} wstate={} others=[{}]", w.clients[me].name, ps.origin[0], ps.origin[1], ps.origin[2], ps.stats[Stat::Health as usize], ps.ping, ps.weapon, ps.ammo, ps.clips, ps.weaponstate, others.join(" ")));
            if let Some(nh) = self.nethost.as_ref() {
                let sp = &nh.cl.snap.ps;
                s.push_str(&format!(" snap: weapon={} ammo={} clips={} misc={:?} ps11.weapon={} ps11.ammo={:?}", sp.weapon, sp.ammo, sp.clips, sp.misc, nh.cl.snap.ps11.weapon, nh.cl.snap.ps11.ammo));
            }
        }
        GString::from(s.as_str())
    }

    /// `com_dedicated` (wt-a, dec037): hud.gd removes the ui.
    #[func]
    fn is_dedicated(&self) -> bool {
        self.netext.dedicated
    }

    /// Connecting to a server or loading its map (clc.state between
    /// CA_CONNECTING and CA_PRIMED; the connect / loading screen is up).
    #[func]
    pub(crate) fn is_connecting(&self) -> bool {
        self.connect_screen.is_some() || self.loading.as_ref().is_some_and(|l| l.mode == LoadMode::Client)
    }

    /// A map is loading (the loading screen is up).
    #[func]
    pub(crate) fn is_loading(&self) -> bool {
        self.loading.is_some() || self.connect_screen.is_some()
    }

    /// The camera of the 3D view (it renders into the view SubViewport, so
    /// the root viewport's `get_camera_3d` does not return it).
    #[func]
    fn view_camera(&self) -> Option<Gd<Camera3D>> {
        self.camera.clone()
    }

    /// Write the built-in arena as maps/arena.bsp inside a pk3 (a test map for
    /// the BSP path and a starting point for mappers).
    #[func]
    fn export_arena_pk3(&self, path: GString) -> GString {
        let bytes = trem_sim::kernel::bsp::from_sheet(MapId::Arena).write();
        let mut z = godot::classes::ZipPacker::new_gd();
        if z.open(&path) != godot::global::Error::OK {
            return "cannot create pk3".into();
        }
        z.start_file("maps/arena.bsp");
        z.write_file(&PackedByteArray::from(bytes.as_slice()));
        z.close_file();
        // one panel texture per sheet material, so the BSP renders like the sheet map
        for m in MATERIALS.iter() {
            let Some(mut img) = Image::create(64, 64, false, godot::classes::image::Format::RGB8) else { continue };
            let c = hex_color(m.color);
            img.fill(c);
            for a in 0..64 {
                for b in [0, 1] {
                    img.set_pixel(a, b, c.darkened(0.35));
                    img.set_pixel(b, a, c.darkened(0.35));
                }
            }
            let png = img.save_png_to_buffer();
            z.start_file(&format!("textures/sheet/{}.png", m.id));
            z.write_file(&png);
            z.close_file();
        }
        z.close();
        GString::new()
    }

    /// Run a client command for the local player ("team humans", "buy shotgun", ...).
    /// Returns "" on success or the error text.
    #[func]
    fn command(&mut self, line: GString) -> GString {
        self.finish_loading();
        GString::from(self.client_command_line(&line.to_string()).as_str())
    }

    /// A command for the Godot side of the client (`ClientHost::host`):
    /// togglemenu / escape, toggleconsole, messagemode, messagemode2,
    /// menu <name>, closemenus, +scores, -scores, scoresUp, scoresDown,
    /// quit, disconnect, screenshot, screenshotJPEG, port_restart,
    /// port_maps, port_help, clear. Emitted deferred (hud.gd calls back).
    #[signal]
    fn host_command(line: GString);

    /// Console text through the client's command buffer (`Cbuf_AddText` +
    /// `Cbuf_Execute`): binds, cvars, cgame / ui commands, the server
    /// console, else a client command for the server. Returns the output.
    #[func]
    fn exec(&mut self, text: GString) -> GString {
        self.finish_loading();
        let mut t = text.to_string();
        if !t.ends_with('\n') {
            t.push('\n');
        }
        self.client.add_text(&t);
        self.with_client(|c, g| c.execute(g));
        let out = self.client.take_output();
        for l in out.lines() {
            godot_print!("{l}");
        }
        self.misc.con.print(&out); // wt-misc: CL_ConsolePrint
        GString::from(out.trim_end())
    }

    /// `CL_KeyEvent` for a keycodes.h key number (scripts / tests).
    #[func]
    fn key_event(&mut self, keynum: i32, down: bool) {
        self.key_event_num(keynum, down);
    }

    /// keycodes.h number of a key name (`Key_StringToKeynum`), -1 if none.
    #[func]
    fn key_number(&self, name: GString) -> i32 {
        crate::keys::key_string_to_keynum(&name.to_string())
    }

    /// The keys bound to `command` as `CG_KeyNameForCommand` names them
    /// ("" when unbound).
    #[func]
    fn keys_for(&self, command: GString) -> GString {
        let c = command.to_string();
        let b = self.client.keys.bindings();
        let keys: Vec<String> = (0..crate::keys::K_LAST_KEY).filter(|&k| !b[k as usize].is_empty() && b[k as usize].eq_ignore_ascii_case(&c)).take(2).map(|k| crate::keys::key_keynum_to_string(k).to_uppercase()).collect();
        GString::from(keys.join(" or ").as_str())
    }

    #[func]
    fn add_bots(&mut self, team: GString, n: i32) {
        self.finish_loading();
        let Some(w) = self.game_world_mut() else { return };
        let t = team_from(&team.to_string());
        for _ in 0..n {
            let id = (0..MAX_CLIENTS).filter(|&i| w.clients[i].connected).count();
            let name = format!("{}{}", if t == Team::Aliens { "Xeno" } else { "Marine" }, id);
            bot::add_bot(w, &name, t);
        }
    }

    #[func]
    fn remove_bot(&mut self, team: GString) {
        self.finish_loading();
        if let Some(w) = self.game_world_mut() {
            bot::remove_bot(w, Some(team_from(&team.to_string())));
        }
    }

    /// Godot-space centres of the live entities with this classname (tests, debugging).
    #[func]
    fn find_entities(&self, classname: GString) -> PackedVector3Array {
        let Some(w) = self.world.as_ref() else { return PackedVector3Array::new() };
        let c = classname.to_string();
        (0..w.num_entities).filter(|&i| w.ents[i].inuse && w.ents[i].classname == c).map(|i| q2g(&crate::mapents::sound_origin(w, i))).collect()
    }

    /// Debug hook (tests): the animated-model actors and the structure keys that failed to build.
    #[func]
    fn actor_info(&self) -> PackedStringArray {
        let mut out = PackedStringArray::new();
        let mut keys: Vec<_> = self.actors.keys().copied().collect();
        keys.sort();
        for i in keys {
            let a = &self.actors[&i];
            let o = self.world.as_ref().map(|w| w.ents[i].origin).unwrap_or([0.0; 3]);
            out.push(&format!("ent {i} origin=({:.0},{:.0},{:.0}) {}", o[0], o[1], o[2], a.debug_line()));
        }
        for k in &self.failed_actors {
            out.push(&format!("FAILED {k}"));
        }
        out
    }

    /// A free camera spot looking at the `n`th entity of a classname (tests).
    #[func]
    fn entity_view(&self, classname: GString, n: i32) -> PackedVector3Array {
        let Some(w) = self.world.as_ref() else { return PackedVector3Array::new() };
        let c = classname.to_string();
        let Some(i) = (0..w.num_entities).filter(|&i| w.ents[i].inuse && w.ents[i].classname == c).nth(n.max(0) as usize) else { return PackedVector3Array::new() };
        let at = crate::mapents::sound_origin(w, i);
        let mut best = (0.0f32, at);
        for dist in [160.0f32, 112.0, 80.0] {
            for k_ in 0..8 {
                let yaw = k_ as f32 * 45.0;
                let (f, _, _) = angle_vectors(&[-10.0, yaw, 0.0]);
                let p = ma(&at, dist, &f);
                let tr = w.trace(&at, Some(&[-8.0; 3]), Some(&[8.0; 3]), &p, i as i32, k::MASK_SOLID);
                let d = tr.fraction * dist;
                if d > best.0 {
                    best = (d, tr.endpos);
                }
            }
            if best.0 >= dist * 0.95 {
                break;
            }
        }
        [q2g(&best.1), q2g(&at)].into_iter().collect()
    }

    /// The local player's HUD state.
    #[func]
    fn get_hud(&mut self) -> VarDictionary {
        let mut d = VarDictionary::new();
        let (Some(w), Some(me)) = (self.world.as_mut(), self.me) else { return d };
        let c = &w.clients[me];
        let ps = &c.ps;
        put(&mut d, "time", w.level.time);
        put(&mut d, "team", team_name(c.team));
        put(&mut d, "spectating", c.spectator != SpectatorState::Not as i32);
        put(&mut d, "queued", ps.pm_flags & k::PMF_QUEUED != 0);
        put(&mut d, "queue_pos", ps.persistant[trem_sim::gen::enums::Pers::Queuepos as usize]);
        let following = ps.pm_flags & k::PMF_FOLLOW != 0;
        put(&mut d, "following", if following { w.clients.get(ps.client_num as usize).map(|f| f.name.clone()).unwrap_or_default() } else { String::new() });
        put(&mut d, "alive", (following || c.spectator == SpectatorState::Not as i32) && ps.stats[Stat::Health as usize] > 0);
        put(&mut d, "health", ps.stats[Stat::Health as usize].max(0));
        put(&mut d, "max_health", ps.stats[Stat::MaxHealth as usize]);
        put(&mut d, "class", class_name(ps.stats[Stat::Class as usize]));
        put(&mut d, "class_id", ClassId::from_num(ps.stats[Stat::Class as usize] as u8).map(|c| c.id()).unwrap_or(""));
        put(&mut d, "weapon", weapon_name(ps.weapon));
        put(&mut d, "weapon_id", WeaponId::from_num(ps.weapon as u8).filter(|_| ps.weapon > 0).map(|w| w.id()).unwrap_or(""));
        let wd = bg::weapon(ps.weapon);
        put(&mut d, "ammo", if wd.infinite_ammo { -1 } else { ps.ammo });
        put(&mut d, "clips", ps.clips);
        put(&mut d, "funds", w.display_funds(me));
        put(&mut d, "score", c.score);
        put(&mut d, "stamina", ps.stats[Stat::Stamina as usize]);
        put(&mut d, "bp", ps.persistant[trem_sim::gen::enums::Pers::Bp as usize]);
        put(&mut d, "build_timer", ps.stats[Stat::Misc as usize]);
        let sb = ps.stats[Stat::Buildable as usize];
        put(&mut d, "blueprint", BuildableId::from_num((sb & !k::SB_VALID_TOGGLEBIT) as u8).filter(|_| sb & !k::SB_VALID_TOGGLEBIT > 0).map(|b| b.cold().display_name).unwrap_or(""));
        put(&mut d, "blueprint_valid", sb & k::SB_VALID_TOGGLEBIT != 0);
        put(&mut d, "alien_stage", w.level.alien_stage + 1);
        put(&mut d, "human_stage", w.level.human_stage + 1);
        put(&mut d, "alien_credits", w.level.alien_credits);
        put(&mut d, "human_credits", w.level.human_credits);
        put(&mut d, "alien_next_stage", w.level.alien_next_stage);
        put(&mut d, "human_next_stage", w.level.human_next_stage);
        put(&mut d, "damage", ps.damage_count);
        let state = ps.stats[Stat::State as usize];
        let mut flags = Vec::new();
        for (bit, name) in [(k::SS_POISONED, "Poisoned"), (k::SS_BOOSTED, "Boosted"), (k::SS_BLOBLOCKED, "Trapped"), (k::SS_GRABBED, "Grabbed"), (k::SS_CREEPSLOWED, "Creep"), (k::SS_HEALING_2X, "Healing")] {
            if state & bit != 0 {
                flags.push(name);
            }
        }
        if bg::upgrade_is_active(UpgradeId::Jetpack.num() as i32, &ps.stats) && bg::inventory_contains_upgrade(UpgradeId::Jetpack.num() as i32, &ps.stats) {
            flags.push("Jetpack");
        }
        put(&mut d, "status", flags.join(" "));
        let items: Vec<&str> = UPGRADES.iter().filter(|u| bg::inventory_contains_upgrade(u.num as i32, &ps.stats)).map(|u| UpgradeId::from_num(u.num).unwrap().cold().display_name).collect();
        put(&mut d, "items", items.join(", "));
        if let Some((t, m)) = &c.message {
            if w.level.time - *t < 4000 {
                put(&mut d, "message", m.as_str());
            }
        }
        put(&mut d, "wants_menu", std::mem::take(&mut w.clients[me].wants_menu));
        let winner = match (w.level.intermission_queued != 0 || w.level.intermission_time != 0, w.level.winner) {
            (true, Some(Team::Aliens)) => "Aliens win!",
            (true, Some(Team::Humans)) => "Humans win!",
            (true, _) => "Stalemate",
            _ => "",
        };
        put(&mut d, "winner", winner);
        put(&mut d, "intermission", w.level.intermission_time != 0);
        // Team_GetLocation (target_location messages, colour codes stripped)
        let loc = trem_sim::kernel::team::get_location(w, me).and_then(|l| w.location_names.get(w.ents[l].generic1 as usize)).map(|s| strip_colors(s)).unwrap_or_default();
        put(&mut d, "location", loc);
        // trap_SendServerCommand "cp": target_print, sudden death, timelimit, votes
        let now = w.level.time;
        let team = w.clients[me].team;
        let cp = w.center_prints.iter().rev().find(|(t, tm, cl, _)| now - *t < 3000 && tm.is_none_or(|x| x == team) && cl.is_none_or(|x| x == me)).map(|c| c.3.clone()).unwrap_or_default();
        put(&mut d, "center_print", cp);
        if w.center_prints.len() > 64 {
            w.center_prints.drain(..32);
        }
        // CS_VOTE_*: the global vote, then our team's
        let mut votes = Vec::new();
        for (label, t) in [("VOTE", Team::None), ("TEAMVOTE", team)] {
            let v = &w.level.votes[t as usize];
            if v.time != 0 && (label == "VOTE" || t != Team::None) {
                let left = ((trem_sim::kernel::vote::VOTE_TIME - (now - v.time)) / 1000).max(0);
                votes.push(format!("{label}({left}): {} [{}]  yes:{} no:{}", v.display, v.caller, v.yes, v.no));
            }
        }
        put(&mut d, "vote", votes.join("\n"));
        put(&mut d, "ready", w.clients[me].ready_to_exit);
        let now = self.clock;
        let before = self.feed.len();
        self.feed.retain(|(t, _)| now - t < 6.0);
        self.feed_dropped += (before - self.feed.len()) as u64;
        put(&mut d, "feed", self.feed.iter().map(|x| x.1.clone()).collect::<Vec<_>>().join("\n"));
        let my_team = w.clients[me].team;
        if let Some(stage) = w.level.stage_events.pop() {
            self.feed.push((now, format!("{} reached stage {}", team_name(stage.0), stage.1)));
            // cgs.media.alienStageTransition / humanStageTransition, for that team only
            if stage.0 == my_team {
                let snd = if my_team == Team::Aliens { "sound/announcements/overmindevolved.wav" } else { "sound/announcements/reinforcement.wav" };
                self.play_sound(snd, &[0.0; 3], true);
            }
        }
        d
    }

    /// Scoreboard rows.
    #[func]
    fn get_scores(&self) -> VarArray {
        let mut a = VarArray::new();
        let Some(w) = self.world.as_ref() else { return a };
        let mut rows: Vec<usize> = (0..MAX_CLIENTS).filter(|&i| w.clients[i].connected).collect();
        rows.sort_by_key(|&i| (w.clients[i].team as i32, -w.clients[i].score));
        for i in rows {
            let c = &w.clients[i];
            let mut d = VarDictionary::new();
            put(&mut d, "name", c.name.as_str());
            put(&mut d, "team", team_name(c.team));
            put(&mut d, "score", c.score);
            put(&mut d, "kills", c.kills);
            put(&mut d, "deaths", c.deaths);
            put(&mut d, "class", class_name(c.ps.stats[Stat::Class as usize]));
            put(&mut d, "me", Some(i) == self.me);
            a.push(&d.to_variant());
        }
        a
    }

    /// Menu rows: kind = team | spawn | evolve | armoury | build.
    #[func]
    fn menu_items(&self, kind: GString) -> VarArray {
        let mut a = VarArray::new();
        let (Some(w), Some(me)) = (self.world.as_ref(), self.me) else { return a };
        let c = &w.clients[me];
        let mut row = |cmd: String, label: String, detail: String, ok: bool| {
            let mut d = VarDictionary::new();
            put(&mut d, "cmd", cmd.as_str());
            put(&mut d, "label", label.as_str());
            put(&mut d, "detail", detail.as_str());
            put(&mut d, "enabled", ok);
            a.push(&d.to_variant());
        };
        // a Tremulous 1.1.0 game: the 1.1.0 ui lists (menus.rs, dec041)
        if w.vanilla && kind.to_string() != "team" {
            let p = crate::menus::Player::from_world(w, me);
            let entries = match kind.to_string().as_str() {
                "spawn" if c.team == Team::Aliens => crate::menus::load_alien_classes(&p),
                "spawn" if c.team == Team::Humans => crate::menus::load_human_items(&p),
                "evolve" => crate::menus::load_alien_upgrades(&p),
                "armoury" => {
                    let mut v = crate::menus::load_armoury_buys(&p);
                    v.extend(crate::menus::load_armoury_sells(&p));
                    v
                }
                "build" => crate::menus::load_builds(&p, c.team),
                _ => Vec::new(),
            };
            for e in entries {
                let detail = crate::menus::info_pane_text(&e, &p).rsplit("\n\n").next().unwrap_or("").to_string();
                row(e.cmd, e.text, detail, true);
            }
            return a;
        }
        match kind.to_string().as_str() {
            "team" => {
                row("team aliens".into(), "Aliens".into(), format!("{} players", w.level.num_alien_clients), true);
                row("team humans".into(), "Humans".into(), format!("{} players", w.level.num_human_clients), true);
                row("team spectate".into(), "Spectate".into(), String::new(), true);
            }
            "spawn" => match c.team {
                Team::Aliens => {
                    for cl in [ClassId::Level0, ClassId::Builder, ClassId::Builderupg] {
                        let ok = bg::class_allowed_in_stage(cl, w.level.alien_stage);
                        row(format!("class {}", cl.id()), cl.cold().display_name.into(), String::new(), ok);
                    }
                }
                Team::Humans => {
                    row("class rifle".into(), "Rifle".into(), "assault".into(), true);
                    row("class ckit".into(), "Construction Kit".into(), "builder".into(), true);
                }
                Team::None => {}
            },
            "evolve" => {
                let cur = bg::class_id(c.ps.stats[Stat::Class as usize]);
                for cl in CLASSES.iter().map(|x| ClassId::from_num(x.num).unwrap()) {
                    if cl.def().team != Team::Aliens || cl == cur {
                        continue;
                    }
                    let cost = bg::class_can_evolve_from_to(cur, cl, c.credit, w.level.alien_stage, 0);
                    if cost < 0 && !cur.def().evolves_to.contains(&cl) {
                        continue;
                    }
                    let detail = if cost >= 0 { format!("{} evos", cost / k::ALIEN_CREDITS_PER_KILL) } else { "unavailable".into() };
                    row(format!("class {}", cl.id()), cl.cold().display_name.into(), detail, cost >= 0);
                }
            }
            "armoury" => {
                for wd in WEAPONS.iter().filter(|x| x.team == Team::Humans && x.purchasable) {
                    let id = WeaponId::from_num(wd.num).unwrap();
                    let held = bg::inventory_contains_weapon(wd.num as i32, &c.ps.stats);
                    let ok = bg::weapon_allowed_in_stage(id, w.level.human_stage) && wd.price <= c.credit && !held;
                    let cmd = if held { format!("sell {}", id.id()) } else { format!("buy {}", id.id()) };
                    row(cmd, id.cold().display_name.into(), if held { format!("sell +{}", wd.price) } else { format!("{} cr", wd.price) }, ok || held);
                }
                for ud in UPGRADES.iter().filter(|x| x.team == Team::Humans && x.purchasable) {
                    let id = UpgradeId::from_num(ud.num).unwrap();
                    let held = bg::inventory_contains_upgrade(ud.num as i32, &c.ps.stats);
                    let ok = bg::upgrade_allowed_in_stage(id, w.level.human_stage) && ud.price <= c.credit && !held;
                    let cmd = if held { format!("sell {}", id.id()) } else { format!("buy {}", id.id()) };
                    row(cmd, id.cold().display_name.into(), if held { format!("sell +{}", ud.price) } else { format!("{} cr", ud.price) }, ok || held);
                }
                row("buy ammo".into(), "Ammunition".into(), "free".into(), true);
            }
            "build" => {
                let stage = if c.team == Team::Aliens { w.level.alien_stage } else { w.level.human_stage };
                for bd in BUILDABLES.iter() {
                    let id = BuildableId::from_num(bd.num).unwrap();
                    if !bd.build_weapons.iter().any(|x| x.num() as i32 == c.ps.weapon) {
                        continue;
                    }
                    let ok = bg::buildable_allowed_in_stage(id, stage);
                    row(format!("build {}", id.id()), id.cold().display_name.into(), format!("{} bp", bd.build_points), ok);
                }
            }
            _ => {}
        }
        a
    }
}

impl TremGame {
    /// Read access for other Rust-side nodes (the HUD): the world, the local
    /// client, the client whose view is shown (followed player), and the
    /// original-data library.
    pub(crate) fn hud_world(&self) -> Option<&World> {
        self.world.as_ref()
    }
    /// cg.lastEvolveAttempt / cg.lastBuildAttempt (level time, 0 = never).
    pub(crate) fn hud_attempts(&self) -> (i32, i32) {
        (self.last_evolve_attempt, self.last_build_attempt)
    }
    pub(crate) fn hud_me(&self) -> Option<usize> {
        self.me
    }
    pub(crate) fn hud_view_client(&self) -> Option<usize> {
        self.view_ent()
    }
    pub(crate) fn hud_library(&mut self) -> Option<&mut Library> {
        self.lib.as_mut()
    }
    /// The console/notify feed (clock seconds, line): chat, prints and
    /// obituaries, for the HUD's CG_CONSOLE; and the number of lines ever
    /// added (the feed only drops lines from its front).
    pub(crate) fn hud_feed(&self) -> (&[(f64, String)], u64) {
        (&self.feed, self.feed_dropped + self.feed.len() as u64)
    }
    /// cg.refdef for the HUD: view origin and angles (Quake) and
    /// fov_x, fov_y in degrees.
    pub(crate) fn hud_camera(&self) -> Option<([f32; 3], [f32; 3], f32, f32)> {
        self.camera.as_ref()?;
        Some((self.cam_q.0, self.cam_q.1, self.view_fov.0, self.view_fov.1))
    }
    /// cg.refdef x, y, width, height in pixels (`CG_CalcVrect`; `CG_TileClear`).
    pub(crate) fn hud_vrect(&self) -> (i32, i32, i32, i32) {
        self.vrect
    }
    /// cg_viewsize (the scanner / buildable status clip).
    pub(crate) fn hud_viewsize(&self) -> i32 {
        self.client.cvars.integer("cg_viewsize").clamp(1, 100)
    }
    /// cg.renderingThirdPerson.
    pub(crate) fn hud_rendering_third_person(&self) -> bool {
        self.rendering_third_person()
    }
    /// cg.chaseFollow (`CG_DrawFollow`).
    pub(crate) fn hud_chase_follow(&self) -> bool {
        self.cgv.chase_follow
    }
}

impl TremGame {
    /// One server console line: host commands (map loading, rotation, client
    /// cvars), then the game's ConsoleCommand, else a cvar set.
    fn console_line(&mut self, l: &str) -> String {
        let parts: Vec<&str> = l.split_whitespace().collect();
        let lower: Vec<String> = parts.iter().map(|p| p.to_lowercase()).collect();
        let lp: Vec<&str> = lower.iter().map(|s| s.as_str()).collect();
        match lp.as_slice() {
            ["maprotation", _] => {
                let name = parts[1];
                if self.start_map_rotation(GString::from(name)) { String::new() } else { format!("maprotation: invalid map rotation \"{name}\"") }
            }
            ["advancemaprotation"] => {
                // Svcmd_G_AdvanceMapRotation_f
                let Some(ch) = self.advance_rotation() else { return "advanceMapRotation: no rotation active".into() };
                let paths = self.pk3_paths.clone();
                let then = if ch.post_command.is_empty() { Vec::new() } else { vec![ch.post_command.clone()] };
                self.begin_load(&paths, &ch.map, then, false)
            }
            ["stopmaprotation"] => {
                self.rotations.stop();
                String::new()
            }
            ["cg_lightflare", v] => {
                // client cvar: 0 off, 1 nofade, 2 timefade, 3 realfade
                self.mapents.light_flare = v.parse().ok();
                String::new()
            }
            ["cg_lightflare"] => format!("\"cg_lightFlare\" is:\"{}\" default:\"3\"", self.mapents.light_flare.unwrap_or(3)),
            ["cg_viewdebug"] => self.cgv.debug_string(),
            // cg_view.c / cg_weapons.c cvars (view.rs ViewCvars)
            [name, _] if self.cgv.cvars.get(name).is_some() => {
                self.cgv.cvars.set(parts[0], parts[1]);
                String::new()
            }
            [name] if self.cgv.cvars.get(name).is_some() => format!("\"{}\" is:\"{}\"", parts[0], self.cgv.cvars.get(name).unwrap_or_default()),
            ["map", _] | ["devmap", _] => {
                let paths = self.pk3_paths.clone();
                self.begin_load(&paths, parts[1], Vec::new(), lp[0] == "devmap")
            }
            ["map_restart", ..] => {
                self.restart();
                String::new()
            }
            _ => {
                // the engine's network commands (nethost.rs)
                if let Some(out) = self.net_engine_command(l) {
                    return out.trim_end().to_string();
                }
                let handled = self.game_world_mut().is_some_and(|w| {
                    w.real_time = real_time();
                    w.console.clear();
                    svcmds::console_command(w, l)
                });
                if handled {
                    return self.game_world_mut().map(|w| std::mem::take(&mut w.console)).unwrap_or_default();
                }
                self.server_command(l);
                String::new()
            }
        }
    }

    /// Run the game's queued console lines (EXEC_APPEND); the ones it does
    /// not handle (map, map_restart, set ...) go through `console_line`.
    /// Returns true when a host command ran (the World may be a new one).
    fn run_exec(&mut self) -> bool {
        let out = self.run_exec_output();
        for l in out.lines() {
            godot_print!("trem: {l}");
        }
        self.exec_ran
    }

    fn run_exec_output(&mut self) -> String {
        self.exec_ran = false;
        let mut out = String::new();
        for _ in 0..16 {
            let Some(w) = self.game_world_mut() else { break };
            let host = svcmds::run_exec(w);
            out.push_str(&std::mem::take(&mut w.console));
            if host.is_empty() {
                break;
            }
            self.exec_ran = true;
            for line in host {
                let r = self.console_line(&line);
                if !r.is_empty() {
                    out.push_str(&r);
                    out.push('\n');
                }
            }
        }
        out
    }

    /// `trap_Cvar_Set` from the host side (kept across World reloads).
    fn set_host_cvar(&mut self, name: &str, value: &str) {
        self.cvars.retain(|(x, _)| !x.eq_ignore_ascii_case(name));
        self.cvars.push((name.to_string(), value.to_string()));
        if let Some(w) = self.game_world_mut() {
            w.set_cvar(name, value);
        }
        self.update_server_cvars();
    }

    fn take_host_cvar(&mut self, name: &str) -> Option<String> {
        let i = self.cvars.iter().position(|(x, _)| x.eq_ignore_ascii_case(name))?;
        Some(self.cvars.remove(i).1)
    }

    /// `G_InitMapRotations`: maprotation.cfg from the data folders.
    fn init_map_rotations(&mut self) {
        let mut dirs: Vec<String> = self.pk3_paths.iter().filter_map(|p| p.rsplit_once(['/', '\\']).map(|x| x.0.to_string())).collect();
        dirs.push(godot::classes::ProjectSettings::singleton().globalize_path("res://base").to_string());
        dirs.dedup();
        let maps: Vec<String> = crate::assets::Pk3Set::open(&self.pk3_paths).maps();
        let text = dirs.iter().find_map(|d| std::fs::read_to_string(format!("{d}/maprotation.cfg")).ok());
        let mut mr = trem_sim::kernel::maprotation::MapRotations::default();
        match text {
            Some(t) => {
                if !mr.parse(&t, &|m| maps.iter().any(|x| x.eq_ignore_ascii_case(m))) {
                    godot_warn!("trem: failed to parse maprotation.cfg");
                }
            }
            None => godot_print!("trem: maprotation.cfg file not found."),
        }
        for l in mr.log.drain(..) {
            godot_warn!("trem: {l}");
        }
        self.rotations = mr;
        let init = self.initial_map_rotation.to_string();
        if !self.rotations.active() && !init.is_empty() {
            self.rotations.begin(&init);
            self.initial_map_rotation = GString::new();
        }
    }

    /// `G_AdvanceMapRotation` with this World's facts.
    fn advance_rotation(&mut self) -> Option<trem_sim::kernel::maprotation::MapChange> {
        let maps: Vec<String> = crate::assets::Pk3Set::open(&self.pk3_paths).maps();
        let (n, win) = self.game_world().map(|w| (w.clients.iter().filter(|c| c.connected).count() as i32, w.level.last_win)).unwrap_or((0, None));
        let mut seed = self.clock.to_bits() as u32 | 1;
        let mut rnd = move || {
            seed = seed.wrapping_mul(1103515245).wrapping_add(12345);
            ((seed >> 1) & 0x7fff_ffff) as i32
        };
        let exists = |m: &str| maps.iter().any(|x| x.eq_ignore_ascii_case(m));
        let mut ctx = trem_sim::kernel::maprotation::Ctx { num_connected: n, last_win: win, map_exists: &exists, rand: &mut rnd };
        let ch = self.rotations.advance(&mut ctx);
        for l in self.rotations.log.drain(..) {
            godot_warn!("trem: {l}");
        }
        // G_IssueMapChange: the entry's layouts become g_layouts for the next map
        if let Some(c) = ch.as_ref().filter(|c| !c.layouts.trim().is_empty()) {
            self.set_host_cvar("g_layouts", c.layouts.trim());
        }
        ch
    }

    /// Server console text: cvar sets are kept (and applied to this world).
    fn server_command(&mut self, text: &str) {
        for cmd in text.split([';', '\n']).map(str::trim).filter(|c| !c.is_empty()) {
            // Cmd_TokenizeString: "quoted values" stay whole; set takes Cmd_ArgsFrom( 2 )
            let toks = svcmds::tokenize(cmd);
            let kv = match toks.as_slice() {
                [s, n, v @ ..] if !v.is_empty() && matches!(s.to_ascii_lowercase().as_str(), "set" | "seta" | "sets" | "setu") => Some((n.clone(), v.join(" "))),
                [n, v, ..] => Some((n.clone(), v.clone())),
                _ => None,
            };
            if let Some((k_, v)) = kv {
                // sv_hostname / g_motd: serverinfo and CS_MOTD for the loading screen
                let host = matches!(k_.to_ascii_lowercase().as_str(), "sv_hostname" | "g_motd");
                let host = host || self.is_net_server_cvar(&k_);
                let known = host || self.game_world_mut().map(|w| w.set_cvar(&k_, &v)).unwrap_or(false);
                if known {
                    self.cvars.retain(|(x, _)| !x.eq_ignore_ascii_case(&k_));
                    self.cvars.push((k_, v));
                    self.update_server_cvars();
                } else {
                    godot_print!("trem: unhandled server command: {cmd}");
                }
            }
        }
    }

    /// Load maps/<name>.bsp at once (no loading screen frames): map_restart
    /// and the scripting entry point `load_bsp`.
    fn load_bsp_inner(&mut self, paths: &[String], name: &str) -> String {
        let err = self.begin_load(paths, name, Vec::new(), false);
        if err.is_empty() {
            while self.loading.is_some() {
                self.load_step();
            }
        }
        err
    }

    /// `SV_SpawnServer` up to the BSP read: start loading maps/<name>.bsp
    /// across frames (`step_loading`), with the loading screen up. "" or
    /// an error (nothing changes then). `then`: console lines to run when
    /// the map is up; `cheats`: devmap.
    fn begin_load(&mut self, paths: &[String], name: &str, then: Vec<String>, cheats: bool) -> String {
        if self.netext.dedicated {
            return self.dedicated_load(paths, name, then, cheats); // wt-a (dec037)
        }
        let mut set = crate::assets::Pk3Set::open(paths);
        let Some(data) = set.read(&format!("maps/{name}.bsp")) else { return format!("maps/{name}.bsp not found in the pk3 files") };
        let bsp = match trem_sim::kernel::bsp::parse(data.as_slice()) {
            Ok(b) => b,
            Err(e) => return format!("{name}: {e}"),
        };
        // CS_MESSAGE: the worldspawn "message" (SP_worldspawn)
        let message = bsp.entity_list().first().and_then(|e| e.get("message").cloned()).unwrap_or_default();
        let cvar = |n: &str, d: &str| self.cvars.iter().find(|(k_, _)| k_.eq_ignore_ascii_case(n)).map(|(_, v)| v.clone()).unwrap_or_else(|| d.to_string());
        // sv_hostname (sv_init.c default "noname") and g_motd (CS_MOTD)
        let info = LoadInfo { mapname: name.to_string(), message, hostname: cvar("sv_hostname", "noname"), motd: cvar("g_motd", ""), ..Default::default() };
        let mode = if self.wants_listen_server() { LoadMode::Listen } else { LoadMode::Single };
        self.loading = Some(Box::new(MapLoad {
            paths: paths.to_vec(),
            name: name.to_string(),
            set,
            bsp,
            step: 0,
            info,
            world: None,
            sessions: None,
            render: None,
            meshes: None,
            rt_geom: None,
            acoustic: None,
            fogs: Vec::new(),
            brush: HashMap::new(),
            sky: None,
            sky_hd: None,
            grid: None,
            shadow_dirs: None,
            then,
            cheats,
            mode,
        }));
        String::new()
    }

    /// Scripted calls that need the level (commands, console, cheats) finish
    /// a pending load at once.
    fn finish_loading(&mut self) {
        if self.loading.is_some() && std::env::var_os("TREM_LOAD_DEBUG").is_some() {
            godot_print!("trem: finishing the map load at once");
        }
        while self.loading.is_some() {
            self.load_step();
        }
    }

    /// One frame of loading: steps until ~30 ms are used (at least one), so
    /// the loading menu is drawn between them (trap_UpdateScreen).
    fn step_loading(&mut self) {
        let t0 = std::time::Instant::now();
        while self.loading.is_some() {
            self.load_step();
            if t0.elapsed().as_millis() >= 30 {
                break;
            }
        }
    }

    /// The loading screen's facts while a map loads.
    pub(crate) fn hud_loading(&self) -> Option<&LoadInfo> {
        self.loading.as_ref().map(|l| &l.info).or(self.connect_screen.as_ref())
    }

    /// One step of the map load, in `G_InitGame` then `CG_Init` order with
    /// its fractions (`CG_UpdateMediaFraction`: cg.mediaFraction, then the
    /// loading menu draws on the next frame): trails and particles (media 0.05), sounds (0.60), the
    /// world and inline models (`CG_RegisterGraphics`: 0.66 .. 0.9),
    /// weapons (0.95), upgrades (1.0), then each buildable
    /// (`CG_InitBuildables`) and each class model (`CG_RegisterClients`).
    fn load_step(&mut self) {
        let Some(mut ld) = self.loading.take() else { return };
        let nb = BUILDABLES.len();
        let nc = CLASSES.len();
        let step = ld.step;
        ld.step += 1;
        match step {
            0 if ld.mode != LoadMode::Single => {
                // SV_SpawnServer of a listen server (the game World goes to
                // the server; its log and the clients carry over there), or
                // CL_InitCGame for a remote server: no game World here
                if ld.mode == LoadMode::Listen {
                    let layouts = self.take_host_cvar("g_layouts").unwrap_or_default();
                    let auto = self.cvars.iter().find(|(k_, _)| k_.eq_ignore_ascii_case("g_layoutAuto")).is_none_or(|(_, v)| v.trim() != "0");
                    let fs: Box<dyn trem_sim::kernel::world::Fs> = Box::new(crate::hostfs::HostFs::new(&self.pk3_paths));
                    ld.world = Some(World::from_bsp_with(&ld.bsp, &ld.name, Some(fs), Some((&layouts, auto))));
                    if let Some(old) = self.nethost.as_mut().and_then(|n| n.server.as_mut()) {
                        trem_sim::kernel::log::shutdown_game(&mut old.world);
                    }
                }
                self.clear_scene();
                self.world = None;
                self.me = None;
            }
            0 => {
                // a single-process map ends any network session (CL_MapLoading)
                if let Some(nh) = self.nethost.as_mut() {
                    nh.disconnect();
                }
                // SV_SpawnServer / G_InitGame (g_layouts is a one-time-use cvar)
                let layouts = self.take_host_cvar("g_layouts").unwrap_or_default();
                let auto = self.cvars.iter().find(|(k_, _)| k_.eq_ignore_ascii_case("g_layoutAuto")).is_none_or(|(_, v)| v.trim() != "0");
                let fs: Box<dyn trem_sim::kernel::world::Fs> = Box::new(crate::hostfs::HostFs::new(&self.pk3_paths));
                ld.world = Some(World::from_bsp_with(&ld.bsp, &ld.name, Some(fs), Some((&layouts, auto))));
                // G_WriteSessionData of the level being left, then CG_Shutdown
                ld.sessions = self.world.as_ref().map(session::write_session_data);
                // G_ShutdownGame's log lines for the level being left
                if let Some(old) = self.world.as_mut() {
                    trem_sim::kernel::log::shutdown_game(old);
                }
                self.clear_scene();
                self.world = None;
                self.me = None;
            }
            1 => {
                // CG_LoadTrailSystems, CG_LoadParticleSystems
                let parent = self.to_gd().upcast::<Node3D>();
                if let Some(lib) = self.lib.as_mut().filter(|_| std::env::var_os("TREM_NO_FX").is_none()) {
                    self.effects = Some(crate::effects::Effects::new(lib, &parent));
                }
                // R_MarkFragments' world (tr_marks.c) for CG_ImpactMark
                if let Some(fx) = self.effects.as_mut() {
                    fx.set_mark_world(trem_sim::kernel::marks::MarkWorld::from_bsp(&ld.bsp));
                }
                ld.info.media = 0.05;
            }
            2 => {
                // CG_RegisterSounds: the media sounds and the game's CS_SOUNDS
                if let Some(lib) = self.lib.as_mut() {
                    for s in CG_MEDIA_SOUNDS {
                        lib.sound(s);
                    }
                    for i in 1..=4 {
                        for kind in ["step", "flesh", "splash", "clank"] {
                            lib.sound(&format!("sound/player/footsteps/{kind}{i}.wav"));
                        }
                    }
                    for i in 0..4 {
                        lib.sound(&format!("sound/buildables/human/damage{i}.wav"));
                    }
                    if let Some(w) = ld.world.as_ref() {
                        for n in w.sound_names.iter().filter(|n| !n.is_empty() && !n.starts_with('*')) {
                            lib.sound(n);
                        }
                    }
                }
                ld.info.media = 0.60;
            }
            3 => {
                // CG_RegisterGraphics: trap_R_LoadWorldMap
                let MapLoad { bsp, set, .. } = &mut *ld;
                let mut render = crate::bspview::BspRender::new(bsp, set);
                ld.meshes = Some(render.model(&ld.bsp, &mut ld.set, 0));
                ld.rt_geom = Some(std::mem::take(&mut render.rt));
                ld.acoustic = Some(std::mem::take(&mut render.acoustic));
                ld.fogs = std::mem::take(&mut render.fogs);
                ld.render = Some(render);
                ld.info.media = 0.66;
            }
            4 => {
                // the inline models drawn by brush movers (CG_Mover)
                let MapLoad { bsp, set, render, world, brush, mode, .. } = &mut *ld;
                if *mode == LoadMode::Client {
                    // no World yet: every inline model a mover may use
                    if let Some(render) = render.as_mut() {
                        for m in 1..bsp.models.len() as i32 {
                            brush.insert(m, render.model(bsp, set, m as usize));
                        }
                    }
                } else if let (Some(render), Some(w)) = (render.as_mut(), world.as_ref()) {
                    for i in 0..w.num_entities {
                        let e = &w.ents[i];
                        if e.inuse && e.bmodel && e.etype == EType::Mover && e.model_index > 0 && !brush.contains_key(&e.model_index) {
                            brush.insert(e.model_index, render.model(bsp, set, e.model_index as usize));
                        }
                    }
                }
                ld.info.media = 0.8;
            }
            5 => {
                // the sky and the light grid
                ld.sky = crate::bspview::sky_def(&ld.bsp, &mut ld.set);
                ld.sky_hd = crate::bspview::sky_textures_hd(&ld.bsp, &mut ld.set);
                ld.grid = crate::bspview::LightGrid::from_bsp(&ld.bsp);
                // the world-fixed shadow directions (lamps.rs): the grid's, the sky's sun
                let sun = match ld.render.as_ref() {
                    Some(r) => crate::lamps::sun(&ld.bsp, r.shaders()),
                    None => crate::lamps::sun(&ld.bsp, &crate::q3shader::load_all(&mut ld.set)),
                };
                ld.shadow_dirs = ld.grid.as_ref().map(|g| crate::lamps::ShadowDirs::cluster(&g.cells(), sun, crate::lamps::DIRECTIONS));
                ld.render = None;
                ld.info.media = 0.9;
            }
            6 => {
                // `CG_InitWeapons`: weapon.cfg, animation.cfg, models
                if let Some(lib) = self.lib.as_mut() {
                    for wd in WEAPONS.iter().filter(|x| x.num > 0) {
                        if let Some(h) = ViewHand::new(lib, wd.num as i32) {
                            h.free();
                        }
                        let cfg = lib.weapon_cfg(WeaponId::from_num(wd.num).unwrap().id());
                        if let Some(m) = cfg.model.as_deref().and_then(|m| lib.model(m)) {
                            lib.instance(&m, None).free();
                        }
                        // CG_RegisterWeapon's trap_S_RegisterSound: the fire and
                        // impact sounds decoded now, not on the first shot
                        for list in cfg.flash.iter().chain(cfg.impact.iter()).chain(cfg.impact_flesh.iter()) {
                            for s in list {
                                for p in s.split('|') {
                                    if lib.sound(p).is_some() {
                                        break;
                                    }
                                }
                            }
                        }
                    }
                }
                ld.info.media = 0.95;
            }
            7 => {
                // CG_InitUpgrades (the upgrade models are loaded with the clients)
                ld.info.media = 1.0;
            }
            n if n < LOAD_FIXED_STEPS + nb => {
                // CG_InitBuildables: one buildable per step
                let i = n - LOAD_FIXED_STEPS;
                if let (Some(lib), Some(b)) = (self.lib.as_mut(), BuildableId::from_num(BUILDABLES[i].num)) {
                    if let Some(a) = Actor::buildable(lib, b, String::new()) {
                        a.free();
                    }
                }
                ld.info.buildables = (i + 1) as f32 / nb as f32;
            }
            n if n < LOAD_FIXED_STEPS + nb + nc => {
                // CG_RegisterClients: CG_PrecacheClientInfo per class
                let i = n - LOAD_FIXED_STEPS - nb;
                let class = ClassId::from_num(CLASSES[i].num);
                if let (Some(lib), Some(c)) = (self.lib.as_mut(), class) {
                    if let Some(a) = Actor::player(lib, c, c.def().team, &[0; 16], 0, None, String::new(), 0, false) {
                        a.free();
                    }
                }
                ld.info.char_models = (i + 1) as f32 / nc as f32;
            }
            _ => {
                // done: the level starts (cg.infoScreenText cleared)
                let mut ld = ld;
                if ld.mode != LoadMode::Single {
                    self.finish_net_load(*ld);
                    return;
                }
                let w = ld.world.take().expect("world");
                self.current = CurrentMap::Bsp(ld.paths.clone(), ld.name.clone());
                let map = w.map;
                self.gfx.pending_geom = ld.rt_geom.take();
                self.gfx.pending_fogs = std::mem::take(&mut ld.fogs);
                self.set_acoustic(ld.acoustic.take(), &ld.bsp);
                self.populate_world(w, map, ld.meshes.take(), ld.sessions.take());
                self.brush_meshes = std::mem::take(&mut ld.brush);
                self.lightgrid = ld.grid.take();
                self.shadow_dirs = ld.shadow_dirs.take();
                if let Some(sky) = ld.sky.take() {
                    self.set_sky(sky, ld.sky_hd.take());
                }
                if ld.cheats {
                    self.set_cheats(true);
                }
                for l in &ld.then {
                    self.server_command(l);
                }
                return;
            }
        }
        self.loading = Some(ld);
    }

    /// Replace the background colour with the map's Q3 sky (box and clouds).
    fn set_sky(&mut self, def: crate::bspview::SkyDef, hd: Option<Vec<Gd<godot::classes::Texture2D>>>) {
        let mat = crate::bspview::sky_material(&def, hd.as_ref());
        let faces: Vec<Gd<ImageTexture>> = match def.faces.as_ref() {
            Some(f) => f.clone(),
            None => def.clouds.iter().map(|c| c.0.clone()).take(1).collect(),
        };
        // the sky's mean colour: the ray tracer's radiance for rays that leave the map
        let mut sum = [0f32; 3];
        for t in faces.iter() {
            if let Some(img) = t.get_image() {
                let m = crate::gfx::derive_maps(&img, false).mean_linear;
                for k in 0..3 {
                    sum[k] += m[k] / faces.len().max(1) as f32;
                }
            }
        }
        self.gfx.sky = sum;
        let mut sky = godot::classes::Sky::new_gd();
        sky.set_material(&mat);
        let children = self.base().get_children();
        for ch in children.iter_shared() {
            if let Ok(we) = ch.try_cast::<WorldEnvironment>() {
                if let Some(mut env) = we.get_environment() {
                    env.set_background(godot::classes::environment::BgMode::SKY);
                    env.set_sky(&sky);
                }
            }
        }
        self.apply_gfx();
    }

    fn start_map(&mut self, map: MapId) {
        self.lightgrid = None;
        self.shadow_dirs = None;
        self.current = CurrentMap::Sheet(map);
        let mut w = World::new(map);
        w.fs = Some(Box::new(crate::hostfs::HostFs::new(&self.pk3_paths)));
        self.start_world(w, map, None);
        if let Some(fx) = self.effects.as_mut() {
            fx.set_mark_world(trem_sim::kernel::marks::MarkWorld::from_bsp(&trem_sim::kernel::bsp::from_sheet(map)));
        }
    }

    fn start_world(&mut self, w: World, map: MapId, bsp_meshes: Option<Vec<Gd<MeshInstance3D>>>) {
        // G_WriteSessionData of the level being left (g_session.c), G_ShutdownGame's log lines
        let sessions = self.world.as_ref().map(session::write_session_data);
        if let Some(old) = self.world.as_mut() {
            trem_sim::kernel::log::shutdown_game(old);
        }
        self.loading = None;
        self.clear_scene();
        self.populate_world(w, map, bsp_meshes, sessions);
    }

    /// Free the previous level's nodes and presentation state.
    fn clear_scene(&mut self) {
        let children = self.base().get_children();
        for mut ch in children.iter_shared() {
            ch.queue_free();
        }
        self.visuals.clear();
        self.float_sprites.clear();
        self.fx.clear();
        self.camera = None;
        self.view_vp = None;
        self.view_sprite = None;
        self.viewmodel = None;
        self.ghost = None;
        self.ghost_model = None;
        self.cgv.reset();
        self.actors.clear();
        self.free_ragdolls();
        self.death_push.clear();
        self.alive_vel.clear();
        self.missiles.clear();
        self.flashes.clear();
        self.hand = None;
        self.audio = None;
        self.mapents = Default::default();
        self.brush_meshes.clear();
        self.effects = None;
        self.lightgrid = None;
        self.shadow_dirs = None;
    }

    /// Start a level on a new World: effects, audio, server cvars, the
    /// local player and the bots, session data, then the scene.
    fn populate_world(&mut self, mut w: World, map: MapId, bsp_meshes: Option<Vec<Gd<MeshInstance3D>>>, sessions: Option<Vec<Option<String>>>) {
        self.start_presentation();
        let me = self.prepare_game_world(&mut w, sessions, true);
        self.world = Some(w);
        self.me = me;
        self.build_level(map, bsp_meshes);
    }

    /// The level's effects and audio (the cgame side of a new level).
    fn start_presentation(&mut self) {
        let parent = self.to_gd().upcast::<Node3D>();
        if let Some(lib) = self.lib.as_mut().filter(|_| self.effects.is_none() && std::env::var_os("TREM_NO_FX").is_none()) {
            self.effects = Some(crate::effects::Effects::new(lib, &parent));
        }
        if self.lib.is_some() {
            let (audio, nodes) = Audio::new();
            for n in nodes {
                self.base_mut().add_child(&n);
            }
            self.audio = Some(audio);
        }
    }

    /// `G_InitGame` on a new World: server cvars, admin.dat, the game log,
    /// team locks, the local player (single process) and the bots, session
    /// data. Returns the local player's slot.
    fn prepare_game_world(&mut self, w: &mut World, sessions: Option<Vec<Option<String>>>, local_player: bool) -> Option<usize> {
        for (k_, v) in &self.cvars {
            w.set_cvar(k_, v);
        }
        // G_InitGame: G_admin_readconfig (user://admin.dat), g_lockTeamsAtStart
        w.real_time = real_time();
        trem_sim::kernel::log::init_game(w);
        trem_sim::kernel::client::load_emoticons(w);
        admin::readconfig(w, None);
        session::init_game_locks(w);
        // ClientConnect with the userinfo `name` (wt-misc: userinfo.rs)
        let me = if local_player { w.connect(&self.client.cvars.string("name"), false) } else { None };
        self.misc.names.clear();
        self.misc.vote.reset();
        self.misc.dev.reset();
        // port_bots: the createserver menu's Bots Per Team (0: no bots)
        let n = self.net_cvar("port_bots").map(|v| trem_sim::kernel::spawn::c_atoi(&v)).unwrap_or(self.bots_per_team).clamp(0, 16);
        for t in [Team::Aliens, Team::Humans] {
            for k_ in 0..n {
                let name = format!("{}{}", if t == Team::Aliens { "Xeno" } else { "Marine" }, k_ + 1);
                bot::add_bot(w, &name, t);
            }
        }
        // G_ReadSessionData for the slots that were connected on the last level
        if let Some(sess) = sessions {
            for (i, s) in sess.iter().enumerate() {
                if let Some(s) = s.as_deref().filter(|_| w.clients[i].connected) {
                    session::read_session_data(w, i, s);
                }
            }
        }
        for l in std::mem::take(&mut w.console).lines() {
            godot_print!("trem: {l}");
        }
        if let Some(me) = me {
            // cl.viewangles start where the server put us (applied by write_cmd:
            // the client may be out in with_client while a command restarts)
            self.reset_view = Some(w.clients[me].ps.viewangles);
        }
        self.last_spawn = None;
        // G_MapExists for map / nextmap votes
        {
            let set = crate::assets::Pk3Set::open(&self.pk3_paths);
            w.level.map_list = set.maps();
        }
        me
    }

    fn material(&mut self, color: &str, alpha: f32, emission: f32) -> Gd<StandardMaterial3D> {
        let key = format!("{color}/{alpha}/{emission}");
        if let Some(m) = self.mats.get(&key) {
            return m.clone();
        }
        let mut m = StandardMaterial3D::new_gd();
        let mut c = hex_color(color);
        c.a = alpha;
        m.set_albedo(c);
        m.set_roughness(0.7);
        if alpha < 1.0 {
            m.set_transparency(Transparency::ALPHA);
        }
        if emission > 0.0 {
            m.set_feature(Feature::EMISSION, true);
            m.set_emission(hex_color(color));
            m.set_emission_energy_multiplier(emission);
        }
        self.mats.insert(key, m.clone());
        m
    }

    /// A light panel-grid texture so flat brush faces read as surfaces.
    fn grid_texture() -> Option<Gd<ImageTexture>> {
        let n = 128;
        let mut img = Image::create(n, n, false, ImageFormat::RGBA8)?;
        img.fill(Color::from_rgb(1.0, 1.0, 1.0));
        for a in 0..n {
            for b in [0, 1, n / 2] {
                let c = if b == n / 2 { Color::from_rgb(0.88, 0.88, 0.9) } else { Color::from_rgb(0.62, 0.62, 0.66) };
                img.set_pixel(a, b, c);
                img.set_pixel(b, a, c);
            }
        }
        img.generate_mipmaps();
        ImageTexture::create_from_image(&img)
    }

    fn build_level(&mut self, map: MapId, bsp_meshes: Option<Vec<Gd<MeshInstance3D>>>) {
        let grid = Self::grid_texture();
        let md = map.def();
        // environment
        let mut env = Environment::new_gd();
        env.set_background(godot::classes::environment::BgMode::COLOR);
        env.set_bg_color(hex_color(md.sky_color));
        env.set_ambient_source(godot::classes::environment::AmbientSource::COLOR);
        env.set_ambient_light_color(Color::from_rgb(0.8, 0.85, 1.0));
        env.set_ambient_light_energy(md.ambient * 1.5);
        env.set_tonemapper(godot::classes::environment::ToneMapper::LINEAR);
        env.set_glow_enabled(true);
        let mut we = WorldEnvironment::new_alloc();
        we.set_environment(&env);
        // the graphics modes (gfx.rs): the vanilla settings above, and the
        // ray-traced effects where the GPU has hardware ray tracing (Vulkan ray tracing pipelines)
        self.gfx.vanilla = Some(crate::gfx::VanillaEnv::capture(&env));
        self.gfx.env = Some(env.clone());
        self.gfx.is_bsp = bsp_meshes.is_some();
        self.gfx.fake_lights.clear();
        if self.gfx.caps.as_ref().is_some_and(|c| c.backend() != crate::gfx::RtBackend::None) {
            let rt = self.gfx.rt.get_or_insert_with(crate::rt::TremRtEffect::create).clone();
            let mut comp = godot::classes::Compositor::new_gd();
            let mut effects: Array<Gd<godot::classes::CompositorEffect>> = Array::new();
            effects.push(&rt.upcast::<godot::classes::CompositorEffect>());
            comp.set_compositor_effects(&effects);
            we.set_compositor(&comp);
        }
        self.base_mut().add_child(&we);
        let mut sun = DirectionalLight3D::new_alloc();
        sun.set_shadow(true);
        sun.set_param(LightParam::ENERGY, 1.2);
        sun.set_basis(Basis::from_euler(EulerOrder::YXZ, Vector3::new(-1.0, 0.6, 0.0)));
        self.base_mut().add_child(&sun);
        self.gfx.sun = Some(sun.clone());
        // base lights
        let spots = self.world.as_ref().map(|w| w.spots.clone()).unwrap_or_default();
        for me in spots.iter() {
            if matches!(me.buildable, Some(BuildableId::Overmind) | Some(BuildableId::Reactor)) {
                let mut l = OmniLight3D::new_alloc();
                l.set_color(if me.team == Team::Aliens { Color::from_rgb(0.5, 1.0, 0.4) } else { Color::from_rgb(0.5, 0.7, 1.0) });
                l.set_param(LightParam::RANGE, 22.0);
                l.set_param(LightParam::ENERGY, 1.6);
                let mut o = me.origin;
                o[2] += 200.0;
                l.set_position(q2g(&o));
                self.base_mut().add_child(&l);
                self.gfx.fake_lights.push(l);
            }
        }
        // the map's fog brushes as volumetric fog (enhanced mode)
        let mut root: Gd<Node> = self.to_gd().upcast();
        self.gfx.build_fogs(&mut root);

        // BSP world: prebuilt meshes; sheet world: brushes, one surface per material
        let is_bsp = bsp_meshes.is_some();
        for mi in bsp_meshes.unwrap_or_default() {
            self.base_mut().add_child(&mi);
        }
        let mut by_mat: HashMap<&'static str, Vec<usize>> = HashMap::new();
        for (i, b) in BRUSHES.iter().enumerate() {
            if b.map == map && !is_bsp {
                by_mat.entry(b.material).or_default().push(i);
            }
        }
        let mut keys: Vec<_> = by_mat.keys().copied().collect();
        keys.sort();
        for mat in keys {
            let mut st = SurfaceTool::new_gd();
            st.begin(PrimitiveType::TRIANGLES);
            for &bi in &by_mat[mat] {
                for (poly, n) in geom::brush_polygons(&BRUSHES[bi]) {
                    let gn = q2g_dir(&[n[0] as f32, n[1] as f32, n[2] as f32]);
                    let pts: Vec<Vector3> = poly.iter().map(|p| q2g(&[p[0] as f32, p[1] as f32, p[2] as f32])).collect();
                    for t in 1..pts.len() - 1 {
                        for v in [pts[0], pts[t + 1], pts[t]] {
                            st.set_normal(gn);
                            // world-aligned UVs for the checker look
                            st.set_uv(Vector2::new(v.x + v.y * 0.5, v.z + v.y * 0.5));
                            st.add_vertex(v);
                        }
                    }
                }
            }
            let Some(mesh) = st.commit() else { continue };
            let def = MATERIALS.iter().find(|m| m.id == mat);
            let mut m = StandardMaterial3D::new_gd();
            if let Some(d) = def {
                m.set_albedo(hex_color(d.color));
                m.set_metallic(d.metallic);
                m.set_roughness(d.roughness);
                if d.emission > 0.0 {
                    m.set_feature(Feature::EMISSION, true);
                    m.set_emission(hex_color(d.color));
                    m.set_emission_energy_multiplier(d.emission * 4.0);
                }
            }
            m.set_cull_mode(CullMode::DISABLED);
            if let Some(g) = &grid {
                m.set_texture(TextureParam::ALBEDO, g);
                m.set_flag(MatFlags::UV1_USE_TRIPLANAR, true);
                m.set_flag(MatFlags::UV1_USE_WORLD_TRIPLANAR, true);
                m.set_uv1_scale(Vector3::new(0.25, 0.25, 0.25));
                m.set_uv1_triplanar_blend_sharpness(40.0);
            }
            let mut mi = MeshInstance3D::new_alloc();
            mi.set_mesh(&mesh);
            mi.set_material_override(&m);
            mi.set_name(&format!("brushes_{mat}"));
            self.base_mut().add_child(&mi);
        }

        // camera + view model + build ghost
        let mut cam = Camera3D::new_alloc();
        cam.set_fov(90.0);
        cam.set_near(0.03);
        cam.set_current(true);
        let mut vm = MeshInstance3D::new_alloc();
        let mut bm = BoxMesh::new_gd();
        bm.set_size(Vector3::new(0.045, 0.06, 0.3));
        vm.set_mesh(&bm);
        vm.set_position(Vector3::new(0.2, -0.17, -0.42));
        cam.add_child(&vm);
        // the view renders into a SubViewport of the main World3D
        // (CG_CalcVrect / fov_x warps, see sync_view_rect)
        let (vp, sprite) = Self::make_view_viewport(&self.base().get_viewport());
        let mut vp_node = vp.clone();
        vp_node.add_child(&cam);
        self.base_mut().add_child(&vp);
        self.base_mut().add_child(&sprite);
        self.view_vp = Some(vp);
        self.view_sprite = Some(sprite);
        self.vrect = (0, 0, 0, 0);
        self.gfx.view_vp = self.view_vp.clone();
        self.gfx.view_msaa = None;
        let mut ghost = MeshInstance3D::new_alloc();
        ghost.set_mesh(&BoxMesh::new_gd());
        ghost.set_visible(false);
        self.base_mut().add_child(&ghost);
        self.camera = Some(cam);
        self.viewmodel = Some(vm);
        self.ghost = Some(ghost);
        self.apply_gfx();
    }

    /// r_enhanced / r_rtMode / r_rtDebug -> gfx.rs.
    fn apply_gfx(&mut self) {
        // the 4K pack (hdtex.rs): its textures are loaded with each map
        let hd_on = self.client.cvars.integer("r_hdTextures") != 0;
        let hd_path = self.client.cvars.find("r_hdPath").map(|v| v.string.clone()).unwrap_or_default();
        let hd_max = self.client.cvars.integer("r_hdMaxSize").clamp(256, 8192) as usize;
        crate::hdtex::configure(hd_on, &hd_path, hd_max);
        let enhanced = self.client.cvars.integer("r_enhanced") != 0;
        let rt = self.client.cvars.find("r_rtMode").map(|v| v.string.trim().parse::<i32>().unwrap_or(-1)).unwrap_or(-1);
        let debug = self.client.cvars.integer("r_rtDebug");
        let backend = self.client.cvars.integer("r_rtBackend");
        self.gfx.upscale = (self.client.cvars.integer("r_upscale"), self.client.cvars.value("r_upscaleScale"));
        self.gfx.shadow_max = self.client.cvars.integer("r_dlightShadows").clamp(0, 16) as usize;
        self.gfx.model_debug = self.client.cvars.integer("r_modelDebug");
        self.gfx.apply(enhanced, rt, debug, backend);
    }

    /// The FMOD + Steam Audio sound path once per frame: start it (s_fmod),
    /// the map's acoustic geometry, the listener (the view camera), the
    /// options (s_occlusion, s_reverb) and the master volume (s_volume's bus).
    fn fmod_frame(&mut self) {
        let want = self.client.cvars.integer("s_fmod") != 0;
        if !want {
            if self.fmod.take().is_some() {
                godot_print!("trem: FMOD sound path off (s_fmod 0): Godot audio");
            }
            self.fmod_tried = false;
            return;
        }
        if self.fmod.is_none() && !self.fmod_tried {
            self.fmod_tried = true;
            // the project's bin folder, or (an exported game: res:// is its
            // pack) the folder of the executable, where the export puts them
            let mut dir = godot::classes::ProjectSettings::singleton().globalize_path("res://bin").to_string();
            if !std::path::Path::new(&format!("{dir}/trem_audio.dll")).exists() {
                let exe = godot::classes::Os::singleton().get_executable_path().to_string();
                if let Some((d, _)) = exe.rsplit_once(['/', '\\']) {
                    dir = d.to_string();
                }
            }
            let rate = godot::classes::AudioServer::singleton().get_mix_rate() as i32;
            match crate::fmodaudio::FmodAudio::open(&dir, rate) {
                Ok(f) => {
                    godot_print!("trem: FMOD + Steam Audio sound path up ({rate} Hz)");
                    self.fmod = Some(f);
                }
                Err(e) => godot_print!("trem: FMOD sound path unavailable, using Godot audio: {e}"),
            }
        }
        let Some(fm) = self.fmod.as_mut() else { return };
        if let Some(scene) = self.fmod_scene.take() {
            let cache = godot::classes::ProjectSettings::singleton().globalize_path("user://acoustics").to_string();
            let pathing = self.client.cvars.integer("s_pathing") != 0;
            if fm.set_scene(&scene, pathing.then_some(cache.as_str())) {
                godot_print!("trem: acoustic scene: {} triangles", scene.tris.len());
            } else {
                godot_warn!("trem: acoustic scene failed: {}", fm.error());
            }
        }
        if let Some(cam) = self.camera.as_ref().filter(|c| c.is_inside_tree() && !self.fmod_listener_locked) {
            let t = cam.get_global_transform();
            let f = -t.basis.col_c();
            let u = t.basis.col_b();
            fm.listener([t.origin.x, t.origin.y, t.origin.z], [f.x, f.y, f.z], [u.x, u.y, u.z]);
        }
        let master = {
            let a = godot::classes::AudioServer::singleton();
            let db = a.get_bus_volume_db(0);
            if a.is_bus_mute(0) { 0.0 } else { 10f32.powf(db / 20.0) }
        };
        let occ = crate::fmodaudio::occlusion_on(self.client.cvars.integer("s_occlusion"), self.client.cvars.integer("r_enhanced") != 0);
        let rev = self.client.cvars.integer("s_reverb") != 0;
        fm.set_options(occ, rev, master);
        let enhanced = self.client.cvars.integer("r_enhanced") != 0;
        fm.set_enhanced(enhanced);
        let cv = |n: &str| self.client.cvars.integer(n) != 0;
        let thick_on = enhanced && cv("s_wallThickness") && occ;
        fm.set_features(crate::fmodaudio::feature_bits(enhanced, cv("s_hrtf"), cv("s_airAbsorption"), cv("s_doppler"), cv("s_wallThickness"), cv("s_pathing")));
        // sounds follow their entity while they play (S_UpdateEntityPosition)
        let voices = &mut self.fmod_voices;
        if let Some(w) = self.world.as_ref() {
            self.fmod_attached.retain(|&(v, e)| {
                if e >= w.ents.len() || !fm.playing(v) {
                    return false;
                }
                let q = crate::mapents::sound_origin(w, e);
                let g = q2g(&q);
                fm.set_pos(v, [g.x, g.y, g.z]);
                if let Some(x) = voices.iter_mut().find(|x| x.0 == v) {
                    x.1 = q;
                }
                true
            });
        } else {
            self.fmod_attached.clear();
        }
        // wall thickness (enhanced): the solid between each sound and the
        // listener, through the map's brushes, ~10 times a second
        self.fmod_frames = self.fmod_frames.wrapping_add(1);
        voices.retain(|&(v, _)| fm.playing(v));
        if thick_on && self.fmod_frames % 6 == 0 {
            if let (Some(w), Some(cam)) = (self.world.as_ref(), self.camera.as_ref().filter(|c| c.is_inside_tree())) {
                let o = cam.get_global_transform().origin;
                let lis = [o.x / crate::geom::SCALE, -o.z / crate::geom::SCALE, o.y / crate::geom::SCALE];
                for &(v, q) in voices.iter() {
                    let t = solid_thickness(&w.cm, &lis, &q);
                    fm.set_thickness(v, t * crate::geom::SCALE);
                }
            }
        }
        fm.update();
    }

    /// `port_gfx [vanilla|enhanced|rt|pt|auto|off|info]` (F10: toggle).
    fn port_gfx(&mut self, arg: &str) {
        match arg.to_ascii_lowercase().as_str() {
            "" | "toggle" => {
                let on = self.client.cvars.integer("r_enhanced") != 0;
                self.client.cvars.set("r_enhanced", if on { "0" } else { "1" });
            }
            "vanilla" | "0" => {
                self.client.cvars.set("r_enhanced", "0");
            }
            "enhanced" | "1" => {
                self.client.cvars.set("r_enhanced", "1");
            }
            "rt" | "raytrace" | "raytracing" => {
                self.client.cvars.set("r_enhanced", "1");
                self.client.cvars.set("r_rtMode", "1");
            }
            "pt" | "pathtrace" | "pathtracing" => {
                self.client.cvars.set("r_enhanced", "1");
                self.client.cvars.set("r_rtMode", "2");
            }
            "auto" => {
                self.client.cvars.set("r_rtMode", "-1");
            }
            "off" | "nort" => {
                self.client.cvars.set("r_rtMode", "0");
            }
            "software" | "sw" => {
                self.client.cvars.set("r_rtBackend", "1");
            }
            "hardware" | "hw" => {
                self.client.cvars.set("r_rtBackend", "0");
            }
            a if a.starts_with("debug") => {
                // the ray tracer's terms alone: 1 AO, 2 indirect light, 3 reflections
                let n = a.trim_start_matches("debug").trim().parse::<i32>().unwrap_or(0);
                self.client.cvars.set("r_rtDebug", &n.to_string());
            }
            _ => {}
        }
        self.apply_gfx();
        let msg = self.gfx.describe();
        godot_print!("{}", msg.trim_end());
        self.misc.con.print(&msg);
    }

    /// Run `f` on the client with this node as its `ClientHost`.
    fn with_client<R>(&mut self, f: impl FnOnce(&mut crate::clientcmd::Client, &mut TremGame) -> R) -> R {
        let mut c = std::mem::take(&mut self.client);
        let r = f(&mut c, self);
        self.client = c;
        r
    }

    /// `port_vision` (G): the enhanced mode's flashlight (humans, on the
    /// gun) or night vision (aliens). Client-side only: nothing is sent to
    /// the server (others do not see it); vanilla has neither.
    fn port_vision(&mut self) {
        // (a console command runs while the client is lent out by
        // with_client: its cvars are not here, so the frame does it)
        self.vision_toggle = true;
    }

    fn toggle_vision(&mut self) {
        if self.client.cvars.integer("r_enhanced") == 0 {
            self.misc.con.print("flashlight / night vision: enhanced graphics only (F10)\n");
            return;
        }
        self.vision_on = !self.vision_on;
        let alien = self.world.as_ref().zip(self.me).is_some_and(|(w, me)| w.clients[me].ps.stat(Stat::Team) == Team::Aliens as i32);
        let what = if alien { "night vision" } else { "flashlight" };
        let msg = format!("{what} {}\n", if self.vision_on { "on" } else { "off" });
        godot_print!("{}", msg.trim_end());
        self.misc.con.print(&msg);
    }

    /// The flashlight follows the view (from the gun, a little right of and
    /// below the eye); night vision draws over the 3D view (not the HUD).
    fn sync_vision(&mut self) {
        use godot::classes::{ColorRect, Shader, ShaderMaterial, SpotLight3D};
        if std::mem::take(&mut self.vision_toggle) {
            self.toggle_vision();
        }
        let enhanced = self.client.cvars.integer("r_enhanced") != 0;
        let (mut torch, mut nv) = (false, false);
        let mut pose = None;
        if let (true, true, Some(w), Some(me)) = (enhanced, self.vision_on, self.world.as_ref(), self.me) {
            let ps = &w.clients[me].ps;
            let alive = ps.stat(Stat::Health) > 0 && ps.pm_type == trem_sim::gen::enums::PmType::Normal as i32;
            let team = ps.stat(Stat::Team);
            torch = alive && team == Team::Humans as i32;
            nv = alive && team == Team::Aliens as i32;
            let (org, ang) = self.cam_q;
            let (f, r, u) = angle_vectors(&ang);
            let mut p = org;
            for k in 0..3 {
                p[k] += f[k] * 10.0 + r[k] * 7.0 - u[k] * 6.0;
            }
            pose = Some(Transform3D::new(basis_from_angles(&ang), q2g(&p)));
        }
        if torch && self.flashlight.as_ref().is_none_or(|l| !l.is_instance_valid()) {
            let mut l = SpotLight3D::new_alloc();
            l.set_name("Flashlight");
            l.set_color(Color::from_rgb(1.0, 0.95, 0.85));
            // (the baked light is already in the surfaces: a torch has to be
            // strong to show over it, as one does in a lit corridor)
            l.set_param(LightParam::ENERGY, 28.0);
            l.set_param(LightParam::RANGE, 32.0);
            l.set_param(LightParam::SPOT_ANGLE, 26.0);
            l.set_param(LightParam::SPOT_ATTENUATION, 0.5);
            l.set_param(LightParam::ATTENUATION, 1.0);
            l.set_shadow(true);
            // our own (shadow-only) body and gun stay out of its shadows
            l.set("shadow_caster_mask", &(crate::gfx::SHADOW_CASTER_LAYER as i64).to_variant());
            self.base_mut().add_child(&l);
            self.flashlight = Some(l);
        }
        if let Some(l) = self.flashlight.as_mut().filter(|l| l.is_instance_valid()) {
            l.set_visible(torch);
            if let (true, Some(t)) = (torch, pose) {
                l.set_transform(t);
            }
        }
        if nv && self.night_vision.as_ref().is_none_or(|n| !n.is_instance_valid()) {
            if let Some(mut vp) = self.view_vp.clone() {
                let mut sh = Shader::new_gd();
                sh.set_code(NIGHT_VISION_SHADER);
                let mut m = ShaderMaterial::new_gd();
                m.set_shader(&sh);
                let mut r = ColorRect::new_alloc();
                r.set_name("NightVision");
                r.set_material(&m);
                r.set_mouse_filter(godot::classes::control::MouseFilter::IGNORE);
                r.set_anchors_preset(godot::classes::control::LayoutPreset::FULL_RECT);
                vp.add_child(&r);
                self.night_vision = Some(r);
            }
        }
        if let Some(n) = self.night_vision.as_mut().filter(|n| n.is_instance_valid()) {
            n.set_visible(nv);
            if nv {
                let size = self.view_vp.as_ref().map(|v| v.get_size()).unwrap_or_default();
                n.set_position(Vector2::ZERO);
                n.set_size(Vector2::new(size.x as f32, size.y as f32));
            }
        }
    }

    /// `CL_KeyEvent` (key repeats are filtered by the caller).
    fn key_event_num(&mut self, key: i32, down: bool) {
        // G toggles the enhanced flashlight / night vision unless bound
        if key == 'g' as i32 && down {
            let b = self.client.keys.get_binding(key).to_string();
            if b.is_empty() {
                self.port_vision();
                return;
            }
        }
        // F10 switches the graphics mode (like Halo: CE Anniversary's
        // switch) unless the player bound it to something else
        if key == crate::keys::K_F1 + 9 && down {
            let b = self.client.keys.get_binding(key).to_string();
            if b.is_empty() || b.eq_ignore_ascii_case("port_gfx") {
                self.port_gfx("");
                return;
            }
        }
        self.sync_catcher();
        let time = godot::classes::Time::singleton().get_ticks_msec() as u32;
        self.with_client(|c, g| c.key_event(key, down, time, g));
        self.misc_print_output(); // wt-misc: and the console
    }

    /// `Key_SetCatcher` from the HUD's state (a menu or the console open).
    fn sync_catcher(&mut self) {
        let catcher = if self.input_enabled { 0 } else { crate::keys::KEYCATCH_UI };
        if catcher != self.client.catcher {
            self.with_client(|c, g| c.set_catcher(catcher, g));
        }
    }

    /// The client's part of a frame: the catcher, the command buffer (what
    /// a `wait` held), cg_thirdPerson, `CG_DrawItemSelect`'s selection
    /// check, and `Com_WriteConfiguration` to user://autogen.cfg.
    fn client_frame(&mut self) {
        self.sync_catcher();
        if !self.client.cbuf.is_empty() {
            self.with_client(|c, g| c.execute(g));
        }
        self.misc_client_frame(); // wt-misc: CL_CheckUserinfo, gamepad, console text
        // the graphics mode cvars (gfx.rs)
        let mut gfx_changed = false;
        for n in ["r_enhanced", "r_rtMode", "r_rtDebug", "r_rtBackend", "r_hdTextures", "r_hdPath", "r_hdMaxSize", "r_upscale", "r_upscaleScale", "r_dlightShadows", "r_modelDebug"] {
            gfx_changed |= self.client.cvars.take_modified(n);
        }
        if gfx_changed {
            self.apply_gfx();
        }
        // cg_thirdPerson <-> the third_person property
        let tp = self.client.cvars.integer("cg_thirdPerson") != 0;
        if self.client.cvars.take_modified("cg_thirdPerson") {
            self.third_person = tp;
        } else if tp != self.third_person {
            self.client.cvars.set("cg_thirdPerson", if self.third_person { "1" } else { "0" });
            self.client.cvars.take_modified("cg_thirdPerson");
        }
        if let (Some(w), Some(me)) = (self.world.as_ref(), self.me) {
            let ps = w.clients[me].ps;
            self.client.wsel.validate(&ps, w.level.time);
        }
        if self.client.config_modified() {
            let text = self.client.write_config();
            let path = godot::classes::ProjectSettings::singleton().globalize_path(&format!("user://{}", crate::clientcmd::CONFIG_CFG)).to_string();
            if std::fs::write(&path, text).is_err() {
                godot_warn!("trem: Couldn't write {path}.");
            }
        }
    }

    /// `CL_CreateCmd` for this frame (`CL_FinishMove`: cg.weaponSelect and
    /// cg.zoomSensitivity from the cgame). Button presses that started and
    /// ended in frames without a tick are carried into the next cmd.
    fn write_cmd(&mut self) {
        let (Some(w), Some(me)) = (self.world.as_ref(), self.me) else { return };
        let server_time = w.level.time;
        let t = godot::classes::Time::singleton().get_ticks_msec() as u32;
        let zoom = self.view_state_zoom_sensitivity();
        if let Some(v) = self.reset_view.take() {
            self.client.input.viewangles = v;
        }
        let mut cmd = self.client.create_cmd(t, zoom, server_time);
        self.held_buttons = self.client.input.held_buttons(cmd.buttons);
        cmd.buttons |= self.pending_buttons;
        self.pending_buttons = cmd.buttons & !self.held_buttons;
        if let Some(w) = self.world.as_mut() {
            w.clients[me].cmd = cmd;
        }
    }

    /// After a tick consumed the cmd: later ticks of the frame send only
    /// the buttons still held.
    fn after_tick(&mut self) {
        self.pending_buttons = 0;
        if let (Some(w), Some(me)) = (self.world.as_mut(), self.me) {
            w.clients[me].cmd.buttons = self.held_buttons;
        }
    }

    /// A client command for the server (`CL_AddReliableCommand` + the
    /// game's `ClientCommand`): "" or the error text.
    fn client_command_line(&mut self, line: &str) -> String {
        if self.net_active() {
            // CL_ForwardCommandToServer -> CL_AddReliableCommand
            let Some(nh) = self.nethost.as_mut() else { return "no game".into() };
            if nh.cl.state != trem_sim::kernel::cl::ConnState::Active || line.starts_with('+') {
                return format!("Unknown command \"{}\"", line.split_whitespace().next().unwrap_or(""));
            }
            return nh.cl.add_reliable_command(line, false).err().unwrap_or_default();
        }
        let (Some(w), Some(me)) = (self.world.as_mut(), self.me) else { return "no game".into() };
        w.real_time = real_time();
        let r = client::command(w, me, line);
        // admin commands (changemap, restart, custom [command]s) queue console lines
        self.run_exec();
        r.err().unwrap_or_default()
    }

    /// The server console takes this command word (svcmds, map / rotation
    /// commands, server and cgame view cvars).
    fn is_server_word(&self, word: &str) -> bool {
        let l = word.to_lowercase();
        if trem_sim::gen::client_commands::by_id(&l).is_some() {
            return false;
        }
        svcmds::is_svcmd(word)
            || l.starts_with("g_")
            || l == "cg_viewdebug"
            || self.cgv.cvars.get(&l).is_some()
            || matches!(l.as_str(), "set" | "seta" | "map" | "devmap" | "map_restart" | "maprotation" | "advancemaprotation" | "stopmaprotation" | "timelimit" | "cg_lightflare")
    }

    /// The client's cvars (`trap_Cvar_*` of the ui).
    pub(crate) fn client_cvars(&mut self) -> &mut crate::cmd::Cvars {
        &mut self.client.cvars
    }

    pub(crate) fn client_cvars_ref(&self) -> &crate::cmd::Cvars {
        &self.client.cvars
    }

    /// The ui's `trap_Cvar_Set`: the client cvar, and the server's for a
    /// server cvar (g_*, timelimit, sv_*: kept for the next map too).
    pub(crate) fn ui_set_cvar(&mut self, name: &str, value: &str) {
        self.client.cvars.set(name, value);
        if self.netext_ui_cvar(name, value) {
            return; // wt-a (dec037): com_downloadPrompt
        }
        let l = name.to_ascii_lowercase();
        if l.starts_with("g_") || l.starts_with("sv_") || l == "timelimit" {
            self.set_host_cvar(name, value);
        } else if self.is_server_word(name) {
            let r = self.console_line(&format!("{name} {value}"));
            if !r.is_empty() {
                godot_print!("{r}");
            }
        }
    }

    /// main.tscn is the running scene (not instanced by a test scene), or
    /// `--main-menu` was given.
    pub(crate) fn main_menu_scene(&self) -> bool {
        let args = godot::classes::Os::singleton().get_cmdline_user_args();
        // TREM_WINDOW_CVARS: a test scene that wants the video cvars applied
        if args.as_slice().iter().any(|a| a.to_string() == "--main-menu") || std::env::var_os("TREM_WINDOW_CVARS").is_some() {
            return true;
        }
        let tree = self.base().get_tree();
        let Some(cur) = tree.get_current_scene() else { return false };
        let owner = self.base().get_owner();
        owner.is_some_and(|o| o == cur)
    }

    /// The bind table (`trap_Key_GetBindingBuf` for cgame and ui).
    pub(crate) fn hud_bindings(&self) -> Vec<String> {
        self.client.keys.bindings().to_vec()
    }

    /// cg.weaponSelect, cg.weaponSelectTime.
    pub(crate) fn hud_weapon_select(&self) -> (i32, i32) {
        (self.client.wsel.select, self.client.wsel.time)
    }

    /// `trap_Key_SetBinding` (ui `Controls_SetConfig`).
    pub(crate) fn set_binding(&mut self, key: i32, command: &str) {
        self.client.keys.set_binding(key, command);
    }

    /// `CG_EntityEvent` sounds (cg_event.c), for the events this port emits.
    fn event_sound(&mut self, e: &GameEvent) {
        if self.lib.is_none() {
            return;
        }
        let r = (self.clock * 997.0) as usize % 4 + 1; // rand() & 3
        let ent = e.entity;
        let foot = ent.try_into().ok().and_then(|i: usize| self.actors.get(&i)).map(|a| a.footsteps().to_string()).unwrap_or_else(|| "normal".into());
        let model_dir = ent.try_into().ok().and_then(|i: usize| self.actors.get(&i)).map(|a| a.model_dir.clone()).unwrap_or_default();
        let wname = WeaponId::from_num(e.weapon as u8).filter(|_| e.weapon > 0).map(|w| w.id().to_string());
        let wcfg = match (&wname, self.lib.as_mut()) {
            (Some(n), Some(l)) => Some(l.weapon_cfg(n)),
            _ => None,
        };
        let pick = |v: &Vec<String>| -> Option<String> { if v.is_empty() { None } else { Some(v[r % v.len()].clone()) } };
        let path: Option<String> = match e.event {
            Event::Footstep => match foot.as_str() {
                "none" => None,
                "flesh" => Some(format!("sound/player/footsteps/flesh{r}.wav")),
                "custom" => Some(format!("sound/player/{model_dir}/step{r}.wav|sound/player/footsteps/step{r}.wav")),
                _ => Some(format!("sound/player/footsteps/step{r}.wav")),
            },
            Event::FootstepMetal => match foot.as_str() {
                "none" => None,
                "custom" => Some(format!("sound/player/{model_dir}/clank{r}.wav|sound/player/footsteps/clank{r}.wav")),
                _ => Some(format!("sound/player/footsteps/clank{r}.wav")),
            },
            Event::FootstepSquelch => Some(format!("sound/player/footsteps/flesh{r}.wav")),
            Event::Footsplash | Event::Footwade | Event::Swim => Some(format!("sound/player/footsteps/splash{r}.wav")),
            Event::FallShort => Some("sound/player/land1.wav".into()),
            Event::FallMedium => Some(self.custom_sound(ent, "pain100_1.wav")),
            Event::FallFar => Some(self.custom_sound(ent, "fall1.wav")),
            Event::Jump => Some(self.custom_sound(ent, "jump1.wav")),
            Event::Taunt => Some(self.custom_sound(ent, "taunt.wav")),
            Event::Pain => {
                // local player sounds are triggered in CG_CheckLocalSounds,
                // so ignore events on the player
                let own = self.me.zip(self.world.as_ref()).map(|(m, w)| w.clients[m].ps.client_num) == Some(ent);
                self.cgv.time = e.time;
                let snd = if own { None } else { self.cgv.pain_event(ent, e.parm) };
                snd.map(|s| self.custom_sound(ent, &s))
            }
            Event::Death1 | Event::Death2 | Event::Death3 => Some(self.custom_sound(ent, &format!("death{}.wav", e.event as i32 - Event::Death1 as i32 + 1))),
            Event::Noammo => Some("sound/weapons/click.wav".into()),
            Event::ChangeWeapon => Some("sound/weapons/change.wav".into()),
            Event::FireWeapon | Event::FireWeapon2 | Event::FireWeapon3 => {
                let m = e.event as usize - Event::FireWeapon as usize;
                wcfg.as_ref().and_then(|c| pick(&c.flash[m]).or_else(|| pick(&c.flash[0])))
            }
            Event::MissileHit | Event::BulletHitFlesh => wcfg.as_ref().and_then(|c| pick(&c.impact_flesh[0]).or_else(|| pick(&c.impact[0]))),
            Event::MissileMiss | Event::MissileMissMetal | Event::BulletHitWall => wcfg.as_ref().and_then(|c| pick(&c.impact[0])),
            Event::GrenadeBounce => Some(format!("sound/misc/hard_bounce{}.wav", r % 2 + 1)),
            Event::Lev1Grab => Some("sound/player/level1/grab.wav".into()),
            Event::Lev4TramplePrepare => Some("sound/player/level4/charge_prepare.wav".into()),
            Event::Lev4TrampleStart => Some("sound/player/level4/charge_start.wav".into()),
            Event::RptuseSound => Some("sound/buildables/repeater/use.wav".into()),
            Event::HumanBuildableExplosion => Some("sound/buildables/human/explosion.wav".into()),
            Event::AlienBuildableExplosion => Some("sound/buildables/alien/explosion.wav".into()),
            Event::BuildRepair => Some("sound/buildables/human/repair.wav".into()),
            Event::BuildRepaired => Some("sound/buildables/human/repaired.wav".into()),
            Event::MgturretSpinup => Some("sound/buildables/mgturret/spinup.wav".into()),
            Event::AlienEvolve => Some("sound/player/alienevolve.wav".into()),
            Event::MedkitUsed => Some("sound/upgrades/medkit/medkit.wav".into()),
            Event::GeneralSound | Event::GlobalSound => {
                let w = self.world.as_ref().unwrap();
                let name = w.sound_name(e.parm).map(|s| s.to_string());
                let origin = if ent >= 0 && (ent as usize) < w.ents.len() { crate::mapents::sound_origin(w, ent as usize) } else { e.origin };
                let path = name.map(|n| match n.strip_prefix('*') {
                    Some(c) => self.custom_sound(ent, c),
                    None => n,
                });
                if let Some(p) = path {
                    self.play_sound(&p, &origin, e.event == Event::GlobalSound);
                }
                None
            }
            Event::OvermindAttack | Event::OvermindDying | Event::OvermindSpawns => {
                // aliens-only announcements
                let mine = self.me.and_then(|m| self.world.as_ref().map(|w| w.clients[m].team == Team::Aliens)).unwrap_or(false);
                if mine {
                    let n = match e.event {
                        Event::OvermindAttack => "overmindattack",
                        Event::OvermindDying => "overminddying",
                        _ => "overmindspawns",
                    };
                    let p = format!("sound/announcements/{n}.wav");
                    self.play_sound(&p, &e.origin, true);
                }
                None
            }
            _ => None,
        };
        if let Some(p) = path {
            use crate::fmodaudio::OwnSound;
            let mine = self.me.zip(self.world.as_ref()).map(|(m, w)| w.clients[m].ps.client_num) == Some(ent);
            let kind = match e.event {
                Event::Footstep | Event::FootstepMetal | Event::FootstepSquelch | Event::Footsplash | Event::Footwade | Event::Swim | Event::FallShort => OwnSound::Feet,
                Event::FireWeapon | Event::FireWeapon2 | Event::FireWeapon3 | Event::Noammo | Event::ChangeWeapon => OwnSound::Weapon,
                Event::Taunt | Event::Jump | Event::FallMedium | Event::FallFar | Event::Pain | Event::Death1 | Event::Death2 | Event::Death3 => OwnSound::Voice,
                _ => OwnSound::Body,
            };
            let follow = usize::try_from(ent).ok().filter(|&i| self.world.as_ref().is_some_and(|w| i < w.ents.len()));
            let big = matches!(
                e.event,
                Event::FireWeapon | Event::FireWeapon2 | Event::FireWeapon3 | Event::MissileHit | Event::MissileMiss | Event::MissileMissMetal | Event::BulletHitWall | Event::BulletHitFlesh | Event::HumanBuildableExplosion | Event::AlienBuildableExplosion
            );
            self.play_sound_big(&p, &e.origin, false, mine.then_some(kind), follow, big);
        }
    }

    /// Weapon flash dlight (weapon.cfg flashDlightColor) at the shooter's muzzle.
    fn muzzle_flash(&mut self, e: &GameEvent) {
        let Some(lib) = self.lib.as_mut() else { return };
        let Some(wid) = WeaponId::from_num(e.weapon as u8).filter(|_| e.weapon > 0) else { return };
        let mode = e.event as usize - Event::FireWeapon as usize;
        let cfg = lib.weapon_cfg(wid.id());
        let Some(c) = cfg.flash_dlight[mode].or(cfg.flash_dlight[0]) else { return };
        let Some(w) = self.world.as_ref() else { return };
        let i = e.entity as usize;
        if i >= MAX_CLIENTS {
            return;
        }
        let ps = &w.clients[i].ps;
        let (f, _, _) = angle_vectors(&ps.viewangles);
        let p = ma(&bg::client_view_origin(ps), 24.0, &f);
        let mut l = godot::classes::OmniLight3D::new_alloc();
        l.set_color(Color::from_rgb(c[0], c[1], c[2]));
        l.set_param(LightParam::RANGE, 9.0);
        l.set_param(LightParam::ENERGY, 3.0);
        // enhanced: the flash lights the lit world; gfx.rs's shadow budget
        // decides whether it casts shadows
        l.set_shadow(false);
        l.add_to_group(crate::gfx::DLIGHT_GROUP);
        l.set_position(q2g(&p));
        self.base_mut().add_child(&l);
        self.flashes.push((l, 0.06));
    }

    fn on_event(&mut self, e: &GameEvent) {
        self.misc_event(e); // wt-misc: cg_debugEvents
        self.event_sound(e);
        if let (Some(w), Some(me)) = (self.world.as_ref(), self.me) {
            let ps = w.clients[me].ps;
            self.cgv.time = e.time;
            self.cgv.entity_event(e, &ps, ps.pm_flags & k::PMF_FOLLOW != 0);
        }
        let now = (self.clock * 1000.0) as i32;
        if let (Some(fx), Some(w), Some(lib)) = (self.effects.as_mut(), self.world.as_ref(), self.lib.as_mut()) {
            fx.on_event(w, lib, e, now);
        }
        let have_fx = self.effects.is_some();
        if matches!(e.event, Event::FireWeapon | Event::FireWeapon2 | Event::FireWeapon3) && e.entity >= 0 {
            self.muzzle_flash(e);
        }
        let me = self.me.map(|m| m as i32).unwrap_or(-1);
        match e.event {
            Event::Obituary => {
                let w = self.world.as_ref().unwrap();
                let victim = e.weapon as usize;
                let attacker = e.other;
                let vname = w.clients.get(victim).map(|c| c.name.clone()).unwrap_or_default();
                let aname = if attacker >= 0 && (attacker as usize) < MAX_CLIENTS && w.ents[attacker as usize].client {
                    w.clients[attacker as usize].name.clone()
                } else if let Some(b) = w.ents.get(attacker.max(0) as usize).and_then(|x| x.buildable) {
                    format!("{} {}", if b.def().team == Team::Aliens { "Alien" } else { "Human" }, b.cold().display_name)
                } else {
                    "the world".into()
                };
                // the ragdoll's push (enhanced): along attacker -> victim, by weapon
                if let Some(speed) = MeansOfDeath::from_raw(e.parm as u8).and_then(crate::ragdoll::death_push) {
                    if victim < MAX_CLIENTS {
                        let vo = w.clients[victim].ps.origin;
                        let ao = if attacker >= 0 && (attacker as usize) < w.ents.len() && attacker as usize != victim {
                            if (attacker as usize) < MAX_CLIENTS { Some(w.clients[attacker as usize].ps.origin) } else { Some(w.ents[attacker as usize].origin) }
                        } else {
                            None
                        };
                        let dir = ao.map(|a| q2g(&vo) - q2g(&a)).filter(|d| d.length() > 0.01).map(|d| Vector3::new(d.x, 0.0, d.z).normalized()).unwrap_or(Vector3::ZERO);
                        let push = (dir + Vector3::new(0.0, 0.35, 0.0)) * speed;
                        self.death_push.insert(victim, (push, self.clock));
                        for r in self.ragdolls.iter_mut().filter(|r| r.ent == victim && !r.corpse) {
                            r.push_late(push, self.clock);
                        }
                    }
                }
                let w = self.world.as_ref().unwrap();
                let how = MeansOfDeath::from_raw(e.parm as u8).map(|m| m.name()).unwrap_or("?");
                let line = if attacker == victim as i32 { format!("{vname} killed themself ({how})") } else { format!("{aname} killed {vname} ({how})") };
                self.feed.push((self.clock, line));
                if self.feed.len() > 6 {
                    self.feed.remove(0);
                    self.feed_dropped += 1;
                }
            }
            Event::MissileHit | Event::BulletHitFlesh if !have_fx => self.spawn_fx(e.origin, "#c02020", 4.0, 0.25, 2.0),
            Event::MissileMiss | Event::MissileMissMetal | Event::BulletHitWall if !have_fx => self.spawn_fx(e.origin, "#ffd070", 3.0, 0.2, 2.0),
            Event::Shotgun if !have_fx => self.spawn_fx(e.origin, "#ffd070", 6.0, 0.15, 1.0),
            Event::HumanBuildableExplosion | Event::AlienBuildableExplosion if !have_fx => self.spawn_fx(e.origin, if e.event == Event::AlienBuildableExplosion { "#60ff40" } else { "#ff9030" }, 40.0, 0.8, 4.0),
            Event::AlienAcidtube if !have_fx => self.spawn_fx(e.origin, "#80ff20", 60.0, 0.6, 2.0),
            // the port's pulse at the overmind (cg_event.c only plays the
            // announcement): enhanced mode only; in the classic (vanilla)
            // mode its lit, emissive sphere showed as purple and green
            // lights (the base light over the overmind) and a glow
            Event::OvermindAttack if self.gfx.enhanced => self.spawn_fx(e.origin, "#b040ff", 30.0, 0.4, 3.0),
            Event::Teslatrail | Event::Lev2Zap if !have_fx => self.spawn_fx(e.origin, "#80c0ff", 10.0, 0.25, 2.0),
            Event::Pain if e.entity == me => {}
            Event::BuildDelay if e.entity == me => {
                self.last_build_attempt = e.time.max(1);
                self.feed.push((self.clock, "Build timer still running".into()));
            }
            Event::AlienEvolveFailed if e.entity == me => self.last_evolve_attempt = e.time.max(1),
            _ => {}
        }
    }

    /// Per-frame particle / trail work (CG_AddParticles, CG_AddTrails).
    fn update_effects(&mut self) {
        let now = (self.clock * 1000.0) as i32;
        let me = self.me;
        let first_person = !self.rendering_third_person() && self.view_ent() == me;
        let (eye, angles) = self.cam_q;
        let TremGame { effects, world, lib, actors, hand, .. } = self;
        let (Some(fx), Some(w), Some(lib)) = (effects.as_mut(), world.as_ref(), lib.as_mut()) else { return };
        fx.sync(w, lib, me, now);
        fx.update(w, lib, actors, hand.as_ref(), me, first_person, now, eye, angles);
    }

    fn spawn_fx(&mut self, origin: [f32; 3], color: &str, size: f32, life: f64, grow: f32) {
        if self.fx.len() > 120 {
            return;
        }
        let mat = self.material(color, 0.8, 3.0);
        let mut s = SphereMesh::new_gd();
        s.set_radius(size * SCALE);
        s.set_height(size * SCALE * 2.0);
        s.set_radial_segments(8);
        s.set_rings(4);
        let mut mi = MeshInstance3D::new_alloc();
        mi.set_mesh(&s);
        mi.set_material_override(&mat);
        mi.set_position(q2g(&origin));
        self.base_mut().add_child(&mi);
        self.fx.push(Fx { node: mi, life, max: life, grow });
    }

    fn update_fx(&mut self, delta: f64) {
        for f in self.flashes.iter_mut() {
            f.1 -= delta;
        }
        self.flashes.retain_mut(|f| {
            if f.1 <= 0.0 {
                f.0.queue_free();
                false
            } else {
                true
            }
        });
        for f in self.fx.iter_mut() {
            f.life -= delta;
            let t = 1.0 - (f.life / f.max).max(0.0) as f32;
            let s = 1.0 + t * f.grow;
            f.node.set_scale(Vector3::new(s, s, s));
        }
        self.fx.retain_mut(|f| {
            if f.life <= 0.0 {
                f.node.queue_free();
                false
            } else {
                true
            }
        });
    }

    fn visual_key(w: &World, i: usize) -> Option<String> {
        let e = &w.ents[i];
        if !e.inuse || !e.linked || e.free_after_event {
            return None;
        }
        Some(match e.etype {
            EType::Player => {
                let c = &w.clients[i];
                format!("p{}:{}:{}", c.ps.stats[Stat::Class as usize], c.ps.stats[Stat::Health as usize] > 0, c.ps.weapon)
            }
            EType::Buildable => format!("b{:?}:{}:{}", e.buildable, e.spawned, e.health > 0),
            EType::Missile => format!("m{:?}", e.missile),
            EType::Corpse => format!("c{}", e.generic1),
            _ => return None,
        })
    }

    fn make_visual(&mut self, i: usize) -> Gd<Node3D> {
        let w = self.world.as_ref().unwrap();
        let e = w.ents[i].clone();
        let mut root = Node3D::new_alloc();
        let size = Vector3::new((e.maxs[0] - e.mins[0]) * SCALE, (e.maxs[2] - e.mins[2]) * SCALE, (e.maxs[1] - e.mins[1]) * SCALE);
        match e.etype {
            EType::Player => {
                let c = w.clients[i].clone();
                let class = bg::class_id(c.ps.stats[Stat::Class as usize]);
                let alien = c.team == Team::Aliens;
                let dead = c.ps.stats[Stat::Health as usize] <= 0;
                let (col, accent) = if alien {
                    (["#5f8f3a", "#6faa48", "#7cbf55", "#8fae3f", "#9fbf4f", "#7b6fb0", "#8a7fc0", "#b05a3a", "#c06a4a", "#7a3a8a"][class.num().saturating_sub(1) as usize % 10], "#e8ff60")
                } else if class == ClassId::HumanBsuit {
                    ("#5a6a7a", "#80c0ff")
                } else {
                    ("#3a5f9a", "#c0e0ff")
                };
                let body_mat = self.material(col, 1.0, 0.0);
                let accent_mat = self.material(accent, 1.0, 2.0);
                let mut body = MeshInstance3D::new_alloc();
                if alien {
                    let mut s = SphereMesh::new_gd();
                    s.set_radius(0.5);
                    s.set_height(1.0);
                    body.set_mesh(&s);
                    body.set_scale(Vector3::new(size.x, size.y.max(0.2), size.z * 1.3));
                } else {
                    let mut cap = CapsuleMesh::new_gd();
                    cap.set_radius(size.x * 0.45);
                    cap.set_height(size.y);
                    body.set_mesh(&cap);
                }
                body.set_material_override(&body_mat);
                if dead {
                    body.set_scale(Vector3::new(size.x, size.y * 0.25, size.z));
                }
                root.add_child(&body);
                // facing marker (eyes / visor)
                let mut eye = MeshInstance3D::new_alloc();
                let mut bm = BoxMesh::new_gd();
                bm.set_size(Vector3::new(size.x * 0.6, size.y * 0.12, 0.08));
                eye.set_mesh(&bm);
                eye.set_material_override(&accent_mat);
                eye.set_position(Vector3::new(0.0, size.y * 0.25, -size.z * 0.5));
                root.add_child(&eye);
                if !alien && !dead {
                    let mut gun = MeshInstance3D::new_alloc();
                    let mut gm = BoxMesh::new_gd();
                    gm.set_size(Vector3::new(0.08, 0.08, 0.6));
                    gun.set_mesh(&gm);
                    let gmat = self.material("#202428", 1.0, 0.0);
                    gun.set_material_override(&gmat);
                    gun.set_position(Vector3::new(size.x * 0.45, size.y * 0.05, -size.z * 0.35));
                    root.add_child(&gun);
                }
                let mut label = Label3D::new_alloc();
                label.set_text(&c.name);
                label.set_billboard_mode(BillboardMode::ENABLED);
                label.set_pixel_size(0.004);
                label.set_font_size(48);
                label.set_position(Vector3::new(0.0, size.y * 0.5 + 0.3, 0.0));
                label.set_modulate(if alien { Color::from_rgb(0.7, 1.0, 0.5) } else { Color::from_rgb(0.6, 0.8, 1.0) });
                root.add_child(&label);
            }
            EType::Buildable => {
                let b = e.buildable.unwrap();
                let alien = e.team == Team::Aliens;
                let col = match b {
                    BuildableId::Overmind => "#7a3aa0",
                    BuildableId::Eggpod => "#8a6a3a",
                    BuildableId::Barricade => "#4a6a2a",
                    BuildableId::AcidTube => "#6aa02a",
                    BuildableId::Trapper => "#8a4a6a",
                    BuildableId::Booster => "#c0a020",
                    BuildableId::Hive => "#a07a20",
                    BuildableId::Reactor => "#2a6ad0",
                    BuildableId::Telenode => "#40a0c0",
                    BuildableId::Mgturret => "#606a74",
                    BuildableId::Tesla => "#4080ff",
                    BuildableId::Arm => "#c08030",
                    BuildableId::Dcc => "#80a0b0",
                    BuildableId::Medistat => "#e04040",
                    BuildableId::Repeater => "#60c0ff",
                    // Tremulous 1.1.0's hovel (placeholder box without the data)
                    BuildableId::Hovel => "#6a5a3a",
                };
                let alpha = if e.spawned { 1.0 } else { 0.45 };
                let mat = self.material(col, alpha, if e.health > 0 { 0.4 } else { 0.0 });
                let mut mi = MeshInstance3D::new_alloc();
                if alien {
                    let mut s = SphereMesh::new_gd();
                    s.set_radius(0.5);
                    s.set_height(1.0);
                    mi.set_mesh(&s);
                    mi.set_scale(size);
                } else if matches!(b, BuildableId::Mgturret | BuildableId::Tesla | BuildableId::Repeater) {
                    let mut c = CylinderMesh::new_gd();
                    c.set_top_radius(size.x * 0.35);
                    c.set_bottom_radius(size.x * 0.5);
                    c.set_height(size.y);
                    mi.set_mesh(&c);
                } else {
                    let mut bm = BoxMesh::new_gd();
                    bm.set_size(size);
                    mi.set_mesh(&bm);
                }
                mi.set_material_override(&mat);
                root.add_child(&mi);
                if b == BuildableId::Mgturret {
                    let mut barrel = MeshInstance3D::new_alloc();
                    let mut bm = BoxMesh::new_gd();
                    bm.set_size(Vector3::new(0.1, 0.1, size.z * 0.9));
                    barrel.set_mesh(&bm);
                    let gm = self.material("#202428", 1.0, 0.0);
                    barrel.set_material_override(&gm);
                    barrel.set_position(Vector3::new(0.0, size.y * 0.3, -size.z * 0.4));
                    barrel.set_name("barrel");
                    root.add_child(&barrel);
                }
                let mut label = Label3D::new_alloc();
                label.set_text(b.cold().display_name);
                label.set_billboard_mode(BillboardMode::ENABLED);
                label.set_pixel_size(0.004);
                label.set_font_size(40);
                label.set_position(Vector3::new(0.0, size.y * 0.5 + 0.25, 0.0));
                label.set_name("label");
                root.add_child(&label);
            }
            EType::Missile => {
                let col = match e.missile.map(|m| m.id()) {
                    Some("flame") => "#ff7020",
                    Some("blaster") => "#ff4020",
                    Some("pulse") => "#40c0ff",
                    Some("lcannon") => "#80d0ff",
                    Some("grenade") => "#304030",
                    Some("hive") => "#ffd030",
                    Some("lockblob") | Some("slowblob") => "#a0ff40",
                    _ => "#ffffff",
                };
                let r = (e.maxs[0].max(3.0) * SCALE).max(0.08);
                let mat = self.material(col, 1.0, 4.0);
                let mut s = SphereMesh::new_gd();
                s.set_radius(r);
                s.set_height(r * 2.0);
                s.set_radial_segments(8);
                s.set_rings(4);
                let mut mi = MeshInstance3D::new_alloc();
                mi.set_mesh(&s);
                mi.set_material_override(&mat);
                root.add_child(&mi);
            }
            EType::Corpse => {
                let col = if e.team == Team::Aliens { "#3a5a2a" } else { "#3a3a5a" };
                let mat = self.material(col, 1.0, 0.0);
                let mut bm = BoxMesh::new_gd();
                bm.set_size(Vector3::new(size.x.max(0.4), 0.15, size.z.max(0.4)));
                let mut mi = MeshInstance3D::new_alloc();
                mi.set_mesh(&bm);
                mi.set_material_override(&mat);
                root.add_child(&mi);
            }
            _ => {}
        }
        root
    }

    /// A new map's acoustic mesh: for the sound path (fmod_frame) and, as
    /// the surface ragdolls land on, for physics.
    /// A new map's acoustic mesh (also the drawn-surface collider of the
    /// ragdolls) and solid brushes (the ragdolls' brush colliders).
    fn set_acoustic(&mut self, m: Option<crate::fmodaudio::AcousticMesh>, bsp: &trem_sim::kernel::bsp::Bsp) {
        self.free_ragdolls();
        self.world_hulls = std::rc::Rc::new(crate::ragdoll::bsp_hulls(bsp, 0));
        self.world_tris = std::rc::Rc::new(m.as_ref().map(|a| (a.verts.clone(), a.tris.clone())).unwrap_or_default());
        self.mover_hulls = std::rc::Rc::new((1..bsp.models.len()).map(|i| (i as i32, crate::ragdoll::bsp_hulls(bsp, i))).filter(|(_, h)| !h.is_empty()).collect());
        self.fmod_scene = m;
    }

    /// Every ragdoll and every body they collide with (vanilla has none).
    fn free_ragdolls(&mut self) {
        for r in self.ragdolls.drain(..) {
            r.free();
        }
        for mut c in self.ragdoll_world.take().into_iter().chain(self.ragdoll_brushes.take()) {
            c.queue_free();
        }
        for (_, mut c) in self.ragdoll_movers.drain() {
            if c.is_instance_valid() {
                c.queue_free();
            }
        }
        for (_, (_, mut c)) in self.ragdoll_solids.drain() {
            c.queue_free();
        }
    }

    /// The environment ragdolls collide with: the map (drawn surfaces and
    /// solid brushes), doors / lifts moving with their entities, structures.
    fn sync_ragdoll_world(&mut self) {
        if self.ragdoll_world.is_none() {
            if !self.world_tris.1.is_empty() {
                let body = crate::ragdoll::world_collider(&self.world_tris.0, &self.world_tris.1);
                self.base_mut().add_child(&body);
                self.ragdoll_world = Some(body);
            }
        }
        if self.ragdoll_brushes.is_none() && !self.world_hulls.is_empty() {
            let body = crate::ragdoll::brush_collider(&self.world_hulls);
            self.base_mut().add_child(&body);
            self.ragdoll_brushes = Some(body);
        }
        let Some(w) = self.world.as_ref() else { return };
        // movers: under their drawn node, so they move (and push) with it
        self.ragdoll_movers.retain(|i, b| {
            let keep = b.is_instance_valid() && w.ents.get(*i).is_some_and(|e| e.inuse && e.etype == EType::Mover && e.bmodel);
            if !keep && b.is_instance_valid() {
                b.queue_free();
            }
            keep
        });
        for i in 0..w.num_entities {
            let e = &w.ents[i];
            if !e.inuse || e.etype != EType::Mover || !e.bmodel || self.ragdoll_movers.contains_key(&i) {
                continue;
            }
            let (Some(h), Some(mut node)) = (self.mover_hulls.get(&e.model_index), self.mapents.brush_node(i)) else { continue };
            let body = crate::ragdoll::mover_collider(h);
            node.add_child(&body);
            self.ragdoll_movers.insert(i, body);
        }
        // structures: their server box
        let mut seen = HashSet::new();
        let mut add = Vec::new();
        for i in MAX_CLIENTS..w.num_entities {
            let e = &w.ents[i];
            if !e.inuse || e.etype != EType::Buildable || !e.linked || e.maxs[0] <= e.mins[0] {
                continue;
            }
            seen.insert(i);
            let key = format!("{:?}{:?}{:?}", e.origin, e.mins, e.maxs);
            if self.ragdoll_solids.get(&i).is_some_and(|(k, _)| *k == key) {
                continue;
            }
            let lo = q2g(&[e.origin[0] + e.mins[0], e.origin[1] + e.mins[1], e.origin[2] + e.mins[2]]);
            let hi = q2g(&[e.origin[0] + e.maxs[0], e.origin[1] + e.maxs[1], e.origin[2] + e.maxs[2]]);
            let body = crate::ragdoll::box_collider((lo + hi) * 0.5, (hi - lo).abs());
            if let Some((_, mut old)) = self.ragdoll_solids.insert(i, (key, body.clone())) {
                old.queue_free();
            }
            add.push(body);
        }
        let gone: Vec<usize> = self.ragdoll_solids.keys().copied().filter(|i| !seen.contains(i)).collect();
        for i in gone {
            if let Some((_, mut b)) = self.ragdoll_solids.remove(&i) {
                b.queue_free();
            }
        }
        for b in add {
            self.base_mut().add_child(&b);
        }
    }

    /// Enhanced mode, a BSP map, r_dynamicShadows: the world-fixed shadow
    /// lights (gfx.rs, lamps.rs). Otherwise none (vanilla draws no dynamic
    /// shadows).
    fn shadow_sun_on(&self) -> bool {
        self.client.cvars.integer("r_enhanced") != 0 && self.client.cvars.integer("r_dynamicShadows") != 0 && self.gfx.is_bsp && self.world.is_some() && self.lightgrid.is_some() && self.shadow_dirs.is_some()
    }

    /// The shadow lights: one along each of the map's light directions that
    /// light the area around the view (picked four times a second), faded in
    /// and out (their negative energy), never turned: a shadow keeps its
    /// direction however the viewer moves.
    fn sync_shadow_sun(&mut self, view: Vector3, dt: f32) {
        use godot::classes::directional_light_3d::ShadowMode;
        // Godot draws at most 8 directional lights
        const MAX_LIGHTS: usize = 8;
        let mut rs = godot::classes::RenderingServer::singleton();
        if !self.shadow_sun_on() {
            if !self.gfx.shadow_lights.is_empty() {
                for (_, mut l, _) in self.gfx.shadow_lights.drain(..) {
                    if l.is_instance_valid() {
                        l.queue_free();
                    }
                }
                rs.global_shader_parameter_set(crate::gfx::SHADOW_GLOBAL, &0.0f32.to_variant());
            }
            self.gfx.shadow_want.clear();
            self.gfx.shadow_pick_t = 0.0;
            return;
        }
        self.gfx.shadow_lights.retain(|(_, l, _)| l.is_instance_valid());
        self.gfx.shadow_pick_t -= dt;
        if self.gfx.shadow_pick_t <= 0.0 {
            self.gfx.shadow_pick_t = 0.25;
            let q = [view.x / crate::geom::SCALE, -view.z / crate::geom::SCALE, view.y / crate::geom::SCALE];
            let dirs = self.shadow_dirs.as_ref().unwrap();
            self.gfx.shadow_want = dirs.pick(q, crate::lamps::PICK_RADIUS, crate::lamps::ACTIVE).into_iter().map(|(i, _)| i).collect();
        }
        if self.gfx.shadow_lights.is_empty() {
            rs.global_shader_parameter_set(crate::gfx::SHADOW_GLOBAL, &crate::gfx::SHADOW_STRENGTH.to_variant());
        }
        for i in self.gfx.shadow_want.clone() {
            if self.gfx.shadow_lights.iter().any(|(j, _, _)| *j == i) || self.gfx.shadow_lights.len() >= MAX_LIGHTS {
                continue;
            }
            let d = q2g_dir(&self.shadow_dirs.as_ref().unwrap().dirs[i]).normalized();
            let mut s = DirectionalLight3D::new_alloc();
            s.set_name(&format!("ShadowLight{i}"));
            s.set_shadow(true);
            s.set_color(Color::from_rgb(1.0, 1.0, 1.0));
            // shadow-only: negative energy marks it for the shaders' light()
            s.set_param(LightParam::ENERGY, 0.0);
            s.set_cull_mask(crate::gfx::SHADOW_RECEIVER_LAYER);
            s.set("shadow_caster_mask", &((crate::gfx::SHADOW_CASTER_LAYER | crate::gfx::SHADOW_SELF_LAYER) as i64).to_variant());
            s.set_shadow_mode(ShadowMode::PARALLEL_2_SPLITS);
            s.set_param(LightParam::SHADOW_MAX_DISTANCE, 30.0);
            s.set_param(LightParam::SHADOW_BIAS, 0.03);
            s.set_param(LightParam::SHADOW_NORMAL_BIAS, 1.0);
            s.set_param(LightParam::SHADOW_BLUR, 1.5);
            s.set_blend_splits(true);
            let up = if d.y.abs() > 0.99 { Vector3::FORWARD } else { Vector3::UP };
            // a directional light shines along its -Z: from the light's side
            let x = up.cross(d).normalized();
            s.set_basis(Basis::from_cols(x, d.cross(x).normalized(), d));
            self.base_mut().add_child(&s);
            self.gfx.shadow_lights.push((i, s, 0.0));
        }
        let want = self.gfx.shadow_want.clone();
        let step = (dt * 3.0).min(1.0);
        self.gfx.shadow_lights.retain_mut(|(i, l, w)| {
            let target = if want.contains(i) { 1.0 } else { 0.0 };
            *w = if target > *w { (*w + step).min(1.0) } else { (*w - step).max(0.0) };
            if *w <= 0.0 && target == 0.0 {
                l.queue_free();
                return false;
            }
            // a hair below zero even when faded out: still a shadow light
            l.set_param(LightParam::ENERGY, -(*w).max(1e-4));
            true
        });
    }

    /// Enhanced mode: dead players and corpses fall as ragdolls. A ragdoll
    /// starts at the death (pushed by the killing hit), carries on as the
    /// server's corpse when it replaces the player, sinks with it and goes
    /// with it. The original corpse stays where the server has it, hidden:
    /// the vanilla mode (or F10 back to it) shows it again, unchanged.
    fn sync_ragdolls(&mut self, me: Option<usize>, first_person: bool) {
        let on = self.client.cvars.integer("r_enhanced") != 0 && self.client.cvars.integer("r_ragdolls") != 0;
        if !on || self.world.is_none() || self.lib.is_none() {
            // vanilla: no ragdoll and no physics body at all
            if !self.ragdolls.is_empty() || self.ragdoll_world.is_some() || self.ragdoll_brushes.is_some() || !self.ragdoll_movers.is_empty() || !self.ragdoll_solids.is_empty() {
                self.free_ragdolls();
            }
            return;
        }
        self.sync_ragdoll_world();
        if self.rigs.is_none() {
            let text = godot::classes::FileAccess::get_file_as_string("res://data/rigs.txt").to_string();
            let rigs = crate::ragdoll::parse_rigs(&text);
            if rigs.is_empty() {
                godot_warn!("trem: res://data/rigs.txt missing or empty: no ragdolls");
            }
            self.rigs = Some(std::rc::Rc::new(rigs));
        }
        let rigs = self.rigs.clone().unwrap();
        let now = self.clock;
        let mut new_roots = Vec::new();
        {
            let TremGame { world, lib, actors, ragdolls, death_push, alive_vel, lightgrid, .. } = self;
            let (w, lib) = (world.as_ref().unwrap(), lib.as_ref().unwrap());
            // dead: PM_DEAD / EF_DEAD (a player waiting to spawn also has no
            // health, and is not a corpse)
            let dead_player = |i: usize| -> bool {
                i < MAX_CLIENTS && w.ents[i].etype == EType::Player && {
                    let ps = &w.clients[i].ps;
                    ps.pm_type == trem_sim::gen::enums::PmType::Dead as i32 || ps.e_flags & k::EF_DEAD != 0
                }
            };
            let corpse = |i: usize| i < w.ents.len() && w.ents[i].inuse && w.ents[i].etype == EType::Corpse;
            for i in 0..MAX_CLIENTS.min(w.ents.len()) {
                if w.ents[i].inuse && w.ents[i].etype == EType::Player && !dead_player(i) {
                    alive_vel.insert(i, q2g(&w.clients[i].ps.velocity));
                }
            }
            // where the server has each dying body (the ragdoll travels on)
            for r in ragdolls.iter_mut().filter(|r| !r.corpse && dead_player(r.ent)) {
                r.server_pos = q2g(&w.clients[r.ent].ps.origin);
            }
            // the ragdolls whose body is gone: a corpse freed, a dead player
            // respawned and not taken over by its corpse within 3 s
            ragdolls.retain_mut(|r| {
                let keep = if r.corpse { corpse(r.ent) && actors.contains_key(&r.ent) } else { dead_player(r.ent) || now - r.born < 3.0 };
                if !keep {
                    r.free_in_place();
                }
                keep
            });
            let mut ids: Vec<usize> = actors.keys().copied().collect();
            ids.sort_unstable();
            for i in ids {
                let is_corpse = corpse(i);
                if !(is_corpse || dead_player(i)) || ragdolls.iter().any(|r| r.ent == i && r.corpse == is_corpse) {
                    continue;
                }
                let a = &actors[&i];
                if is_corpse {
                    // the corpse replacing a dying player's ragdoll (CopyToBodyQue):
                    // it spawns where the server had the body, wherever the
                    // ragdoll has travelled to
                    let at = q2g(&w.ents[i].origin);
                    if let Some(r) = ragdolls.iter_mut().filter(|r| !r.corpse && r.model_dir == a.model_dir && !dead_player(r.ent)).min_by(|x, y| x.server_pos.distance_to(at).partial_cmp(&y.server_pos.distance_to(at)).unwrap()) {
                        if r.server_pos.distance_to(at) < 3.0 {
                            r.ent = i;
                            r.corpse = true;
                            r.corpse_z = Some(w.ents[i].origin[2]);
                            continue;
                        }
                    }
                }
                let Some(rig) = rigs.get(&a.model_dir) else { continue };
                // a dying body: its momentum and the killing hit's push
                let push = if is_corpse {
                    None
                } else {
                    let hit = death_push.get(&i).filter(|(_, t)| now - t < 1.0).map(|(v, _)| *v).unwrap_or(Vector3::ZERO);
                    let alive = alive_vel.remove(&i).unwrap_or(Vector3::ZERO);
                    let dying = q2g(&w.clients[i].ps.velocity);
                    let v = crate::ragdoll::death_velocity([alive.x, alive.y, alive.z], [dying.x, dying.y, dying.z], [hit.x, hit.y, hit.z]);
                    Some(Vector3::new(v[0], v[1], v[2]))
                };
                // the least used self-collision layer
                let layer = (0..crate::ragdoll::SELF_LAYERS).map(crate::ragdoll::self_layer).min_by_key(|l| ragdolls.iter().filter(|r| r.layer == *l).count()).unwrap();
                if let Some(mut r) = crate::ragdoll::Ragdoll::build(lib, rig, &a.model_dir, a.model_scale(), &a.body_parts(), i, push, now, layer) {
                    if is_corpse {
                        r.corpse = true;
                        r.corpse_z = Some(w.ents[i].origin[2]);
                    }
                    new_roots.push(r.root.clone());
                    ragdolls.push(r);
                }
            }
            for r in ragdolls.iter_mut() {
                // the corpse sinks (the server lowers it before freeing it)
                if let Some(z0) = r.corpse_z {
                    let below = (z0 - w.ents[r.ent].origin[2]) * crate::geom::SCALE;
                    if below > 0.01 {
                        r.sink_to(below);
                    }
                }
                let p = r.position();
                let q = [p.x / crate::geom::SCALE, -p.z / crate::geom::SCALE, p.y / crate::geom::SCALE];
                let (amb, dcol, ld) = light_at(lightgrid.as_ref(), &q);
                r.set_light(amb, dcol, ld);
                r.set_visible(!(Some(r.ent) == me && first_person && !r.corpse));
                r.track_hinges();
                if let Some(a) = actors.get_mut(&r.ent) {
                    a.set_visible(false);
                }
            }
        }
        for n in new_roots {
            self.base_mut().add_child(&n);
        }
        for r in self.ragdolls.iter_mut() {
            r.start();
        }
    }

    /// Players, corpses and structures with the original animated models.
    /// Returns the entities drawn this way.
    fn sync_actors(&mut self) -> HashSet<usize> {
        let mut done = HashSet::new();
        if self.lib.is_none() || self.world.is_none() {
            return done;
        }
        let now = (self.clock * 1000.0) as i32;
        let me = self.view_ent();
        let first_person = !self.rendering_third_person();
        let shadows = self.shadow_sun_on();
        let mut new_nodes: Vec<Gd<Node3D>> = Vec::new();
        let mut sounds: Vec<SoundReq> = Vec::new();
        {
            let TremGame { lib, world, actors, failed_actors, lightgrid, missiles, .. } = self;
            let lib = lib.as_mut().unwrap();
            let w = world.as_ref().unwrap();
            let mut seen = HashSet::new();
            for i in 0..w.num_entities {
                let Some(key) = actor_key(w, i) else { continue };
                if failed_actors.contains(&key) {
                    continue;
                }
                if actors.get(&i).map(|a| a.key != key).unwrap_or(true) {
                    if let Some(old) = actors.remove(&i) {
                        old.free();
                    }
                    let e = &w.ents[i];
                    let made = match e.etype {
                        EType::Buildable => Actor::buildable(lib, e.buildable.unwrap(), key.clone()),
                        EType::Player => {
                            let c = &w.clients[i];
                            let class = ClassId::from_num(c.ps.stats[Stat::Class as usize] as u8).unwrap_or(ClassId::Spectator);
                            let name = if Some(i) == me { None } else { Some(c.name.as_str()) };
                            Actor::player(lib, class, c.team, &c.ps.stats, c.ps.weapon, name, key.clone(), now, w.vanilla)
                        }
                        EType::Corpse => {
                            let class = ClassId::from_num(e.generic1 as u8).unwrap_or(ClassId::HumanBase);
                            Actor::player(lib, class, e.team, &[0; 16], 0, None, key.clone(), now, w.vanilla)
                        }
                        _ => None,
                    };
                    match made {
                        Some(a) => {
                            new_nodes.extend(a.root_nodes());
                            actors.insert(i, a);
                        }
                        None => {
                            godot_warn!("actor {key} (entity {i}) could not be built; drawn as a placeholder");
                            failed_actors.insert(key);
                            continue;
                        }
                    }
                }
                let a = actors.get_mut(&i).unwrap();
                let lo = if w.ents[i].etype == EType::Player { w.clients[i].ps.origin } else { w.ents[i].origin };
                let (amb, dcol, ld) = light_at(lightgrid.as_ref(), &lo);
                a.set_light(amb, dcol, ld);
                match w.ents[i].etype {
                    EType::Buildable => a.update_buildable(w, i, now, &mut sounds),
                    _ => a.update_player(w, i, now),
                }
                // our own body in first person: unseen; enhanced, its shadow
                // still falls (the shadow sun)
                let own_hidden = Some(i) == me && first_person;
                if own_hidden && shadows {
                    a.set_visible(true);
                    a.set_shadow_only(true);
                } else {
                    a.set_shadow_only(false);
                    a.set_visible(!own_hidden);
                }
                seen.insert(i);
            }
            // projectiles: missile model or sprite from weapon.cfg
            let mut mseen = HashSet::new();
            for i in MAX_CLIENTS..w.num_entities {
                let e = &w.ents[i];
                if !e.inuse || e.etype != EType::Missile || !e.linked {
                    continue;
                }
                let key = format!("m{}:{}", e.weapon, e.generic1);
                if failed_actors.contains(&key) {
                    continue;
                }
                if missiles.get(&i).map(|m| m.key != key).unwrap_or(true) {
                    if let Some(old) = missiles.remove(&i) {
                        old.free();
                    }
                    match MissileVis::new(lib, e.weapon, (e.generic1 - 1).max(0) as usize, key.clone()) {
                        Some(m) => {
                            new_nodes.extend(m.nodes());
                            missiles.insert(i, m);
                        }
                        None => {
                            failed_actors.insert(key);
                            continue;
                        }
                    }
                }
                missiles.get_mut(&i).unwrap().update(&e.origin, &e.pos.tr_delta);
                mseen.insert(i);
            }
            let mgone: Vec<usize> = missiles.keys().copied().filter(|i| !mseen.contains(i)).collect();
            for i in mgone {
                if let Some(m) = missiles.remove(&i) {
                    m.free();
                }
            }
            seen.extend(mseen);
            let gone: Vec<usize> = actors.keys().copied().filter(|i| !seen.contains(i)).collect();
            for i in gone {
                if let Some(a) = actors.remove(&i) {
                    a.free();
                }
            }
            done = seen;
        }
        for n in new_nodes {
            self.base_mut().add_child(&n);
        }
        self.sync_ragdolls(me, first_person);
        for s in sounds {
            if let Some(path) = s.path.strip_prefix("@loop:") {
                // find the actor owning this loop: closest buildable at the origin
                let stream = self.resolve_sound(path);
                if let (Some(stream), Some(a)) = (stream, self.actors.get_mut(&s.ent)) {
                    if let Some(l) = a.looping_player() {
                        if let Ok(mut wav) = stream.clone().try_cast::<godot::classes::AudioStreamWav>() {
                            wav.set_loop_mode(godot::classes::audio_stream_wav::LoopMode::FORWARD);
                            let frames = wav.get_length() * wav.get_mix_rate() as f64;
                            wav.set_loop_end(frames as i32);
                        }
                        l.set_stream(&stream);
                        l.play();
                    }
                }
            } else {
                self.play_sound(&s.path, &s.origin, false);
            }
        }
        done
    }

    /// "a.wav|b.wav": first one present in the pk3s.
    fn resolve_sound(&mut self, paths: &str) -> Option<Gd<godot::classes::AudioStream>> {
        let lib = self.lib.as_mut()?;
        paths.split('|').find_map(|p| lib.sound(p))
    }

    fn play_sound(&mut self, paths: &str, origin: &[f32; 3], local: bool) {
        self.play_sound_ex(paths, origin, local, None, None);
    }

    /// `own`: the player's own sound (placed at the gun / feet / centre in
    /// enhanced mode); `ent`: the entity the sound follows while it plays.
    fn play_sound_ex(&mut self, paths: &str, origin: &[f32; 3], local: bool, own: Option<crate::fmodaudio::OwnSound>, ent: Option<usize>) {
        self.play_sound_big(paths, origin, local, own, ent, false);
    }

    /// `big`: weapon fire, impacts and explosions get their own reflections
    /// (enhanced mode).
    fn play_sound_big(&mut self, paths: &str, origin: &[f32; 3], local: bool, own: Option<crate::fmodaudio::OwnSound>, ent: Option<usize>, big: bool) {
        let enhanced = self.client.cvars.integer("r_enhanced") != 0;
        let view = self.camera.as_ref().filter(|c| c.is_inside_tree()).map(|c| c.get_global_transform());
        // enhanced: the high-band sound pack (outside the pk3 search path)
        let hd_root = (enhanced && self.client.cvars.integer("s_hdSounds") != 0).then(|| crate::hdtex::pack_dir("sfx_hd"));
        // the FMOD + Steam Audio path (fmodaudio.rs) when it is up
        if let (Some(fm), Some(lib)) = (self.fmod.as_mut(), self.lib.as_mut()) {
            for p in paths.split('|') {
                let key = p.to_lowercase();
                let hd = hd_root.as_ref().and_then(|r| fm.sound(&format!("hd:{key}"), || std::fs::read(format!("{r}/{key}")).ok()));
                let id = hd.or_else(|| fm.sound(&key, || lib.pk3.read(p).map(|d| d.as_slice().to_vec())));
                if let Some(id) = id {
                    let g = q2g(origin);
                    let (pos, own_placed) = match (local, own.filter(|_| enhanced), view) {
                        (true, _, _) => (None, false),
                        (false, Some(k), Some(t)) => {
                            let (o, f, r, u) = (t.origin, -t.basis.col_c(), t.basis.col_a(), t.basis.col_b());
                            (crate::fmodaudio::own_sound_pos(k, [o.x, o.y, o.z], [f.x, f.y, f.z], [r.x, r.y, r.z], [u.x, u.y, u.z]), true)
                        }
                        _ => (Some([g.x, g.y, g.z]), false),
                    };
                    let v = fm.play(id, pos, own_placed, big, false, 1.0);
                    if v >= 0 && pos.is_some() && !own_placed {
                        self.fmod_voices.retain(|x| x.0 != v);
                        self.fmod_voices.push((v, *origin));
                    }
                    if v >= 0 {
                        if let (Some(e), false, false) = (ent, local, own_placed) {
                            self.fmod_attached.push((v, e));
                        }
                        return;
                    }
                    break;
                }
            }
            // a sound FMOD cannot open or play: Godot's audio plays it
        }
        let Some(stream) = self.resolve_sound(paths) else { return };
        if let Some(a) = self.audio.as_mut() {
            if local {
                a.play_local(&stream, 0.0);
            } else {
                a.play_at(&stream, origin, 0.0);
            }
        }
    }

    /// `CG_CustomSound`: "*name" resolves to the entity's model sound dir.
    fn custom_sound(&self, ent: i32, name: &str) -> String {
        let dir = self.world.as_ref().and_then(|w| {
            let i = usize::try_from(ent).ok()?;
            let class = if i < MAX_CLIENTS && w.ents[i].client { w.clients[i].ps.stats[Stat::Class as usize] } else { w.ents.get(i)?.generic1 };
            ClassId::from_num(class as u8).and_then(|c| class_model(c, w.vanilla)).map(|m| m.0)
        });
        let dir = dir.unwrap_or_else(|| "human_base".into());
        format!("sound/player/{dir}/{name}|sound/player/human_base/{name}")
    }

    fn sync_map_entities(&mut self) {
        let Some(w) = self.world.as_ref() else { return };
        let now = (self.clock * 1000.0) as i32;
        let (view, fwd) = match self.me {
            Some(m) => {
                let ps = &w.clients[m].ps;
                (bg::client_view_origin(ps), angle_vectors(&ps.viewangles).0)
            }
            None => ([0.0; 3], [1.0, 0.0, 0.0]),
        };
        let grid = self.lightgrid.as_ref();
        let light = |p: &[f32; 3]| light_at(grid, p);
        let (add, sounds) = self.mapents.sync(w, self.lib.as_mut(), &mut self.brush_meshes, now, view, fwd, self.me, light);
        for n in add {
            self.base_mut().add_child(&n);
        }
        for (p, o) in sounds {
            self.play_sound(&p, &o, false);
        }
    }

    /// `CG_PlayerSprites` for every player: `CG_PlayerFloatSprite` of
    /// cgs.media.connectionShader (gfx/2d/net) 48 units over a player with
    /// EF_CONNECTION, radius 10, only in mirrors (RF_THIRD_PERSON) for
    /// ourselves in first person. (As in the original, ClientEndFrame's flag
    /// is overwritten by BG_PlayerStateToEntityState, and every client here
    /// sends commands each frame, so it does not show in practice.)
    fn player_sprites(&mut self) {
        let Some(w) = self.world.as_ref() else { return };
        let me = self.view_ent();
        let third = self.rendering_third_person();
        let mut want: Vec<(usize, [f32; 3])> = Vec::new();
        for i in 0..MAX_CLIENTS.min(w.num_entities) {
            let e = &w.ents[i];
            if !e.inuse || e.etype != EType::Player || e.e_flags & k::EF_CONNECTION == 0 {
                continue;
            }
            if Some(i) == me && !third {
                continue; // RF_THIRD_PERSON: only show in mirrors
            }
            let mut o = w.clients[i].ps.origin;
            o[2] += 48.0;
            want.push((i, o));
        }
        let stale: Vec<usize> = self.float_sprites.keys().filter(|i| !want.iter().any(|(j, _)| j == *i)).copied().collect();
        for i in stale {
            if let Some(mut n) = self.float_sprites.remove(&i) {
                n.queue_free();
            }
        }
        for (i, o) in want {
            if !self.float_sprites.contains_key(&i) {
                let Some(tex) = self.lib.as_mut().map(|l| l.texture("gfx/2d/net").unwrap_or_else(|| l.default_texture())) else { return };
                let mut quad = godot::classes::QuadMesh::new_gd();
                quad.set_size(Vector2::splat(20.0 * SCALE));
                let mut mat = StandardMaterial3D::new_gd();
                mat.set_texture(TextureParam::ALBEDO, &tex);
                mat.set_billboard_mode(BillboardMode::ENABLED);
                mat.set_shading_mode(godot::classes::base_material_3d::ShadingMode::UNSHADED);
                let mut mi = MeshInstance3D::new_alloc();
                mi.set_mesh(&quad);
                mi.set_material_override(&mat);
                self.base_mut().add_child(&mi);
                self.float_sprites.insert(i, mi);
            }
            if let Some(n) = self.float_sprites.get_mut(&i) {
                n.set_position(q2g(&o));
            }
        }
    }

    /// Dead (ourselves or the followed player): the third person death cam.
    fn dead_view(&self) -> bool {
        let (Some(w), Some(me)) = (self.world.as_ref(), self.me) else { return false };
        let ps = &w.clients[me].ps;
        ps.stats[Stat::Health as usize] <= 0 && (ps.pm_flags & k::PMF_FOLLOW != 0 || w.clients[me].spectator == SpectatorState::Not as i32)
    }

    /// cg.renderingThirdPerson: cg_thirdPerson, dead, or the chase cam
    /// (cg.chaseFollow while following).
    fn rendering_third_person(&self) -> bool {
        let following = self.world.as_ref().zip(self.me).is_some_and(|(w, me)| w.clients[me].ps.pm_flags & k::PMF_FOLLOW != 0);
        self.third_person || self.dead_view() || (self.cgv.chase_follow && following)
    }

    /// The entity whose eyes we look through: ourselves, or the player we
    /// follow as a spectator (PMF_FOLLOW, ps.clientNum).
    fn view_ent(&self) -> Option<usize> {
        let (w, me) = (self.world.as_ref()?, self.me?);
        let ps = &w.clients[me].ps;
        if ps.pm_flags & k::PMF_FOLLOW != 0 && (ps.client_num as usize) < MAX_CLIENTS {
            Some(ps.client_num as usize)
        } else {
            Some(me)
        }
    }

    fn sync_visuals(&mut self) {
        self.sync_map_entities();
        let handled = self.sync_actors();
        self.player_sprites();
        let me = self.view_ent();
        let Some(w) = self.world.as_ref() else { return };
        let first_person = !self.rendering_third_person();
        let n = w.num_entities;
        let mut want: Vec<(usize, String)> = Vec::new();
        for i in 0..n {
            if Some(i) == me && first_person || handled.contains(&i) {
                continue;
            }
            if let Some(k_) = Self::visual_key(w, i) {
                want.push((i, k_));
            }
        }
        // remove stale
        let keep: HashMap<usize, &String> = want.iter().map(|(i, k_)| (*i, k_)).collect();
        let stale: Vec<usize> = self.visuals.iter().filter(|(i, v)| keep.get(i).map(|k_| **k_ != v.key).unwrap_or(true)).map(|(i, _)| *i).collect();
        for i in stale {
            if let Some(mut v) = self.visuals.remove(&i) {
                v.node.queue_free();
            }
        }
        for (i, key) in want {
            if !self.visuals.contains_key(&i) {
                let node = self.make_visual(i);
                self.base_mut().add_child(&node);
                self.visuals.insert(i, Visual { key, node });
            }
            let w = self.world.as_ref().unwrap();
            let e = &w.ents[i];
            let (origin, angles) = if e.etype == EType::Player { (w.clients[i].ps.origin, w.clients[i].ps.viewangles) } else { (e.origin, e.angles) };
            let mut c = origin;
            for k_ in 0..3 {
                c[k_] += (e.mins[k_] + e.maxs[k_]) * 0.5;
            }
            let yaw_only = [0.0, angles[1], 0.0];
            let basis = if e.etype == EType::Missile { Basis::IDENTITY } else { basis_from_angles(&yaw_only) };
            let health_text = if e.etype == EType::Buildable {
                let b = e.buildable.unwrap();
                Some(format!("{} {}%", b.cold().display_name, (e.health.max(0) * 100 / b.def().health.max(1))))
            } else {
                None
            };
            let turret_aim = e.turret_aim;
            let v = self.visuals.get_mut(&i).unwrap();
            v.node.set_position(q2g(&c));
            v.node.set_basis(basis);
            if let Some(t) = health_text {
                if let Some(mut l) = v.node.try_get_node_as::<Label3D>("label") {
                    l.set_text(&t);
                }
                if let Some(mut b) = v.node.try_get_node_as::<MeshInstance3D>("barrel") {
                    let rel = [turret_aim[0], turret_aim[1] - angles[1], 0.0];
                    b.set_basis(basis_from_angles(&rel));
                }
            }
        }
    }

    /// cg.zoomSensitivity (trap_SetUserCmdValue): mouse look scale.
    fn view_state_zoom_sensitivity(&self) -> f32 {
        self.cgv.zoom_sensitivity
    }

    /// `CG_TransitionPlayerState` for our (snapshot) player state after a
    /// tick: damage kick, local pain sounds, duck smoothing (view.rs).
    fn transition_player_state(&mut self) {
        let (Some(w), Some(me)) = (self.world.as_ref(), self.me) else { return };
        let ps = w.clients[me].ps;
        self.cgv.time = w.level.time;
        // CG_Respawn on a new PERS_SPAWN_COUNT (or another followed player)
        let spawn = (ps.client_num, ps.persistant[trem_sim::gen::enums::Pers::SpawnCount as usize]);
        if self.last_spawn != Some(spawn) {
            self.last_spawn = Some(spawn);
            self.client.wsel.respawn(&ps, w.level.time);
        }
        for s in self.cgv.transition_player_state(&ps) {
            let p = self.custom_sound(ps.client_num, &s);
            self.play_sound_ex(&p, &ps.origin, false, Some(crate::fmodaudio::OwnSound::Voice), None);
        }
    }

    /// cg_viewsize for `CG_CalcVrect` (the intermission is always full screen).
    fn view_size(&self, ps: &trem_sim::kernel::types::PlayerState) -> i32 {
        if ps.pm_type == trem_sim::gen::enums::PmType::Intermission as i32 {
            100
        } else {
            // (sizeup / sizedown keep it in 30..100; a typed value is clamped to the screen)
            self.client.cvars.integer("cg_viewsize").clamp(1, 100)
        }
    }

    /// The SubViewport the 3D view renders into (sharing the main World3D,
    /// with the root viewport's 3D settings) and the Sprite2D that shows it
    /// under the HUD's canvas layer.
    fn make_view_viewport(root: &Option<Gd<godot::classes::Viewport>>) -> (Gd<godot::classes::SubViewport>, Gd<godot::classes::Sprite2D>) {
        use godot::classes::sub_viewport::UpdateMode;
        let mut vp = godot::classes::SubViewport::new_alloc();
        vp.set_name("view3d");
        vp.set_update_mode(UpdateMode::ALWAYS);
        vp.set_as_audio_listener_3d(true);
        if let Some(r) = root {
            vp.set_msaa_3d(r.get_msaa_3d());
            vp.set_screen_space_aa(r.get_screen_space_aa());
            vp.set_use_taa(r.is_using_taa());
            vp.set_use_debanding(r.is_using_debanding());
            vp.set_scaling_3d_mode(r.get_scaling_3d_mode());
            vp.set_scaling_3d_scale(r.get_scaling_3d_scale());
            vp.set_positional_shadow_atlas_size(r.get_positional_shadow_atlas_size());
            vp.set_anisotropic_filtering_level(r.get_anisotropic_filtering_level());
            let size = r.get_visible_rect().size;
            vp.set_size(Vector2i::new((size.x as i32).max(2), (size.y as i32).max(2)));
        }
        let mut sprite = godot::classes::Sprite2D::new_alloc();
        sprite.set_name("view3d_display");
        sprite.set_centered(false);
        sprite.set_region_enabled(true);
        sprite.set_texture(&vp.get_texture().map(|t| t.upcast::<godot::classes::Texture2D>()).unwrap());
        (vp, sprite)
    }

    /// `CG_CalcVrect` + the refdef projection: size the view SubViewport to
    /// cg.refdef's window, render with a vertical fov that covers both
    /// fov_x and fov_y (`projection_crop`) and show the part that spans
    /// exactly fov_x by fov_y stretched over the window, so the underwater
    /// and poison cloud warps of `CG_CalcFov` (fov_x and fov_y moving
    /// independently) take effect. `CG_TileClear` (the HUD) fills around it.
    fn sync_view_rect(&mut self, rd: &crate::view::Refdef) {
        let Some(root) = self.base().get_viewport() else { return };
        let size = root.get_visible_rect().size;
        let ps = self.world.as_ref().zip(self.me).map(|(w, me)| w.clients[me].ps).unwrap_or_default();
        let (x, y, w, h) = crate::view::calc_vrect(size.x as i32, size.y as i32, self.view_size(&ps));
        // a camera made current on the root viewport (tests' fixed views) wins
        let root_cam = root.get_camera_3d().is_some();
        let (Some(vp), Some(sprite), Some(cam)) = (self.view_vp.as_mut(), self.view_sprite.as_mut(), self.camera.as_mut()) else { return };
        if root_cam || w < 2 || h < 2 {
            sprite.set_visible(false);
            vp.set_update_mode(godot::classes::sub_viewport::UpdateMode::DISABLED);
            return;
        }
        if !sprite.is_visible() {
            sprite.set_visible(true);
            vp.set_update_mode(godot::classes::sub_viewport::UpdateMode::ALWAYS);
        }
        if self.vrect.2 != w || self.vrect.3 != h {
            vp.set_size(Vector2i::new(w, h));
        }
        self.vrect = (x, y, w, h);
        let (cam_fov_y, u, v) = crate::view::projection_crop(rd.fov_x, rd.fov_y, w as f32, h as f32);
        cam.set_fov(cam_fov_y);
        let (rw, rh) = (w as f32 * u, h as f32 * v);
        sprite.set_region_rect(Rect2::new(Vector2::new((w as f32 - rw) * 0.5, (h as f32 - rh) * 0.5), Vector2::new(rw, rh)));
        sprite.set_position(Vector2::new(x as f32, y as f32));
        sprite.set_scale(Vector2::new(w as f32 / rw, h as f32 / rh));
    }

    /// `CG_CalcViewValues` (view.rs) onto the camera, then `CG_AddViewWeapon`:
    /// the build ghost (`CG_GhostBuildable`) and the first-person weapon.
    fn sync_camera(&mut self) {
        let (Some(w), Some(me)) = (self.world.as_ref(), self.me) else { return };
        // SpectatorClientEndFrame copied the followed player's state into ours
        let ps = w.clients[me].ps;
        let cmd = w.clients[me].cmd;
        let time = w.level.time + self.acc_ms as i32;
        // CG_CalcVrect: cg.refdef.width / height for CG_CalcFov
        let size = self.base().get_viewport().map(|v| v.get_visible_rect().size).unwrap_or(Vector2::new(4.0, 3.0));
        let (_, _, vw, vh) = crate::view::calc_vrect(size.x as i32, size.y as i32, self.view_size(&ps));
        let aspect = if vh > 0 { vw as f32 / vh as f32 } else { 4.0 / 3.0 };
        let rd = {
            let TremGame { world, cgv, third_person, .. } = self;
            cgv.calc_view_values(world.as_ref().unwrap(), &ps, &cmd, *third_person, aspect, time)
        };
        if let Some(cam) = self.camera.as_mut() {
            cam.set_position(q2g(&rd.vieworg));
            cam.set_basis(basis_from_angles(&rd.angles));
        }
        self.view_fov = (rd.fov_x, rd.fov_y);
        self.sync_view_rect(&rd);
        self.cam_q = (rd.vieworg, rd.angles);
        if rd.send_follow {
            // trap_SendClientCommand( "follow\n" )
            let out = self.client_command_line("follow");
            if !out.is_empty() {
                godot_print!("{out}");
            }
        }
        let w = self.world.as_ref().unwrap();
        let wpn = ps.weapon;
        // STAT_TEAM: the followed player's team when spectating
        let human = ps.stat(Stat::Team) == Team::Humans as i32;
        let view = self.view_ent().unwrap_or(me);
        let active = wpn != 0 && ps.persistant[trem_sim::gen::enums::Pers::Specstate as usize] == 0 && ps.pm_type != trem_sim::gen::enums::PmType::Intermission as i32;
        // CG_GhostBuildable: a prospective buildable infront of the player
        let sb = ps.stats[Stat::Buildable as usize];
        let ghost = if active && sb & !k::SB_VALID_TOGGLEBIT > 0 {
            BuildableId::from_num((sb & !k::SB_VALID_TOGGLEBIT) as u8).map(|b| {
                let d = b.def();
                // a Tremulous 1.1.0 game: its boxes and placement (dec041)
                let (mins, maxs) = match trem_sim::kernel::v11::buildable11_of(b).filter(|_| w.vanilla) {
                    Some(b11) => (b11.def().mins, b11.def().maxs),
                    None => (d.mins, d.maxs),
                };
                let (o, _, tr) = if w.vanilla { trem_sim::kernel::bg11::position_buildable_relative_to_player(&ps, &mins, &maxs, w) } else { bg::position_buildable_relative_to_player(&ps, &mins, &maxs, w) };
                let (axis, org) = crate::view::position_and_orientate_buildable(w, &ps.viewangles, &o, &tr.plane.normal, ps.client_num, &mins, &maxs);
                (b, axis, org, tr.plane.normal, o, sb & k::SB_VALID_TOGGLEBIT != 0)
            })
        } else {
            None
        };
        self.cgv.draw_debug.clear();
        // the original model with the green/red build shader when the data is present
        let mut ghost_done = false;
        if let (Some((b, axis, org, normal, _, ok)), true) = (ghost, self.lib.is_some()) {
            if self.ghost_model.as_ref().is_none_or(|g| g.buildable != b) {
                if let Some(mut g) = self.ghost_model.take() {
                    g.hide();
                    g.free();
                }
                if let Some(g) = GhostModel::new(self.lib.as_mut().unwrap(), b) {
                    for n in g.nodes() {
                        self.base_mut().add_child(&n);
                    }
                    self.ghost_model = Some(g);
                }
            }
            self.cgv.draw_debug = format!("ghost {:?} model {} at {:.0} {:.0} {:.0} normal {:.2} {:.2} {:.2} valid {ok}", b, self.ghost_model.is_some(), org[0], org[1], org[2], normal[0], normal[1], normal[2]);
            if let Some(g) = self.ghost_model.as_mut() {
                if normal == [0.0; 3] {
                    // no surface below: the C's axis is degenerate (nothing visible)
                    g.hide();
                } else {
                    // offset on the Z axis if required
                    let o = ma(&org, g.z_offset, &normal);
                    g.update(&o, &axis, ok);
                }
                ghost_done = true;
            }
        } else if let Some(g) = self.ghost_model.as_mut() {
            g.hide();
        }
        // first-person weapon (CG_AddViewWeapon): no gun in third person,
        // zoomed weapons keep it like the C (only cg_drawGun hides it)
        let show_hand = self.cgv.draws_view_weapon(&ps, &rd);
        let mut hand_ok = false;
        if self.lib.is_some() {
            if self.hand.as_ref().map(|h| h.weapon != wpn).unwrap_or(true) {
                if let Some(h) = self.hand.take() {
                    h.free();
                }
                if wpn > 0 {
                    if let Some(h) = ViewHand::new(self.lib.as_mut().unwrap(), wpn) {
                        for n in h.nodes() {
                            self.base_mut().add_child(&n);
                        }
                        self.hand = Some(h);
                    }
                }
            }
            let frames = self.actors.get(&view).map(|a| {
                let (o, f, b) = a.torso_frames();
                let an = a.torso_anims();
                (map_torso_to_weapon_frame(&an, o), map_torso_to_weapon_frame(&an, f), b)
            });
            let (amb, dcol, ld) = light_at(self.lightgrid.as_ref(), &ps.origin);
            if self.hand.is_some() {
                self.cgv.weapon_no_drift = self.hand.as_ref().is_some_and(|h| h.no_drift);
                let (o, axis) = self.cgv.view_weapon_position(&ps, &rd);
                let (mode, firing) = crate::view::view_weapon_firing(&ps);
                let barrel = self.cgv.machinegun_spin_angle(firing);
                let cont = self.hand.as_ref().map(|h| h.continuous_flash[mode]).unwrap_or(false);
                let flash = self.cgv.view_flash_roll(firing, cont);
                let h = self.hand.as_mut().unwrap();
                h.set_light(amb, dcol, ld);
                h.update(&o, &axis, frames.unwrap_or((0, 0, 0.0)), show_hand, barrel, flash, ps.weapon_anim, time);
                hand_ok = true;
            }
        }
        // placeholder box gun without the original data (humans)
        if let Some(vm) = self.viewmodel.as_mut() {
            vm.set_visible(show_hand && human && !hand_ok);
        }
        if let Some(vm) = self.viewmodel.clone().as_mut() {
            if show_hand && human {
                let col = match WeaponId::from_num(wpn as u8) {
                    Some(WeaponId::Ckit) | Some(WeaponId::Ackit) => "#c0a030",
                    Some(WeaponId::Blaster) => "#a04040",
                    Some(WeaponId::Lcannon) | Some(WeaponId::Prifle) | Some(WeaponId::Lgun) => "#4080c0",
                    Some(WeaponId::Flamer) => "#c06020",
                    _ => "#30343a",
                };
                let m = self.material(col, 1.0, 0.0);
                vm.set_material_override(&m);
            }
        }
        // fallback: the structure's bounding box in green/red
        match ghost.filter(|_| !ghost_done) {
            Some((b, _, _, _, o, ok)) => {
                let d = b.def();
                let mut c = o;
                for k_ in 0..3 {
                    c[k_] += (d.mins[k_] + d.maxs[k_]) * 0.5;
                }
                let size = Vector3::new((d.maxs[0] - d.mins[0]) * SCALE, (d.maxs[2] - d.mins[2]) * SCALE, (d.maxs[1] - d.mins[1]) * SCALE);
                let m = self.material(if ok { "#40ff60" } else { "#ff4040" }, 0.35, 1.0);
                if let Some(g) = self.ghost.as_mut() {
                    let mut bm = BoxMesh::new_gd();
                    bm.set_size(size);
                    g.set_mesh(&bm);
                    g.set_material_override(&m);
                    g.set_position(q2g(&c));
                    g.set_visible(true);
                }
            }
            None => {
                if let Some(g) = self.ghost.as_mut() {
                    g.set_visible(false);
                }
            }
        }
    }
}

// =====================================================================
// networking (dec033): the network host around the presentation
// =====================================================================

impl TremGame {
    /// A network session is up: a listen server, or a client connected or
    /// connecting somewhere. The presentation then reads the view World.
    fn net_active(&self) -> bool {
        self.nethost.as_ref().is_some_and(|n| n.server_running() || n.client_connected())
    }

    /// The authoritative game World: the listen server's, else (single
    /// process) the one ticked here; None as a remote server's client.
    fn game_world(&self) -> Option<&World> {
        match self.nethost.as_ref() {
            Some(n) if n.server_running() => n.server.as_ref().map(|s| &s.world),
            Some(n) if n.client_connected() => None,
            _ => self.world.as_ref(),
        }
    }

    fn game_world_mut(&mut self) -> Option<&mut World> {
        let net = self.nethost.as_ref().map(|n| (n.server_running(), n.client_connected()));
        match net {
            Some((true, _)) => self.nethost.as_mut().and_then(|n| n.server.as_mut()).map(|s| &mut s.world),
            Some((false, true)) => None,
            _ => self.world.as_mut(),
        }
    }

    /// A cvar from the server side (rotation / console sets), else the client's.
    /// dec046: a network server asks the router to forward its port
    /// (`net_upnp`, on unless 0), on a thread; once per server.
    fn upnp_enabled(&self) -> bool {
        !self
            .net_cvar("net_upnp")
            .is_some_and(|v| trem_sim::kernel::spawn::c_atoi(&v) == 0)
    }

    fn upnp_start(&mut self) {
        if self.upnp_pending.is_some() || !self.port_keepers.is_empty() || !self.upnp_enabled() || self.host_lan_only {
            return;
        }
        let Some(port) = self.nethost.as_ref().and_then(|n| n.net.port(trem_sim::kernel::netchan::NetSrc::Server)) else { return };
        // the IPv6 socket's pinhole, when the server has a global IPv6 address
        let v6 = self.server_v6();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(crate::pcp::open_udp(port, v6, "Tremulous"));
        });
        self.upnp_pending = Some(rx);
    }

    /// The hosted server's global IPv6 address and IPv6 port (dec076).
    fn server_v6(&self) -> Option<(std::net::Ipv6Addr, u16)> {
        let nh = self.nethost.as_ref()?;
        let src = trem_sim::kernel::netchan::NetSrc::Server;
        if !nh.net.is_open6(src) || nh.net.ports6[src as usize] == 0 {
            return None;
        }
        Some((crate::pcp::global_v6()?, nh.net.ports6[src as usize]))
    }

    /// dec076: the network check (`net_natCheck`, archived, on unless 0):
    /// two STUN requests from the server's game socket when it starts.
    fn natcheck_start(&mut self) {
        self.client.cvars.get("net_natCheck", "1", crate::cmd::CVAR_ARCHIVE);
        self.client.cvars.get("net_stunServers", crate::natcheck::DEFAULT_SERVERS, crate::cmd::CVAR_ARCHIVE);
        if self.nat_probe.is_some() || self.nat_answers.is_some() || !self.natcheck_enabled() {
            return;
        }
        let list = self.net_cvar("net_stunServers").map(|v| crate::natcheck::server_list(&v)).filter(|l| !l.is_empty()).unwrap_or_else(|| crate::natcheck::server_list(crate::natcheck::DEFAULT_SERVERS));
        self.nat_probe = Some(crate::natcheck::Probe::start(list, crate::natcheck::resolve_v4));
    }

    fn natcheck_enabled(&self) -> bool {
        !self
            .net_cvar("net_natCheck")
            .is_some_and(|v| trem_sim::kernel::spawn::c_atoi(&v) == 0)
    }

    fn natcheck_poll(&mut self) {
        let (Some(p), Some(nh)) = (self.nat_probe.as_mut(), self.nethost.as_mut()) else { return };
        if p.poll(&mut nh.net) {
            let p = self.nat_probe.take().unwrap();
            godot_print!("{}", p.asked_line());
            self.nat_answers = Some((p.mapped(), p.other_mapped(), p.asked_line()));
            // the status went out without it (frames stalled): an update
            self.status_refresh();
        }
    }

    /// An answer that came after the status was printed (the router, the
    /// network check): the changed lines as "Update: ...", and the open
    /// dialog redrawn with the new text.
    fn status_refresh(&mut self) {
        if self.host_report.is_some() {
            return;
        }
        let Some(old) = self.last_status.clone() else { return };
        let mut st = old.clone();
        if let Some((fw, _)) = self.fw_late.take() {
            st.fw = fw;
        }
        if let Some(r) = &self.router_last {
            if st.router != crate::hoststatus::Router::Off {
                st.router = r.clone();
            }
        }
        if let Some((mapped, other, asked)) = &self.nat_answers {
            st.nat = Some(crate::natcheck::classify(crate::upnp::default_route_ip(), mapped, &crate::hoststatus::router_facts(&st.router), *other));
            st.asked = Some(asked.clone());
        }
        if let Some(v) = &st.v6 {
            st.v6 = crate::hoststatus::v6_join(Some(v.addr), v.port, self.port_last.as_ref());
        }
        if st == old {
            return;
        }
        for l in crate::hoststatus::updates(&old, &st) {
            self.host_say(l);
        }
        if let (Some(r), true) = (&self.firewall_refresh, self.firewall_choice.is_some()) {
            let exe = std::env::current_exe().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
            let p = st.status_prompt(&exe, false);
            let mut content = p.content;
            if old.fw == crate::hoststatus::Fw::Checking && st.fw.lacks() {
                // the open window was made without the firewall's buttons
                content.push_str("\n\nTo open the firewall now: type firewall_allow in the console, or close this window and type hosting_status.");
            }
            r.set(&p.main, &content);
        }
        self.last_status = Some(st);
    }

    /// The renewal threads' news: the router restarted (mapped again) or
    /// the mapping was lost.
    fn port_events_poll(&mut self) {
        let Some(rx) = self.port_events.as_ref() else { return };
        let mut news = Vec::new();
        while let Ok(e) = rx.try_recv() {
            news.push(e);
        }
        for e in news {
            let l = match e {
                crate::pcp::Event::Remapped(l) => format!("Update: the router had restarted or moved the port; {} opened UDP {} again.", l.proto.name(), l.internal_port),
                crate::pcp::Event::Lost(p) => format!("Update: the router stopped forwarding the port ({}). Internet friends cannot join until it is forwarded again.", p.name()),
                crate::pcp::Event::UpnpLost => "Update: the router stopped forwarding the port (UPnP). Internet friends cannot join until it is forwarded again.".into(),
            };
            self.host_say(l);
        }
    }

    /// The router's answer: kept for the status (and the mapping to take back).
    fn upnp_poll(&mut self) {
        let Some(rx) = self.upnp_pending.as_ref() else { return };
        let Ok(r) = rx.try_recv() else { return };
        self.upnp_pending = None;
        let router = crate::hoststatus::Router::from_port(&r);
        godot_print!("Port forwarding: {r:?}");
        let (tx, rx) = std::sync::mpsc::channel();
        if let Some(crate::pcp::Opened::Upnp(m)) = &r.opened {
            // a timed lease (2 h), renewed like PCP's; permanent only on error 725 routers
            self.port_keepers.push(crate::pcp::keep_upnp(m.clone(), crate::pcp::RECHECK, tx.clone()));
        }
        let leases = [r.pcp.lease(), r.v6.as_ref().and_then(|o| o.lease())];
        for l in leases.into_iter().flatten() {
            self.port_keepers.push(crate::pcp::keep(l.clone(), crate::pcp::RECHECK, tx.clone()));
        }
        if !self.port_keepers.is_empty() {
            self.port_events = Some(rx);
        }
        self.port_last = Some(r);
        self.router_last = Some(router.clone());
        if let Some(rep) = self.host_report.as_mut() {
            rep.router = Some(router);
        } else {
            // the dialog did not wait for it (4 s): say it once it is known
            self.status_refresh();
        }
    }

    /// A status to collect for the game being hosted (`ask`: the dialog may open).
    fn new_host_report(&self, ask: bool) -> HostReport {
        let upnp_on = self.upnp_enabled();
        let router = if self.host_lan_only {
            Some(crate::hoststatus::Router::Released)
        } else if !upnp_on {
            Some(crate::hoststatus::Router::Off)
        } else if !self.port_keepers.is_empty() {
            self.router_last.clone()
        } else {
            None
        };
        HostReport {
            // the map being loaded; hosting_status and the re-check after Allow come later: the last status's
            map: self.view_map.clone().or_else(|| self.last_status.as_ref().map(|s| s.map.clone())).unwrap_or_default(),
            port: self
                .nethost
                .as_ref()
                .and_then(|n| n.net.port(trem_sim::kernel::netchan::NetSrc::Server))
                .unwrap_or(trem_sim::kernel::netchan::PORT_SERVER),
            clock: crate::natcheck::FrameClock::default(),
            since: std::time::Instant::now(),
            fw: (!cfg!(windows)).then(|| {
                (
                    crate::hoststatus::Fw::NotApplicable,
                    crate::firewall::Verdict::Unknown("not Windows".into()),
                )
            }),
            router,
            upnp_on,
            nat_on: self.natcheck_enabled(),
            ask,
            force: false,
        }
    }

    /// Prints the status (and asks) once the firewall has answered and the
    /// router has, or UPnP is off, or 4 s have passed (dec076).
    fn host_report_poll(&mut self) {
        let Some(r) = self.host_report.as_mut() else {
            return;
        };
        let waited = r.clock.tick(std::time::Instant::now());
        if !crate::hoststatus::ready(
            r.fw.is_some(),
            r.router.is_some(),
            r.upnp_on,
            !r.nat_on || self.nat_answers.is_some(),
            waited,
            r.since.elapsed().as_secs_f64(),
        ) {
            return;
        }
        let Some(r) = self.host_report.take() else {
            return;
        };
        // the firewall check is slow or stuck: the status and the window do not wait for it
        let (fw, verdict) = match r.fw {
            Some(x) => x,
            None => {
                self.host_note(crate::hoststatus::say::still_checking(crate::hoststatus::FW_WAIT_SECS));
                (crate::hoststatus::Fw::Checking, crate::firewall::Verdict::Unknown("still checking".into()))
            }
        };
        self.fw_late = None;
        let router = r.router.unwrap_or(crate::hoststatus::Router::NoAnswer);
        let main = crate::upnp::default_route_ip();
        // the network check (dec076): its answers with what the router said
        let (nat, asked) = match (&self.nat_answers, r.nat_on) {
            (Some((mapped, other, asked)), true) => (
                Some(crate::natcheck::classify(main, mapped, &crate::hoststatus::router_facts(&router), *other)),
                Some(asked.clone()),
            ),
            _ => (None, None),
        };
        let v6 = self.server_v6();
        let st = crate::hoststatus::Status {
            map: r.map,
            port: r.port,
            fw,
            router,
            addrs: crate::hoststatus::order_addrs(&trem_sim::kernel::netchan::local_ipv4(), main),
            nat,
            v6: crate::hoststatus::v6_join(v6.map(|v| v.0), v6.map(|v| v.1).unwrap_or(r.port), self.port_last.as_ref()),
            asked,
        };
        for l in st.console() {
            self.host_say(l);
        }
        // the window on every host (once per hosted server) unless "Don't
        // ask again when I host"; `hosting_status` shows it anyway
        let open = self.firewall_choice.is_some();
        let setting = self.client.cvars.integer("net_hostWindow") as i64;
        let exe = std::env::current_exe()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        let ask = if r.force {
            cfg!(windows) && !open && self.firewall_fixing.is_none()
        } else {
            crate::firewall::should_ask(
                &verdict,
                setting,
                self.firewall_asked || open,
                self.firewall_fixing.is_some(),
                r.ask,
                cfg!(windows),
            )
        };
        if ask {
            self.firewall_asked = true;
            self.host_event(format!("status shown; opening the hosting window (net_hostWindow {setting}{})", if r.force { ", asked for with hosting_status" } else { "" }));
            self.firewall_ask(st.status_prompt(&exe, setting == 0));
        } else if cfg!(windows) && r.ask && setting == 0 && !open {
            self.host_note(crate::hoststatus::say::window_off());
        } else if let (true, Some(rf)) = (open, &self.firewall_refresh) {
            // the window is open already: the new facts into it
            let p = st.status_prompt(&exe, setting == 0);
            rf.set(&p.main, &p.content);
        }
        self.last_status = Some(st);
        self.write_hosting_file();
    }

    /// dec047: asks Windows (on a thread) whether other PCs can reach the hosted game.
    fn firewall_start(&mut self) {
        if !cfg!(windows) || self.firewall_pending.is_some() {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(crate::firewall::check());
        });
        self.firewall_pending = Some(rx);
        self.fw_started = Some(std::time::Instant::now());
        self.host_event("firewall check started".into());
    }

    /// dec076: the startup check is silent (a line in the log and console,
    /// no dialog). The question is asked when the player hosts a network
    /// game (`firewall_host_check`): the moment friends need the rule.
    /// net_hostWindow 0 (archived, default 1) means "Don't ask when I host".
    /// The older net_firewallPrompt is not read (a stale 0 must not silence it).
    fn firewall_frame(&mut self) {
        if !std::mem::replace(&mut self.firewall_started, true) {
            self.client
                .cvars
                .get("net_hostWindow", "1", crate::cmd::CVAR_ARCHIVE);
            // autogen.cfg is read before this cvar exists: a net_* `seta` then lands in the host's
            // list, not the client's. Move the saved value over, so "Don't ask again" keeps across restarts.
            if let Some(v) = self.take_host_cvar("net_hostWindow") {
                self.client.cvars.set("net_hostWindow", &v);
            }
            self.firewall_start();
        }
        if self.nethost.is_none() {
            // net_process polls while a network game runs
            self.firewall_poll();
        }
        if let Some(rx) = self.firewall_choice.as_ref() {
            if let Ok(a) = rx.try_recv() {
                self.firewall_choice = None;
                // the native dialog is closed (Godot's fallback cannot be redrawn)
                self.firewall_refresh = None;
                self.dialog_godot = false;
                let a = match a {
                    Ok(a) => a,
                    Err(reason) => {
                        // the native dialog could not be shown: Godot's own, same question
                        self.host_note(crate::hoststatus::say::taskdialog_failed(&reason));
                        // Godot's dialog blocks until it is closed: the file is written first
                        self.write_hosting_file();
                        if let Some(p) = self.firewall_prompt.take() {
                            self.firewall_ask_godot(&p);
                        }
                        return;
                    }
                };
                self.firewall_prompt = None;
                let (a, never) = a;
                self.host_event(format!("hosting window answered: choice {a}, box {}", if never { "ticked" } else { "not ticked" }));
                let was_never = self.client.cvars.integer("net_hostWindow") == 0;
                if never || a == crate::firewall::ANSWER_NEVER {
                    if !was_never {
                        self.client.cvars.set("net_hostWindow", "0");
                        self.host_say("Hosting status: not shown when you host (net_hostWindow 0). Type hosting_status to see it again, and untick the box there to have it back.".into());
                    }
                } else if was_never {
                    // hosting_status opened it with the box ticked, and the player unticked it
                    self.client.cvars.set("net_hostWindow", "1");
                    self.host_say("Hosting status: shown again each time you host (net_hostWindow 1).".into());
                }
                match a {
                    crate::firewall::ANSWER_ALLOW => {
                        let out = self.firewall_allow(false);
                        self.host_say(out.trim_end().to_string());
                    }
                    crate::firewall::ANSWER_LAN => {
                        self.host_lan_release();
                        let out = self.firewall_allow(true);
                        self.host_say(out.trim_end().to_string());
                    }
                    _ => {}
                }
            }
        }
        if let Some(rx) = self.firewall_fixing.as_ref() {
            if let Ok(r) = rx.try_recv() {
                self.firewall_fixing = None;
                match r {
                    Ok(()) => {
                        self.host_say(
                            "Windows Firewall: the rule was added; checking again".into(),
                        );
                        self.host_report = Some(self.new_host_report(false));
                        self.firewall_start();
                    }
                    Err(e) => self.host_say(format!("Windows Firewall: {e}")),
                }
            }
        }
    }

    /// dec076: a network game was started: check the firewall and, when other
    /// PCs cannot reach it, ask (once a run, unless the player said never).
    fn firewall_host_check(&mut self) {
        self.client
            .cvars
            .get("net_hostWindow", "1", crate::cmd::CVAR_ARCHIVE);
        self.firewall_start();
    }

    /// "Host for my home network only" (dec076): the router's port is
    /// closed again now, and not opened again until the server stops.
    fn host_lan_release(&mut self) {
        self.host_lan_only = true;
        self.upnp_pending = None;
        for k in std::mem::take(&mut self.port_keepers) {
            k.stop(false);
        }
        self.port_events = None;
        self.port_last = None;
        self.router_last = Some(crate::hoststatus::Router::Released);
        self.host_say("Router: closed UDP port again: you host for your home network only (LAN).".into());
    }

    /// `firewall_allow` (`lan`: `firewall_allow lan`): Windows' administrator
    /// prompt (on a thread: it waits for the player) adds the inbound rule
    /// for the networks found lacking one, replacing a block rule and the
    /// game's own stale rules, in one prompt (dec076).
    fn firewall_allow(&mut self, lan: bool) -> String {
        if self.firewall_fixing.is_some() {
            return "firewall_allow: already waiting for the administrator prompt\n".into();
        }
        // the rule covers the networks this PC is on now (the code decides which; the player is not asked)
        let mut prof = self.firewall_missing.clone();
        if prof.is_empty() {
            prof = vec!["Private".into()];
        }
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(crate::firewall::request_allow(&prof, lan));
        });
        self.firewall_fixing = Some(rx);
        if lan {
            "Asked Windows to allow players on your network in: approve the administrator prompt\n".into()
        } else {
            "Asked Windows to allow this game in: approve the administrator prompt\n".into()
        }
    }

    /// dec076: the question as a native Windows TaskDialog on a thread (the
    /// game keeps running; the answer comes back through a channel, no
    /// re-entry into TremGame). A `None` answer means the dialog could not
    /// be shown: `firewall_frame` then uses Godot's own dialog.
    fn firewall_ask(&mut self, p: crate::firewall::Prompt) {
        let (tx, rx) = std::sync::mpsc::channel::<Result<(i64, bool), String>>();
        let q = p.clone();
        let refresh = crate::taskdialog::Refresh::default();
        let r2 = refresh.clone();
        std::thread::spawn(move || {
            let _ = tx.send(crate::taskdialog::show(&q, crate::firewall::ANSWER_HOST, &r2));
        });
        self.firewall_refresh = Some(refresh);
        self.firewall_choice = Some(rx);
        self.firewall_prompt = Some(p);
        self.dialog_godot = false;
    }

    /// The fallback: Godot's DisplayServer.dialog_show, plain text, the same
    /// answers (closing it means Host without it).
    fn firewall_ask_godot(&mut self, p: &crate::firewall::Prompt) {
        let (tx, rx) = std::sync::mpsc::channel::<Result<(i64, bool), String>>();
        let plain_buttons = p.plain_buttons();
        let ids: Vec<i64> = plain_buttons.iter().map(|b| b.0).collect();
        let cb = Callable::from_fn("firewall_choice", move |args: &[&Variant]| {
            let i = args
                .first()
                .and_then(|v| v.try_to::<i64>().ok())
                .unwrap_or(-1);
            let id = ids.get(i as usize).copied().unwrap_or(crate::firewall::ANSWER_HOST);
            let _ = tx.send(Ok((id, false)));
        });
        let buttons: PackedStringArray = plain_buttons
            .iter()
            .map(|b| GString::from(b.1.as_str()))
            .collect();
        let e = godot::classes::DisplayServer::singleton().dialog_show(
            p.title.as_str(),
            p.plain().as_str(),
            &buttons,
            &cb,
        );
        if e == godot::global::Error::OK {
            self.firewall_choice = Some(rx);
            self.dialog_godot = true;
            self.host_event("the plain hosting dialog is open".into());
        } else {
            self.host_note(crate::hoststatus::say::godot_dialog_failed(&format!("{e:?}")));
        }
    }

    fn firewall_poll(&mut self) {
        let Some(rx) = self.firewall_pending.as_ref() else { return };
        let Ok(v) = rx.try_recv() else { return };
        self.firewall_pending = None;
        self.firewall_missing = crate::firewall::missing(&v);
        // the startup check is silent (engine log only); hosting prints the status
        godot_print!("Windows Firewall check: {v:?}");
        let took = self.fw_started.map(|t| t.elapsed().as_secs_f64()).unwrap_or(0.0);
        self.fw_last = Some(format!("answered after {took:.1} s: {v:?}"));
        self.host_event(format!("firewall check answered after {took:.1} s: {v:?}"));
        if let crate::firewall::Verdict::Unknown(why) = &v {
            if cfg!(windows) && self.nethost.as_ref().is_some_and(|n| n.server_running() && !n.loopback_only) {
                self.host_note(crate::hoststatus::say::check_failed(why));
            }
        }
        if let Some(r) = self.host_report.as_mut() {
            if r.fw.is_none() {
                r.fw = Some((crate::hoststatus::Fw::from_verdict(&v), v));
            }
        } else if self.last_status.as_ref().is_some_and(|s| s.fw == crate::hoststatus::Fw::Checking) {
            // the status went out without it (8 s): the answer into the console and the open window
            let lacks = crate::hoststatus::Fw::from_verdict(&v).lacks();
            self.fw_late = Some((crate::hoststatus::Fw::from_verdict(&v), v));
            self.status_refresh();
            if lacks {
                self.host_say("Type firewall_allow to open it (Windows asks for permission), or hosting_status for the window with the buttons.".into());
            }
            self.write_hosting_file();
        }
    }

    /// A line for the host: the engine log and the game console.
    fn host_say(&mut self, l: String) {
        godot_print!("{}", strip_colors(&l));
        self.misc.con.print(&format!("{l}\n"));
    }

    /// The server stops: the router's forwarding is taken back (on a
    /// thread; `wait` at exit, so it is sent before the process ends).
    fn upnp_release(&mut self, wait: bool) {
        self.upnp_pending = None;
        self.host_lan_only = false;
        // PCP / NAT-PMP: deleted with lifetime 0, UPnP with DeletePortMapping, by their renewal threads
        for k in std::mem::take(&mut self.port_keepers) {
            k.stop(wait);
        }
        self.port_events = None;
        self.port_last = None;
        self.nat_probe = None;
        self.nat_answers = None;
        // the next server shows the hosting window again
        self.firewall_asked = false;
    }

    /// `hosting_status`: checks the router (asks it again when nothing is
    /// open), the network (STUN, net_natCheck) and this PC's firewall again,
    /// prints the status and, on Windows, shows the window even with "Don't
    /// ask again when I host" (net_hostWindow 0; the box then starts
    /// ticked, unticking it turns the window back on).
    fn hosting_status(&mut self) -> String {
        let hosting = self.nethost.as_ref().is_some_and(|n| n.server_running() && !n.loopback_only);
        let mut out;
        if !hosting {
            let setting = self.client.cvars.integer("net_hostWindow");
            out = format!(
                "hosting_status: host a network game first; the router, the network and this PC's firewall are checked for the server. \
                 The window when you host is {} (net_hostWindow {setting}).\n",
                if setting == 0 { "off" } else { "on" }
            );
        } else if self.host_report.is_some() {
            // a check is running: say what is known and that the rest is still being checked; the window opens when it ends
            if let Some(r) = self.host_report.as_mut() {
                r.force = true;
            }
            let fw_secs = self.fw_started.filter(|_| self.firewall_pending.is_some()).map(|t| t.elapsed().as_secs_f64());
            let r = self.host_report.as_ref().unwrap();
            let lines = crate::hoststatus::known_lines(
                r.fw.as_ref().map(|f| f.0.word().to_string()),
                fw_secs,
                r.router.as_ref().map(|_| "answered".to_string()),
                self.nat_answers.as_ref().map(|_| "answered".to_string()).or_else(|| (!r.nat_on).then(|| "off (net_natCheck 0)".to_string())),
            );
            out = String::from("Hosting status: still checking; what is known so far:\n");
            for l in lines {
                out.push_str(&format!("  {l}\n"));
            }
            out.push_str(&format!("The window opens when the checks end, after {:.0} s at the most.\n", crate::hoststatus::FW_WAIT_SECS));
            self.host_event("hosting_status typed while a check was running".into());
        } else {
            self.host_event("hosting_status typed: checking everything again".into());
            self.nat_probe = None;
            self.nat_answers = None;
            self.natcheck_start();
            self.upnp_start();
            let mut r = self.new_host_report(true);
            r.force = true;
            self.host_report = Some(r);
            self.firewall_start();
            out = "Hosting status: checking the router, the network (STUN) and this PC's firewall again...\n".into();
        }
        let path = self.write_hosting_file();
        out.push_str(&format!("The facts are also written to {path}\n"));
        out
    }

    /// The facts behind `hosting_status` (file text).
    fn debug_facts(&self) -> crate::hoststatus::DebugFacts {
        let st = self.last_status.as_ref();
        crate::hoststatus::DebugFacts {
            host_window: self.client.cvars.find("net_hostWindow").map(|v| v.string.clone()).unwrap_or_else(|| "1".into()),
            old_prompt: self.net_cvar("net_firewallPrompt"),
            windows: cfg!(windows),
            hosting: self.nethost.as_ref().is_some_and(|n| n.server_running() && !n.loopback_only),
            firewall: match (&self.firewall_pending, self.fw_started, &self.fw_last) {
                (Some(_), Some(t), _) => format!("running for {:.0} s (stopped after {} s)", t.elapsed().as_secs_f64(), crate::firewall::CHECK_TIMEOUT_SECS),
                (_, _, Some(l)) => l.clone(),
                _ => "not started".into(),
            },
            router: match (st, &self.upnp_pending) {
                (Some(s), _) => s.router_line().trim_start_matches("Router: ").to_string(),
                (None, Some(_)) => "asking".into(),
                _ => "no answer yet".into(),
            },
            network: match (st.and_then(|s| s.nat.as_ref()), &self.nat_probe) {
                (Some(n), _) => st.map(|s| n.label(s.port)).unwrap_or_default(),
                (None, Some(_)) => "asking the STUN servers".into(),
                _ => "no result".into(),
            },
            collecting: self.host_report.is_some(),
            dialog: match (self.firewall_choice.is_some(), self.dialog_godot) {
                (true, true) => "plain dialog open".into(),
                (true, false) => "Windows dialog open".into(),
                _ => "none open".into(),
            },
            events: self.host_events.clone(),
            status: st.map(|s| s.console().iter().map(|l| strip_colors(l)).collect()).unwrap_or_default(),
        }
    }

    /// Writes user://hosting_status.txt (a tester can read it when the
    /// console font is too small); returns the full path, or why it failed.
    fn write_hosting_file(&mut self) -> String {
        let text = crate::hoststatus::debug_text(&self.debug_facts());
        let mut path = godot::classes::ProjectSettings::singleton().globalize_path("user://hosting_status.txt").to_string();
        if cfg!(windows) {
            path = path.replace('/', "\\");
        }
        match std::fs::write(&path, text) {
            Ok(()) => path,
            Err(e) => {
                let l = format!("could not write {path}: {e}");
                godot_print!("{l}");
                l
            }
        }
    }

    /// A step of the hosting story: the engine log and hosting_status.txt.
    fn host_event(&mut self, l: String) {
        godot_print!("Hosting: {l}");
        let t = self.host_t0.map(|t| t.elapsed().as_secs_f64()).unwrap_or(0.0);
        self.host_events.push(format!("+{t:.1} s {}", strip_colors(&l)));
        if self.host_events.len() > 40 {
            self.host_events.remove(0);
        }
    }

    /// The same, and a line in the console: a skipped or failed step the
    /// player should read.
    fn host_note(&mut self, l: String) {
        self.host_event(l.clone());
        self.misc.con.print(&format!("{l}\n"));
    }

    fn net_cvar(&self, name: &str) -> Option<String> {
        self.cvars.iter().rev().find(|(k_, _)| k_.eq_ignore_ascii_case(name)).map(|(_, v)| v.clone()).or_else(|| self.client.cvars.find(name).map(|v| v.string.clone()))
    }

    /// `map` starts a listen server: one runs already, or net_enabled is set
    /// (the createserver menu sets it; the single-process game is the default).
    fn wants_listen_server(&self) -> bool {
        self.nethost.as_ref().is_some_and(|n| n.server_running()) || self.net_cvar("net_enabled").is_some_and(|v| trem_sim::kernel::spawn::c_atoi(&v) != 0) || self.wants_loopback_server()
    }

    /// The server engine's own cvars (sv_*, dedicated, net_*, rconPassword).
    fn is_net_server_cvar(&self, name: &str) -> bool {
        let l = name.to_ascii_lowercase();
        l.starts_with("sv_") || l.starts_with("net_") || l == "dedicated" || l == "rconpassword"
    }

    /// The running server picks up changed sv_* cvars.
    fn update_server_cvars(&mut self) {
        let cheats = self.nethost.as_ref().and_then(|n| n.server.as_ref()).map(|s| s.cvars.cheats);
        let Some(cheats) = cheats else { return };
        let mut sc = self.server_cvars();
        sc.cheats = cheats;
        if let Some(sv) = self.nethost.as_mut().and_then(|n| n.server.as_mut()) {
            sv.cvars = sc;
        }
    }

    fn server_cvars(&self) -> trem_sim::kernel::sv::ServerCvars {
        let get = |n: &str| self.net_cvar(n);
        trem_sim::kernel::nethost::server_cvars(&get)
    }

    /// `NET_Init` / `CL_Init`'s network part on first use: the client
    /// cvars, userinfo, and the qkey for cl_guid (`CL_GenerateQKey`).
    fn ensure_nethost(&mut self) {
        if self.nethost.is_some() {
            return;
        }
        use crate::cmd::{CVAR_ARCHIVE, CVAR_USERINFO};
        self.wtb_register_cvars(); // wt-b: cl_voip* and cl_voipProtocol (userinfo)
        // cl_main.c / cg_main.c CVAR_USERINFO cvars
        let ui: &[(&str, &str, i32)] = &[
            ("name", "Player", CVAR_USERINFO | CVAR_ARCHIVE),
            ("rate", "90000", CVAR_USERINFO | CVAR_ARCHIVE),
            ("snaps", "40", CVAR_USERINFO | CVAR_ARCHIVE),
            ("color1", "4", CVAR_USERINFO | CVAR_ARCHIVE),
            ("color2", "5", CVAR_USERINFO | CVAR_ARCHIVE),
            ("handicap", "100", CVAR_USERINFO | CVAR_ARCHIVE),
            ("sex", "male", CVAR_USERINFO | CVAR_ARCHIVE),
            ("password", "", CVAR_USERINFO),
            ("teamoverlay", "1", CVAR_USERINFO | CVAR_ARCHIVE),
            ("cg_wwFollow", "1", CVAR_USERINFO | CVAR_ARCHIVE),
            ("cg_wwToggle", "1", CVAR_USERINFO | CVAR_ARCHIVE),
            ("cg_disableBlueprintErrors", "0", CVAR_USERINFO | CVAR_ARCHIVE),
            ("cg_stickySpec", "1", CVAR_USERINFO | CVAR_ARCHIVE),
            ("cg_sprintToggle", "0", CVAR_USERINFO | CVAR_ARCHIVE),
            ("cg_unlagged", "1", CVAR_USERINFO | CVAR_ARCHIVE),
            ("cg_flySpeed", "600", CVAR_USERINFO | CVAR_ARCHIVE),
            ("cg_smoothClients", "0", CVAR_USERINFO | CVAR_ARCHIVE),
            ("voice", "default", CVAR_USERINFO | CVAR_ARCHIVE),
            ("cl_maxpackets", "125", CVAR_ARCHIVE),
            ("cl_packetdup", "1", CVAR_ARCHIVE),
            ("cl_maxPing", "800", CVAR_ARCHIVE),
            ("cl_guidServerUniq", "1", CVAR_ARCHIVE),
            ("cl_lanForcePackets", "1", CVAR_ARCHIVE),
        ];
        for (n, v, f) in ui {
            self.client.cvars.get(n, v, *f);
        }
        let mut nh = Box::new(trem_sim::kernel::nethost::NetHost::new());
        // QKEY_FILE in the home path (user://qkey): 2048 random bytes
        let path = godot::classes::ProjectSettings::singleton().globalize_path("user://qkey").to_string();
        let mut q = std::fs::read(&path).unwrap_or_default();
        if q.len() != trem_sim::kernel::cl::QKEY_SIZE {
            godot_print!("QKEY building random string");
            let mut rng = godot::classes::RandomNumberGenerator::new_gd();
            rng.randomize();
            q = (0..trem_sim::kernel::cl::QKEY_SIZE).map(|_| (rng.randi() & 0xff) as u8).collect();
            if std::fs::write(&path, &q).is_ok() {
                godot_print!("QKEY generated");
            } else {
                godot_print!("QKEY could not open {path} for write");
            }
        } else {
            godot_print!("QKEY found.");
        }
        nh.cl.qkey = Some(q);
        self.nethost = Some(nh);
        self.netext_setup(); // wt-a (dec037)
        self.sync_net_cvars(true);
    }

    /// The client cvars and the userinfo into the network client
    /// (`CL_CheckUserinfo`'s "userinfo" command when they change).
    fn sync_net_cvars(&mut self, force: bool) {
        use crate::cmd::CVAR_USERINFO;
        let changed = force || self.client.cvars.modified_flags & CVAR_USERINFO != 0;
        self.client.cvars.modified_flags &= !CVAR_USERINFO;
        let info = self.client.cvars.info_string(CVAR_USERINFO);
        let Some(mut nh) = self.nethost.take() else { return };
        {
            let get = |n: &str| self.net_cvar(n);
            trem_sim::kernel::nethost::apply_client_cvars(&mut nh.cl, &get);
        }
        self.nethost = Some(nh);
        let nh = self.nethost.as_mut().unwrap();
        if changed {
            let guid = trem_sim::kernel::svgame::info_value(&nh.cl.userinfo, "cl_guid");
            let mut info = info;
            trem_sim::kernel::svgame::info_set(&mut info, "cl_guid", &guid);
            if info != nh.cl.userinfo {
                nh.cl.userinfo = info;
                nh.cl.userinfo_modified = true;
            }
        }
        self.netext_sync(); // wt-a (dec037)
    }

    // (`CL_Connect_f` is netext_connect, with its -4 / -6 option; wt-a dec037)

    /// The engine's network console commands; None for other commands.
    fn net_engine_command(&mut self, line: &str) -> Option<String> {
        let argv = svcmds::tokenize(line);
        let c = argv.first()?.to_ascii_lowercase();
        // wt-a (dec037): record, stoprecord, demo
        if let Some(out) = self.netext_command(&argv) {
            return Some(out);
        }
        match c.as_str() {
            "connect" => {
                if argv.len() < 2 {
                    return Some("usage: connect [-4|-6] server [-1|-69|-71]\n".into());
                }
                Some(self.netext_connect(&argv[1..]))
            }
            "firewall_allow" => {
                // dec047: Windows' administrator prompt adds the inbound rule;
                // dec076: "firewall_allow lan" for the home network only
                let lan = argv.get(1).is_some_and(|a| a.eq_ignore_ascii_case("lan"));
                Some(self.firewall_allow(lan))
            }
            // the hosting window again, at any time while hosting (and the way back from "Don't ask again")
            "hosting_status" => Some(self.hosting_status()),
            "reconnect" => {
                // CL_Reconnect_f: connect cl_reconnectArgs
                let args = self.nethost.as_ref().map(|n| n.cl.reconnect_args.clone()).unwrap_or_default();
                if args.is_empty() {
                    return Some(String::new());
                }
                Some(self.netext_connect(&svcmds::tokenize(&args)))
            }
            "localservers" | "globalservers" | "ping" | "serverstatus" | "heartbeat" | "serverinfo" | "systeminfo" | "killserver" => {
                self.ensure_nethost();
                self.sync_net_cvars(false);
                let nh = self.nethost.as_mut().unwrap();
                let was_listen = nh.server_running();
                let mut out = nh.command(&argv).unwrap_or_default();
                out.push_str(&std::mem::take(&mut nh.out));
                if c == "killserver" && was_listen {
                    self.shutdown_map();
                }
                Some(out)
            }
            "rcon" => {
                // over the network when connected or rconAddress is set; else
                // (no network session) the listen server's console runs it
                let remote = self.nethost.as_ref().is_some_and(|n| n.client_connected()) || self.net_cvar("rconAddress").is_some_and(|a| !a.is_empty());
                if !remote {
                    return None;
                }
                self.ensure_nethost();
                self.sync_net_cvars(false);
                let nh = self.nethost.as_mut().unwrap();
                let mut out = nh.command(&argv).unwrap_or_default();
                out.push_str(&std::mem::take(&mut nh.out));
                Some(out)
            }
            _ => None,
        }
    }

    /// The ui's `trap_LAN_*` lists (menus.rs `Lan`) in step with the
    /// network client's: favorites and visible flags go to the client, the
    /// pings the ui asked for are sent (`CL_UpdateVisiblePings_f`), the
    /// local / global lists and the answers come back.
    pub(crate) fn lan_sync(&mut self, lan: &mut crate::menus::Lan) {
        use crate::menus::ServerInfo as UiServer;
        use trem_sim::kernel::cl::ServerInfo as ClServer;
        use trem_sim::kernel::netchan::{compare_adr, string_to_adr, NetAdrType};
        let Some(nh) = self.nethost.as_mut() else {
            lan.net = false;
            return;
        };
        lan.net = true;
        // favorites: the ui's list is the one saved (resolved when it changes)
        let addrs: Vec<String> = lan.favorites.iter().map(|f| f.addr.clone()).collect();
        if addrs != self.lan_fav_addrs {
        self.lan_fav_addrs = addrs;
        let favs: Vec<ClServer> = lan
            .favorites
            .iter()
            .map(|f| {
                let (r, mut adr) = string_to_adr(&f.addr, NetAdrType::Unspec);
                if r == 0 {
                    // an unresolvable favorite stays in its slot, never answering
                    return ClServer { ping: 0, ..Default::default() };
                }
                if r == 2 {
                    adr.port = trem_sim::kernel::netchan::PORT_SERVER;
                }
                let old = nh.cl.favorite_servers.iter().find(|s| compare_adr(&s.adr, &adr)).cloned();
                old.unwrap_or(ClServer { adr, ping: -1, ..Default::default() })
            })
            .collect();
        nh.cl.favorite_servers = favs;
        }
        let vis = |ui: &[UiServer], cl: &mut [ClServer]| {
            for c in cl.iter_mut() {
                let a = c.adr.to_string_port();
                if let Some(u) = ui.iter().find(|u| u.addr == a) {
                    c.visible = u.visible;
                }
            }
        };
        vis(&lan.local, &mut nh.cl.local_servers);
        vis(&lan.global, &mut nh.cl.global_servers);
        for (c, u) in nh.cl.favorite_servers.iter_mut().zip(lan.favorites.iter()) {
            c.visible = u.visible;
        }
        if let Some(src) = lan.want_pings.take() {
            nh.cl.update_visible_pings(&mut nh.net, src);
        }
        let (vis_local, vis_global) = (lan.new_slot_visible(trem_sim::kernel::cl::AS_LOCAL), lan.new_slot_visible(trem_sim::kernel::cl::AS_GLOBAL));
        let to_ui = |c: &ClServer, old: &[UiServer], slot: Option<bool>| {
            let addr = c.adr.to_string_port();
            let visible = old.iter().find(|u| u.addr == addr).map(|u| u.visible).unwrap_or(slot.unwrap_or(c.visible));
            UiServer { addr, host_name: c.host_name.clone(), map_name: c.map_name.clone(), game: c.game.clone(), clients: c.clients, max_clients: c.max_clients, ping: c.ping, visible, protocol: c.protocol }
        };
        lan.local = nh.cl.local_servers.iter().map(|c| to_ui(c, &lan.local, vis_local)).collect();
        lan.global = nh.cl.global_servers.iter().map(|c| to_ui(c, &lan.global, vis_global)).collect();
        lan.global_count = if nh.cl.num_global_servers < 0 { -1 } else { lan.global.len() as i32 };
        for (u, c) in lan.favorites.iter_mut().zip(nh.cl.favorite_servers.iter()) {
            let name = if c.host_name.is_empty() { u.host_name.clone() } else { c.host_name.clone() };
            *u = UiServer { host_name: name, addr: u.addr.clone(), ..to_ui(c, std::slice::from_ref(u), None) };
        }
        lan.outstanding = nh.cl.pinglist.iter().filter(|p| p.adr.port != 0).count();
    }

    /// The connection failed or the server dropped us (`Com_Error`): the
    /// text for the ui's error popup.
    pub(crate) fn take_net_error(&mut self) -> Option<String> {
        self.net_error.take()
    }

    /// `UI_DrawConnectScreen`'s lines while connecting (shown on the
    /// loading screen): the server, the connection state, its message.
    fn update_connect_screen(&mut self) {
        use trem_sim::kernel::cl::ConnState;
        let Some(nh) = self.nethost.as_ref() else {
            self.connect_screen = None;
            return;
        };
        let st = nh.cl.state;
        if !(ConnState::Connecting..ConnState::Active).contains(&st) || self.loading.is_some() || (self.world.is_some() && st >= ConnState::Loading) {
            self.connect_screen = None;
            return;
        }
        let si = &nh.cl.engine_cs(trem_sim::kernel::svgame::cs::SERVERINFO);
        let mut media = 0.0;
        let mut motd = nh.cl.server_message.clone();
        let mut message = match st {
            ConnState::Connecting => format!("Awaiting connection...{}", nh.cl.connect_packet_count),
            ConnState::Challenging => format!("Awaiting challenge...{}", nh.cl.connect_packet_count),
            ConnState::Connected => match self.download_info() {
                Some((s, f)) => {
                    media = f;
                    s
                }
                None => "Awaiting gamestate...".to_string(),
            },
            _ => String::new(),
        };
        // the level name box holds one line: the download's bytes, rate,
        // time left and queue go in the (multi-line) MOTD box under it
        if let Some((first, rest)) = message.split_once('\n') {
            motd = rest.to_string();
            message = first.to_string();
        }
        let hostname = trem_sim::kernel::svgame::info_value(si, "sv_hostname");
        self.connect_screen = Some(LoadInfo {
            mapname: trem_sim::kernel::svgame::info_value(si, "mapname"),
            message,
            hostname: if hostname.is_empty() { format!("Connecting to {}", nh.cl.servername) } else { hostname },
            motd,
            media,
            ..Default::default()
        });
    }

    /// `CL_InitCGame` for a server's map: load its view World (with the
    /// loading screen), or the sheet map at once.
    fn begin_client_load(&mut self, map: &str) {
        let paths = self.client_load_paths(); // wt-a (dec037): a pure server's paks
        let mut set = crate::assets::Pk3Set::open(&paths);
        let data = set.read(&format!("maps/{map}.bsp"));
        let si = self.nethost.as_ref().map(|n| n.cl.configstrings.clone()).unwrap_or_default();
        let cs = |i: usize| si.get(i).cloned().unwrap_or_default();
        let info = LoadInfo {
            mapname: map.to_string(),
            message: cs(trem_sim::kernel::svgame::cs::MESSAGE),
            hostname: trem_sim::kernel::svgame::info_value(&cs(trem_sim::kernel::svgame::cs::SERVERINFO), "sv_hostname"),
            motd: cs(trem_sim::kernel::svgame::cs::MOTD),
            ..Default::default()
        };
        if let Some(data) = data {
            match trem_sim::kernel::bsp::parse(data.as_slice()) {
                Ok(bsp) => {
                    self.loading = Some(Box::new(MapLoad {
                        paths,
                        name: map.to_string(),
                        set,
                        bsp,
                        step: 0,
                        info,
                        world: None,
                        sessions: None,
                        render: None,
                        meshes: None,
                        rt_geom: None,
                        acoustic: None,
                        fogs: Vec::new(),
                        brush: HashMap::new(),
                        sky: None,
                        sky_hd: None,
                        grid: None,
                        shadow_dirs: None,
                        then: Vec::new(),
                        cheats: false,
                        mode: LoadMode::Client,
                    }));
                    self.connect_screen = None;
                }
                Err(e) => self.net_drop(format!("{map}: {e}")),
            }
            return;
        }
        // the built-in sheet maps (arena)
        if let Some(id) = MapId::ALL.iter().copied().find(|m| m.def().id.eq_ignore_ascii_case(map)) {
            self.clear_scene();
            self.current = CurrentMap::Sheet(id);
            self.start_presentation();
            self.world = Some(World::client_view(id));
            self.me = None;
            self.build_level(id, None);
            if let Some(fx) = self.effects.as_mut() {
                fx.set_mark_world(trem_sim::kernel::marks::MarkWorld::from_bsp(&trem_sim::kernel::bsp::from_sheet(id)));
            }
            self.net_view_loaded();
            return;
        }
        // CL_InitDownloads fetched the referenced paks (dec037); the map is
        // still not there (an unpure server, cl_allowDownload 0)
        self.net_drop(format!("Couldn't load maps/{map}.bsp"));
    }

    /// The view World is up: `CL_InitCGame` done, "we're ready".
    fn net_view_loaded(&mut self) {
        if let Some(nh) = self.nethost.as_mut() {
            nh.view_loaded();
        }
        // CL_ClearState: cl.viewangles start at zero (delta_angles do the rest)
        self.reset_view = Some([0.0; 3]);
        self.last_spawn = None;
        self.view_map = None;
    }

    /// `Com_Error( ERR_DROP )`: drop the session and the map; the ui shows the text.
    fn net_drop(&mut self, reason: String) {
        godot_print!("{reason}");
        self.loading = None;
        self.shutdown_map();
        self.net_error = Some(reason);
    }

    /// The end of a networked map load (listen server or client).
    fn finish_net_load(&mut self, mut ld: MapLoad) {
        self.current = CurrentMap::Bsp(ld.paths.clone(), ld.name.clone());
        let mut server_world = ld.world.take();
        if let Some(w) = server_world.as_mut() {
            // G_InitGame of the listen server's World (no local player:
            // the host's own client connects over the loopback)
            self.start_presentation();
            self.prepare_game_world(w, None, false);
            w.level.cheats = ld.cheats;
        } else {
            self.start_presentation();
        }
        let view = World::client_view_from_bsp(&ld.bsp, &ld.name);
        let map = view.map;
        self.world = Some(view);
        self.me = None;
        self.gfx.pending_geom = ld.rt_geom.take();
        self.gfx.pending_fogs = std::mem::take(&mut ld.fogs);
        self.set_acoustic(ld.acoustic.take(), &ld.bsp);
        self.build_level(map, ld.meshes.take());
        self.brush_meshes = std::mem::take(&mut ld.brush);
        self.lightgrid = ld.grid.take();
        self.shadow_dirs = ld.shadow_dirs.take();
        if let Some(sky) = ld.sky.take() {
            self.set_sky(sky, ld.sky_hd.take());
        }
        match server_world {
            Some(w) => {
                // SV_SpawnServer: the server takes the World, our client
                // connects to it (or gets the new gamestate)
                self.ensure_nethost();
                self.sync_net_cvars(true);
                let mut sc = self.server_cvars();
                sc.cheats = ld.cheats;
                let ip = self.net_cvar("net_ip").unwrap_or_default();
                let port = self.net_cvar("net_port").map(|p| trem_sim::kernel::spawn::c_atoi(&p) as u16).filter(|p| *p != 0).unwrap_or(trem_sim::kernel::netchan::PORT_SERVER);
                self.view_map = Some(ld.name.clone());
                // wt-a (dec037): net_enabled 0 (a recorded single-player game): loopback only
                let loopback_only = !self.net_cvar("net_enabled").is_some_and(|v| trem_sim::kernel::spawn::c_atoi(&v) != 0);
                let nh = self.nethost.as_mut().unwrap();
                nh.loopback_only = loopback_only;
                nh.spawn_server(w, &ld.name, sc, &ip, port, true);
                let out = std::mem::take(&mut nh.out);
                for l in out.lines() {
                    godot_print!("{l}");
                }
                if !loopback_only {
                    self.host_t0 = Some(std::time::Instant::now());
                    self.host_events.clear();
                    self.host_event(format!("hosting {} (network game)", ld.name));
                    // the status (join addresses, firewall, router) is printed once both have answered
                    self.natcheck_start();
                    self.host_report = Some(self.new_host_report(true));
                    self.upnp_start();
                    self.firewall_host_check();
                }
                for l in &ld.then {
                    self.server_command(l);
                }
            }
            None => self.net_view_loaded(),
        }
        self.update_connect_screen();
    }

    /// One frame of a network session (`Com_Frame`): the server, the
    /// client and its cgame into the view World at the 16 ms tick, then the
    /// same presentation as the single-process game.
    fn net_process(&mut self, delta: f64) {
        self.upnp_poll();
        self.natcheck_poll();
        self.port_events_poll();
        self.firewall_poll();
        self.host_report_poll();
        self.clock += delta;
        self.client_frame();
        self.sync_net_cvars(false);
        self.netext_frame(); // wt-a (dec037): the download prompt
        // CL_CreateCmd once per frame (cl_input.c, from CA_PRIMED), sent with every tick
        let primed = self.nethost.as_ref().and_then(|n| n.client_num()).is_some() && self.view_map.is_none();
        if self.world.is_some() && primed {
            let server_time = self.nethost.as_ref().map(|n| n.cl.server_time).unwrap_or(0);
            let t = godot::classes::Time::singleton().get_ticks_msec() as u32;
            let zoom = self.view_state_zoom_sensitivity();
            if let Some(v) = self.reset_view.take() {
                self.client.input.viewangles = v;
            }
            let mut cmd = self.client.create_cmd(t, zoom, server_time);
            self.held_buttons = self.client.input.held_buttons(cmd.buttons);
            cmd.buttons |= self.pending_buttons;
            self.pending_buttons = cmd.buttons & !self.held_buttons;
            self.net_cmd = Some(cmd);
        } else {
            self.net_cmd = None;
        }
        self.acc_ms += delta * 1000.0;
        let mut steps = 0;
        while self.acc_ms >= TICK_MS as f64 && steps < 8 {
            self.acc_ms -= TICK_MS as f64;
            steps += 1;
            let Some(mut nh) = self.nethost.take() else { return };
            if let Some(sv) = nh.server.as_mut() {
                sv.world.real_time = real_time();
            }
            let cmd = self.net_cmd;
            let evs = nh.frame(TICK_MS, self.world.as_mut(), cmd);
            let out = std::mem::take(&mut nh.out);
            self.nethost = Some(nh);
            self.wtb_voip_tick(); // wt-b: CL_CaptureVoip, CL_PlayVoip's raw streams
            for l in out.lines() {
                godot_print!("{l}");
            }
            // later ticks of the frame send only the buttons still held
            self.pending_buttons = 0;
            if let Some(c) = self.net_cmd.as_mut() {
                c.buttons = self.held_buttons;
            }
            for e in evs {
                match e {
                    trem_sim::kernel::nethost::HostEvent::LoadView { map } => {
                        if self.view_map.as_deref().is_some_and(|m| m.eq_ignore_ascii_case(&map)) && self.world.is_some() {
                            self.net_view_loaded();
                        } else {
                            self.begin_client_load(&map);
                            if self.loading.is_some() || self.world.is_none() {
                                return;
                            }
                        }
                    }
                    trem_sim::kernel::nethost::HostEvent::Dropped { reason } => {
                        self.net_drop(reason);
                        return;
                    }
                    trem_sim::kernel::nethost::HostEvent::Rcon { from, line } => {
                        let mut out = self.console_line(&line);
                        out.push('\n');
                        out.push_str(&self.run_exec_output());
                        if let Some(nh) = self.nethost.as_mut() {
                            nh.rcon_reply(&from, &out);
                        }
                    }
                    // wt-a (dec037): FS_Restart after a download, CL_DemoCompleted
                    e @ (trem_sim::kernel::nethost::HostEvent::FsRestart | trem_sim::kernel::nethost::HostEvent::DemoCompleted) => {
                        if self.netext_event(&e) {
                            return;
                        }
                    }
                }
            }
            if !self.net_active() {
                // killserver / disconnect from an rcon line
                return;
            }
            // the cgame runs from CA_ACTIVE (the first snapshot)
            let active = self.nethost.as_ref().is_some_and(|n| n.cl.state == trem_sim::kernel::cl::ConnState::Active);
            self.me = self.nethost.as_ref().and_then(|n| n.client_num()).filter(|_| active && self.world.is_some() && self.view_map.is_none());
            if self.me.is_some() {
                let evs = self.world.as_mut().map(|w| w.drain_events()).unwrap_or_default();
                self.transition_player_state();
                for e in evs {
                    self.on_event(&e);
                }
                // "chat" / "print" server commands (the view holds ours only)
                if let Some(w) = self.world.as_mut() {
                    for (_, _, _, line) in std::mem::take(&mut w.prints) {
                        self.feed.push((self.clock, strip_colors(&line)));
                    }
                    for (_, _, _, mode, line) in std::mem::take(&mut w.chat) {
                        let tag = match mode {
                            trem_sim::kernel::cmds::SayMode::All => "",
                            trem_sim::kernel::cmds::SayMode::Team => "(team) ",
                            trem_sim::kernel::cmds::SayMode::Area => "(area) ",
                            trem_sim::kernel::cmds::SayMode::Admins | trem_sim::kernel::cmds::SayMode::AdminsPublic | trem_sim::kernel::cmds::SayMode::Raw => "",
                            _ => "(private) ",
                        };
                        self.feed.push((self.clock, format!("{tag}{}", strip_colors(&line))));
                    }
                }
            }
            // the listen server's game: EXEC_APPEND lines, ExitLevel
            if self.nethost.as_ref().is_some_and(|n| n.server_running()) {
                if self.run_exec() {
                    return;
                }
                let exited = self.game_world().is_some_and(|w| w.level.exited);
                if exited {
                    let mut next = self.game_world().and_then(|w| w.level.next_map.clone());
                    let mut post = String::new();
                    if next.is_none() && self.rotations.active() {
                        if let Some(ch) = self.advance_rotation() {
                            next = Some(ch.map);
                            post = ch.post_command;
                        }
                    }
                    let paths = match &self.current {
                        CurrentMap::Bsp(p, _) => p.clone(),
                        _ => self.pk3_paths.clone(),
                    };
                    let name = next.or_else(|| match &self.current {
                        CurrentMap::Bsp(_, n) => Some(n.clone()),
                        _ => None,
                    });
                    let then = if post.is_empty() { Vec::new() } else { vec![post] };
                    if let Some(n) = name {
                        let err = self.begin_load(&paths, &n, then, false);
                        if !err.is_empty() {
                            godot_error!("{err}");
                        }
                    }
                    return;
                }
            }
        }
        if steps == 8 {
            self.acc_ms = 0.0;
        }
        self.update_connect_screen();
        if self.world.is_some() && self.me.is_some() {
            self.sync_visuals();
            self.sync_camera();
            self.sync_vision();
            self.update_fx(delta);
            self.update_effects();
        }
    }
}

/// `Com_StartupVariable` / `Com_AddStartupCommands`: the command line's
/// "+" commands after `--` except +map / +devmap (`startup_map`), as
/// console lines ("+set net_enabled 1", "+connect host:port").
/// The Quake-style command line: `+set ...` / `+map ...` after Godot's
/// `--` (`get_cmdline_user_args`), or straight on the game's command line
/// (a packaged game's shortcut: `Tremulous.exe +connect host`), where Godot
/// leaves the arguments it does not know in `get_cmdline_args`.
fn quake_args() -> Vec<String> {
    let os = godot::classes::Os::singleton();
    let user: Vec<String> = os.get_cmdline_user_args().as_slice().iter().map(|a| a.to_string()).collect();
    if user.iter().any(|a| a.starts_with('+')) {
        return user;
    }
    let all: Vec<String> = os.get_cmdline_args().as_slice().iter().map(|a| a.to_string()).collect();
    match all.iter().position(|a| a.starts_with('+')) {
        Some(i) => all[i..].to_vec(),
        None => user,
    }
}

fn startup_commands() -> Vec<String> {
    let args = quake_args();
    let mut cmds: Vec<Vec<String>> = Vec::new();
    for a in args {
        if let Some(c) = a.strip_prefix('+') {
            cmds.push(vec![c.to_string()]);
        } else if let Some(c) = cmds.last_mut() {
            c.push(a);
        }
    }
    cmds.into_iter()
        .filter(|c| !matches!(c[0].to_ascii_lowercase().as_str(), "map" | "devmap"))
        .map(|c| c.iter().map(|x| if x.contains(' ') || x.is_empty() { format!("\"{x}\"") } else { x.clone() }).collect::<Vec<_>>().join(" "))
        .collect()
}

/// Which entities get an original model (players, corpses, structures).
fn actor_key(w: &World, i: usize) -> Option<String> {
    let e = &w.ents[i];
    if !e.inuse || e.free_after_event {
        return None;
    }
    match e.etype {
        EType::Player if e.linked => {
            let c = &w.clients[i];
            if c.spectator != SpectatorState::Not as i32 {
                return None;
            }
            let st = &c.ps.stats;
            let has = |u: UpgradeId| bg::inventory_contains_upgrade(u.num() as i32, st) as u8;
            Some(format!("p{}:{}:{}{}{}{}", st[Stat::Class as usize], c.ps.weapon, has(UpgradeId::Larmour), has(UpgradeId::Helmet), has(UpgradeId::Jetpack), has(UpgradeId::Battpack)))
        }
        EType::Corpse => Some(format!("c{}", e.generic1)),
        EType::Buildable => Some(format!("b{:?}", e.buildable)),
        _ => None,
    }
}

/// The aliens' night vision (enhanced, port_vision): the view's light
/// amplified (dark corners come up, bright ones roll off), green, a little
/// grain and vignette; drawn over the 3D view only, so the HUD stays as is.
const NIGHT_VISION_SHADER: &str = r#"
shader_type canvas_item;
uniform sampler2D screen : hint_screen_texture, filter_linear;
void fragment() {
    vec3 c = texture(screen, SCREEN_UV).rgb;
    float l = dot(c, vec3(0.3, 0.59, 0.11));
    float g = 1.0 - exp(-l * 7.0);
    float n = fract(sin(dot(SCREEN_UV * vec2(12.9898, 78.233) + vec2(TIME * 3.1, TIME * 1.7), vec2(1.0, 1.0))) * 43758.5453);
    vec2 d = SCREEN_UV - 0.5;
    float vig = clamp(1.0 - dot(d, d) * 1.4, 0.0, 1.0);
    vec3 nv = vec3(0.3, 1.0, 0.4) * (g * 0.95 + 0.05) * vig + (n - 0.5) * 0.06;
    COLOR = vec4(nv, 1.0);
}
"#;

/// The pk3 search path: user://base, <executable>/base, res://base, --base=DIR.
/// `Com_AddStartupCommands`' map: `+map <name>` / `+devmap <name>` on the
/// command line (after `--`).
fn startup_map() -> Option<(String, String)> {
    let args = quake_args();
    args.windows(2).find_map(|w| {
        let c = w[0].to_ascii_lowercase();
        (c == "+map" || c == "+devmap").then(|| (c[1..].to_string(), w[1].clone()))
    })
}

pub fn find_pk3s() -> Vec<String> {
    use godot::classes::{DirAccess, Os, ProjectSettings};
    let os = Os::singleton();
    let mut dirs: Vec<String> = vec![
        ProjectSettings::singleton().globalize_path("user://base").to_string(),
        format!("{}/base", os.get_executable_path().to_string().rsplit_once(['/', '\\']).map(|x| x.0.to_string()).unwrap_or_default()),
        ProjectSettings::singleton().globalize_path("res://base").to_string(),
    ];
    // base/paths.txt: extra pk3 folders, one per line (e.g. an existing
    // Tremulous install's base folder), so the data needn't be copied
    let res_base = ProjectSettings::singleton().globalize_path("res://base").to_string();
    if let Ok(t) = std::fs::read_to_string(format!("{res_base}/paths.txt")) {
        for l in t.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
            dirs.push(l.replace('\\', "/"));
        }
    }
    for a in os.get_cmdline_args().as_slice().iter().chain(os.get_cmdline_user_args().as_slice().iter()) {
        if let Some(d) = a.to_string().strip_prefix("--base=") {
            dirs.push(d.to_string());
        }
    }
    let mut out: Vec<String> = Vec::new();
    // one folder once however it is spelt (an exported game's res://base
    // is the executable's base folder)
    let mut seen: Vec<String> = Vec::new();
    for d in dirs {
        let mut d = d.replace('\\', "/").trim_end_matches('/').to_string();
        // relative (an exported game's res://base is "base"): from the working folder
        if !d.is_empty() && std::path::Path::new(&d).is_relative() {
            if let Ok(cwd) = std::env::current_dir() {
                d = format!("{}/{d}", cwd.to_string_lossy().replace('\\', "/").trim_end_matches('/'));
            }
        }
        let key = d.to_lowercase();
        if seen.contains(&key) {
            continue;
        }
        seen.push(key);
        let Some(da) = DirAccess::open(&d) else { continue };
        for f in da.get_files().as_slice() {
            let f = f.to_string();
            if f.to_lowercase().ends_with(".pk3") {
                let full = format!("{d}/{f}");
                if !out.contains(&full) {
                    out.push(full);
                }
            }
        }
    }
    out
}

/// Model lighting: the map's light grid, else a fixed key light (tr.sunDirection-like).
fn light_at(grid: Option<&crate::bspview::LightGrid>, p: &[f32; 3]) -> ([f32; 3], [f32; 3], Vector3) {
    match grid {
        Some(g) => {
            let (a, d, dir) = g.sample(&[p[0], p[1], p[2] + 16.0]);
            (a, d, q2g_dir(&dir))
        }
        None => ([0.55; 3], [0.6; 3], Vector3::new(0.4, 0.8, 0.45).normalized()),
    }
}

/// `trap_RealTime`: wall-clock seconds since the epoch.
fn real_time() -> i32 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i32).unwrap_or(0)
}

impl crate::clientcmd::ClientHost for TremGame {
    fn snap_ps(&self) -> Option<trem_sim::kernel::types::PlayerState> {
        let (w, me) = (self.world.as_ref()?, self.me?);
        Some(w.clients[me].ps)
    }

    fn cg_time(&self) -> i32 {
        self.world.as_ref().map(|w| w.level.time).unwrap_or(0)
    }

    /// `FS_ReadFile`: user://, then the pk3s; default.cfg and
    /// port_default.cfg fall back to the built-in copies.
    fn read_file(&self, path: &str) -> Option<String> {
        use trem_sim::kernel::world::Fs;
        if let Some(t) = self.client_fs.as_ref().and_then(|fs| fs.read(path)) {
            return Some(t);
        }
        if path.eq_ignore_ascii_case("default.cfg") {
            return Some(crate::clientcmd::BUILTIN_DEFAULT_CFG.to_string());
        }
        if path.eq_ignore_ascii_case("port_default.cfg") {
            return Some(crate::clientcmd::PORT_DEFAULT_CFG.to_string());
        }
        None
    }

    fn is_server_command(&self, word: &str) -> bool {
        self.is_server_word(word)
    }

    fn server_console(&mut self, line: &str) -> String {
        let out = self.console_line(line);
        let more = self.run_exec_output();
        format!("{out}{more}")
    }

    fn server_set(&mut self, name: &str, value: &str) -> bool {
        if self.is_server_word(name) {
            let r = self.console_line(&format!("{name} {value}"));
            if !r.is_empty() {
                godot_print!("{r}");
            }
            return true;
        }
        let known = self.is_net_server_cvar(name) || self.game_world_mut().map(|w| w.set_cvar(name, value)).unwrap_or(false);
        if known {
            self.cvars.retain(|(x, _)| !x.eq_ignore_ascii_case(name));
            self.cvars.push((name.to_string(), value.to_string()));
            self.update_server_cvars();
        }
        known
    }

    fn engine_command(&mut self, argv: &[String]) -> Option<String> {
        if let Some(out) = self.wtb_engine_command(argv) {
            return Some(out); // wt-b: voip, video, stopvideo
        }
        let line = argv.iter().map(|a| if a.contains(' ') || a.is_empty() { format!("\"{a}\"") } else { a.clone() }).collect::<Vec<_>>().join(" ");
        self.net_engine_command(&line)
    }

    fn forward_to_server(&mut self, line: &str) -> String {
        self.client_command_line(line)
    }

    /// The port helpers run here; the rest goes to hud.gd (`host_command`).
    fn host(&mut self, argv: &[String]) {
        if self.misc_host(argv) {
            return; // wt-misc: cgame debug commands, condump
        }
        let a1 = argv.get(1).map(|s| s.as_str()).unwrap_or("");
        match argv[0].to_ascii_lowercase().as_str() {
            "port_addbot" => self.add_bots(GString::from(a1), 1),
            "port_removebot" => self.remove_bot(GString::from(a1)),
            "port_restart" => self.restart(),
            "port_gfx" => self.port_gfx(a1),
            "port_vision" => self.port_vision(),
            "s_fmodinfo" => {
                let msg = match self.fmod.as_ref() {
                    Some(f) => format!("FMOD + Steam Audio: {}\n", f.stats()),
                    None => "FMOD sound path not running (s_fmod 0, or its DLLs missing): Godot audio\n".to_string(),
                };
                godot_print!("{}", msg.trim_end());
                self.misc.con.print(&msg);
            }
            _ => {}
        }
        let line = argv.join(" ");
        self.base_mut().call_deferred("emit_signal", &["host_command".to_variant(), GString::from(line.as_str()).to_variant()]);
    }

    /// `CG_Viewpos_f`.
    fn viewpos(&self) -> String {
        let (o, a) = self.cam_q;
        format!("({} {} {}) : {}\n", o[0] as i32, o[1] as i32, o[2] as i32, a[1] as i32)
    }

    /// `CG_ClientList_f`.
    fn client_list(&self) -> String {
        let Some(w) = self.world.as_ref() else { return String::new() };
        let mut s = String::new();
        let mut count = 0;
        for (i, c) in w.clients.iter().enumerate().filter(|(_, c)| c.connected) {
            let t = match c.team {
                Team::Aliens => "^1A   ^7",
                Team::Humans => "^5H   ^7",
                _ => "S   ",
            };
            s.push_str(&format!("{i:2} {t}{}\n", c.name));
            count += 1;
        }
        s.push_str(&format!("Listed {count:2} clients\n"));
        s
    }
}

// ===================================================================== wt-misc
// The client / cgame pieces kept in misc.rs: CL_CheckUserinfo (userinfo.rs),
// IN_JoyMove (joystick.rs), the console (console.rs), CG_StatusMessages'
// rename / team prints, CG_DrawVote's talk beep and the cgame debug commands
// (devcmds.rs). Everything of this work in game.rs is in this block, the
// `misc` field and its one-line call sites (marked "wt-misc").
impl TremGame {
    /// The client's Com_Printf text: the terminal and `CL_ConsolePrint`.
    fn misc_print_output(&mut self) {
        let out = self.client.take_output();
        for l in out.lines() {
            godot_print!("{l}");
        }
        if !out.is_empty() {
            self.misc.con.print(&out);
        }
    }

    /// Before the ticks: the command buffer's output, `CL_CheckUserinfo`
    /// (the listen server runs `ClientUserinfoChanged` for the local
    /// player) and `IN_Frame`'s `IN_JoyMove`.
    fn misc_client_frame(&mut self) {
        self.misc_print_output();
        // networked, the network client sends the "userinfo" command instead
        // (CL_CheckUserinfo in nethost sync)
        if crate::userinfo::take_userinfo_modified(&mut self.client.cvars) && !self.net_active() {
            if let (Some(w), Some(me)) = (self.world.as_mut(), self.me) {
                let name = self.client.cvars.string("name");
                client::userinfo_changed(w, me, &name, false);
            }
        }
        // IN_InitJoystick at start-up and on in_restart (in_joystick is CVAR_LATCH)
        if !self.misc.joy.inited || std::mem::take(&mut self.client.in_restart) {
            let names = crate::joystick::godot_joystick_names();
            self.misc.joy.init(&mut self.client.cvars, &names);
        }
        if let Some(stick) = self.misc.joy.stick.and_then(crate::joystick::poll_godot) {
            let threshold = self.client.cvars.value("joy_threshold");
            for ev in self.misc.joy.joy_move(threshold, &stick) {
                match ev {
                    crate::joystick::JoyEvent::Key(key, down) => {
                        self.key_event_num(key, down);
                        self.wtb_joy_key(key, down); // wt-b: the ui / console key catchers
                    }
                    crate::joystick::JoyEvent::Axis(axis, value) => self.client.input.joystick_event(axis, value),
                }
            }
        }
    }

    /// After the ticks and the camera: the console's copy of the notify
    /// lines, `CG_StatusMessages` (renames, team changes), `CG_DrawVote`'s beep,
    /// `CG_AddTestModel`, cg_drawBBOX and cg_drawSurfNormal.
    fn misc_frame(&mut self) {
        let total = self.feed_dropped + self.feed.len() as u64;
        if total > self.misc.feed_seen {
            let n = ((total - self.misc.feed_seen) as usize).min(self.feed.len());
            let lines: Vec<String> = self.feed[self.feed.len() - n..].iter().map(|(_, l)| format!("{l}\n")).collect();
            for l in lines {
                self.misc.con.print(&l);
            }
            self.misc.feed_seen = total;
        }
        let Some(w) = self.world.as_ref() else { return };
        let renames = self.misc.names.update(w.clients.iter().map(|c| (c.connected, c.name.as_str(), c.team as i32)));
        self.misc.vote.update([0, 1, 2].map(|t| (w.level.votes[t].time, w.level.votes[t].yes, w.level.votes[t].no)));
        let intermission = w.level.intermission_time != 0;
        let my_team = self.me.map(|m| w.clients[m].team as usize).unwrap_or(0);
        for l in renames {
            self.feed.push((self.clock, strip_colors(l.trim_end())));
        }
        // CG_Say: the talk sound of each chat line (the team's for team / area / team private)
        for mode in std::mem::take(&mut self.misc.chat_sounds) {
            use trem_sim::kernel::cmds::SayMode;
            let sound = match (mode, my_team) {
                (SayMode::Team | SayMode::Area | SayMode::TeamPrivate, 1) => "sound/misc/alien_talk.wav",
                (SayMode::Team | SayMode::Area | SayMode::TeamPrivate, 2) => "sound/misc/human_talk.wav",
                _ => "sound/misc/talk.wav",
            };
            self.play_sound(sound, &[0.0; 3], true);
        }
        // CG_Draw2D (not at intermission): CG_DrawVote( TEAM_NONE ), CG_DrawVote( team )
        if !intermission {
            let beeps = self.misc.vote.draw(0) as usize + self.misc.vote.draw(my_team) as usize;
            for _ in 0..beeps {
                self.play_sound("sound/misc/talk.wav", &[0.0; 3], true);
            }
        }
        // CG_AddTestModel
        let (vieworg, viewangles) = self.cam_q;
        let gun = [self.cgv.cvars.gun_x, self.cgv.cvars.gun_y, self.cgv.cvars.gun_z];
        let mut parent = self.to_gd().upcast::<Node3D>();
        let grid = self.lightgrid.as_ref();
        if let Some(msg) = self.misc.dev.add_test_model(self.lib.as_mut(), &mut parent, vieworg, viewangles, gun, |p| light_at(grid, p)) {
            self.misc.con.print(&msg);
        }
        // CG_AddViewWeapon: "don't draw if testing a gun model"
        if self.misc.dev.test.as_ref().is_some_and(|t| t.gun && !t.name.is_empty()) {
            if let Some(h) = self.hand.as_ref() {
                for mut n in h.nodes() {
                    n.set_visible(false);
                }
            }
        }
        let mut polys = Vec::new();
        let Some(w) = self.world.as_ref() else { return };
        if self.client.cvars.integer("cg_drawBBOX") != 0 {
            for e in w.ents.iter().take(w.num_entities).filter(|e| e.inuse && e.linked && matches!(e.etype, EType::Buildable | EType::Missile | EType::Corpse)) {
                let (mins, maxs) = crate::devcmds::solid_box(crate::devcmds::packed_solid(e.bmodel, e.contents, e.mins, e.maxs));
                polys.extend(crate::devcmds::bounding_box(e.origin, mins, maxs));
            }
        }
        if self.client.cvars.integer("cg_drawSurfNormal") != 0 {
            let (f, r, _) = angle_vectors(&viewangles);
            let end = ma(&vieworg, 8192.0, &f);
            let pass = self.me.map(|m| m as i32).unwrap_or(-1);
            let tr = w.trace(&vieworg, None, None, &end, pass, k::MASK_SOLID);
            polys.push(crate::devcmds::surf_normal(tr.endpos, tr.plane.normal, [-r[0], -r[1], -r[2]]));
        }
        self.misc.dev.draw_polys(&mut parent, &polys);
    }

    /// cg_debugEvents (`CG_EntityEvent`'s first print).
    fn misc_event(&mut self, e: &GameEvent) {
        if self.client.cvars.integer("cg_debugevents") != 0 {
            let mut line = crate::devcmds::debug_event_line(e.entity, e.event as i32);
            line = format!("{} t={} {}\n", line.trim_end(), (self.clock * 1000.0) as i64, if e.predictable { "predicted" } else { "" });
            godot_print!("{}", line.trim_end());
            self.misc.con.print(&line);
        }
    }

    /// The host commands of this block: the cgame debug commands,
    /// `Con_Clear_f` and `Con_Dump_f`. True when handled.
    fn misc_host(&mut self, argv: &[String]) -> bool {
        let cmd = argv[0].to_ascii_lowercase();
        let (vieworg, viewangles) = self.cam_q;
        let now = (self.clock * 1000.0) as i32;
        let out = match cmd.as_str() {
            "testmodel" | "testgun" => {
                let mut parent = self.to_gd().upcast::<Node3D>();
                self.misc.dev.test_model_f(argv, vieworg, viewangles, cmd == "testgun", self.lib.as_mut(), &mut parent)
            }
            "nextframe" | "prevframe" | "nextskin" | "prevskin" => self.misc.dev.step(&cmd),
            "testps" => {
                let (f, _, _) = angle_vectors(&viewangles);
                let origin = ma(&vieworg, 100.0, &f);
                if let (Some(name), Some(fx)) = (argv.get(1), self.effects.as_mut()) {
                    fx.test_ps(name, origin, now);
                }
                String::new()
            }
            "destroytestps" => {
                if let Some(fx) = self.effects.as_mut() {
                    fx.destroy_test_ps();
                }
                String::new()
            }
            "testts" => {
                if let (Some(name), Some(fx)) = (argv.get(1), self.effects.as_mut()) {
                    fx.test_ts(name, now);
                }
                String::new()
            }
            "destroytestts" => {
                if let Some(fx) = self.effects.as_mut() {
                    fx.destroy_test_ts();
                }
                String::new()
            }
            // BG_MemoryInfo: there is no BG_Alloc zone
            "cgame_memory" => "cgame_memory: Rust allocator (no BG_Alloc zone)\n".into(),
            "clear" => {
                self.misc.con.con.clear();
                return false; // hud.gd clears its own lines too
            }
            "condump" => {
                if argv.len() != 2 {
                    "usage: condump <filename>\n".into()
                } else {
                    let mut name = argv[1].clone();
                    if !name.rsplit(['/', '\\']).next().unwrap_or("").contains('.') {
                        name.push_str(".txt");
                    }
                    let text = self.misc.con.con.dump();
                    use trem_sim::kernel::world::Fs;
                    match self.client_fs.as_mut().map(|fs| fs.write(&name, &text)) {
                        Some(true) => format!("Dumped console text to {name}.\n"),
                        _ => format!("ERROR: couldn't open {name}.\n"),
                    }
                }
            }
            _ => return false,
        };
        if !out.is_empty() {
            godot_print!("{}", out.trim_end());
            self.misc.con.print(&out);
        }
        true
    }

    /// The console (TremConsole draws it).
    pub(crate) fn misc_console(&mut self) -> &mut crate::console::Console {
        if !self.misc.con_history_loaded {
            // CL_LoadConsoleHistory
            self.misc.con_history_loaded = true;
            let path = godot::classes::ProjectSettings::singleton().globalize_path(&format!("user://{}", crate::console::CONSOLE_HISTORY_FILE)).to_string();
            let ok = std::fs::read(&path).ok().is_some_and(|b| self.misc.con.load_history(&String::from_utf8_lossy(&b)));
            if !ok {
                self.misc.con.print(&format!("Couldn't read {}.\n", crate::console::CONSOLE_HISTORY_FILE));
            }
        }
        &mut self.misc.con
    }

    /// `Console_Key` while the console has the keys: Enter runs the line
    /// through the command buffer, Tab completes, the history is saved.
    pub(crate) fn misc_console_key(&mut self, key: i32, ctrl: bool, shift: bool, clipboard: &str) {
        let active = self.world.is_some();
        let actions = self.misc_console().key(key, ctrl, shift, active, clipboard);
        for a in actions {
            match a {
                crate::console::ConsoleAction::Exec(text) => {
                    self.client.add_text(&text);
                    self.with_client(|c, g| c.execute(g));
                    self.misc_print_output();
                }
                crate::console::ConsoleAction::Complete => {
                    let src = self.misc_completion_source();
                    self.misc.con.complete(&src);
                }
                crate::console::ConsoleAction::SaveHistory(text) => {
                    let path = godot::classes::ProjectSettings::singleton().globalize_path(&format!("user://{}", crate::console::CONSOLE_HISTORY_FILE)).to_string();
                    if std::fs::write(&path, text).is_err() {
                        self.misc.con.print(&format!("Couldn't write {}.\n", crate::console::CONSOLE_HISTORY_FILE));
                    }
                }
                crate::console::ConsoleAction::None => {}
            }
        }
    }

    /// What tab completion sees: the client's commands, the server's
    /// (`G_RegisterCommands`, the engine's map commands), every cvar and
    /// the cfg / txt files of the user folder and the pk3s.
    fn misc_completion_source(&self) -> crate::misc::Completions {
        let mut commands = self.client.command_names();
        commands.extend(["map", "devmap", "map_restart", "killserver", "status"].map(String::from));
        if let Some(w) = self.world.as_ref() {
            commands.extend(svcmds::register_commands(w));
            // ClientBegin's G_ListCommands -> CG_GameCmds_f
            if let Some(me) = self.me {
                commands.extend(trem_sim::kernel::cmds::list_commands(w, me));
            }
        }
        commands.sort_by_key(|c| c.to_ascii_lowercase());
        commands.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
        let mut cvars: Vec<(String, String)> = self.client.cvars.iter().map(|v| (v.name.clone(), v.string.clone())).collect();
        cvars.extend(crate::view::ViewCvars::NAMES.iter().map(|n| (n.to_string(), self.cgv.cvars.get(n).unwrap_or_default())));
        cvars.extend(self.cvars.iter().cloned());
        cvars.sort_by_key(|c| c.0.to_ascii_lowercase());
        cvars.dedup_by(|a, b| a.0.eq_ignore_ascii_case(&b.0));
        let mut files = Vec::new();
        for ext in ["cfg", "txt"] {
            if let Ok(rd) = std::fs::read_dir(godot::classes::ProjectSettings::singleton().globalize_path("user://").to_string()) {
                files.extend(rd.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).filter(|n| n.to_lowercase().ends_with(&format!(".{ext}"))));
            }
            if let Some(lib) = self.lib.as_ref() {
                files.extend(lib.pk3.files().filter(|f| !f.contains('/') && f.to_lowercase().ends_with(&format!(".{ext}"))).cloned());
            }
        }
        files.sort_by_key(|f| f.to_lowercase());
        files.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
        crate::misc::Completions { commands, cvars, files }
    }
}

// ===================================================================== wt-b
// VoIP (voipsnd.rs over the kernel's voip.rs), the `video` capture
// (avicap.rs over the kernel's avi.rs) and joystick keys for the ui and
// console key catchers (cl_keys.c `CL_KeyDownEvent` / `CL_KeyUpEvent`'s
// `UI_KEY_EVENT` / `Console_Key`). Everything of this work in game.rs is
// in this block, the `wtb` field and its one-line call sites ("wt-b").

/// The wt-b state of the TremGame node.
#[derive(Default)]
pub(crate) struct WtB {
    voip: crate::voipsnd::VoipSound,
    video: Option<crate::avicap::VideoCapture>,
    /// joystick key events for a key catcher: (console, key, down)
    joy_keys: Vec<(bool, i32, bool)>,
    /// TremConsole has the keys (KEYCATCH_CONSOLE)
    console_down: bool,
    registered: bool,
    /// the voice HUD's note under the microphone: (text, warning, until ms)
    voip_note: Option<(String, bool, u64)>,
    /// voip_toggle as the last frame saw it
    voip_prev_toggle: bool,
    /// when the local player's captured voice was last over the talking
    /// threshold (ms): the server never sends a player's own voice back,
    /// so last_heard cannot list them
    voip_self_talk: u64,
    /// voip_refreshdevices ran: done on the next frame, since a console
    /// command runs while the client (and its cvars) is lent out
    voip_refresh: bool,
    /// when the microphone check goes to the console and the log (ms, 0
    /// none): what the capture delivered in its first 2 s
    voip_check_at: u64,
    /// the loudest power since the microphone went on
    voip_peak_power: f32,
    /// the microphone test's power (outside an online game)
    voip_local_power: f32,
}

impl TremGame {
    /// `CL_Init`'s cl_voip* / cl_avi* cvars and `cl_voipProtocol`
    /// (CVAR_USERINFO | CVAR_ROM, "opus" while cl_voip is on).
    fn wtb_register_cvars(&mut self) {
        if std::mem::replace(&mut self.wtb.registered, true) {
            return;
        }
        use crate::cmd::{CVAR_ARCHIVE, CVAR_ROM, CVAR_USERINFO};
        for (n, v, archive) in trem_sim::kernel::voip::CL_VOIP_CVARS {
            self.client.cvars.get(n, v, if *archive { CVAR_ARCHIVE } else { 0 });
        }
        self.client.cvars.set("cl_voipSend", "0");
        let p = trem_sim::kernel::voip::client_voip_protocol(self.client.cvars.integer("cl_voip"));
        self.client.cvars.get("cl_voipProtocol", p, CVAR_USERINFO | CVAR_ROM);
        self.client.cvars.set("cl_voipProtocol", p);
        for (n, v) in crate::avicap::AVI_CVARS {
            self.client.cvars.get(n, v, if *n == "cl_forceavidemo" { 0 } else { CVAR_ARCHIVE });
        }
        // the port's voice HUD and capture device (dec053)
        self.client.cvars.get("cl_voipHud", "1", CVAR_ARCHIVE);
        self.client.cvars.get("s_voipInputDevice", "Default", CVAR_ARCHIVE);
        let dev = self.client.cvars.string("s_voipInputDevice");
        self.wtb.voip.set_input_device(&dev);
        self.client.cvars.take_modified("s_voipInputDevice");
    }

    /// The voice HUD's microphone note.
    fn wtb_voip_note(&mut self, text: &str, warn: bool) {
        let now = godot::classes::Time::singleton().get_ticks_msec();
        self.wtb.voip_note = Some((text.to_string(), warn, now + crate::voiphud::NOTE_MS));
    }

    /// Each frame: the capture device follows s_voipInputDevice; the
    /// microphone toggle goes off where there is nothing to send to (no
    /// online game); its changes become HUD notes.
    fn wtb_voip_frame(&mut self) {
        if std::mem::take(&mut self.wtb.voip_refresh) {
            let out = self.wtb_voip_refresh_devices();
            self.wtb_print(&out);
        }
        if self.client.cvars.take_modified("s_voipInputDevice") {
            let dev = self.client.cvars.string("s_voipInputDevice");
            self.wtb.voip.set_input_device(&dev);
        }
        let t = self.client.voip_toggle;
        if t != std::mem::replace(&mut self.wtb.voip_prev_toggle, t) {
            let now = godot::classes::Time::singleton().get_ticks_msec();
            if t {
                self.wtb.voip_note = None;
                self.wtb.voip.pumped = 0;
                self.wtb.voip.pump_peak = 0.0;
                self.wtb.voip_peak_power = 0.0;
                self.wtb.voip_check_at = now + 2000;
                let dev = godot::classes::AudioServer::singleton().get_input_device().to_string();
                self.wtb_print(&format!("VoIP: capture device \"{dev}\"\n"));
            } else {
                self.wtb_voip_note("MIC OFF", false);
                if self.wtb.voip_check_at == 0 {
                    let th = self.wtb_voip_talk_threshold(self.client.cvars.integer("cl_voipUseVAD") != 0);
                    self.wtb_print(&format!("VoIP: loudest voice while on: power {:.4} (talking from {:.4})\n", self.wtb.voip_peak_power, th));
                }
                self.wtb.voip_check_at = 0;
            }
        }
        // outside an online game the toggle runs the microphone test: the
        // voice HUD shows the microphone, nothing is sent
        if !self.net_active() {
            if t {
                if !self.wtb.voip.monitoring() {
                    self.client.cvars.set("cl_voipSend", "0");
                    self.wtb_print("VoIP: microphone test (not in an online game, so nothing is sent: Create Server or join one to talk)\n");
                    self.wtb_voip_note("Mic test: voice is sent in online games", false);
                }
                let mut parent: Gd<godot::classes::Node> = self.base().clone().upcast();
                if let Some(p) = self.wtb.voip.monitor(&mut parent) {
                    self.wtb.voip_local_power = p;
                }
            } else if self.wtb.voip.monitoring() {
                self.wtb.voip.stop_monitor();
                self.wtb.voip_local_power = 0.0;
            }
        } else if self.wtb.voip.monitoring() {
            // an online game began with the microphone on: CL_CaptureVoip takes over
            self.wtb.voip.stop_monitor();
            self.wtb.voip_local_power = 0.0;
            if t {
                self.client.cvars.set("cl_voipSend", "1");
            }
        }
        // the local player's talking state, held like a heard talker's
        let monitoring = self.wtb.voip.monitoring();
        let capturing = monitoring || self.nethost.as_ref().is_some_and(|n| n.cl.voip.capturing) && self.client.cvars.integer("cl_voipSend") != 0;
        let power = if monitoring { self.wtb.voip_local_power } else { self.nethost.as_ref().map(|n| n.cl.voip.power).unwrap_or(0.0) };
        let use_vad = self.client.cvars.integer("cl_voipUseVAD") != 0;
        self.wtb.voip_peak_power = self.wtb.voip_peak_power.max(power);
        let now = godot::classes::Time::singleton().get_ticks_msec();
        if self.wtb.voip_check_at > 0 && now >= self.wtb.voip_check_at {
            self.wtb.voip_check_at = 0;
            let v = &self.wtb.voip;
            let msg = if !capturing {
                let nh = self.nethost.as_ref();
                format!(
                    "VoIP: microphone check: not capturing (server voice chat {}, codec {}, cl_voipSend {})\n",
                    nh.is_some_and(|n| n.cl.voip.enabled),
                    nh.is_some_and(|n| n.cl.voip.codec_initialized),
                    self.client.cvars.integer("cl_voipSend")
                )
            } else if v.pumped == 0 {
                "VoIP: microphone check: NO audio from the capture device in 2 s (Windows microphone access, or the device is in use)\n".to_string()
            } else {
                format!("VoIP: microphone check: {} frames in 2 s, peak level {:.4}, power {:.4}\n", v.pumped, v.pump_peak, self.wtb.voip_peak_power)
            };
            self.wtb_print(&msg);
        }
        if capturing && power >= self.wtb_voip_talk_threshold(use_vad) {
            self.wtb.voip_self_talk = godot::classes::Time::singleton().get_ticks_msec();
        }
    }

    /// The local player's talking threshold (voiphud.rs).
    fn wtb_voip_talk_threshold(&self, use_vad: bool) -> f32 {
        let c = &self.client.cvars;
        crate::voiphud::talk_threshold(use_vad, crate::voiphud::effective_vad_threshold(c.value("cl_voipVADThreshold"), c.value("cl_voipCaptureMult")))
    }

    /// `voip_refreshdevices` (the voice page's Refresh button): AudioServer
    /// lists the capture devices again, s_voipInputDevice goes back to
    /// "Default" if its device is gone, and the capture restarts on it.
    fn wtb_voip_refresh_devices(&mut self) -> String {
        let devices = crate::voipsnd::VoipSound::input_devices();
        let mut dev = self.client.cvars.string("s_voipInputDevice");
        let mut out = String::new();
        if !devices.iter().any(|d| *d == dev) {
            if !dev.is_empty() {
                out += &format!("VoIP: microphone \"{dev}\" is gone, using Default\n");
            }
            dev = "Default".into();
            self.client.cvars.set("s_voipInputDevice", &dev);
        }
        self.client.cvars.take_modified("s_voipInputDevice");
        self.wtb.voip.set_input_device(&dev);
        self.wtb.voip.restart_capture();
        out += &format!("VoIP: {} capture device(s): {}\n", devices.len(), devices.join(", "));
        out
    }

    /// The voice HUD (None: hidden or nothing to show).
    pub(crate) fn wtb_voip_hud(&self) -> Option<crate::voiphud::VoipHud> {
        if self.client.cvars.integer("cl_voipHud") == 0 {
            return None;
        }
        let now = godot::classes::Time::singleton().get_ticks_msec();
        let nh = self.nethost.as_ref();
        let monitoring = self.wtb.voip.monitoring();
        let mic_on = monitoring || nh.is_some_and(|n| n.cl.voip.capturing) && self.client.cvars.integer("cl_voipSend") != 0;
        let power = if monitoring { self.wtb.voip_local_power } else { nh.map(|n| n.cl.voip.power).unwrap_or(0.0) };
        let me = nh.and_then(|n| n.client_num()).or(self.me);
        // the local player: from the capture (their voice never comes back
        // from the server), held TALKER_HOLD_MS between packets
        let talking = mic_on && self.wtb.voip_self_talk > 0 && now.saturating_sub(self.wtb.voip_self_talk) < crate::voiphud::TALKER_HOLD_MS;
        let talkers = match self.world.as_ref() {
            Some(w) => {
                let mut v: Vec<String> = Vec::new();
                if let Some(m) = me.filter(|m| talking && *m < w.clients.len()) {
                    v.push(crate::voiphud::plain_name(&w.clients[m].name, 24));
                }
                v.extend(self.wtb.voip.last_heard.iter().enumerate()
                    .filter(|(i, t)| **t > 0 && now.saturating_sub(**t) < crate::voiphud::TALKER_HOLD_MS && Some(*i) != me && *i < w.clients.len())
                    .map(|(i, _)| crate::voiphud::plain_name(&w.clients[i].name, 24)));
                v
            }
            None => Vec::new(),
        };
        let note = self.wtb.voip_note.as_ref().filter(|n| n.2 > now).map(|n| (n.0.clone(), n.1));
        let threshold = self.wtb_voip_talk_threshold(self.client.cvars.integer("cl_voipUseVAD") != 0);
        let h = crate::voiphud::VoipHud { mic_on, talking, level: crate::voiphud::level_of(power, threshold), note, talkers };
        (!h.is_empty()).then_some(h)
    }

    /// `clc.demoplaying` (the demo player sets the kernel's mirror).
    fn wtb_demo_playing(&self) -> bool {
        self.nethost.as_ref().is_some_and(|n| n.cl.voip.demoplaying)
    }

    /// Before the frame: `CL_Frame`'s "if recording an avi, lock to a fixed
    /// fps" (`CL_TakeVideoFrame`, msec = 1000 / cl_aviFrameRate); the
    /// capture device closes without a session; the frame time to use.
    fn wtb_frame(&mut self, delta: f64) -> f64 {
        self.wtb_register_cvars();
        self.wtb_voip_frame();
        if !self.net_active() {
            // CL_Frame runs CL_CaptureVoip disconnected too (CL_Disconnect's
            // clean-up: cl_voipSend 0, S_StopCapture, S_MasterGain( 1 ))
            if self.nethost.is_some() {
                self.wtb_voip_tick();
            }
            let v = self.client.cvars.value("s_volume");
            self.wtb.voip.idle(v);
        }
        if self.wtb.video.is_none() {
            return delta;
        }
        // CL_Disconnect: stop recording any video when the game goes
        let active = self.world.is_some() && self.loading.is_none();
        let force = self.client.cvars.integer("cl_forceavidemo") != 0;
        if self.world.is_none() && self.loading.is_none() && !force {
            let out = self.wtb_stop_video();
            self.wtb_print(&out);
            return delta;
        }
        if self.client.cvars.integer("cl_aviFrameRate") == 0 || delta <= 0.0 {
            return delta;
        }
        if !(active || force) {
            return delta;
        }
        let Some(vp) = self.base().get_viewport() else { return delta };
        let quality = self.client.cvars.integer("r_aviMotionJpegQuality");
        let timescale = self.net_cvar("timescale").map(|v| trem_sim::kernel::spawn::c_atof(&v) as f32).filter(|t| *t > 0.0).unwrap_or(1.0);
        let v = self.wtb.video.as_mut().unwrap();
        let r = v.take_frame(&vp, quality);
        let msec = v.avi.video_frame_msec(timescale);
        let note = std::mem::take(&mut v.avi.print_out);
        self.wtb_print(&note);
        if let Err(e) = r {
            // Com_Error( ERR_DROP, "Failed to write avi file" )
            self.wtb.video = None;
            self.wtb_print(&format!("{e}\n"));
            return delta;
        }
        msec as f64 / 1000.0
    }

    fn wtb_print(&mut self, s: &str) {
        if s.is_empty() {
            return;
        }
        for l in s.lines() {
            godot_print!("{l}");
        }
        self.misc.con.print(s);
    }

    /// After each network tick: `CL_CaptureVoip` with the capture device
    /// and the client cvars, then `CL_PlayVoip`'s raw streams.
    fn wtb_voip_tick(&mut self) {
        let Some(mut nh) = self.nethost.take() else { return };
        // CG_LAST_ATTACKER / CG_CROSSHAIR_PLAYER, read when +voiprecord parses the targets
        let parsing = self.client.cvars.find("cl_voipSend").is_some_and(|v| v.modified);
        let me = nh.client_num();
        let attacker = match (self.cgv.attacker_time, me, self.world.as_ref()) {
            (0, _, _) => -1,
            (_, Some(me), Some(w)) => w.clients[me].ps.persistant[trem_sim::gen::enums::Pers::Attacker as usize],
            _ => -1,
        };
        let crosshair = if parsing { self.wtb_crosshair_player() } else { -1 };
        let parent: Gd<Node> = self.base().clone().upcast();
        // only what CL_CaptureVoip prints (the client's other text has its own readers)
        let before = nh.cl.print_out.len();
        {
            let mic_open = self.client.voip_toggle;
            let mut h = crate::voipsnd::Host { cvars: &mut self.client.cvars, snd: &mut self.wtb.voip, parent: parent.clone(), attacker, crosshair, mic_open };
            nh.cl.capture_voip(&mut h);
        }
        let out = nh.cl.print_out.split_off(before.min(nh.cl.print_out.len()));
        let plays = std::mem::take(&mut nh.cl.voip.playback);
        let mut refused = nh.cl.voip.refused.take();
        // in a game on a server without this voice chat (no sv_voipProtocol
        // opus: Tremulous 1.1 and GPP servers): CL_CaptureVoip returns
        // before its refusal when the codec never started, so say it here
        let no_voice = nh.client_num().is_some() && nh.cl.state == trem_sim::kernel::cl::ConnState::Active && (!nh.cl.voip.enabled || !nh.cl.voip.codec_initialized);
        self.nethost = Some(nh);
        if no_voice && self.client.voip_toggle {
            self.client.cvars.set("cl_voipSend", "0");
            refused = refused.or(Some("this server has no voice chat: it works on servers made with this game (Create Server)"));
        }
        self.wtb_print(&out);
        // the microphone toggle: CL_CaptureVoip would not capture
        if self.client.voip_toggle && self.net_active() && self.client.cvars.integer("cl_voipSend") == 0 {
            self.client.voip_toggle = false;
            self.wtb.voip_prev_toggle = false;
            let why = refused.unwrap_or("not in a game");
            self.wtb_print(&format!("VoIP: microphone off ({why})\n"));
            let mut note = why.to_string();
            if let Some(c) = note.get_mut(0..1) {
                c.make_ascii_uppercase();
            }
            self.wtb_voip_note(&note, true);
        }
        let mut parent = parent;
        for p in plays {
            // loopSounds[ entityNum ].origin: the talker's entity
            let origin = self.world.as_ref().filter(|_| p.entity >= 0).map(|w| w.ents[p.sender].origin);
            self.wtb.voip.play(&mut parent, &p, origin);
        }
        if let Some(w) = self.world.as_ref() {
            self.wtb.voip.follow(|i| Some(w.ents[i].origin));
        }
    }

    /// `CG_CrosshairPlayer` of the HUD (TremHud keeps cg.crosshairClientNum).
    fn wtb_crosshair_player(&self) -> i32 {
        let root = self.base().get_tree().get_root();
        let found = root.find_children_ex("*").type_("TremHud").owned(false).done();
        let r = found.iter_shared().find_map(|n| n.try_cast::<crate::hud::TremHud>().ok()).map(|h| h.bind().crosshair_player()).unwrap_or(-1);
        r
    }

    /// The engine commands of this block: `voip` (`CL_Voip_f`, registered
    /// once the codec runs, else the command goes to the server like the
    /// C's unknown command), `video` (`CL_Video_f`), `stopvideo`.
    fn wtb_engine_command(&mut self, argv: &[String]) -> Option<String> {
        let c = argv.first()?.to_ascii_lowercase();
        match c.as_str() {
            "voip" => {
                let nh = self.nethost.as_mut().filter(|n| n.cl.voip.command_registered)?;
                nh.cl.voip_f(argv);
                Some(std::mem::take(&mut nh.cl.print_out))
            }
            "video" => {
                if !self.wtb_demo_playing() {
                    return Some("The video command can only be used when playing back demos\n".into());
                }
                Some(self.wtb_open_video(argv.get(1).map(|s| s.as_str())))
            }
            "stopvideo" => Some(self.wtb_stop_video()),
            "voip_refreshdevices" => {
                self.wtb.voip_refresh = true;
                Some(String::new())
            }
            _ => None,
        }
    }

    /// `CL_Video_f`'s name + `CL_OpenAVIForWriting` (videos/ in the user folder).
    fn wtb_open_video(&mut self, name: Option<&str>) -> String {
        if self.wtb.video.is_some() {
            return String::new();
        }
        let home = godot::classes::ProjectSettings::singleton().globalize_path("user://").to_string();
        let home = std::path::PathBuf::from(home);
        let Some(file) = trem_sim::kernel::avi::video_file_name(name, |f| home.join(f).exists()) else {
            return "^1ERROR: no free file names to create video\n".into();
        };
        let Some(vp) = self.base().get_viewport() else { return String::new() };
        let rate = self.client.cvars.integer("cl_aviFrameRate");
        let mjpeg = self.client.cvars.integer("cl_aviMotionJpeg") != 0;
        match crate::avicap::VideoCapture::open(&home.join(&file), rate, mjpeg, &vp) {
            Ok(mut v) => {
                let out = std::mem::take(&mut v.avi.print_out);
                self.wtb.video = Some(v);
                out
            }
            Err(e) => e,
        }
    }

    /// `CL_StopVideo_f` / `CL_CloseAVI`.
    fn wtb_stop_video(&mut self) -> String {
        match self.wtb.video.take() {
            Some(v) => v.close().unwrap_or_else(|e| format!("{e}\n")),
            None => String::new(),
        }
    }

    /// A joystick key while a key catcher has the keys: queued for the
    /// console (`Console_Key`, presses) or the ui (`UI_KEY_EVENT`, presses
    /// and releases), which take them in their own frame.
    fn wtb_joy_key(&mut self, key: i32, down: bool) {
        if self.wtb.console_down {
            if down {
                self.wtb.joy_keys.push((true, key, down));
            }
        } else if !self.input_enabled {
            self.wtb.joy_keys.push((false, key, down));
        }
        if self.wtb.joy_keys.len() > 64 {
            self.wtb.joy_keys.remove(0);
        }
    }

    /// The queued joystick keys of one catcher (TremMenus / TremConsole).
    pub(crate) fn wtb_take_joy_keys(&mut self, console: bool) -> Vec<(i32, bool)> {
        let (mine, rest): (Vec<_>, Vec<_>) = self.wtb.joy_keys.drain(..).partition(|e| e.0 == console);
        self.wtb.joy_keys = rest;
        mine.into_iter().map(|e| (e.1, e.2)).collect()
    }

    /// TremConsole's KEYCATCH_CONSOLE state.
    pub(crate) fn wtb_set_console_down(&mut self, down: bool) {
        self.wtb.console_down = down;
    }

    /// `SCR_DrawVoipMeter`'s text for TremConsole (None: not drawn).
    pub(crate) fn wtb_voip_meter(&self) -> Option<String> {
        let nh = self.nethost.as_ref()?;
        let c = &self.client.cvars;
        // the voice HUD shows the level instead
        if c.integer("cl_voipHud") != 0 {
            return None;
        }
        nh.cl.voip_meter(c.integer("cl_voipShowMeter") != 0, c.integer("cl_voipSend") != 0, c.integer("cl_voip") != 0)
    }
}

#[godot_api(secondary)]
impl TremGame {
    /// wt-b: a tone (Hz, amplitude 0..32767) in place of the microphone
    /// for the VoIP capture (tests, machines without one); freq 0 = the
    /// microphone again.
    #[func]
    fn voip_test_tone(&mut self, freq: f32, amplitude: f32) {
        self.wtb.voip.set_test_tone((freq > 0.0).then_some((freq, amplitude)));
    }

    /// dec053: the voice HUD's inputs and what it draws (tests).
    #[func]
    fn voip_hud_state(&self) -> GString {
        let nh = self.nethost.as_ref();
        let s = format!(
            "toggle={} cl_voipSend={} net={} enabled={} capturing={} power={:.6} mic_fx={} pumped={} pump_peak={:.6} buffered={} hud={:?}",
            self.client.voip_toggle,
            self.client.cvars.integer("cl_voipSend"),
            nh.is_some(),
            nh.is_some_and(|n| n.cl.voip.enabled),
            nh.is_some_and(|n| n.cl.voip.capturing),
            nh.map(|n| n.cl.voip.power).unwrap_or(0.0),
            self.wtb.voip.has_capture_fx(),
            self.wtb.voip.pumped,
            self.wtb.voip.pump_peak,
            self.wtb.voip.buffered(),
            self.wtb_voip_hud()
        );
        GString::from(s.as_str())
    }

    /// wt-b: "sender:samples:peak ..." of the voice received so far.
    #[func]
    fn voip_stats(&self) -> GString {
        let v = &self.wtb.voip;
        let s: Vec<String> = v.received.iter().enumerate().filter(|(_, n)| **n > 0).map(|(i, n)| format!("{i}:{n}:{}", v.peak[i])).collect();
        GString::from(s.join(" ").as_str())
    }

    /// wt-b: `CL_OpenAVIForWriting( "videos/<name>.avi" )` without
    /// `CL_Video_f`'s demo check (tests; "" = the next free videoNNNN).
    #[func]
    fn video_open(&mut self, name: GString) -> GString {
        let n = name.to_string();
        let out = self.wtb_open_video(if n.is_empty() { None } else { Some(&n) });
        GString::from(out.as_str())
    }

    /// wt-b: `CL_CloseAVI`; its "Wrote ..." line.
    #[func]
    fn video_close(&mut self) -> GString {
        GString::from(self.wtb_stop_video().as_str())
    }

    /// wt-b (tests): one key of `IN_JoyMove` (K_JOY*, the stick's arrow
    /// keys), as the joystick poll sends it.
    #[func]
    fn joy_key_event(&mut self, keynum: i32, down: bool) {
        self.key_event_num(keynum, down);
        self.wtb_joy_key(keynum, down);
    }

    /// wt-b: `CL_VideoRecording`.
    #[func]
    fn video_recording(&self) -> bool {
        self.wtb.video.is_some()
    }
}

/// Tests: 3 s of white noise at a quarter of full scale, 48 kHz mono 16-bit.
fn test_noise_wav() -> Vec<u8> {
    let n = 48000 * 3;
    let mut w = Vec::with_capacity(44 + n * 2);
    let le32 = |w: &mut Vec<u8>, v: u32| w.extend_from_slice(&v.to_le_bytes());
    w.extend_from_slice(b"RIFF");
    le32(&mut w, 36 + n as u32 * 2);
    w.extend_from_slice(b"WAVEfmt ");
    le32(&mut w, 16);
    w.extend_from_slice(&[1, 0, 1, 0]);
    le32(&mut w, 48000);
    le32(&mut w, 96000);
    w.extend_from_slice(&[2, 0, 16, 0]);
    w.extend_from_slice(b"data");
    le32(&mut w, n as u32 * 2);
    let mut x: u32 = 0x1234_5678;
    for _ in 0..n {
        x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let v = ((x >> 16) as i16 as i32 / 4) as i16;
        w.extend_from_slice(&v.to_le_bytes());
    }
    w
}

/// Quake units of solid (CONTENTS_SOLID brushes) on the segment a -> b:
/// a trace finds each wall, point tests walk through it (4-unit steps).
fn solid_thickness(cm: &trem_sim::kernel::cm::ClipMap, a: &[f32; 3], b: &[f32; 3]) -> f32 {
    use trem_sim::gen::enums::contents;
    let solid = contents::SOLID as i32;
    let d = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
    let len = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
    if len < 1.0 {
        return 0.0;
    }
    let u = [d[0] / len, d[1] / len, d[2] / len];
    let at = |t: f32| [a[0] + u[0] * t, a[1] + u[1] * t, a[2] + u[2] * t];
    let mut t = 0.0f32;
    let mut total = 0.0f32;
    for _ in 0..8 {
        let tr = cm.box_trace(&at(t), b, None, None, solid);
        if tr.fraction >= 1.0 || tr.allsolid {
            break;
        }
        // into the wall, then through it
        let enter = t + (len - t) * tr.fraction;
        let mut x = enter + 1.0;
        while x < len && cm.point_contents(&at(x)) & solid != 0 {
            x += 4.0;
        }
        total += x.min(len) - enter;
        t = x;
        if t >= len {
            break;
        }
    }
    total
}

#[godot_api(secondary)]
impl TremGame {
    /// Tests: play `path` (a pk3 sound) at a Godot point through FMOD;
    /// returns the voice (-1 none).
    #[func]
    fn fmod_test_play(&mut self, path: GString, at: Vector3, looped: bool, big: bool) -> i32 {
        let key = path.to_string().to_lowercase();
        let (Some(fm), Some(lib)) = (self.fmod.as_mut(), self.lib.as_mut()) else { return -1 };
        // "test:noise": 3 s of steady white noise (no gusts, every band)
        let Some(id) = fm.sound(&key, || if key == "test:noise" { Some(test_noise_wav()) } else { lib.pk3.read(&key).map(|d| d.as_slice().to_vec()) }) else { return -1 };
        let v = fm.play(id, Some([at.x, at.y, at.z]), false, big, looped, 1.0);
        if v >= 0 {
            let q = [at.x / crate::geom::SCALE, -at.z / crate::geom::SCALE, at.y / crate::geom::SCALE];
            self.fmod_voices.retain(|x| x.0 != v);
            self.fmod_voices.push((v, q));
        }
        v
    }
    /// Tests: move a voice (Godot point).
    #[func]
    fn fmod_test_move(&mut self, voice: i32, at: Vector3) {
        if let Some(fm) = self.fmod.as_mut() {
            fm.set_pos(voice, [at.x, at.y, at.z]);
            let q = [at.x / crate::geom::SCALE, -at.z / crate::geom::SCALE, at.y / crate::geom::SCALE];
            if let Some(x) = self.fmod_voices.iter_mut().find(|x| x.0 == voice) {
                x.1 = q;
            }
        }
    }
    #[func]
    fn fmod_test_stop(&mut self, voice: i32) {
        if let Some(fm) = self.fmod.as_mut() {
            fm.stop(voice);
        }
    }
    /// Tests: average a voice's output level; `fmod_meter_read` returns it.
    #[func]
    fn fmod_meter(&mut self, voice: i32) {
        if let Some(fm) = self.fmod.as_mut() {
            fm.meter(voice);
        }
    }
    #[func]
    fn fmod_meter_read(&mut self) -> f32 {
        self.fmod.as_mut().map(|f| f.meter_read()).unwrap_or(-1.0)
    }
    /// Tests: [doppler, occlusion, pathing mix, thickness m, transmission
    /// mid, extra source, direct ms, direct max, reflections ms, max,
    /// quality, rays].
    #[func]
    fn fmod_voice_info(&self, voice: i32) -> PackedFloat32Array {
        let a = self.fmod.as_ref().map(|f| f.voice_info(voice)).unwrap_or([0.0; 24]);
        PackedFloat32Array::from(&a[..])
    }
    /// Tests: the pathing scan around the listener (radius in Godot units);
    /// its best blocked point is `fmod_path_best`.
    #[func]
    fn fmod_path_scan(&mut self, radius: f32) -> GString {
        let Some(fm) = self.fmod.as_mut() else { return GString::from("no FMOD") };
        let (txt, best) = fm.path_scan(radius);
        self.fmod_best_path = best;
        GString::from(txt.as_str())
    }
    #[func]
    fn fmod_path_best(&self) -> Vector3 {
        Vector3::new(self.fmod_best_path[0], self.fmod_best_path[1], self.fmod_best_path[2])
    }
    #[func]
    fn fmod_path_far(&self) -> Vector3 {
        Vector3::new(self.fmod_best_path[3], self.fmod_best_path[4], self.fmod_best_path[5])
    }
    /// Tests: one line per ragdoll: "ent corpse started bones x y z"
    /// (its root bone, Godot space).
    #[func]
    fn ragdoll_info(&self) -> PackedStringArray {
        self.ragdolls.iter().map(|r| {
            let p = r.position();
            GString::from(format!("{} {} {} {} {:.3} {:.3} {:.3}", r.ent, r.corpse as i32, r.started() as i32, r.bone_count(), p.x, p.y, p.z).as_str())
        }).collect()
    }
    /// Tests: per ragdoll "worst_overlap_m colliding_pairs".
    #[func]
    fn ragdoll_overlaps(&self) -> PackedStringArray {
        self.ragdolls.iter().filter(|r| r.started() && r.root.is_inside_tree()).map(|r| {
            let (o, n) = r.worst_overlap();
            GString::from(format!("{o:.3} {n}").as_str())
        }).collect()
    }
    /// Tests: "on flashlight-visible night-vision-visible" (port_vision).
    #[func]
    fn vision_state(&self) -> GString {
        let l = self.flashlight.as_ref().is_some_and(|l| l.is_instance_valid() && l.is_visible());
        let n = self.night_vision.as_ref().is_some_and(|n| n.is_instance_valid() && n.is_visible());
        GString::from(format!("{} {} {}", self.vision_on as i32, l as i32, n as i32).as_str())
    }
    /// Tests: per ragdoll hinge "bone min max seen_min seen_max" (flexion, degrees).
    #[func]
    fn ragdoll_hinges(&self) -> PackedStringArray {
        self.ragdolls.iter().flat_map(|r| r.hinge_report()).map(|l| GString::from(l.as_str())).collect()
    }
    /// Tests: the sign of the hinge angle against the flexion (+1 / -1).
    #[func]
    fn ragdoll_test_hinge_sign(&mut self, s: i32) {
        crate::ragdoll::set_hinge_sign(s);
    }
    /// Tests: the ragdolls' colliders: [map surfaces, map brushes, brush
    /// hulls, movers, structures].
    #[func]
    fn ragdoll_colliders(&self) -> PackedInt32Array {
        PackedInt32Array::from(&[self.ragdoll_world.is_some() as i32, self.ragdoll_brushes.is_some() as i32, self.world_hulls.len() as i32, self.ragdoll_movers.len() as i32, self.ragdoll_solids.len() as i32][..])
    }
    /// Tests: the push the next death of `ent` gets (as an obituary would set it).
    #[func]
    fn ragdoll_test_push(&mut self, ent: i32, push: Vector3) {
        self.death_push.insert(ent.max(0) as usize, (push, self.clock));
    }
    /// Tests: whether the original (vanilla) model of `ent` is drawn.
    #[func]
    fn actor_visible(&self, ent: i32) -> bool {
        self.actors.get(&(ent.max(0) as usize)).map(|a| a.root_nodes().first().map(|n| n.is_visible()).unwrap_or(false)).unwrap_or(false)
    }
    /// Tests: [entity, health, max health] of the structures of `classname`
    /// ("ent health max" each).
    #[func]
    fn structure_health(&self, classname: GString) -> PackedStringArray {
        // the game's world (a listen server's, whose entities keep their classnames)
        let Some(w) = self.game_world() else { return PackedStringArray::new() };
        let c = classname.to_string();
        (0..w.num_entities).filter(|&i| w.ents[i].inuse && w.ents[i].buildable.is_some_and(|b| w.ents[i].classname == c || b.cold().entity_name == c)).map(|i| GString::from(format!("{} {} {}", i, w.ents[i].health, w.ents[i].buildable.unwrap().def().health).as_str())).collect()
    }
    /// Tests: set a structure's health (a local game's world).
    #[func]
    fn test_set_health(&mut self, ent: i32, health: i32) -> bool {
        match self.game_world_mut() {
            Some(w) if (ent as usize) < w.ents.len() && w.ents[ent as usize].inuse => {
                w.ents[ent as usize].health = health;
                true
            }
            _ => false,
        }
    }
    /// Tests: the entity of the local player (-1 none).
    #[func]
    fn my_entity(&self) -> i32 {
        self.me.map(|m| m as i32).unwrap_or(-1)
    }

    /// Tests: freeze the listener where it is (true) or follow the camera.
    #[func]
    fn fmod_lock_listener(&mut self, on: bool) {
        self.fmod_listener_locked = on;
    }
    #[func]
    fn fmod_stats(&self) -> GString {
        GString::from(self.fmod.as_ref().map(|f| f.stats()).unwrap_or_default().as_str())
    }
}
