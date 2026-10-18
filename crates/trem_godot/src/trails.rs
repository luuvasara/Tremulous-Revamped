//! Client-side trail systems: a port of Tremulous `cg_trails.c` (beams made of
//! nodes, single-attached fading trails and double-attached zaps with
//! jitter). Godot-free; `particles::FxRenderer` draws the `QuadBatch`es.
//!
//! ```ignore
//! let tlib = TrailLib::load(&files);                       // scripts/*.trail
//! let h = tlib.handle("models/buildables/tesla/zap")?;     // CG_RegisterTrailSystem
//! let id = trails.spawn(&tlib, h, Attach::Entity(src), Some(Attach::Entity(dst)));  // 0 = full
//! trails.set_offset(id, true, [0.0, 0.0, 28.0]);           // CG_SetAttachmentOffset(front)
//! trails.destroy(id);                                      // CG_DestroyTrailSystem (fades out)
//! trails.is_valid(id);  trails.endpoints(id);
//! trails.update(&tlib, now_ms, &resolve, &particles);      // CG_AddTrails, after Particles::update
//! trails.quads(&tlib, cam_origin) -> Vec<QuadBatch>
//! ```
//!
//! As in C, `destroy` does not invalidate the id: the system stays valid
//! until its beams have faded/shrunk away.

// Until game.rs wires the API in, most of it is unused from the cdylib's view.

use std::collections::{HashMap, VecDeque};

use trem_sim::kernel::qmath::{self as qm, Vec3};

use crate::particles::{atof_neg, atoi_neg, color_byte, Attach, Attachment, BatchBuilder, FxVert, Lexer, PLookup, Particles, QuadBatch, Resolver, Rng};

pub const MAX_BEAMS_PER_SYSTEM: usize = 4;
pub const MAX_TRAIL_SYSTEMS: usize = 32;
pub const MAX_TRAIL_BEAMS: usize = MAX_TRAIL_SYSTEMS * MAX_BEAMS_PER_SYSTEM;
pub const MAX_TRAIL_BEAM_NODES: usize = 128;
pub const MAX_TRAIL_BEAM_JITTERS: usize = 4;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TextureType {
    #[default]
    Stretch,
    Repeat,
}

/// `baseTrailJitter_t`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Jitter {
    pub magnitude: f32,
    pub period: i32,
}

/// `baseTrailBeam_t`.
#[derive(Clone, Debug, PartialEq)]
pub struct BaseBeam {
    pub num_segments: i32,
    pub front_width: f32,
    pub back_width: f32,
    pub front_alpha: f32,
    pub back_alpha: f32,
    pub front_color: [u8; 3],
    pub back_color: [u8; 3],
    /// time for a segment to vanish (single attached only)
    pub segment_time: i32,
    /// time for the beam to fade out (double attached only)
    pub fade_out_time: i32,
    pub shader: String,
    pub texture_type: TextureType,
    pub front_texture_coord: f32,
    pub back_texture_coord: f32,
    pub repeat_length: f32,
    pub clamp_to_back: bool,
    pub real_light: bool,
    pub jitters: Vec<Jitter>,
    pub jitter_attachments: bool,
}

impl Default for BaseBeam {
    /// `CG_InitialiseBaseTrailBeam`.
    fn default() -> Self {
        BaseBeam {
            num_segments: 1,
            front_width: 1.0,
            back_width: 1.0,
            front_alpha: 1.0,
            back_alpha: 1.0,
            front_color: [0xFF; 3],
            back_color: [0xFF; 3],
            segment_time: 100,
            fade_out_time: 0,
            shader: String::new(),
            texture_type: TextureType::Stretch,
            front_texture_coord: 0.0,
            back_texture_coord: 1.0,
            repeat_length: 0.0,
            clamp_to_back: false,
            real_light: false,
            jitters: Vec::new(),
            jitter_attachments: false,
        }
    }
}

/// `baseTrailSystem_t`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BaseTrailSystem {
    pub name: String,
    pub beams: Vec<BaseBeam>,
    pub life_time: i32,
    pub third_person_only: bool,
}

type PResult<T> = Result<T, String>;

fn need(lx: &mut Lexer) -> PResult<String> {
    let t = lx.parse();
    if t.is_empty() {
        Err("unexpected end of file".into())
    } else {
        Ok(t)
    }
}

