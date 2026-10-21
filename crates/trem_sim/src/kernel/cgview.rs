//! Kernel `cgview` — the cgame side of a networked client (dec033):
//! cgame/cg_snapshot.c (`CG_ProcessSnapshots`, `CG_SetInitialSnapshot`,
//! `CG_SetNextSnap`, `CG_TransitionSnapshot`, `CG_TransitionEntity`,
//! `CG_ResetEntity`), cgame/cg_predict.c (`CG_PredictPlayerState` with
//! Pmove over the snapshot's solid entities and the prediction error decay,
//! `CG_InterpolatePlayerState`, `CG_AdjustPositionForMover`), cgame/cg_ents.c
//! (`CG_CalcEntityLerpPositions`, `CG_InterpolateEntityPosition`),
//! cgame/cg_event.c (`CG_CheckEvents`), cgame/cg_playerstate.c
//! (`CG_CheckPlayerstateEvents`, `CG_CheckChangedPredictableEvents`) and
//! cgame/cg_servercmds.c (`CG_ServerCommand`: chat, cp, cs, print, scores,
//! servermenu, tinfo, poisoncloud, map_restart, `CG_ConfigStringModified`,
//! `CG_ParseServerinfo`, `CG_ParseScores`, `CG_ParseTeamInfo`).
//!
//! Against a Tremulous 1.1.0 server (protocol 69, `vanilla`, dec041) the
//! snapshots arrive already in legacy terms (kernel::v11) and the cgame
//! predicts with 1.1's movement (kernel::pmove11 on `v11::ps_to_11` of the
//! predicted state), uses the 1.1 class and structure boxes, handles the
//! events 1.1 carries on regular entities (`CG_CheckEvents`' `s.event`
//! half) and fills the build points, spawn counts and stages 1.1 sends in
//! configstrings (`v11::CS11_*`).
//!
//! Its output is a *view World*: a `World` on the same map whose entities,
//! clients, configstring tables and level facts are what the client knows,
//! interpolated to cg.time, with the local player predicted. The
//! presentation reads it exactly like the single-process World, so the
//! listen-server host and remote clients render through the same code.

use super::bg::{self, Trajectory};
use super::cl::{NetClient, Snapshot, CMD_BACKUP, CMD_MASK};
use super::cmds::SayMode;
use super::msg::{EntityState, MAX_GENTITIES};
use super::pmove::Pmove;
use super::qmath::*;
use super::rng::Rng;
use super::svgame::{self, cs, ET_EVENTS, ET_LEV2_ZAP_CHAIN, SOLID_BMODEL};
use super::types::*;
use super::world::{EType, Entity, GameEvent, World, Zap, MAX_CLIENTS};
use crate::gen::buildables::BuildableId;
use crate::gen::classes::ClassId;
use crate::gen::enums::{contents, Event, Pers, PmType, SpectatorState, Stat, Team, Traj};
use crate::gen::missiles::{MissileId, MISSILES};
use crate::gen::upgrades::UpgradeId;

/// `MAX_PREDICTED_EVENTS`.
const MAX_PREDICTED_EVENTS: usize = 16;
/// `SNAPFLAG_SERVERCOUNT` / `SNAPFLAG_NOT_ACTIVE`.
use super::sv::{SNAPFLAG_NOT_ACTIVE, SNAPFLAG_SERVERCOUNT};

/// `centity_t` (the members the snapshot code uses).
#[derive(Clone, Debug, Default)]
pub struct CEntity {
    pub current_state: EntityState,
    pub next_state: EntityState,
    pub interpolate: bool,
    pub current_valid: bool,
    pub previous_event: i32,
    pub snapshot_time: i32,
    pub lerp_origin: Vec3,
    pub lerp_angles: Vec3,
}

/// One row of `cg.scores`.
#[derive(Clone, Copy, Debug, Default)]
pub struct Score {
    pub client: usize,
    pub score: i32,
    pub ping: i32,
    pub time: i32,
    pub weapon: i32,
    pub upgrade: i32,
}

/// `cg` + `cgs` (the parts the view needs).
pub struct Cgame {
    pub client_num: i32,
    pub latest_snapshot_num: i32,
    pub processed_snapshot_num: i32,
    pub snap: Option<Snapshot>,
    pub next_snap: Option<Snapshot>,
    pub server_command_sequence: i32,
    /// cg.time / cg.oldTime / cg.physicsTime
    pub time: i32,
    pub old_time: i32,
    pub physics_time: i32,
    pub frame_interpolation: f32,
    pub this_frame_teleport: bool,
    pub next_frame_teleport: bool,
    pub valid_pps: bool,
    pub predicted_player_state: PlayerState,
    pub pmext: PmoveExt,
    pub predicted_error: Vec3,
    pub predicted_error_time: i32,
    pub event_sequence: i32,
    pub predictable_events: [i32; MAX_PREDICTED_EVENTS],
    pub entities: Vec<CEntity>,
    big_cs: String,
    cs_dirty: bool,
    pub scores: Vec<Score>,
    /// tinfo: (location, health, weapon or class, upgrade) per client
    pub team_info: Vec<Option<(i32, i32, i32, i32)>>,
    /// cg_errorDecay
    pub error_decay: f32,
    /// cg_nopredict
    pub nopredict: bool,
    /// prediction misses seen (length), for tests and cg_showmiss
    pub misses: Vec<f32>,
    pub show_miss: bool,
    /// events fired this frame (also pushed to the view World)
    pub fired: u64,
    rng: Rng,
    alien_stage: i32,
    human_stage: i32,
    /// zap chains from ET_LEV2_ZAP_CHAIN entities
    zaps: Vec<Zap>,
    pub print_out: String,
    /// Connected to a Tremulous 1.1.0 server (protocol 69): from the
    /// serverinfo's "protocol", or set by whoever starts the cgame (dec041).
    pub vanilla: bool,
    /// CS_BUILDPOINTS of 1.1: (alien BP, alien total, human BP, human total,
    /// human powered) and CS_SPAWNS: (alien spawns, human spawns).
    pub bp11: (i32, i32, i32, i32, i32),
    pub spawns11: (i32, i32),
}

impl Cgame {
    /// `CG_Init` for client `client_num` (clc.clientNum).
    pub fn new(client_num: i32) -> Self {
        Cgame {
            client_num,
            latest_snapshot_num: 0,
            processed_snapshot_num: 0,
            snap: None,
            next_snap: None,
            server_command_sequence: 0,
            time: 0,
            old_time: 0,
            physics_time: 0,
            frame_interpolation: 0.0,
            this_frame_teleport: false,
            next_frame_teleport: false,
            valid_pps: false,
            predicted_player_state: PlayerState::default(),
            pmext: PmoveExt::default(),
            predicted_error: [0.0; 3],
            predicted_error_time: 0,
            event_sequence: 0,
            predictable_events: [0; MAX_PREDICTED_EVENTS],
            entities: vec![CEntity::default(); MAX_GENTITIES as usize],
            big_cs: String::new(),
            cs_dirty: true,
            scores: Vec::new(),
            team_info: vec![None; MAX_CLIENTS],
            error_decay: 100.0,
            nopredict: false,
            misses: Vec::new(),
            show_miss: false,
            fired: 0,
            rng: Rng::default(),
            alien_stage: -1,
            human_stage: -1,
            zaps: Vec::new(),
            print_out: String::new(),
            vanilla: false,
            bp11: (0, 0, 0, 0, 0),
            spawns11: (0, 0),
        }
    }

    /// `CG_Init`'s start: the server command sequence of the gamestate and
    /// the snapshot number to start from.
    pub fn init(cl: &NetClient) -> Self {
        let mut cg = Cgame::new(cl.client_num);
        cg.server_command_sequence = cl.server_command_sequence;
        cg.processed_snapshot_num = cl.snap.message_num;
        cg
    }

