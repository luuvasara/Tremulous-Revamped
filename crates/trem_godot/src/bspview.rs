//! Rendering IBSP maps from the player's own pk3 files (dec003): pk3 access
//! through Godot's ZIPReader, shader -> texture resolution, patch
//! subdivision (`R_SubdividePatchToGrid`) and lightmapped meshes. Presentation only — collision comes
//! from `trem_sim::kernel::bsp`.

use godot::classes::image::Format as ImageFormat;
use godot::classes::mesh::PrimitiveType;
use godot::classes::{Image, ImageTexture, MeshInstance3D, Shader, ShaderMaterial, SurfaceTool, Texture2DArray};
use crate::assets::{flat_texture, load_image, Pk3Set};
use godot::prelude::*;
use std::collections::HashMap;
use trem_sim::kernel::bsp::{Bsp, DrawVert, MST_PATCH, MST_PLANAR, MST_TRIANGLE_SOUP};

use crate::geom::{q2g, q2g_dir};

const SURF_SKY: i32 = 0x4;
const SURF_NODRAW: i32 = 0x80;

const WORLD_SHADER: &str = r#"
shader_type spatial;
// trem_gfx 0: vanilla, the original renderer's result as emission (no
// lighting, no post effects); 1: enhanced, the same texture and baked light
// as a lit surface: the light map is the irradiance (SSAO / SSIL / the ray
// tracer act on it), a generated normal map bumps it along the light grid's
// dominant direction (CUSTOM0: direction xyz, directed fraction w).
// r_gamma 1.3 (a common Tremulous setting): gamma-space result -> linear
const float R_GAMMA_EXP = 2.2 / 1.3;
render_mode cull_disabled;
global uniform float trem_gfx;
// @Q3_COMMON
// where the light comes from: 0 the light map (or the vertex colours when
// use_lm is off), 1 the vertex colours, 2 none (rgbGen identity: the
// texture as is; the stage's factor is in tint)
uniform int light_src = 0;
uniform sampler2D tex : filter_linear_mipmap_anisotropic, repeat_enable;
// the 4K pack's texture (enhanced mode only)
uniform sampler2D tex_hd : filter_linear_mipmap_anisotropic, repeat_enable;
uniform bool has_hd = false;
uniform sampler2D lm : filter_linear;
uniform bool use_lm = false;
uniform vec4 tint = vec4(1.0);
// the shader script's alphaFunc (0 none: the image's alpha is ignored, as the
// renderer does - grates like atcs's eq2_grate_01 carry alpha holes their
// shader never cuts; 1 GT0, 2 LT128, 3 GE128)
uniform int alpha_func = 0;
uniform sampler2D nrm : hint_normal, filter_linear_mipmap_anisotropic, repeat_enable;
uniform bool has_nrm = false;
uniform float rough = 0.7;
uniform float spec = 0.4;
uniform float bump = 1.0;
uniform float irr_scale = 1.0;
// enhanced materials (gfx.rs surface_class): metalness, a wet clearcoat,
// and the close-range detail normal map that stands in for parallax mapping
uniform float metal = 0.0;
uniform float wet = 0.0;
uniform float detail_k = 0.0;
uniform sampler2D detail : hint_normal, filter_linear_mipmap_anisotropic, repeat_enable;
// enhanced dynamic shadows (lamps.rs): how much of the baked light a
// model's shadow takes away here (the directed share of the light grid,
// facing it), the darkest a shadow gets; the grid's direction (view space)
global uniform float trem_shadow_k;
varying vec4 grid;
varying vec3 shadow_irr;
varying vec3 shadow_gdir;
// @SHADOW
void vertex() {
    grid = CUSTOM0;
// @Q3_DEFORM
// @Q3_PULL
}
void fragment() {
    shadow_irr = vec3(0.0);
    shadow_gdir = vec3(0.0, 0.0, 1.0);
    vec4 t = ((trem_gfx > 0.5 && has_hd) ? texture(tex_hd, UV) : texture(tex, UV)) * tint;
    if ((alpha_func == 1 && t.a <= 0.0) || (alpha_func == 2 && t.a >= 0.5) || (alpha_func == 3 && t.a < 0.5)) { discard; }
    vec3 light = light_src == 2 ? vec3(0.5) : (use_lm ? texture(lm, UV2).rgb : COLOR.rgb);
    // R_ColorShiftLightingBytes (mapOverBrightBits 2 - overBrightBits 1 = x2,
    // hue-preserving clamp), then the x2 hardware overbright; all in gamma space.
    light *= 2.0;
    float m = max(light.r, max(light.g, light.b));
    if (m > 1.0) { light /= m; }
    if (trem_gfx < 0.5) {
        ALBEDO = vec3(0.0);
        ROUGHNESS = 1.0;
        SPECULAR = 0.0;
        METALLIC = 0.0;
        EMISSION = pow(clamp(t.rgb * light * 2.0, 0.0, 1.0), vec3(R_GAMMA_EXP));
    } else {
        vec3 alb = pow(clamp(t.rgb, 0.0, 1.0), vec3(R_GAMMA_EXP));
        // the overbright is kept (HDR, tone mapped) instead of clamped
        vec3 baked = pow(light * 2.0, vec3(R_GAMMA_EXP));
        float k = 1.0;
        if (has_nrm) {
            vec3 tn = texture(nrm, UV).xyz;
            vec3 nm = tn * 2.0 - 1.0;
            nm.xy *= bump;
            vec3 nb = normalize(TANGENT * nm.x + BINORMAL * nm.y + NORMAL * nm.z);
            vec3 L = normalize((VIEW_MATRIX * vec4(grid.xyz, 0.0)).xyz + vec3(1e-5));
            float d = clamp(grid.w, 0.0, 0.85);
            float lit_b = (1.0 - d) + d * max(dot(nb, L), 0.0);
            float lit_g = (1.0 - d) + d * max(dot(NORMAL, L), 0.0);
            k = clamp(lit_b / max(lit_g, 0.05), 0.45, 1.7);
            NORMAL_MAP = tn;
            NORMAL_MAP_DEPTH = bump;
        }
        // close range: fine relief (whiteout blend over the surface's own
        // normal map), fading out by 12 units so distant walls stay calm
        float near = 1.0 - smoothstep(4.0, 12.0, length(VERTEX));
        if (detail_k > 0.0 && near > 0.0) {
            vec3 base_n = has_nrm ? texture(nrm, UV).xyz * 2.0 - 1.0 : vec3(0.0, 0.0, 1.0);
            vec3 dn = texture(detail, UV * 6.0).xyz * 2.0 - 1.0;
            vec3 bn = normalize(vec3(base_n.xy + dn.xy * detail_k * near, base_n.z * dn.z));
            NORMAL_MAP = bn * 0.5 + 0.5;
            NORMAL_MAP_DEPTH = has_nrm ? bump : 1.0;
        }
        // cavity: dark texels (cracks, seams) take less of the baked light
        float lum = dot(alb, vec3(0.3, 0.59, 0.11));
        AO = mix(1.0, clamp(lum * 4.0, 0.55, 1.0), 0.35);
        AO_LIGHT_AFFECT = 0.0;
        ALBEDO = alb;
        ROUGHNESS = rough;
        SPECULAR = spec;
        METALLIC = metal;
        CLEARCOAT = wet;
        CLEARCOAT_ROUGHNESS = 0.15;
        IRRADIANCE = vec4(baked * k * irr_scale, 1.0);
        RADIANCE = vec4(baked * 0.3 * irr_scale, 1.0);
        vec3 gl = normalize((VIEW_MATRIX * vec4(grid.xyz, 0.0)).xyz + vec3(1e-5));
        float facing = smoothstep(0.0, 0.25, dot(NORMAL, gl));
        shadow_gdir = gl;
        shadow_irr = baked * k * irr_scale * clamp(grid.w, 0.0, 0.85) * facing * trem_shadow_k;
    }
}
// The lights a map surface takes besides its baked light: the dynamic
// lights (muzzle flashes, explosions: Lambert + a highlight, as the default
// would) and, enhanced, the shadow lights (lamps.rs), which add nothing and
// only take the directed share of the baked light away where a model's
// shadow falls.
void light() {
    if (LIGHT_COLOR.r < 0.0) {
        // a world-fixed shadow light (lamps.rs): takes, adds nothing
        DIFFUSE_LIGHT -= shadow_irr * trem_lamp_shadow(LIGHT, LIGHT_COLOR.r, ATTENUATION, shadow_gdir);
    } else if (!LIGHT_IS_DIRECTIONAL) {
        float ndl = clamp(dot(NORMAL, LIGHT), 0.0, 1.0);
        DIFFUSE_LIGHT += LIGHT_COLOR * ATTENUATION * ndl / PI;
        vec3 h = normalize(VIEW + LIGHT);
        float shin = mix(256.0, 6.0, ROUGHNESS);
        float s = pow(clamp(dot(NORMAL, h), 0.0, 1.0), shin) * (shin + 8.0) / (8.0 * PI);
        SPECULAR_LIGHT += LIGHT_COLOR * ATTENUATION * ndl * s * SPECULAR_AMOUNT * 0.5;
    }
}
"#;

fn world_shader_code() -> String {
    WORLD_SHADER
        .replace("// @Q3_COMMON", crate::q3shader::GLSL_COMMON)
        .replace("// @Q3_DEFORM", crate::q3shader::GLSL_VERTEX_DEFORM)
        .replace("// @Q3_PULL", crate::q3shader::GLSL_VERTEX_PULL)
        .replace("// @SHADOW", crate::lamps::GLSL_SHADOW)
}

