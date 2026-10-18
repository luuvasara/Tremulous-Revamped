//! Definition of done of the `bots` feature (dec015, dec028), checked on
//! the sheet arena always and on the eight official maps when their pk3s
//! are in godot/base (or `$TREM_BSP_DIR/<map>.bsp`):
//!
//! **Match** — three bots a side, ten minutes (or until a team wins):
//! * both teams kill at least `MIN_KILLS` enemy players;
//! * deaths to the map (falls, kill triggers, crushers, lava) are at most
//!   `MAX_HAZARD_SHARE` of all deaths — enemy knockback into a pit is part
//!   of the game — and none at all on the arena;
//! * no bot lingers stuck: never more than `MAX_STUCK_MS` within
//!   `STUCK_RADIUS` while it has no target and is away from its goal (a bot
//!   wedged in the map types `/kill` after 10 s, which takes 20 s), and at
//!   most `MAX_SUICIDES` such `/kill`s;
//! * aliens evolve (`class`) and humans buy (`buy`) with the credits they earn;
//! * both teams damage the enemy's structures (evolved aliens raid the human
//!   base, dec028).
//!
//! **Objective** — two bots of one team alone with the team's credit cap,
//! five minutes (or until they win), for each team:
//! * they spend credits (evolve / buy) and damage the enemy's structures by
//!   at least `MIN_OBJECTIVE_DAMAGE` health;
//! * at most `MAX_OBJECTIVE_HAZARDS` deaths to the map, none on the arena,
//!   and no bot stuck longer than `MAX_STUCK_MS`.
//!
//! **Balance** — the match above on each official map with `BALANCE_SEEDS`
//! bot / game RNG seeds (`bot_match::seed`):
//! * on every map neither team wins more than `MAX_MAP_WIN_SHARE` of the
//!   runs, and over all maps not more than `MAX_WIN_SHARE`;
//! * matches last most of the ten minutes: at least `MIN_MEAN_MATCH_S` on
//!   average over all runs and `MIN_MAP_MEAN_MATCH_S` on every map.
//!
//! The official maps take about two minutes in release, so that test is
//! `#[ignore]`d, as is the balance test (about five minutes): `cargo test
//! --release -p trem_sim --test bots -- --ignored --nocapture`.
//! Measurements: `cargo run --release -p trem_sim --example match_sim --
//! 600 3 arena atcs ...` (`-- 300 a2 ...` / `h2` for the objective
//! scenario; `TREM_SEEDS=0,1,2 ...` for the balance summary).

mod common;
use common::bot_match::{self, Balance, Report, OFFICIAL_MAPS};
use trem_sim::gen::enums::Team;

const MATCH_MS: i32 = 600_000;
const OBJECTIVE_MS: i32 = 300_000;
const MIN_KILLS: i32 = 3;
const MAX_HAZARD_SHARE: f32 = 0.10;
const MAX_STUCK_MS: i32 = 35_000;
const MAX_SUICIDES: i32 = 5;
const MIN_OBJECTIVE_DAMAGE: i32 = 100;
const MAX_OBJECTIVE_HAZARDS: i32 = 2;
const BALANCE_SEEDS: u32 = 6;
const MAX_MAP_WIN_SHARE: f32 = 0.67;
const MAX_WIN_SHARE: f32 = 0.60;
const MIN_MEAN_MATCH_S: f32 = 480.0;
const MIN_MAP_MEAN_MATCH_S: f32 = 360.0;

fn deaths(r: &Report) -> i32 {
    r.kills[0] + r.kills[1] + r.world_deaths.values().sum::<i32>()
}

fn check_match(r: &Report, hazards_allowed: bool) -> Vec<String> {
    let mut bad = Vec::new();
    let mut need = |ok: bool, what: String| {
        if !ok {
            bad.push(format!("{} match: {what}", r.map));
        }
    };
    need(r.kills[0] >= MIN_KILLS && r.kills[1] >= MIN_KILLS, format!("kills A{} H{} (need {MIN_KILLS} each)", r.kills[0], r.kills[1]));
    let hazard_cap = if hazards_allowed { MAX_HAZARD_SHARE * deaths(r) as f32 } else { 0.0 };
    need(r.hazard_deaths as f32 <= hazard_cap, format!("{} of {} deaths to the map", r.hazard_deaths, deaths(r)));
    need(r.suicides <= MAX_SUICIDES, format!("{} suicides", r.suicides));
    need(r.max_stuck_ms <= MAX_STUCK_MS, format!("stuck {} ms at {}", r.max_stuck_ms, r.max_stuck_at));
    need(r.evolves >= 1, "aliens never evolved".into());
    need(r.buys >= 1, "humans never bought".into());
    need(r.struct_damage[1] > 0, "humans never damaged an alien structure".into());
    need(r.struct_damage[0] > 0, "aliens never damaged a human structure".into());
    bad
}

