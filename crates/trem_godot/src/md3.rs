//! Quake 3 / Tremulous model formats, engine-independent:
//! * MD3 (qcommon/qfiles.h md3Header_t ... md3XyzNormal_t), vertex-animated
//!   meshes with per-frame tags,
//! * `.skin` files (surface -> image),
//! * `animation.cfg` for segmented players, non-segmented aliens and
//!   buildables (cg_players.c CG_ParseAnimationFile, cg_buildable.c),
//! * the `lerpFrame_t` animation clock (cg_animation.c CG_RunLerpFrame).

use std::collections::HashMap;

pub const MD3_IDENT: &[u8; 4] = b"IDP3";
pub const MD3_VERSION: i32 = 15;
pub const MD3_XYZ_SCALE: f32 = 1.0 / 64.0;

#[derive(Clone, Debug, PartialEq)]
pub struct Tag {
    pub name: String,
    pub origin: [f32; 3],
    /// rows: forward, left, up (Quake axis[3]).
    pub axis: [[f32; 3]; 3],
}

#[derive(Clone, Debug)]
pub struct Surface {
    pub name: String,
    pub shaders: Vec<String>,
    pub triangles: Vec<[u32; 3]>,
    pub st: Vec<[f32; 2]>,
    /// frames x verts positions (model units).
    pub xyz: Vec<Vec<[f32; 3]>>,
    pub normals: Vec<Vec<[f32; 3]>>,
}

#[derive(Clone, Debug)]
pub struct Md3 {
    #[allow(dead_code)] // kept for debugging dumps
    pub name: String,
    pub num_frames: usize,
    pub frame_bounds: Vec<([f32; 3], [f32; 3])>,
    /// frames x tags.
    pub tags: Vec<Vec<Tag>>,
    pub surfaces: Vec<Surface>,
}

struct Rd<'a>(&'a [u8]);
impl Rd<'_> {
    fn i32(&self, o: usize) -> Result<i32, String> {
        self.0.get(o..o + 4).map(|b| i32::from_le_bytes(b.try_into().unwrap())).ok_or_else(|| "truncated md3".to_string())
    }
    fn f32(&self, o: usize) -> Result<f32, String> {
        self.0.get(o..o + 4).map(|b| f32::from_le_bytes(b.try_into().unwrap())).ok_or_else(|| "truncated md3".to_string())
    }
    fn i16(&self, o: usize) -> Result<i16, String> {
        self.0.get(o..o + 2).map(|b| i16::from_le_bytes(b.try_into().unwrap())).ok_or_else(|| "truncated md3".to_string())
    }
    fn v3(&self, o: usize) -> Result<[f32; 3], String> {
        Ok([self.f32(o)?, self.f32(o + 4)?, self.f32(o + 8)?])
    }
    fn name(&self, o: usize, n: usize) -> Result<String, String> {
        let s = self.0.get(o..o + n).ok_or("truncated md3")?;
        let end = s.iter().position(|&c| c == 0).unwrap_or(n);
        Ok(String::from_utf8_lossy(&s[..end]).into_owned())
    }
}

/// Decode the lat/lng byte pair of md3XyzNormal_t (R_LoadMD3 / LatLongToNormal).
pub fn latlong_normal(n: i16) -> [f32; 3] {
    let lat = ((n as u16 >> 8) & 0xff) as f32 * (2.0 * std::f32::consts::PI / 255.0);
    let lng = (n as u16 & 0xff) as f32 * (2.0 * std::f32::consts::PI / 255.0);
    [lat.cos() * lng.sin(), lat.sin() * lng.sin(), lng.cos()]
}