/// The world's opaque triangles for the ray tracer (rt.rs), in Godot space:
/// positions, indices and per-vertex surface data (the outgoing baked
/// radiance = mean albedo x light map, and the mean albedo, both linear).
#[derive(Default, Clone)]
pub struct RtGeometry {
    pub pos: Vec<[f32; 3]>,
    pub rad: Vec<[f32; 4]>,
    pub alb: Vec<[f32; 4]>,
    pub idx: Vec<u32>,
}

/// Longest edge (Quake units) of a ray tracer triangle: the light map is
/// sampled at the vertices, so big faces are split to keep its detail.
const RT_MAX_EDGE: f32 = 48.0;
const RT_MAX_TRIS: usize = 1_500_000;

fn color_shift_f(c: [f32; 3]) -> [f32; 3] {
    let mut l = [c[0] * 2.0, c[1] * 2.0, c[2] * 2.0];
    let m = l[0].max(l[1]).max(l[2]);
    if m > 1.0 {
        l.iter_mut().for_each(|x| *x /= m);
    }
    l
}

fn lightmap_textures(bsp: &Bsp) -> Vec<Gd<ImageTexture>> {
    let n = bsp.lightmaps.len() / (128 * 128 * 3);
    (0..n)
        .filter_map(|i| {
            let bytes = &bsp.lightmaps[i * 128 * 128 * 3..(i + 1) * 128 * 128 * 3];
            let data = PackedByteArray::from(bytes);
            let img = Image::create_from_data(128, 128, false, ImageFormat::RGB8, &data)?;
            ImageTexture::create_from_image(&img)
        })
        .collect()
}

#[derive(Clone, Copy, Default)]
struct V {
    xyz: [f32; 3],
    st: [f32; 2],
    lm: [f32; 2],
    n: [f32; 3],
    c: [f32; 4],
    /// autosprite / autosprite2 data (CUSTOM1, CUSTOM2), Godot space
    a1: [f32; 4],
    a2: [f32; 4],
}
fn v_of(d: &DrawVert) -> V {
    V { xyz: d.xyz, st: d.st, lm: d.lightmap, n: d.normal, c: d.color.map(|x| x as f32 / 255.0), a1: [0.0; 4], a2: [0.0; 4] }
}
fn lerp(a: &V, b: &V, t: f32) -> V {
    let l = |x: f32, y: f32| x + (y - x) * t;
    V {
        xyz: [l(a.xyz[0], b.xyz[0]), l(a.xyz[1], b.xyz[1]), l(a.xyz[2], b.xyz[2])],
        st: [l(a.st[0], b.st[0]), l(a.st[1], b.st[1])],
        lm: [l(a.lm[0], b.lm[0]), l(a.lm[1], b.lm[1])],
        n: [l(a.n[0], b.n[0]), l(a.n[1], b.n[1]), l(a.n[2], b.n[2])],
        c: [l(a.c[0], b.c[0]), l(a.c[1], b.c[1]), l(a.c[2], b.c[2]), l(a.c[3], b.c[3])],
        a1: [0.0; 4],
        a2: [0.0; 4],
    }
}
impl trem_sim::kernel::marks::GridVert for V {
    fn xyz(&self) -> [f32; 3] {
        self.xyz
    }
    /// tr_curve.c `LerpDrawVert` (the byte colours as floats)
    fn lerp_half(a: &V, b: &V) -> V {
        lerp(a, b, 0.5)
    }
    fn set_normal(&mut self, n: [f32; 3]) {
        self.n = n;
    }
}

/// A patch as the renderer draws it: tr_curve.c `R_SubdividePatchToGrid`
/// (r_subdivisions 4, the grid the mark code clips against too) and
/// tr_surface.c `RB_SurfaceGrid`'s triangles at full detail.
fn tessellate(ctrl: &[V], w: usize, h: usize, out_v: &mut Vec<V>, out_i: &mut Vec<u32>) {
    let (gw, gh, verts) = trem_sim::kernel::marks::subdivide_patch_to_grid(w, h, ctrl, trem_sim::kernel::marks::R_SUBDIVISIONS);
    if gw < 2 || gh < 2 {
        return;
    }
    let base = out_v.len() as u32;
    out_v.extend_from_slice(&verts);
    let row = gw as u32;
    for i in 0..gh as u32 - 1 {
        for j in 0..gw as u32 - 1 {
            // v1 = i*width + j + 1, v2 = v1 - 1, v3 = v2 + width, v4 = v3 + 1: (v2 v3 v1) (v1 v3 v4)
            let v2 = base + i * row + j;
            let (v1, v3) = (v2 + 1, v2 + row);
            out_i.extend_from_slice(&[v2, v3, v1, v1, v3, v3 + 1]);
        }
    }
}

/// Mesh builder for one BSP: lightmaps, shader-script images and the three
/// world shaders are made once and shared by every model drawn from it.
/// A fog brush (tr_bsp.c R_LoadFogs): its box in Quake units and the
/// shader's `fogparms` colour and distance to opaque.
#[derive(Clone, Debug, PartialEq)]
pub struct FogBox {
    pub mins: [f32; 3],
    pub maxs: [f32; 3],
    pub color: [f32; 3],
    pub dist: f32,
}

/// The fogs lump (dfog_t: shader[64], brushNum, visibleSide) as boxes: the
/// first six sides of a fog brush are its axial planes, -x +x -y +y -z +z.
pub fn fog_boxes(bsp: &Bsp, scripts: &HashMap<String, crate::assets::ShaderInfo>) -> Vec<FogBox> {
    let Some(raw) = bsp.raw.get(&trem_sim::kernel::bsp::LUMP_FOGS) else { return Vec::new() };
    let mut out = Vec::new();
    for rec in raw.chunks_exact(72) {
        let name: String = rec[..64].iter().take_while(|&&c| c != 0).map(|&c| c as char).collect();
        let brush = i32::from_le_bytes([rec[64], rec[65], rec[66], rec[67]]);
        let Some(b) = usize::try_from(brush).ok().and_then(|i| bsp.brushes.get(i)) else { continue };
        if b.num_sides < 6 {
            continue;
        }
        let dist = |k: i32| -> Option<f32> {
            let side = bsp.brush_sides.get((b.first_side + k) as usize)?;
            bsp.planes.get(side.plane as usize).map(|p| p[3])
        };
        let (Some(x0), Some(x1), Some(y0), Some(y1), Some(z0), Some(z1)) = (dist(0), dist(1), dist(2), dist(3), dist(4), dist(5)) else { continue };
        let Some((color, d)) = scripts.get(&name.to_lowercase()).and_then(|i| i.fog) else { continue };
        if d <= 0.0 {
            continue;
        }
        out.push(FogBox { mins: [-x0, -y0, -z0], maxs: [x1, y1, z1], color, dist: d });
    }
    out
}

pub struct BspRender {
    script_images: HashMap<String, crate::assets::ShaderInfo>,
    lightmaps: Vec<Gd<ImageTexture>>,
    shader: Gd<Shader>,
    /// the full shader scripts (q3shader.rs), the stage shaders by blend,
    /// and the stage images
    q3: HashMap<String, crate::q3shader::Q3Shader>,
    stage_shaders: HashMap<(crate::q3shader::Variant, bool, bool), Gd<Shader>>,
    images: HashMap<String, Option<Gd<ImageTexture>>>,
    anims: HashMap<Vec<String>, Option<Gd<Texture2DArray>>>,
    white: Gd<ImageTexture>,
    tex_cache: HashMap<i32, Gd<ImageTexture>>,
    /// per shader: the generated normal map and the mean linear albedo (gfx.rs)
    derived: HashMap<i32, (Option<Gd<ImageTexture>>, [f32; 3])>,
    /// per shader: the 4K pack's albedo (hdtex.rs), sampled in the enhanced mode
    hd: HashMap<i32, Option<Gd<ImageTexture>>>,
    /// the light grid, for the enhanced mode's bumped baked light (CUSTOM0)
    grid: Option<LightGrid>,
    /// the world's opaque triangles for the ray tracer (model 0)
    pub rt: RtGeometry,
    /// the same, unsplit, for the acoustic simulation (fmodaudio.rs)
    pub acoustic: crate::fmodaudio::AcousticMesh,
    /// the map's fog brushes (enhanced mode: volumetric fog volumes)
    pub fogs: Vec<FogBox>,
}

impl BspRender {
    pub fn new(bsp: &Bsp, pk3: &mut Pk3Set) -> Self {
        let mk = |code: &str| {
            let mut sh = Shader::new_gd();
            sh.set_code(code);
            sh
        };
        BspRender {
            script_images: pk3.shader_images(),
            lightmaps: lightmap_textures(bsp),
            shader: mk(&world_shader_code()),
            q3: crate::q3shader::load_all(pk3),
            stage_shaders: HashMap::new(),
            images: HashMap::new(),
            anims: HashMap::new(),
            white: flat_texture(Color::from_rgb(1.0, 1.0, 1.0)),
            tex_cache: HashMap::new(),
            derived: HashMap::new(),
            hd: HashMap::new(),
            grid: LightGrid::from_bsp(bsp),
            rt: RtGeometry::default(),
            acoustic: Default::default(),
            fogs: Vec::new(),
        }
        .with_fogs(bsp)
    }

    fn with_fogs(mut self, bsp: &Bsp) -> Self {
        self.fogs = fog_boxes(bsp, &self.script_images);
        self
    }

