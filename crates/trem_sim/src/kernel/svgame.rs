//! Kernel `svgame` — what the server engine reads from the game module
//! (dec033): the shared `entityState_t` of every gentity (the C keeps it in
//! `ent->s`; this port's `Entity` flattens it, so `entity_state` packs it
//! back field for field), `BG_PlayerStateToEntityState`, the
//! `SV_LinkEntity` cluster / area bookkeeping and the `s.solid` box code,
//! `BG_PackEntityNumbers`, the configstrings the game sets with
//! `trap_SetConfigstring` (`CS_*`, `ClientUserinfoChanged`'s CS_PLAYERS
//! string), and the server commands built from the game's output queues
//! (`ScoreboardMessage`, `TeamplayInfoMessage`, print / chat / cp).
//!
//! Port differences (dec033): this World keeps its temp events in
//! `World::events` instead of event entities, so the server turns each one
//! into a temp entity (`eType = ET_EVENTS + event`) in a slot the World
//! does not use, alive for EVENT_VALID_MSEC like `G_TempEntity`'s; the
//! level 2 zap chains (`World::zaps`) become ET_LEV2_ZAP_CHAIN entities the
//! same way (`G_UpdateZapEffect`).

use super::bg;
use super::msg::{EntityState, GENTITYNUM_BITS};
use super::qmath::*;
use super::types::*;
use super::world::{EType, Entity, GameEvent, World, MAX_CLIENTS};
use crate::gen::buildables::BuildableId;
use crate::gen::enums::{contents, Pers, PmType, SpectatorState, Stat, Team, Traj, WeaponMode};
use crate::gen::upgrades::UpgradeId;

// ------------------------------------------------------------- entityType_t
pub const ET_GENERAL: i32 = 0;
pub const ET_PLAYER: i32 = 1;
pub const ET_ITEM: i32 = 2;
pub const ET_BUILDABLE: i32 = 3;
pub const ET_LOCATION: i32 = 4;
pub const ET_MISSILE: i32 = 5;
pub const ET_MOVER: i32 = 6;
pub const ET_BEAM: i32 = 7;
pub const ET_PORTAL: i32 = 8;
pub const ET_SPEAKER: i32 = 9;
pub const ET_PUSH_TRIGGER: i32 = 10;
pub const ET_TELEPORT_TRIGGER: i32 = 11;
pub const ET_INVISIBLE: i32 = 12;
pub const ET_GRAPPLE: i32 = 13;
pub const ET_CORPSE: i32 = 14;
pub const ET_PARTICLE_SYSTEM: i32 = 15;
pub const ET_ANIMMAPOBJ: i32 = 16;
pub const ET_MODELDOOR: i32 = 17;
pub const ET_LIGHTFLARE: i32 = 18;
pub const ET_LEV2_ZAP_CHAIN: i32 = 19;
pub const ET_EVENTS: i32 = 20;

/// The C's entityType_t number of a port entity type.
pub fn et_num(t: EType) -> i32 {
    match t {
        EType::General => ET_GENERAL,
        EType::Player => ET_PLAYER,
        EType::Buildable => ET_BUILDABLE,
        EType::Missile => ET_MISSILE,
        EType::Corpse => ET_CORPSE,
        EType::Location => ET_LOCATION,
        EType::Mover => ET_MOVER,
        EType::Portal => ET_PORTAL,
        EType::Speaker => ET_SPEAKER,
        EType::PushTrigger => ET_PUSH_TRIGGER,
        EType::TeleportTrigger => ET_TELEPORT_TRIGGER,
        EType::Invisible => ET_INVISIBLE,
        EType::ParticleSystem => ET_PARTICLE_SYSTEM,
        EType::AnimMapObj => ET_ANIMMAPOBJ,
        EType::ModelDoor => ET_MODELDOOR,
        EType::LightFlare => ET_LIGHTFLARE,
    }
}

/// The port entity type of an entityType_t number (events and zap chains
/// have none: None).
pub fn et_from_num(n: i32) -> Option<EType> {
    Some(match n {
        ET_GENERAL | ET_ITEM | ET_BEAM | ET_GRAPPLE => EType::General,
        ET_PLAYER => EType::Player,
        ET_BUILDABLE => EType::Buildable,
        ET_LOCATION => EType::Location,
        ET_MISSILE => EType::Missile,
        ET_MOVER => EType::Mover,
        ET_PORTAL => EType::Portal,
        ET_SPEAKER => EType::Speaker,
        ET_PUSH_TRIGGER => EType::PushTrigger,
        ET_TELEPORT_TRIGGER => EType::TeleportTrigger,
        ET_INVISIBLE => EType::Invisible,
        ET_CORPSE => EType::Corpse,
        ET_PARTICLE_SYSTEM => EType::ParticleSystem,
        ET_ANIMMAPOBJ => EType::AnimMapObj,
        ET_MODELDOOR => EType::ModelDoor,
        ET_LIGHTFLARE => EType::LightFlare,
        _ => return None,
    })
}