    /// `CG_DrawActiveFrame`'s simulation: snapshots, server commands,
    /// prediction and the entity positions, written into the view World.
    pub fn frame(&mut self, cl: &mut NetClient, w: &mut World) {
        self.old_time = self.time;
        self.time = cl.server_time;
        w.vanilla = self.vanilla;
        if self.cs_dirty {
            self.apply_configstrings(cl, w);
        }
        // set up cg.snap and possibly cg.nextSnap
        self.process_snapshots(cl, w);
        // if we haven't received any snapshots yet, all
        // we can draw is the information screen
        if self.snap.is_none() || self.snap.as_ref().is_some_and(|s| s.snap_flags & SNAPFLAG_NOT_ACTIVE != 0) {
            return;
        }
        if self.cs_dirty {
            self.apply_configstrings(cl, w);
        }
        // cg.frameInterpolation
        let (st, nt) = (self.snap.as_ref().unwrap().server_time, self.next_snap.as_ref().map(|n| n.server_time));
        self.frame_interpolation = match nt {
            Some(nt) if nt > st => (self.time - st) as f32 / (nt - st) as f32,
            _ => 0.0,
        };
        w.level.previous_time = w.level.time;
        w.level.time = self.time;
        w.level.msec = (self.time - self.old_time).max(0);
        // the other entities first: the prediction traces against them
        self.add_packet_entities(w);
        // update cg.predictedPlayerState
        self.predict_player_state(cl, w);
        self.write_local_player(w);
    }

    // ============================================================ snapshots

    /// `CG_ReadNextSnapshot`.
    fn read_next_snapshot(&mut self, cl: &NetClient) -> Option<Snapshot> {
        while self.processed_snapshot_num < self.latest_snapshot_num {
            // try to read the snapshot from the client system
            self.processed_snapshot_num += 1;
            if let Some(s) = cl.get_snapshot(self.processed_snapshot_num) {
                return Some(s);
            }
            // a GetSnapshot will return failure if the snapshot
            // never arrived, or  is so old that its entities
            // have been shoved off the end of the circular
            // buffer in the client system.
        }
        None
    }

    /// `CG_ProcessSnapshots`.
    fn process_snapshots(&mut self, cl: &mut NetClient, w: &mut World) {
        // see what the latest snapshot the client system has is
        let n = cl.snap.message_num;
        if n != self.latest_snapshot_num {
            if n < self.latest_snapshot_num {
                // this should never happen: a new gamestate restarts the numbers
                self.processed_snapshot_num = n;
            }
            self.latest_snapshot_num = n;
        }
        // If we have yet to receive a snapshot, check for it.
        while self.snap.is_none() {
            let Some(s) = self.read_next_snapshot(cl) else { return };
            // set our weapon selection to what
            // the playerstate is currently using
            if s.snap_flags & SNAPFLAG_NOT_ACTIVE == 0 {
                self.set_initial_snapshot(cl, w, s);
            }
        }
        // loop until we either have a valid nextSnap with a serverTime
        // greater than cg.time to interpolate towards, or we run
        // out of available snapshots
        loop {
            // if we don't have a nextframe, try and read a new one in
            if self.next_snap.is_none() {
                let Some(s) = self.read_next_snapshot(cl) else { break };
                self.set_next_snap(s);
                // if time went backwards, we have a level restart
                if self.next_snap.as_ref().unwrap().server_time < self.snap.as_ref().unwrap().server_time {
                    // CG_Error "Server time went backwards": restart from it
                    let s = self.next_snap.take().unwrap();
                    self.set_initial_snapshot(cl, w, s);
                    continue;
                }
            }
            // if our time is < nextFrame's, we have a nice interpolating state
            let (st, nt) = (self.snap.as_ref().unwrap().server_time, self.next_snap.as_ref().unwrap().server_time);
            if self.time >= st && self.time < nt {
                break;
            }
            // we have passed the transition from nextFrame to frame
            self.transition_snapshot(cl, w);
        }
        // assert our valid conditions upon exiting
        let st = self.snap.as_ref().unwrap().server_time;
        if self.time < st {
            // this can happen right after a vid_restart
            self.time = st;
        }
    }

    /// `CG_SetInitialSnapshot`.
    fn set_initial_snapshot(&mut self, cl: &mut NetClient, w: &mut World, snap: Snapshot) {
        let pnum = snap.ps.client_num.clamp(0, MAX_GENTITIES - 1) as usize;
        self.entities[pnum].current_state = svgame::player_state_to_entity_state(&snap.ps, false);
        let seq = snap.server_command_sequence;
        let ents = snap.entities.clone();
        self.snap = Some(snap);
        // execute any server string commands before transitioning entities
        self.execute_new_server_commands(cl, w, seq);
        // CG_Respawn
        self.valid_pps = false;
        for s in ents {
            let cent = &mut self.entities[s.number as usize];
            cent.current_state = s;
            cent.interpolate = false;
            cent.current_valid = true;
            self.reset_entity(s.number as usize);
            // check for events
            self.check_events(w, s.number as usize);
        }
    }

    /// `CG_SetNextSnap`.
    fn set_next_snap(&mut self, snap: Snapshot) {
        let pnum = snap.ps.client_num.clamp(0, MAX_GENTITIES - 1) as usize;
        self.entities[pnum].next_state = svgame::player_state_to_entity_state(&snap.ps, false);
        if let Some(s) = self.snap.as_ref() {
            let cn = s.ps.client_num.clamp(0, MAX_GENTITIES - 1) as usize;
            self.entities[cn].interpolate = true;
        }
        // check for extrapolation errors
        for es in &snap.entities {
            let cent = &mut self.entities[es.number as usize];
            cent.next_state = *es;
            // if this frame is a teleport, or the entity wasn't in the
            // previous frame, don't interpolate
            cent.interpolate = cent.current_valid && (cent.current_state.e_flags ^ es.e_flags) & k::EF_TELEPORT_BIT == 0;
        }
        let cur = self.snap.as_ref().unwrap();
        // if the next frame is a teleport for the playerstate, we
        // can't interpolate during demos
        self.next_frame_teleport = (snap.ps.e_flags ^ cur.ps.e_flags) & k::EF_TELEPORT_BIT != 0;
        // if changing follow mode, don't interpolate
        if snap.ps.client_num != cur.ps.client_num {
            self.next_frame_teleport = true;
        }
        // if changing server restarts, don't interpolate
        if (snap.snap_flags ^ cur.snap_flags) & SNAPFLAG_SERVERCOUNT != 0 {
            self.next_frame_teleport = true;
        }
        self.next_snap = Some(snap);
    }

    /// `CG_TransitionSnapshot`.
    fn transition_snapshot(&mut self, cl: &mut NetClient, w: &mut World) {
        let next = self.next_snap.take().unwrap();
        // execute any server string commands before transitioning entities
        self.execute_new_server_commands(cl, w, next.server_command_sequence);
        // clear the currentValid flag for all entities in the existing snapshot
        if let Some(old) = self.snap.as_ref() {
            for e in &old.entities {
                self.entities[e.number as usize].current_valid = false;
            }
        }
        // move nextSnap to snap and do the transitions
        let old = self.snap.replace(next);
        let snap = self.snap.as_ref().unwrap();
        let pnum = snap.ps.client_num.clamp(0, MAX_GENTITIES - 1) as usize;
        self.entities[pnum].current_state = svgame::player_state_to_entity_state(&snap.ps, false);
        self.entities[pnum].interpolate = false;
        let (nums, st): (Vec<usize>, i32) = (snap.entities.iter().map(|e| e.number as usize).collect(), snap.server_time);
        for n in nums {
            // CG_TransitionEntity
            let cent = &mut self.entities[n];
            cent.current_state = cent.next_state;
            cent.current_valid = true;
            // reset if the entity wasn't in the last frame or was teleported
            if !cent.interpolate {
                self.reset_entity(n);
            }
            // clear the next state.  if will be set by the next CG_SetNextSnap
            self.entities[n].interpolate = false;
            // check for events
            self.check_events(w, n);
            self.entities[n].snapshot_time = st;
        }
        // check for playerstate transition events
        if let Some(old) = old {
            let (ops, ps) = (old.ps, self.snap.as_ref().unwrap().ps);
            // teleporting checks are irrespective of prediction
            if (ps.e_flags ^ ops.e_flags) & k::EF_TELEPORT_BIT != 0 {
                self.this_frame_teleport = true; // will be cleared by prediction code
            }
            // if we are not doing client side movement prediction for any
            // reason, then the client events and view changes will be issued now
            if ps.pm_flags & k::PMF_FOLLOW != 0 || self.nopredict {
                self.transition_player_state(w, &ps, &ops);
            }
        }
    }

