//! Client-side particle systems: a port of Tremulous `cg_particles.c` (plus the
//! parts of `cg_attachment.c` it needs). Trails live in `trails.rs`.
//!
//! The parser and the simulation are plain Rust (no Godot types) so they can be
//! unit-tested; the Godot side is `FxRenderer` at the bottom of this file.
//!
//! # API
//!
//! ```ignore
//! // load once (pass every scripts/*.particle / scripts/*.trail file; others are ignored)
//! let plib = ParticleLib::load(&files);            // files: &[(path, contents)]
//! let tlib = TrailLib::load(&files);
//! plib.handle("models/weapons/rifle/impactPS") -> Option<usize>   // CG_RegisterParticleSystem
//! tlib.handle("models/weapons/prifle/missileTS") -> Option<usize> // CG_RegisterTrailSystem
//!
//! let mut ps = Particles::new();   let mut ts = Trails::new();
//! ps.set_time(now_ms);                       // optional: cg.time for spawns made before update()
//! let id: u32 = ps.spawn(&plib, h, Attach::Point(origin));   // 0 = pool full
//! ps.set_normal(id, dir);  ps.set_charge(id, c);  ps.set_cent(id, ent);
//! ps.set_attach(id, Attach::...);  ps.set_offset(id, off);
//! ps.destroy(id);  ps.is_valid(id) -> bool;  ps.is_infinite(id) -> bool;
//! ps.update(&plib, &tlib, now_ms, &cm, &resolve, &mut ts);  // CG_AddParticles (gc+eject+physics)
//! ts.update(&tlib, now_ms, &resolve, &ps);                   // CG_AddTrails (after particles)
//! ps.quads(&plib, cam_origin, cam_axis) -> Vec<QuadBatch>    // camera-facing sprites
//! ps.models(&plib) -> Vec<FxModel>; ps.lights(&plib) -> Vec<FxLight>
//! ts.quads(&tlib, cam_origin) -> Vec<QuadBatch>
//! // Godot:
//! let mut fx = FxRenderer::new(&mut library, &parent_node3d, &plib, &tlib);
//! fx.draw(&mut library, &[ps_quads, ts_quads].concat(), &ps.models(&plib), &ps.lights(&plib));
//! ```
//!
//! `resolve(&Attach::Entity(n))` must return the entity's lerped origin, its
//! axis (`AnglesToAxis(lerpAngles)`) and velocity (`pos.trDelta`), or `None`
//! when the entity is no longer valid (systems whose cent is gone are lazily
//! removed, trails destroyed, exactly like `CG_Garbage*`). The local player
//! must always resolve. `resolve(&Attach::Tag{entity, tag})` returns the world
//! transform of that model tag (origin + axis rows forward/left/up). Both are
//! in Quake units. `Attach::Point` and `Attach::Particle` never reach `resolve`.
//!
//! Attachments follow `attachment_t`: one "attached" kind plus independent
//! extras. `Attach::Entity(n)`/`Attach::Tag{entity:n,..}` also set the cent
//! (used for parent velocity, PVS removal and `thirdPersonOnly`);
//! `set_cent` adds a cent to a point attachment (bleed / no-gun muzzle).

// Until game.rs wires the API in, most of it is unused from the cdylib's view.

use std::collections::HashMap;

use trem_sim::gen::enums::contents;
use trem_sim::kernel::cm::ClipMap;
use trem_sim::kernel::qmath::{self as qm, Vec3};

pub use crate::trails::{TrailLib, Trails};

// ---------------------------------------------------------------------------
// limits (cg_local.h)

pub const MAX_PARTICLE_SYSTEMS: usize = 48;
pub const MAX_EJECTORS_PER_SYSTEM: usize = 4;
pub const MAX_PARTICLES_PER_EJECTOR: usize = 4;
pub const MAX_PARTICLE_EJECTORS: usize = MAX_PARTICLE_SYSTEMS * MAX_EJECTORS_PER_SYSTEM;
pub const MAX_PARTICLES: usize = MAX_PARTICLE_EJECTORS * 5;
const MAX_PS_SHADER_FRAMES: usize = 32;
const MAX_PS_MODELS: usize = 8;
const PARTICLES_INFINITE: i32 = -1;
const PARTICLES_SAME_AS_INITIAL: f32 = -2.0;

// ---------------------------------------------------------------------------
// small PRNG standing in for libc rand()

#[derive(Clone, Debug)]
pub(crate) struct Rng(u32);

impl Rng {
    pub fn new(seed: u32) -> Self {
        Rng(seed.max(1))
    }
    /// libc-style `rand()` (non-negative).
    pub fn rand(&mut self) -> u32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        x >> 1
    }
    /// `random()`: `(rand() & 0x7fff) / (float)0x7fff`.
    pub fn random(&mut self) -> f32 {
        (self.rand() & 0x7fff) as f32 / 0x7fff as f32
    }
    /// `crandom()`: `2.0 * (random() - 0.5)`.
    pub fn crandom(&mut self) -> f32 {
        (2.0 * (self.random() as f64 - 0.5)) as f32
    }
}

// ---------------------------------------------------------------------------
// tokenizer: COM_ParseExt / SkipBracedSection

pub(crate) struct Lexer<'a> {
    d: &'a [u8],
    p: usize,
    eof: bool,
}

impl<'a> Lexer<'a> {
    pub fn new(text: &'a str) -> Self {
        Lexer { d: text.as_bytes(), p: 0, eof: false }
    }
    fn at(&self, i: usize) -> u8 {
        self.d.get(i).copied().unwrap_or(0)
    }
    /// `COM_Parse` (line breaks allowed).
    pub fn parse(&mut self) -> String {
        self.parse_ext(true)
    }
    /// `COM_ParseExt`: empty string at end of data, or (when
    /// `allow_line_breaks` is false) when the next token is on another line.
    pub fn parse_ext(&mut self, allow_line_breaks: bool) -> String {
        if self.eof {
            return String::new();
        }
        let mut has_nl = false;
        loop {
            loop {
                let c = self.at(self.p);
                if c == 0 {
                    self.eof = true;
                    return String::new();
                }
                if c > b' ' {
                    break;
                }
                if c == b'\n' {
                    has_nl = true;
                }
                self.p += 1;
            }
            if has_nl && !allow_line_breaks {
                return String::new();
            }
            let c = self.at(self.p);
            if c == b'/' && self.at(self.p + 1) == b'/' {
                self.p += 2;
                while self.at(self.p) != 0 && self.at(self.p) != b'\n' {
                    self.p += 1;
                }
            } else if c == b'/' && self.at(self.p + 1) == b'*' {
                self.p += 2;
                while self.at(self.p) != 0 && !(self.at(self.p) == b'*' && self.at(self.p + 1) == b'/') {
                    self.p += 1;
                }
                if self.at(self.p) != 0 {
                    self.p += 2;
                }
            } else {
                break;
            }
        }
        let mut tok = Vec::new();
        if self.at(self.p) == b'"' {
            self.p += 1;
            loop {
                let c = self.at(self.p);
                if c == 0 {
                    self.eof = true;
                    break;
                }
                self.p += 1;
                if c == b'"' {
                    break;
                }
                tok.push(c);
            }
            return String::from_utf8_lossy(&tok).into_owned();
        }
        loop {
            tok.push(self.at(self.p));
            self.p += 1;
            if self.at(self.p) <= 32 {
                break;
            }
        }
        String::from_utf8_lossy(&tok).into_owned()
    }
    /// `SkipBracedSection(program, depth)`.
    pub fn skip_braced_section(&mut self, mut depth: i32) -> bool {
        loop {
            let t = self.parse_ext(true);
            if t == "{" {
                depth += 1;
            } else if t == "}" {
                depth -= 1;
            }
            if depth == 0 || self.eof {
                break;
            }
        }
        depth == 0
    }
}

/// C `atof`: longest numeric prefix, 0 if none.
pub(crate) fn c_atof(s: &str) -> f32 {
    let b = s.trim_start().as_bytes();
    let mut i = 0;
    if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
        i += 1;
    }
    let ds = i;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    let mut nd = i - ds;
    if i < b.len() && b[i] == b'.' {
        i += 1;
        let fs = i;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        nd += i - fs;
    }
    if nd == 0 {
        return 0.0;
    }
    if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
        let mut j = i + 1;
        if j < b.len() && (b[j] == b'+' || b[j] == b'-') {
            j += 1;
        }
        if j < b.len() && b[j].is_ascii_digit() {
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            i = j;
        }
    }
    let t = std::str::from_utf8(&b[..i]).unwrap_or("0");
    let t = if t.ends_with('.') && t.len() > 1 { &t[..t.len() - 1] } else { t };
    let t = if let Some(r) = t.strip_prefix('+') { r } else { t };
    let t = if t.starts_with('.') { format!("0{t}") } else if t.starts_with("-.") { format!("-0{}", &t[1..]) } else { t.to_string() };
    t.parse::<f64>().map(|v| v as f32).unwrap_or(0.0)
}

/// C `atoi`.
pub(crate) fn c_atoi(s: &str) -> i32 {
    let b = s.trim_start().as_bytes();
    let mut i = 0;
    let mut neg = false;
    if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
        neg = b[i] == b'-';
        i += 1;
    }
    let mut v: i64 = 0;
    while i < b.len() && b[i].is_ascii_digit() {
        v = (v * 10 + (b[i] - b'0') as i64).min(i32::MAX as i64 + 1);
        i += 1;
    }
    let v = if neg { -v } else { v };
    v.clamp(i32::MIN as i64, i32::MAX as i64) as i32
}

/// `atof_neg` (bg_misc.c): negative values become 1 unless allowed.
pub(crate) fn atof_neg(s: &str, allow_negative: bool) -> f32 {
    let v = c_atof(s);
    if !allow_negative && v < 0.0 {
        1.0
    } else {
        v
    }
}

/// `atoi_neg` (bg_misc.c).
pub(crate) fn atoi_neg(s: &str, allow_negative: bool) -> i32 {
    let v = c_atoi(s);
    if !allow_negative && v < 0 {
        1
    } else {
        v
    }
}

/// `CG_ParseValueAndVariance`: "v", "v~var" (absolute, or relative to v when
/// v != 0), "v~var%" (percent). Returns (value, variance-fraction).
pub(crate) fn parse_value_and_variance(token: &str, allow_negative: bool) -> (f32, f32) {
    // valueBuffer[16]
    let mut buf: String = token.chars().take(15).collect();
    while buf.len() > 15 {
        buf.pop();
    }
    if let Some(i) = buf.find('~') {
        let (vs, rest) = buf.split_at(i);
        let rest = &rest[1..];
        let value = atof_neg(vs, allow_negative);
        let variance = if let Some(j) = rest.find('%') {
            atof_neg(&rest[..j], false) / 100.0
        } else if value != 0.0 {
            atof_neg(rest, false) / value
        } else {
            atof_neg(rest, false)
        };
        (value, variance)
    } else {
        (atof_neg(&buf, allow_negative), 0.0)
    }
}

/// Parsed `{ r g b }` colour channel: `(int)(255 * atof_neg)` stored in a byte.
pub(crate) fn color_byte(tok: &str) -> u8 {
    ((0xFF as f32 * atof_neg(tok, false)) as i32) as u8
}

