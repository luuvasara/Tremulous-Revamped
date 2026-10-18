//! Collision on the real Tremulous maps (patches, inline models, world) is
//! bit-identical to the original C: 30,000 queries per map hashed against
//! re/traces/cm/<map>.sha256, produced by re/harness/cm_ref (the original
//! cm_load/cm_trace/cm_test/cm_patch/cm_polylib compiled natively):
//!
//!   cargo run --release -p trem_sim --example cm_query MAP.bsp 30000 7 DIR
//!   re/harness/cm_ref MAP.bsp < DIR/queries.txt | sha256sum
//!
//! Maps come from godot/base/map-*.pk3 (tracked in git); the test fails without them.

use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::PathBuf;
use trem_sim::{cmq, kernel::bsp, kernel::patch};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn bsp_from_pk3(path: &std::path::Path) -> Option<(String, Vec<u8>)> {
    let f = std::fs::File::open(path).ok()?;
    let mut z = zip::ZipArchive::new(f).ok()?;
    for i in 0..z.len() {
        let mut e = z.by_index(i).ok()?;
        let name = e.name().to_string();
        if name.starts_with("maps/") && name.ends_with(".bsp") {
            let mut v = Vec::new();
            e.read_to_end(&mut v).ok()?;
            let stem = name.trim_start_matches("maps/").trim_end_matches(".bsp").to_string();
            return Some((stem, v));
        }
    }
    None
}

#[test]
fn real_maps_match_original_collision() {
    let base = root().join("godot/base");
    let dir = std::fs::read_dir(&base).unwrap_or_else(|e| panic!("{}: {e} (tracked in git, restore it)", base.display()));
    let mut checked = 0;
    let mut pk3s: Vec<_> = dir.flatten().map(|e| e.path()).filter(|p| p.file_name().unwrap().to_string_lossy().starts_with("map-")).collect();
    pk3s.sort();
    for pk3 in pk3s {
        let Some((name, bytes)) = bsp_from_pk3(&pk3) else { continue };
        let golden = root().join(format!("re/traces/cm/{name}.sha256"));
        let want = std::fs::read_to_string(&golden).unwrap_or_else(|e| panic!("{}: {e} (tracked in git, restore it)", golden.display()));
        let cm = bsp::parse(&bytes).unwrap().clip_map();
        let q = cmq::queries(&cm, 30000, 7);
        let mut out = cmq::patch_lines(&cm);
        out.extend(q.iter().map(|l| cmq::answer(&cm, l)));
        let text = out.join("\n") + "\n";
        let got = format!("{:x}", Sha256::digest(text.as_bytes()));
        assert_eq!(got, want.trim(), "{name}: collision differs from the original (see the header of this test to diff)");
        checked += 1;
    }
    assert!(checked > 0, "no map-*.pk3 with a .bsp found in {} (tracked in git, restore them)", base.display());
    eprintln!("checked {checked} maps");
}

/// A flat 3x3 patch: collide from a known grid, traces from above stop on it,
/// a box resting on it is not stuck, and traces beside it pass.
#[test]
fn flat_patch_collides() {
    let mut pts = Vec::new();
    for j in 0..3 {
        for i in 0..3 {
            pts.push([i as f32 * 64.0, j as f32 * 64.0, 0.0]);
        }
    }
    let pc = patch::generate(3, 3, &pts).unwrap();
    assert!(!pc.facets.is_empty());
    let offsets = |mn: [f32; 3], mx: [f32; 3]| {
        let mut o = [[0.0; 3]; 8];
        for (s, v) in o.iter_mut().enumerate() {
            *v = [if s & 1 != 0 { mx[0] } else { mn[0] }, if s & 2 != 0 { mx[1] } else { mn[1] }, if s & 4 != 0 { mx[2] } else { mn[2] }];
        }
        o
    };
    let off = offsets([-8.0; 3], [8.0; 3]);
    let (s, e) = ([64.0, 64.0, 100.0], [64.0, 64.0, -100.0]);
    let bounds = [[56.0, 56.0, -108.0], [72.0, 72.0, 108.0]];
    let mut frac = 1.0;
    let hit = patch::trace_through(&patch::PatchWork { start: &s, end: &e, offsets: &off, bounds: &bounds, is_point: false }, &pc, &mut frac);
    assert!(hit.is_some());
    let stop_z = s[2] + frac * (e[2] - s[2]);
    assert!((stop_z - 8.0).abs() < 0.5, "box stops on the surface, got {stop_z}");
    let rest = [64.0, 64.0, 9.0];
    assert!(!patch::position_test(&patch::PatchWork { start: &rest, end: &rest, offsets: &off, bounds: &bounds, is_point: false }, &pc));
    let (s, e) = ([300.0, 64.0, 100.0], [300.0, 64.0, -100.0]);
    let bounds = [[292.0, 56.0, -108.0], [308.0, 72.0, 108.0]];
    let mut frac = 1.0;
    assert!(patch::trace_through(&patch::PatchWork { start: &s, end: &e, offsets: &off, bounds: &bounds, is_point: false }, &pc, &mut frac).is_none());
}