    /// The light map at (s, t) of light map `lm` (bilinear), or the vertex
    /// colour without one; gamma space, before the overbright shift.
    fn sample_lightmap(bsp: &Bsp, lm: i32, v: &V) -> [f32; 3] {
        let n = bsp.lightmaps.len() / (128 * 128 * 3);
        if lm < 0 || lm as usize >= n {
            return [v.c[0], v.c[1], v.c[2]];
        }
        let base = lm as usize * 128 * 128 * 3;
        let fx = (v.lm[0] * 128.0 - 0.5).clamp(0.0, 127.0);
        let fy = (v.lm[1] * 128.0 - 0.5).clamp(0.0, 127.0);
        let (x0, y0) = (fx.floor() as usize, fy.floor() as usize);
        let (x1, y1) = ((x0 + 1).min(127), (y0 + 1).min(127));
        let (tx, ty) = (fx - x0 as f32, fy - y0 as f32);
        let px = |x: usize, y: usize, k: usize| bsp.lightmaps[base + (y * 128 + x) * 3 + k] as f32 / 255.0;
        [0, 1, 2].map(|k| {
            let a = px(x0, y0, k) + (px(x1, y0, k) - px(x0, y0, k)) * tx;
            let b = px(x0, y1, k) + (px(x1, y1, k) - px(x0, y1, k)) * tx;
            a + (b - a) * ty
        })
    }

    /// Add one triangle to the ray tracer's geometry, split until its
    /// longest edge is at most RT_MAX_EDGE.
    fn rt_triangle(&mut self, bsp: &Bsp, lm: i32, alb: [f32; 3], a: V, b: V, c: V, depth: u32) {
        if self.rt.idx.len() / 3 >= RT_MAX_TRIS {
            return;
        }
        let d = |p: &V, q: &V| ((p.xyz[0] - q.xyz[0]).powi(2) + (p.xyz[1] - q.xyz[1]).powi(2) + (p.xyz[2] - q.xyz[2]).powi(2)).sqrt();
        let (ab, bc, ca) = (d(&a, &b), d(&b, &c), d(&c, &a));
        let m = ab.max(bc).max(ca);
        if m > RT_MAX_EDGE && depth < 8 {
            if m == ab {
                let mid = lerp(&a, &b, 0.5);
                self.rt_triangle(bsp, lm, alb, a, mid, c, depth + 1);
                self.rt_triangle(bsp, lm, alb, mid, b, c, depth + 1);
            } else if m == bc {
                let mid = lerp(&b, &c, 0.5);
                self.rt_triangle(bsp, lm, alb, a, b, mid, depth + 1);
                self.rt_triangle(bsp, lm, alb, a, mid, c, depth + 1);
            } else {
                let mid = lerp(&c, &a, 0.5);
                self.rt_triangle(bsp, lm, alb, a, b, mid, depth + 1);
                self.rt_triangle(bsp, lm, alb, mid, b, c, depth + 1);
            }
            return;
        }
        for v in [a, b, c] {
            let l = color_shift_f(Self::sample_lightmap(bsp, lm, &v));
            let baked = l.map(|x| (x * 2.0).powf(crate::gfx::R_GAMMA_EXP));
            let g = q2g(&v.xyz);
            self.rt.idx.push(self.rt.pos.len() as u32);
            self.rt.pos.push([g.x, g.y, g.z]);
            self.rt.rad.push([alb[0] * baked[0], alb[1] * baked[1], alb[2] * baked[2], 1.0]);
            self.rt.alb.push([alb[0], alb[1], alb[2], 1.0]);
        }
    }

    /// One MeshInstance3D per (shader, lightmap) batch of inline model `m`
    /// (0 = the world), in the model's own coordinates. Each batch draws its
    /// shader's stages as the renderer does: a lightmapped texture pair (or a
    /// single opaque stage) is the base pass, lit in the enhanced mode; every
    /// other stage is a pass of its own over it (next_pass), with its blend,
    /// rgbGen / alphaGen, tcGen / tcMods and animMap; deformVertexes move
    /// every pass alike.
    pub fn model(&mut self, bsp: &Bsp, pk3: &mut Pk3Set, m: usize) -> Vec<Gd<MeshInstance3D>> {
        let Some(model) = bsp.models.get(m).copied() else { return Vec::new() };
        let mut batches: HashMap<(i32, i32), (Vec<V>, Vec<u32>)> = HashMap::new();
        for si in model.first_surface..model.first_surface + model.num_surfaces {
            let Some(s) = bsp.surfaces.get(si as usize) else { continue };
            let Some(sh) = bsp.shaders.get(s.shader as usize) else { continue };
            if sh.surface_flags & (SURF_NODRAW | SURF_SKY) != 0 {
                continue;
            }
            let sprite = self.q3.get(&sh.name.to_lowercase()).and_then(|q| {
                q.deforms.iter().find_map(|d| match d {
                    crate::q3shader::Deform::AutoSprite => Some(1),
                    crate::q3shader::Deform::AutoSprite2 => Some(2),
                    _ => None,
                })
            });
            let verts = &bsp.verts[s.first_vert as usize..(s.first_vert + s.num_verts) as usize];
            let (bv, bi) = batches.entry((s.shader, s.lightmap_num)).or_default();
            match s.surface_type {
                MST_PLANAR | MST_TRIANGLE_SOUP => {
                    let base = bv.len() as u32;
                    let mut vs: Vec<V> = verts.iter().map(v_of).collect();
                    let idx: Vec<u32> = (0..s.num_indexes).map(|k| bsp.indexes[(s.first_index + k) as usize] as u32).collect();
                    match sprite {
                        Some(1) => autosprite(&mut vs),
                        Some(_) => autosprite2(&mut vs, &idx),
                        None => {}
                    }
                    bv.extend(vs);
                    bi.extend(idx.iter().map(|i| base + i));
                }
                MST_PATCH => {
                    let ctrl: Vec<V> = verts.iter().map(v_of).collect();
                    tessellate(&ctrl, s.patch_width as usize, s.patch_height as usize, bv, bi);
                }
                _ => {}
            }
        }
        let mut keys: Vec<_> = batches.keys().copied().collect();
        keys.sort();
        let mut out = Vec::new();
        for key in keys {
            let (vs, is) = &batches[&key];
            if is.is_empty() {
                continue;
            }
            let (shader_num, lm) = key;
            let name = bsp.shaders[shader_num as usize].name.clone();
            let lm_ok = lm >= 0 && (lm as usize) < self.lightmaps.len();
            let q3 = self.q3.get(&name.to_lowercase()).cloned();
            let Some(plan) = plan_passes(&name, q3.as_ref(), lm_ok) else { continue };
            let sprite = plan.deforms.iter().any(|d| matches!(d, crate::q3shader::Deform::AutoSprite | crate::q3shader::Deform::AutoSprite2));
            let mut first: Option<Gd<ShaderMaterial>> = None;
            let mut opaque = false;
            let mut mean = [0.3f32; 3];
            let mut nrm: Option<Gd<ImageTexture>> = None;
            if let Some(b) = plan.base.as_ref() {
                opaque = true;
                let mat = self.base_material(bsp, pk3, shader_num, lm, b, &plan);
                if let Some((n, mn)) = self.derived.get(&shader_num).cloned() {
                    nrm = n;
                    mean = mn;
                }
                first = Some(mat);
            }
            // the stages drawn as passes of their own
            let mut passes: Vec<Gd<ShaderMaterial>> = Vec::new();
            for (i, st) in plan.stages.iter().enumerate() {
                let on_top = first.is_some() || i > 0;
                let Some(mat) = self.stage_material(pk3, st, lm, &plan, on_top, i) else { continue };
                if first.is_none() && passes.is_empty() {
                    opaque = crate::q3shader::blend_variant(st.blend.as_ref()).0 == crate::q3shader::Variant::Opaque;
                    if let crate::q3shader::Source::Image(f, _) = &st.source {
                        if let Some(img) = self.image(pk3, &f[0]).and_then(|t| t.get_image()) {
                            mean = crate::gfx::derive_maps(&img, false).mean_linear;
                        }
                    }
                }
                passes.push(mat);
            }
            let mut chain: Vec<Gd<ShaderMaterial>> = first.into_iter().chain(passes).collect();
            if chain.is_empty() {
                continue;
            }
            for i in (1..chain.len()).rev() {
                let next = chain[i].clone();
                chain[i - 1].set_next_pass(&next);
            }
            // the ray tracer and the acoustics see the world's opaque surfaces
            if m == 0 && opaque && !sprite && plan.deforms.is_empty() {
                let sh = &bsp.shaders[shader_num as usize];
                let mat = crate::fmodaudio::material_of(&sh.name, sh.surface_flags);
                let base = self.acoustic.verts.len() as i32;
                for v in vs {
                    let g = q2g(&v.xyz);
                    self.acoustic.verts.push([g.x, g.y, g.z]);
                }
                for t in is.chunks_exact(3) {
                    self.acoustic.tris.push([base + t[0] as i32, base + t[1] as i32, base + t[2] as i32]);
                    self.acoustic.mats.push(mat);
                }
                for t in is.chunks_exact(3) {
                    let (a, b, c) = (vs[t[0] as usize], vs[t[1] as usize], vs[t[2] as usize]);
                    self.rt_triangle(bsp, lm, mean, a, b, c, 0);
                }
            }
            let mut st = SurfaceTool::new_gd();
            st.begin(PrimitiveType::TRIANGLES);
            st.set_custom_format(0, godot::classes::surface_tool::CustomFormat::RGBA_FLOAT);
            if sprite {
                st.set_custom_format(1, godot::classes::surface_tool::CustomFormat::RGBA_FLOAT);
                st.set_custom_format(2, godot::classes::surface_tool::CustomFormat::RGBA_FLOAT);
            }
            for v in vs {
                st.set_normal(q2g_dir(&v.n));
                st.set_uv(Vector2::new(v.st[0], v.st[1]));
                st.set_uv2(Vector2::new(v.lm[0], v.lm[1]));
                st.set_color(Color::from_rgba(v.c[0], v.c[1], v.c[2], v.c[3]));
                // the light grid's dominant direction (Godot space) and the
                // directed share of its light, for the bumped baked light
                let g = match self.grid.as_ref() {
                    Some(g) => {
                        let (amb, dir, d) = g.sample(&v.xyz);
                        let la = amb[0] * 0.3 + amb[1] * 0.59 + amb[2] * 0.11;
                        let ld = dir[0] * 0.3 + dir[1] * 0.59 + dir[2] * 0.11;
                        let gd = q2g_dir(&d);
                        Color::from_rgba(gd.x, gd.y, gd.z, if la + ld > 0.0 { ld / (la + ld) } else { 0.0 })
                    }
                    None => Color::from_rgba(0.0, 1.0, 0.0, 0.0),
                };
                st.set_custom(0, g);
                if sprite {
                    st.set_custom(1, Color::from_rgba(v.a1[0], v.a1[1], v.a1[2], v.a1[3]));
                    st.set_custom(2, Color::from_rgba(v.a2[0], v.a2[1], v.a2[2], v.a2[3]));
                }
                st.add_vertex(q2g(&v.xyz));
            }
            // Q3 winding is clockwise seen from the front, Godot's too: keep order.
            for &i in is {
                st.add_index(i as i32);
            }
            if nrm.is_some() {
                st.generate_tangents();
            }
            let Some(mesh) = st.commit() else { continue };
            let mut mi = MeshInstance3D::new_alloc();
            mi.set_mesh(&mesh);
            mi.set_material_override(&chain[0]);
            mi.set_name(&format!("bsp{m}_{shader_num}_{lm}"));
            if !plan.deforms.is_empty() {
                // moved vertices: keep the mesh drawn when its rest pose leaves the view
                mi.set_extra_cull_margin(4.0);
            }
            // the map takes the shadow sun's shadows and casts none (its
            // light is baked: from inside a room the sun would black it out)
            mi.set_layer_mask(1 | crate::gfx::SHADOW_RECEIVER_LAYER);
            mi.set_cast_shadows_setting(godot::classes::geometry_instance_3d::ShadowCastingSetting::OFF);
            out.push(mi);
        }
        out
    }