// ---------------------------------------------------------------------------
// templates (compile-time structures)

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MoveType {
    #[default]
    Static,
    StaticTransform,
    Tag,
    Cent,
    Normal,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DirType {
    #[default]
    Linear,
    Point,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MoveValues {
    pub dir_type: DirType,
    pub dir: Vec3,
    pub dir_rand_angle: f32,
    pub point: Vec3,
    pub point_rand_angle: f32,
    pub mag: f32,
    pub mag_rand_frac: f32,
    pub parent_vel_frac: f32,
    pub parent_vel_frac_rand_frac: f32,
}

/// `pLerpValues_t`: `delay initial final` with random fractions.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LerpValues {
    pub delay: i32,
    pub delay_rand_frac: f32,
    pub initial: f32,
    pub initial_rand_frac: f32,
    pub final_: f32,
    pub final_rand_frac: f32,
    pub rand_frac: f32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ModelAnimation {
    pub first_frame: i32,
    pub num_frames: i32,
    pub loop_frames: i32,
    pub reversed: bool,
    /// ms per frame; -1 = "sync" (lifetime / numFrames)
    pub frame_lerp: i32,
}

/// `baseParticle_t`.
#[derive(Clone, Debug, PartialEq)]
pub struct BaseParticle {
    pub displacement: Vec3,
    pub rand_displacement: Vec3,
    pub normal_displacement: f32,
    pub vel_move_type: MoveType,
    pub vel: MoveValues,
    pub acc_move_type: MoveType,
    pub acc: MoveValues,
    pub life_time: i32,
    pub life_time_rand_frac: f32,
    pub bounce_frac: f32,
    pub bounce_frac_rand_frac: f32,
    pub bounce_cull: bool,
    pub bounce_mark_name: String,
    pub bounce_mark_radius: f32,
    pub bounce_mark_radius_rand_frac: f32,
    pub bounce_mark_count: f32,
    pub bounce_mark_count_rand_frac: f32,
    pub bounce_sound_name: String,
    pub bounce_sound_count: f32,
    pub bounce_sound_count_rand_frac: f32,
    pub radius: LerpValues,
    pub physics_radius: i32,
    pub alpha: LerpValues,
    pub rotation: LerpValues,
    pub dynamic_light: bool,
    pub dlight_radius: LerpValues,
    pub dlight_color: [u8; 3],
    pub color_delay: i32,
    pub color_delay_rand_frac: f32,
    pub initial_color: [u8; 3],
    pub final_color: [u8; 3],
    pub child_system: String,
    pub child_system_handle: Option<usize>,
    pub on_death_system: String,
    pub on_death_system_handle: Option<usize>,
    pub child_trail_system: String,
    pub shaders: Vec<String>,
    /// frames per second; 0 = "sync" (frames spread over the lifetime)
    pub framerate: f32,
    pub models: Vec<String>,
    pub model_animation: ModelAnimation,
    pub overdraw_protection: bool,
    pub real_light: bool,
    pub cull_on_start_solid: bool,
    pub scale_with_charge: f32,
}

impl Default for BaseParticle {
    /// `CG_InitialiseBaseParticle`: zero, colours white.
    fn default() -> Self {
        BaseParticle {
            displacement: [0.0; 3],
            rand_displacement: [0.0; 3],
            normal_displacement: 0.0,
            vel_move_type: MoveType::Static,
            vel: MoveValues::default(),
            acc_move_type: MoveType::Static,
            acc: MoveValues::default(),
            life_time: 0,
            life_time_rand_frac: 0.0,
            bounce_frac: 0.0,
            bounce_frac_rand_frac: 0.0,
            bounce_cull: false,
            bounce_mark_name: String::new(),
            bounce_mark_radius: 0.0,
            bounce_mark_radius_rand_frac: 0.0,
            bounce_mark_count: 0.0,
            bounce_mark_count_rand_frac: 0.0,
            bounce_sound_name: String::new(),
            bounce_sound_count: 0.0,
            bounce_sound_count_rand_frac: 0.0,
            radius: LerpValues::default(),
            physics_radius: 0,
            alpha: LerpValues::default(),
            rotation: LerpValues::default(),
            dynamic_light: false,
            dlight_radius: LerpValues::default(),
            dlight_color: [0; 3],
            color_delay: 0,
            color_delay_rand_frac: 0.0,
            initial_color: [0xFF; 3],
            final_color: [0xFF; 3],
            child_system: String::new(),
            child_system_handle: None,
            on_death_system: String::new(),
            on_death_system_handle: None,
            child_trail_system: String::new(),
            shaders: Vec::new(),
            framerate: 0.0,
            models: Vec::new(),
            model_animation: ModelAnimation::default(),
            overdraw_protection: false,
            real_light: false,
            cull_on_start_solid: false,
            scale_with_charge: 0.0,
        }
    }
}

/// `baseParticleEjector_t`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BaseEjector {
    pub particles: Vec<BaseParticle>,
    /// `eject`: delay, initial/final period (final may be SAME_AS_INITIAL), randFrac
    pub eject: LerpValues,
    /// -1 = infinite
    pub total_particles: i32,
    pub total_particles_rand_frac: f32,
}

/// `baseParticleSystem_t`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BaseSystem {
    pub name: String,
    pub ejectors: Vec<BaseEjector>,
    pub third_person_only: bool,
}

// ---------------------------------------------------------------------------
// parser

type PResult<T> = Result<T, String>;

fn need(lx: &mut Lexer) -> PResult<String> {
    let t = lx.parse();
    if t.is_empty() {
        Err("unexpected end of file".into())
    } else {
        Ok(t)
    }
}

fn parse_type(lx: &mut Lexer, t: &mut MoveType) -> PResult<()> {
    let tok = need(lx)?.to_lowercase();
    match tok.as_str() {
        "static" => *t = MoveType::Static,
        "static_transform" => *t = MoveType::StaticTransform,
        "tag" => *t = MoveType::Tag,
        "cent" => *t = MoveType::Cent,
        "normal" => *t = MoveType::Normal,
        _ => {} // the C silently keeps the previous value
    }
    Ok(())
}

fn parse_dir(lx: &mut Lexer, mv: &mut MoveValues) -> PResult<()> {
    let tok = need(lx)?.to_lowercase();
    match tok.as_str() {
        "linear" => mv.dir_type = DirType::Linear,
        "point" => mv.dir_type = DirType::Point,
        _ => {}
    }
    Ok(())
}

/// `CG_ParseFinal`.
fn parse_final(lx: &mut Lexer, lv: &mut LerpValues) -> PResult<()> {
    let tok = need(lx)?;
    if tok == "-" {
        lv.final_ = PARTICLES_SAME_AS_INITIAL;
        lv.final_rand_frac = 0.0;
    } else {
        let (v, r) = parse_value_and_variance(&tok, false);
        lv.final_ = v;
        lv.final_rand_frac = r;
    }
    Ok(())
}

/// `delay initial final` triplet (radius / alpha / rotation / dynamicLight).
fn parse_lerp(lx: &mut Lexer, lv: &mut LerpValues, initial_neg: bool) -> PResult<()> {
    let (n, r) = parse_value_and_variance(&need(lx)?, false);
    lv.delay = n as i32;
    lv.delay_rand_frac = r;
    let (v, r) = parse_value_and_variance(&need(lx)?, initial_neg);
    lv.initial = v;
    lv.initial_rand_frac = r;
    parse_final(lx, lv)
}

/// `CG_ParseColor`: three channels then `}`.
fn parse_color(lx: &mut Lexer) -> PResult<[u8; 3]> {
    let mut c = [0u8; 3];
    for ch in c.iter_mut() {
        *ch = color_byte(&need(lx)?);
    }
    if lx.parse() != "}" {
        return Err("missing '}'".into());
    }
    Ok(c)
}

/// `CG_CopyLine`: the rest of the line, up to `max` tokens.
fn copy_line(lx: &mut Lexer, out: &mut Vec<String>, max: usize) {
    while out.len() < max {
        let t = lx.parse_ext(false);
        if t.is_empty() {
            break;
        }
        out.push(t);
    }
}

fn parse_vec_and_angle(lx: &mut Lexer) -> PResult<(Vec3, f32)> {
    let mut v = [0.0; 3];
    for x in v.iter_mut() {
        *x = atof_neg(&need(lx)?, true);
    }
    let (_, a) = parse_value_and_variance(&need(lx)?, false);
    Ok((v, a))
}

/// `CG_ParseParticle`.
fn parse_particle(lx: &mut Lexer, bp: &mut BaseParticle) -> PResult<()> {
    loop {
        let token = lx.parse();
        if token.is_empty() {
            return Err("unexpected end of file in particle".into());
        }
        match token.to_lowercase().as_str() {
            "bounce" => {
                let t = need(lx)?;
                if t.eq_ignore_ascii_case("cull") {
                    bp.bounce_cull = true;
                    bp.bounce_frac = -1.0;
                    bp.bounce_frac_rand_frac = 0.0;
                } else {
                    (bp.bounce_frac, bp.bounce_frac_rand_frac) = parse_value_and_variance(&t, false);
                }
            }
            "bouncemark" => {
                (bp.bounce_mark_count, bp.bounce_mark_count_rand_frac) = parse_value_and_variance(&need(lx)?, false);
                (bp.bounce_mark_radius, bp.bounce_mark_radius_rand_frac) = parse_value_and_variance(&need(lx)?, false);
                let t = lx.parse_ext(false);
                if t.is_empty() {
                    return Err("bounceMark without a shader".into());
                }
                bp.bounce_mark_name = t;
            }
            "bouncesound" => {
                (bp.bounce_sound_count, bp.bounce_sound_count_rand_frac) = parse_value_and_variance(&need(lx)?, false);
                bp.bounce_sound_name = need(lx)?;
            }
            "shader" => {
                if !bp.models.is_empty() {
                    return Err("'shader' not allowed in conjunction with 'model'".into());
                }
                let t = need(lx)?;
                bp.framerate = if t.eq_ignore_ascii_case("sync") { 0.0 } else { atof_neg(&t, false) };
                copy_line(lx, &mut bp.shaders, MAX_PS_SHADER_FRAMES);
            }
            "model" => {
                if !bp.shaders.is_empty() {
                    return Err("'model' not allowed in conjunction with 'shader'".into());
                }
                copy_line(lx, &mut bp.models, MAX_PS_MODELS);
            }
            "modelanimation" => {
                let a = &mut bp.model_animation;
                a.first_frame = atoi_neg(&need(lx)?, false);
                a.num_frames = c_atoi(&need(lx)?);
                a.reversed = false;
                if a.num_frames < 0 {
                    a.num_frames = -a.num_frames;
                    a.reversed = true;
                }
                a.loop_frames = c_atoi(&need(lx)?);
                let t = need(lx)?;
                if t.eq_ignore_ascii_case("sync") {
                    a.frame_lerp = -1;
                } else {
                    let mut fps = atof_neg(&t, false);
                    if fps == 0.0 {
                        fps = 1.0;
                    }
                    a.frame_lerp = (1000.0 / fps) as i32;
                }
            }
            "velocitytype" => parse_type(lx, &mut bp.vel_move_type)?,
            "velocitydir" => parse_dir(lx, &mut bp.vel)?,
            "velocitymagnitude" => (bp.vel.mag, bp.vel.mag_rand_frac) = parse_value_and_variance(&need(lx)?, false),
            "parentvelocityfraction" => {
                (bp.vel.parent_vel_frac, bp.vel.parent_vel_frac_rand_frac) = parse_value_and_variance(&need(lx)?, false)
            }
            "velocity" => (bp.vel.dir, bp.vel.dir_rand_angle) = parse_vec_and_angle(lx)?,
            "velocitypoint" => (bp.vel.point, bp.vel.point_rand_angle) = parse_vec_and_angle(lx)?,
            "accelerationtype" => parse_type(lx, &mut bp.acc_move_type)?,
            "accelerationdir" => parse_dir(lx, &mut bp.acc)?,
            "accelerationmagnitude" => (bp.acc.mag, bp.acc.mag_rand_frac) = parse_value_and_variance(&need(lx)?, false),
            "acceleration" => (bp.acc.dir, bp.acc.dir_rand_angle) = parse_vec_and_angle(lx)?,
            "accelerationpoint" => (bp.acc.point, bp.acc.point_rand_angle) = parse_vec_and_angle(lx)?,
            "displacement" => {
                for i in 0..3 {
                    (bp.displacement[i], bp.rand_displacement[i]) = parse_value_and_variance(&need(lx)?, true);
                }
                // optional 4th token on the same line: extra displacement in all directions
                let t = lx.parse_ext(false);
                let (_, rand_frac) = parse_value_and_variance(&t, true);
                for i in 0..3 {
                    if bp.displacement[i] != 0.0 {
                        bp.rand_displacement[i] *= bp.displacement[i];
                    }
                    bp.rand_displacement[i] += rand_frac;
                }
            }
            "normaldisplacement" => bp.normal_displacement = atof_neg(&need(lx)?, true),
            "overdrawprotection" => bp.overdraw_protection = true,
            "reallight" => bp.real_light = true,
            "dynamiclight" => {
                bp.dynamic_light = true;
                parse_lerp(lx, &mut bp.dlight_radius, false)?;
                // the token after the triplet is consumed even when it is not '{'
                if need(lx)? == "{" {
                    bp.dlight_color = parse_color(lx)?;
                }
            }
            "cullonstartsolid" => bp.cull_on_start_solid = true,
            "radius" => parse_lerp(lx, &mut bp.radius, false)?,
            "physicsradius" => bp.physics_radius = c_atoi(&need(lx)?),
            "alpha" => parse_lerp(lx, &mut bp.alpha, false)?,
            "color" => {
                let (n, r) = parse_value_and_variance(&need(lx)?, false);
                bp.color_delay = n as i32;
                bp.color_delay_rand_frac = r;
                if need(lx)? != "{" {
                    return Err("missing '{'".into());
                }
                bp.initial_color = parse_color(lx)?;
                let t = need(lx)?;
                if t == "-" {
                    bp.final_color = bp.initial_color;
                } else if t == "{" {
                    bp.final_color = parse_color(lx)?;
                } else {
                    return Err("missing '{'".into());
                }
            }
            "rotation" => parse_lerp(lx, &mut bp.rotation, true)?,
            "lifetime" => {
                let (n, r) = parse_value_and_variance(&need(lx)?, false);
                bp.life_time = n as i32;
                bp.life_time_rand_frac = r;
            }
            "childsystem" => bp.child_system = need(lx)?,
            "ondeathsystem" => bp.on_death_system = need(lx)?,
            "childtrailsystem" => bp.child_trail_system = need(lx)?,
            "scalewithcharge" => bp.scale_with_charge = c_atof(&need(lx)?),
            "}" => return Ok(()),
            _ => return Err(format!("unknown token '{token}' in particle")),
        }
    }
}