// ------------------------------------------------------------- svFlags
pub use super::spawn::{SVF_BROADCAST, SVF_NOCLIENT, SVF_PORTAL};
pub const SVF_CLIENTMASK: i32 = 0x2;
pub const SVF_SINGLECLIENT: i32 = 0x100;
pub const SVF_NOSERVERINFO: i32 = 0x200;
pub const SVF_NOTSINGLECLIENT: i32 = 0x800;

/// `SOLID_BMODEL`: a solid_box will never create this value.
pub const SOLID_BMODEL: i32 = 0xffffff;
/// `MAX_NUM_PACKED_ENTITY_NUMS`.
pub const MAX_NUM_PACKED_ENTITY_NUMS: usize = 10;
pub const GENTITYNUM_MASK: i32 = (1 << GENTITYNUM_BITS) - 1;

/// `SV_LinkEntity`'s `s.solid`: SOLID_BMODEL for brush models, an encoded
/// box for solid / body contents, 0 otherwise.
pub fn solid_for(e: &Entity) -> i32 {
    if e.bmodel {
        return SOLID_BMODEL;
    }
    if e.contents & (contents::SOLID as i32 | contents::BODY as i32) != 0 {
        // assume that x/y are equal and symetric
        let i = (e.maxs[0] as i32).clamp(1, 255);
        // z is not symetric
        let j = (-e.mins[2] as i32).clamp(1, 255);
        // and z maxs can be negative...
        let k = ((e.maxs[2] + 32.0) as i32).clamp(1, 255);
        return (k << 16) | (j << 8) | i;
    }
    0
}

/// The box encoded in `s.solid` (CG_ClipMoveToEntities / CG_BuildSolidList).
pub fn solid_box(solid: i32) -> (Vec3, Vec3) {
    let x = (solid & 255) as f32;
    let zd = ((solid >> 8) & 255) as f32;
    let zu = ((solid >> 16) & 255) as f32 - 32.0;
    ([-x, -x, -zd], [x, x, zu])
}

/// `svEntity_t`'s visibility part, as `SV_LinkEntity` computes it.
#[derive(Clone, Debug, Default)]
pub struct SvEntity {
    pub areanum: i32,
    pub areanum2: i32,
    pub clusternums: Vec<i32>,
    pub last_cluster: i32,
}

/// `MAX_ENT_CLUSTERS` / `MAX_TOTAL_ENT_LEAFS`.
const MAX_ENT_CLUSTERS: usize = 16;
const MAX_TOTAL_ENT_LEAFS: usize = 128;

/// `SV_LinkEntity`: areas and clusters of an absolute box.
pub fn link_box(w: &World, absmin: &Vec3, absmax: &Vec3) -> SvEntity {
    let mut leafs = w.cm.box_leafnums(absmin, absmax);
    let mut sv = SvEntity { areanum: -1, areanum2: -1, ..Default::default() };
    let last_leaf = if leafs.len() > MAX_TOTAL_ENT_LEAFS { leafs.get(MAX_TOTAL_ENT_LEAFS).copied() } else { None };
    leafs.truncate(MAX_TOTAL_ENT_LEAFS);
    if leafs.is_empty() {
        return sv;
    }
    let leaf = |l: i32| w.cm.leafs.get(l as usize);
    // set areas, even from clusters that don't fit in the entity array
    for &l in &leafs {
        let area = leaf(l).map(|x| x.area).unwrap_or(-1);
        if area != -1 {
            // doors may legally straggle two areas, but nothing should ever need more than that
            if sv.areanum != -1 && sv.areanum != area {
                sv.areanum2 = area;
            } else {
                sv.areanum = area;
            }
        }
    }
    // store as many explicit clusters as we can
    let mut i = 0;
    while i < leafs.len() {
        let cluster = leaf(leafs[i]).map(|x| x.cluster).unwrap_or(-1);
        if cluster != -1 {
            sv.clusternums.push(cluster);
            if sv.clusternums.len() == MAX_ENT_CLUSTERS {
                break;
            }
        }
        i += 1;
    }
    // store off a last cluster if we need to
    if i != leafs.len() || last_leaf.is_some() {
        let l = last_leaf.unwrap_or(*leafs.last().unwrap());
        sv.last_cluster = leaf(l).map(|x| x.cluster).unwrap_or(0);
    }
    sv
}