    /// The map's shader scripts (q3shader.rs).
    pub fn shaders(&self) -> &HashMap<String, crate::q3shader::Q3Shader> {
        &self.q3
    }

    /// A stage image (cached; None when the data has no such image).
    fn image(&mut self, pk3: &mut Pk3Set, path: &str) -> Option<Gd<ImageTexture>> {
        let key = path.to_lowercase();
        if let Some(t) = self.images.get(&key) {
            return t.clone();
        }
        let t = load_image(pk3, path).and_then(|i| ImageTexture::create_from_image(&i));
        self.images.insert(key, t.clone());
        t
    }

    /// An animMap's frames as one texture array (frames resized to the first's size).
    fn anim(&mut self, pk3: &mut Pk3Set, frames: &[String]) -> Option<Gd<Texture2DArray>> {
        let key: Vec<String> = frames.iter().map(|f| f.to_lowercase()).collect();
        if let Some(t) = self.anims.get(&key) {
            return t.clone();
        }
        let mut imgs: Array<Gd<Image>> = Array::new();
        let mut size = None;
        for f in frames {
            let Some(mut img) = load_image(pk3, f) else { continue };
            img.clear_mipmaps();
            img.convert(ImageFormat::RGBA8);
            let (w, h) = *size.get_or_insert((img.get_width(), img.get_height()));
            if img.get_width() != w || img.get_height() != h {
                img.resize(w, h);
            }
            img.generate_mipmaps();
            imgs.push(&img);
        }
        let t = if imgs.is_empty() {
            None
        } else {
            let mut arr = Texture2DArray::new_gd();
            (arr.create_from_images(&imgs) == godot::global::Error::OK).then_some(arr)
        };
        self.anims.insert(key, t.clone());
        t
    }

    fn stage_shader(&mut self, v: crate::q3shader::Variant, depth_write: bool, offset: bool) -> Gd<Shader> {
        self.stage_shaders
            .entry((v, depth_write, offset))
            .or_insert_with(|| {
                let mut sh = Shader::new_gd();
                sh.set_code(&crate::q3shader::stage_shader_code(v, depth_write, offset));
                sh
            })
            .clone()
    }

    /// The pass of one stage. `on_top`: drawn over an earlier pass of the
    /// same surface (depthFunc equal: pulled a hair towards the camera).
    fn stage_material(&mut self, pk3: &mut Pk3Set, st: &crate::q3shader::Stage, lm: i32, plan: &Plan, on_top: bool, index: usize) -> Option<Gd<ShaderMaterial>> {
        use crate::q3shader::{blend_variant, Source, Variant};
        let (variant, out_kind) = blend_variant(st.blend.as_ref());
        let depth_write = variant != Variant::Opaque && st.depth_write;
        let shader = self.stage_shader(variant, depth_write, plan.polygon_offset);
        let mut mat = ShaderMaterial::new_gd();
        mat.set_shader(&shader);
        match &st.source {
            Source::Image(frames, freq) if frames.len() > 1 => {
                let t = self.anim(pk3, frames)?;
                mat.set_shader_parameter("frames", &t.to_variant());
                mat.set_shader_parameter("src_kind", &2.to_variant());
                mat.set_shader_parameter("nframes", &(frames.len() as i32).to_variant());
                mat.set_shader_parameter("anim_freq", &freq.to_variant());
            }
            Source::Image(frames, _) => {
                let t = match self.image(pk3, &frames[0]) {
                    Some(t) => t,
                    // the first pass of a surface draws even without its image (the default image)
                    None if !on_top => flat_texture(Color::from_rgb(0.55, 0.55, 0.6)),
                    None => return None,
                };
                mat.set_shader_parameter(if st.clamp { "tex_c" } else { "tex" }, &t.to_variant());
                mat.set_shader_parameter("src_kind", &(if st.clamp { 1 } else { 0 }).to_variant());
                // enhanced: the 4K pack's image, as the base pass has it
                if let Some(hd) = crate::hdtex::get(&crate::hdtex::key_of(&frames[0])) {
                    mat.set_shader_parameter(if st.clamp { "tex_hd_c" } else { "tex_hd" }, &hd.albedo.to_variant());
                    mat.set_shader_parameter("has_hd", &true.to_variant());
                }
            }
            Source::Lightmap => {
                let t = if lm >= 0 && (lm as usize) < self.lightmaps.len() { self.lightmaps[lm as usize].clone() } else { self.white.clone() };
                mat.set_shader_parameter("lm", &t.to_variant());
                mat.set_shader_parameter("src_kind", &3.to_variant());
            }
            Source::White => mat.set_shader_parameter("src_kind", &4.to_variant()),
        }
        crate::q3shader::set_stage(&mut mat, st, out_kind);
        crate::q3shader::set_deforms(&mut mat, &plan.deforms);
        if index < plan.masked {
            if let Some(m) = plan.base.as_ref().and_then(|b| self.image(pk3, &b.image)) {
                mat.set_shader_parameter("mask_tex", &m.to_variant());
                mat.set_shader_parameter("has_mask", &true.to_variant());
            }
        }
        if let (Some(k), true) = (plan.lm_mod, variant != Variant::Mul) {
            let t = if lm >= 0 && (lm as usize) < self.lightmaps.len() { self.lightmaps[lm as usize].clone() } else { self.white.clone() };
            mat.set_shader_parameter("lm", &t.to_variant());
            mat.set_shader_parameter("lm_mod", &true.to_variant());
            mat.set_shader_parameter("lm_mod_k", &Vector3::new(k[0], k[1], k[2]).to_variant());
        }
        let mut pull = plan.pull;
        // over an earlier pass, or blended over the opaque world: a hair
        // nearer, so that a coplanar surface wins its depth test as with the
        // C's GL_LEQUAL (atcs's force fields lie in their frames' planes)
        if on_top || variant != Variant::Opaque {
            pull *= 0.99995;
        }
        mat.set_shader_parameter("q3_pull", &pull.to_variant());
        // the shader's sort, then the stage order (Q3 draws by sort, not by depth)
        mat.set_render_priority(((plan.sort * 6.0) as i32 + index as i32).clamp(-120, 120));
        Some(mat)
    }