pub fn parse(bytes: &[u8]) -> Result<Md3, String> {
    let r = Rd(bytes);
    if bytes.len() < 108 || &bytes[0..4] != MD3_IDENT {
        return Err("not an MD3".into());
    }
    if r.i32(4)? != MD3_VERSION {
        return Err(format!("MD3 version {}", r.i32(4)?));
    }
    let name = r.name(8, 64)?;
    let (num_frames, num_tags, num_surfaces) = (r.i32(76)? as usize, r.i32(80)? as usize, r.i32(84)? as usize);
    let (ofs_frames, ofs_tags, ofs_surfaces) = (r.i32(92)? as usize, r.i32(96)? as usize, r.i32(100)? as usize);
    if num_frames == 0 || num_frames > 1024 || num_tags > 16 || num_surfaces > 32 {
        return Err("md3 counts out of range".into());
    }
    let mut frame_bounds = Vec::with_capacity(num_frames);
    for f in 0..num_frames {
        let o = ofs_frames + f * 56;
        frame_bounds.push((r.v3(o)?, r.v3(o + 12)?));
    }
    let mut tags = Vec::with_capacity(num_frames);
    for f in 0..num_frames {
        let mut row = Vec::with_capacity(num_tags);
        for t in 0..num_tags {
            let o = ofs_tags + (f * num_tags + t) * 112;
            row.push(Tag { name: r.name(o, 64)?, origin: r.v3(o + 64)?, axis: [r.v3(o + 76)?, r.v3(o + 88)?, r.v3(o + 100)?] });
        }
        tags.push(row);
    }
    let mut surfaces = Vec::with_capacity(num_surfaces);
    let mut so = ofs_surfaces;
    for _ in 0..num_surfaces {
        if &bytes.get(so..so + 4).ok_or("truncated md3")?[..] != MD3_IDENT {
            return Err("bad md3 surface ident".into());
        }
        let sname = r.name(so + 4, 64)?;
        let s_frames = r.i32(so + 72)? as usize;
        let n_shaders = r.i32(so + 76)? as usize;
        let n_verts = r.i32(so + 80)? as usize;
        let n_tris = r.i32(so + 84)? as usize;
        let (o_tris, o_shaders, o_st, o_xyz, o_end) = (r.i32(so + 88)? as usize, r.i32(so + 92)? as usize, r.i32(so + 96)? as usize, r.i32(so + 100)? as usize, r.i32(so + 104)? as usize);
        if s_frames != num_frames || n_verts > 4096 || n_tris > 8192 {
            return Err(format!("surface {sname}: counts out of range"));
        }
        let shaders = (0..n_shaders).map(|k| r.name(so + o_shaders + k * 68, 64)).collect::<Result<Vec<_>, _>>()?;
        let mut triangles = Vec::with_capacity(n_tris);
        for k in 0..n_tris {
            let o = so + o_tris + k * 12;
            let t = [r.i32(o)?, r.i32(o + 4)?, r.i32(o + 8)?];
            if t.iter().any(|&x| x < 0 || x as usize >= n_verts) {
                return Err(format!("surface {sname}: bad index"));
            }
            triangles.push(t.map(|x| x as u32));
        }
        let st = (0..n_verts).map(|k| Ok([r.f32(so + o_st + k * 8)?, r.f32(so + o_st + k * 8 + 4)?])).collect::<Result<Vec<_>, String>>()?;
        let mut xyz = Vec::with_capacity(num_frames);
        let mut normals = Vec::with_capacity(num_frames);
        for f in 0..num_frames {
            let mut p = Vec::with_capacity(n_verts);
            let mut n = Vec::with_capacity(n_verts);
            for k in 0..n_verts {
                let o = so + o_xyz + (f * n_verts + k) * 8;
                p.push([r.i16(o)? as f32 * MD3_XYZ_SCALE, r.i16(o + 2)? as f32 * MD3_XYZ_SCALE, r.i16(o + 4)? as f32 * MD3_XYZ_SCALE]);
                n.push(latlong_normal(r.i16(o + 6)?));
            }
            xyz.push(p);
            normals.push(n);
        }
        surfaces.push(Surface { name: sname, shaders, triangles, st, xyz, normals });
        so += o_end;
    }
    Ok(Md3 { name, num_frames, frame_bounds, tags, surfaces })
}

impl Md3 {
    /// `R_LerpTag`: interpolated tag between two frames (backlerp = weight of `old`).
    pub fn lerp_tag(&self, name: &str, old: usize, new: usize, backlerp: f32) -> Option<Tag> {
        let old = old.min(self.num_frames - 1);
        let new = new.min(self.num_frames - 1);
        let ti = self.tags.first()?.iter().position(|t| t.name == name)?;
        let (a, b) = (&self.tags[old][ti], &self.tags[new][ti]);
        let fl = 1.0 - backlerp;
        let mut out = Tag { name: name.to_string(), origin: [0.0; 3], axis: [[0.0; 3]; 3] };
        for i in 0..3 {
            out.origin[i] = a.origin[i] * backlerp + b.origin[i] * fl;
            for j in 0..3 {
                out.axis[i][j] = a.axis[i][j] * backlerp + b.axis[i][j] * fl;
            }
        }
        for row in out.axis.iter_mut() {
            let l = (row[0] * row[0] + row[1] * row[1] + row[2] * row[2]).sqrt();
            if l > 0.0 {
                row.iter_mut().for_each(|x| *x /= l);
            }
        }
        Some(out)
    }
    /// Union of every frame's bounds (for culling an animated mesh).
    pub fn all_bounds(&self) -> ([f32; 3], [f32; 3]) {
        let mut mn = [f32::MAX; 3];
        let mut mx = [f32::MIN; 3];
        for (a, b) in &self.frame_bounds {
            for i in 0..3 {
                mn[i] = mn[i].min(a[i]);
                mx[i] = mx[i].max(b[i]);
            }
        }
        (mn, mx)
    }
}

