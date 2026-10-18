//! `Item_Model_Paint` (ui_shared.c) in Godot: an ITEM_TYPE_MODEL item (the
//! main menu's `models/splash/splash_screen.md3`) rendered into its rect
//! the way the ui VM builds the refdef: RDF_NOWORLDMODEL, an identity view
//! axis at the origin, the model `len / 0.268` units ahead (bounds-centred),
//! yawed by `model_angle` (advanced by `model_rotation` ms per degree) and
//! seen with `model_fovx` x `model_fovy`.
//!
//! The scene is a SubViewport with its own world. Q3 projects with
//! independent horizontal and vertical fields of view; the viewport is sized
//! to that aspect (tan(fovx/2) : tan(fovy/2)) with a KEEP_HEIGHT camera and
//! then stretched into the item rect, which is the same projection.
//!
//! The model surfaces get the stages of their shader scripts (tr_shader.c
//! subset: `map`/`clampMap`, `blendFunc` add/blend/filter/GL_ pairs,
//! `rgbGen identity | identityLighting | const | wave | lightingDiffuse`,
//! `tcMod scale / scroll`), one Godot pass per stage (`next_pass`); an image
//! without a script is the implicit `rgbGen lightingDiffuse` stage, lit as
//! `R_SetupEntityLighting` lights an entity without a world (ambient 150+32,
//! directed 150 along the default sun direction).
use std::collections::HashMap;

use godot::classes::camera_3d::KeepAspect;
use godot::classes::sub_viewport::UpdateMode;
use godot::classes::{Camera3D, Material, MeshInstance3D, Node, Node3D, Shader, ShaderMaterial, SubViewport, Texture2D};
use godot::prelude::*;

use crate::geom::{basis_from_angles, q2g, q2g_dir};
use crate::models::{model_basis, Library};

/// `genFunc_t` of a `wave` (0 = none).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum WaveFunc {
    Sin = 1,
    Triangle = 2,
    Square = 3,
    Sawtooth = 4,
    InverseSawtooth = 5,
}

/// `blendFunc` of a stage.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum StageBlend {
    Opaque,
    /// GL_ONE GL_ONE / add
    Add,
    /// GL_SRC_ALPHA GL_ONE_MINUS_SRC_ALPHA / blend
    Mix,
    /// GL_DST_COLOR GL_ZERO / filter
    Filter,
}

/// `rgbGen`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RgbGen {
    Identity,
    Const([f32; 3]),
    /// func, base, amplitude, phase, frequency
    Wave(WaveFunc, f32, f32, f32, f32),
    LightingDiffuse,
}

/// One `shaderStage_t` (the parts the menu model uses).
#[derive(Clone, Debug, PartialEq)]
pub struct Stage {
    pub map: String,
    pub blend: StageBlend,
    pub rgb: RgbGen,
    /// tcMod scale / scroll folded into st' = st * scale + off + vel * t
    pub tc_scale: [f32; 2],
    pub tc_off: [f32; 2],
    pub tc_vel: [f32; 2],
}

impl Stage {
    fn new(map: &str) -> Stage {
        Stage { map: map.to_string(), blend: StageBlend::Opaque, rgb: RgbGen::Identity, tc_scale: [1.0, 1.0], tc_off: [0.0; 2], tc_vel: [0.0; 2] }
    }
}

fn wave_func(s: &str) -> Option<WaveFunc> {
    Some(match s.to_ascii_lowercase().as_str() {
        "sin" => WaveFunc::Sin,
        "triangle" => WaveFunc::Triangle,
        "square" => WaveFunc::Square,
        "sawtooth" => WaveFunc::Sawtooth,
        "inversesawtooth" => WaveFunc::InverseSawtooth,
        _ => return None,
    })
}