    /// The base pass: a texture under its light map (or vertex colours, or
    /// none), the surface the enhanced mode lights.
    fn base_material(&mut self, bsp: &Bsp, pk3: &mut Pk3Set, shader_num: i32, lm: i32, b: &BasePass, plan: &Plan) -> Gd<ShaderMaterial> {
        let opaque = true;
        let derived = &mut self.derived;
        let hd_cache = &mut self.hd;
        let tex = self
            .tex_cache
            .entry(shader_num)
            .or_insert_with(|| {
                let name = bsp.shaders[shader_num as usize].name.clone();
                let img = load_image(pk3, &b.image).or_else(|| load_image(pk3, &name));
                // the 4K pack's texture (hdtex.rs): its normal map and
                // mean colour come with it; else the generated maps (gfx.rs)
                let hd = crate::hdtex::get(&crate::hdtex::key_of(&b.image)).or_else(|| crate::hdtex::get(&crate::hdtex::key_of(&name)));
                let d = match (hd.as_ref(), img.as_ref()) {
                    (Some(h), _) => crate::gfx::DerivedMaps { normal: if opaque { h.normal.clone() } else { None }, mean_linear: h.mean },
                    (None, Some(i)) => crate::gfx::derive_maps(i, opaque),
                    (None, None) => crate::gfx::DerivedMaps { normal: None, mean_linear: [0.3, 0.3, 0.32] },
                };
                derived.insert(shader_num, (d.normal, d.mean_linear));
                hd_cache.insert(shader_num, hd.map(|h| h.albedo));
                match img.and_then(|i| ImageTexture::create_from_image(&i)) {
                    Some(t) => t,
                    None => flat_texture(Color::from_rgb(0.55, 0.55, 0.6)),
                }
            })
            .clone();
        let nrm = self.derived.get(&shader_num).and_then(|d| d.0.clone());
        let mut mat = ShaderMaterial::new_gd();
        mat.set_shader(&self.shader);
        mat.set_shader_parameter("tex", &tex.to_variant());
        mat.set_shader_parameter("alpha_func", &(b.alpha_func as i32).to_variant());
        mat.set_shader_parameter("tint", &Color::from_rgba(b.tint[0], b.tint[1], b.tint[2], 1.0).to_variant());
        if let Some(Some(hd)) = self.hd.get(&shader_num) {
            mat.set_shader_parameter("tex_hd", &hd.to_variant());
            mat.set_shader_parameter("has_hd", &true.to_variant());
        }
        let use_lm = b.light_src == 0 && lm >= 0 && (lm as usize) < self.lightmaps.len();
        mat.set_shader_parameter("light_src", &b.light_src.to_variant());
        mat.set_shader_parameter("use_lm", &use_lm.to_variant());
        mat.set_shader_parameter("lm", &(if use_lm { self.lightmaps[lm as usize].clone() } else { self.white.clone() }).to_variant());
        let class = crate::gfx::surface_class(&bsp.shaders[shader_num as usize].name);
        let (rough, spec, bump) = class.base;
        mat.set_shader_parameter("rough", &rough.to_variant());
        mat.set_shader_parameter("spec", &spec.to_variant());
        mat.set_shader_parameter("bump", &bump.to_variant());
        mat.set_shader_parameter("metal", &class.metal.to_variant());
        mat.set_shader_parameter("wet", &class.wet.to_variant());
        mat.set_shader_parameter("detail_k", &class.detail.to_variant());
        mat.set_shader_parameter("detail", &crate::gfx::detail_normal().to_variant());
        if let Some(n) = nrm.as_ref() {
            mat.set_shader_parameter("nrm", &n.to_variant());
            mat.set_shader_parameter("has_nrm", &true.to_variant());
        }
        crate::q3shader::set_deforms(&mut mat, &plan.deforms);
        mat.set_shader_parameter("q3_pull", &plan.pull.to_variant());
        mat
    }
}

/// The base pass of a surface: its texture, where its light comes from
/// (0 light map, 1 vertex colours, 2 none), the stages' constant colour
/// factor (fb units) and the alphaFunc.
#[derive(Clone, Debug, PartialEq)]
pub struct BasePass {
    pub image: String,
    pub light_src: i32,
    pub tint: [f32; 3],
    pub alpha_func: u8,
}

/// How a surface's shader is drawn.
#[derive(Clone, Debug, PartialEq)]
pub struct Plan {
    pub base: Option<BasePass>,
    /// the stages drawn as passes of their own, in order
    pub stages: Vec<crate::q3shader::Stage>,
    pub deforms: Vec<crate::q3shader::Deform>,
    /// view-space pull of every pass (polygonOffset)
    pub pull: f32,
    /// polygonOffset: also offset by the depth slope (glPolygonOffset), for
    /// decals on curved patches whose facets cross the wall's
    pub polygon_offset: bool,
    pub sort: f32,
    /// a last stage `$lightmap` filtered onto everything before it, folded
    /// into the earlier passes (exact: the multiply distributes over their
    /// blends) with its constant colour; the base pass then takes the light map
    pub lm_mod: Option<[f32; 3]>,
    /// the first `masked` stages are cut by the base image's alpha: a later
    /// stage repeated the base image over them with blendFunc blend (atcs's
    /// eq2_bounce: the fan shows through the pad's hole only). Folded in,
    /// exactly, so that the pad itself is the (enhanced) base pass
    pub masked: usize,
}

/// Splits a shader into the base pass and the stage passes. None: nothing
/// to draw (nodraw, or a script without stages: a fog volume's sides).
pub fn plan_passes(name: &str, q3: Option<&crate::q3shader::Q3Shader>, lm_ok: bool) -> Option<Plan> {
    use crate::q3shader::{const_rgb, AlphaGen, RgbGen, Source, TcGen};
    let Some(sh) = q3 else {
        // no script: the implicit shader (texture x light map, or the vertex colours)
        return Some(Plan { base: Some(BasePass { image: name.to_string(), light_src: if lm_ok { 0 } else { 1 }, tint: [1.0; 3], alpha_func: 0 }), stages: Vec::new(), deforms: Vec::new(), pull: 1.0, polygon_offset: false, sort: 3.0, lm_mod: None, masked: 0 });
    };
    if sh.nodraw || sh.stages.is_empty() {
        return None;
    }
    let s = &sh.stages;
    let image_of = |st: &crate::q3shader::Stage| match &st.source {
        Source::Image(f, _) if f.len() == 1 => Some(f[0].clone()),
        _ => None,
    };
    // a stage the base pass can draw: one image, no tcMod, constant colour
    let plain = |st: &crate::q3shader::Stage| st.tc_mods.is_empty() && st.alpha_gen == AlphaGen::Identity && const_rgb(&st.rgb_gen).is_some();
    let mul = |a: [f32; 3], b: [f32; 3]| [a[0] * b[0], a[1] * b[1], a[2] * b[2]];
    let lm_src = if lm_ok { 0 } else { 2 };
    let mut base = None;
    let mut rest = 0;
    if s.len() >= 2 && s[0].is_lightmap() && s[0].blend.is_none() && plain(&s[0]) && s[1].is_filter() && plain(&s[1]) && s[1].tc_gen == TcGen::Base {
        if let Some(img) = image_of(&s[1]) {
            // light map, then the texture filtered onto it
            base = Some(BasePass { image: img, light_src: lm_src, tint: mul(const_rgb(&s[0].rgb_gen).unwrap(), const_rgb(&s[1].rgb_gen).unwrap()), alpha_func: s[1].alpha_func.max(s[0].alpha_func) });
            rest = 2;
        }
    } else if s.len() >= 2 && s[0].blend.is_none() && plain(&s[0]) && s[0].tc_gen == TcGen::Base && s[1].is_lightmap() && s[1].is_filter() && plain(&s[1]) {
        if let Some(img) = image_of(&s[0]) {
            // the texture, then the light map filtered onto it (grates: the texture's alphaFunc)
            base = Some(BasePass { image: img, light_src: lm_src, tint: mul(const_rgb(&s[0].rgb_gen).unwrap(), const_rgb(&s[1].rgb_gen).unwrap()), alpha_func: s[0].alpha_func });
            rest = 2;
        }
    }
    if base.is_none() && s[0].blend.is_none() && s[0].tc_gen == TcGen::Base && s[0].tc_mods.is_empty() && s[0].alpha_gen == AlphaGen::Identity {
        if let Some(img) = image_of(&s[0]) {
            let lit = match s[0].rgb_gen {
                RgbGen::Vertex => Some((1, [0.5; 3])),
                RgbGen::ExactVertex => Some((1, [1.0; 3])),
                ref g => const_rgb(g).map(|c| (2, c)),
            };
            if let Some((light_src, tint)) = lit {
                base = Some(BasePass { image: img, light_src, tint, alpha_func: s[0].alpha_func });
                rest = 1;
            }
        }
    }
    let mut stages = s[rest..].to_vec();
    let mut lm_mod = None;
    let foldable = |b: &Option<BasePass>| b.as_ref().map_or(true, |b| b.light_src == 2);
    if let Some(last) = stages.last() {
        if last.is_lightmap() && last.is_filter() && plain(last) && (base.is_some() || stages.len() >= 2) && foldable(&base) && !stages[..stages.len() - 1].iter().any(|st| st.is_filter() || st.is_lightmap()) {
            let k = const_rgb(&last.rgb_gen).unwrap();
            stages.pop();
            match base.as_mut() {
                Some(b) => {
                    b.light_src = lm_src;
                    b.tint = mul(b.tint, k);
                    lm_mod = (!stages.is_empty()).then_some(k);
                }
                None => lm_mod = Some(k),
            }
        }
    }
    // base, A, B, base image again with blendFunc blend: the result is the
    // base with A and B drawn through the image's transparent texels only
    let mut masked = 0;
    if let Some(b) = base.as_ref() {
        let lm_k = lm_mod.unwrap_or([1.0; 3]);
        let same_light = (b.light_src == lm_src && lm_mod.is_some()) || (b.light_src == 2 && lm_mod.is_none());
        let repeat = stages.iter().position(|st| {
            image_of(st).is_some_and(|i| i.eq_ignore_ascii_case(&b.image))
                && crate::q3shader::blend_variant(st.blend.as_ref()) == (crate::q3shader::Variant::Mix, 0)
                && st.tc_gen == TcGen::Base
                && plain(st)
                && st.alpha_func == 0
                && const_rgb(&st.rgb_gen).is_some_and(|c| mul(c, lm_k).iter().zip(b.tint.iter()).all(|(x, y)| (x - y).abs() < 1e-4))
        });
        if let (Some(j), true) = (repeat, same_light) {
            if j > 0 && b.alpha_func == 0 {
                stages.remove(j);
                masked = j;
            }
        }
    }
    Some(Plan { base, stages, deforms: sh.deforms.iter().copied().filter(|d| *d != crate::q3shader::Deform::Normal).collect(), pull: if sh.polygon_offset { 0.9995 } else { 1.0 }, polygon_offset: sh.polygon_offset, sort: sh.effective_sort(), lm_mod, masked })
}

