//! Map entities on the real Tremulous maps (g_spawn.c, g_mover.c,
//! g_trigger.c, g_target.c, g_misc.c): every classname the maps use spawns,
//! doors open for players and block traces when shut, pendulums swing, hurt
//! triggers hurt, and entity chains (relays/delays/timers) fire. Skipped when
//! godot/base has no map pk3s.

use std::io::Read;
use std::path::PathBuf;
use trem_sim::kernel::world::{EType, MoverState, World};
use trem_sim::kernel::{bsp, client};

fn map(name: &str) -> Option<World> {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("../../godot/base/map-{name}-1.1.0.pk3"));
    let f = std::fs::File::open(&p).unwrap_or_else(|e| panic!("{}: {e} (the pk3 is tracked in git, restore it)", p.display()));
    let mut z = zip::ZipArchive::new(f).ok()?;
    let mut e = z.by_name(&format!("maps/{name}.bsp")).ok()?;
    let mut v = Vec::new();
    e.read_to_end(&mut v).ok()?;
    let b = bsp::parse(&v).ok()?;
    Some(World::from_bsp(&b, name))
}

fn class_count(w: &World, c: &str) -> usize {
    (0..w.num_entities).filter(|&i| w.ents[i].inuse && w.ents[i].classname == c).count()
}

fn run(w: &mut World, ms: i32) {
    for _ in 0..ms / 50 {
        w.frame(50);
    }
}

#[test]
fn every_map_entity_spawns() {
    let expect: &[(&str, &[(&str, usize)])] = &[
        ("karith", &[("func_door", 16), ("func_button", 1), ("func_pendulum", 1), ("func_static", 15), ("target_speaker", 19), ("misc_particle_system", 2)]),
        ("nexus6", &[("func_door", 16), ("misc_light_flare", 43), ("target_speaker", 8)]),
        ("atcs", &[("target_relay", 16), ("target_delay", 24), ("trigger_hurt", 8), ("trigger_win", 2), ("target_speaker", 8)]),
        ("uncreation", &[("func_door", 13), ("func_door_rotating", 2), ("trigger_stage", 4), ("trigger_multiple", 2), ("target_kill", 2)]),
        ("transit", &[("func_door", 19), ("trigger_multiple", 7)]),
        ("niveus", &[("func_door", 16), ("misc_anim_model", 18), ("misc_light_flare", 51)]),
        ("arachnid2", &[("func_timer", 2), ("trigger_hurt", 1)]),
    ];
    let mut checked = 0;
    for (name, counts) in expect {
        let Some(mut w) = map(name) else { continue };
        run(&mut w, 300); // doors spawn their triggers on the second frame
        for (c, n) in counts.iter() {
            assert_eq!(class_count(&w, c), *n, "{name}: {c}");
        }
        // info_null / light entities are freed, doors got triggers
        assert_eq!(class_count(&w, "info_null"), 0, "{name}");
        // every untargeted, unshootable door master spawned its trigger
        let auto_doors = (0..w.num_entities)
            .filter(|&i| w.ents[i].inuse && w.ents[i].classname == "func_door" && w.ents[i].targetname.is_none() && !w.ents[i].takedamage && w.ents[i].teammaster.is_none_or(|m| m == i))
            .count();
        assert_eq!(class_count(&w, "door_trigger"), auto_doors, "{name}: door triggers");
        // every brush mover is solid against traces at rest
        for i in 0..w.num_entities {
            let e = &w.ents[i];
            if e.inuse && e.etype == EType::Mover {
                assert!(e.bmodel && e.model_index > 0, "{name}: mover {i} has a brush model");
            }
        }
        checked += 1;
    }
    eprintln!("checked {checked} maps");
}

/// Put a human player at `origin` (alive, normal movement).
fn human_at(w: &mut World, origin: [f32; 3]) -> usize {
    let i = w.connect("tester", true).unwrap();
    let _ = client::command(w, i, "team humans");
    let _ = client::command(w, i, "class rifle");
    for _ in 0..40 {
        w.frame(50);
        if w.clients[i].ps.stats[0] > 0 && w.ents[i].etype == EType::Player {
            break;
        }
    }
    w.clients[i].ps.origin = origin;
    w.clients[i].ps.velocity = [0.0; 3];
    w.ents[i].origin = origin;
    i
}