fn blend_of(a: &[String]) -> StageBlend {
    let a: Vec<String> = a.iter().map(|x| x.to_ascii_lowercase()).collect();
    match a.as_slice() {
        [x, ..] if x == "add" => StageBlend::Add,
        [x, ..] if x == "blend" => StageBlend::Mix,
        [x, ..] if x == "filter" => StageBlend::Filter,
        [s, d, ..] if (s == "gl_one" || s == "gl_src_alpha") && d == "gl_one" => StageBlend::Add,
        [s, d, ..] if (s == "gl_one" && d == "gl_one_minus_src_color") || (s == "gl_one_minus_dst_color" && d == "gl_one") => StageBlend::Add,
        [s, d, ..] if s == "gl_src_alpha" && d == "gl_one_minus_src_alpha" => StageBlend::Mix,
        [s, d, ..] if (s == "gl_dst_color" && d == "gl_zero") || (s == "gl_zero" && d == "gl_src_color") => StageBlend::Filter,
        _ => StageBlend::Opaque,
    }
}

/// `ParseShader` / `ParseStage` for one shader of a script text: None when
/// the text does not define `name`.
pub fn parse_stages(text: &str, name: &str) -> Option<Vec<Stage>> {
    let want = name.to_ascii_lowercase().replace('\\', "/");
    let mut depth = 0;
    let mut current: Option<String> = None;
    let mut stages: Vec<Stage> = Vec::new();
    let mut st: Option<Stage> = None;
    for raw in text.lines() {
        let line = raw.split("//").next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let toks: Vec<String> = line.split_whitespace().map(|s| s.to_string()).collect();
        let mut i = 0;
        while i < toks.len() {
            let t = toks[i].as_str();
            match t {
                "{" => {
                    depth += 1;
                    if depth == 2 {
                        st = Some(Stage::new(""));
                    }
                }
                "}" => {
                    if depth == 2 {
                        if let Some(s) = st.take().filter(|s| !s.map.is_empty()) {
                            stages.push(s);
                        }
                    }
                    depth -= 1;
                    if depth == 0 {
                        if current.as_deref() == Some(want.as_str()) {
                            return Some(stages);
                        }
                        current = None;
                        stages.clear();
                    }
                }
                _ if depth == 0 => current = Some(t.to_ascii_lowercase().replace('\\', "/")),
                _ if depth == 2 && current.as_deref() == Some(want.as_str()) => {
                    let s = st.get_or_insert_with(|| Stage::new(""));
                    let arg = |k: usize| toks.get(i + k).cloned().unwrap_or_default();
                    let num = |k: usize| toks.get(i + k).and_then(|x| x.parse::<f32>().ok()).unwrap_or(0.0);
                    match t.to_ascii_lowercase().as_str() {
                        "map" | "clampmap" => {
                            let a = arg(1);
                            if !a.starts_with('$') {
                                s.map = a.replace('\\', "/");
                            }
                        }
                        "animmap" => s.map = arg(2).replace('\\', "/"),
                        "blendfunc" => s.blend = blend_of(&toks[i + 1..]),
                        "rgbgen" => {
                            s.rgb = match arg(1).to_ascii_lowercase().as_str() {
                                "wave" => match wave_func(&arg(2)) {
                                    Some(f) => RgbGen::Wave(f, num(3), num(4), num(5), num(6)),
                                    None => RgbGen::Identity,
                                },
                                "const" | "constant" => {
                                    // rgbGen const ( r g b )
                                    let v: Vec<f32> = toks[i + 2..].iter().filter_map(|x| x.trim_matches(|c| c == '(' || c == ')').parse().ok()).collect();
                                    RgbGen::Const([v.first().copied().unwrap_or(1.0), v.get(1).copied().unwrap_or(1.0), v.get(2).copied().unwrap_or(1.0)])
                                }
                                "lightingdiffuse" => RgbGen::LightingDiffuse,
                                _ => RgbGen::Identity,
                            }
                        }
                        "tcmod" => match arg(1).to_ascii_lowercase().as_str() {
                            "scale" => {
                                let (sx, sy) = (num(2), num(3));
                                s.tc_scale = [s.tc_scale[0] * sx, s.tc_scale[1] * sy];
                                s.tc_off = [s.tc_off[0] * sx, s.tc_off[1] * sy];
                                s.tc_vel = [s.tc_vel[0] * sx, s.tc_vel[1] * sy];
                            }
                            "scroll" => {
                                s.tc_vel = [s.tc_vel[0] + num(2), s.tc_vel[1] + num(3)];
                            }
                            _ => {}
                        },
                        _ => {}
                    }
                    i = toks.len();
                    continue;
                }
                _ => {}
            }
            i += 1;
        }
    }
    None
}

