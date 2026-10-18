//! `R_MarkFragments` (tr_marks.c, kernel `marks`): mark polygons clipped
//! to world surfaces — a quad on a plane, across an edge and off a ledge,
//! on a curved patch, the fragment limits, and real maps when present.

use trem_sim::kernel::marks::{self, DrawSurf, MarkWorld, RPlane};
use trem_sim::kernel::qmath::*;

const MAX_MARK_POINTS: usize = 384;
const MAX_MARK_FRAGMENTS: usize = 128;

/// A planar face (two triangles) spanning [x0,x1] x [y0,y1] at height z.
fn floor(x0: f32, x1: f32, y0: f32, y1: f32, z: f32) -> DrawSurf {
    let points = vec![[x0, y0, z], [x1, y0, z], [x1, y1, z], [x0, y1, z]];
    DrawSurf::Face { points, indexes: vec![0, 1, 2, 0, 2, 3], plane: RPlane::new([0.0, 0.0, 1.0], z) }
}

/// `CG_ImpactMark` up to `trap_CM_MarkFragments`.
fn impact(w: &MarkWorld, origin: Vec3, dir: Vec3, orientation: f32, radius: f32) -> (Vec<Vec3>, Vec<marks::MarkFragment>) {
    let (pts, _) = marks::impact_mark_polygon(&origin, &dir, orientation, radius);
    w.mark_fragments(&pts, &scale(&dir, -20.0), MAX_MARK_POINTS, MAX_MARK_FRAGMENTS)
}

fn poly_area(p: &[Vec3]) -> f32 {
    let mut s = [0.0f32; 3];
    for i in 1..p.len().saturating_sub(1) {
        s = add(&s, &cross(&sub(&p[i], &p[0]), &sub(&p[i + 1], &p[0])));
    }
    length(&s) * 0.5
}

fn total_area(pts: &[Vec3], frags: &[marks::MarkFragment]) -> f32 {
    frags.iter().map(|f| poly_area(&pts[f.first_point..f.first_point + f.num_points])).sum()
}

#[test]
fn chop_keeps_the_front_part() {
    let tri = [[0.0, 0.0, 0.0], [10.0, 0.0, 0.0], [0.0, 10.0, 0.0]];
    // plane x = 5 facing -x keeps x < 5
    let out = marks::chop_poly_behind_plane(&tri, &[-1.0, 0.0, 0.0], -5.0, 0.5);
    assert_eq!(out.len(), 4);
    assert!(out.iter().all(|p| p[0] <= 5.0 + 1e-4));
    assert!((poly_area(&out) - (50.0 - 12.5)).abs() < 1e-3);
    // all in front / all behind
    assert_eq!(marks::chop_poly_behind_plane(&tri, &[0.0, 0.0, 1.0], -1.0, 0.5).len(), 3);
    assert!(marks::chop_poly_behind_plane(&tri, &[0.0, 0.0, 1.0], 1.0, 0.5).is_empty());
}

#[test]
fn quad_on_a_plane() {
    let w = MarkWorld::from_surfaces(vec![floor(-128.0, 128.0, -128.0, 128.0, 0.0)]);
    for orientation in [0.0, 30.0, 45.0, 290.0] {
        let (pts, frags) = impact(&w, [10.0, -20.0, 0.0], [0.0, 0.0, 1.0], orientation, 16.0);
        assert!((1..=2).contains(&frags.len()), "one fragment per face triangle the mark covers");
        assert!(pts.iter().all(|p| p[2].abs() < 1e-4), "fragments lie on the floor");
        assert!(pts.iter().all(|p| distance(&[p[0], p[1], 0.0], &[10.0, -20.0, 0.0]) <= 16.0 * 2f32.sqrt() + 0.01));
        // (the side planes are normalized with Q_rsqrt, as in the C: ~0.2% off)
        assert!((total_area(&pts, &frags) - 32.0 * 32.0).abs() < 4.0, "area {}", total_area(&pts, &frags));
    }
    // a face turned away from the projection gets no mark (dot > -0.5)
    let (_, frags) = impact(&w, [0.0, 0.0, 0.0], [0.0, 0.0, -1.0], 0.0, 16.0);
    assert!(frags.is_empty());
    // too far in front of the surface (beyond the far clip plane): nothing
    let (_, frags) = impact(&w, [0.0, 0.0, 40.0], [0.0, 0.0, 1.0], 0.0, 16.0);
    assert!(frags.is_empty());
}