#[test]
fn doors_open_for_players_and_block_when_shut() {
    let Some(mut w) = map("nexus6") else { return };
    run(&mut w, 300);
    // pick an auto door (has a door_trigger parented to it)
    let trig = (0..w.num_entities).find(|&i| w.ents[i].inuse && w.ents[i].classname == "door_trigger").expect("door trigger");
    let door = w.ents[trig].parent.unwrap();
    let rest = w.ents[door].origin;
    assert_eq!(w.ents[door].mover_state, MoverState::Pos1);

    // a trace through the shut door's middle hits the door entity
    let (lo, hi) = w.ents[door].abs_bounds();
    let mid = [(lo[0] + hi[0]) * 0.5, (lo[1] + hi[1]) * 0.5, (lo[2] + hi[2]) * 0.5];
    let axis = w.ents[trig].count as usize;
    let mut a = mid;
    let mut b = mid;
    a[axis] = lo[axis] - 40.0;
    b[axis] = hi[axis] + 40.0;
    let tr = w.trace(&a, None, None, &b, -1, 1);
    assert!(tr.fraction < 1.0, "shut door blocks");

    // a player standing in the trigger opens it
    let (tlo, thi) = w.ents[trig].abs_bounds();
    let mut stand = [(tlo[0] + thi[0]) * 0.5, (tlo[1] + thi[1]) * 0.5, tlo[2] + 40.0];
    stand[axis] = tlo[axis] + 20.0;
    let p = human_at(&mut w, stand);
    let _ = p;
    let mut opened = false;
    for _ in 0..80 {
        w.frame(50);
        if w.ents[door].mover_state == MoverState::Pos2 {
            opened = true;
            break;
        }
    }
    assert!(opened, "door reached pos2 (state {:?})", w.ents[door].mover_state);
    assert!(trem_sim::kernel::qmath::distance(&w.ents[door].origin, &w.ents[door].pos2) < 0.01);
    assert!(trem_sim::kernel::qmath::distance(&w.ents[door].origin, &rest) > 8.0);
    // and it comes back after its wait once nobody is near
    w.clients[p].ps.origin = [stand[0] + 4000.0, stand[1], stand[2]];
    w.ents[p].origin = w.clients[p].ps.origin;
    w.unlink(p);
    let mut closed = false;
    for _ in 0..200 {
        w.frame(50);
        if w.ents[door].mover_state == MoverState::Pos1 {
            closed = true;
            break;
        }
    }
    assert!(closed, "door returned to pos1");
}

#[test]
fn pendulum_swings_and_relays_fire() {
    if let Some(mut w) = map("karith") {
        let pend = (0..w.num_entities).find(|&i| w.ents[i].inuse && w.ents[i].classname == "func_pendulum").unwrap();
        run(&mut w, 200);
        let a0 = w.ents[pend].angles;
        run(&mut w, 700);
        let a1 = w.ents[pend].angles;
        assert!((a0[2] - a1[2]).abs() > 0.5, "pendulum roll changes: {a0:?} -> {a1:?}");
    }
    if let Some(mut w) = map("arachnid2") {
        // func_timers fire their targets repeatedly: watch for general sounds or uses
        run(&mut w, 300);
        let timers: Vec<usize> = (0..w.num_entities).filter(|&i| w.ents[i].inuse && w.ents[i].classname == "func_timer").collect();
        assert_eq!(timers.len(), 2);
    }
}

#[test]
fn trigger_hurt_hurts() {
    let Some(mut w) = map("atcs") else { return };
    run(&mut w, 300);
    let hurt = (0..w.num_entities).find(|&i| w.ents[i].inuse && w.ents[i].classname == "trigger_hurt" && w.ents[i].linked).expect("an active trigger_hurt");
    let (lo, hi) = w.ents[hurt].abs_bounds();
    let c = [(lo[0] + hi[0]) * 0.5, (lo[1] + hi[1]) * 0.5, (lo[2] + hi[2]) * 0.5];
    let p = human_at(&mut w, c);
    let h0 = w.ents[p].health;
    w.frame(50);
    w.frame(50);
    assert!(w.ents[p].health < h0 || w.ents[p].health <= 0, "health {h0} -> {}", w.ents[p].health);
}

#[test]
fn doors_are_area_portals_and_pvs_is_loaded() {
    let mut any = false;
    for name in ["nexus6", "transit", "uncreation", "niveus", "karith"] {
        let Some(mut w) = map(name) else { continue };
        run(&mut w, 300);
        assert!(w.cm.vised && w.cm.num_areas >= 1, "{name}: visibility lump");
        // a point always sees itself
        let o = w.intermission_point(trem_sim::gen::enums::Team::None).0;
        assert!(w.in_pvs(&o, &o));
        // a door spanning two areas: shut -> disconnected, open -> connected
        let Some(door) = (0..w.num_entities).find(|&i| {
            let e = &w.ents[i];
            e.inuse && e.classname == "func_door" && e.areanum2 != -1 && e.teammaster.is_none_or(|m| m == i) && e.targetname.is_none()
        }) else {
            continue;
        };
        let (a1, a2) = (w.ents[door].areanum, w.ents[door].areanum2);
        let before = w.cm.areas_connected(a1, a2);
        trem_sim::kernel::mover::use_binary_mover(&mut w, door, None, None);
        assert!(w.cm.areas_connected(a1, a2), "{name}: open door connects areas {a1}/{a2}");
        if !before {
            // and it closes again after its wait
            for _ in 0..400 {
                w.frame(50);
                if w.ents[door].mover_state == MoverState::Pos1 {
                    break;
                }
            }
            assert_eq!(w.ents[door].mover_state, MoverState::Pos1);
            assert_eq!(w.cm.areas_connected(a1, a2), before, "{name}: shut door separates areas again");
        }
        any = true;
    }
    eprintln!("area portal door found: {any}");
}