/// `CG_ParseParticleEjector`.
fn parse_ejector(lx: &mut Lexer) -> PResult<BaseEjector> {
    let mut bpe = BaseEjector::default();
    loop {
        let token = lx.parse();
        if token.is_empty() {
            return Err("unexpected end of file in particle ejector".into());
        }
        match token.to_lowercase().as_str() {
            "{" => {
                let mut bp = BaseParticle::default();
                parse_particle(lx, &mut bp).map_err(|e| format!("{e}; failed to parse particle"))?;
                if bpe.particles.len() == MAX_PARTICLES_PER_EJECTOR {
                    return Err(format!("ejector has > {MAX_PARTICLES_PER_EJECTOR} particles"));
                }
                bpe.particles.push(bp);
            }
            "delay" => {
                let (n, r) = parse_value_and_variance(&need(lx)?, false);
                bpe.eject.delay = n as i32;
                bpe.eject.delay_rand_frac = r;
            }
            "period" => {
                bpe.eject.initial = atoi_neg(&need(lx)?, false) as f32;
                let t = need(lx)?;
                bpe.eject.final_ = if t == "-" { PARTICLES_SAME_AS_INITIAL } else { atoi_neg(&t, false) as f32 };
                bpe.eject.rand_frac = parse_value_and_variance(&need(lx)?, false).1;
            }
            "count" => {
                let t = need(lx)?;
                if t.eq_ignore_ascii_case("infinite") {
                    bpe.total_particles = PARTICLES_INFINITE;
                    bpe.total_particles_rand_frac = 0.0;
                } else {
                    let (n, r) = parse_value_and_variance(&t, false);
                    bpe.total_particles = n as i32;
                    bpe.total_particles_rand_frac = r;
                }
            }
            "particle" => {}
            "}" => return Ok(bpe),
            _ => return Err(format!("unknown token '{token}' in particle ejector")),
        }
    }
}

/// `CG_ParseParticleSystem`.
fn parse_system(lx: &mut Lexer, name: &str) -> PResult<BaseSystem> {
    let mut bps = BaseSystem { name: name.to_string(), ..Default::default() };
    loop {
        let token = lx.parse();
        if token.is_empty() {
            return Err("unexpected end of file in particle system".into());
        }
        match token.to_lowercase().as_str() {
            "{" => {
                let bpe = parse_ejector(lx).map_err(|e| format!("{e}; failed to parse particle ejector"))?;
                if bpe.total_particles == PARTICLES_INFINITE && (bpe.eject.initial == 0.0 || bpe.eject.final_ == 0.0) {
                    return Err("ejector with 'count infinite' potentially has zero period".into());
                }
                if bps.ejectors.len() == MAX_EJECTORS_PER_SYSTEM {
                    return Err(format!("particle system has > {MAX_EJECTORS_PER_SYSTEM} ejectors"));
                }
                bps.ejectors.push(bpe);
            }
            "thirdpersononly" => bps.third_person_only = true,
            "ejector" => {}
            "}" => return Ok(bps),
            _ => return Err(format!("unknown token '{token}' in particle system {name}")),
        }
    }
}

/// Parsed particle templates (`baseParticleSystems[]`).
#[derive(Clone, Debug, Default)]
pub struct ParticleLib {
    pub systems: Vec<BaseSystem>,
    by_name: HashMap<String, usize>,
    /// Errors/warnings the C would print while loading (empty for the stock data).
    pub errors: Vec<String>,
}

impl ParticleLib {
    /// `CG_LoadParticleSystems`: every `*.particle` among `files` (path,
    /// contents), in path order; other files are ignored.
    pub fn load(files: &[(String, String)]) -> Self {
        let mut lib = ParticleLib::default();
        let mut list: Vec<&(String, String)> = files.iter().filter(|(p, _)| p.to_lowercase().ends_with(".particle")).collect();
        list.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()));
        for (path, text) in list {
            lib.parse_file(path, text);
        }
        // connect child / onDeath systems to their handles
        let names = lib.by_name.clone();
        let mut warn = Vec::new();
        for s in lib.systems.iter_mut() {
            for e in s.ejectors.iter_mut() {
                for p in e.particles.iter_mut() {
                    if !p.child_system.is_empty() {
                        p.child_system_handle = names.get(&p.child_system.to_lowercase()).copied();
                        if p.child_system_handle.is_none() {
                            warn.push(format!("WARNING: failed to find child {}", p.child_system));
                            p.child_system.clear();
                        }
                    }
                    if !p.on_death_system.is_empty() {
                        p.on_death_system_handle = names.get(&p.on_death_system.to_lowercase()).copied();
                        if p.on_death_system_handle.is_none() {
                            warn.push(format!("WARNING: failed to find onDeath system {}", p.on_death_system));
                            p.on_death_system.clear();
                        }
                    }
                }
            }
        }
        lib.errors.extend(warn);
        lib
    }

    /// `CG_ParseParticleFile`. Returns false where the C gives up on the file.
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
                    self.errors.push(format!("{path}: unnamed particle system"));
                    return false;
                };
                match parse_system(&mut lx, &n) {
                    Ok(s) => {
                        self.by_name.insert(n.to_lowercase(), self.systems.len());
                        self.systems.push(s);
                    }
                    Err(e) => {
                        self.errors.push(format!("{path}: {e}; failed to parse particle system {n}"));
                        return false;
                    }
                }
            } else if name.is_none() {
                if self.by_name.contains_key(&token.to_lowercase()) {
                    self.errors.push(format!("{path}: a particle system is already named {token}"));
                    lx.skip_braced_section(0);
                    continue;
                }
                name = Some(token);
            } else {
                self.errors.push(format!("{path}: particle system already named"));
                return false;
            }
        }
        true
    }

    /// `CG_RegisterParticleSystem` (case-insensitive name -> handle).
    pub fn handle(&self, name: &str) -> Option<usize> {
        self.by_name.get(&name.to_lowercase()).copied()
    }

    #[allow(dead_code)] // API used by tests / callers to come
    pub fn get(&self, handle: usize) -> Option<&BaseSystem> {
        self.systems.get(handle)
    }

    /// Every shader any particle can use (for pre-loading textures).
    pub fn shader_names(&self) -> Vec<String> {
        let mut v: Vec<String> = Vec::new();
        for s in &self.systems {
            for e in &s.ejectors {
                for p in &e.particles {
                    for sh in &p.shaders {
                        if !v.contains(sh) {
                            v.push(sh.clone());
                        }
                    }
                }
            }
        }
        v
    }

    /// Every model any particle can use.
    #[allow(dead_code)] // API used by tests / callers to come
    pub fn model_names(&self) -> Vec<String> {
        let mut v: Vec<String> = Vec::new();
        for s in &self.systems {
            for e in &s.ejectors {
                for p in &e.particles {
                    for m in &p.models {
                        if !v.contains(m) {
                            v.push(m.clone());
                        }
                    }
                }
            }
        }
        v
    }
}

// ---------------------------------------------------------------------------
// attachments (cg_attachment.c)

/// Reference to a live particle (slot + generation, so stale refs fail).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PRef {
    pub slot: usize,
    pub serial: u32,
}

/// What a system/trail end is attached to.
#[derive(Clone, Debug, PartialEq)]
pub enum Attach {
    /// `CG_SetAttachmentPoint` + `CG_AttachToPoint`
    Point(Vec3),
    /// `CG_SetAttachmentCent` + `CG_AttachToCent` (entity number)
    Entity(i32),
    /// `CG_SetAttachmentTag` + `CG_SetAttachmentCent` + `CG_AttachToTag`
    Tag { entity: i32, tag: String },
    /// `CG_SetAttachmentParticle` + `CG_AttachToParticle` (internal)
    Particle(PRef),
}

/// World-space state of an entity / tag, supplied by the game each update.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AttachPos {
    pub origin: Vec3,
    /// rows: forward, left, up (`AnglesToAxis` / tag axis)
    pub axis: [Vec3; 3],
    /// entity velocity (`currentState.pos.trDelta`); ignored for tags
    pub velocity: Vec3,
}

impl AttachPos {
    pub fn at(origin: Vec3) -> Self {
        AttachPos { origin, axis: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]], velocity: [0.0; 3] }
    }
}

pub type Resolver<'a> = dyn Fn(&Attach) -> Option<AttachPos> + 'a;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AKind {
    Static,
    Tag,
    Cent,
    Particle,
}

/// `attachment_t`.
#[derive(Clone, Debug, Default)]
pub(crate) struct Attachment {
    kind: Option<AKind>,
    point: Option<Vec3>,
    cent: Option<i32>,
    tag: Option<(i32, String)>,
    particle: Option<PRef>,
    particle_valid: bool,
    offset: Option<Vec3>,
    pub last_valid: Vec3,
    cent_pos: Option<AttachPos>,
    pub cent_alive: bool,
    tag_pos: Option<AttachPos>,
}

/// Live particle lookup: (origin, velocity).
pub(crate) type PLookup<'a> = dyn Fn(PRef) -> Option<(Vec3, Vec3)> + 'a;