#[test]
fn quad_across_an_edge() {
    // two coplanar faces meeting at x = 0: the mark continues over the seam
    let w = MarkWorld::from_surfaces(vec![floor(-128.0, 0.0, -128.0, 128.0, 0.0), floor(0.0, 128.0, -128.0, 128.0, 0.0)]);
    let (pts, frags) = impact(&w, [4.0, 0.0, 0.0], [0.0, 0.0, 1.0], 0.0, 16.0);
    assert!(frags.len() >= 2, "a fragment on each face");
    assert!((total_area(&pts, &frags) - 1024.0).abs() < 4.0);
    let left: f32 = frags.iter().map(|f| &pts[f.first_point..f.first_point + f.num_points]).filter(|p| p.iter().all(|v| v[0] <= 0.0 + 1e-4)).map(poly_area).sum();
    assert!((left - 12.0 * 32.0).abs() < 2.0, "left part {left}");
    // a ledge: the floor ends at x = 0 and the mark is cut there
    let w = MarkWorld::from_surfaces(vec![floor(-128.0, 0.0, -128.0, 128.0, 0.0)]);
    let (pts, frags) = impact(&w, [0.0, 0.0, 0.0], [0.0, 0.0, 1.0], 0.0, 16.0);
    assert!(pts.iter().all(|p| p[0] <= 1e-4));
    assert!((total_area(&pts, &frags) - 512.0).abs() < 2.0);
    // an inside corner: a wall at x = 0 facing -x next to the floor; a
    // mark on the wall wraps onto the floor only within its projection depth
    let wall = DrawSurf::Face {
        points: vec![[0.0, -128.0, 0.0], [0.0, 128.0, 0.0], [0.0, 128.0, 128.0], [0.0, -128.0, 128.0]],
        indexes: vec![0, 1, 2, 0, 2, 3],
        plane: RPlane::new([-1.0, 0.0, 0.0], 0.0),
    };
    let w = MarkWorld::from_surfaces(vec![floor(-128.0, 0.0, -128.0, 128.0, 0.0), wall]);
    let (pts, frags) = impact(&w, [0.0, 0.0, 8.0], [-1.0, 0.0, 0.0], 0.0, 16.0);
    let on_wall: f32 = frags.iter().map(|f| &pts[f.first_point..f.first_point + f.num_points]).filter(|p| p.iter().all(|v| v[0].abs() < 1e-3)).map(poly_area).sum();
    assert!((on_wall - 24.0 * 32.0).abs() < 2.0, "wall part {on_wall}");
    // the floor faces up, at 90 degrees to the projection: not marked
    assert!(pts.iter().all(|p| p[0].abs() < 1e-3));
}

#[test]
fn quad_on_a_patch() {
    // a 3x3 patch arching along x: z = 0 at the ends, 64 at the middle control row
    let mut ctrl = Vec::new();
    for y in [-128.0f32, 0.0, 128.0] {
        for (x, z) in [(-128.0f32, 0.0f32), (0.0, 64.0), (128.0, 0.0)] {
            ctrl.push([x, y, z]);
        }
    }
    let grids = marks::patch_grids(&ctrl, 3, 3);
    assert_eq!(grids.len(), 1);
    // R_SubdividePatchToGrid: columns along x until every chord is within
    // r_subdivisions (4) of the curve; the straight y direction is culled
    // to its two ends; then flipped for longer strips (width >= height)
    let (gw, gh, verts) = (grids[0].0, grids[0].1, &grids[0].2);
    assert_eq!((gw, gh), (9, 2));
    // every grid point lies on the Bezier curve
    for v in verts {
        let t = (v[0] + 128.0) / 256.0;
        assert!((v[2] - 2.0 * t * (1.0 - t) * 64.0).abs() < 1e-3, "{v:?}");
    }
    // the grid rows run along y, the columns along x, so the surface faces up
    let w = MarkWorld::from_surfaces(vec![DrawSurf::Grid { grids: grids.clone() }]);
    // the top of the arch (z = 32 at x = 0)
    let (pts, frags) = impact(&w, [0.0, 0.0, 32.0], [0.0, 0.0, 1.0], 0.0, 16.0);
    assert!(!frags.is_empty());
    // every point is on the drawn grid: within r_subdivisions of the curve,
    // below it (the chords cut under the arch)
    for p in &pts {
        let t = (p[0] + 128.0) / 256.0;
        let z = 2.0 * t * (1.0 - t) * 64.0;
        assert!(p[2] <= z + 1e-3 && z - p[2] <= marks::R_SUBDIVISIONS, "{p:?} off the grid (curve z {z})");
    }
    let flat: f32 = frags
        .iter()
        .map(|f| {
            let p: Vec<Vec3> = pts[f.first_point..f.first_point + f.num_points].iter().map(|v| [v[0], v[1], 0.0]).collect();
            poly_area(&p)
        })
        .sum();
    assert!((flat - 1024.0).abs() < 4.0, "projected area {flat}");
    // on the slope: the mark follows the curve across several grid rows
    let t = 0.25f32;
    let o = [-128.0 + 256.0 * t, 0.0, 2.0 * t * (1.0 - t) * 64.0];
    let slope = 64.0 * 2.0 * (1.0 - 2.0 * t) / 256.0 * 1.0;
    let n = normalized(&[-slope, 0.0, 1.0]);
    let (pts, frags) = impact(&w, o, n, 0.0, 24.0);
    assert!(frags.len() >= 4, "{} fragments", frags.len());
    assert!(pts.iter().all(|p| p[2] >= -1e-3));
}