/// The refdef and entity of `Item_Model_Paint` for model bounds, the item
/// rect in pixels (after the +1/-2 inset and UI_AdjustFrom640), the
/// model fovs and the current angle: (entity origin, fov_x, fov_y).
pub fn item_model_refdef(mins: [f32; 3], maxs: [f32; 3], w_px: f32, h_px: f32, fov: [f32; 2]) -> ([f32; 3], f32, f32) {
    let mut origin = [0.0f32; 3];
    origin[2] = -0.5 * (mins[2] + maxs[2]);
    origin[1] = 0.5 * (mins[1] + maxs[1]);
    // calculate distance so the model nearly fills the box
    let len = 0.5 * (maxs[2] - mins[2]);
    origin[0] = len / 0.268; // len / tan( fov/2 )
    let fov_x = if fov[0] != 0.0 { fov[0] } else { w_px };
    let fov_y = if fov[1] != 0.0 { fov[1] } else { h_px };
    (origin, fov_x, fov_y)
}

/// Pixel size of the offscreen view for a `h_px` tall rect: the aspect of
/// the projection the two fovs make.
pub fn viewport_size(h_px: f32, fov_x: f32, fov_y: f32) -> (i32, i32) {
    let tx = (fov_x.to_radians() / 2.0).tan();
    let ty = (fov_y.to_radians() / 2.0).tan().max(1e-4);
    let h = h_px.round().clamp(16.0, 2048.0);
    let w = (h * tx / ty).round().clamp(16.0, 4096.0);
    (w as i32, h as i32)
}

const STAGE_SHADER: &str = r#"
shader_type spatial;
render_mode unshaded, cull_disabled, BLEND_MODE, DEPTH_MODE;
uniform sampler2D tex : filter_linear_mipmap, repeat_enable;
uniform int rgb_gen = 0; // 0 identity 1 const 2 wave 3 lightingDiffuse
uniform vec3 rgb_const = vec3(1.0);
uniform int wave_func = 1;
uniform vec4 wave = vec4(1.0, 0.0, 0.0, 0.0); // base amp phase freq
uniform vec2 tc_scale = vec2(1.0);
uniform vec2 tc_off = vec2(0.0);
uniform vec2 tc_vel = vec2(0.0);
uniform float time_s = 0.0;
uniform vec3 sun_dir = vec3(0.45, 0.9, -0.3);
float eval_wave(float x) {
    x = fract(x);
    if (wave_func == 1) return sin(x * 6.2831853);
    if (wave_func == 2) return x < 0.25 ? x * 4.0 : (x < 0.75 ? 2.0 - x * 4.0 : x * 4.0 - 4.0);
    if (wave_func == 3) return x < 0.5 ? 1.0 : -1.0;
    if (wave_func == 4) return x;
    return 1.0 - x;
}
void fragment() {
    vec2 st = UV * tc_scale + tc_off + tc_vel * time_s;
    vec4 c = texture(tex, st);
    vec3 g = vec3(1.0);
    if (rgb_gen == 1) {
        g = rgb_const;
    } else if (rgb_gen == 2) {
        // RB_CalcWaveColor: EvalWaveFormClamped
        g = vec3(clamp(wave.x + wave.y * eval_wave(wave.z + time_s * wave.w), 0.0, 1.0));
    } else if (rgb_gen == 3) {
        // RB_CalcDiffuseColor without a world: ambient 182, directed 150 (x2 overbright)
        vec3 n = normalize((INV_VIEW_MATRIX * vec4(NORMAL, 0.0)).xyz);
        g = vec3(min((182.0 + 150.0 * max(dot(n, normalize(sun_dir)), 0.0)) / 255.0, 1.0));
    }
    vec3 rgb = clamp(c.rgb * g, 0.0, 1.0);
    // gamma-space result to linear (r_gamma is the screen pass's)
    ALBEDO = pow(rgb, vec3(2.2));
    ALPHA_LINE
}
"#;