/// deformVertexes autosprite (tr_shade_calc.c AutoSprite): each quad's
/// centre and radius, and the corner each vertex becomes.
fn autosprite(vs: &mut [V]) {
    for q in vs.chunks_exact_mut(4) {
        let g: Vec<Vector3> = q.iter().map(|v| q2g(&v.xyz)).collect();
        let mid = (g[0] + g[1] + g[2] + g[3]) / 4.0;
        let radius = (g[0] - mid).length() * 0.707;
        for (k, v) in q.iter_mut().enumerate() {
            v.a1 = [mid.x, mid.y, mid.z, radius];
            v.a2 = [0.0, 0.0, 0.0, k as f32];
        }
    }
}

/// deformVertexes autosprite2 (Autosprite2): the midpoints of each quad's
/// two short edges (the long axis) and each vertex's signed half width.
fn autosprite2(vs: &mut [V], idx: &[u32]) {
    const EDGES: [[usize; 2]; 6] = [[0, 1], [0, 2], [0, 3], [1, 2], [1, 3], [2, 3]];
    for qi in 0..vs.len() / 4 {
        let q: Vec<[f32; 3]> = (0..4).map(|k| vs[qi * 4 + k].xyz).collect();
        let d2 = |a: &[f32; 3], b: &[f32; 3]| (a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2);
        let (mut nums, mut lengths) = ([0usize; 2], [f32::MAX; 2]);
        for (j, e) in EDGES.iter().enumerate() {
            let l = d2(&q[e[0]], &q[e[1]]);
            if l < lengths[0] {
                nums[1] = nums[0];
                lengths[1] = lengths[0];
                nums[0] = j;
                lengths[0] = l;
            } else if l < lengths[1] {
                nums[1] = j;
                lengths[1] = l;
            }
        }
        let mid: Vec<[f32; 3]> = (0..2).map(|j| {
            let e = EDGES[nums[j]];
            [(q[e[0]][0] + q[e[1]][0]) * 0.5, (q[e[0]][1] + q[e[1]][1]) * 0.5, (q[e[0]][2] + q[e[1]][2]) * 0.5]
        }).collect();
        let (m0, m1) = (q2g(&mid[0]), q2g(&mid[1]));
        let quad_idx = idx.get(qi * 6..qi * 6 + 6).unwrap_or(&[]);
        for j in 0..2 {
            let e = EDGES[nums[j]];
            let l = 0.5 * lengths[j].sqrt() * crate::geom::SCALE;
            // the edge's direction in the index list decides the side (as the C does)
            let along = (0..quad_idx.len().saturating_sub(1)).any(|k| quad_idx[k] as usize == qi * 4 + e[0] && quad_idx[k + 1] as usize == qi * 4 + e[1]);
            let sgn = if along { -1.0 } else { 1.0 };
            for (vk, sl) in [(e[0], sgn * l), (e[1], -sgn * l)] {
                let v = &mut vs[qi * 4 + vk];
                v.a1 = [m0.x, m0.y, m0.z, sl];
                v.a2 = [m1.x, m1.y, m1.z, j as f32];
            }
        }
    }
}

/// The 4K pack's six skybox faces (hdtex.rs), when it has all of them.
pub fn sky_textures_hd(bsp: &Bsp, pk3: &mut Pk3Set) -> Option<Vec<Gd<godot::classes::Texture2D>>> {
    let infos = pk3.shader_images();
    let env = bsp.shaders.iter().filter(|s| s.surface_flags & SURF_SKY != 0).find_map(|s| infos.get(&s.name.to_lowercase()).and_then(|i| i.sky.clone()))?;
    ["rt", "lf", "bk", "ft", "up", "dn"].iter().map(|suf| crate::hdtex::get(&crate::hdtex::key_of(&format!("{env}_{suf}"))).map(|h| h.albedo.upcast::<godot::classes::Texture2D>())).collect()
}

/// A map's sky (tr_sky.c): the box of `skyparms <env>` and the cloud
/// layers (the sky shader's stages at `cloudheight`).
pub struct SkyDef {
    /// rt, lf, bk, ft, up, dn
    pub faces: Option<Vec<Gd<ImageTexture>>>,
    pub clouds: Vec<(Gd<ImageTexture>, crate::q3shader::Stage)>,
    pub cloud_height: f32,
}

pub const SKY_CLOUD_LAYERS: usize = 3;

/// The sky of the map's sky shader, None without one (or with nothing to draw).
pub fn sky_def(bsp: &Bsp, pk3: &mut Pk3Set) -> Option<SkyDef> {
    let shaders = crate::q3shader::load_all(pk3);
    let sh = bsp.shaders.iter().filter(|s| s.surface_flags & SURF_SKY != 0).find_map(|s| shaders.get(&s.name.to_lowercase()).filter(|q| q.sky.is_some()))?;
    let parms = sh.sky.clone()?;
    let faces = parms.outer.as_ref().and_then(|env| {
        let mut out = Vec::new();
        for suf in ["rt", "lf", "bk", "ft", "up", "dn"] {
            let img = load_image(pk3, &format!("{env}_{suf}"))?;
            out.push(ImageTexture::create_from_image(&img)?);
        }
        Some(out)
    });
    let mut clouds = Vec::new();
    for st in &sh.stages {
        let crate::q3shader::Source::Image(f, _) = &st.source else { continue };
        if let Some(t) = load_image(pk3, &f[0]).and_then(|i| ImageTexture::create_from_image(&i)) {
            if clouds.len() < SKY_CLOUD_LAYERS {
                clouds.push((t, st.clone()));
            }
        }
    }
    if faces.is_none() && clouds.is_empty() {
        return None;
    }
    Some(SkyDef { faces, clouds, cloud_height: parms.cloud_height })
}

/// The sky shader's uniforms for its cloud layers and the box.
pub fn sky_material(def: &SkyDef, hd: Option<&Vec<Gd<godot::classes::Texture2D>>>) -> Gd<ShaderMaterial> {
    let mut sh = Shader::new_gd();
    sh.set_code(&sky_shader_code());
    let mut mat = ShaderMaterial::new_gd();
    mat.set_shader(&sh);
    if let Some(faces) = def.faces.as_ref() {
        for (name, t) in ["rt", "lf", "bk", "ft", "up", "dn"].iter().zip(faces.iter()) {
            mat.set_shader_parameter(*name, &t.to_variant());
        }
        mat.set_shader_parameter("has_box", &true.to_variant());
        if let Some(hd) = hd.filter(|h| h.len() == 6) {
            for (name, t) in ["rt_hd", "lf_hd", "bk_hd", "ft_hd", "up_hd", "dn_hd"].iter().zip(hd.iter()) {
                mat.set_shader_parameter(*name, &t.to_variant());
            }
            mat.set_shader_parameter("has_hd", &true.to_variant());
        }
    }
    mat.set_shader_parameter("cloud_h", &def.cloud_height.to_variant());
    for (i, (tex, st)) in def.clouds.iter().enumerate() {
        // the stage's uniforms under a per-layer prefix
        let mut tmp = ShaderMaterial::new_gd();
        let (variant, out_kind) = crate::q3shader::blend_variant(st.blend.as_ref());
        crate::q3shader::set_stage(&mut tmp, st, out_kind);
        for u in ["tck", "tca", "tcb", "rgb_kind", "rgb_const", "rgb_wave", "rgb_func", "a_kind", "a_const", "a_wave", "a_func", "out_kind"] {
            mat.set_shader_parameter(&format!("c{i}_{u}"), &tmp.get_shader_parameter(u));
        }
        let v = match variant {
            crate::q3shader::Variant::Opaque => 0,
            crate::q3shader::Variant::Add => 1,
            crate::q3shader::Variant::Mix => 2,
            crate::q3shader::Variant::Premul => 3,
            crate::q3shader::Variant::Mul => 4,
        };
        mat.set_shader_parameter(&format!("c{i}_on"), &true.to_variant());
        mat.set_shader_parameter(&format!("c{i}_variant"), &v.to_variant());
        mat.set_shader_parameter(&format!("c{i}_tex"), &tex.to_variant());
    }
    mat
}

/// Sky shader reproducing tr_sky.c: the box mapping (MakeSkyVec /
/// st_to_vec) drawn at identityLight, the cloud layers over the upper half
/// (R_InitSkyTexCoords' sphere of radius 4096 at cloudheight, their tcMods
/// and blends), and the screen's x2 overbright.
pub fn sky_shader_code() -> String {
    let mut s = String::from(SKY_SHADER_HEAD);
    s.push_str(crate::q3shader::GLSL_COMMON);
    for i in 0..SKY_CLOUD_LAYERS {
        s.push_str(&SKY_LAYER_UNIFORMS.replace("cN_", &format!("c{i}_")));
    }
    s.push_str(SKY_SHADER_FNS);
    s.push_str("void sky() {\n");
    s.push_str(SKY_BOX);
    s.push_str("    if (q.z > 0.0) {\n        vec2 st0 = cloud_st(normalize(q));\n");
    for i in 0..SKY_CLOUD_LAYERS {
        s.push_str(&SKY_LAYER.replace("cN_", &format!("c{i}_")));
    }
    s.push_str("    }\n    COLOR = pow(clamp(fb * 2.0, 0.0, 1.0), vec3(2.2));\n}\n");
    s
}

const SKY_SHADER_HEAD: &str = r#"
shader_type sky;
global uniform float trem_gfx;
uniform bool has_box = false;
uniform sampler2D rt : source_color, filter_linear;
uniform sampler2D lf : source_color, filter_linear;
uniform sampler2D bk : source_color, filter_linear;
uniform sampler2D ft : source_color, filter_linear;
uniform sampler2D up : source_color, filter_linear;
uniform sampler2D dn : source_color, filter_linear;
// the 4K pack's faces (enhanced mode)
uniform bool has_hd = false;
uniform sampler2D rt_hd : source_color, filter_linear_mipmap;
uniform sampler2D lf_hd : source_color, filter_linear_mipmap;
uniform sampler2D bk_hd : source_color, filter_linear_mipmap;
uniform sampler2D ft_hd : source_color, filter_linear_mipmap;
uniform sampler2D up_hd : source_color, filter_linear_mipmap;
uniform sampler2D dn_hd : source_color, filter_linear_mipmap;
uniform float cloud_h = 512.0;
"#;