/// `.skin`: "surface,image" lines (tags have empty images).
pub fn parse_skin(text: &str) -> HashMap<String, String> {
    let mut m = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if let Some((s, img)) = line.split_once(',') {
            let img = img.trim().trim_matches('"');
            if !img.is_empty() {
                m.insert(s.trim().to_lowercase(), img.to_string());
            }
        }
    }
    m
}

/// `animation_t`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Animation {
    pub first_frame: i32,
    pub num_frames: i32,
    pub loop_frames: i32,
    pub frame_lerp: i32,
    pub initial_lerp: i32,
    pub reversed: bool,
    pub flipflop: bool,
}

#[derive(Clone, Debug, Default)]
pub struct AnimSet {
    pub anims: Vec<Animation>,
    pub nonsegmented: bool,
    pub footsteps: String,
}

/// Tokenizer of COM_Parse with // and /* */ comments.
fn tokens(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut s = text;
    loop {
        s = s.trim_start();
        if s.is_empty() {
            break;
        }
        if s.starts_with("//") {
            s = s.find('\n').map(|i| &s[i..]).unwrap_or("");
            continue;
        }
        if s.starts_with("/*") {
            s = s.find("*/").map(|i| &s[i + 2..]).unwrap_or("");
            continue;
        }
        if let Some(rest) = s.strip_prefix('"') {
            let e = rest.find('"').unwrap_or(rest.len());
            out.push(rest[..e].to_string());
            s = rest.get(e + 1..).unwrap_or("");
            continue;
        }
        let e = s.find(|c: char| c.is_whitespace()).unwrap_or(s.len());
        out.push(s[..e].to_string());
        s = &s[e..];
    }
    out
}

fn anim_from(first: i32, num: i32, looping: i32, fps: f32) -> Animation {
    let fps = if fps == 0.0 { 1.0 } else { fps };
    let lerp = (1000.0 / fps) as i32;
    Animation { first_frame: first, num_frames: num.abs(), loop_frames: looping, frame_lerp: lerp, initial_lerp: lerp, reversed: num < 0, flipflop: false }
}

// bg_public.h animNumber_t indices used below
pub const TORSO_GESTURE: usize = 6;
pub const LEGS_WALKCR: usize = 13;
pub const LEGS_WALK: usize = 14;
pub const TORSO_GETFLAG: usize = 25;
pub const TORSO_NEGATIVE: usize = 30;
pub const MAX_PLAYER_ANIMATIONS: usize = 31;
pub const LEGS_BACKCR: usize = 32;
pub const LEGS_BACKWALK: usize = 33;
pub const MAX_PLAYER_TOTALANIMATIONS: usize = 37;
pub const MAX_NONSEG_PLAYER_ANIMATIONS: usize = 32;
pub const MAX_BUILDABLE_ANIMATIONS: usize = 15;

/// bg_public.h MAX_WEAPON_ANIMATIONS (WANIM_NONE .. WANIM_ATTACK8).
pub const MAX_WEAPON_ANIMATIONS: usize = 13;

/// `CG_ParseWeaponAnimationFile`: models/weapons/<name>/animation.cfg, one
/// "first num loop fps" line per animation from WANIM_IDLE on, or the
/// `noDrift` flag. Returns (animations, noDrift); Err when the file is
/// short like the C (which then keeps the zeroed animations: frame 0).
pub fn parse_weapon_anims(text: &str) -> Result<(AnimSet, bool), String> {
    let t = tokens(text);
    let mut set = AnimSet { anims: vec![Animation::default(); MAX_WEAPON_ANIMATIONS], ..Default::default() };
    let mut no_drift = false;
    let mut p = 0;
    let mut i = 1;
    let next = |p: &mut usize| {
        let tok = t.get(*p).cloned();
        *p += 1;
        tok
    };
    while i < MAX_WEAPON_ANIMATIONS {
        let Some(tok) = next(&mut p) else { break };
        if tok.eq_ignore_ascii_case("noDrift") {
            no_drift = true;
            continue;
        }
        let first = crate::md3::atoi(&tok);
        let Some(num) = next(&mut p) else { break };
        let Some(looping) = next(&mut p) else { break };
        let Some(fps) = next(&mut p) else { break };
        set.anims[i] = anim_from(first, crate::md3::atoi(&num), crate::md3::atoi(&looping), fps.parse::<f32>().unwrap_or(0.0));
        i += 1;
    }
    if i != MAX_WEAPON_ANIMATIONS {
        return Err("Error parsing animation file".into());
    }
    Ok((set, no_drift))
}