#[test]
fn fragment_limits() {
    // a floor cut into 2x2 unit faces under a big mark: the 64-surface
    // list cap and the fragment cap, never more
    let mut s = Vec::new();
    for i in 0..40 {
        for j in 0..40 {
            s.push(floor(i as f32 * 2.0 - 40.0, i as f32 * 2.0 - 38.0, j as f32 * 2.0 - 40.0, j as f32 * 2.0 - 38.0, 0.0));
        }
    }
    let w = MarkWorld::from_surfaces(s);
    let (pts, frags) = impact(&w, [0.0, 0.0, 0.0], [0.0, 0.0, 1.0], 0.0, 64.0);
    assert!(frags.len() <= MAX_MARK_FRAGMENTS && pts.len() <= MAX_MARK_POINTS);
    assert_eq!(frags.len(), 128, "64 surfaces x 2 triangles");
}

/// Marks on the official maps: shots at the walls and floors around each
/// intermission point land fragments on the surfaces they hit.
#[test]
fn marks_on_official_maps() {
    use std::io::Read;
    for map in ["atcs", "nexus6"] {
        let p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("../../godot/base/map-{map}-1.1.0.pk3"));
        let f = std::fs::File::open(&p).unwrap_or_else(|e| panic!("{}: {e} (the pk3 is tracked in git, restore it)", p.display()));
        let mut z = zip::ZipArchive::new(f).unwrap();
        let mut v = Vec::new();
        z.by_name(&format!("maps/{map}.bsp")).unwrap().read_to_end(&mut v).unwrap();
        let bsp = trem_sim::kernel::bsp::parse(&v).unwrap();
        let mw = MarkWorld::from_bsp(&bsp);
        let w = trem_sim::kernel::world::World::from_bsp(&bsp, map);
        let mut hits = 0;
        let mut marked = 0;
        for team in [trem_sim::gen::enums::Team::Aliens, trem_sim::gen::enums::Team::Humans] {
            let (o, _) = w.intermission_point(team);
            for k_ in 0..32 {
                let yaw = k_ as f32 * 360.0 / 32.0;
                for pitch in [-30.0f32, 0.0, 45.0] {
                    let (f, _, _) = angle_vectors(&[pitch, yaw, 0.0]);
                    let tr = w.cm.box_trace(&o, &ma(&o, 2048.0, &f), None, None, trem_sim::gen::enums::contents::SOLID as i32);
                    if tr.fraction == 1.0 || tr.startsolid || tr.surface_flags & (trem_sim::gen::enums::surface::NOIMPACT | trem_sim::gen::enums::surface::NOMARKS | trem_sim::gen::enums::surface::SKY) as i32 != 0 {
                        continue;
                    }
                    hits += 1;
                    let (pts, frags) = impact(&mw, tr.endpos, tr.plane.normal, 0.0, 8.0);
                    if !frags.is_empty() {
                        marked += 1;
                        // within the mark square around the hit (plus projection depth)
                        assert!(pts.iter().all(|p| distance(p, &tr.endpos) < 8.0 * 1.5 + 32.0), "{map}: fragment far from the hit");
                    }
                }
            }
        }
        println!("{map}: {marked} of {hits} hits marked");
        assert!(hits > 50 && marked * 10 >= hits * 9, "{map}: {marked} of {hits} hits marked");
    }
}