/// `CG_ParseTrailBeamColor` + the closing brace.
fn parse_color(lx: &mut Lexer) -> PResult<[u8; 3]> {
    let mut c = [0u8; 3];
    for ch in c.iter_mut() {
        *ch = color_byte(&need(lx)?);
    }
    if !lx.parse().eq_ignore_ascii_case("}") {
        return Err("missing '}'".into());
    }
    Ok(c)
}

/// `CG_ParseTrailBeam`. Warnings (non-fatal) go to `warn`.
fn parse_beam(lx: &mut Lexer, b: &mut BaseBeam, warn: &mut Vec<String>) -> PResult<()> {
    loop {
        let token = lx.parse();
        if token.is_empty() {
            return Err("unexpected end of file in trail beam".into());
        }
        match token.to_lowercase().as_str() {
            "segments" => {
                b.num_segments = atoi_neg(&need(lx)?, false);
                if b.num_segments >= MAX_TRAIL_BEAM_NODES as i32 {
                    b.num_segments = MAX_TRAIL_BEAM_NODES as i32 - 1;
                    warn.push("WARNING: too many segments in trail beam".into());
                }
            }
            "width" => {
                b.front_width = atof_neg(&need(lx)?, false);
                let t = need(lx)?;
                b.back_width = if t == "-" { b.front_width } else { atof_neg(&t, false) };
            }
            "alpha" => {
                b.front_alpha = atof_neg(&need(lx)?, false);
                let t = need(lx)?;
                b.back_alpha = if t == "-" { b.front_alpha } else { atof_neg(&t, false) };
            }
            "color" => {
                if need(lx)? != "{" {
                    return Err("missing '{'".into());
                }
                b.front_color = parse_color(lx)?;
                let t = need(lx)?;
                if t == "-" {
                    b.back_color = b.front_color;
                } else if t == "{" {
                    b.back_color = parse_color(lx)?;
                } else {
                    return Err("missing '{'".into());
                }
            }
            "segmenttime" => b.segment_time = atoi_neg(&need(lx)?, false),
            "fadeouttime" => b.fade_out_time = atoi_neg(&need(lx)?, false),
            "shader" => b.shader = need(lx)?,
            "texturetype" => {
                let t = need(lx)?;
                if t.eq_ignore_ascii_case("stretch") {
                    b.texture_type = TextureType::Stretch;
                    b.front_texture_coord = atof_neg(&need(lx)?, false);
                    b.back_texture_coord = atof_neg(&need(lx)?, false);
                } else if t.eq_ignore_ascii_case("repeat") {
                    b.texture_type = TextureType::Repeat;
                    let c = need(lx)?;
                    if c.eq_ignore_ascii_case("front") {
                        b.clamp_to_back = false;
                    } else if c.eq_ignore_ascii_case("back") {
                        b.clamp_to_back = true;
                    } else {
                        return Err(format!("unknown textureType clamp \"{c}\""));
                    }
                    b.repeat_length = atof_neg(&need(lx)?, false);
                } else {
                    return Err(format!("unknown textureType \"{t}\""));
                }
            }
            "reallight" => b.real_light = true,
            "jitter" => {
                if b.jitters.len() == MAX_TRAIL_BEAM_JITTERS {
                    return Err("too many jitters".into());
                }
                let magnitude = atof_neg(&need(lx)?, false);
                let period = atoi_neg(&need(lx)?, false);
                b.jitters.push(Jitter { magnitude, period });
            }
            "jitterattachments" => b.jitter_attachments = true,
            "}" => return Ok(()),
            _ => return Err(format!("unknown token '{token}' in trail beam")),
        }
    }
}

/// `CG_ParseTrailSystem`.
fn parse_system(lx: &mut Lexer, name: &str, warn: &mut Vec<String>) -> PResult<BaseTrailSystem> {
    let mut s = BaseTrailSystem { name: name.to_string(), ..Default::default() };
    loop {
        let token = lx.parse();
        if token.is_empty() {
            return Err("unexpected end of file in trail system".into());
        }
        match token.to_lowercase().as_str() {
            "{" => {
                let mut b = BaseBeam::default();
                parse_beam(lx, &mut b, warn).map_err(|e| format!("{e}; failed to parse trail beam"))?;
                if s.beams.len() == MAX_BEAMS_PER_SYSTEM {
                    return Err(format!("trail system has > {MAX_BEAMS_PER_SYSTEM} beams"));
                }
                s.beams.push(b);
            }
            "thirdpersononly" => s.third_person_only = true,
            "lifetime" => s.life_time = atoi_neg(&need(lx)?, false),
            "beam" => {}
            "}" => return Ok(s),
            _ => return Err(format!("unknown token '{token}' in trail system {name}")),
        }
    }
}