/// C `atoi` (leading integer, else 0).
fn atoi(s: &str) -> i32 {
    trem_sim::kernel::spawn::c_atoi(s)
}

/// `CG_ParseAnimationFile` (players: segmented or `nonsegmented`).
pub fn parse_player_anims(text: &str) -> Result<AnimSet, String> {
    let t = tokens(text);
    let mut i = 0;
    let mut set = AnimSet { footsteps: "normal".into(), ..Default::default() };
    while i < t.len() {
        let tok = t[i].to_lowercase();
        if tok.chars().next().map(|c| c.is_ascii_digit()).unwrap_or(false) {
            break;
        }
        match tok.as_str() {
            "footsteps" => {
                set.footsteps = t.get(i + 1).cloned().unwrap_or_default().to_lowercase();
                i += 2;
            }
            "headoffset" => i += 4,
            "sex" => i += 2,
            "nonsegmented" => {
                set.nonsegmented = true;
                i += 1
            }
            _ => i += 1,
        }
    }
    let nums: Vec<&String> = t[i..].iter().collect();
    let get = |k: usize| -> Option<f32> { nums.get(k).and_then(|s| s.parse::<f32>().ok()) };
    if !set.nonsegmented {
        let mut anims = vec![Animation::default(); MAX_PLAYER_TOTALANIMATIONS];
        let mut skip = 0;
        let mut a = 0;
        let mut k = 0;
        while a < MAX_PLAYER_ANIMATIONS {
            let Some(first) = get(k) else {
                if (TORSO_GETFLAG..=TORSO_NEGATIVE).contains(&a) {
                    anims[a] = Animation { reversed: false, flipflop: false, ..anims[TORSO_GESTURE] };
                    a += 1;
                    continue;
                }
                break;
            };
            let mut first = first as i32;
            if a == LEGS_WALKCR {
                skip = first - anims[TORSO_GESTURE].first_frame;
            }
            if (LEGS_WALKCR..TORSO_GETFLAG).contains(&a) {
                first -= skip;
            }
            let (Some(n), Some(l), Some(f)) = (get(k + 1), get(k + 2), get(k + 3)) else { break };
            anims[a] = anim_from(first, n as i32, l as i32, f);
            a += 1;
            k += 4;
        }
        if a != MAX_PLAYER_ANIMATIONS {
            return Err("Error parsing animation file".into());
        }
        anims[LEGS_BACKCR] = Animation { reversed: true, ..anims[LEGS_WALKCR] };
        anims[LEGS_BACKWALK] = Animation { reversed: true, ..anims[LEGS_WALK] };
        set.anims = anims;
    } else {
        let mut anims = vec![Animation::default(); MAX_NONSEG_PLAYER_ANIMATIONS];
        let mut a = 0;
        while a < MAX_NONSEG_PLAYER_ANIMATIONS {
            let (Some(first), Some(n), Some(l), Some(f)) = (get(a * 4), get(a * 4 + 1), get(a * 4 + 2), get(a * 4 + 3)) else { break };
            anims[a] = anim_from(first as i32, n as i32, l as i32, f);
            a += 1;
        }
        // the 1.1 configs stop before the last few death entries on some models
        if a < 20 {
            return Err("Error parsing animation file".into());
        }
        set.anims = anims;
    }
    Ok(set)
}

/// `CG_ParseBuildableAnimationFile`: entries for BANIM_NONE+1 .. MAX.
pub fn parse_buildable_anims(text: &str) -> Result<AnimSet, String> {
    let t = tokens(text);
    let nums: Vec<f32> = t.iter().filter_map(|s| s.parse().ok()).collect();
    let mut anims = vec![Animation::default(); MAX_BUILDABLE_ANIMATIONS];
    for a in 1..MAX_BUILDABLE_ANIMATIONS {
        let k = (a - 1) * 4;
        if k + 3 >= nums.len() {
            return Err("Error parsing buildable animation file".into());
        }
        anims[a] = anim_from(nums[k] as i32, nums[k + 1] as i32, nums[k + 2] as i32, nums[k + 3]);
    }
    Ok(AnimSet { anims, nonsegmented: false, footsteps: String::new() })
}