/// Marks on the curved patches of a real map: a shot straight at the
/// middle of each drawn patch (down onto floors, sideways at walls) is
/// clipped to its R_SubdividePatchToGrid grid.
#[test]
fn marks_on_patches_of_niveus() {
    use std::io::Read;
    let map = "niveus";
    let p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("../../godot/base/map-{map}-1.1.0.pk3"));
    let f = std::fs::File::open(&p).unwrap_or_else(|e| panic!("{}: {e} (the pk3 is tracked in git, restore it)", p.display()));
    let mut z = zip::ZipArchive::new(f).unwrap();
    let mut v = Vec::new();
    z.by_name(&format!("maps/{map}.bsp")).unwrap().read_to_end(&mut v).unwrap();
    let bsp = trem_sim::kernel::bsp::parse(&v).unwrap();
    let mw = MarkWorld::from_bsp(&bsp);
    let w = trem_sim::kernel::world::World::from_bsp(&bsp, map);
    let (mut hits, mut marked) = (0, 0);
    for (si, s) in mw.surfs.iter().enumerate() {
        let DrawSurf::Grid { grids } = s else { continue };
        for (gw, gh, verts) in grids {
            // the middle vertex of the grid and its neighbours' normal
            let (i, j) = (gw / 2, gh / 2);
            if i + 1 >= *gw || j + 1 >= *gh {
                continue;
            }
            let p0 = verts[j * gw + i];
            let n = normalized(&cross(&sub(&verts[(j + 1) * gw + i], &p0), &sub(&verts[j * gw + i + 1], &p0)));
            for sign in [1.0f32, -1.0] {
                let start = ma(&p0, 48.0 * sign, &n);
                let tr = w.cm.box_trace(&start, &ma(&p0, -16.0 * sign, &n), None, None, trem_sim::gen::enums::contents::SOLID as i32);
                if tr.fraction == 1.0 || tr.startsolid || tr.allsolid || tr.surface_flags & (trem_sim::gen::enums::surface::NOIMPACT | trem_sim::gen::enums::surface::NOMARKS | trem_sim::gen::enums::surface::SKY) as i32 != 0 {
                    continue;
                }
                if distance(&tr.endpos, &p0) > 8.0 {
                    continue; // something else is in front of this patch
                }
                hits += 1;
                let (_, frags) = impact(&mw, tr.endpos, tr.plane.normal, 0.0, 8.0);
                if !frags.is_empty() {
                    marked += 1;
                } else {
                    println!("{map}: surface {si} at {:?} normal {:?} not marked", tr.endpos, tr.plane.normal);
                }
            }
        }
    }
    println!("{map}: {marked} of {hits} patch hits marked");
    assert!(hits > 50 && marked * 10 >= hits * 9, "{map}: {marked} of {hits} patch hits marked");
}

/// R_SubdividePatchToGrid against the original tr_curve.c (re/harness
/// curve_ref): every patch of the 8 official maps, grid sizes, vertex
/// positions and MakeMeshNormals' normals bit for bit
/// (re/traces/curve/<map>.sha256).
#[test]
fn patch_grids_match_the_original_tr_curve() {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    use trem_sim::kernel::marks::{subdivide_patch_to_grid, GridPoint, R_SUBDIVISIONS};
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut checked = 0;
    for map in ["atcs", "arachnid2", "karith", "nexus6", "niveus", "transit", "tremor", "uncreation"] {
        let pk3 = root.join(format!("godot/base/map-{map}-1.1.0.pk3"));
        let f = std::fs::File::open(&pk3).unwrap_or_else(|e| panic!("{}: {e} (the pk3 is tracked in git, restore it)", pk3.display()));
        let mut z = zip::ZipArchive::new(f).unwrap();
        let mut v = Vec::new();
        z.by_name(&format!("maps/{map}.bsp")).unwrap().read_to_end(&mut v).unwrap();
        let bsp = trem_sim::kernel::bsp::parse(&v).unwrap();
        let mut out = String::new();
        for (i, s) in bsp.surfaces.iter().enumerate() {
            if s.surface_type != trem_sim::kernel::bsp::MST_PATCH || s.patch_width > 32 || s.patch_height > 32 {
                continue;
            }
            let fv = s.first_vert as usize;
            let pts: Vec<GridPoint> = bsp.verts[fv..fv + s.num_verts as usize].iter().map(|d| GridPoint { xyz: d.xyz, normal: d.normal }).collect();
            let (w, h, grid) = subdivide_patch_to_grid(s.patch_width as usize, s.patch_height as usize, &pts, R_SUBDIVISIONS);
            out.push_str(&format!("{i} {w} {h}\n"));
            for g in &grid {
                let b: Vec<String> = g.xyz.iter().chain(g.normal.iter()).map(|x| format!("{:08x}", x.to_bits())).collect();
                out.push_str(&b.join(" "));
                out.push('\n');
            }
        }
        let hash: String = Sha256::digest(out.as_bytes()).iter().map(|b| format!("{b:02x}")).collect();
        let want = std::fs::read_to_string(root.join(format!("re/traces/curve/{map}.sha256"))).unwrap();
        assert_eq!(hash, want.trim(), "{map}: patch grids differ from tr_curve.c");
        checked += 1;
    }
    println!("{checked} maps' patch grids identical to tr_curve.c");
}