impl Attachment {
    pub fn new(a: Attach) -> Self {
        let mut at = Attachment { cent_alive: true, ..Default::default() };
        at.set(a);
        at
    }
    /// `CG_SetAttachment*` + `CG_AttachTo*`.
    pub fn set(&mut self, a: Attach) {
        match a {
            Attach::Point(p) => {
                self.point = Some(p);
                self.kind = Some(AKind::Static);
            }
            Attach::Entity(n) => {
                if self.cent != Some(n) {
                    self.cent_pos = None;
                }
                self.cent = Some(n);
                self.cent_alive = true;
                self.kind = Some(AKind::Cent);
            }
            Attach::Tag { entity, tag } => {
                if self.cent != Some(entity) {
                    self.cent_pos = None;
                }
                if self.tag.as_ref() != Some(&(entity, tag.clone())) {
                    self.tag_pos = None;
                }
                self.cent = Some(entity);
                self.cent_alive = true;
                self.tag = Some((entity, tag));
                self.kind = Some(AKind::Tag);
            }
            Attach::Particle(r) => {
                self.particle = Some(r);
                self.particle_valid = true;
                self.kind = Some(AKind::Particle);
            }
        }
    }
    /// `CG_SetAttachmentCent` without changing the attached kind.
    pub fn set_cent(&mut self, n: i32) {
        if self.cent != Some(n) {
            self.cent_pos = None;
        }
        self.cent = Some(n);
        self.cent_alive = true;
    }
    pub fn clear_cent(&mut self) {
        self.cent = None;
    }
    #[allow(dead_code)] // API used by tests / callers to come
    pub fn set_offset(&mut self, o: Vec3) {
        self.offset = Some(o);
    }
    /// `CG_Attached`.
    pub fn attached(&self) -> bool {
        self.kind.is_some()
    }
    /// `CG_AttachmentCentNum`.
    pub fn cent_num(&self) -> Option<i32> {
        self.cent
    }
    #[allow(dead_code)] // API used by tests / callers to come
    pub fn is_point(&self) -> bool {
        self.kind == Some(AKind::Static)
    }
    /// Pull this frame's entity/tag transforms from the game. Results are
    /// sticky (the C keeps reading the stale centity / refEntity).
    pub fn refresh(&mut self, resolve: &Resolver) {
        if let Some(n) = self.cent {
            match resolve(&Attach::Entity(n)) {
                Some(p) => {
                    self.cent_pos = Some(p);
                    self.cent_alive = true;
                }
                None => self.cent_alive = false,
            }
        }
        if let Some((e, t)) = &self.tag {
            if let Some(p) = resolve(&Attach::Tag { entity: *e, tag: t.clone() }) {
                self.tag_pos = Some(p);
            }
        }
    }
    /// `CG_AttachmentPoint`. On failure `last_valid` still holds the last good point.
    pub fn point(&mut self, lk: &PLookup) -> Option<Vec3> {
        let mut v = match self.kind? {
            AKind::Static => self.point?,
            AKind::Tag => self.tag_pos?.origin,
            AKind::Cent => self.cent_pos?.origin,
            AKind::Particle => {
                if !self.particle_valid {
                    return None;
                }
                match self.particle.and_then(|r| lk(r)) {
                    Some((o, _)) => o,
                    None => {
                        self.particle_valid = false;
                        return None;
                    }
                }
            }
        };
        if let Some(o) = self.offset {
            v = qm::add(&v, &o);
        }
        self.last_valid = v;
        Some(v)
    }
    /// `CG_AttachmentDir` (normalised).
    pub fn dir(&mut self, lk: &PLookup) -> Option<Vec3> {
        let mut v = match self.kind? {
            AKind::Static => return None,
            AKind::Tag => self.tag_pos?.axis[0],
            AKind::Cent => self.cent_pos?.axis[0],
            AKind::Particle => {
                if !self.particle_valid {
                    return None;
                }
                match self.particle.and_then(|r| lk(r)) {
                    Some((_, vel)) => vel,
                    None => {
                        self.particle_valid = false;
                        return None;
                    }
                }
            }
        };
        qm::normalize(&mut v);
        Some(v)
    }
    /// `CG_AttachmentAxis`.
    pub fn axis(&self) -> Option<[Vec3; 3]> {
        match self.kind? {
            AKind::Static | AKind::Particle => None,
            AKind::Tag => Some(self.tag_pos?.axis),
            AKind::Cent => Some(self.cent_pos?.axis),
        }
    }
    /// `CG_AttachmentVelocity`.
    pub fn velocity(&self, lk: &PLookup) -> Option<Vec3> {
        if self.particle_valid {
            if let Some((_, v)) = self.particle.and_then(|r| lk(r)) {
                return Some(v);
            }
        }
        if self.cent.is_some() {
            return Some(self.cent_pos.map(|p| p.velocity).unwrap_or([0.0; 3]));
        }
        None
    }
}

// ---------------------------------------------------------------------------
// math helpers

fn lerp_values(a: f32, b: f32, f: f32) -> f32 {
    if b == PARTICLES_SAME_AS_INITIAL {
        a
    } else {
        a + f * (b - a)
    }
}

fn randomise(rng: &mut Rng, value: f32, variance: f32) -> f32 {
    if value != 0.0 {
        value * (1.0 + rng.random() * variance)
    } else {
        rng.random() * variance
    }
}

/// `CG_SpreadVector`.
fn spread_vector(rng: &mut Rng, v: &mut Vec3, spread: f32) {
    let random_spread = rng.crandom() * spread;
    let random_rotation = rng.random() * 360.0;
    let p = qm::perpendicular_vector(v);
    let r1 = qm::rotate_point_around_vector(&p, v, random_spread);
    let r2 = qm::rotate_point_around_vector(v, &r1, random_rotation);
    *v = r2;
}

/// `VectorMatrixMultiply`.
fn vec_mat(p: &Vec3, m: &[Vec3; 3]) -> Vec3 {
    [
        m[0][0] * p[0] + m[1][0] * p[1] + m[2][0] * p[2],
        m[0][1] * p[0] + m[1][1] * p[1] + m[2][1] * p[2],
        m[0][2] * p[0] + m[1][2] * p[1] + m[2][2] * p[2],
    ]
}

/// `CG_CalculateTimeFrac`.
fn time_frac(now: i32, birth: i32, life: i32, delay: i32) -> f32 {
    let f = (now as f32 - (birth + delay) as f32) / (life - delay) as f32;
    if f.is_nan() {
        0.0
    } else {
        f.clamp(0.0, 1.0)
    }
}

// ---------------------------------------------------------------------------
// render output (shared with trails.rs)

/// One vertex of an effect quad (Quake units).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FxVert {
    pub pos: Vec3,
    pub uv: [f32; 2],
    pub rgba: [u8; 4],
}

/// Quads (4 vertices each, fan order 0-1-2-3) sharing one shader.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct QuadBatch {
    pub shader: String,
    pub verts: Vec<FxVert>,
}

impl QuadBatch {
    #[allow(dead_code)] // API used by tests / callers to come
    pub fn quads(&self) -> usize {
        self.verts.len() / 4
    }
}

#[derive(Default)]
pub(crate) struct BatchBuilder {
    pub out: Vec<QuadBatch>,
    idx: HashMap<String, usize>,
}

impl BatchBuilder {
    pub fn push(&mut self, shader: &str, q: [FxVert; 4]) {
        if q.iter().any(|v| v.pos.iter().any(|x| !x.is_finite())) {
            return;
        }
        let i = match self.idx.get(shader) {
            Some(&i) => i,
            None => {
                self.out.push(QuadBatch { shader: shader.to_string(), verts: Vec::new() });
                self.idx.insert(shader.to_string(), self.out.len() - 1);
                self.out.len() - 1
            }
        };
        self.out[i].verts.extend_from_slice(&q);
    }
}

/// A model-based particle (gibs, shell casings).
#[derive(Clone, Debug, PartialEq)]
pub struct FxModel {
    pub model: String,
    pub origin: Vec3,
    /// rows forward/left/up, already scaled by the particle radius
    pub axis: [Vec3; 3],
    pub frame: i32,
}

/// `trap_R_AddLightToScene` from a `dynamicLight` particle.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FxLight {
    pub origin: Vec3,
    pub radius: f32,
    pub color: [f32; 3],
}

// ---------------------------------------------------------------------------
// run-time structures

#[derive(Clone, Debug)]
struct Ejector {
    base: usize,
    period_initial: f32,
    period_final: f32,
    period_rand_frac: f32,
    count: i32,
    total: i32,
    next_ejection: i32,
    valid: bool,
    live: usize,
}

#[derive(Clone, Debug)]
struct System {
    id: u32,
    handle: usize,
    attachment: Attachment,
    valid: bool,
    /// the owner called destroy (the C NULLs its pointer)
    released: bool,
    lazy_remove: bool,
    normal: Option<Vec3>,
    charge: i32,
    ejectors: Vec<Ejector>,
}

#[derive(Clone, Debug, Default)]
struct Particle {
    valid: bool,
    serial: u32,
    frame_invalidated: i32,
    sys: usize,
    ej: usize,
    bp: usize,
    birth: i32,
    life: i32,
    bounce_mark_count: i32,
    bounce_sound_count: i32,
    at_rest: bool,
    origin: Vec3,
    velocity: Vec3,
    last_eval: i32,
    radius: LerpValues,
    alpha: LerpValues,
    rotation: LerpValues,
    dlight: LerpValues,
    color_delay: i32,
    model: usize,
    last_axis: [Vec3; 3],
}

/// Live particle systems, ejectors and particles.
pub struct Particles {
    systems: Vec<Option<System>>,
    particles: Vec<Particle>,
    next_id: u32,
    now: i32,
    frame: i32,
    frametime: i32,
    rng: Rng,
    /// `cg_bounceParticles` (C default 0: particles are only culled in solids).
    pub bounce_particles: bool,
    /// `cg_depthSortParticles` (default 1).
    pub depth_sort: bool,
    /// Local client number while rendering first person: `thirdPersonOnly`
    /// systems attached to it are hidden (`RF_THIRD_PERSON`).
    pub first_person_client: Option<i32>,
}

impl Default for Particles {
    fn default() -> Self {
        Self::new()
    }
}

impl Particles {
    pub fn new() -> Self {
        Particles {
            systems: (0..MAX_PARTICLE_SYSTEMS).map(|_| None).collect(),
            particles: vec![Particle { frame_invalidated: -10, ..Default::default() }; MAX_PARTICLES],
            next_id: 1,
            now: 0,
            frame: 0,
            frametime: 0,
            rng: Rng::new(0x1234_5678),
            bounce_particles: false,
            depth_sort: true,
            first_person_client: None,
        }
    }

    /// Set `cg.time` for spawns made before the next `update`.
    pub fn set_time(&mut self, now_ms: i32) {
        self.now = now_ms;
    }

    #[allow(dead_code)] // API used by tests / callers to come
    pub fn now(&self) -> i32 {
        self.now
    }

    fn slot(&self, id: u32) -> Option<usize> {
        if id == 0 {
            return None;
        }
        self.systems.iter().position(|s| matches!(s, Some(s) if s.valid && !s.released && s.id == id))
    }

    /// `CG_SpawnNewParticleSystem` + attach. Returns 0 when the pool is full
    /// or the handle is bad.
    pub fn spawn(&mut self, lib: &ParticleLib, handle: usize, attach: Attach) -> u32 {
        match self.spawn_system(lib, handle) {
            Some(s) => {
                let sys = self.systems[s].as_mut().unwrap();
                sys.attachment.set(attach);
                sys.id
            }
            None => 0,
        }
    }