/// `sound.cfg` of a buildable: (enabled, looped) per BANIM.
pub fn parse_buildable_sounds(text: &str) -> Vec<(bool, bool)> {
    let nums: Vec<i32> = tokens(text).iter().filter_map(|s| s.parse().ok()).collect();
    let mut out = vec![(false, false); MAX_BUILDABLE_ANIMATIONS];
    for a in 1..MAX_BUILDABLE_ANIMATIONS {
        let k = (a - 1) * 2;
        if k + 1 < nums.len() {
            out[a] = (nums[k] != 0, nums[k + 1] != 0);
        }
    }
    out
}

/// Key/value config (overrides/classes/*.cfg, overrides/buildables/*.cfg, weapon.cfg top level).
pub fn parse_kv(text: &str) -> Vec<Vec<String>> {
    text.lines()
        .map(|l| l.split("//").next().unwrap_or(""))
        .map(tokens)
        .filter(|t| !t.is_empty())
        .collect()
}

/// `lerpFrame_t` + `CG_RunLerpFrame` / `CG_SetLerpFrameAnimation`.
#[derive(Clone, Copy, Debug, Default)]
pub struct LerpFrame {
    pub old_frame: i32,
    pub old_frame_time: i32,
    pub frame: i32,
    pub frame_time: i32,
    pub backlerp: f32,
    pub animation_number: i32,
    pub animation: Option<Animation>,
    pub animation_time: i32,
}

impl LerpFrame {
    pub fn set_animation(&mut self, set: &AnimSet, number: i32, toggle_mask: i32) {
        self.animation_number = number;
        let n = (number & !toggle_mask) as usize;
        let anim = set.anims.get(n).copied().unwrap_or_default();
        self.animation = Some(anim);
        self.animation_time = self.frame_time + anim.initial_lerp;
    }

    /// `CG_RunLerpFrame(lf, 1.0)` at time `now`.
    pub fn run(&mut self, now: i32) {
        let Some(anim) = self.animation else { return };
        if now >= self.frame_time {
            self.old_frame = self.frame;
            self.old_frame_time = self.frame_time;
            if anim.frame_lerp == 0 {
                return;
            }
            if now < self.animation_time {
                self.frame_time = self.animation_time;
            } else {
                self.frame_time = self.old_frame_time + anim.frame_lerp;
            }
            let mut f = (self.frame_time - self.animation_time) / anim.frame_lerp;
            let num = if anim.flipflop { anim.num_frames * 2 } else { anim.num_frames };
            if f >= num {
                f -= num;
                if anim.loop_frames != 0 {
                    f %= anim.loop_frames;
                    f += anim.num_frames - anim.loop_frames;
                } else {
                    f = num - 1;
                    self.frame_time = now;
                }
            }
            self.frame = if anim.reversed {
                anim.first_frame + anim.num_frames - 1 - f
            } else if anim.flipflop && f >= anim.num_frames {
                anim.first_frame + anim.num_frames - 1 - (f % anim.num_frames)
            } else {
                anim.first_frame + f
            };
            if now > self.frame_time {
                self.frame_time = now;
            }
        }
        if self.frame_time > now + 200 {
            self.frame_time = now;
        }
        if self.old_frame_time > now {
            self.old_frame_time = now;
        }
        self.backlerp = if self.frame_time == self.old_frame_time { 0.0 } else { 1.0 - (now - self.old_frame_time) as f32 / (self.frame_time - self.old_frame_time) as f32 };
    }

    /// `CG_RunPlayerLerpFrame`.
    pub fn run_player(&mut self, set: &AnimSet, anim: i32, now: i32) {
        if anim != self.animation_number || self.animation.is_none() {
            self.set_animation(set, anim, 0x80);
        }
        self.run(now);
    }