    /// `CG_ResetEntity`.
    fn reset_entity(&mut self, n: usize) {
        let t = self.time;
        let cent = &mut self.entities[n];
        // if the previous snapshot this entity was updated in is at least
        // an event window back in time then we can reset the previous event
        if cent.snapshot_time < t - k::EVENT_VALID_MSEC {
            cent.previous_event = 0;
        }
        cent.lerp_origin = cent.current_state.origin;
        cent.lerp_angles = cent.current_state.angles;
    }

    /// `CG_CheckEvents`: event-only entities fire once; in a 1.1 game also
    /// the event a regular entity carries (`s.event` changed).
    fn check_events(&mut self, w: &mut World, n: usize) {
        let cent = &mut self.entities[n];
        if self.vanilla && cent.current_state.e_type < ET_EVENTS {
            let ev = cent.current_state.event;
            if ev == cent.previous_event {
                return;
            }
            cent.previous_event = ev;
            if ev & !k::EV_EVENT_BITS == 0 {
                return;
            }
            // calculate the position at exactly the frame time
            let st = self.snap.as_ref().map(|s| s.server_time).unwrap_or(self.time);
            let origin = bg::evaluate_trajectory(&cent.current_state.pos, st);
            if let Some(mut e) = super::v11::entity_event_from_11(&cent.current_state, n, &origin) {
                e.time = self.time;
                w.events.push(e);
                self.fired += 1;
            }
            return;
        }
        if cent.current_state.e_type > ET_EVENTS {
            if cent.previous_event != 0 {
                return; // already fired
            }
            cent.previous_event = 1;
            if let Some(mut ev) = svgame::event_from_state(&cent.current_state) {
                ev.time = self.time;
                w.events.push(ev);
                self.fired += 1;
            }
        }
    }

    // ============================================================ prediction

    /// `CG_PredictPlayerState`.
    fn predict_player_state(&mut self, cl: &NetClient, w: &mut World) {
        let snap = self.snap.as_ref().unwrap();
        // if this is the first frame we must guarantee
        // predictedPlayerState is valid even if there is some
        // other error condition
        if !self.valid_pps {
            self.valid_pps = true;
            self.predicted_player_state = snap.ps;
        }
        // non-predicting local movement will grab the latest angles
        if snap.ps.pm_flags & k::PMF_FOLLOW != 0 || self.nopredict {
            self.interpolate_player_state();
            return;
        }
        let tracemask = if self.vanilla {
            // 1.1's CG_PredictPlayerState
            let mut m = if self.predicted_player_state.pm_type == PmType::Dead as i32 { k::MASK_PLAYERSOLID & !(contents::BODY as i32) } else { k::MASK_PLAYERSOLID };
            if snap.ps.persistant[Pers::Specstate as usize] != SpectatorState::Not as i32 {
                m &= !(contents::BODY as i32); // spectators can fly through bodies
            }
            m
        } else if self.predicted_player_state.pm_type == PmType::Dead as i32 || snap.ps.persistant[Pers::Specstate as usize] != SpectatorState::Not as i32 {
            // spectators can fly through bodies
            k::MASK_DEADSOLID
        } else {
            k::MASK_PLAYERSOLID
        };
        // save the state before the pmove so we can detect transitions
        let old_ps = self.predicted_player_state;
        let current = cl.cmd_number;
        // if we don't have the commands right after the snapshot, we
        // can't accurately predict a current position, so just freeze at
        // the last good position we had
        let oldest = cl.cmds[(current - CMD_BACKUP as i32 + 1) as usize & CMD_MASK];
        if oldest.server_time > snap.ps.command_time && oldest.server_time < self.time {
            // special check for map_restart: exceeded PACKET_BACKUP on commands
            return;
        }
        // get the latest command so we can know which commands are from previous map_restarts
        let latest = cl.cmds[current as usize & CMD_MASK];
        // get the most recent information we have, even if
        // the server time is beyond our current cg.time,
        // because predicted player positions are going to
        // be ahead of everything else anyway
        match self.next_snap.as_ref() {
            Some(n) if !self.next_frame_teleport && !self.this_frame_teleport => {
                self.predicted_player_state = n.ps;
                self.physics_time = n.server_time;
            }
            _ => {
                self.predicted_player_state = snap.ps;
                self.physics_time = snap.server_time;
            }
        }
        // run cmds
        let mut moved = false;
        for cmd_num in current - CMD_BACKUP as i32 + 1..=current {
            // get the command
            let cmd = cl.cmds[cmd_num as usize & CMD_MASK];
            // don't do anything if the time is before the snapshot player time
            if cmd.server_time <= self.predicted_player_state.command_time {
                continue;
            }
            // don't do anything if the command was from a previous map_restart
            if cmd.server_time > latest.server_time {
                continue;
            }
            // check for a prediction error from last frame
            // on a lan, this will often be the exact value
            // from the snapshot, but on a wan we will have
            // to predict several commands to get to the point
            // we want to compare
            if self.predicted_player_state.command_time == old_ps.command_time {
                if self.this_frame_teleport {
                    // a teleport will not cause an error decay
                    self.predicted_error = [0.0; 3];
                    self.this_frame_teleport = false;
                } else {
                    let adjusted = self.adjust_position_for_mover(&self.predicted_player_state.origin, self.predicted_player_state.ground_entity_num, self.physics_time, self.old_time);
                    let delta = sub(&old_ps.origin, &adjusted);
                    let len = length(&delta);
                    if len > 0.1 {
                        self.misses.push(len);
                        if self.show_miss {
                            self.print_out.push_str(&format!("Prediction miss: {len}\n"));
                        }
                        if self.error_decay > 0.0 {
                            let t = self.time - self.predicted_error_time;
                            let f = ((self.error_decay - t as f32) / self.error_decay).max(0.0);
                            self.predicted_error = scale(&self.predicted_error, f);
                        } else {
                            self.predicted_error = [0.0; 3];
                        }
                        self.predicted_error = add(&delta, &self.predicted_error);
                        self.predicted_error_time = self.old_time;
                    }
                }
            }
            if self.vanilla {
                // 1.1's movement on the 1.1 playerState
                let mut ps = super::v11::ps_to_11(&self.predicted_player_state);
                {
                    let mut pm = super::pmove11::Pmove11::new(&mut ps, cmd, &*w, &mut self.rng);
                    pm.tracemask = tracemask;
                    pm.pmove();
                }
                self.predicted_player_state = super::v11::ps_from_11(&ps);
            } else {
                let mut ps = self.predicted_player_state;
                let mut pmext = self.pmext;
                {
                    let mut pm = Pmove::new(&mut ps, &mut pmext, cmd, &*w, &mut self.rng);
                    pm.tracemask = tracemask;
                    pm.pmove();
                }
                self.predicted_player_state = ps;
                self.pmext = pmext;
            }
            moved = true;
        }
        let _ = moved;
        // adjust for the movement of the groundentity
        self.predicted_player_state.origin = self.adjust_position_for_mover(&self.predicted_player_state.origin, self.predicted_player_state.ground_entity_num, self.physics_time, self.time);
        // fire events and other transition triggered things
        self.transition_player_state(w, &self.predicted_player_state.clone(), &old_ps);
    }