/// Parsed trail templates (`baseTrailSystems[]`).
#[derive(Clone, Debug, Default)]
pub struct TrailLib {
    pub systems: Vec<BaseTrailSystem>,
    by_name: HashMap<String, usize>,
    /// Errors/warnings the C would print (empty for the stock data).
    pub errors: Vec<String>,
}

impl TrailLib {
    /// `CG_LoadTrailSystems`: every `*.trail` among `files`, in path order.
    pub fn load(files: &[(String, String)]) -> Self {
        let mut lib = TrailLib::default();
        let mut list: Vec<&(String, String)> = files.iter().filter(|(p, _)| p.to_lowercase().ends_with(".trail")).collect();
        list.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()));
        for (path, text) in list {
            lib.parse_file(path, text);
        }
        lib
    }

    /// `CG_ParseTrailFile`.
    fn parse_file(&mut self, path: &str, text: &str) -> bool {
        let mut lx = Lexer::new(text);
        let mut name: Option<String> = None;
        loop {
            let token = lx.parse();
            if token.is_empty() {
                break;
            }
            if token == "{" {
                let Some(n) = name.take() else {
                    self.errors.push(format!("{path}: unnamed trail system"));
                    return false;
                };
                if self.by_name.contains_key(&n.to_lowercase()) {
                    self.errors.push(format!("{path}: a trail system is already named {n}"));
                    return false;
                }
                let mut warn = Vec::new();
                let r = parse_system(&mut lx, &n, &mut warn);
                self.errors.extend(warn.into_iter().map(|w| format!("{path}: {w}")));
                match r {
                    Ok(s) => {
                        self.by_name.insert(n.to_lowercase(), self.systems.len());
                        self.systems.push(s);
                    }
                    Err(e) => {
                        self.errors.push(format!("{path}: {e}; failed to parse trail system {n}"));
                        return false;
                    }
                }
                continue;
            }
            if name.is_none() {
                name = Some(token);
            } else {
                self.errors.push(format!("{path}: trail system already named"));
                return false;
            }
        }
        true
    }

    /// `CG_RegisterTrailSystem` (case-insensitive).
    pub fn handle(&self, name: &str) -> Option<usize> {
        self.by_name.get(&name.to_lowercase()).copied()
    }

    #[allow(dead_code)] // API used by tests / callers to come
    pub fn get(&self, handle: usize) -> Option<&BaseTrailSystem> {
        self.systems.get(handle)
    }

    pub fn shader_names(&self) -> Vec<String> {
        let mut v: Vec<String> = Vec::new();
        for b in self.systems.iter().flat_map(|s| &s.beams) {
            if !v.contains(&b.shader) {
                v.push(b.shader.clone());
            }
        }
        v
    }
}

// ---------------------------------------------------------------------------
// run time

#[derive(Clone, Debug, Default)]
struct Node {
    ref_pos: Vec3,
    pos: Vec3,
    time_left: i32,
    jitters: [[f32; 2]; MAX_TRAIL_BEAM_JITTERS],
}

#[derive(Clone, Debug)]
struct Beam {
    base: usize,
    /// front (index 0) to back
    nodes: VecDeque<Node>,
    last_eval: i32,
    valid: bool,
    next_jitter: [i32; MAX_TRAIL_BEAM_JITTERS],
}

#[derive(Clone, Debug)]
struct TrailSystem {
    id: u32,
    handle: usize,
    front: Attachment,
    back: Attachment,
    birth: i32,
    destroy_time: i32,
    valid: bool,
    beams: Vec<Beam>,
}

/// Live trail systems and beams.
pub struct Trails {
    systems: Vec<Option<TrailSystem>>,
    next_id: u32,
    now: i32,
    rng: Rng,
    /// Local client while rendering first person (hides `thirdPersonOnly`).
    pub first_person_client: Option<i32>,
}

impl Default for Trails {
    fn default() -> Self {
        Self::new()
    }
}

/// `CG_DestroyTrailSystem`.
fn destroy_system(ts: &mut TrailSystem, now: i32, lk: &PLookup) {
    ts.destroy_time = now;
    if ts.front.attached() && !ts.back.attached() {
        // pin the trail head to a static point
        let _ = ts.front.point(lk);
        let v = ts.front.last_valid;
        ts.front.set(Attach::Point(v));
        ts.front.clear_cent();
    }
}