    /// `CG_ClearLerpFrame`.
    pub fn clear(&mut self, set: &AnimSet, anim: i32, now: i32) {
        self.frame_time = now;
        self.old_frame_time = now;
        self.set_animation(set, anim, 0x80);
        let f = self.animation.map(|a| a.first_frame).unwrap_or(0);
        self.old_frame = f;
        self.frame = f;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weapon_animation_cfg() {
        let mut text = String::from("noDrift\n");
        for k in 1..MAX_WEAPON_ANIMATIONS {
            text += &format!("{} {} 0 {}\n", k * 10, if k == 2 { -5 } else { 5 }, 20);
        }
        let (set, nd) = parse_weapon_anims(&text).unwrap();
        assert!(nd);
        assert_eq!(set.anims[1].first_frame, 10);
        assert_eq!(set.anims[1].frame_lerp, 50);
        assert!(set.anims[2].reversed && set.anims[2].num_frames == 5);
        assert_eq!(set.anims[12].first_frame, 120);
        // too short: rejected like the C
        assert!(parse_weapon_anims("0 1 0 10").is_err());
    }

    #[test]
    fn human_animation_cfg_skips_torso_frames_for_legs() {
        let cfg = "sex m\nfootsteps custom\n0 30 0 25\n29 1 0 25\n30 30 0 25\n59 1 0 25\n60 30 0 25\n89 1 0 25\n90 40 0 20\n130 6 0 15\n136 6 0 15\n142 5 0 20\n147 4 0 20\n151 1 0 15\n152 1 0 15\n153 8 8 16\n161 12 12 20\n173 9 9 15\n182 10 10 20\n192 10 10 15\n202 8 0 15\n210 1 0 15\n202 8 0 15\n210 1 0 15\n220 10 10 15\n230 10 10 15\n240 7 7 15\n";
        let s = parse_player_anims(cfg).unwrap();
        assert!(!s.nonsegmented);
        assert_eq!(s.footsteps, "custom");
        // skip = 153 - 90 = 63
        assert_eq!(s.anims[LEGS_WALKCR].first_frame, 90);
        assert_eq!(s.anims[LEGS_WALK].first_frame, 161 - 63);
        assert_eq!(s.anims[TORSO_GETFLAG].first_frame, 90);
        assert!(s.anims[LEGS_BACKWALK].reversed);
        assert_eq!(s.anims[0].frame_lerp, 40);
    }

    #[test]
    fn lerp_frame_loops_and_clamps() {
        let set = AnimSet { anims: vec![anim_from(10, 4, 4, 10.0), anim_from(20, 3, 0, 10.0)], ..Default::default() };
        let mut lf = LerpFrame::default();
        lf.clear(&set, 0, 0);
        let mut seen = Vec::new();
        for t in (0..1000).step_by(100) {
            lf.run_player(&set, 0, t);
            seen.push(lf.frame);
        }
        assert!(seen.iter().all(|&f| (10..14).contains(&f)), "{seen:?}");
        assert!(seen.windows(2).any(|w| w[1] < w[0]), "loops: {seen:?}");
        for t in (1000..2000).step_by(100) {
            lf.run_player(&set, 1, t);
        }
        assert_eq!(lf.frame, 22, "one-shot sticks on its last frame");
    }

    #[test]
    fn normals_are_unit() {
        for n in [0i16, 0x1234, -1, 0x7f7f] {
            let v = latlong_normal(n);
            assert!(((v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt() - 1.0).abs() < 1e-4);
        }
    }

    /// Runs against the real data-1.1.0.pk3 (or TREM_TEST_DATA_110, see testdata.rs).
    #[test]
    fn real_models_parse() {
        let d = crate::testdata::d110();
        for m in ["models/players/human_base/lower.md3", "models/players/human_base/upper.md3", "models/players/level4/nonseg.md3", "models/buildables/mgturret/turret_base.md3", "models/weapons/rifle/rifle.md3"] {
            let md = parse(&d.must_read(m)).unwrap_or_else(|e| panic!("{m}: {e}"));
            assert!(md.num_frames >= 1 && !md.surfaces.is_empty(), "{m}");
        }
        for c in ["human_base", "human_bsuit", "level0", "level1", "level2", "level3", "level4", "builder"] {
            let t = d.must_text(&format!("models/players/{c}/animation.cfg"));
            let s = parse_player_anims(&t).unwrap_or_else(|e| panic!("{c}: {e}"));
            assert_eq!(s.nonsegmented, !c.starts_with("human"), "{c}");
        }
        for b in ["eggpod", "overmind", "barricade", "acid_tube", "trapper", "booster", "hive", "telenode", "mgturret", "tesla", "arm", "dcc", "medistat", "reactor", "repeater"] {
            let t = d.must_text(&format!("models/buildables/{b}/animation.cfg"));
            parse_buildable_anims(&t).unwrap_or_else(|e| panic!("{b}: {e}"));
        }
    }
}