fn check_objective(r: &Report, team: Team, hazards_allowed: bool) -> Vec<String> {
    let mut bad = Vec::new();
    let (who, dmg, spent) = match team {
        Team::Aliens => ("aliens", r.struct_damage[0], r.evolves),
        _ => ("humans", r.struct_damage[1], r.buys),
    };
    let mut need = |ok: bool, what: String| {
        if !ok {
            bad.push(format!("{} {who} objective: {what}", r.map));
        }
    };
    need(spent >= 1, "no credits spent".into());
    need(dmg >= MIN_OBJECTIVE_DAMAGE, format!("enemy structures lost {dmg} health (need {MIN_OBJECTIVE_DAMAGE})"));
    let cap = if hazards_allowed { MAX_OBJECTIVE_HAZARDS } else { 0 };
    need(r.hazard_deaths <= cap, format!("{} deaths to the map", r.hazard_deaths));
    need(r.suicides <= MAX_SUICIDES, format!("{} suicides", r.suicides));
    need(r.max_stuck_ms <= MAX_STUCK_MS, format!("stuck {} ms at {}", r.max_stuck_ms, r.max_stuck_at));
    bad
}

/// Match and both objective runs on `map`; None when it is not found.
fn play(map: &str, hazards_allowed: bool) -> Option<Vec<String>> {
    let mut w = bot_match::load(map)?;
    let r = bot_match::run(&mut w, map, 3, MATCH_MS, false);
    println!("match      {}", r.line());
    let mut bad = check_match(&r, hazards_allowed);
    for team in [Team::Aliens, Team::Humans] {
        let mut w = bot_match::load(map)?;
        let r = bot_match::run_objective(&mut w, map, team, 2, OBJECTIVE_MS, false);
        println!("{:<10} {}", format!("{team:?}"), r.line());
        bad.extend(check_objective(&r, team, hazards_allowed));
    }
    Some(bad)
}

#[test]
fn bots_play_the_sheet_arena() {
    let bad = play("arena", false).unwrap();
    assert!(bad.is_empty(), "{bad:#?}");
}

#[test]
fn bot_match_report_is_deterministic() {
    let run = || {
        let mut w = bot_match::load("arena").unwrap();
        bot_match::run(&mut w, "arena", 2, 90_000, false).line()
    };
    assert_eq!(run(), run());
}

#[test]
#[ignore = "about two minutes in release; needs godot/base's map pk3s"]
fn bots_play_the_official_maps() {
    let mut bad = Vec::new();
    for m in OFFICIAL_MAPS {
        match play(m, true) {
            Some(b) => bad.extend(b),
            None => panic!("{m}: map not found (godot/base/map-{m}-1.1.0.pk3 or $TREM_BSP_DIR/{m}.bsp)"),
        }
    }
    assert!(bad.is_empty(), "{bad:#?}");
}

/// The balance criteria over match reports of several seeds per map.
fn check_balance(all: &[Report]) -> Vec<String> {
    let mut bad = Vec::new();
    let mut maps: Vec<&str> = all.iter().map(|r| r.map.as_str()).collect();
    maps.dedup();
    for m in maps {
        let b = Balance::of(all.iter().filter(|r| r.map == m));
        if b.max_win_share() > MAX_MAP_WIN_SHARE {
            bad.push(format!("{m}: one team won too often ({})", b.line(m)));
        }
        if b.mean_s < MIN_MAP_MEAN_MATCH_S {
            bad.push(format!("{m}: matches too short ({})", b.line(m)));
        }
    }
    let b = Balance::of(all.iter());
    if b.max_win_share() > MAX_WIN_SHARE {
        bad.push(format!("one team won too often overall ({})", b.line("all")));
    }
    if b.mean_s < MIN_MEAN_MATCH_S {
        bad.push(format!("matches too short overall ({})", b.line("all")));
    }
    bad
}

#[test]
fn balance_criteria() {
    let r = |map: &str, winner: Option<Team>, s: i32| Report { map: map.into(), ms: s * 1000, winner, ..Default::default() };
    let even = vec![r("a", Some(Team::Aliens), 400), r("a", Some(Team::Humans), 600), r("a", None, 600), r("b", None, 600), r("b", Some(Team::Aliens), 500), r("b", None, 600)];
    assert!(check_balance(&even).is_empty(), "{:?}", check_balance(&even));
    let one_sided = vec![r("a", Some(Team::Aliens), 400), r("a", Some(Team::Aliens), 600), r("a", Some(Team::Aliens), 600), r("b", None, 600)];
    let bad = check_balance(&one_sided);
    assert!(bad.iter().any(|b| b.starts_with("a: one team")) && bad.iter().any(|b| b.starts_with("one team won too often overall")), "{bad:?}");
    let short = vec![r("a", None, 200), r("a", None, 300), r("b", None, 600)];
    assert!(check_balance(&short).iter().any(|b| b.starts_with("a: matches too short")));
}

#[test]
#[ignore = "about five minutes in release; needs godot/base's map pk3s"]
fn bots_are_balanced_on_the_official_maps() {
    let mut all = Vec::new();
    for m in OFFICIAL_MAPS {
        for seed in 0..BALANCE_SEEDS {
            let mut w = bot_match::load(m).unwrap_or_else(|| panic!("{m}: map not found (godot/base/map-{m}-1.1.0.pk3 or $TREM_BSP_DIR/{m}.bsp)"));
            bot_match::seed(&mut w, seed);
            let r = bot_match::run(&mut w, m, 3, MATCH_MS, false);
            println!("seed {seed} {}", r.line());
            all.push(r);
        }
    }
    println!("{}", bot_match::balance_summary(&all));
    let bad = check_balance(&all);
    assert!(bad.is_empty(), "{bad:#?}");
}