const SKY_LAYER_UNIFORMS: &str = r#"
uniform bool cN_on = false;
uniform sampler2D cN_tex : filter_linear_mipmap, repeat_enable;
uniform int cN_variant = 0;
uniform ivec4 cN_tck;
uniform mat4 cN_tca;
uniform mat4 cN_tcb;
uniform int cN_rgb_kind = 0;
uniform vec3 cN_rgb_const = vec3(1.0);
uniform vec4 cN_rgb_wave;
uniform int cN_rgb_func = 0;
uniform int cN_a_kind = 0;
uniform float cN_a_const = 1.0;
uniform vec4 cN_a_wave;
uniform int cN_a_func = 0;
uniform int cN_out_kind = 0;
"#;

const SKY_SHADER_FNS: &str = r#"
vec4 face(sampler2D t, float s, float tt) {
    return texture(t, vec2((s + 1.0) * 0.5, 1.0 - (tt + 1.0) * 0.5));
}
// Godot needs one function per sampler setting, so HD faces get their own.
vec4 face_hd(sampler2D t, float s, float tt) {
    return texture(t, vec2((s + 1.0) * 0.5, 1.0 - (tt + 1.0) * 0.5));
}
// R_InitSkyTexCoords: where the view ray meets the cloud sphere (radius
// 4096 + cloudheight around a point 4096 below the eye), as acos angles
vec2 cloud_st(vec3 d) {
    float r = 4096.0;
    float b = d.z * r;
    float p = -b + sqrt(b * b + 2.0 * r * cloud_h + cloud_h * cloud_h);
    vec3 v = normalize(d * p + vec3(0.0, 0.0, r));
    return vec2(acos(v.x), acos(v.y));
}
float layer_rgb_a(int kind, vec3 cst, vec4 w, int f, out vec3 rgb) {
    rgb = vec3(0.5);
    if (kind == 1 || kind == 7) { rgb = vec3(1.0); }
    else if (kind == 2) { rgb = cst; }
    else if (kind == 3) { float g = f == 5 ? q3_wave(5, w, TIME) : q3_wave(f, w, TIME) * 0.5; rgb = vec3(clamp(g, 0.0, 1.0)); }
    else if (kind == 8) { rgb = vec3(0.0); }
    return 1.0;
}
vec3 layer_blend(vec3 dst, vec4 c, int variant, int ok) {
    vec4 o = c;
    if (ok == 1) { o = vec4(c.rgb * c.a, 1.0); }
    else if (ok == 2) { o = vec4(c.rgb, 1.0 - c.a); }
    else if (ok == 3) { o = vec4(c.rgb + vec3(c.a), 1.0); }
    else if (ok == 4) { o = vec4(c.rgb + vec3(1.0), 1.0); }
    else if (ok == 5) { o = vec4(vec3(1.0) - c.rgb, 1.0); }
    else if (ok == 6) { o = vec4(vec3(c.a), 1.0); }
    else if (ok == 7) { o = vec4(vec3(1.0 - c.a), 1.0); }
    else if (ok == 8) { o = vec4(c.rgb * 2.0, 1.0); }
    if (variant == 0) { return o.rgb; }
    if (variant == 1) { return dst + o.rgb; }
    if (variant == 2) { return mix(dst, o.rgb, clamp(o.a, 0.0, 1.0)); }
    if (variant == 3) { return o.rgb + dst * (1.0 - clamp(o.a, 0.0, 1.0)); }
    return dst * o.rgb;
}
"#;

const SKY_BOX: &str = r#"
    vec3 q = vec3(EYEDIR.x, -EYEDIR.z, EYEDIR.y);
    vec3 a = abs(q);
    bool hd = has_hd && trem_gfx > 0.5;
    vec4 c = vec4(0.0);
    if (has_box) {
        if (a.x >= a.y && a.x >= a.z) {
            c = q.x > 0.0 ? (hd ? face_hd(rt_hd, -q.y / a.x, q.z / a.x) : face(rt, -q.y / a.x, q.z / a.x)) : (hd ? face_hd(lf_hd, q.y / a.x, q.z / a.x) : face(lf, q.y / a.x, q.z / a.x));
        } else if (a.y >= a.z) {
            c = q.y > 0.0 ? (hd ? face_hd(bk_hd, q.x / a.y, q.z / a.y) : face(bk, q.x / a.y, q.z / a.y)) : (hd ? face_hd(ft_hd, -q.x / a.y, q.z / a.y) : face(ft, -q.x / a.y, q.z / a.y));
        } else {
            c = q.z > 0.0 ? (hd ? face_hd(up_hd, -q.y / a.z, -q.x / a.z) : face(up, -q.y / a.z, -q.x / a.z)) : (hd ? face_hd(dn_hd, -q.y / a.z, q.x / a.z) : face(dn, -q.y / a.z, q.x / a.z));
        }
    }
    // the box is drawn at tr.identityLight (0.5); the screen doubles it
    vec3 fb = 0.5 * pow(max(c.rgb, vec3(0.0)), vec3(1.0 / 2.2));
"#;

const SKY_LAYER: &str = r#"
        if (cN_on) {
            vec2 st = st0;
            for (int k = 0; k < 4; k++) {
                st = q3_tcmod(cN_tck[k], cN_tca[k], cN_tcb[k], st, vec3(0.0), TIME);
            }
            vec4 t = texture(cN_tex, st);
            vec3 rgb;
            layer_rgb_a(cN_rgb_kind, cN_rgb_const, cN_rgb_wave, cN_rgb_func, rgb);
            float al = 1.0;
            if (cN_a_kind == 1) { al = cN_a_const; }
            else if (cN_a_kind == 2) { al = clamp(q3_wave(cN_a_func, cN_a_wave, TIME), 0.0, 1.0); }
            fb = layer_blend(fb, t * vec4(rgb, al), cN_variant, cN_out_kind);
        }
"#;

/// The map's light grid (tr_bsp.c R_LoadLightGrid + tr_light.c
/// R_SetupEntityLightingGrid): ambient + directed light for models.
pub struct LightGrid {
    origin: [f32; 3],
    size: [f32; 3],
    bounds: [i32; 3],
    data: Vec<u8>,
}

/// `R_ColorShiftLightingBytes` with mapOverBrightBits 2, overBrightBits 1 (x2, hue-preserving clamp).
fn color_shift(c: [u8; 3]) -> [f32; 3] {
    let mut r = [c[0] as f32 * 2.0, c[1] as f32 * 2.0, c[2] as f32 * 2.0];
    let m = r[0].max(r[1]).max(r[2]);
    if m > 255.0 {
        r.iter_mut().for_each(|x| *x *= 255.0 / m);
    }
    r
}

impl LightGrid {
    pub fn from_bsp(bsp: &Bsp) -> Option<LightGrid> {
        let data = bsp.raw.get(&15)?.clone();
        let m = bsp.models.first()?;
        let mut size = [64.0f32, 64.0, 128.0];
        if let Some(ws) = bsp.entity_list().into_iter().find(|e| e.get("classname").map(|c| c == "worldspawn").unwrap_or(false)) {
            if let Some(v) = ws.get("gridsize").and_then(|g| trem_sim::kernel::bsp::parse_vec(g)) {
                size = v;
            }
        }
        let mut origin = [0.0; 3];
        let mut bounds = [0; 3];
        for i in 0..3 {
            origin[i] = size[i] * (m.mins[i] / size[i]).ceil();
            let maxs = size[i] * (m.maxs[i] / size[i]).floor();
            bounds[i] = ((maxs - origin[i]) / size[i]) as i32 + 1;
        }
        let n = (bounds[0] * bounds[1] * bounds[2]) as usize;
        if n == 0 || data.len() < n * 8 {
            return None;
        }
        Some(LightGrid { origin, size, bounds, data })
    }

    /// Every lit cell: (centre, direction towards the light, the strongest
    /// channel of its directed light, 0..255), Quake space (lamps.rs).
    pub fn cells(&self) -> Vec<([f32; 3], [f32; 3], f32)> {
        let mut out = Vec::new();
        let (bx, by, bz) = (self.bounds[0], self.bounds[1], self.bounds[2]);
        for z in 0..bz {
            for y in 0..by {
                for x in 0..bx {
                    let o = (((z * by + y) * bx + x) * 8) as usize;
                    let c = &self.data[o..o + 8];
                    let l = c[3].max(c[4]).max(c[5]) as f32;
                    if l <= 0.0 {
                        continue;
                    }
                    let lat = c[7] as f32 * (2.0 * std::f32::consts::PI / 255.0);
                    let lng = c[6] as f32 * (2.0 * std::f32::consts::PI / 255.0);
                    let p = [self.origin[0] + x as f32 * self.size[0], self.origin[1] + y as f32 * self.size[1], self.origin[2] + z as f32 * self.size[2]];
                    out.push((p, [lat.cos() * lng.sin(), lat.sin() * lng.sin(), lng.cos()], l));
                }
            }
        }
        out
    }