/// `BG_PackEntityNumbers`.
pub fn pack_entity_numbers(es: &mut EntityState, nums: &[i32]) {
    es.misc = 0;
    es.time = 0;
    es.time2 = 0;
    es.constant_light = 0;
    for i in 0..MAX_NUM_PACKED_ENTITY_NUMS {
        let n = nums.get(i).copied().unwrap_or(ENTITYNUM_NONE) & GENTITYNUM_MASK;
        match i {
            0 => es.misc |= n,
            1 => es.time |= n,
            2 => es.time |= n << GENTITYNUM_BITS,
            3 => es.time |= n << (GENTITYNUM_BITS * 2),
            4 => es.time2 |= n,
            5 => es.time2 |= n << GENTITYNUM_BITS,
            6 => es.time2 |= n << (GENTITYNUM_BITS * 2),
            7 => es.constant_light |= n,
            8 => es.constant_light |= n << GENTITYNUM_BITS,
            _ => es.constant_light |= n << (GENTITYNUM_BITS * 2),
        }
    }
}

/// `BG_UnpackEntityNumbers` (stops at the first ENTITYNUM_NONE).
pub fn unpack_entity_numbers(es: &EntityState, count: usize) -> Vec<i32> {
    let mut out = Vec::new();
    for i in 0..count.min(MAX_NUM_PACKED_ENTITY_NUMS) {
        let n = match i {
            0 => es.misc,
            1 => es.time,
            2 => es.time >> GENTITYNUM_BITS,
            3 => es.time >> (GENTITYNUM_BITS * 2),
            4 => es.time2,
            5 => es.time2 >> GENTITYNUM_BITS,
            6 => es.time2 >> (GENTITYNUM_BITS * 2),
            7 => es.constant_light,
            8 => es.constant_light >> GENTITYNUM_BITS,
            _ => es.constant_light >> (GENTITYNUM_BITS * 2),
        } & GENTITYNUM_MASK;
        if n == ENTITYNUM_NONE {
            break;
        }
        out.push(n);
    }
    out
}

/// `BG_PlayerStateToEntityState`. Events travel as temp entities in this
/// port (see the module notes), so `event` / `eventParm` stay 0.
pub fn player_state_to_entity_state(ps: &PlayerState, snap: bool) -> EntityState {
    let mut s = EntityState::default();
    s.e_type = if ps.pm_type == PmType::Intermission as i32 || ps.pm_type == PmType::Spectator as i32 || ps.pm_type == PmType::Freeze as i32 || ps.persistant[Pers::Specstate as usize] != SpectatorState::Not as i32 {
        ET_INVISIBLE
    } else {
        ET_PLAYER
    };
    s.number = ps.client_num;
    s.pos.tr_type = Traj::Interpolate as i32;
    s.pos.tr_base = ps.origin;
    if snap {
        snap_vector(&mut s.pos.tr_base);
    }
    // set the trDelta for flag direction
    s.pos.tr_delta = ps.velocity;
    s.apos.tr_type = Traj::Interpolate as i32;
    s.apos.tr_base = ps.viewangles;
    if snap {
        snap_vector(&mut s.apos.tr_base);
    }
    s.time2 = ps.movement_dir;
    s.legs_anim = ps.legs_anim;
    s.torso_anim = ps.torso_anim;
    s.weapon_anim = ps.weapon_anim;
    // ET_PLAYER looks here instead of at number so corpses can also
    // reference the proper config
    s.client_num = ps.client_num;
    s.e_flags = ps.e_flags;
    if ps.stats[Stat::Health as usize] <= 0 {
        s.e_flags |= k::EF_DEAD;
    } else {
        s.e_flags &= !k::EF_DEAD;
    }
    if ps.stats[Stat::State as usize] & k::SS_BLOBLOCKED != 0 {
        s.e_flags |= k::EF_BLOBLOCKED;
    } else {
        s.e_flags &= !k::EF_BLOBLOCKED;
    }
    s.weapon = ps.weapon;
    s.ground_entity_num = ps.ground_entity_num;
    // store items held and active items in modelindex and modelindex2
    for u in UpgradeId::ALL {
        let i = u as i32;
        if bg::inventory_contains_upgrade(i, &ps.stats) {
            s.modelindex |= 1 << i;
            if bg::upgrade_is_active(i, &ps.stats) {
                s.modelindex2 |= 1 << i;
            }
        }
    }
    // use misc field to store team/class info
    s.misc = ps.stats[Stat::Team as usize] | (ps.stats[Stat::Class as usize] << 8);
    // have to get the surfNormal through somehow...
    s.angles2 = ps.grapple_point;
    s.loop_sound = ps.loop_sound;
    s.generic1 = ps.generic1;
    if s.generic1 <= WeaponMode::None as i32 || s.generic1 >= WeaponMode::NumWeaponmodes as i32 {
        s.generic1 = WeaponMode::Primary as i32;
    }
    s.other_entity_num = ps.other_entity_num;
    s
}