fn stage_shader(blend: StageBlend) -> Gd<Shader> {
    // the view is composited premultiplied (TremMenus' PREMULT_ALPHA layer):
    // an additive stage adds colour and leaves coverage alone
    // (blend_premul_alpha with ALPHA 0), a blended one covers by its alpha
    let (b, d, a) = match blend {
        StageBlend::Opaque => ("blend_mix", "depth_draw_opaque", ""),
        StageBlend::Add => ("blend_premul_alpha", "depth_draw_never", "ALPHA = 0.0;"),
        StageBlend::Mix => ("blend_mix", "depth_draw_never", "ALPHA = c.a;"),
        StageBlend::Filter => ("blend_mul", "depth_draw_never", ""),
    };
    let mut sh = Shader::new_gd();
    sh.set_code(&STAGE_SHADER.replace("BLEND_MODE", b).replace("DEPTH_MODE", d).replace("ALPHA_LINE", a));
    sh
}

/// One rendered ITEM_TYPE_MODEL: the offscreen viewport, its camera and the
/// entity, plus the stage materials whose `time_s` follows refdef.time.
pub struct MenuModel {
    pub viewport: Gd<SubViewport>,
    entity: Gd<Node3D>,
    mats: Vec<Gd<ShaderMaterial>>,
    camera: Gd<Camera3D>,
    mins: [f32; 3],
    maxs: [f32; 3],
    /// drawn this frame (else its viewport stops updating)
    pub used: bool,
}