    /// (ambient, directed) in 0..1 gamma units and the light direction (Quake space, towards the light).
    pub fn sample(&self, p: &[f32; 3]) -> ([f32; 3], [f32; 3], [f32; 3]) {
        let mut pos = [0i32; 3];
        let mut frac = [0f32; 3];
        for i in 0..3 {
            let v = (p[i] - self.origin[i]) / self.size[i];
            pos[i] = v.floor() as i32;
            frac[i] = v - pos[i] as f32;
            pos[i] = pos[i].clamp(0, self.bounds[i] - 1);
        }
        let step = [8, 8 * self.bounds[0], 8 * self.bounds[0] * self.bounds[1]];
        let base = pos[0] * step[0] + pos[1] * step[1] + pos[2] * step[2];
        let (mut amb, mut dir, mut d) = ([0f32; 3], [0f32; 3], [0f32; 3]);
        let mut total = 0.0;
        'corner: for i in 0..8 {
            let mut factor = 1.0;
            let mut off = base;
            for j in 0..3 {
                if i & (1 << j) != 0 {
                    if pos[j] + 1 > self.bounds[j] - 1 {
                        continue 'corner;
                    }
                    factor *= frac[j];
                    off += step[j];
                } else {
                    factor *= 1.0 - frac[j];
                }
            }
            let o = off as usize;
            let c = &self.data[o..o + 8];
            if c[0] as u32 + c[1] as u32 + c[2] as u32 == 0 {
                continue;
            }
            total += factor;
            let a = color_shift([c[0], c[1], c[2]]);
            let b = color_shift([c[3], c[4], c[5]]);
            for k in 0..3 {
                amb[k] += factor * a[k];
                dir[k] += factor * b[k];
            }
            let lat = c[7] as f32 * (2.0 * std::f32::consts::PI / 255.0);
            let lng = c[6] as f32 * (2.0 * std::f32::consts::PI / 255.0);
            let n = [lat.cos() * lng.sin(), lat.sin() * lng.sin(), lng.cos()];
            for k in 0..3 {
                d[k] += factor * n[k];
            }
        }
        if total > 0.0 && total < 0.99 {
            for k in 0..3 {
                amb[k] /= total;
                dir[k] /= total;
            }
        }
        // minimum light add (identityLight * 32) and the ambient clamp (identityLightByte)
        for k in 0..3 {
            amb[k] = (amb[k] + 16.0).min(127.0) / 255.0;
            dir[k] /= 255.0;
        }
        let l = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
        let d = if l > 0.0 { [d[0] / l, d[1] / l, d[2] / l] } else { [0.0, 0.0, 1.0] };
        (amb, dir, d)
    }
}

#[cfg(test)]
mod fog_tests {
    use super::*;
    use trem_sim::kernel::bsp::{DBrush, DBrushSide};

    #[test]
    fn fog_brush_becomes_a_box() {
        let mut bsp = Bsp::default();
        // planes -x (dist 10 -> min x -10), +x 20, -y 5, +y 6, -z 1, +z 2
        bsp.planes = vec![[-1.0, 0.0, 0.0, 10.0], [1.0, 0.0, 0.0, 20.0], [0.0, -1.0, 0.0, 5.0], [0.0, 1.0, 0.0, 6.0], [0.0, 0.0, -1.0, 1.0], [0.0, 0.0, 1.0, 2.0]];
        bsp.brush_sides = (0..6).map(|i| DBrushSide { plane: i, shader: 0 }).collect();
        bsp.brushes = vec![DBrush { first_side: 0, num_sides: 6, shader: 0 }];
        let mut rec = vec![0u8; 72];
        rec[..9].copy_from_slice(b"fog/green");
        bsp.raw.insert(trem_sim::kernel::bsp::LUMP_FOGS, rec);
        let mut scripts = HashMap::new();
        scripts.insert("fog/green".to_string(), crate::assets::ShaderInfo { fog: Some(([0.1, 0.8, 0.2], 300.0)), ..Default::default() });
        let f = fog_boxes(&bsp, &scripts);
        assert_eq!(f, vec![FogBox { mins: [-10.0, -5.0, -1.0], maxs: [20.0, 6.0, 2.0], color: [0.1, 0.8, 0.2], dist: 300.0 }]);
    }
}

#[cfg(test)]
mod lightgrid_tests {
    use super::*;
    /// TREM_BSP=<file.bsp>: light grid gives plausible light at every map placement.
    #[test]
    #[ignore = "needs TREM_BSP=<file.bsp>; run with --ignored"]
    fn lightgrid_samples_real_map() {
        let p = std::env::var("TREM_BSP").expect("set TREM_BSP to a .bsp file");
        let bsp = trem_sim::kernel::bsp::parse(&std::fs::read(p).unwrap()).unwrap();
        let g = LightGrid::from_bsp(&bsp).expect("grid");
        println!("grid origin {:?} size {:?} bounds {:?}", g.origin, g.size, g.bounds);
        for e in bsp.entity_list() {
            if let Some(o) = e.get("origin").and_then(|o| trem_sim::kernel::bsp::parse_vec(o)) {
                let (a, d, dir) = g.sample(&[o[0], o[1], o[2] + 16.0]);
                println!("{:<28} amb {:.2?} dir {:.2?} {:.2?}", e.get("classname").unwrap(), a, d, dir);
            }
        }
    }
}

#[cfg(test)]
mod plan_tests {
    use super::*;
    use std::collections::HashMap;

    fn plan(src: &str, name: &str, lm_ok: bool) -> Option<Plan> {
        let mut m = HashMap::new();
        crate::q3shader::parse_script(src, &mut m);
        plan_passes(name, m.get(name), lm_ok)
    }

    #[test]
    fn light_fixture_is_a_lit_base_and_a_glow() {
        let p = plan("t/l\n{\n {\n  map $lightmap\n  rgbGen identity\n }\n {\n  map t/l.tga\n  blendFunc GL_DST_COLOR GL_ZERO\n  rgbGen identity\n }\n {\n  map t/l.blend.tga\n  blendfunc GL_ONE GL_ONE\n }\n}\n", "t/l", true).unwrap();
        assert_eq!(p.base, Some(BasePass { image: "t/l.tga".into(), light_src: 0, tint: [1.0; 3], alpha_func: 0 }));
        assert_eq!(p.stages.len(), 1);
        assert_eq!(p.lm_mod, None);
    }

    #[test]
    fn trailing_lightmap_filter_folds_into_the_passes() {
        // atcs's eq2_bounce: texture, rotating fan, texture, light map
        let p = plan("t/b\n{\n {\n  map t/b.tga\n  rgbGen identity\n }\n {\n  clampmap t/f.tga\n  tcMod rotate 512\n  blendFunc blend\n  depthWrite\n  rgbGen identity\n }\n {\n  map t/b.tga\n  blendfunc blend\n  rgbGen identity\n }\n {\n  map $lightmap\n  rgbGen identity\n  blendFunc GL_DST_COLOR GL_ZERO\n  depthFunc equal\n }\n}\n", "t/b", true).unwrap();
        let b = p.base.unwrap();
        assert_eq!((b.light_src, b.tint), (0, [1.0; 3]));
        // the pad drawn again over the fan: folded into a mask of the fan
        assert_eq!(p.stages.len(), 1);
        assert_eq!(p.masked, 1);
        assert_eq!(p.lm_mod, Some([1.0; 3]));
        assert!(p.stages[0].explicit_depth_write);
    }

    #[test]
    fn stageless_and_nodraw_scripts_draw_nothing() {
        assert_eq!(plan("t/fog\n{\n surfaceparm fog\n fogparms ( 1 0 0 ) 300\n}\n", "t/fog", true), None);
        assert!(plan("x", "t/implicit", false).unwrap().base.unwrap().light_src == 1);
    }

    #[test]
    fn decals_pull_and_blend_only_stages_stay_passes() {
        let p = plan("t/d\n{\n polygonOffset\n {\n  map t/d.tga\n  blendfunc blend\n }\n}\n", "t/d", true).unwrap();
        assert!(p.base.is_none());
        assert_eq!(p.stages.len(), 1);
        assert!(p.pull < 1.0 && p.polygon_offset);
        assert_eq!(p.sort, 4.0);
    }

    #[test]
    fn single_stage_identity_is_fullbright() {
        let p = plan("t/s\n{\n {\n  map t/s.tga\n  rgbGen identity\n }\n}\n", "t/s", true).unwrap();
        assert_eq!(p.base.unwrap().light_src, 2);
        let p = plan("t/v\n{\n {\n  map t/v.tga\n  rgbGen vertex\n  alphaFunc GE128\n }\n}\n", "t/v", true).unwrap();
        let b = p.base.unwrap();
        assert_eq!((b.light_src, b.tint, b.alpha_func), (1, [0.5; 3], 3));
    }

    #[test]
    fn autosprite2_finds_the_long_axis() {
        // a 64 x 8 quad: the short edges are its ends
        let mk = |x: f32, y: f32| V { xyz: [x, y, 0.0], ..Default::default() };
        let mut vs = vec![mk(0.0, 0.0), mk(64.0, 0.0), mk(64.0, 8.0), mk(0.0, 8.0)];
        autosprite2(&mut vs, &[0, 1, 2, 0, 2, 3]);
        let m0 = q2g(&[0.0, 4.0, 0.0]);
        let m1 = q2g(&[64.0, 4.0, 0.0]);
        let mids: Vec<[f32; 3]> = vs.iter().map(|v| if v.a2[3] > 0.5 { [v.a2[0], v.a2[1], v.a2[2]] } else { [v.a1[0], v.a1[1], v.a1[2]] }).collect();
        assert!(mids.iter().any(|m| (Vector3::new(m[0], m[1], m[2]) - m0).length() < 1e-4 || (Vector3::new(m[0], m[1], m[2]) - m1).length() < 1e-4));
        for v in &vs {
            assert!((v.a1[3].abs() - 4.0 * crate::geom::SCALE).abs() < 1e-5);
        }
    }
}