/// `GetPerpendicularViewVector`.
fn perp_view(point: &Vec3, p1: &Vec3, p2: &Vec3) -> Vec3 {
    let v1 = qm::normalized(&qm::sub(point, p1));
    let v2 = qm::normalized(&qm::sub(point, p2));
    qm::normalized(&qm::cross(&v1, &v2))
}

/// Node "up" vector used for rendering and jitter (`CG_RenderBeam`).
fn node_up(cam: &Vec3, pos: &[Vec3], i: usize) -> Option<Vec3> {
    let n = pos.len();
    let prev = (i > 0).then(|| pos[i - 1]);
    let next = (i + 1 < n).then(|| pos[i + 1]);
    match (prev, next) {
        (Some(p), Some(x)) => Some(perp_view(cam, &x, &p)),
        (None, Some(x)) => Some(perp_view(cam, &x, &pos[i])),
        (Some(p), None) => Some(perp_view(cam, &pos[i], &p)),
        (None, None) => None,
    }
}

impl Trails {
    pub fn new() -> Self {
        Trails { systems: (0..MAX_TRAIL_SYSTEMS).map(|_| None).collect(), next_id: 1, now: 0, rng: Rng::new(0x9e37_79b9), first_person_client: None }
    }

    /// Set `cg.time` for spawns made before the next `update`.
    pub fn set_time(&mut self, now_ms: i32) {
        self.now = now_ms;
    }

    fn slot(&self, id: u32) -> Option<usize> {
        if id == 0 {
            return None;
        }
        self.systems.iter().position(|s| matches!(s, Some(s) if s.valid && s.id == id))
    }