/// `SnapVector` (x86_64: round half to even, as the oracle's trap_SnapVector).
pub fn snap_vector(v: &mut Vec3) {
    for c in v.iter_mut() {
        *c = (*c as f64).round_ties_even() as f32;
    }
}

/// `ent->s` of a non-client gentity, and whether it is linked and sent.
/// Returns None for entities the snapshot never carries (unused, unlinked,
/// freed after their event, SVF_NOCLIENT).
pub fn entity_state(w: &World, i: usize) -> Option<(EntityState, i32)> {
    let e = &w.ents[i];
    if !e.inuse || !e.linked || e.free_after_event {
        return None;
    }
    if i < MAX_CLIENTS && e.client {
        let c = &w.clients[i];
        if !c.connected {
            return None;
        }
        let mut s = player_state_to_entity_state(&c.ps, true);
        s.number = i as i32;
        s.solid = solid_for(e);
        return Some((s, 0));
    }
    let mut s = EntityState { number: i as i32, e_type: et_num(e.etype), e_flags: e.e_flags, pos: e.pos, apos: e.apos, ..Default::default() };
    // s.origin / s.angles: the spawn values for map entities, else where it is
    s.origin = if e.s_origin != [0.0; 3] || e.etype == EType::Mover { e.s_origin } else { e.origin };
    s.angles = if e.s_angles != [0.0; 3] || e.etype == EType::Mover { e.s_angles } else { e.angles };
    s.origin2 = e.origin2;
    s.angles2 = e.angles2;
    s.time = e.s_time;
    s.ground_entity_num = e.ground_entity;
    s.constant_light = e.constant_light;
    s.loop_sound = e.loop_sound;
    s.modelindex = e.model_index;
    s.modelindex2 = e.model_index2;
    s.client_num = e.s_client_num;
    s.frame = e.frame;
    s.event_parm = e.event_parm;
    s.misc = e.misc;
    s.weapon = e.weapon;
    s.legs_anim = e.legs_anim;
    s.torso_anim = e.torso_anim;
    s.generic1 = e.generic1;
    match e.etype {
        EType::Buildable => {
            // s.modelindex = the buildable (BA_*), s.generic1 = health
            s.modelindex = e.buildable.map(|b| b.num() as i32).unwrap_or(0);
            if e.buildable == Some(BuildableId::Mgturret) {
                s.angles2 = e.angles2;
            }
        }
        EType::Missile => {
            // s.otherEntityNum: the owner (cgame trails and impacts)
            s.other_entity_num = if e.owner == ENTITYNUM_NONE { ENTITYNUM_NONE } else { e.owner };
        }
        EType::Corpse => {
            // corpses carry the dead player's class in generic1 and team
            s.modelindex2 = e.team as i32;
        }
        _ => {}
    }
    s.solid = solid_for(e);
    Some((s, e.svflags))
}

/// A temp event entity (`G_TempEntity` / `G_AddEvent` + EF_PLAYER_EVENT
/// for the predictable ones, see the module notes).
pub fn event_state(ev: &GameEvent, number: i32) -> EntityState {
    let mut s = EntityState { number, e_type: ET_EVENTS + ev.event as i32, ..Default::default() };
    s.pos.tr_base = ev.origin;
    s.origin = ev.origin;
    s.origin2 = ev.origin2;
    s.other_entity_num = if ev.entity < 0 { ENTITYNUM_NONE } else { ev.entity };
    s.other_entity_num2 = if ev.other < 0 { ENTITYNUM_NONE } else { ev.other & GENTITYNUM_MASK };
    s.event_parm = ev.parm & 0xff;
    // the full parm (sound and model indexes, means of death...) when it
    // does not fit eventParm's 8 bits
    s.time2 = ev.parm;
    s.weapon = ev.weapon & 0xff;
    s.generic1 = ev.generic1;
    s.torso_anim = ev.charge & 0xff;
    s.time = ev.time;
    if ev.predictable {
        s.e_flags |= k::EF_PLAYER_EVENT;
    }
    s
}

/// The GameEvent a temp event entity carries (the client's CG_CheckEvents).
pub fn event_from_state(s: &EntityState) -> Option<GameEvent> {
    let ev = crate::gen::enums::Event::from_raw((s.e_type - ET_EVENTS).clamp(0, 255) as u8)?;
    let ent = |n: i32| if n == ENTITYNUM_NONE { -1 } else { n };
    Some(GameEvent {
        time: s.time,
        event: ev,
        origin: s.pos.tr_base,
        origin2: s.origin2,
        entity: ent(s.other_entity_num),
        other: ent(s.other_entity_num2),
        parm: if s.time2 != 0 { s.time2 } else { s.event_parm },
        weapon: s.weapon,
        generic1: s.generic1,
        charge: s.torso_anim,
        predictable: s.e_flags & k::EF_PLAYER_EVENT != 0,
    })
}

