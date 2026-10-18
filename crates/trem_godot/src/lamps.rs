//! World-fixed shadow directions for the enhanced mode's dynamic shadows
//! (port addition, dec044).
//!
//! A shadow should fall away from the light that lights the spot it falls
//! on, whoever looks at it. A compiled map keeps no light entities (q3map2
//! strips them; of its shaders only the lamps' `q3map_surfacelight` and the
//! sky's `q3map_sun` remain, and on atcs those explain under a third of the
//! light), but its light grid still says, every 64 units, where the light
//! comes from. Those directions are clustered here once per map (the sky's
//! sun kept exact as one of them); the game puts a shadow-only directional
//! light along each of the few clusters that light the area around the
//! view, and a surface takes a light's shadow only where its own grid
//! direction is that light's. So every spot shows the shadows its own light
//! throws, fixed in the world: walking about does not turn them.

use trem_sim::kernel::bsp::Bsp;

/// The directions of a map: 12 fit 82-99% of the shipped maps' light
/// within 30 degrees (FIT_COS); with the 6 that light the view's area
/// (ACTIVE) 70-98% of the light near the view has its shadow.
pub const DIRECTIONS: usize = 12;
pub const FIT_COS: f32 = 0.87;
pub const ACTIVE: usize = 6;
/// How far from the view the light is weighed for picking (units).
pub const PICK_RADIUS: f32 = 1200.0;

/// The grid cells weaker than this (directed light, 0..255) set no direction.
const MIN_DIRECTED: f32 = 24.0;

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn norm(a: [f32; 3]) -> [f32; 3] {
    let l = dot(a, a).sqrt();
    if l > 1e-6 {
        [a[0] / l, a[1] / l, a[2] / l]
    } else {
        [0.0, 0.0, 1.0]
    }
}

/// The map's shadow directions (Quake space, towards the light) and, per
/// light grid cell, the one that lights it.
#[derive(Clone, Debug, Default)]
pub struct ShadowDirs {
    pub dirs: Vec<[f32; 3]>,
    /// the sun's index in `dirs`
    pub sun: Option<usize>,
    /// (cell centre, direction index, directed light)
    cells: Vec<([f32; 3], usize, f32)>,
}

impl ShadowDirs {
    /// Spherical k-means of the cells' directions weighted by their directed
    /// light, from the strongest cell, then each next centre the direction
    /// its nearest centre explains worst (deterministic). The sun is a fixed
    /// centre.
    pub fn cluster(cells: &[([f32; 3], [f32; 3], f32)], sun: Option<[f32; 3]>, k: usize) -> ShadowDirs {
        let cells: Vec<([f32; 3], [f32; 3], f32)> = cells.iter().filter(|c| c.2 >= MIN_DIRECTED).map(|c| (c.0, norm(c.1), c.2)).collect();
        let mut dirs: Vec<[f32; 3]> = Vec::new();
        let fixed = sun.is_some() as usize;
        if let Some(s) = sun {
            dirs.push(norm(s));
        }
        if dirs.is_empty() {
            if let Some(first) = cells.iter().max_by(|a, b| a.2.partial_cmp(&b.2).unwrap_or(std::cmp::Ordering::Equal)) {
                dirs.push(first.1);
            }
        }
        while dirs.len() < k && !cells.is_empty() {
            let far = cells
                .iter()
                .map(|c| (c, (1.0 - dirs.iter().map(|d| dot(*d, c.1)).fold(-1.0, f32::max)) * c.2))
                .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
            match far {
                // within ~11 degrees of a centre already: enough centres
                Some((c, e)) if e > 0.02 * c.2 => dirs.push(c.1),
                _ => break,
            }
        }
        let nearest = |dirs: &[[f32; 3]], v: [f32; 3]| (0..dirs.len()).max_by(|&a, &b| dot(dirs[a], v).partial_cmp(&dot(dirs[b], v)).unwrap_or(std::cmp::Ordering::Equal)).unwrap_or(0);
        for _ in 0..12 {
            let mut sum = vec![[0.0f32; 3]; dirs.len()];
            for c in &cells {
                let i = nearest(&dirs, c.1);
                for j in 0..3 {
                    sum[i][j] += c.1[j] * c.2;
                }
            }
            for (i, s) in sum.iter().enumerate().skip(fixed) {
                if dot(*s, *s) > 1e-9 {
                    dirs[i] = norm(*s);
                }
            }
        }
        let cells = cells.iter().map(|c| (c.0, nearest(&dirs, c.1), c.2)).collect();
        ShadowDirs { dirs, sun: sun.map(|_| 0), cells }
    }