impl MenuModel {
    /// `DC->registerModel` + the scene for it; None when the model is missing.
    pub fn new(lib: &mut Library, path: &str, shaders: &mut ShaderScripts, parent: &mut Gd<Node>) -> Option<MenuModel> {
        let res = lib.model(path)?;
        // R_ModelBounds: frame 0 of an MD3
        let (mins, maxs) = res.md3.frame_bounds.first().copied().unwrap_or(([0.0; 3], [0.0; 3]));
        let mut vp = SubViewport::new_alloc();
        vp.set_use_own_world_3d(true);
        vp.set_transparent_background(true);
        vp.set_update_mode(UpdateMode::ALWAYS);
        vp.set_size(Vector2i::new(64, 48));
        let mut cam = Camera3D::new_alloc();
        cam.set_keep_aspect_mode(KeepAspect::HEIGHT);
        cam.set_near(0.05);
        cam.set_far(1000.0);
        // AxisClear( refdef.viewaxis ) at the origin
        cam.set_transform(Transform3D::new(basis_from_angles(&[0.0; 3]), Vector3::ZERO));
        vp.add_child(&cam);
        cam.set_current(true);
        let mut entity = Node3D::new_alloc();
        let mut mats = Vec::new();
        let mut cache: HashMap<StageBlend, Gd<Shader>> = HashMap::new();
        for s in &res.surfs {
            let name = s.shader.replace('\\', "/");
            let stages = shaders.stages(lib, &name).unwrap_or_else(|| {
                // an image without a script: the implicit lightingDiffuse stage
                let mut st = Stage::new(&name);
                st.rgb = RgbGen::LightingDiffuse;
                vec![st]
            });
            let mut first: Option<Gd<ShaderMaterial>> = None;
            let mut prev: Option<Gd<ShaderMaterial>> = None;
            for (k, st) in stages.iter().enumerate() {
                let Some(tex) = lib.texture(&st.map) else { continue };
                let sh = cache.entry(st.blend).or_insert_with(|| stage_shader(st.blend)).clone();
                let mut m = ShaderMaterial::new_gd();
                m.set_shader(&sh);
                m.set_shader_parameter("tex", &tex.upcast::<Texture2D>().to_variant());
                let (gen, c, f, w) = match st.rgb {
                    RgbGen::Identity => (0, [1.0; 3], 1, [1.0, 0.0, 0.0, 0.0]),
                    RgbGen::Const(c) => (1, c, 1, [1.0, 0.0, 0.0, 0.0]),
                    RgbGen::Wave(f, b, a, p, q) => (2, [1.0; 3], f as i32, [b, a, p, q]),
                    RgbGen::LightingDiffuse => (3, [1.0; 3], 1, [1.0, 0.0, 0.0, 0.0]),
                };
                m.set_shader_parameter("rgb_gen", &gen.to_variant());
                m.set_shader_parameter("rgb_const", &Vector3::new(c[0], c[1], c[2]).to_variant());
                m.set_shader_parameter("wave_func", &f.to_variant());
                m.set_shader_parameter("wave", &Vector4::new(w[0], w[1], w[2], w[3]).to_variant());
                m.set_shader_parameter("tc_scale", &Vector2::new(st.tc_scale[0], st.tc_scale[1]).to_variant());
                m.set_shader_parameter("tc_off", &Vector2::new(st.tc_off[0], st.tc_off[1]).to_variant());
                m.set_shader_parameter("tc_vel", &Vector2::new(st.tc_vel[0], st.tc_vel[1]).to_variant());
                m.set_shader_parameter("sun_dir", &q2g_dir(&[0.45, 0.3, 0.9]).to_variant());
                // later stages draw over the earlier ones of the surface
                m.set_render_priority(k as i32);
                if let Some(mut p) = prev.take() {
                    p.set_next_pass(&m.clone().upcast::<Material>());
                }
                if first.is_none() {
                    first = Some(m.clone());
                }
                prev = Some(m.clone());
                mats.push(m);
            }
            let Some(first) = first else { continue };
            let mut mi = MeshInstance3D::new_alloc();
            mi.set_mesh(&s.mesh);
            mi.set_material_override(&first.upcast::<Material>());
            entity.add_child(&mi);
        }
        vp.add_child(&entity);
        parent.add_child(&vp);
        Some(MenuModel { viewport: vp, entity, mats, camera: cam, mins, maxs, used: true })
    }

    /// One `Item_Model_Paint`: the refdef for a `w_px` x `h_px` rect, the
    /// entity at `angle` degrees of yaw, the stage clocks at `time` ms.
    pub fn paint(&mut self, w_px: f32, h_px: f32, fov: [f32; 2], angle: f32, time: i32) {
        let (origin, fov_x, fov_y) = item_model_refdef(self.mins, self.maxs, w_px, h_px, fov);
        let (vw, vh) = viewport_size(h_px, fov_x, fov_y);
        if self.viewport.get_size() != Vector2i::new(vw, vh) {
            self.viewport.set_size(Vector2i::new(vw, vh));
        }
        self.camera.set_fov(fov_y);
        // VectorSet( angles, 0, modelPtr->angle, 0 ); AnglesToAxis( angles, ent.axis )
        self.entity.set_transform(Transform3D::new(model_basis(&[0.0, angle, 0.0]), q2g(&origin)));
        let t = (time as f64 / 1000.0) as f32;
        for m in self.mats.iter_mut() {
            m.set_shader_parameter("time_s", &t.to_variant());
        }
        self.viewport.set_update_mode(UpdateMode::ALWAYS);
        self.used = true;
    }