/// Events the C sends with SVF_BROADCAST (G_BroadcastEvent, the obituary).
pub fn event_is_broadcast(ev: &GameEvent) -> bool {
    use crate::gen::enums::Event;
    matches!(ev.event, Event::Obituary | Event::OvermindSpawns | Event::OvermindDying | Event::OvermindAttack | Event::DccAttack | Event::GlobalSound)
}

/// `G_UpdateZapEffect`: an ET_LEV2_ZAP_CHAIN entity for a zap.
pub fn zap_state(w: &World, z: &super::world::Zap, number: i32) -> EntityState {
    let mut s = EntityState { number, e_type: ET_LEV2_ZAP_CHAIN, ..Default::default() };
    let mut nums = vec![z.creator as i32];
    nums.extend(z.targets.iter().map(|&t| t as i32));
    pack_entity_numbers(&mut s, &nums);
    s.pos.tr_base = w.ents[z.creator].origin;
    s.origin = s.pos.tr_base;
    s
}

// ================================================================ configstrings

/// bg_public.h configstring indexes.
pub mod cs {
    use super::super::world::MAX_CLIENTS;
    pub const SERVERINFO: usize = 0;
    pub const SYSTEMINFO: usize = 1;
    pub const MUSIC: usize = 2;
    pub const MESSAGE: usize = 3;
    pub const MOTD: usize = 4;
    pub const WARMUP: usize = 5;
    pub const VOTE_TIME: usize = 6;
    pub const VOTE_STRING: usize = VOTE_TIME + 3;
    pub const VOTE_YES: usize = VOTE_STRING + 3;
    pub const VOTE_NO: usize = VOTE_YES + 3;
    pub const VOTE_CALLER: usize = VOTE_NO + 3;
    pub const GAME_VERSION: usize = VOTE_CALLER + 3;
    pub const LEVEL_START_TIME: usize = GAME_VERSION + 1;
    pub const INTERMISSION: usize = LEVEL_START_TIME + 1;
    pub const WINNER: usize = INTERMISSION + 1;
    pub const SHADERSTATE: usize = WINNER + 1;
    pub const BOTINFO: usize = SHADERSTATE + 1;
    pub const CLIENTS_READY: usize = BOTINFO + 1;
    pub const ALIEN_STAGES: usize = CLIENTS_READY + 1;
    pub const HUMAN_STAGES: usize = ALIEN_STAGES + 1;
    pub const MODELS: usize = HUMAN_STAGES + 1;
    pub const MAX_MODELS: usize = 256;
    pub const MAX_SOUNDS: usize = 256;
    pub const MAX_GAME_SHADERS: usize = 64;
    pub const MAX_GAME_PARTICLE_SYSTEMS: usize = 64;
    pub const MAX_LOCATIONS: usize = 64;
    pub const SOUNDS: usize = MODELS + MAX_MODELS;
    pub const SHADERS: usize = SOUNDS + MAX_SOUNDS;
    pub const PARTICLE_SYSTEMS: usize = SHADERS + MAX_GAME_SHADERS;
    pub const PLAYERS: usize = PARTICLE_SYSTEMS + MAX_GAME_PARTICLE_SYSTEMS;
    pub const LOCATIONS: usize = PLAYERS + MAX_CLIENTS;
    pub const MAX: usize = LOCATIONS + MAX_LOCATIONS;
    /// `MAX_CONFIGSTRINGS`.
    pub const MAX_CONFIGSTRINGS: usize = 1024;
}

/// `GAME_VERSION`.
pub const GAME_VERSION: &str = "base";

/// `Info_SetValueForKey` on a `\key\value` string (keys are replaced).
pub fn info_set(info: &mut String, key: &str, value: &str) {
    let mut pairs = info_pairs(info);
    pairs.retain(|(k_, _)| !k_.eq_ignore_ascii_case(key));
    if !value.is_empty() {
        pairs.push((key.to_string(), value.to_string()));
    }
    *info = pairs.iter().map(|(k_, v)| format!("\\{k_}\\{v}")).collect();
}

/// `Info_ValueForKey`.
pub fn info_value(info: &str, key: &str) -> String {
    info_pairs(info).into_iter().find(|(k_, _)| k_.eq_ignore_ascii_case(key)).map(|(_, v)| v).unwrap_or_default()
}