    /// The `max` directions that light the area around `view` (Quake space),
    /// the most light first: (index, share of the light there).
    pub fn pick(&self, view: [f32; 3], radius: f32, max: usize) -> Vec<(usize, f32)> {
        let mut w = vec![0.0f32; self.dirs.len()];
        for (p, i, l) in &self.cells {
            let d = [p[0] - view[0], p[1] - view[1], p[2] - view[2]];
            let d2 = dot(d, d);
            if d2 < radius * radius {
                w[*i] += l / (1.0 + d2 / (256.0 * 256.0));
            }
        }
        let total: f32 = w.iter().sum();
        let mut c: Vec<(usize, f32)> = w.iter().enumerate().filter(|(_, x)| **x > 0.0).map(|(i, x)| (i, x / total.max(1e-6))).collect();
        c.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal).then(a.0.cmp(&b.0)));
        c.truncate(max);
        c
    }

    /// How many grid cells set a direction.
    pub fn cell_count(&self) -> usize {
        self.cells.len()
    }

    /// The share of the directed light whose cell's direction lies within
    /// `cos` of its centre (tests: how well the directions fit the map).
    pub fn fit(&self, all: &[([f32; 3], [f32; 3], f32)], cos: f32) -> f32 {
        let (mut ok, mut tot) = (0.0, 0.0);
        for c in all.iter().filter(|c| c.2 >= MIN_DIRECTED) {
            tot += c.2;
            if self.dirs.iter().any(|d| dot(*d, norm(c.1)) >= cos) {
                ok += c.2;
            }
        }
        if tot > 0.0 { ok / tot } else { 1.0 }
    }
}

/// The sun of the map's sky shaders (`q3map_sun`): its direction towards the
/// sun (Quake space). The strongest when several.
pub fn sun(bsp: &Bsp, shaders: &std::collections::HashMap<String, crate::q3shader::Q3Shader>) -> Option<[f32; 3]> {
    bsp.shaders
        .iter()
        .filter_map(|s| shaders.get(&s.name.to_lowercase()).and_then(|q| q.sun))
        .filter(|(i, _)| *i > 0.0)
        .max_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(_, d)| d)
}

/// The GLSL both lit shaders share (bspview.rs, models.rs): how much of the
/// directed light a shadow-only light (negative energy) takes away here.
/// `gdir_vs`: the light grid's direction at the surface (view space, towards
/// the light); the light shadows it only when that is the light's own
/// direction. Its energy (-1 .. 0, x PI) fades it in and out.
pub const GLSL_SHADOW: &str = r#"
float trem_lamp_shadow(vec3 light_vs, float light_r, float atten, vec3 gdir_vs) {
    float align = smoothstep(0.80, 0.90, dot(normalize(light_vs), normalize(gdir_vs + vec3(1e-5))));
    float w = clamp(-light_r / PI, 0.0, 1.0);
    return (1.0 - clamp(atten, 0.0, 1.0)) * align * w;
}
"#;

#[cfg(test)]
mod tests {
    use super::*;

    fn cell(x: f32, d: [f32; 3], l: f32) -> ([f32; 3], [f32; 3], f32) {
        ([x, 0.0, 0.0], d, l)
    }

    #[test]
    fn two_rooms_two_directions() {
        // a room lit from +x+z, another from -y+z, a dim cell ignored
        let mut c = Vec::new();
        for i in 0..20 {
            let j = i as f32 * 0.01;
            c.push(cell(i as f32 * 64.0, [0.7 + j, 0.0, 0.7], 120.0));
            c.push(cell(4000.0 + i as f32 * 64.0, [0.0, -0.7, 0.7 - j], 90.0));
        }
        c.push(cell(0.0, [0.0, 0.0, -1.0], 5.0));
        let s = ShadowDirs::cluster(&c, None, 8);
        assert_eq!(s.dirs.len(), 2, "{:?}", s.dirs);
        assert_eq!(s.cell_count(), 40);
        assert!(s.fit(&c, 0.97) > 0.99);
        // the view in the first room: its light first, the other out of reach
        let p = s.pick([300.0, 0.0, 0.0], 1500.0, 4);
        assert_eq!(p.len(), 1);
        assert!(s.dirs[p[0].0][0] > 0.6);
        let p = s.pick([4500.0, 0.0, 0.0], 1500.0, 4);
        assert!(s.dirs[p[0].0][1] < -0.6);
    }