    /// `CG_InterpolatePlayerState` (following another player).
    fn interpolate_player_state(&mut self) {
        let prev = self.snap.as_ref().unwrap();
        let mut out = prev.ps;
        if let Some(next) = self.next_snap.as_ref().filter(|n| !self.next_frame_teleport && n.server_time > prev.server_time) {
            let f = (self.time - prev.server_time) as f32 / (next.server_time - prev.server_time) as f32;
            let mut i = next.ps.bob_cycle;
            if i < prev.ps.bob_cycle {
                i += 256; // handle wraparound
            }
            out.bob_cycle = prev.ps.bob_cycle + (f * (i - prev.ps.bob_cycle) as f32) as i32;
            for k_ in 0..3 {
                out.origin[k_] = prev.ps.origin[k_] + f * (next.ps.origin[k_] - prev.ps.origin[k_]);
                out.viewangles[k_] = lerp_angle(prev.ps.viewangles[k_], next.ps.viewangles[k_], f);
                out.velocity[k_] = prev.ps.velocity[k_] + f * (next.ps.velocity[k_] - prev.ps.velocity[k_]);
            }
        }
        self.predicted_player_state = out;
    }

    /// `CG_AdjustPositionForMover`.
    fn adjust_position_for_mover(&self, input: &Vec3, mover: i32, from_time: i32, to_time: i32) -> Vec3 {
        if mover <= 0 || mover >= k::ENTITYNUM_MAX_NORMAL {
            return *input;
        }
        let cent = &self.entities[mover as usize];
        if cent.current_state.e_type != svgame::ET_MOVER {
            return *input;
        }
        let old_origin = bg::evaluate_trajectory(&cent.current_state.pos, from_time);
        let origin = bg::evaluate_trajectory(&cent.current_state.pos, to_time);
        let delta = sub(&origin, &old_origin);
        // FIXME: origin change when on a rotating object
        add(input, &delta)
    }

    /// `CG_TransitionPlayerState`'s event half: `CG_CheckPlayerstateEvents`
    /// then `CG_CheckChangedPredictableEvents` (the view effects of the
    /// transition are the presentation's, view.rs).
    fn transition_player_state(&mut self, w: &mut World, ps: &PlayerState, ops: &PlayerState) {
        let fire = |cg: &mut Cgame, w: &mut World, event: i32, parm: i32| {
            if let Some(e) = Event::from_raw(event as u8) {
                w.events.push(GameEvent {
                    time: cg.time,
                    event: e,
                    origin: ps.origin,
                    origin2: ps.grapple_point,
                    entity: ps.client_num,
                    other: -1,
                    parm,
                    weapon: ps.weapon,
                    generic1: ps.generic1,
                    charge: ps.torso_anim,
                    predictable: true,
                });
                cg.fired += 1;
            }
        };
        // respawning: no events from the previous life
        if ps.client_num != ops.client_num {
            self.event_sequence = ps.event_sequence;
            return;
        }
        if ps.external_event != 0 && ps.external_event != ops.external_event {
            fire(self, w, ps.external_event, ps.external_event_parm);
        }
        // go through the predictable events buffer
        let m = MAX_PS_EVENTS as i32;
        for i in ps.event_sequence - m..ps.event_sequence {
            // if we have a new predictable event
            if i >= ops.event_sequence
                // or the server told us to play another event instead of a predicted event we already issued
                // or something the server told us changed our prediction causing a different event
                || (i > ops.event_sequence - m && ps.events[(i & (m - 1)) as usize] != ops.events[(i & (m - 1)) as usize])
            {
                let event = ps.events[(i & (m - 1)) as usize];
                fire(self, w, event, ps.event_parms[(i & (m - 1)) as usize]);
                self.predictable_events[i as usize & (MAX_PREDICTED_EVENTS - 1)] = event;
                self.event_sequence += 1;
            }
        }
        // CG_CheckChangedPredictableEvents (with the snapshot's ps)
        if let Some(sps) = self.snap.as_ref().map(|s| s.ps) {
            for i in sps.event_sequence - m..sps.event_sequence {
                if i >= self.event_sequence {
                    continue;
                }
                // if this event is not further back in than the maximum predictable events we remember
                if i > self.event_sequence - MAX_PREDICTED_EVENTS as i32 {
                    let event = sps.events[(i & (m - 1)) as usize];
                    // if the new playerstate event is different from a previously predicted one
                    if event != self.predictable_events[i as usize & (MAX_PREDICTED_EVENTS - 1)] {
                        fire(self, w, event, sps.event_parms[(i & (m - 1)) as usize]);
                        self.predictable_events[i as usize & (MAX_PREDICTED_EVENTS - 1)] = event;
                    }
                }
            }
        }
    }

    // ============================================================ entities

    /// `CG_CalcEntityLerpPositions` for every entity of the snapshot, then
    /// the view World's entities and clients from them.
    fn add_packet_entities(&mut self, w: &mut World) {
        let snap = self.snap.as_ref().unwrap();
        let st = snap.server_time;
        let nst = self.next_snap.as_ref().map(|n| n.server_time);
        let me = snap.ps.client_num.clamp(0, MAX_GENTITIES - 1) as usize;
        let mut present = vec![false; MAX_GENTITIES as usize];
        let nums: Vec<usize> = snap.entities.iter().map(|e| e.number as usize).collect();
        for &n in &nums {
            let f = self.frame_interpolation;
            let t = self.time;
            let cent = &mut self.entities[n];
            let s = cent.current_state;
            // make sure the clients use TR_INTERPOLATE
            if cent.interpolate && (s.pos.tr_type == Traj::Interpolate as i32 || (s.pos.tr_type == Traj::LinearStop as i32 && n < MAX_CLIENTS)) && nst.is_some() {
                // CG_InterpolateEntityPosition
                let cur = bg::evaluate_trajectory(&cent.current_state.pos, st);
                let next = bg::evaluate_trajectory(&cent.next_state.pos, nst.unwrap());
                for k_ in 0..3 {
                    cent.lerp_origin[k_] = cur[k_] + f * (next[k_] - cur[k_]);
                }
                let cur = bg::evaluate_trajectory(&cent.current_state.apos, st);
                let next = bg::evaluate_trajectory(&cent.next_state.apos, nst.unwrap());
                for k_ in 0..3 {
                    cent.lerp_angles[k_] = lerp_angle(cur[k_], next[k_], f);
                }
            } else {
                // just use the current frame and evaluate as best we can
                cent.lerp_origin = bg::evaluate_trajectory(&s.pos, t);
                cent.lerp_angles = bg::evaluate_trajectory(&s.apos, t);
            }
            // adjust for riding a mover
            if s.e_type < ET_EVENTS {
                let o = self.entities[n].lerp_origin;
                let a = self.adjust_position_for_mover(&o, s.ground_entity_num, st, t);
                self.entities[n].lerp_origin = a;
            }
            present[n] = true;
        }
        // the view World: drop what left the snapshot
        let mut top = MAX_CLIENTS;
        let mut zaps = Vec::new();
        for n in 0..MAX_GENTITIES as usize - 2 {
            if !present[n] || n == me {
                if n != me && (w.ents[n].inuse || n < MAX_CLIENTS) {
                    w.ents[n].inuse = false;
                    w.ents[n].linked = false;
                }
                continue;
            }
            let cent = &self.entities[n];
            let s = cent.current_state;
            if s.e_type >= ET_EVENTS {
                // event entities are not drawn; their event already fired
                w.ents[n].inuse = false;
                w.ents[n].linked = false;
                continue;
            }
            if s.e_type == ET_LEV2_ZAP_CHAIN {
                let nums = svgame::unpack_entity_numbers(&s, svgame::MAX_NUM_PACKED_ENTITY_NUMS);
                if nums.len() >= 2 {
                    zaps.push(Zap { creator: nums[0] as usize, targets: nums[1..].iter().map(|&x| x as usize).collect(), distances: vec![0.0; nums.len() - 1], time_to_live: 0 });
                }
                w.ents[n].inuse = false;
                w.ents[n].linked = false;
                continue;
            }
            apply_entity(w, n, &s, &cent.lerp_origin, &cent.lerp_angles);
            if n < MAX_CLIENTS && s.e_type == svgame::ET_PLAYER {
                self.apply_remote_player(w, n, &s);
            } else if n < MAX_CLIENTS {
                // a spectator's entity
                w.clients[n].spectator = SpectatorState::Free as i32;
            }
            top = top.max(n + 1);
        }
        w.num_entities = top.max(me + 1).max(w.num_entities.min(MAX_GENTITIES as usize));
        self.zaps = zaps;
        w.zaps = self.zaps.clone();
        // the target_location chain (Team_GetLocation / CG_GetPlayerLocation)
        let mut head = None;
        for n in (MAX_CLIENTS..w.num_entities).rev() {
            if w.ents[n].inuse && w.ents[n].etype == EType::Location {
                w.ents[n].next_train = head;
                head = Some(n);
            }
        }
        w.level.location_head = head;
    }