/// `Info_NextPair` over the whole string.
pub fn info_pairs(info: &str) -> Vec<(String, String)> {
    let s = info.strip_prefix('\\').unwrap_or(info);
    let parts: Vec<&str> = s.split('\\').collect();
    parts.chunks(2).filter(|c| !c[0].is_empty()).map(|c| (c[0].to_string(), c.get(1).copied().unwrap_or("").to_string())).collect()
}

/// `Com_ClientListString`: "%08x%08x" of the hi and lo words.
pub fn client_list_string(set: impl Fn(usize) -> bool) -> String {
    let (mut lo, mut hi) = (0u32, 0u32);
    for i in 0..MAX_CLIENTS {
        if set(i) {
            if i < 32 {
                lo |= 1 << i;
            } else {
                hi |= 1 << (i - 32);
            }
        }
    }
    format!("{hi:08x}{lo:08x}")
}

/// `Com_ClientListParse` + `Com_ClientListContains`.
pub fn client_list_contains(s: &str, i: usize) -> bool {
    if s.len() != 16 || i >= MAX_CLIENTS {
        return false;
    }
    let (Some(h), Some(l)) = (s.get(..8), s.get(8..)) else { return false };
    let hi = u32::from_str_radix(h, 16).unwrap_or(0);
    let lo = u32::from_str_radix(l, 16).unwrap_or(0);
    if i < 32 {
        lo & (1 << i) != 0
    } else {
        hi & (1 << (i - 32)) != 0
    }
}

/// `ClientUserinfoChanged`'s CS_PLAYERS string.
pub fn client_info_string(w: &World, i: usize) -> String {
    let c = &w.clients[i];
    if !c.connected {
        return String::new();
    }
    // the clientInfo model: the class chosen to spawn as, else the battlesuit
    let cl = c.class_selection.unwrap_or(crate::gen::classes::ClassId::HumanBsuit);
    let cold = cl.cold();
    let model = format!("{}/{}", cold.model.unwrap_or(""), cold.skin.unwrap_or("default"));
    let ignore = client_list_string(|j| c.ignored.contains(&j));
    format!("n\\{}\\t\\{}\\model\\{}\\ig\\{:>16}\\v\\{}", c.name, c.team as i32, model, ignore, "")
}

/// `BuildShaderStateConfig`: "old=new:time@..." of the shader remaps.
pub fn shader_state_config(w: &World) -> String {
    let mut s = String::new();
    for (old, new, t) in &w.level.shader_remaps {
        s.push_str(&format!("{old}={new}:{t:5.2}@"));
    }
    s
}

/// Every configstring the game module sets, from the World's state (the C
/// sets them when they change; the server diffs this list, which has the
/// same effect). `serverinfo` / `systeminfo` are the engine's cvar info
/// strings; `motd` is g_motd.
pub fn configstrings(w: &World, serverinfo: &str, systeminfo: &str, motd: &str, message: &str) -> Vec<(usize, String)> {
    let l = &w.level;
    let mut v: Vec<(usize, String)> = vec![
        (cs::SERVERINFO, serverinfo.to_string()),
        (cs::SYSTEMINFO, systeminfo.to_string()),
        (cs::MESSAGE, message.to_string()),
        (cs::MOTD, motd.to_string()),
        (cs::WARMUP, "-1".into()),
        (cs::GAME_VERSION, GAME_VERSION.into()),
        (cs::LEVEL_START_TIME, l.start_time.to_string()),
        (cs::INTERMISSION, if l.intermission_time != 0 { "1" } else { "0" }.into()),
        (cs::SHADERSTATE, shader_state_config(w)),
    ];
    let winner = if l.intermission_queued != 0 || l.intermission_time != 0 {
        match l.winner {
            Some(Team::Aliens) => "Aliens Win",
            Some(Team::Humans) => "Humans Win",
            _ => "Stalemate",
        }
    } else {
        ""
    };
    v.push((cs::WINNER, winner.into()));
    for t in 0..3 {
        let vs = &l.votes[t];
        v.push((cs::VOTE_TIME + t, if vs.time != 0 { vs.time.to_string() } else { String::new() }));
        v.push((cs::VOTE_STRING + t, if vs.time != 0 { vs.display.clone() } else { String::new() }));
        v.push((cs::VOTE_YES + t, vs.yes.to_string()));
        v.push((cs::VOTE_NO + t, vs.no.to_string()));
        v.push((cs::VOTE_CALLER + t, vs.caller.clone()));
    }
    v.push((cs::CLIENTS_READY, client_list_string(|i| w.clients[i].connected && w.clients[i].ready_to_exit)));
    v.push((cs::ALIEN_STAGES, format!("{} {} {}", l.alien_stage, l.alien_credits, l.alien_next_stage)));
    v.push((cs::HUMAN_STAGES, format!("{} {} {}", l.human_stage, l.human_credits, l.human_next_stage)));
    for (i, n) in w.model_names.iter().enumerate().skip(1).take(cs::MAX_MODELS - 1) {
        v.push((cs::MODELS + i, n.clone()));
    }
    for (i, n) in w.sound_names.iter().enumerate().skip(1).take(cs::MAX_SOUNDS - 1) {
        v.push((cs::SOUNDS + i, n.clone()));
    }
    for (i, n) in w.shader_names.iter().enumerate().skip(1).take(cs::MAX_GAME_SHADERS - 1) {
        v.push((cs::SHADERS + i, n.clone()));
    }
    for (i, n) in w.particle_names.iter().enumerate().skip(1).take(cs::MAX_GAME_PARTICLE_SYSTEMS - 1) {
        v.push((cs::PARTICLE_SYSTEMS + i, n.clone()));
    }
    for i in 0..MAX_CLIENTS {
        v.push((cs::PLAYERS + i, client_info_string(w, i)));
    }
    for (i, n) in w.location_names.iter().enumerate().skip(1).take(cs::MAX_LOCATIONS - 1) {
        v.push((cs::LOCATIONS + i, n.clone()));
    }
    v
}