    /// Not drawn this frame: stop rendering it.
    pub fn idle(&mut self) {
        self.viewport.set_update_mode(UpdateMode::DISABLED);
    }

    pub fn texture(&self) -> Option<Gd<Texture2D>> {
        self.viewport.get_texture().map(|t| t.upcast::<Texture2D>())
    }

}

/// The shader scripts (scripts/*.shader) looked up by name for menu models.
#[derive(Default)]
pub struct ShaderScripts {
    texts: Option<Vec<String>>,
    found: HashMap<String, Option<Vec<Stage>>>,
}

impl ShaderScripts {
    /// The stages of shader `name` (None: no script defines it).
    pub fn stages(&mut self, lib: &mut Library, name: &str) -> Option<Vec<Stage>> {
        let key = name.to_ascii_lowercase();
        if let Some(s) = self.found.get(&key) {
            return s.clone();
        }
        if self.texts.is_none() {
            let files: Vec<String> = lib.pk3.files().filter(|f| f.to_ascii_lowercase().starts_with("scripts/") && f.to_ascii_lowercase().ends_with(".shader")).cloned().collect();
            self.texts = Some(files.iter().filter_map(|f| lib.text(f)).collect());
        }
        let stem = key.trim_end_matches(".tga").trim_end_matches(".jpg").to_string();
        let r = self.texts.as_ref().unwrap().iter().find_map(|t| parse_stages(t, &stem));
        self.found.insert(key, r.clone());
        r
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menumodel_splash_shader_stages() {
        let text = crate::testdata::legacy().must_text("scripts/splash.shader");
        let n = parse_stages(&text, "models/splash/nebula").expect("nebula");
        assert_eq!(n.len(), 2);
        assert_eq!(n[0].map, "models/splash/nebula.tga");
        assert_eq!(n[0].blend, StageBlend::Add);
        assert_eq!(n[0].rgb, RgbGen::Wave(WaveFunc::Sin, 0.5, 1.0, 5.0, 0.05));
        assert_eq!(n[0].tc_scale, [-1.0, 1.0]);
        let t = parse_stages(&text, "models/splash/trem_black").expect("title");
        assert_eq!(t.len(), 1, "the commented-out highlights stage is not one");
        assert_eq!((t[0].map.as_str(), t[0].blend), ("ui/assets/title.tga", StageBlend::Mix));
        assert!(parse_stages(&text, "models/splash/none").is_none());
        // scroll then scale: the scale applies to the scroll too
        let s = parse_stages("a\n{\n{\nmap x.tga\ntcMod scroll 1 0\ntcMod scale 2 2\n}\n}\n", "a").unwrap();
        assert_eq!((s[0].tc_scale, s[0].tc_vel), ([2.0, 2.0], [2.0, 0.0]));
    }

    #[test]
    fn menumodel_item_model_refdef() {
        // the splash model's frame 0 bounds, main.menu: fovx 32 fovy 24
        let (mins, maxs) = ([-335.96875, -200.0, 10.09375], [-242.84375, 200.0, 191.75]);
        let (o, fx, fy) = item_model_refdef(mins, maxs, 1278.0, 718.0, [32.0, 24.0]);
        assert!((o[0] - 90.828125 / 0.268).abs() < 1e-3);
        assert_eq!(o[1], 0.0);
        assert!((o[2] + 100.921875).abs() < 1e-4);
        assert_eq!((fx, fy), (32.0, 24.0));
        // no fov: the C takes the pixel width / height
        assert_eq!(item_model_refdef(mins, maxs, 300.0, 200.0, [0.0, 0.0]).1, 300.0);
        // a 4:3-ish offscreen view for 32 x 24 degrees
        let (w, h) = viewport_size(480.0, 32.0, 24.0);
        assert_eq!(h, 480);
        assert!((w as f32 / h as f32 - (16f32.to_radians().tan() / 12f32.to_radians().tan())).abs() < 0.01);
    }
}