    /// A player that is not us: its gclient as far as the entity tells.
    fn apply_remote_player(&self, w: &mut World, n: usize, s: &EntityState) {
        let cent = &self.entities[n];
        let c = &mut w.clients[n];
        let ps = &mut c.ps;
        ps.client_num = n as i32;
        ps.origin = cent.lerp_origin;
        ps.viewangles = cent.lerp_angles;
        ps.velocity = s.pos.tr_delta;
        ps.weapon = s.weapon;
        ps.generic1 = s.generic1;
        ps.legs_anim = s.legs_anim;
        ps.torso_anim = s.torso_anim;
        ps.weapon_anim = s.weapon_anim;
        ps.e_flags = s.e_flags;
        ps.movement_dir = s.time2;
        ps.grapple_point = s.angles2;
        ps.ground_entity_num = s.ground_entity_num;
        ps.loop_sound = s.loop_sound;
        ps.other_entity_num = s.other_entity_num;
        ps.stats[Stat::Team as usize] = s.misc & 0xff;
        ps.stats[Stat::Class as usize] = (s.misc >> 8) & 0xff;
        ps.stats[Stat::Items as usize] = s.modelindex;
        ps.stats[Stat::Activeitems as usize] = s.modelindex2;
        let class = ClassId::from_num(((s.misc >> 8) & 0xff) as u8).unwrap_or(ClassId::Spectator);
        let max = if self.vanilla { super::bg11::health(super::v11::class_to_11(class as i32)) } else { class.def().health };
        ps.stats[Stat::MaxHealth as usize] = max;
        let dead = s.e_flags & k::EF_DEAD != 0;
        // tinfo health for team mates, else alive / dead
        let health = self.team_info.get(n).copied().flatten().map(|t| t.1).filter(|_| !dead).unwrap_or(if dead { 0 } else { max.max(1) });
        ps.stats[Stat::Health as usize] = if dead { 0 } else { health.max(1) };
        ps.pm_type = if dead { PmType::Dead as i32 } else { PmType::Normal as i32 };
        ps.persistant[Pers::Specstate as usize] = SpectatorState::Not as i32;
        c.spectator = SpectatorState::Not as i32;
        if c.team == Team::None {
            c.team = Team::from_raw((s.misc & 0xff) as u8).unwrap_or(Team::None);
        }
    }

    /// The local player: the predicted playerstate with the prediction
    /// error decayed into the drawn origin (CG_CalcViewValues).
    fn write_local_player(&mut self, w: &mut World) {
        let mut ps = self.predicted_player_state;
        if self.vanilla {
            // what 1.1 sends in CS_BUILDPOINTS / CS_SPAWNS (cg_draw.c draws
            // the team's build points; legacy keeps them in persistant[])
            let (abp, _, hbp, _, _) = self.bp11;
            let team = ps.stats[Stat::Team as usize];
            ps.persistant[Pers::Bp as usize] = if team == Team::Aliens as i32 { abp } else if team == Team::Humans as i32 { hbp } else { 0 };
            ps.persistant[Pers::Markedbp as usize] = 0;
            ps.persistant[Pers::Spawns as usize] = if team == Team::Aliens as i32 { self.spawns11.0 } else if team == Team::Humans as i32 { self.spawns11.1 } else { 0 };
        }
        // smooth out the prediction error
        let t = self.time - self.predicted_error_time;
        let f = (self.error_decay - t as f32) / self.error_decay.max(1.0);
        if f > 0.0 && f < 1.0 {
            ps.origin = ma(&ps.origin, f, &self.predicted_error);
        }
        let me = self.snap.as_ref().map(|s| s.ps.client_num).unwrap_or(self.client_num).clamp(0, MAX_CLIENTS as i32 - 1) as usize;
        let own = self.client_num.clamp(0, MAX_CLIENTS as i32 - 1) as usize;
        // following: our client shows the followed player's playerstate
        // (SpectatorClientEndFrame), like the single-process World
        // playerState_t.ping is not sent in snapshots (msg.c has no field for
        // it): keep the server's figure from the last "scores"
        let ping = w.clients[own].ps.ping;
        w.clients[own].ps = ps;
        w.clients[own].ps.ping = ping;
        w.clients[own].connected = true;
        w.clients[own].credit = ps.persistant[Pers::Credit as usize];
        w.clients[own].score = ps.persistant[Pers::Score as usize].max(w.clients[own].score.min(ps.persistant[Pers::Score as usize]));
        w.clients[own].spectator = ps.persistant[Pers::Specstate as usize];
        w.clients[own].pmext = self.pmext;
        if ps.pm_flags & k::PMF_FOLLOW != 0 {
            w.clients[own].spectator_client = ps.client_num;
        }
        // our entity: BG_PlayerStateToEntityState of the predicted state
        let s = svgame::player_state_to_entity_state(&ps, false);
        apply_entity(w, own, &s, &ps.origin, &ps.viewangles);
        if s.e_type != svgame::ET_PLAYER {
            w.ents[own].linked = false;
        }
        let _ = me;
        w.ents[own].client = true;
        let (mins, maxs, ..) = class_bbox(w.vanilla, ps.stats[Stat::Class as usize]);
        w.ents[own].mins = mins;
        w.ents[own].maxs = maxs;
        w.ents[own].contents = if ps.stats[Stat::Health as usize] > 0 { contents::BODY as i32 } else { 0 };
    }

    // ============================================================ server commands

    /// `CG_ExecuteNewServerCommands`.
    fn execute_new_server_commands(&mut self, cl: &mut NetClient, w: &mut World, latest: i32) {
        while self.server_command_sequence < latest {
            self.server_command_sequence += 1;
            if let Some(argv) = cl.get_server_command(self.server_command_sequence, &mut self.big_cs) {
                self.server_command(cl, w, &argv);
            }
            // a 1.1 server command that became several legacy ones (dec040)
            while let Some(argv) = cl.take_pending_server_command() {
                self.server_command(cl, w, &argv);
            }
        }
    }