/// `G_UpdateTeamConfigStrings`: the other team's votes and stages are
/// restricted (sent empty) to a client of `team`.
pub fn configstring_restricted(index: usize, team: Team) -> bool {
    let t = |x: Team| x as usize;
    match team {
        Team::Humans => [cs::VOTE_TIME, cs::VOTE_STRING, cs::VOTE_YES, cs::VOTE_NO].iter().any(|b| index == b + t(Team::Aliens)) || index == cs::ALIEN_STAGES,
        Team::Aliens => [cs::VOTE_TIME, cs::VOTE_STRING, cs::VOTE_YES, cs::VOTE_NO].iter().any(|b| index == b + t(Team::Humans)) || index == cs::HUMAN_STAGES,
        Team::None => false,
    }
}

// ================================================================ server commands

/// `ScoreboardMessage` for client `to`; `pings[i]` is the server's cl->ping
/// (-1 while connecting).
pub fn scoreboard_message(w: &World, to: usize, pings: &[i32]) -> String {
    let mut sorted: Vec<usize> = (0..MAX_CLIENTS).filter(|&i| w.clients[i].connected).collect();
    // level.sortedClients (CalculateRanks: by score)
    sorted.sort_by_key(|&i| -w.clients[i].score);
    let my_team = w.clients[to].team;
    let mut s = String::new();
    for i in sorted {
        let cl = &w.clients[i];
        let ping = pings.get(i).copied().unwrap_or(0).min(999);
        let (weapon, upgrade) = if cl.spectator == SpectatorState::Not as i32 && (my_team == Team::None || cl.team == my_team) {
            let order = [UpgradeId::Bsuit, UpgradeId::Jetpack, UpgradeId::Battpack, UpgradeId::Helmet, UpgradeId::Larmour];
            (cl.ps.weapon, order.into_iter().find(|u| bg::inventory_contains_upgrade(*u as i32, &cl.ps.stats)).map(|u| u as i32).unwrap_or(0))
        } else {
            (0, 0)
        };
        let entry = format!(" {} {} {} {} {} {}", i, cl.score, ping, (w.level.time - cl.enter_time) / 60000, weapon, upgrade);
        if s.len() + entry.len() >= 1400 {
            break;
        }
        s.push_str(&entry);
    }
    // level.alienKills / humanKills
    let kills = |t: Team| (0..MAX_CLIENTS).filter(|&i| w.clients[i].connected && w.clients[i].team == t).map(|i| w.clients[i].kills).sum::<i32>();
    format!("scores {} {}{}", kills(Team::Aliens), kills(Team::Humans), s)
}