    fn spawn_system(&mut self, lib: &ParticleLib, handle: usize) -> Option<usize> {
        let bps = lib.systems.get(handle)?;
        let slot = self.systems.iter().position(|s| !matches!(s, Some(s) if s.valid))?;
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1).max(1);
        let mut ejectors = Vec::new();
        for (i, bpe) in bps.ejectors.iter().enumerate() {
            // CG_SpawnNewParticleEjector
            let next = self.now + randomise(&mut self.rng, bpe.eject.delay as f32, bpe.eject.delay_rand_frac) as i32;
            let total = randomise(&mut self.rng, bpe.total_particles as f32, bpe.total_particles_rand_frac).round_ties_even() as i32;
            let total = if bpe.total_particles == PARTICLES_INFINITE { PARTICLES_INFINITE } else { total };
            ejectors.push(Ejector {
                base: i,
                period_initial: bpe.eject.initial,
                period_final: bpe.eject.final_,
                period_rand_frac: bpe.eject.rand_frac,
                count: total,
                total,
                next_ejection: next,
                valid: true,
                live: 0,
            });
        }
        self.systems[slot] = Some(System {
            id,
            handle,
            attachment: Attachment { cent_alive: true, ..Default::default() },
            valid: true,
            released: false,
            lazy_remove: false,
            normal: None,
            charge: 0,
            ejectors,
        });
        Some(slot)
    }

    fn sys_mut(&mut self, id: u32) -> Option<&mut System> {
        let s = self.slot(id)?;
        self.systems[s].as_mut()
    }

    /// `CG_SetParticleSystemNormal`.
    pub fn set_normal(&mut self, id: u32, normal: Vec3) {
        if let Some(s) = self.sys_mut(id) {
            let mut n = normal;
            qm::normalize(&mut n);
            s.normal = Some(n);
        }
    }

    /// `ps->charge` (lcannon charge, used with `scaleWithCharge`).
    pub fn set_charge(&mut self, id: u32, charge: i32) {
        if let Some(s) = self.sys_mut(id) {
            s.charge = charge;
        }
    }

    /// `CG_SetAttachmentCent` on top of the current attachment.
    pub fn set_cent(&mut self, id: u32, entity: i32) {
        if let Some(s) = self.sys_mut(id) {
            s.attachment.set_cent(entity);
        }
    }

    /// Re-attach (e.g. `CG_SetAttachmentPoint` every frame for a moving point).
    #[allow(dead_code)] // API used by tests / callers to come
    pub fn set_attach(&mut self, id: u32, attach: Attach) {
        if let Some(s) = self.sys_mut(id) {
            s.attachment.set(attach);
        }
    }

    /// `CG_SetAttachmentOffset`.
    #[allow(dead_code)] // API used by tests / callers to come
    pub fn set_offset(&mut self, id: u32, offset: Vec3) {
        if let Some(s) = self.sys_mut(id) {
            s.attachment.set_offset(offset);
        }
    }

    /// `CG_DestroyParticleSystem`: stop ejecting; live particles finish.
    /// The id is dead for the caller afterwards.
    pub fn destroy(&mut self, id: u32) {
        if let Some(s) = self.sys_mut(id) {
            for e in s.ejectors.iter_mut().filter(|e| e.valid) {
                e.total = 0;
                e.count = 0;
            }
            s.released = true;
        }
    }

    /// `CG_IsParticleSystemValid`.
    pub fn is_valid(&self, id: u32) -> bool {
        self.slot(id).is_some()
    }

    /// `CG_IsParticleSystemInfinite`.
    pub fn is_infinite(&self, id: u32) -> bool {
        self.slot(id).and_then(|s| self.systems[s].as_ref()).is_some_and(|s| s.ejectors.iter().any(|e| e.valid && e.total == PARTICLES_INFINITE))
    }

    /// Counts (systems, ejectors, particles), like `cg_debugParticles 2`.
    #[allow(dead_code)] // API used by tests / callers to come
    pub fn counts(&self) -> (usize, usize, usize) {
        let mut ns = 0;
        let mut ne = 0;
        for s in self.systems.iter().flatten().filter(|s| s.valid) {
            ns += 1;
            ne += s.ejectors.iter().filter(|e| e.valid).count();
        }
        (ns, ne, self.particles.iter().filter(|p| p.valid).count())
    }

    /// Origin and velocity of a live particle.
    pub fn particle_state(&self, r: PRef) -> Option<(Vec3, Vec3)> {
        let p = self.particles.get(r.slot)?;
        if p.valid && p.serial == r.serial {
            Some((p.origin, p.velocity))
        } else {
            None
        }
    }

    fn lookup<'a>(parts: &'a [Particle]) -> impl Fn(PRef) -> Option<(Vec3, Vec3)> + 'a {
        move |r: PRef| {
            let p = parts.get(r.slot)?;
            if p.valid && p.serial == r.serial {
                Some((p.origin, p.velocity))
            } else {
                None
            }
        }
    }

    /// `CG_AddParticles` minus the rendering: garbage collect, eject, run physics.
    pub fn update(&mut self, lib: &ParticleLib, tlib: &TrailLib, now_ms: i32, cm: &ClipMap, resolve: &Resolver, trails: &mut Trails) {
        self.frametime = now_ms - self.now;
        self.now = now_ms;
        self.frame += 1;
        trails.set_time(now_ms);
        for s in self.systems.iter_mut().flatten().filter(|s| s.valid) {
            s.attachment.refresh(resolve);
        }
        self.garbage_collect();
        self.spawn_new_particles(lib, tlib, trails);
        for i in 0..MAX_PARTICLES {
            if !self.particles[i].valid {
                continue;
            }
            let p = &self.particles[i];
            if p.birth + p.life > self.now {
                self.evaluate_physics(lib, i, cm);
            } else {
                self.destroy_particle(lib, i, None);
            }
        }
        // model particles: direction of travel -> axis (CG_RenderParticle)
        for p in self.particles.iter_mut().filter(|p| p.valid && !p.at_rest) {
            let bp = &lib.systems[self.systems[p.sys].as_ref().unwrap().handle].ejectors[self.systems[p.sys].as_ref().unwrap().ejectors[p.ej].base].particles[p.bp];
            if bp.models.is_empty() {
                continue;
            }
            p.last_axis = velocity_axis(&p.velocity);
        }
    }

    /// `CG_GarbageCollectParticleSystems`.
    fn garbage_collect(&mut self) {
        for s in self.systems.iter_mut() {
            let Some(sys) = s else { continue };
            if !sys.valid {
                continue;
            }
            if !sys.ejectors.iter().any(|e| e.valid) {
                sys.valid = false;
            }
            if sys.attachment.cent_num().is_some() && !sys.attachment.cent_alive {
                sys.lazy_remove = true;
            }
            if !sys.valid {
                *s = None;
            }
        }
    }

    /// `CG_SpawnNewParticles`.
    fn spawn_new_particles(&mut self, lib: &ParticleLib, tlib: &TrailLib, trails: &mut Trails) {
        // systems spawned during this loop (children) are visited too, as in C
        let mut s = 0;
        while s < MAX_PARTICLE_SYSTEMS {
            let Some(sys) = self.systems[s].as_ref() else {
                s += 1;
                continue;
            };
            if !sys.valid || !sys.attachment.attached() {
                s += 1;
                continue;
            }
            let handle = sys.handle;
            for e in 0..self.systems[s].as_ref().unwrap().ejectors.len() {
                let sys = self.systems[s].as_ref().unwrap();
                let ej = &sys.ejectors[e];
                if !ej.valid {
                    continue;
                }
                let bpe = &lib.systems[handle].ejectors[ej.base];
                if !sys.lazy_remove {
                    loop {
                        let ej = &self.systems[s].as_ref().unwrap().ejectors[e];
                        if !(ej.next_ejection <= self.now && (ej.count > 0 || ej.total == PARTICLES_INFINITE)) {
                            break;
                        }
                        for j in 0..bpe.particles.len() {
                            self.spawn_particle(lib, tlib, trails, s, e, j);
                        }
                        let now = self.now;
                        let rng = &mut self.rng;
                        let ej = &mut self.systems[s].as_mut().unwrap().ejectors[e];
                        if ej.count > 0 {
                            ej.count -= 1;
                        }
                        let lerp_frac = 1.0 - (ej.count as f32 / ej.total as f32);
                        ej.next_ejection = now + randomise(rng, lerp_values(ej.period_initial, ej.period_final, lerp_frac), ej.period_rand_frac) as i32;
                    }
                }
                let sys = self.systems[s].as_mut().unwrap();
                let lazy = sys.lazy_remove;
                let ej = &mut sys.ejectors[e];
                if (ej.count == 0 || lazy) && ej.live == 0 {
                    ej.valid = false;
                }
            }
            s += 1;
        }
    }

    /// `CG_SpawnNewParticle`.
    fn spawn_particle(&mut self, lib: &ParticleLib, tlib: &TrailLib, trails: &mut Trails, s: usize, e: usize, j: usize) {
        let frame = self.frame;
        let Some(slot) = self.particles.iter().position(|p| !p.valid && frame > p.frame_invalidated + 1) else {
            return;
        };
        let sys = self.systems[s].as_ref().unwrap();
        let bp = &lib.systems[sys.handle].ejectors[sys.ejectors[e].base].particles[j];
        let charge = sys.charge;
        let rng = &mut self.rng;
        let mut p = Particle { sys: s, ej: e, bp: j, birth: self.now, ..Default::default() };
        p.life = randomise(rng, bp.life_time as f32, bp.life_time_rand_frac) as i32;
        let lv = |rng: &mut Rng, b: &LerpValues| LerpValues {
            delay: randomise(rng, b.delay as f32, b.delay_rand_frac) as i32,
            initial: randomise(rng, b.initial, b.initial_rand_frac),
            final_: randomise(rng, b.final_, b.final_rand_frac),
            ..Default::default()
        };
        p.radius = lv(rng, &bp.radius);
        p.radius.initial += bp.scale_with_charge * charge as f32;
        p.alpha = lv(rng, &bp.alpha);
        p.rotation = lv(rng, &bp.rotation);
        p.dlight = lv(rng, &bp.dlight_radius);
        p.color_delay = randomise(rng, bp.color_delay as f32, bp.color_delay_rand_frac) as i32;
        let _bounce_mark_radius = randomise(rng, bp.bounce_mark_radius, bp.bounce_mark_radius_rand_frac);
        p.bounce_mark_count = randomise(rng, bp.bounce_mark_count, bp.bounce_mark_count_rand_frac).round_ties_even() as i32;
        p.bounce_sound_count = randomise(rng, bp.bounce_sound_count, bp.bounce_sound_count_rand_frac).round_ties_even() as i32;
        if !bp.models.is_empty() {
            p.model = rng.rand() as usize % bp.models.len();
        }

        let normal = self.systems[s].as_ref().unwrap().normal;
        {
            let lk = Self::lookup(&self.particles);
            let att = &mut self.systems[s].as_mut().unwrap().attachment;
            if init_motion(att, &lk, &mut self.rng, bp, normal, &mut p).is_none() {
                return;
            }
        }
        p.last_eval = self.now;
        p.valid = true;
        p.serial = self.particles[slot].serial.wrapping_add(1);
        p.frame_invalidated = self.particles[slot].frame_invalidated;
        p.last_axis = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
        let pref = PRef { slot, serial: p.serial };
        self.particles[slot] = p;
        self.systems[s].as_mut().unwrap().ejectors[e].live += 1;

        let child = bp.child_system_handle.filter(|_| !bp.child_system.is_empty());
        let child_trail = (!bp.child_trail_system.is_empty()).then(|| tlib.handle(&bp.child_trail_system)).flatten();
        if let Some(h) = child {
            self.spawn(lib, h, Attach::Particle(pref));
        }
        if let Some(h) = child_trail {
            trails.spawn(tlib, h, Attach::Particle(pref), None);
        }
    }

    /// `CG_DestroyParticle`: spawn the onDeath system, free the slot.
    fn destroy_particle(&mut self, lib: &ParticleLib, i: usize, impact_normal: Option<Vec3>) {
        let p = &self.particles[i];
        if !p.valid {
            return;
        }
        let sys = self.systems[p.sys].as_ref().unwrap();
        let bp = &lib.systems[sys.handle].ejectors[sys.ejectors[p.ej].base].particles[p.bp];
        let origin = p.origin;
        if let Some(h) = bp.on_death_system_handle.filter(|_| !bp.on_death_system.is_empty()) {
            if let Some(slot) = self.spawn_system(lib, h) {
                let ns = self.systems[slot].as_mut().unwrap();
                if let Some(mut n) = impact_normal {
                    qm::normalize(&mut n);
                    ns.normal = Some(n);
                }
                ns.attachment.set(Attach::Point(origin));
            }
        }
        let (s, e) = (self.particles[i].sys, self.particles[i].ej);
        self.particles[i].valid = false;
        // other systems get a couple of frames to notice before the slot is reused
        self.particles[i].frame_invalidated = self.frame;
        let sys = self.systems[s].as_mut().unwrap();
        sys.ejectors[e].live = sys.ejectors[e].live.saturating_sub(1);
    }

    /// `CG_EvaluateParticlePhysics`.
    fn evaluate_physics(&mut self, lib: &ParticleLib, i: usize, cm: &ClipMap) {
        if self.particles[i].at_rest {
            self.particles[i].velocity = [0.0; 3];
            return;
        }
        let (s, e, j) = (self.particles[i].sys, self.particles[i].ej, self.particles[i].bp);
        let sys = self.systems[s].as_ref().unwrap();
        let bp = &lib.systems[sys.handle].ejectors[sys.ejectors[e].base].particles[j];
        let normal = sys.normal;
        let origin = self.particles[i].origin;
        let mut acceleration: Vec3;
        {
            let lk = Self::lookup(&self.particles);
            let att = &mut self.systems[s].as_mut().unwrap().attachment;
            match bp.acc_move_type {
                MoveType::Static => {
                    acceleration = match bp.acc.dir_type {
                        DirType::Point => qm::sub(&bp.acc.point, &origin),
                        DirType::Linear => bp.acc.dir,
                    }
                }
                MoveType::StaticTransform => {
                    let Some(t) = att.axis() else { return };
                    acceleration = match bp.acc.dir_type {
                        DirType::Point => qm::sub(&vec_mat(&bp.acc.point, &t), &origin),
                        DirType::Linear => vec_mat(&bp.acc.dir, &t),
                    }
                }
                MoveType::Tag | MoveType::Cent => match bp.acc.dir_type {
                    DirType::Point => {
                        let Some(pt) = att.point(&lk) else { return };
                        acceleration = qm::sub(&pt, &origin);
                    }
                    DirType::Linear => {
                        let Some(d) = att.dir(&lk) else { return };
                        acceleration = d;
                    }
                },
                MoveType::Normal => {
                    let Some(n) = normal else { return };
                    acceleration = n;
                }
            }
        }
        const MAX_ACC_RADIUS: f32 = 1000.0;
        let rng = &mut self.rng;
        match bp.acc.dir_type {
            DirType::Point => {
                let r2 = qm::dot(&acceleration, &acceleration);
                let mut scale = (MAX_ACC_RADIUS - r2) / MAX_ACC_RADIUS;
                scale = scale.clamp(0.1, 1.0);
                scale *= randomise(rng, bp.acc.mag, bp.acc.mag_rand_frac);
                qm::normalize(&mut acceleration);
                spread_vector(rng, &mut acceleration, bp.acc.dir_rand_angle);
                acceleration = qm::scale(&acceleration, scale);
            }
            DirType::Linear => {
                qm::normalize(&mut acceleration);
                spread_vector(rng, &mut acceleration, bp.acc.dir_rand_angle);
                acceleration = qm::scale(&acceleration, randomise(rng, bp.acc.mag, bp.acc.mag_rand_frac));
            }
        }
        let now = self.now;
        let p = &mut self.particles[i];
        let radius = if bp.physics_radius != 0 {
            bp.physics_radius as f32
        } else {
            lerp_values(p.radius.initial, p.radius.final_, time_frac(now, p.birth, p.life, p.radius.delay))
        };
        let mins = [-radius; 3];
        let maxs = [radius; 3];
        let bounce = randomise(rng, bp.bounce_frac, bp.bounce_frac_rand_frac);
        let delta_time = ((now - p.last_eval) as f64 * 0.001) as f32;
        p.velocity = qm::ma(&p.velocity, delta_time, &acceleration);
        let new_origin = qm::ma(&p.origin, delta_time, &p.velocity);
        p.last_eval = now;

        let solid = contents::SOLID as i32;
        let nodrop = contents::NODROP as i32;
        if !self.bounce_particles {
            let c = cm.point_contents(&new_origin);
            if c & solid != 0 || c & nodrop != 0 {
                self.destroy_particle(lib, i, None);
            } else {
                p.origin = new_origin;
            }
            return;
        }
        let trace = cm.box_trace(&p.origin, &new_origin, Some(&mins), Some(&maxs), solid);
        if trace.fraction == 1.0 || bounce == 0.0 {
            p.origin = new_origin;
            return;
        }
        if cm.point_contents(&trace.endpos) & nodrop != 0 || (bp.cull_on_start_solid && trace.startsolid) {
            self.destroy_particle(lib, i, None);
            return;
        } else if bp.bounce_cull {
            self.destroy_particle(lib, i, Some(trace.plane.normal));
            return;
        }
        let n = trace.plane.normal;
        let dot = qm::dot(&p.velocity, &n);
        p.velocity = qm::ma(&p.velocity, -2.0 * dot, &n);
        p.velocity = qm::scale(&p.velocity, bounce);
        if n[2] > 0.5 && (p.velocity[2] < 40.0 || p.velocity[2] < -(self.frametime as f32) * p.velocity[2]) {
            p.at_rest = true;
        }
        // bounce marks / sounds are not ported (counts are still consumed)
        if !bp.bounce_mark_name.is_empty() && p.bounce_mark_count > 0 {
            p.bounce_mark_count -= 1;
        }
        if !bp.bounce_sound_name.is_empty() && p.bounce_sound_count > 0 {
            p.bounce_sound_count -= 1;
        }
        p.origin = trace.endpos;
    }

    fn base<'a>(&self, lib: &'a ParticleLib, p: &Particle) -> (&'a BaseSystem, &'a BaseParticle) {
        let sys = self.systems[p.sys].as_ref().unwrap();
        let bs = &lib.systems[sys.handle];
        (bs, &bs.ejectors[sys.ejectors[p.ej].base].particles[p.bp])
    }

    fn hidden(&self, bs: &BaseSystem, p: &Particle) -> bool {
        bs.third_person_only && self.first_person_client.is_some() && self.systems[p.sys].as_ref().unwrap().attachment.cent_num() == self.first_person_client
    }

    /// Camera-facing sprites (`CG_RenderParticle`, RT_SPRITE), grouped by
    /// shader, depth sorted back to front within each batch.
    /// `cam_axis` rows: forward, left, up (`cg.refdef.viewaxis`).
    #[allow(dead_code)] // API used by tests / callers to come
    pub fn quads(&self, lib: &ParticleLib, cam_origin: Vec3, cam_axis: [Vec3; 3]) -> Vec<QuadBatch> {
        let mut order: Vec<(usize, f32)> = self
            .particles
            .iter()
            .enumerate()
            .filter(|(_, p)| p.valid)
            .map(|(i, p)| {
                let d = qm::sub(&p.origin, &cam_origin);
                (i, qm::dot(&d, &d))
            })
            .collect();
        if self.depth_sort {
            order.sort_by(|a, b| b.1.total_cmp(&a.1));
        }
        let now = self.now;
        let mut bb = BatchBuilder::default();
        for (i, _) in order {
            let p = &self.particles[i];
            let (bs, bp) = self.base(lib, p);
            if bp.shaders.is_empty() || self.hidden(bs, p) {
                continue;
            }
            let tf = time_frac(now, p.birth, p.life, 0);
            let radius = lerp_values(p.radius.initial, p.radius.final_, time_frac(now, p.birth, p.life, p.radius.delay));
            let mut rgba = [0u8; 4];
            // realLight (trap_R_LightForPoint) is not available: use the colour lerp
            let cf = time_frac(now, p.birth, p.life, p.color_delay);
            for k in 0..3 {
                let range = bp.final_color[k] as f32 - bp.initial_color[k] as f32;
                rgba[k] = (bp.initial_color[k] as f32 + cf * range).clamp(0.0, 255.0) as u8;
            }
            let a = lerp_values(p.alpha.initial, p.alpha.final_, time_frac(now, p.birth, p.life, p.alpha.delay));
            rgba[3] = (255.0 * a).clamp(0.0, 255.0) as u8;
            let rotation = lerp_values(p.rotation.initial, p.rotation.final_, time_frac(now, p.birth, p.life, p.rotation.delay));
            if bp.overdraw_protection && qm::distance(&p.origin, &cam_origin) < radius {
                continue;
            }
            let n = bp.shaders.len();
            let index = if bp.framerate == 0.0 {
                ((tf * (n + 1) as f32) as usize).min(n - 1)
            } else {
                ((bp.framerate as f64 * tf as f64 * p.life as f64 * 0.001) as i64).rem_euclid(n as i64) as usize
            };
            bb.push(&bp.shaders[index], sprite_quad(&p.origin, radius, rotation, &cam_axis, rgba));
        }
        bb.out
    }

    /// Model particles (gibs / casings), `CG_RenderParticle` RT_MODEL.
    pub fn models(&self, lib: &ParticleLib) -> Vec<FxModel> {
        let now = self.now;
        let mut out = Vec::new();
        for p in self.particles.iter().filter(|p| p.valid) {
            let (bs, bp) = self.base(lib, p);
            if bp.models.is_empty() || self.hidden(bs, p) {
                continue;
            }
            let scale = lerp_values(p.radius.initial, p.radius.final_, time_frac(now, p.birth, p.life, p.radius.delay));
            let axis = p.last_axis.map(|r| qm::scale(&r, scale));
            out.push(FxModel { model: bp.models[p.model.min(bp.models.len() - 1)].clone(), origin: p.origin, axis, frame: bp.model_animation.first_frame });
        }
        out
    }

    /// Dynamic lights from `dynamicLight` particles.
    pub fn lights(&self, lib: &ParticleLib) -> Vec<FxLight> {
        let now = self.now;
        let mut out = Vec::new();
        for p in self.particles.iter().filter(|p| p.valid) {
            let (bs, bp) = self.base(lib, p);
            if !bp.dynamic_light || self.hidden(bs, p) {
                continue;
            }
            let r = lerp_values(p.dlight.initial, p.dlight.final_, time_frac(now, p.birth, p.life, p.dlight.delay));
            out.push(FxLight { origin: p.origin, radius: r, color: bp.dlight_color.map(|c| c as f32 / 255.0) });
        }
        out
    }
}