    /// `CG_ServerCommand`.
    fn server_command(&mut self, cl: &NetClient, w: &mut World, argv: &[String]) {
        let a = |i: usize| argv.get(i).map(|s| s.as_str()).unwrap_or("");
        let me = self.client_num.clamp(0, MAX_CLIENTS as i32 - 1) as usize;
        let t = self.time;
        match a(0) {
            "cp" => w.center_prints.push((t, None, Some(me), a(1).to_string())),
            "print" => {
                let s = a(1).trim_end_matches('\n').to_string();
                self.print_out.push_str(a(1));
                w.prints.push((t, None, Some(me), s));
            }
            "chat" => {
                // CG_Chat_f -> CG_Say
                let from = super::spawn::c_atoi(a(1));
                let mode = svgame::say_mode_from_num(super::spawn::c_atoi(a(2)));
                let from = if from < 0 || from as usize >= MAX_CLIENTS { super::world::CONSOLE } else { from as usize };
                w.chat.push((t, me, from, mode, a(3).to_string()));
                let _ = SayMode::All;
            }
            "cs" => self.cs_dirty = true,
            "scores" => self.parse_scores(w, argv),
            "tinfo" => self.parse_team_info(argv),
            "servermenu" => {
                // the port's notice text (dec033)
                if argv.len() >= 3 {
                    w.clients[me].message = Some((t, a(2).to_string()));
                    w.clients[me].wants_menu = super::spawn::c_atoi(a(3)) != 0;
                }
            }
            "poisoncloud" => w.clients[me].last_poison_clouded_time = t,
            "map_restart" => {
                // CG_MapRestart
                self.this_frame_teleport = true;
                self.event_sequence = 0;
                self.predictable_events = [0; MAX_PREDICTED_EVENTS];
            }
            "serverclosemenus" | "cmds" | "voice" | "clientLevelShot" => {}
            other => self.print_out.push_str(&format!("Unknown client game command: {other}\n")),
        }
        let _ = cl;
    }

    /// `CG_ParseScores`.
    fn parse_scores(&mut self, w: &mut World, argv: &[String]) {
        let n = argv.len().saturating_sub(3) / 6;
        let num = |i: usize| super::spawn::c_atoi(argv.get(i).map(|s| s.as_str()).unwrap_or(""));
        self.scores.clear();
        for i in 0..n.min(MAX_CLIENTS) {
            let mut s = Score { client: num(i * 6 + 3).clamp(0, MAX_CLIENTS as i32 - 1) as usize, score: num(i * 6 + 4), ping: num(i * 6 + 5), time: num(i * 6 + 6), weapon: num(i * 6 + 7), upgrade: num(i * 6 + 8) };
            if s.client >= MAX_CLIENTS {
                s.client = 0;
            }
            let c = &mut w.clients[s.client];
            c.score = s.score;
            c.ps.ping = s.ping;
            c.enter_time = w.level.time - s.time * 60000;
            if !w.ents[s.client].inuse && s.client as i32 != self.client_num {
                c.ps.weapon = s.weapon;
                c.ps.stats[Stat::Items as usize] = if (1..32).contains(&s.upgrade) { 1 << s.upgrade } else { 0 };
            }
            self.scores.push(s);
        }
    }

    /// `CG_ParseTeamInfo`.
    fn parse_team_info(&mut self, argv: &[String]) {
        let my_team = self.snap.as_ref().map(|s| s.ps.stats[Stat::Team as usize]).unwrap_or(0);
        let num = |i: usize| super::spawn::c_atoi(argv.get(i).map(|s| s.as_str()).unwrap_or(""));
        let mut i = 1;
        while i < argv.len() {
            let client = num(i);
            if client < 0 || client as usize >= MAX_CLIENTS {
                return;
            }
            let location = num(i + 1);
            let health = num(i + 2);
            let cur = num(i + 3);
            let (upgrade, step) = if my_team == Team::Humans as i32 { (num(i + 4), 5) } else { (0, 4) };
            self.team_info[client as usize] = Some((location, health, cur, upgrade));
            i += step;
        }
    }

    /// `CG_ConfigStringModified` / `CG_ParseServerinfo` / `CG_SetConfigValues`
    /// over the whole gamestate: the view World's tables, clients and level.
    fn apply_configstrings(&mut self, cl: &NetClient, w: &mut World) {
        self.cs_dirty = false;
        let c = &cl.configstrings;
        let t = self.time;
        // CS_SERVERINFO
        let si = &c[cs::SERVERINFO];
        let iv = |k_: &str| super::spawn::c_atoi(&svgame::info_value(si, k_));
        // the server's "protocol" (CVAR_SERVERINFO): a Tremulous 1.1.0 game
        let protocol = svgame::info_value(si, "protocol");
        if !protocol.is_empty() {
            self.vanilla = super::spawn::c_atoi(&protocol) == super::v11::PROTOCOL_VANILLA;
            w.vanilla = self.vanilla;
        }
        w.level.timelimit = iv("timelimit");
        w.level.sudden_death_time = iv("g_suddenDeathTime");
        w.level.mark_deconstruct = iv("g_markDeconstruct");
        w.level.friendly_fire = iv("g_friendlyFire") != 0;
        w.level.cheats = super::spawn::c_atoi(&svgame::info_value(&c[cs::SYSTEMINFO], "sv_cheats")) != 0;
        let mapname = svgame::info_value(si, "mapname");
        if !mapname.is_empty() {
            w.map_name = mapname;
        }
        w.level.start_time = super::spawn::c_atoi(&c[cs::LEVEL_START_TIME]);
        // intermission and the winner
        let inter = super::spawn::c_atoi(&c[cs::INTERMISSION]) != 0;
        if inter && w.level.intermission_time == 0 {
            w.level.intermission_time = t.max(1);
        } else if !inter {
            w.level.intermission_time = 0;
        }
        w.level.winner = match c[cs::WINNER].as_str() {
            "Aliens Win" => Some(Team::Aliens),
            "Humans Win" => Some(Team::Humans),
            _ => None,
        };
        // votes
        for v in 0..3 {
            let vs = &mut w.level.votes[v];
            vs.time = super::spawn::c_atoi(&c[cs::VOTE_TIME + v]);
            vs.display = c[cs::VOTE_STRING + v].clone();
            vs.yes = super::spawn::c_atoi(&c[cs::VOTE_YES + v]);
            vs.no = super::spawn::c_atoi(&c[cs::VOTE_NO + v]);
            vs.caller = c[cs::VOTE_CALLER + v].clone();
        }
        // stages: "stage credits next"
        let stages = |s: &str| -> (i32, i32, i32) {
            let v: Vec<i32> = s.split_whitespace().map(super::spawn::c_atoi).collect();
            (v.first().copied().unwrap_or(0), v.get(1).copied().unwrap_or(0), v.get(2).copied().unwrap_or(0))
        };
        // a 1.1 server's CS_STAGES / CS_BUILDPOINTS / CS_SPAWNS (v11::CS11_*)
        let (mut alien_cs, mut human_cs) = (c[cs::ALIEN_STAGES].clone(), c[cs::HUMAN_STAGES].clone());
        if self.vanilla {
            if let Some((a, h)) = c.get(super::v11::CS11_STAGES).and_then(|s| super::v11::stages_from_11(s)) {
                (alien_cs, human_cs) = (a, h);
            }
            if let Some(s) = c.get(super::v11::CS11_BUILDPOINTS) {
                self.bp11 = super::v11::buildpoints_from_11(s);
            }
            if let Some(s) = c.get(super::v11::CS11_SPAWNS) {
                let v: Vec<i32> = s.split_whitespace().map(super::spawn::c_atoi).collect();
                self.spawns11 = (v.first().copied().unwrap_or(0), v.get(1).copied().unwrap_or(0));
            }
        }
        let (a_st, a_cr, a_nx) = stages(&alien_cs);
        let (h_st, h_cr, h_nx) = stages(&human_cs);
        if !alien_cs.is_empty() {
            if self.alien_stage >= 0 && a_st > self.alien_stage {
                w.level.stage_events.push((Team::Aliens, a_st + 1));
            }
            self.alien_stage = a_st;
            w.level.alien_stage = a_st;
            w.level.alien_credits = a_cr;
            w.level.alien_next_stage = a_nx;
        }
        if !human_cs.is_empty() {
            if self.human_stage >= 0 && h_st > self.human_stage {
                w.level.stage_events.push((Team::Humans, h_st + 1));
            }
            self.human_stage = h_st;
            w.level.human_stage = h_st;
            w.level.human_credits = h_cr;
            w.level.human_next_stage = h_nx;
        }
        // CS_CLIENTS_READY
        let ready = c[cs::CLIENTS_READY].clone();
        // the index tables
        let table = |base: usize, max: usize| -> Vec<String> {
            let mut v = vec![String::new()];
            for i in 1..max {
                v.push(c[base + i].clone());
            }
            while v.len() > 1 && v.last().is_some_and(|s| s.is_empty()) {
                v.pop();
            }
            v
        };
        w.model_names = table(cs::MODELS, cs::MAX_MODELS);
        w.sound_names = table(cs::SOUNDS, cs::MAX_SOUNDS);
        w.shader_names = table(cs::SHADERS, cs::MAX_GAME_SHADERS);
        w.particle_names = table(cs::PARTICLE_SYSTEMS, cs::MAX_GAME_PARTICLE_SYSTEMS);
        w.location_names = table(cs::LOCATIONS, cs::MAX_LOCATIONS);
        // CS_SHADERSTATE "old=new:time@..."
        w.level.shader_remaps = c[cs::SHADERSTATE]
            .split('@')
            .filter_map(|e| {
                let (o, rest) = e.split_once('=')?;
                let (n, tm) = rest.split_once(':')?;
                Some((o.to_string(), n.to_string(), tm.trim().parse().unwrap_or(0.0)))
            })
            .collect();
        // CS_PLAYERS: the clientinfo
        let (mut na, mut nh) = (0, 0);
        for i in 0..MAX_CLIENTS {
            let info = &c[cs::PLAYERS + i];
            let cli = &mut w.clients[i];
            if info.is_empty() {
                if i as i32 != self.client_num {
                    cli.connected = false;
                }
                continue;
            }
            cli.connected = true;
            cli.name = svgame::info_value(info, "n");
            cli.team = Team::from_raw(super::spawn::c_atoi(&svgame::info_value(info, "t")) as u8).unwrap_or(Team::None);
            cli.ready_to_exit = svgame::client_list_contains(&ready, i);
            if i as i32 == self.client_num {
                let ig = svgame::info_value(info, "ig");
                cli.ignored = (0..MAX_CLIENTS).filter(|&j| svgame::client_list_contains(ig.trim(), j)).collect();
            }
            match cli.team {
                Team::Aliens => na += 1,
                Team::Humans => nh += 1,
                Team::None => {}
            }
        }
        w.level.num_alien_clients = na;
        w.level.num_human_clients = nh;
    }

