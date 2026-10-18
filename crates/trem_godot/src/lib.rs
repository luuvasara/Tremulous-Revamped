//! GDExtension entry point: registers `TremGame` with Godot.
use godot::prelude::*;

mod actors;
mod assets;
mod audio;
mod bspview;
mod q3shader; // the full shader-script language for map surfaces
mod cinematic;
mod cgcmds;
mod clientcmd;
mod cmd;
mod mapents;
mod marks;
mod menumodel;
mod menus;
#[cfg(test)]
mod testdata; // game data for the unit tests: env var or the tracked pk3s, never a silent skip
mod md3;
mod models;
mod game;
mod geom;
mod hostfs;
mod hud;
mod input;
mod keys;
mod vidsnd;
mod console;
mod devcmds;
mod joystick;
mod misc;
mod userinfo;
mod view;
mod effects;
mod particles;
mod trails;
mod voipsnd; // wt-b VoIP
mod voiphud; // the port's voice-chat HUD (dec053)
mod avicap; // wt-b video capture
mod gfx; // graphics modes (vanilla / enhanced)
mod rt; // ray-traced effects of the enhanced mode
mod fmodaudio; // FMOD + Steam Audio sound path
mod hdtex; // 4K texture packs (enhanced mode)
mod ragdoll; // ragdoll corpses (enhanced mode)
mod upnp; // the router's port forwarding for hosted games
mod firewall; // the Windows Firewall check for hosted games (dec047)
mod lamps; // the map's lamps and sun: world-fixed shadows (enhanced mode)

struct TremExtension;

#[gdextension]
unsafe impl ExtensionLibrary for TremExtension {}