    #[test]
    fn the_sun_stays_exact() {
        let sun = norm([-0.5, 0.5, 0.7071]);
        let c: Vec<_> = (0..10).map(|i| cell(i as f32, norm([-0.45, 0.55, 0.7]), 200.0)).collect();
        let s = ShadowDirs::cluster(&c, Some(sun), 4);
        assert_eq!(s.sun, Some(0));
        assert_eq!(s.dirs[0], sun);
        assert_eq!(s.pick([0.0; 3], 1000.0, 4)[0].0, 0);
    }

    #[test]
    fn the_sun_points_up_its_elevation() {
        let mut bsp = Bsp::default();
        bsp.shaders.push(trem_sim::kernel::bsp::Shader { name: "t/sky".into(), ..Default::default() });
        let mut m = std::collections::HashMap::new();
        crate::q3shader::parse_script("t/sky\n{\n q3map_sun 1 0.9 0.9 150 135 45\n skyparms env/x - -\n}\n", &mut m);
        let d = sun(&bsp, &m).unwrap();
        assert!((d[2] - 45f32.to_radians().sin()).abs() < 1e-4);
        assert!(d[0] < 0.0 && d[1] > 0.0);
    }

    /// The shipped maps (their pk3s unpacked in $TREM_MAPDATA: maps/*.bsp,
    /// scripts/*.shader; the test is #[ignore]d, and fails if run without it): a dozen directions fit most
    /// of each map's light.
    #[test]
    #[ignore = "needs TREM_MAPDATA=<the maps\' pk3s unpacked>; run with --ignored"]
    fn shipped_maps_fit_their_directions() {
        let dir = std::env::var("TREM_MAPDATA").expect("set TREM_MAPDATA to a folder holding the maps' pk3s unpacked (maps/*.bsp, scripts/*.shader)");
        let mut shaders = std::collections::HashMap::new();
        let mut scripts: Vec<_> = std::fs::read_dir(format!("{dir}/scripts")).unwrap().filter_map(|e| e.ok()).map(|e| e.path()).collect();
        scripts.sort();
        for sc in scripts.iter().filter(|p| p.extension().is_some_and(|e| e == "shader")) {
            crate::q3shader::parse_script(&String::from_utf8_lossy(&std::fs::read(sc).unwrap()), &mut shaders);
        }
        let mut maps: Vec<_> = std::fs::read_dir(format!("{dir}/maps")).unwrap().filter_map(|e| e.ok()).map(|e| e.path()).collect();
        maps.sort();
        for m in maps.iter().filter(|p| p.extension().is_some_and(|e| e == "bsp")) {
            let bsp = trem_sim::kernel::bsp::parse(&std::fs::read(m).unwrap()).unwrap();
            let grid = crate::bspview::LightGrid::from_bsp(&bsp).unwrap();
            let cells = grid.cells();
            let s = ShadowDirs::cluster(&cells, sun(&bsp, &shaders), DIRECTIONS);
            let fit = s.fit(&cells, FIT_COS);
            eprintln!("{}: {} directions, {} cells, sun {:?}, {:.0}% within 30 degrees", m.display(), s.dirs.len(), s.cell_count(), s.sun, fit * 100.0);
            assert!(fit > 0.8, "{}", m.display());
            // from every 50th cell as the view: the share of the light within
            // 512 units whose direction is among the ACTIVE picked
            let (mut ok, mut tot) = (0.0, 0.0);
            for v in s.cells.iter().step_by(50) {
                let picked: Vec<usize> = s.pick(v.0, PICK_RADIUS, ACTIVE).iter().map(|p| p.0).collect();
                for c in &s.cells {
                    let d = [c.0[0] - v.0[0], c.0[1] - v.0[1], c.0[2] - v.0[2]];
                    if dot(d, d) < 512.0 * 512.0 {
                        tot += c.2;
                        if picked.contains(&c.1) {
                            ok += c.2;
                        }
                    }
                }
            }
            eprintln!("    near the view: {:.0}% of the light has its shadow light", 100.0 * ok / tot);
            assert!(ok / tot > 0.6, "{}", m.display());
        }
    }
}