/// `TeamplayInfoMessage` for client `to` (every team mate, not only the
/// changed ones); None when `to` has no team to report on.
pub fn teamplay_info_message(w: &World, to: usize) -> Option<String> {
    let me = &w.clients[to];
    let team = if me.team == Team::None {
        if me.spectator == SpectatorState::Free as i32 || me.spectator_client < 0 {
            return None;
        }
        w.clients.get(me.spectator_client as usize)?.team
    } else {
        me.team
    };
    let mut s = String::new();
    for i in 0..MAX_CLIENTS {
        let cl = &w.clients[i];
        if i == to || !cl.connected || cl.team != team {
            continue;
        }
        let (cur, upgrade) = if cl.spectator != SpectatorState::Not as i32 {
            (0, 0)
        } else if cl.team == Team::Humans {
            let order = [UpgradeId::Bsuit, UpgradeId::Jetpack, UpgradeId::Battpack, UpgradeId::Helmet, UpgradeId::Larmour];
            (cl.ps.weapon, order.into_iter().find(|u| bg::inventory_contains_upgrade(*u as i32, &cl.ps.stats)).map(|u| u as i32).unwrap_or(0))
        } else {
            (cl.ps.stats[Stat::Class as usize], 0)
        };
        let location = super::team::get_location(w, i).map(|l| w.ents[l].generic1).unwrap_or(0);
        let health = cl.ps.stats[Stat::Health as usize].max(0);
        if team == Team::Aliens {
            s.push_str(&format!(" {i} {location} {health} {cur}"));
        } else {
            s.push_str(&format!(" {i} {location} {health} {cur} {upgrade}"));
        }
    }
    Some(format!("tinfo{s}"))
}

/// Quote a server command argument the way `va( "... \"%s\"" )` does.
pub fn quoted(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "'"))
}

/// `G_SayTo`'s mode number (saymode_t).
pub fn say_mode_num(m: super::cmds::SayMode) -> i32 {
    use super::cmds::SayMode::*;
    match m {
        All => 0,
        Team => 1,
        Private => 2,
        TeamPrivate => 3,
        Area => 4,
        Admins => 5,
        AdminsPublic => 6,
        Raw => 7,
    }
}

/// saymode_t back to the port's SayMode.
pub fn say_mode_from_num(n: i32) -> super::cmds::SayMode {
    use super::cmds::SayMode::*;
    match n {
        1 => Team,
        2 => Private,
        3 => TeamPrivate,
        4 => Area,
        5 => Admins,
        6 => AdminsPublic,
        7 => Raw,
        _ => All,
    }
}

/// What the server has already turned into commands.
#[derive(Clone, Debug, Default)]
pub struct Sent {
    /// each client's last `message` sent
    pub last_msg: Vec<Option<(i32, String)>>,
    /// `World::center_prints` entries already sent
    pub center_prints: usize,
}

/// The game's queued output as `trap_SendServerCommand` lines: (client or
/// None for everyone, command). Drains `World::prints` and `chat`; sends
/// the `center_prints` and each client's latest `message` not sent yet.
pub fn drain_server_commands(w: &mut World, sent: &mut Sent) -> Vec<(Option<usize>, String)> {
    if sent.last_msg.len() < MAX_CLIENTS {
        sent.last_msg.resize(MAX_CLIENTS, None);
    }
    let last_msg = &mut sent.last_msg;
    let mut out = std::mem::take(&mut w.server_commands);
    let target = |w: &World, tm: Option<Team>, cl: Option<usize>| -> Vec<Option<usize>> {
        match (tm, cl) {
            (None, None) => vec![None],
            (_, Some(c)) => vec![Some(c)],
            (Some(t), None) => (0..MAX_CLIENTS).filter(|&i| w.clients[i].connected && w.clients[i].team == t).map(Some).collect(),
        }
    };
    for (_, tm, cl, text) in std::mem::take(&mut w.prints) {
        for to in target(w, tm, cl) {
            out.push((to, format!("print {}", quoted(&format!("{text}\n")))));
        }
    }
    for (_, to, from, mode, line) in std::mem::take(&mut w.chat) {
        let from_num = if from == super::world::CONSOLE { -1 } else { from as i32 };
        out.push((Some(to), format!("chat {} {} {}", from_num, say_mode_num(mode), quoted(&line))));
    }
    // center prints stay in the World (the single-process HUD reads them);
    // send the ones not sent yet
    if sent.center_prints > w.center_prints.len() {
        sent.center_prints = 0;
    }
    for (_, tm, cl, text) in w.center_prints[sent.center_prints..].iter() {
        for to in target(w, *tm, *cl) {
            out.push((to, format!("cp {}", quoted(text))));
        }
    }
    sent.center_prints = w.center_prints.len();
    if w.center_prints.len() > 64 {
        w.center_prints.drain(..32);
        sent.center_prints -= 32;
    }
    for i in 0..MAX_CLIENTS.min(last_msg.len()) {
        let c = &mut w.clients[i];
        if !c.connected {
            last_msg[i] = None;
            continue;
        }
        if c.message != last_msg[i] {
            last_msg[i] = c.message.clone();
            if let Some((_, text)) = &c.message {
                // G_TriggerMenu's notice (dec033: the text the cgame's menus key on)
                out.push((Some(i), format!("servermenu 0 {} {}", quoted(text), std::mem::take(&mut c.wants_menu) as i32)));
            }
        }
    }
    out
}