/// Position and velocity of a new particle (middle of `CG_SpawnNewParticle`).
/// `None` where the C returns NULL.
fn init_motion(att: &mut Attachment, lk: &PLookup, rng: &mut Rng, bp: &BaseParticle, normal: Option<Vec3>, p: &mut Particle) -> Option<()> {
    let attachment_point = att.point(lk)?;
    p.origin = attachment_point;
    match att.axis() {
        Some(t) => p.origin = qm::add(&p.origin, &vec_mat(&bp.displacement, &t)),
        None => p.origin = qm::add(&p.origin, &bp.displacement),
    }
    for k in 0..3 {
        p.origin[k] += rng.crandom() * bp.rand_displacement[k];
    }
    match bp.vel_move_type {
        MoveType::Static => match bp.vel.dir_type {
            DirType::Point => p.velocity = qm::sub(&bp.vel.point, &p.origin),
            DirType::Linear => p.velocity = bp.vel.dir,
        },
        MoveType::StaticTransform => {
            let t = att.axis()?;
            match bp.vel.dir_type {
                DirType::Point => p.velocity = qm::sub(&vec_mat(&bp.vel.point, &t), &p.origin),
                DirType::Linear => p.velocity = vec_mat(&bp.vel.dir, &t),
            }
        }
        MoveType::Tag | MoveType::Cent => match bp.vel.dir_type {
            DirType::Point => p.velocity = qm::sub(&attachment_point, &p.origin),
            DirType::Linear => p.velocity = att.dir(lk)?,
        },
        MoveType::Normal => {
            // "ERROR: a particle with velocityType normal has no normal"
            p.velocity = normal?;
            qm::normalize(&mut p.velocity);
            p.origin = qm::ma(&p.origin, bp.normal_displacement, &p.velocity);
        }
    }
    qm::normalize(&mut p.velocity);
    spread_vector(rng, &mut p.velocity, bp.vel.dir_rand_angle);
    p.velocity = qm::scale(&p.velocity, randomise(rng, bp.vel.mag, bp.vel.mag_rand_frac));
    if let Some(av) = att.velocity(lk) {
        p.velocity = qm::ma(&p.velocity, randomise(rng, bp.vel.parent_vel_frac, bp.vel.parent_vel_frac_rand_frac), &av);
    }
    Some(())
}