    /// The prediction error currently drawn (length), for tests.
    pub fn current_error(&self) -> f32 {
        let t = self.time - self.predicted_error_time;
        let f = (self.error_decay - t as f32) / self.error_decay.max(1.0);
        if f > 0.0 && f < 1.0 {
            length(&self.predicted_error) * f
        } else {
            0.0
        }
    }
}

/// `LerpAngle`.
pub fn lerp_angle(from: f32, to: f32, frac: f32) -> f32 {
    let mut to = to;
    if to - from > 180.0 {
        to -= 360.0;
    }
    if to - from < -180.0 {
        to += 360.0;
    }
    from + frac * (to - from)
}

/// One snapshot entity onto the view World's gentity (and what the
/// presentation derives from it: buildable, team, health, solid box...).
pub fn apply_entity(w: &mut World, n: usize, s: &EntityState, lerp_origin: &Vec3, lerp_angles: &Vec3) {
    let et = svgame::et_from_num(s.e_type).unwrap_or(EType::General);
    let vanilla = w.vanilla;
    let keep_credits = std::mem::take(&mut w.ents[n].credits);
    let e: &mut Entity = &mut w.ents[n];
    e.inuse = true;
    e.linked = true;
    e.free_after_event = false;
    e.etype = et;
    e.client = n < MAX_CLIENTS;
    e.origin = *lerp_origin;
    e.angles = *lerp_angles;
    e.pos = s.pos;
    e.apos = s.apos;
    e.s_origin = s.origin;
    e.s_angles = s.angles;
    e.origin2 = s.origin2;
    e.angles2 = s.angles2;
    e.e_flags = s.e_flags;
    e.weapon = s.weapon;
    e.generic1 = s.generic1;
    e.legs_anim = s.legs_anim;
    e.torso_anim = s.torso_anim;
    e.misc = s.misc;
    e.frame = s.frame;
    e.s_time = s.time;
    e.s_client_num = s.client_num;
    e.event_parm = s.event_parm;
    e.loop_sound = s.loop_sound;
    e.constant_light = s.constant_light;
    e.model_index = s.modelindex;
    e.model_index2 = s.modelindex2;
    e.ground_entity = s.ground_entity_num;
    // only a missile's otherEntityNum is its owner (trails, impacts); the
    // others carry 0 or a target, and World::trace skips an entity owned by
    // the mover: CG_ClipMoveToEntities skips nothing but skipNumber, so
    // structures and players stay solid to the predicted player
    e.owner = if et == EType::Missile { s.other_entity_num } else { ENTITYNUM_NONE };
    e.credits = keep_credits;
    if e.credits.len() != MAX_CLIENTS {
        e.credits = vec![0; MAX_CLIENTS];
    }
    // the solid box (CG_BuildSolidList / CG_ClipMoveToEntities)
    e.bmodel = s.solid == SOLID_BMODEL;
    if e.bmodel {
        e.contents = contents::SOLID as i32;
        if let Some(m) = w.cm.models.get(s.modelindex.max(0) as usize) {
            e.mins = m.mins;
            e.maxs = m.maxs;
        }
    } else if s.solid != 0 {
        let (mins, maxs) = svgame::solid_box(s.solid);
        e.mins = mins;
        e.maxs = maxs;
        e.contents = contents::BODY as i32;
    } else {
        e.contents = 0;
    }
    e.classname = "entity";
    match et {
        EType::Player => {
            e.classname = "player";
            let (mins, maxs, ..) = class_bbox(vanilla, (s.misc >> 8) & 0xff);
            if s.solid == 0 {
                e.mins = mins;
                e.maxs = maxs;
            }
        }
        EType::Buildable => {
            let b = BuildableId::from_num(s.modelindex.clamp(0, 255) as u8);
            e.buildable = b;
            e.classname = b.map(|b| b.cold().entity_name).unwrap_or("buildable");
            if let Some(b) = b {
                let d = b.def();
                e.team = d.team;
                e.mins = d.mins;
                e.maxs = d.maxs;
                // a 1.1 game's structures have 1.1's boxes (BG_FindBBoxForBuildable)
                if vanilla {
                    if let Some(b11) = super::v11::buildable11_of(b) {
                        e.mins = b11.def().mins;
                        e.maxs = b11.def().maxs;
                    }
                }
            }
            e.health = s.generic1;
            e.spawned = s.e_flags & k::EF_B_SPAWNED != 0;
            e.powered = s.e_flags & k::EF_B_POWERED != 0;
            e.deconstruct = s.e_flags & k::EF_B_MARKED != 0;
            // the barrel aim from s.angles2 and the surface normal (HMGTurret_TrackEnemy's inverse rotation)
            e.turret_aim = if b == Some(BuildableId::Mgturret) {
                let ref_normal = [0.0f32, 0.0, 1.0];
                let o2 = if s.origin2 == [0.0; 3] { ref_normal } else { s.origin2 };
                let mut x_normal = cross(&o2, &ref_normal);
                let rot = if normalize(&mut x_normal) == 0.0 { 0.0 } else { rad2deg((dot(&o2, &ref_normal) as f64).acos()) as f32 };
                let (f, _, _) = angle_vectors(&s.angles2);
                let dt = if rot == 0.0 { f } else { rotate_point_around_vector(&x_normal, &f, -rot) };
                vectoangles(&dt)
            } else {
                s.angles
            };
        }
        EType::Missile => {
            e.classname = "missile";
            let m = MISSILES.iter().position(|m| m.weapon as i32 == s.weapon).and_then(|i| MissileId::from_num(i as u8));
            e.missile = m;
            if let Some(m) = m {
                let sz = m.def().size;
                e.mins = [-sz; 3];
                e.maxs = [sz; 3];
            }
        }
        EType::Corpse => {
            e.classname = "corpse";
            e.team = Team::from_raw(s.modelindex2.clamp(0, 2) as u8).unwrap_or_else(|| ClassId::from_num(s.generic1.clamp(0, 255) as u8).map(|c| c.def().team).unwrap_or(Team::None));
        }
        EType::Mover => e.classname = "func_mover",
        EType::Location => e.classname = "target_location",
        EType::ParticleSystem => e.classname = "misc_particle_system",
        EType::AnimMapObj => e.classname = "misc_anim_model",
        EType::LightFlare => e.classname = "misc_light_flare",
        EType::Speaker => e.classname = "target_speaker",
        EType::Portal => e.classname = "misc_portal_surface",
        _ => {}
    }
    let _ = UpgradeId::Jetpack;
    let _ = Trajectory::default();
}