    /// `CG_SpawnNewTrailSystem` + attach front (and optionally back).
    /// Returns 0 when the pool is full or the handle is bad.
    pub fn spawn(&mut self, lib: &TrailLib, handle: usize, front: Attach, back: Option<Attach>) -> u32 {
        let Some(bts) = lib.systems.get(handle) else { return 0 };
        let Some(slot) = self.systems.iter().position(|s| !matches!(s, Some(s) if s.valid)) else { return 0 };
        let beams_in_use: usize = self.systems.iter().flatten().filter(|s| s.valid).map(|s| s.beams.iter().filter(|b| b.valid).count()).sum();
        let room = MAX_TRAIL_BEAMS.saturating_sub(beams_in_use);
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1).max(1);
        let beams = (0..bts.beams.len().min(room)).map(|i| Beam { base: i, nodes: VecDeque::new(), last_eval: 0, valid: true, next_jitter: [0; MAX_TRAIL_BEAM_JITTERS] }).collect();
        let mut back_att = Attachment::default();
        if let Some(b) = back {
            back_att = Attachment::new(b);
        }
        self.systems[slot] = Some(TrailSystem { id, handle, front: Attachment::new(front), back: back_att, birth: self.now, destroy_time: -1, valid: true, beams });
        id
    }

    fn sys_mut(&mut self, id: u32) -> Option<&mut TrailSystem> {
        let s = self.slot(id)?;
        self.systems[s].as_mut()
    }

    /// Re-attach one end (`front` = true for the front attachment).
    #[allow(dead_code)] // API used by tests / callers to come
    pub fn set_attach(&mut self, id: u32, front: bool, attach: Attach) {
        if let Some(s) = self.sys_mut(id) {
            if front {
                s.front.set(attach)
            } else {
                s.back.set(attach)
            }
        }
    }

    /// `CG_SetAttachmentOffset`.
    pub fn set_offset(&mut self, id: u32, front: bool, offset: Vec3) {
        if let Some(s) = self.sys_mut(id) {
            if front {
                s.front.set_offset(offset)
            } else {
                s.back.set_offset(offset)
            }
        }
    }

    /// `CG_DestroyTrailSystem`.
    pub fn destroy(&mut self, id: u32) {
        let now = self.now;
        if let Some(s) = self.sys_mut(id) {
            destroy_system(s, now, &|_| None);
        }
    }

    /// `CG_IsTrailSystemValid`.
    pub fn is_valid(&self, id: u32) -> bool {
        self.slot(id).is_some()
    }

    /// Last known front/back points (for the tesla range check in `CG_EntityEffects`, cg_ents.c).
    pub fn endpoints(&self, id: u32) -> Option<(Vec3, Option<Vec3>)> {
        let s = self.systems[self.slot(id)?].as_ref()?;
        Some((s.front.last_valid, s.back.attached().then_some(s.back.last_valid)))
    }

    /// (systems, beams) alive.
    #[allow(dead_code)] // API used by tests / callers to come
    pub fn counts(&self) -> (usize, usize) {
        let mut a = 0;
        let mut b = 0;
        for s in self.systems.iter().flatten().filter(|s| s.valid) {
            a += 1;
            b += s.beams.iter().filter(|b| b.valid).count();
        }
        (a, b)
    }

    /// `CG_AddTrails` minus rendering. Call after `Particles::update`.
    pub fn update(&mut self, lib: &TrailLib, now_ms: i32, resolve: &Resolver, particles: &Particles) {
        self.now = now_ms;
        let now = now_ms;
        let lk = |r| particles.particle_state(r);
        for ts in self.systems.iter_mut().flatten().filter(|s| s.valid) {
            ts.front.refresh(resolve);
            ts.back.refresh(resolve);
        }
        // CG_GarbageCollectTrailSystems
        for slot in self.systems.iter_mut() {
            let Some(ts) = slot else { continue };
            if !ts.valid {
                continue;
            }
            if !ts.beams.iter().any(|b| b.valid) {
                ts.valid = false;
                *slot = None;
                continue;
            }
            // parent cent left the PVS (guarded so a fading beam is not re-destroyed every frame)
            if ts.front.cent_num().is_some() && !ts.front.cent_alive && ts.destroy_time <= 0 {
                destroy_system(ts, now, &lk);
            }
            if ts.back.cent_num().is_some() && !ts.back.cent_alive && ts.destroy_time <= 0 {
                destroy_system(ts, now, &lk);
            }
            let bts = &lib.systems[ts.handle];
            if ts.destroy_time <= 0 && bts.life_time != 0 && ts.birth + bts.life_time < now {
                destroy_system(ts, now, &lk);
            }
        }
        for ts in self.systems.iter_mut().flatten().filter(|s| s.valid) {
            let mut beams = std::mem::take(&mut ts.beams);
            for b in beams.iter_mut().filter(|b| b.valid) {
                Self::update_beam(&mut self.rng, lib, ts, b, now, &lk);
            }
            ts.beams = beams;
        }
    }

    /// `CG_UpdateBeam` (jitter offsets are generated here, applied in `quads`).
    fn update_beam(rng: &mut Rng, lib: &TrailLib, ts: &mut TrailSystem, tb: &mut Beam, now: i32, lk: &PLookup) {
        let btb = &lib.systems[ts.handle].beams[tb.base];
        let delta = now - tb.last_eval;
        tb.last_eval = now;

        if ts.destroy_time <= 0 || btb.fade_out_time > 0 {
            let to_add = (btb.num_segments - tb.nodes.len() as i32 + 1).max(0);
            for _ in 0..to_add {
                if tb.nodes.len() >= MAX_TRAIL_BEAM_NODES {
                    break;
                }
                let mut n = Node { time_left: btb.segment_time, ..Default::default() };
                if tb.nodes.is_empty() && ts.front.attached() {
                    match ts.front.point(lk) {
                        Some(v) => n.ref_pos = v,
                        None => {
                            n.ref_pos = ts.front.last_valid;
                            destroy_system(ts, now, lk);
                        }
                    }
                } else if let Some(prev) = tb.nodes.back() {
                    n.ref_pos = prev.ref_pos;
                }
                tb.nodes.push_back(n);
            }
        }

        let num_nodes = tb.nodes.len();
        for n in tb.nodes.iter_mut() {
            n.pos = n.ref_pos;
        }

        if ts.front.attached() && ts.back.attached() {
            if ts.destroy_time > 0 && now - ts.destroy_time >= btb.fade_out_time {
                tb.valid = false;
                return;
            }
            let front = match ts.front.point(lk) {
                Some(v) => v,
                None => {
                    destroy_system(ts, now, lk);
                    ts.front.last_valid
                }
            };
            let back = match ts.back.point(lk) {
                Some(v) => v,
                None => {
                    destroy_system(ts, now, lk);
                    ts.back.last_valid
                }
            };
            let dir = qm::sub(&back, &front);
            for (j, n) in tb.nodes.iter_mut().enumerate() {
                let scale = j as f32 / (num_nodes as f32 - 1.0);
                n.pos = qm::ma(&front, scale, &dir);
            }
        } else if ts.front.attached() {
            // cull the trail tail
            let len = tb.nodes.len();
            if len > 0 && tb.nodes[len - 1].time_left >= 0 {
                tb.nodes[len - 1].time_left -= delta;
                if tb.nodes[len - 1].time_left < 0 {
                    tb.nodes.pop_back();
                    if tb.nodes.is_empty() {
                        tb.valid = false;
                        return;
                    }
                    // once destroyed, stop creating new nodes
                    if ts.destroy_time <= 0 && tb.nodes.len() < MAX_TRAIL_BEAM_NODES {
                        let rp = tb.nodes[0].ref_pos;
                        tb.nodes.push_front(Node { time_left: btb.segment_time, ref_pos: rp, pos: rp, ..Default::default() });
                    }
                } else if len > 1 {
                    let prev = tb.nodes[len - 2].ref_pos;
                    let last = &mut tb.nodes[len - 1];
                    let mut dir = qm::sub(&last.ref_pos, &prev);
                    let length = qm::normalize(&mut dir) * (last.time_left as f32 / btb.segment_time as f32);
                    last.pos = qm::ma(&prev, length, &dir);
                }
            }
            if !tb.nodes.is_empty() {
                let p = match ts.front.point(lk) {
                    Some(v) => v,
                    None => {
                        destroy_system(ts, now, lk);
                        ts.front.last_valid
                    }
                };
                tb.nodes[0].ref_pos = p;
                tb.nodes[0].pos = p;
            }
        }

        // CG_ApplyJitters, part 1: new random offsets when each period elapses
        for (j, jit) in btb.jitters.iter().enumerate() {
            if tb.next_jitter[j] <= now {
                for n in tb.nodes.iter_mut() {
                    n.jitters[j][0] = rng.crandom() * jit.magnitude;
                    n.jitters[j][1] = rng.crandom() * jit.magnitude;
                }
                tb.next_jitter[j] = now + jit.period;
            }
        }
    }

    /// Beam strips (`CG_ApplyJitters` part 2 + `CG_CalculateBeamNodeProperties`
    /// + `CG_RenderBeam`), grouped by shader.
    pub fn quads(&self, lib: &TrailLib, cam_origin: Vec3) -> Vec<QuadBatch> {
        let mut bb = BatchBuilder::default();
        let cam = cam_origin;
        for ts in self.systems.iter().flatten().filter(|s| s.valid) {
            let bts = &lib.systems[ts.handle];
            if bts.third_person_only && self.first_person_client.is_some() && (ts.front.cent_num() == self.first_person_client || ts.back.cent_num() == self.first_person_client) {
                continue;
            }
            for tb in ts.beams.iter().filter(|b| b.valid && b.nodes.len() > 1) {
                let btb = &bts.beams[tb.base];
                let n = tb.nodes.len();
                let mut pos: Vec<Vec3> = tb.nodes.iter().map(|x| x.pos).collect();

                // jitter (applied in place, front to back, like the C)
                if !btb.jitters.is_empty() {
                    let mut start = 0;
                    let mut end = n - 1;
                    if !btb.jitter_attachments {
                        if ts.front.attached() && n > 1 {
                            start = 1;
                        }
                        if ts.back.attached() && n > 1 {
                            end = n - 2;
                        }
                    }
                    let mut i = start;
                    while i < n {
                        if let Some(up) = node_up(&cam, &pos, i) {
                            let forward = if i > 0 && i + 1 < n {
                                qm::sub(&pos[i + 1], &pos[i - 1])
                            } else if i + 1 < n {
                                qm::sub(&pos[i + 1], &pos[i])
                            } else {
                                qm::sub(&pos[i], &pos[i - 1])
                            };
                            let forward = qm::normalized(&forward);
                            let right = qm::normalized(&qm::cross(&forward, &up));
                            let mut uj = 0.0;
                            let mut rj = 0.0;
                            for j in 0..btb.jitters.len() {
                                uj += tb.nodes[i].jitters[j][0];
                                rj += tb.nodes[i].jitters[j][1];
                            }
                            pos[i] = qm::ma(&pos[i], uj, &up);
                            pos[i] = qm::ma(&pos[i], rj, &right);
                        }
                        if i == end {
                            break;
                        }
                        i += 1;
                    }
                }

                // node properties
                let mut fade = 1.0f32;
                if ts.destroy_time > 0 && btb.fade_out_time != 0 {
                    fade -= (self.now - ts.destroy_time) as f32 / btb.fade_out_time as f32;
                    fade = fade.max(0.0);
                }
                let dists: Vec<f32> = (0..n - 1).map(|k| qm::distance(&pos[k], &pos[k + 1])).collect();
                let total: f32 = dists.iter().sum();
                let tc_range = btb.back_texture_coord - btb.front_texture_coord;
                let w_range = btb.back_width - btb.front_width;
                let a_range = btb.back_alpha - btb.front_alpha;
                let mut tc = vec![0.0f32; n];
                let mut hw = vec![0.0f32; n];
                let mut rgba = vec![[0u8; 4]; n];
                let mut position = 0.0f32;
                for j in 0..n {
                    let f = if total > 0.0 { position / total } else { 0.0 };
                    tc[j] = match btb.texture_type {
                        TextureType::Stretch => btb.front_texture_coord + f * tc_range,
                        TextureType::Repeat if btb.clamp_to_back => (total - position) / btb.repeat_length,
                        TextureType::Repeat => position / btb.repeat_length,
                    };
                    hw[j] = (btb.front_width + f * w_range) / 2.0;
                    let a = 255.0 * (btb.front_alpha + f * a_range) * fade;
                    for k in 0..3 {
                        let c = btb.front_color[k] as f32 + f * (btb.back_color[k] as f32 - btb.front_color[k] as f32);
                        rgba[j][k] = c.clamp(0.0, 255.0) as u8;
                    }
                    rgba[j][3] = a.clamp(0.0, 255.0) as u8;
                    if j < n - 1 {
                        position += dists[j];
                    }
                }

                // strip: one quad per segment
                let ups: Vec<Vec3> = (0..n).map(|i| node_up(&cam, &pos, i).unwrap_or([0.0; 3])).collect();
                for k in 0..n - 1 {
                    let v = |i: usize, s: f32, t: f32| FxVert { pos: qm::ma(&pos[i], s * hw[i], &ups[i]), uv: [tc[i], t], rgba: rgba[i] };
                    bb.push(&btb.shader, [v(k, -1.0, 0.0), v(k, 1.0, 1.0), v(k + 1, 1.0, 1.0), v(k + 1, -1.0, 0.0)]);
                }
            }
        }
        bb.out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::particles::AttachPos;

    fn data_files() -> Vec<(String, String)> {
        let d = crate::testdata::d110();
        let v: Vec<(String, String)> = d.list("scripts", &[".particle", ".trail"]).into_iter().map(|f| (f.clone(), d.must_text(&f))).collect();
        assert!(!v.is_empty(), "no scripts/*.particle or *.trail in the test data (see testdata.rs)");
        v
    }

    #[test]
    fn trails_parse_all_data() {
        let files = data_files();
        let lib = TrailLib::load(&files);
        assert!(lib.errors.is_empty(), "{:?}", lib.errors);
        assert_eq!(lib.systems.len(), 3);
        let pr = lib.get(lib.handle("models/weapons/prifle/missileTS").unwrap()).unwrap();
        assert_eq!(pr.beams.len(), 1);
        let b = &pr.beams[0];
        assert_eq!((b.num_segments, b.segment_time), (2, 120));
        assert_eq!((b.front_width, b.back_width, b.front_alpha, b.back_alpha), (6.0, 4.0, 1.0, 0.1));
        assert_eq!(b.texture_type, TextureType::Stretch);
        assert_eq!(b.shader, "gfx/prifle/red_streak");

        let lz = lib.get(lib.handle("models/weapons/lev2zap/lightning").unwrap()).unwrap();
        assert_eq!(lz.beams.len(), 2);
        assert_eq!(lz.beams[0].jitters, vec![Jitter { magnitude: 4.0, period: 200 }, Jitter { magnitude: 1.0, period: 20 }]);
        assert_eq!(lz.beams[0].front_color, [204, 204, 204]);
        assert_eq!(lz.beams[0].back_color, [204, 204, 204]);
        assert!(!lz.beams[0].clamp_to_back && lz.beams[1].clamp_to_back);
        assert_eq!(lz.beams[1].repeat_length, 50.0);

        let tz = lib.get(lib.handle("MODELS/BUILDABLES/TESLA/ZAP").unwrap()).unwrap();
        assert_eq!(tz.beams[0].num_segments, 9);
        assert_eq!(tz.beams[1].front_color, [127, 204, 255]);
    }

    #[test]
    fn trail_parse_errors() {
        let l = TrailLib::load(&[("scripts/a.trail".into(), "x { beam { foo } }".into())]);
        assert!(l.errors[0].contains("unknown token 'foo'"));
        let l = TrailLib::load(&[("scripts/a.trail".into(), "x { beam { segments 500 } } x { }".into())]);
        assert_eq!(l.systems.len(), 1);
        assert_eq!(l.systems[0].beams[0].num_segments, 127);
        assert!(l.errors.iter().any(|e| e.contains("too many segments")));
        assert!(l.errors.iter().any(|e| e.contains("already named")));
    }

    #[test]
    fn missile_trail_follows_and_shrinks() {
        let files = data_files();
        let lib = TrailLib::load(&files);
        let ps = Particles::new();
        let mut tr = Trails::new();
        let h = lib.handle("models/weapons/prifle/missileTS").unwrap();
        tr.set_time(1000);
        let id = tr.spawn(&lib, h, Attach::Entity(5), None);
        let pos = std::cell::Cell::new([0.0f32; 3]);
        let alive = std::cell::Cell::new(true);
        let resolve = |a: &Attach| match a {
            Attach::Entity(5) if alive.get() => Some(AttachPos { velocity: [1000.0, 0.0, 0.0], ..AttachPos::at(pos.get()) }),
            _ => None,
        };
        for t in 0..30 {
            let now = 1000 + t * 16;
            pos.set([t as f32 * 16.0, 0.0, 0.0]);
            tr.update(&lib, now, &resolve, &ps);
        }
        assert!(tr.is_valid(id));
        let q = tr.quads(&lib, [0.0, -200.0, 0.0]);
        assert_eq!(q.len(), 1);
        assert_eq!(q[0].quads(), 2, "2 segments");
        // the head is on the missile
        let head = q[0].verts[0].pos;
        assert!((head[0] - 29.0 * 16.0).abs() < 4.0, "{head:?}");
        // the missile explodes: entity gone -> trail pinned and shrinks away
        alive.set(false);
        for t in 30..80 {
            tr.update(&lib, 1000 + t * 16, &resolve, &ps);
        }
        assert!(!tr.is_valid(id));
        assert_eq!(tr.counts(), (0, 0));
    }

    #[test]
    fn zap_between_two_entities() {
        let files = data_files();
        let lib = TrailLib::load(&files);
        let ps = Particles::new();
        let mut tr = Trails::new();
        let h = lib.handle("models/buildables/tesla/zap").unwrap();
        let id = tr.spawn(&lib, h, Attach::Entity(1), Some(Attach::Entity(2)));
        tr.set_offset(id, true, [0.0, 0.0, 28.0]);
        let resolve = |a: &Attach| match a {
            Attach::Entity(1) => Some(AttachPos::at([0.0, 0.0, 0.0])),
            Attach::Entity(2) => Some(AttachPos::at([300.0, 0.0, 0.0])),
            _ => None,
        };
        for t in 0..5 {
            tr.update(&lib, t * 16, &resolve, &ps);
        }
        let (f, b) = tr.endpoints(id).unwrap();
        assert_eq!(f, [0.0, 0.0, 28.0]);
        assert_eq!(b, Some([300.0, 0.0, 0.0]));
        let q = tr.quads(&lib, [150.0, -300.0, 0.0]);
        assert_eq!(q.len(), 1, "both beams share the shader");
        assert_eq!(q[0].quads(), 9 + 9);
        // jitter displaces interior nodes but not the attached ends
        let (a, b) = (q[0].verts[0].pos, q[0].verts[1].pos);
        let mid = [(a[0] + b[0]) / 2.0, (a[1] + b[1]) / 2.0, (a[2] + b[2]) / 2.0];
        assert!(qm::distance(&mid, &[0.0, 0.0, 28.0]) < 1e-3, "{mid:?}");
        assert!((qm::distance(&a, &b) - 8.0).abs() < 1e-3, "width 8");
        // interior nodes are jittered off the straight line
        let interior = q[0].verts[4].pos;
        assert!(interior[1].abs() > 1e-4 || (interior[2] - 28.0 * (1.0 - interior[0] / 300.0)).abs() > 1e-4);
        // fadeOutTime 0: destroyed beams vanish on the next update
        tr.destroy(id);
        assert!(tr.is_valid(id));
        tr.update(&lib, 100, &resolve, &ps);
        tr.update(&lib, 116, &resolve, &ps);
        assert!(!tr.is_valid(id));
    }
}