/// Direction of travel -> axis (model particles).
fn velocity_axis(vel: &Vec3) -> [Vec3; 3] {
    let mut a0 = [0.0; 3];
    qm::normalize2(vel, &mut a0);
    if a0[0] == 0.0 && a0[1] == 0.0 {
        return [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    }
    let mut a2 = qm::project_point_on_plane(&[0.0, 0.0, 1.0], &a0);
    qm::normalize(&mut a2);
    let a1 = qm::cross(&a2, &a0);
    [a0, a1, a2]
}

/// `RB_SurfaceSprite` + `RB_AddQuadStamp`.
pub(crate) fn sprite_quad(origin: &Vec3, radius: f32, rotation: f32, cam_axis: &[Vec3; 3], rgba: [u8; 4]) -> [FxVert; 4] {
    let (left, up) = if rotation == 0.0 {
        (qm::scale(&cam_axis[1], radius), qm::scale(&cam_axis[2], radius))
    } else {
        let ang = std::f32::consts::PI * rotation / 180.0;
        let (s, c) = ang.sin_cos();
        let left = qm::ma(&qm::scale(&cam_axis[1], c * radius), -s * radius, &cam_axis[2]);
        let up = qm::ma(&qm::scale(&cam_axis[2], c * radius), s * radius, &cam_axis[1]);
        (left, up)
    };
    let v = |pos: Vec3, u: f32, t: f32| FxVert { pos, uv: [u, t], rgba };
    [
        v(qm::add(&qm::add(origin, &left), &up), 0.0, 0.0),
        v(qm::add(&qm::sub(origin, &left), &up), 1.0, 0.0),
        v(qm::sub(&qm::sub(origin, &left), &up), 1.0, 1.0),
        v(qm::sub(&qm::add(origin, &left), &up), 0.0, 1.0),
    ]
}

// ---------------------------------------------------------------------------
// Godot renderer

mod render {
    use super::{FxLight, FxModel, QuadBatch};
    use crate::assets::Blend;
    use crate::geom::{q2g, SCALE};
    use crate::models::{tag_basis, Library, ModelInst};
    use godot::classes::base_material_3d::{CullMode, DepthDrawMode, Flags, ShadingMode, TextureParam, Transparency, BlendMode};
    use godot::classes::geometry_instance_3d::ShadowCastingSetting;
    use godot::classes::light_3d::Param as LightParam;
    use godot::classes::mesh::PrimitiveType;
    use godot::classes::{ImmediateMesh, MeshInstance3D, OmniLight3D, StandardMaterial3D};
    use godot::prelude::*;
    use std::collections::HashMap;

    struct Layer {
        mesh: Gd<ImmediateMesh>,
        _mi: Gd<MeshInstance3D>,
        dirty: bool,
    }

    /// Draws particle/trail quads (one ImmediateMesh per shader), model
    /// particles (pooled MD3 instances) and particle dynamic lights.
    pub struct FxRenderer {
        root: Gd<Node3D>,
        layers: HashMap<String, Layer>,
        models: HashMap<String, Vec<ModelInst>>,
        lights: Vec<Gd<OmniLight3D>>,
    }

    const MAX_LIGHTS: usize = 8;

    impl FxRenderer {
        /// Adds an "Fx" node under `parent` and pre-builds materials for
        /// every shader the scripts reference.
        pub fn new(lib: &mut Library, parent: &Gd<Node3D>, plib: &super::ParticleLib, tlib: &super::TrailLib) -> Self {
            let mut root = Node3D::new_alloc();
            root.set_name("Fx");
            parent.clone().add_child(&root);
            let mut r = FxRenderer { root, layers: HashMap::new(), models: HashMap::new(), lights: Vec::new() };
            for s in plib.shader_names().into_iter().chain(tlib.shader_names()) {
                r.layer(lib, &s);
            }
            r
        }

        fn material(lib: &mut Library, shader: &str) -> Gd<StandardMaterial3D> {
            let info = lib.shader_info(shader);
            let mut m = StandardMaterial3D::new_gd();
            m.set_shading_mode(ShadingMode::UNSHADED);
            m.set_flag(Flags::ALBEDO_FROM_VERTEX_COLOR, true);
            m.set_flag(Flags::SRGB_VERTEX_COLOR, true);
            m.set_cull_mode(CullMode::DISABLED);
            if let Some(t) = lib.texture(shader) {
                m.set_texture(TextureParam::ALBEDO, &t);
            }
            match info.blend {
                Blend::Add => {
                    m.set_transparency(Transparency::ALPHA);
                    m.set_blend_mode(BlendMode::ADD);
                    m.set_depth_draw_mode(DepthDrawMode::DISABLED);
                }
                Blend::Mix => {
                    m.set_transparency(Transparency::ALPHA);
                    m.set_blend_mode(BlendMode::MIX);
                    m.set_depth_draw_mode(DepthDrawMode::DISABLED);
                }
                Blend::Opaque => {
                    if info.alpha_test {
                        m.set_transparency(Transparency::ALPHA_SCISSOR);
                        m.set_alpha_scissor_threshold(0.5);
                    }
                }
            }
            // enhanced mode: soft particles, lit smoke (gfx.rs)
            if matches!(info.blend, Blend::Add | Blend::Mix) {
                crate::gfx::register_fx_material(m.clone(), info.blend == Blend::Mix);
            }
            m
        }

        fn layer(&mut self, lib: &mut Library, shader: &str) -> &mut Layer {
            let key = shader.to_lowercase();
            if !self.layers.contains_key(&key) {
                let mesh = ImmediateMesh::new_gd();
                let mut mi = MeshInstance3D::new_alloc();
                mi.set_mesh(&mesh);
                mi.set_material_override(&Self::material(lib, shader));
                mi.set_cast_shadows_setting(ShadowCastingSetting::OFF);
                self.root.add_child(&mi);
                // enhanced mode: heat haze behind flame particles (the same
                // quads, drawn first, bending the screen behind them)
                if key.contains("flame") || key.contains("fire") {
                    let mut sh = godot::classes::Shader::new_gd();
                    sh.set_code(crate::gfx::HAZE_SHADER);
                    let mut hm = godot::classes::ShaderMaterial::new_gd();
                    hm.set_shader(&sh);
                    hm.set_render_priority(-1);
                    if let Some(t) = lib.texture(shader) {
                        hm.set_shader_parameter("tex", &t.to_variant());
                    }
                    let mut haze = MeshInstance3D::new_alloc();
                    haze.set_mesh(&mesh);
                    haze.set_material_override(&hm);
                    haze.set_cast_shadows_setting(ShadowCastingSetting::OFF);
                    haze.set_scale(Vector3::splat(1.6));
                    self.root.add_child(&haze);
                    crate::gfx::register_fx_haze(haze);
                }
                self.layers.insert(key.clone(), Layer { mesh, _mi: mi, dirty: false });
            }
            self.layers.get_mut(&key).unwrap()
        }

        /// Rebuild this frame's geometry. Positions are Quake units.
        pub fn draw(&mut self, lib: &mut Library, batches: &[QuadBatch], models: &[FxModel], lights: &[FxLight]) {
            for l in self.layers.values_mut() {
                if l.dirty {
                    l.mesh.clear_surfaces();
                    l.dirty = false;
                }
            }
            // merge batches that share a shader (particles + trails)
            let mut merged: Vec<(&str, Vec<&QuadBatch>)> = Vec::new();
            for b in batches.iter().filter(|b| b.verts.len() >= 4) {
                match merged.iter_mut().find(|(s, _)| s.eq_ignore_ascii_case(&b.shader)) {
                    Some((_, v)) => v.push(b),
                    None => merged.push((&b.shader, vec![b])),
                }
            }
            for (shader, bs) in merged {
                let l = self.layer(lib, shader);
                l.mesh.surface_begin(PrimitiveType::TRIANGLES);
                for b in bs {
                    for q in b.verts.chunks_exact(4) {
                        for &k in &[0usize, 1, 2, 0, 2, 3] {
                            let v = &q[k];
                            l.mesh.surface_set_color(Color::from_rgba8(v.rgba[0], v.rgba[1], v.rgba[2], v.rgba[3]));
                            l.mesh.surface_set_uv(Vector2::new(v.uv[0], v.uv[1]));
                            l.mesh.surface_add_vertex(q2g(&v.pos));
                        }
                    }
                }
                l.mesh.surface_end();
                l.dirty = true;
            }

            // model particles
            let mut used: HashMap<String, usize> = HashMap::new();
            for m in models {
                let n = used.entry(m.model.clone()).or_insert(0);
                let pool = self.models.entry(m.model.clone()).or_default();
                if pool.len() <= *n {
                    let Some(res) = lib.model(&m.model) else { continue };
                    let mut inst = lib.instance(&res, None);
                    inst.set_frames(m.frame, m.frame, 0.0);
                    inst.set_light([0.6; 3], [0.5; 3], Vector3::new(0.3, 1.0, 0.2).normalized());
                    self.root.add_child(&inst.root);
                    pool.push(inst);
                }
                let inst = &mut pool[*n];
                inst.set_transform(Transform3D::new(tag_basis(&m.axis), q2g(&m.origin)));
                inst.set_visible(true);
                *n += 1;
            }
            for (name, pool) in self.models.iter_mut() {
                let n = used.get(name).copied().unwrap_or(0);
                for inst in pool.iter_mut().skip(n) {
                    inst.set_visible(false);
                }
            }

            // dynamic lights
            while self.lights.len() < lights.len().min(MAX_LIGHTS) {
                let mut l = OmniLight3D::new_alloc();
                l.set_shadow(false);
                l.add_to_group(crate::gfx::DLIGHT_GROUP);
                self.root.add_child(&l);
                self.lights.push(l);
            }
            for (i, l) in self.lights.iter_mut().enumerate() {
                match lights.get(i) {
                    Some(d) if d.radius > 0.0 => {
                        l.set_position(q2g(&d.origin));
                        l.set_param(LightParam::RANGE, d.radius * SCALE);
                        l.set_param(LightParam::ENERGY, 1.0);
                        l.set_color(Color::from_rgb(d.color[0], d.color[1], d.color[2]));
                        l.set_visible(true);
                    }
                    _ => l.set_visible(false),
                }
            }
        }
    }
}

#[allow(unused_imports)] // used by game.rs once integrated
pub use render::FxRenderer;

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn data_files() -> Vec<(String, String)> {
        let d = crate::testdata::d110();
        let v: Vec<(String, String)> = d.list("scripts", &[".particle", ".trail"]).into_iter().map(|f| (f.clone(), d.must_text(&f))).collect();
        assert!(!v.is_empty(), "no scripts/*.particle or *.trail in the test data (see testdata.rs)");
        v
    }

    fn no_resolve(_: &Attach) -> Option<AttachPos> {
        None
    }

    #[test]
    fn value_and_variance() {
        assert_eq!(parse_value_and_variance("400~100", false), (400.0, 0.25));
        assert_eq!(parse_value_and_variance("~360", false), (0.0, 360.0));
        assert_eq!(parse_value_and_variance("0~2", false), (0.0, 2.0));
        assert_eq!(parse_value_and_variance("~0%", false), (0.0, 0.0));
        assert_eq!(parse_value_and_variance("10~50%", false), (10.0, 0.5));
        assert_eq!(parse_value_and_variance("-20.0~40.0", true), (-20.0, -2.0));
        // final values reject negatives (atof_neg -> 1)
        assert_eq!(parse_value_and_variance("-20.0~40.0", false), (1.0, 40.0));
        assert_eq!(parse_value_and_variance(".85", false), (0.85, 0.0));
        assert_eq!(c_atof("1e2x"), 100.0);
        assert_eq!(c_atoi("-12abc"), -12);
    }

    #[test]
    fn lexer_lines() {
        let mut lx = Lexer::new("a b // c\n d \"e f\" /* x \n */ g");
        assert_eq!(lx.parse(), "a");
        assert_eq!(lx.parse_ext(false), "b");
        assert_eq!(lx.parse_ext(false), "");
        assert_eq!(lx.parse(), "d");
        assert_eq!(lx.parse(), "e f");
        assert_eq!(lx.parse(), "g");
        assert_eq!(lx.parse(), "");
    }

    #[test]
    fn particles_parse_all_data() {
        let files = data_files();
        let lib = ParticleLib::load(&files);
        assert!(lib.errors.is_empty(), "{:?}", lib.errors);
        assert_eq!(lib.systems.len(), 31);
        let n_ej: usize = lib.systems.iter().map(|s| s.ejectors.len()).sum();
        let n_p: usize = lib.systems.iter().flat_map(|s| &s.ejectors).map(|e| e.particles.len()).sum();
        assert!(n_ej >= 31 && n_p >= n_ej, "{n_ej} {n_p}");

        let jet = lib.get(lib.handle("JETPACKASCENDPS").unwrap()).unwrap();
        assert_eq!(jet.ejectors[0].total_particles, PARTICLES_INFINITE);
        assert_eq!(jet.ejectors[0].eject.initial, 70.0);
        assert_eq!(jet.ejectors[0].eject.final_, PARTICLES_SAME_AS_INITIAL);
        let p = &jet.ejectors[0].particles[0];
        assert_eq!(p.vel_move_type, MoveType::Tag);
        assert_eq!(p.vel.dir_rand_angle, 15.0);
        assert_eq!(p.acc.dir, [0.0, 0.0, -1.0]);
        assert_eq!(p.radius.initial, 4.0);
        assert_eq!(p.radius.final_, 20.0);
        assert_eq!(p.rotation.initial_rand_frac, 360.0);
        assert_eq!(p.rotation.final_, PARTICLES_SAME_AS_INITIAL);
        assert_eq!(p.shaders, vec!["gfx/sprites/smoke".to_string()]);
        assert_eq!(p.framerate, 0.0);
        assert_eq!(p.life_time, 1500);

        let hive = lib.get(lib.handle("models/weapons/hive/missilePS").unwrap()).unwrap();
        let p = &hive.ejectors[0].particles[0];
        assert_eq!(p.shaders.len(), 2);
        assert_eq!(p.framerate, 20.0);
        assert_eq!(p.acc_move_type, MoveType::Cent);
        assert_eq!(p.acc.dir_type, DirType::Point);
        assert_eq!(p.rand_displacement, [4.0; 3]);

        let imp = lib.get(lib.handle("models/weapons/rifle/impactPS").unwrap()).unwrap();
        assert_eq!(imp.ejectors.len(), 3);
        assert_eq!(imp.ejectors[0].total_particles, 0);
        assert_eq!(imp.ejectors[0].total_particles_rand_frac, 2.0);
        assert_eq!(imp.ejectors[1].particles[0].normal_displacement, 3.0);

        let muz = lib.get(lib.handle("models/weapons/rifle/muzzlePS").unwrap()).unwrap();
        let shell = &muz.ejectors[1].particles[0];
        assert_eq!(shell.models, vec!["models/weapons/shells/rifle-shell.md3".to_string()]);
        assert_eq!(shell.vel_move_type, MoveType::StaticTransform);
        assert_eq!(shell.displacement, [-5.0, -3.0, 0.0]);

        let bleed = lib.get(lib.handle("alienBleedPS").unwrap()).unwrap();
        assert!(bleed.third_person_only);
        assert_eq!(bleed.ejectors[0].particles.len(), 2);
        assert_eq!(bleed.ejectors[0].particles[0].initial_color, [255, 255, 153]);
        assert_eq!(bleed.ejectors[0].particles[0].alpha.delay, 250);

        let hbd = lib.get(lib.handle("humanBuildableDestroyedPS").unwrap()).unwrap();
        let all: Vec<&BaseParticle> = hbd.ejectors.iter().flat_map(|e| &e.particles).collect();
        assert!(all.iter().any(|p| p.child_system_handle == lib.handle("humanBuildableDestroyedTrailPS")));
        let gib = all.iter().find(|p| p.models.len() == 8).unwrap();
        assert_eq!(gib.bounce_mark_name, "gfx/marks/burn_mrk");
        assert!(gib.bounce_sound_name.ends_with("fragmentbounce.wav"));
        let dl = all.iter().find(|p| p.dynamic_light).unwrap();
        assert_eq!(dl.dlight_radius.initial, 300.0);
        assert_eq!(dl.dlight_color, [255, 191, 0]);

        for name in ["humanBuildableDamagedPS", "alienBuildableDamagedPS", "humanBuildableDestroyedPS", "alienBuildableDestroyedPS", "alienAcidTubePS", "firstPersonPoisonCloudPS", "alienEvolvePS", "alienBleedPS", "humanBleedPS", "disconnectPS", "jetPackDescendPS", "jetPackHoverPS"] {
            assert!(lib.handle(name).is_some(), "{name}");
        }
        // registered by cg_main.c but absent from the 1.1.0 data
        for name in ["poisonCloudedPS", "humanBuildableBleedPS", "alienBuildableBleedPS"] {
            assert!(lib.handle(name).is_none(), "{name}");
        }
    }

    #[test]
    fn parse_errors_like_c() {
        let f = |t: &str| ParticleLib::load(&[("scripts/x.particle".to_string(), t.to_string())]);
        let l = f("a { { { bogus 1 } count 1 } }");
        assert_eq!(l.systems.len(), 0);
        assert!(l.errors[0].contains("unknown token 'bogus'"));
        let l = f("a { { { lifeTime 5 } count infinite period 0 - 0 } }");
        assert!(l.errors[0].contains("zero period"));
        // duplicate names: first one wins, the rest of the file continues
        let l = f("a { { { lifeTime 5 } count 1 } } a { { { lifeTime 6 } } } b { }");
        assert_eq!(l.systems.len(), 2);
        assert_eq!(l.systems[0].ejectors[0].particles[0].life_time, 5);
        assert!(l.errors[0].contains("already named"));
    }

    fn lib_and_trails() -> (ParticleLib, TrailLib) {
        let files = data_files();
        (ParticleLib::load(&files), TrailLib::load(&files))
    }

    #[test]
    fn particles_impact_emits_and_expires() {
        let (lib, tlib) = lib_and_trails();
        assert!(!lib.systems.is_empty());
        let cm = ClipMap::default();
        let mut ps = Particles::new();
        let mut ts = Trails::new();
        ps.set_time(1000);
        let h = lib.handle("models/weapons/rifle/impactPS").unwrap();
        let id = ps.spawn(&lib, h, Attach::Point([10.0, 20.0, 30.0]));
        ps.set_normal(id, [0.0, 0.0, 1.0]);
        assert!(ps.is_valid(id));
        assert!(!ps.is_infinite(id));
        let mut max_p = 0;
        let mut quads = 0;
        for t in 0..60 {
            let now = 1000 + t * 16;
            ps.update(&lib, &tlib, now, &cm, &no_resolve, &mut ts);
            ts.update(&tlib, now, &no_resolve, &ps);
            let (_, _, np) = ps.counts();
            max_p = max_p.max(np);
            let q: usize = ps.quads(&lib, [0.0; 3], [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]).iter().map(|b| b.quads()).sum();
            quads = quads.max(q);
        }
        assert!(max_p >= 1, "smoke puff always spawns");
        assert!(quads >= 1);
        // lifetimes are <= 350 ms; everything is gone after ~1 s
        assert_eq!(ps.counts(), (0, 0, 0));
        assert!(!ps.is_valid(id));
    }

    #[test]
    fn particles_infinite_follow_entity_then_destroy() {
        let (lib, tlib) = lib_and_trails();
        assert!(!lib.systems.is_empty());
        let cm = ClipMap::default();
        let mut ps = Particles::new();
        let mut ts = Trails::new();
        let h = lib.handle("jetPackHoverPS").unwrap();
        ps.set_time(0);
        let id = ps.spawn(&lib, h, Attach::Tag { entity: 3, tag: "tag_flash".into() });
        assert!(ps.is_infinite(id));
        let pos = std::cell::Cell::new([0.0f32, 0.0, 100.0]);
        let resolve = |a: &Attach| -> Option<AttachPos> {
            match a {
                Attach::Entity(3) => Some(AttachPos { velocity: [50.0, 0.0, 0.0], ..AttachPos::at(pos.get()) }),
                Attach::Tag { entity: 3, .. } => Some(AttachPos::at(qm::add(&pos.get(), &[0.0, 0.0, 10.0]))),
                _ => None,
            }
        };
        for t in 0..100 {
            pos.set([t as f32 * 2.0, 0.0, 100.0]);
            ps.update(&lib, &tlib, t * 10, &cm, &resolve, &mut ts);
        }
        // period 80 ms, lifetime 1500 ms -> ~12 alive
        let (_, _, np) = ps.counts();
        assert!((10..=14).contains(&np), "{np}");
        let q = ps.quads(&lib, [0.0; 3], [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]);
        assert_eq!(q.len(), 1);
        assert_eq!(q[0].shader, "gfx/sprites/smoke");
        assert_eq!(q[0].quads(), np);
        // hidden in first person for thirdPersonOnly systems only
        ps.first_person_client = Some(3);
        assert_eq!(ps.quads(&lib, [0.0; 3], [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]])[0].quads(), np);
        ps.destroy(id);
        assert!(!ps.is_valid(id));
        for t in 100..300 {
            ps.update(&lib, &tlib, t * 10, &cm, &resolve, &mut ts);
        }
        assert_eq!(ps.counts(), (0, 0, 0));
    }

    #[test]
    fn particles_lazy_remove_when_entity_gone() {
        let (lib, tlib) = lib_and_trails();
        assert!(!lib.systems.is_empty());
        let cm = ClipMap::default();
        let mut ps = Particles::new();
        let mut ts = Trails::new();
        let h = lib.handle("models/weapons/hive/missilePS").unwrap();
        let id = ps.spawn(&lib, h, Attach::Entity(7));
        let alive = std::cell::Cell::new(true);
        let resolve = |a: &Attach| match a {
            Attach::Entity(7) if alive.get() => Some(AttachPos::at([0.0, 0.0, 0.0])),
            _ => None,
        };
        for t in 0..50 {
            ps.update(&lib, &tlib, t * 20, &cm, &resolve, &mut ts);
        }
        assert!(ps.counts().2 > 0);
        alive.set(false);
        for t in 50..400 {
            ps.update(&lib, &tlib, t * 20, &cm, &resolve, &mut ts);
        }
        assert!(!ps.is_valid(id));
        assert_eq!(ps.counts(), (0, 0, 0));
    }

    #[test]
    fn particles_children_and_models() {
        let (lib, tlib) = lib_and_trails();
        assert!(!lib.systems.is_empty());
        let cm = ClipMap::default();
        let mut ps = Particles::new();
        let mut ts = Trails::new();
        let h = lib.handle("humanBuildableDestroyedPS").unwrap();
        let id = ps.spawn(&lib, h, Attach::Point([0.0, 0.0, 0.0]));
        ps.set_normal(id, [0.0, 0.0, 1.0]);
        let mut max_sys = 0;
        let mut saw_models = false;
        let mut saw_light = false;
        for t in 0..40 {
            ps.update(&lib, &tlib, t * 16, &cm, &no_resolve, &mut ts);
            max_sys = max_sys.max(ps.counts().0);
            saw_models |= !ps.models(&lib).is_empty();
            saw_light |= !ps.lights(&lib).is_empty();
        }
        assert!(max_sys > 1, "child systems spawned: {max_sys}");
        assert!(saw_models && saw_light);
        for t in 40..2000 {
            ps.update(&lib, &tlib, t * 16, &cm, &no_resolve, &mut ts);
        }
        assert_eq!(ps.counts(), (0, 0, 0));
    }

    #[test]
    fn child_trail_and_on_death() {
        let text = "
            parent { ejector { particle { shader sync a
                    velocityType static velocity 0 0 1 0 velocityMagnitude 100
                    lifeTime 200 childTrailSystem t1 onDeathSystem boom } count 1 delay 0 period 0 - 0 } }
            boom { ejector { particle { shader sync b
                    lifeTime 100 } count 3 delay 0 period 0 - 0 } }";
        let trail = "t1 { beam { shader c segments 3 segmentTime 50 } }";
        let files = vec![("scripts/x.particle".to_string(), text.to_string()), ("scripts/x.trail".to_string(), trail.to_string())];
        let lib = ParticleLib::load(&files);
        let tlib = TrailLib::load(&files);
        assert!(lib.errors.is_empty() && tlib.errors.is_empty(), "{:?} {:?}", lib.errors, tlib.errors);
        let cm = ClipMap::default();
        let mut ps = Particles::new();
        let mut ts = Trails::new();
        ps.spawn(&lib, lib.handle("parent").unwrap(), Attach::Point([0.0; 3]));
        let mut saw_trail_quads = false;
        let mut saw_boom = false;
        for t in 0..40 {
            let now = t * 10;
            ps.update(&lib, &tlib, now, &cm, &no_resolve, &mut ts);
            ts.update(&tlib, now, &no_resolve, &ps);
            saw_trail_quads |= ts.quads(&tlib, [100.0, 50.0, 0.0]).iter().any(|b| b.shader == "c" && b.quads() > 0);
            saw_boom |= ps.quads(&lib, [100.0, 0.0, 0.0], [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]).iter().any(|b| b.shader == "b");
        }
        assert!(saw_trail_quads);
        assert!(saw_boom);
        for t in 40..100 {
            ps.update(&lib, &tlib, t * 10, &cm, &no_resolve, &mut ts);
            ts.update(&tlib, t * 10, &no_resolve, &ps);
        }
        assert_eq!(ps.counts(), (0, 0, 0));
        assert_eq!(ts.counts(), (0, 0));
    }

    #[test]
    fn sprite_quad_faces_camera() {
        let ax = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
        let q = sprite_quad(&[0.0; 3], 2.0, 0.0, &ax, [255; 4]);
        assert_eq!(q[0].pos, [0.0, 2.0, 2.0]);
        assert_eq!(q[2].pos, [0.0, -2.0, -2.0]);
        assert_eq!(q[1].uv, [1.0, 0.0]);
    }
}