/// `BG_ClassBoundingBox` of a legacy class number, or 1.1's
/// `BG_FindBBoxForClass` in a protocol 69 game.
pub fn class_bbox(vanilla: bool, class: i32) -> (Vec3, Vec3, Vec3, Vec3, Vec3) {
    if vanilla {
        super::bg11::class_bbox(super::v11::class_to_11(class))
    } else {
        bg::class_bbox(class)
    }
}

#[cfg(test)]
mod server_value_bounds_tests {
    use super::*;
    use crate::gen::maps::MapId;

    fn argv(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    /// Every server command with numbers far outside what this version holds,
    /// for a player number outside the range too: nothing panics.
    #[test]
    fn server_commands_with_out_of_range_numbers_do_not_panic() {
        let cl = NetClient::new(1);
        for me in [0, 63, 64, 127, 1023, -5, i32::MAX] {
            let mut cg = Cgame::new(me);
            let mut w = World::client_view(MapId::Arena);
            for a in [
                argv(&["servermenu", "x", "hello", "1"]),
                argv(&["poisoncloud"]),
                argv(&["chat", "127", "99", "hi"]),
                argv(&["chat", "-3", "-9", "hi"]),
                argv(&["chat", "2147483647", "2147483647", "hi"]),
                argv(&["scores", "1", "2", "3", "127", "1", "1", "1", "1", "99", "9999", "5", "5", "5", "5", "5", "-1", "-1", "-1", "-1", "-1", "-1"]),
                argv(&["scores", "1", "2", "3", "5", "1", "1", "1", "1", "2147483647"]),
                argv(&["tinfo", "127", "1", "1", "1", "9999", "1", "1", "1", "1"]),
                argv(&["tinfo", "-1", "1"]),
            ] {
                cg.server_command(&cl, &mut w, &a);
            }
        }
    }

    #[test]
    fn a_score_row_for_a_number_beyond_the_table_is_ignored_or_clamped() {
        let cl = NetClient::new(1);
        let mut cg = Cgame::new(0);
        let mut w = World::client_view(MapId::Arena);
        cg.server_command(&cl, &mut w, &argv(&["scores", "1", "2", "3", "127", "10", "20", "30", "1", "40"]));
        assert!(cg.scores.iter().all(|s| s.client < MAX_CLIENTS));
    }

    #[test]
    fn client_lists_with_multibyte_text_are_not_sliced() {
        // 16 bytes, but byte 8 is inside a character
        let s = "aaaaaaa\u{e9}aaaaaaa";
        assert_eq!(s.len(), 16);
        for i in [0, 1, 40, 63, 64, 127, usize::MAX] {
            assert!(!svgame::client_list_contains(s, i));
        }
        assert!(svgame::client_list_contains("0000000000000001", 0));
    }

    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self) -> u32 {
            self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (self.0 >> 33) as u32
        }
        /// small, bit-width-ish, or extreme
        fn v(&mut self) -> i32 {
            match self.next() % 5 {
                0 => (self.next() % 8) as i32,
                1 => (self.next() % 256) as i32,
                2 => (self.next() % 1024) as i32,
                3 => self.next() as i32,
                _ => [i32::MIN, i32::MAX, -1, 0, 127, 128, 255, 1023][(self.next() % 8) as usize],
            }
        }
    }

    /// Entity states and player states with every number random, extreme ones
    /// included, through the view World: nothing panics.
    #[test]
    fn random_server_entities_and_player_state_do_not_panic() {
        let mut r = Lcg(0x5eed);
        for round in 0..300 {
            let mut cg = Cgame::new(if round % 3 == 0 { r.v() } else { (r.next() % 64) as i32 });
            let mut w = World::client_view(MapId::Arena);
            let mut snap = Snapshot::default();
            snap.ps.client_num = r.v();
            for s in snap.ps.stats.iter_mut() {
                *s = r.v();
            }
            for p in snap.ps.persistant.iter_mut() {
                *p = r.v();
            }
            snap.ps.weapon = r.v();
            snap.ps.generic1 = r.v();
            snap.ps.ground_entity_num = r.v();
            snap.ps.pm_flags = r.v();
            snap.ps.pm_type = r.v();
            let mut used = std::collections::HashSet::new();
            for _ in 0..40 {
                let n = (r.next() % 1022) as i32;
                if !used.insert(n) {
                    continue;
                }
                let mut s = EntityState::default();
                s.number = n;
                s.e_type = (r.next() % 40) as i32;
                if r.next() % 3 == 0 {
                    s.e_type = svgame::ET_PLAYER;
                }
                s.e_flags = r.v();
                s.pos.tr_type = (r.next() % 10) as i32;
                s.apos.tr_type = (r.next() % 10) as i32;
                s.time = r.v();
                s.time2 = r.v();
                s.other_entity_num = r.v();
                s.other_entity_num2 = r.v();
                s.ground_entity_num = r.v();
                s.constant_light = r.v();
                s.loop_sound = r.v();
                s.modelindex = r.v();
                s.modelindex2 = r.v();
                s.client_num = r.v();
                s.frame = r.v();
                s.solid = r.v();
                s.event = r.v();
                s.event_parm = r.v();
                s.misc = r.v();
                s.weapon = r.v();
                s.legs_anim = r.v();
                s.torso_anim = r.v();
                s.weapon_anim = r.v();
                s.generic1 = r.v();
                cg.entities[n as usize].current_state = s;
                cg.entities[n as usize].next_state = s;
                cg.entities[n as usize].interpolate = r.next() % 2 == 0;
                snap.entities.push(s);
            }
            cg.vanilla = round % 4 == 0;
            cg.time = 1000;
            snap.server_time = 900;
            cg.snap = Some(snap);
            for n in 0..1023 {
                cg.check_events(&mut w, n);
            }
            cg.add_packet_entities(&mut w);
            cg.write_local_player(&mut w);
        }
    }
}
