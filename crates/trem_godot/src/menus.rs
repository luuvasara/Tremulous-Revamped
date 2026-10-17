//! The ui module of tremulous-legacy: the ui_shared.c menu system driven by
//! the legacy `ui/*.menu` scripts, drawn by a `TremMenus` Control in the
//! 640x480 virtual screen.
//!
//! * Menus: the lists `ui/menus.txt` (main, news, joinserver, options,
//!   createserver, mod, demo, connect, password, quit, error, download, drop,
//!   serverinfo, findplayer, quitcredit, createfavorite), `ui/ingame.txt`
//!   (ingame, ingame_game, ingame_options, ingame_leave, ingame_help) and
//!   `ui/tremulous.txt` (team select, alien class, human item, builds,
//!   armoury, dialogs, alien upgrade, say) as `UI_Init` loads them, plus
//!   cgame's `teamscore_menu` (ui/hud.txt), parsed with the `.menu` parser
//!   of `hud.rs`.
//! * The main menu is the start screen (`UI_SetActiveMenu`( UIMENU_MAIN )
//!   after the `cinematic splash.RoQ` of Com_Init, cinematic.rs): Play opens
//!   the server browser (the `trap_LAN_*` lists mirror the network
//!   client's: LAN scan, masters, pings, favorites; dec035), Create Server
//!   starts a listen server through TremGame (`net_enabled 1`, `map
//!   <name>`), Leave / disconnect come back here. ITEM_TYPE_MODEL
//!   items are rendered (menumodel.rs), the option cvars act on Godot
//!   (vidsnd.rs), fades / transitions / orbits run, and the menu
//!   `soundLoop` / `playlooped` background track plays (S_StartBackgroundTrack).
//! * Emoticons: legacy's `UI_Text_Paint_Generic` draws `[name]` from
//!   `emoticons/*.tga`; the 1.1.0 data has no emoticons folder, so text
//!   paints as plain text (as the C does with an empty emoticon list).
//! * [`Ui`] is the display context + menu state of ui_shared.c (`Menus[]`,
//!   `menuStack`, `g_editingField`, the capture functions) and the uiInfo /
//!   feeder / uiScript / ownerdraw side of ui_main.c, independent of Godot:
//!   the simulation is read through a [`GameView`] snapshot and the commands
//!   a menu runs come back as [`Out`] values.
//! * Aspect compensation (`Menu_AspectCompensate`) is re-applied whenever the
//!   window aspect changes; painting and hit-testing use the same rects.
//! * Text uses the original bitmap fonts (fonts/fontImage_{20,26,34}.dat:
//!   smallFont / font / bigFont of the assetGlobalDef) with the legacy
//!   `UI_FontForScale` thresholds (ui_smallFont 0.2, ui_bigFont 0.5).
use std::collections::HashMap;

use godot::classes::control::{LayoutPreset, MouseFilter};
use godot::classes::{AudioStreamGenerator, AudioStreamGeneratorPlayback, AudioStreamPlayer, AudioStreamWav, Control, IControl, Image, ImageTexture, Input, InputEvent, InputEventKey, InputEventMouseButton, InputEventMouseMotion, RenderingServer, Texture2D, Time};
use godot::global::Key;
use godot::prelude::*;

use crate::game::TremGame;
use crate::hud::{self, ItemDef, MenuDef, MenuSet, Script, Tok};
use crate::cinematic::{self, Roq};
use crate::menumodel::{MenuModel, ShaderScripts};
use crate::models::Library;
use crate::vidsnd::{self, VidSnd};
use trem_sim::gen::buildables::BuildableId;
use trem_sim::gen::classes::ClassId;
use trem_sim::gen::constants as k;
use trem_sim::gen::enums::{Stat, Team};
use trem_sim::gen::upgrades::UpgradeId;
use trem_sim::gen::weapons::WeaponId;
use trem_sim::gen::buildables11::Buildable11Id;
use trem_sim::gen::classes11::Class11Id;
use trem_sim::gen::upgrades11::Upgrade11Id;
use trem_sim::gen::weapons11::Weapon11Id;
use trem_sim::kernel::bg;
use trem_sim::kernel::{bg11, v11};
use trem_sim::kernel::world::{World, MAX_CLIENTS};

type Rgba = [f32; 4];
type R = [f32; 4];
const WHITE: Rgba = [1.0; 4];

// =====================================================================
// legacy menudef.h / ui_shared.h constants
// =====================================================================

#[allow(dead_code)]
pub const IT_NONE: i32 = 0;
pub const IT_TEXT: i32 = 1;
pub const IT_BUTTON: i32 = 2;
#[allow(dead_code)]
pub const IT_RADIOBUTTON: i32 = 3;
#[allow(dead_code)]
pub const IT_CHECKBOX: i32 = 4;
pub const IT_EDITFIELD: i32 = 5;
pub const IT_SAYFIELD: i32 = 6;
pub const IT_CYCLE: i32 = 7;
pub const IT_LISTBOX: i32 = 8;
pub const IT_COMBOBOX: i32 = 9;
pub const IT_MODEL: i32 = 10;
pub const IT_OWNERDRAW: i32 = 11;
pub const IT_NUMERICFIELD: i32 = 12;
pub const IT_SLIDER: i32 = 13;
pub const IT_YESNO: i32 = 14;
pub const IT_MULTI: i32 = 15;
pub const IT_BIND: i32 = 16;

pub const ALIGN_LEFT: i32 = 0;
pub const ALIGN_CENTER: i32 = 1;
pub const ALIGN_RIGHT: i32 = 2;
pub const ASPECT_NONE: i32 = 3;
pub const VALIGN_BOTTOM: i32 = 0;
pub const VALIGN_CENTER: i32 = 1;
pub const VALIGN_TOP: i32 = 2;

const TEXTSTYLE_BLINK: i32 = 1;
const TEXTSTYLE_SHADOWED: i32 = 3;
const TEXTSTYLE_SHADOWEDMORE: i32 = 6;
const TEXTSTYLE_NEON: i32 = 7;

const BORDER_FULL: i32 = 1;
const BORDER_HORZ: i32 = 2;
const BORDER_VERT: i32 = 3;
const BORDER_KCGRADIENT: i32 = 4;
const STYLE_FILLED: i32 = 1;
const STYLE_GRADIENT: i32 = 2;
const STYLE_SHADER: i32 = 3;
const STYLE_CINEMATIC: i32 = 5;
const LISTBOX_IMAGE: i32 = 1;

// windowDef_t flags
const WINDOW_MOUSEOVER: u32 = 0x1;
const WINDOW_HASFOCUS: u32 = 0x2;
const WINDOW_VISIBLE: u32 = 0x4;
const WINDOW_DECORATION: u32 = 0x10;
const WINDOW_FADINGOUT: u32 = 0x20;
const WINDOW_FADINGIN: u32 = 0x40;
const WINDOW_MOUSEOVERTEXT: u32 = 0x80;
const WINDOW_FORECOLORSET: u32 = 0x200;
const WINDOW_INTRANSITION: u32 = 0x100;
const WINDOW_LB_UPARROW: u32 = 0x800;
const WINDOW_LB_DOWNARROW: u32 = 0x1000;
const WINDOW_LB_THUMB: u32 = 0x2000;
const WINDOW_LB_PGUP: u32 = 0x4000;
const WINDOW_LB_PGDN: u32 = 0x8000;
const WINDOW_ORBITING: u32 = 0x10000;
const WINDOW_OOB_CLICK: u32 = 0x20000;
const WINDOW_WRAPPED: u32 = 0x80000;
const WINDOW_FORCED: u32 = 0x100000;
const WINDOW_POPUP: u32 = 0x200000;
const WINDOW_BACKCOLORSET: u32 = 0x400000;
const LB_FLAGS: u32 = WINDOW_LB_UPARROW | WINDOW_LB_DOWNARROW | WINDOW_LB_THUMB | WINDOW_LB_PGUP | WINDOW_LB_PGDN;

const CVAR_ENABLE: u32 = 1;
const CVAR_DISABLE: u32 = 2;
const CVAR_SHOW: u32 = 4;
const CVAR_HIDE: u32 = 8;

// menudef.h display flags (UI_OwnerDrawVisible)
const UI_SHOW_FAVORITESERVERS: i32 = 0x1;
const UI_SHOW_NOTFAVORITESERVERS: i32 = 0x2;
const UI_SHOW_VOTEACTIVE: i32 = 0x4;
const UI_SHOW_CANVOTE: i32 = 0x8;
const UI_SHOW_TEAMVOTEACTIVE: i32 = 0x10;
const UI_SHOW_CANTEAMVOTE: i32 = 0x20;
const UI_SHOW_NOTSPECTATING: i32 = 0x40;

const ITEM_VALUE_OFFSET: f32 = 8.0;
const MAX_EDITFIELD: i32 = 256;
const MIN_FIELD_WIDTH: i32 = 10;
const EDIT_CURSOR_WIDTH: i32 = 10;
const SCROLLBAR_ARROW_SIZE: f32 = 16.0;
const SLIDER_HEIGHT: f32 = 16.0;
const SLIDER_THUMB_HEIGHT: f32 = 20.0;
const DOUBLE_CLICK_DELAY: i32 = 300;
const SCROLL_TIME_START: i32 = 500;
const SCROLL_TIME_ADJUST: i32 = 150;
const SCROLL_TIME_ADJUSTOFFSET: i32 = 40;
const SCROLL_TIME_FLOOR: i32 = 20;
const BLINK_DIVISOR: i32 = 200;
const PULSE_DIVISOR: f32 = 75.0;
const INDENT_MARKER: u8 = 0x0b;
const MAX_STRING_CHARS: usize = 1024;
const SPECTATORS_PIXELS_PER_SECOND: f32 = 30.0;

/// AssetCache (ui_shared.h ASSET_*) and the assetGlobalDef of ui/ingame.menu.
const ASSET_GRADIENTBAR: &str = "ui/assets/gradientbar2";
const ASSET_SCROLLBAR: &str = "ui/assets/scrollbar";
const ASSET_SCROLLBAR_ARROWDOWN: &str = "ui/assets/scrollbar_arrow_dwn_a";
const ASSET_SCROLLBAR_ARROWUP: &str = "ui/assets/scrollbar_arrow_up_a";
const ASSET_SCROLL_THUMB: &str = "ui/assets/scrollbar_thumb";
const ASSET_SLIDER_BAR: &str = "ui/assets/slider2";
const ASSET_SLIDER_THUMB: &str = "ui/assets/sliderbutt_1";
const ITEM_FOCUS_SOUND: &str = "sound/misc/menu2.wav";
const CURSOR: &str = "ui/assets/3_cursor3";
pub const SCORE_MENU: &str = "teamscore_menu";

/// The menu lists `UI_Load` reads in game (ui/menus.txt is the main menu,
/// which this port replaces with its own map menu).
pub const MENU_LISTS: &[&str] = &["ui/menus.txt", "ui/ingame.txt", "ui/tremulous.txt"];

/// q_shared.h `AS_*` server sources (AS_MPLAYER is no longer used).
pub const AS_GLOBAL: i32 = 0;
pub const AS_MPLAYER: i32 = 1;
pub const AS_LOCAL: i32 = 2;
pub const AS_FAVORITES: i32 = 3;
/// ui_main.c `netSources[]`
const NET_SOURCES: [&str; 4] = ["Internet", "Mplayer", "LAN", "Favorites"];
/// ui_main.c `MonthAbbrev[]`
const MONTH_ABBREV: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
/// ui_local.h limits
const MAX_NEWS_LINES: usize = 50;
const MAX_NEWS_LINEWIDTH: usize = 85;
const MAX_MODS: usize = 64;
const MAX_DEMOS: usize = 256;
const MAX_MOVIES: usize = 256;
const MAX_OTHER_SERVERS: usize = 128;
/// qcommon.h PROTOCOL_VERSION (the "protocol" cvar): the demo extension
/// `dm_<protocol>` (wt-a dec037: 71, as the demos the client records)
const PROTOCOL_VERSION: i32 = 71;
/// cgame's scoreboard menu (ui/hud.txt `teamscore.menu`).
pub const CG_MENU_FILES: &[&str] = &["ui/teamscore.menu"];

/// A key event as ui_shared.c sees it (keycodes.h K_*; `Char` is a
/// `K_CHAR_FLAG` character event).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum K {
    Char(u8),
    Tab,
    Enter,
    KpEnter,
    Escape,
    Backspace,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PgUp,
    PgDn,
    Del,
    Ins,
    Mouse1,
    Mouse2,
    Mouse3,
    WheelUp,
    WheelDown,
    /// any other key (letters, function keys: no default handling)
    Other,
}

impl K {
    fn is_mouse(self) -> bool {
        matches!(self, K::Mouse1 | K::Mouse2 | K::Mouse3)
    }
}

// =====================================================================
// Q3 bitmap fonts (fontInfo_t, RE_RegisterFont's fontImage_<size>.dat)
// =====================================================================

#[derive(Clone, Debug, Default)]
pub struct Glyph {
    pub height: i32,
    pub top: i32,
    pub xskip: i32,
    pub iw: i32,
    pub ih: i32,
    pub s: f32,
    pub t: f32,
    pub s2: f32,
    pub t2: f32,
    pub shader: String,
}

#[derive(Clone, Debug)]
pub struct QFont {
    pub glyphs: Vec<Glyph>,
    pub glyph_scale: f32,
}

/// fontInfo_t: 256 x glyphInfo_t (7 ints, 4 floats, handle, char[32]) + glyphScale + name.
pub fn parse_font_dat(d: &[u8]) -> Option<QFont> {
    if d.len() < 256 * 80 + 4 {
        return None;
    }
    let int = |o: usize| i32::from_le_bytes([d[o], d[o + 1], d[o + 2], d[o + 3]]);
    let flt = |o: usize| f32::from_le_bytes([d[o], d[o + 1], d[o + 2], d[o + 3]]);
    let glyphs = (0..256)
        .map(|i| {
            let b = i * 80;
            let name = &d[b + 48..b + 80];
            let end = name.iter().position(|&c| c == 0).unwrap_or(32);
            Glyph {
                height: int(b),
                top: int(b + 4),
                xskip: int(b + 16),
                iw: int(b + 20),
                ih: int(b + 24),
                s: flt(b + 28),
                t: flt(b + 32),
                s2: flt(b + 36),
                t2: flt(b + 40),
                shader: String::from_utf8_lossy(&name[..end]).into_owned(),
            }
        })
        .collect();
    Some(QFont { glyphs, glyph_scale: flt(256 * 80) })
}

/// g_color_table (legacy q_math.c: ^0 is dark grey).
const COLOR_TABLE: [Rgba; 8] = [[0.2, 0.2, 0.2, 1.0], [1.0, 0.0, 0.0, 1.0], [0.0, 1.0, 0.0, 1.0], [1.0, 1.0, 0.0, 1.0], [0.0, 0.0, 1.0, 1.0], [0.0, 1.0, 1.0, 1.0], [1.0, 0.0, 1.0, 1.0], [1.0, 1.0, 1.0, 1.0]];

/// `Q_IsColorString` (legacy: `^` followed by an alphanumeric).
fn is_color_string(b: &[u8], i: usize) -> bool {
    i + 1 < b.len() && b[i] == b'^' && b[i + 1].is_ascii_alphanumeric()
}

/// `ColorIndex`.
fn color_index(c: u8) -> usize {
    ((c as i32 - '0' as i32) & 7) as usize
}

/// smallFont / font / bigFont of the assetGlobalDef with the
/// `smallFontScale` / `bigFontScale` of the display context.
pub struct Fonts {
    pub small: QFont,
    pub text: QFont,
    pub big: QFont,
    pub small_scale: f32,
    pub big_scale: f32,
}

impl Fonts {
    pub fn new(small: QFont, text: QFont, big: QFont) -> Fonts {
        // ui_smallFont 0.2 / ui_bigFont 0.5 (ui_main.c cvarTable)
        Fonts { small, text, big, small_scale: 0.2, big_scale: 0.5 }
    }
    /// `UI_FontForScale`.
    fn pick(&self, scale: f32) -> &QFont {
        if scale <= self.small_scale {
            &self.small
        } else if scale >= self.big_scale {
            &self.big
        } else {
            &self.text
        }
    }
    /// `UI_Char_Width`: the width of the character at `*i` (advancing past
    /// it). As in the original, the glyph read after the increment is the
    /// one of the *following* character.
    fn char_width(&self, b: &[u8], i: &mut usize, scale: f32, aspect: f32) -> f32 {
        if *i >= b.len() {
            return 0.0;
        }
        if is_color_string(b, *i) {
            *i += 2;
            return 0.0;
        }
        if b[*i] == INDENT_MARKER {
            *i += 1;
            return 0.0;
        }
        let f = self.pick(scale);
        *i += 1;
        let g = &f.glyphs[b.get(*i).copied().unwrap_or(0) as usize];
        g.xskip as f32 * aspect * scale * f.glyph_scale
    }
    /// `UI_Text_Width`.
    pub fn width(&self, text: &str, scale: f32, aspect: f32) -> f32 {
        let b = text.as_bytes();
        let (indent, mut i) = parse_indent(b);
        let mut out = 0.0;
        while i < b.len() {
            out += self.char_width(b, &mut i, scale, aspect);
        }
        out + indent
    }
    /// `UI_Text_Height`.
    pub fn height(&self, text: &str, scale: f32) -> f32 {
        let f = self.pick(scale);
        let b = text.as_bytes();
        let (mut i, mut max) = (0, 0.0f32);
        while i < b.len() {
            if is_color_string(b, i) {
                i += 2;
                continue;
            }
            max = max.max(f.glyphs[b[i] as usize].height as f32);
            i += 1;
        }
        max * scale * f.glyph_scale
    }
    /// `UI_Text_EmHeight`.
    fn em_height(&self, scale: f32) -> f32 {
        self.height("M", scale)
    }
}

/// `UI_Parse_Indent`: a leading "<pixels>\v" indent; returns (pixels, index after it).
fn parse_indent(b: &[u8]) -> (f32, usize) {
    let mut p = 0;
    while p < b.len() && (b[p].is_ascii_digit() || b[p] == b'.') {
        p += 1;
    }
    if p >= b.len() || b[p] != INDENT_MARKER || p > 31 {
        return (0.0, 0);
    }
    match std::str::from_utf8(&b[..p]).ok().and_then(|s| s.parse::<f32>().ok()) {
        Some(px) => (px, p + 1),
        None => (0.0, 0),
    }
}

/// `SkipColorCodes`.
fn skip_color_codes(b: &[u8], p: &mut usize, last: &mut Option<[u8; 2]>) {
    while is_color_string(b, *p) {
        *last = Some([b[*p], b[*p + 1]]);
        *p += 2;
    }
}

/// `SkipWhiteSpace`.
fn skip_white_space(b: &[u8], p: &mut usize, last: &mut Option<[u8; 2]>) {
    while *p < b.len() {
        skip_color_codes(b, p, last);
        if *p < b.len() && b[*p] != b'\n' && b[*p].is_ascii_whitespace() {
            *p += 1;
        } else {
            break;
        }
    }
}

/// `Item_Text_Wrap`: `text` broken into `\n`-terminated lines no wider than
/// `width`, carrying colour codes and `\v` indents to the next line.
pub fn item_text_wrap(fonts: &Fonts, text: &str, scale: f32, width: f32, aspect: f32) -> String {
    let b = text.as_bytes();
    let eos = b.len();
    let mut out: Vec<u8> = Vec::new();
    let mut c: Option<[u8; 2]> = None;
    let mut indent_width = 0.0f32;
    let mut p = 0usize;
    while p < eos {
        let mut text_width = 0.0f32;
        let mut eol = p;
        let mut q = p;
        let test_width = width - indent_width;
        skip_color_codes(b, &mut q, &mut c);
        while text_width < test_width {
            if q >= eos {
                eol = eos;
                break;
            }
            if q > p && b[q] == INDENT_MARKER {
                indent_width = text_width;
                eol = p;
            }
            skip_color_codes(b, &mut q, &mut c);
            if q < eos && b[q] == b'\n' {
                eol = q + 1;
                break;
            }
            if q < eos && b[q].is_ascii_whitespace() {
                eol = q;
            }
            text_width += fonts.char_width(b, &mut q, scale, aspect);
        }
        // no split has taken place, so just split mid-word
        if eol == p {
            eol = q.min(eos);
        }
        // (a width below one glyph would never advance; the C overflows here)
        if eol <= p {
            eol = (p + 1).min(eos);
        }
        out.extend_from_slice(&b[p..eol]);
        p = eol;
        if out.last() == Some(&b'\n') {
            // the line is deliberately broken: clear the colour and indent
            c = None;
            indent_width = 0.0;
        } else {
            out.push(b'\n');
            if indent_width > 0.0 {
                out.extend_from_slice(format!("{indent_width:.6}").as_bytes());
                out.push(INDENT_MARKER);
            }
            skip_white_space(b, &mut p, &mut c);
        }
        if let Some(cc) = c {
            out.extend_from_slice(&cc);
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

// =====================================================================
// drawing (display context: drawHandlePic, fillRect, drawRect, text)
// =====================================================================

/// One draw command in 640x480 virtual coordinates.
#[derive(Clone, Debug, PartialEq)]
pub enum Cmd {
    /// image `img` (texture region `uv` = s, t, s2, t2) drawn into `r`
    Pic { img: String, r: R, uv: [f32; 4], c: Rgba },
    Fill { r: R, c: Rgba },
    /// `Item_Model_Paint`: model `asset` rendered into `r` (the refdef's
    /// 640x480 rect) with the item's fovs and yaw at `time` ms
    Model { asset: String, r: R, fov: [f32; 2], angle: f32, time: i32 },
    /// `DC->drawCinematic`: the cinematic `name` (video/... path) in `r`
    Cinematic { name: String, r: R },
}

pub struct Painter<'a> {
    pub fonts: &'a Fonts,
    pub aspect: f32,
    pub real_time: i32,
    pub out: Vec<Cmd>,
    /// `UI_SetClipRegion`
    clip: Option<R>,
}

/// Clip `r` (and its texture region) to `clip`; None when nothing is left.
fn clip_rect(r: R, uv: [f32; 4], clip: Option<R>) -> Option<(R, [f32; 4])> {
    let Some(c) = clip else { return Some((r, uv)) };
    let (x0, y0, x1, y1) = (r[0].max(c[0]), r[1].max(c[1]), (r[0] + r[2]).min(c[0] + c[2]), (r[1] + r[3]).min(c[1] + c[3]));
    if x1 <= x0 || y1 <= y0 || r[2] <= 0.0 || r[3] <= 0.0 {
        return None;
    }
    let fu = |x: f32| uv[0] + (uv[2] - uv[0]) * (x - r[0]) / r[2];
    let fv = |y: f32| uv[1] + (uv[3] - uv[1]) * (y - r[1]) / r[3];
    Some(([x0, y0, x1 - x0, y1 - y0], [fu(x0), fv(y0), fu(x1), fv(y1)]))
}

impl<'a> Painter<'a> {
    pub fn new(fonts: &'a Fonts, aspect: f32, real_time: i32) -> Painter<'a> {
        Painter { fonts, aspect, real_time, out: Vec::new(), clip: None }
    }
    /// `DC->drawHandlePic` with the current colour.
    fn pic(&mut self, img: &str, r: R, c: Rgba) {
        self.pic_uv(img, r, [0.0, 0.0, 1.0, 1.0], c);
    }
    fn pic_uv(&mut self, img: &str, r: R, uv: [f32; 4], c: Rgba) {
        if img.is_empty() || c[3] <= 0.0 {
            return;
        }
        if let Some((r, uv)) = clip_rect(r, uv, self.clip) {
            self.out.push(Cmd::Pic { img: img.to_string(), r, uv, c });
        }
    }
    /// `UI_FillRect`.
    fn fill(&mut self, r: R, c: Rgba) {
        if c[3] <= 0.0 {
            return;
        }
        if let Some((r, _)) = clip_rect(r, [0.0; 4], self.clip) {
            self.out.push(Cmd::Fill { r, c });
        }
    }
    /// `UI_DrawSides`.
    fn draw_sides(&mut self, r: R, size: f32, c: Rgba) {
        self.fill([r[0], r[1] + size, size, r[3] - size * 2.0], c);
        self.fill([r[0] + r[2] - size, r[1] + size, size, r[3] - size * 2.0], c);
    }
    /// `UI_DrawTopBottom`.
    fn draw_top_bottom(&mut self, r: R, size: f32, c: Rgba) {
        self.fill([r[0], r[1], r[2], size], c);
        self.fill([r[0], r[1] + r[3] - size, r[2], size], c);
    }
    /// `UI_DrawRect`.
    fn draw_rect(&mut self, r: R, size: f32, c: Rgba) {
        self.draw_top_bottom(r, size, c);
        self.draw_sides(r, size, c);
    }
    /// `GradientBar_Paint`.
    fn gradient_bar(&mut self, r: R, c: Rgba) {
        self.pic(ASSET_GRADIENTBAR, r, c);
    }
    /// `UI_Text_PaintChar`.
    fn paint_char(&mut self, mut x: f32, mut y: f32, scale: f32, g: &Glyph, size: f32, c: Rgba) {
        let (mut w, mut h) = (g.iw as f32, g.ih as f32);
        if size > 0.0 {
            let half = size * 0.5 * scale;
            x -= half;
            y -= half;
            w += size;
            h += size;
        }
        w *= self.aspect * scale;
        h *= scale;
        y -= g.top as f32 * scale;
        if g.iw > 0 && g.ih > 0 {
            let shader = g.shader.clone();
            self.pic_uv(&shader, [x, y, w, h], [g.s, g.t, g.s2, g.t2], c);
        }
    }
    /// `UI_Text_Paint_Generic`: returns the x after the text (`maxX`).
    #[allow(clippy::too_many_arguments)]
    fn text_generic(&mut self, mut x: f32, y: f32, scale: f32, gap: f32, text: &str, color: Rgba, style: i32, limit: usize, max_x: Option<f32>, cursor_pos: i32, cursor: u8) -> f32 {
        let fonts = self.fonts;
        let f = fonts.pick(scale);
        let use_scale = scale * f.glyph_scale;
        let b = text.as_bytes();
        let mut len = b.len();
        if limit > 0 && len > limit {
            len = limit;
        }
        let mut new_color = color;
        let (indent, mut s) = parse_indent(b);
        x += indent;
        let mut count = 0usize;
        let mut cursor_x = -1.0f32;
        while s < b.len() && count < len {
            let mut t = s;
            let char_width = fonts.char_width(b, &mut t, scale, self.aspect);
            let g = &f.glyphs[b[s] as usize];
            if let Some(mx) = max_x {
                if char_width + x > mx {
                    break;
                }
            }
            if cursor_pos < 0 {
                if is_color_string(b, s) {
                    let n = COLOR_TABLE[color_index(b[s + 1])];
                    new_color = [n[0], n[1], n[2], color[3]];
                    s += 2;
                    continue;
                }
                if b[s] == INDENT_MARKER {
                    s += 1;
                    continue;
                }
            }
            if style == TEXTSTYLE_SHADOWED || style == TEXTSTYLE_SHADOWEDMORE {
                let ofs = if style == TEXTSTYLE_SHADOWED { 1.0 } else { 2.0 };
                self.paint_char(x + ofs, y + ofs, use_scale, g, 0.0, [0.0, 0.0, 0.0, new_color[3]]);
            } else if style == TEXTSTYLE_NEON {
                let glow = [new_color[0], new_color[1], new_color[2], new_color[3] * 0.2];
                self.paint_char(x, y, use_scale, g, 6.0, glow);
                self.paint_char(x, y, use_scale, g, 4.0, glow);
                self.paint_char(x, y, use_scale, g, 2.0, new_color);
            }
            self.paint_char(x, y, use_scale, g, 0.0, new_color);
            if count as i32 == cursor_pos {
                cursor_x = x;
            }
            x += g.xskip as f32 * self.aspect * use_scale + gap;
            s += 1;
            count += 1;
        }
        if cursor_pos >= 0 {
            if cursor_pos as usize == len {
                cursor_x = x;
            }
            if cursor_x >= 0.0 && (self.real_time / BLINK_DIVISOR) & 1 == 0 {
                let g = f.glyphs[cursor as usize].clone();
                self.paint_char(cursor_x, y, use_scale, &g, 0.0, new_color);
            }
        }
        x
    }
    /// `UI_Text_Paint`.
    fn text(&mut self, x: f32, y: f32, scale: f32, color: Rgba, text: &str, adjust: f32, limit: usize, style: i32) {
        self.text_generic(x, y, scale, adjust, text, color, style, limit, None, -1, 0);
    }
    fn width(&self, t: &str, scale: f32) -> f32 {
        self.fonts.width(t, scale, self.aspect)
    }
    fn height(&self, t: &str, scale: f32) -> f32 {
        self.fonts.height(t, scale)
    }
}

/// `LerpColor`.
fn lerp_color(a: Rgba, b: Rgba, t: f32) -> Rgba {
    let mut c = [0.0; 4];
    for i in 0..4 {
        c[i] = (a[i] + t * (b[i] - a[i])).clamp(0.0, 1.0);
    }
    c
}

/// `Rect_ContainsPoint`.
fn contains(r: R, x: f32, y: f32) -> bool {
    x > r[0] && x < r[0] + r[2] && y > r[1] && y < r[1] + r[3]
}

// =====================================================================
// bg_misc.c `info` strings and the cvarTable defaults
// =====================================================================

/// The `info` strings of bg_misc.c (`bg_buildableList`, `bg_classList`,
/// `bg_weapons`, `bg_upgrades`) the info panes show (`UI_DrawInfoPane`).
const BUILDABLE_INFO: &[(&str, &str)] = &[
    ("eggpod", "The most basic alien structure. It allows aliens to spawn and protect the Overmind. Without any of these, the Overmind is left nearly defenseless and defeat is imminent."),
    ("overmind", "A collective consciousness that controls all the alien structures in its vicinity. It must be protected at all costs, since its death will render alien structures defenseless."),
    ("barricade", "Used to obstruct corridors and doorways, hindering humans from threatening the spawns and Overmind. Barricades will shrink to allow aliens to pass over them, however."),
    ("acid_tube", "Ejects lethal poisonous acid at an approaching human. These are highly effective when used in conjunction with a trapper to hold the victim in place."),
    ("trapper", "Fires a blob of adhesive spit at any non-alien in its line of sight. This hinders their movement, making them an easy target for other defensive structures or aliens."),
    ("booster", "Laces the attacks of any alien that touches it with a poison that will gradually deal damage to any humans exposed to it. The booster also increases the rate of health regeneration for any nearby aliens."),
    ("hive", "Houses millions of tiny insectoid aliens. When a human approaches this structure, the insectoids attack."),
    ("telenode", "The most basic human structure. It provides a means for humans to enter the battle arena. Without any of these the humans cannot spawn and defeat is imminent."),
    ("mgturret", "Automated base defense that is effective against large targets but slow to begin firing. Should always be backed up by physical support."),
    ("tesla", "A structure equipped with a strong electrical attack that fires instantly and always hits its target. It is effective against smaller aliens and for consolidating basic defense."),
    ("arm", "An essential part of the human base, providing a means to upgrade the basic human equipment. A range of upgrades and weapons are available for sale from the armoury."),
    ("dcc", "A structure that enables self-repair functionality in human structures. Each Defence Computer built increases repair rate slightly."),
    ("medistat", "A structure that automatically restores the health and stamina of any human that stands on it. It may only be used by one person at a time. This structure also issues medkits."),
    ("reactor", "All structures except the telenode rely on a reactor to operate. The reactor provides power for all the human structures either directly or via repeaters. Only one reactor can be built at a time."),
    ("repeater", "A power distributor that transmits power from the reactor to remote locations, so that bases may be built far from the reactor."),
    // Tremulous 1.1.0's hovel (data-1.1.0.pk3 ui/infopanes.def "hovelbuild", dec041)
    ("hovel", "An armoured shell used by the builder class to hide in, while the alien base is under attack. It may be entered or exited at any time."),
];
const CLASS_INFO: &[(&str, &str)] = &[
    ("spectator", ""),
    ("builder", "Responsible for building and maintaining all the alien structures. Has a weak melee slash attack."),
    ("builderupg", "Similar to the base Granger, except that in addition to being able to build structures it has a spit attack that slows victims and the ability to crawl on walls."),
    ("level0", "Has a lethal reflexive bite and the ability to crawl on walls and ceilings."),
    ("level1", "A support class able to crawl on walls and ceilings. Its melee attack is most effective when combined with the ability to grab and hold its victims in place. Provides a weak healing aura that accelerates the healing rate of nearby aliens."),
    ("level1upg", "In addition to the basic Basilisk abilities, the Advanced Basilisk sprays a poisonous gas which disorients any nearby humans. Has a strong healing aura that that accelerates the healing rate of nearby aliens."),
    ("level2", "Has a melee attack and the ability to jump off walls. This allows the Marauder to gather great speed in enclosed areas."),
    ("level2upg", "The Advanced Marauder has all the abilities of the basic Marauder with the addition of an area effect electric shock attack."),
    ("level3", "Possesses a melee attack and the pounce ability, which may be used as both an attack and a means to reach remote locations inaccessible from the ground."),
    ("level3upg", "In addition to the basic Dragoon abilities, the Advanced Dragoon has 3 barbs which may be used to attack humans from a distance."),
    ("level4", "A large alien with a strong melee attack, this class can also charge at enemy humans and structures, inflicting great damage. Any humans or their structures caught under a falling Tyrant will be crushed by its weight."),
    ("human_base", ""),
    ("human_bsuit", ""),
];
const WEAPON_INFO: &[(&str, &str)] = &[
    ("level0", ""),
    ("level1", ""),
    ("level1upg", ""),
    ("level2", ""),
    ("level2upg", ""),
    ("level3", ""),
    ("level3upg", ""),
    ("level4", ""),
    ("blaster", ""),
    ("rifle", "Basic weapon. Cased projectile weapon, with a slow clip based reload system."),
    ("psaw", "Similar to a chainsaw, but instead of a chain it has an electric arc capable of dealing a great deal of damage at close range."),
    ("shotgun", "Close range weapon that is useful against larger foes. It has a slow repeat rate, but can be devastatingly effective."),
    ("lgun", "Slightly more powerful than the basic rifle, rapidly fires small packets of energy."),
    ("mdriver", "A portable particle accelerator which causes minor nuclear reactions at the point of impact. It has a very large payload, but fires slowly."),
    ("chaingun", "Belt drive, cased projectile weapon. It has a high repeat rate but a wide firing angle and is therefore relatively inaccurate."),
    ("flamer", "Sprays fire at its target. It is powered by compressed gas. The relatively low rate of fire means this weapon is most effective against static targets."),
    ("prifle", "An energy weapon that fires rapid pulses of concentrated energy."),
    ("lcannon", "Blaster technology scaled up to deliver devastating power. Primary fire must be charged before firing. It has a quick secondary attack that does not require charging."),
    ("grenade", ""),
    ("lockblob", ""),
    ("hive", ""),
    ("teslagen", ""),
    ("mgturret", ""),
    ("abuild", ""),
    ("abuildupg", ""),
    ("ckit", "Used for building structures. This includes spawns, power and basic defense. More structures become available with new stages."),
    // Tremulous 1.1.0's advanced construction kit (data-1.1.0.pk3 ui/infopanes.def "ackititem", dec041)
    ("ackit", "Used for building advanced structures. This includes combat computers and advanced defense."),
];
const UPGRADE_INFO: &[(&str, &str)] = &[
    ("larmour", "Protective armour that helps to defend against light alien melee attacks."),
    ("helmet", "In addition to protecting your head, the helmet provides a scanner indicating the presence of any friendly or hostile lifeforms and structures in your immediate vicinity."),
    ("medkit", ""),
    ("battpack", "Back-mounted battery pack that permits storage of one and a half times the normal energy capacity for energy weapons."),
    ("jetpack", "Back-mounted jet pack that enables the user to fly to remote locations. It is very useful against alien spawns in hard to reach spots."),
    ("bsuit", "A full body armour that is highly effective at repelling alien attacks. It allows the user to enter hostile situations with a greater degree of confidence."),
    ("gren", "A small incendinary device ideal for damaging tightly packed alien structures. Has a five second timer."),
    ("ammo", "Ammunition for the currently held weapon."),
];

/// `vmCvar_t` defaults of ui_main.c / cg_main.c `cvarTable` (and the engine
/// cvars the options menus show), the initial values of the cvar store.
const CVAR_DEFAULTS: &[(&str, &str)] = &[
    ("cg_animspeed", "1"),
    ("cg_bounceParticles", "0"),
    ("cg_centertime", "3"),
    ("cg_chatTeamPrefix", "1"),
    ("cg_consoleLatency", "3000"),
    ("cg_crosshairSize", "1"),
    ("cg_debuganim", "0"),
    ("cg_debugevents", "0"),
    ("cg_debugMove", "0"),
    ("cg_debugParticles", "0"),
    ("cg_debugposition", "0"),
    ("cg_debugPVS", "0"),
    ("cg_debugRandom", "0"),
    ("cg_debugTrails", "0"),
    ("cg_debugVoices", "0"),
    ("cg_depthSortParticles", "1"),
    ("cg_disableBuildDialogs", "0"),
    ("cg_disableCommandDialogs", "0"),
    ("cg_disableScannerPlane", "0"),
    ("cg_disableUpgradeDialogs", "0"),
    ("cg_disableWarningDialogs", "0"),
    ("cg_draw2D", "1"),
    ("cg_drawBBOX", "0"),
    ("cg_drawChargeBar", "1"),
    ("cg_drawClock", "0"),
    ("cg_drawCrosshair", "2"),
    ("cg_drawCrosshairNames", "1"),
    ("cg_drawDemoState", "1"),
    ("cg_drawFPS", "1"),
    ("cg_drawGun", "1"),
    ("cg_drawSnapshot", "0"),
    ("cg_drawSpeed", "0"),
    ("cg_drawSurfNormal", "0"),
    ("cg_drawTeamOverlay", "1"),
    ("cg_drawTimer", "1"),
    ("cg_emoticons", "1"),
    ("cg_errordecay", "100"),
    ("cg_footsteps", "1"),
    ("cg_gunX", "0"),
    ("cg_gunY", "0"),
    ("cg_gunZ", "0"),
    ("cg_hudFiles", "ui/hud.txt"),
    ("cg_lagometer", "0"),
    ("cg_lightFlare", "3"),
    ("cg_marks", "1"),
    ("cg_noplayeranims", "0"),
    ("cg_nopredict", "0"),
    ("cg_noPrintDuplicate", "0"),
    ("cg_noTaunt", "0"),
    ("cg_noVoiceChats", "0"),
    ("cg_noVoiceText", "0"),
    ("cg_optimizePrediction", "1"),
    ("cg_painBlendDownRate", "0.5"),
    ("cg_painBlendMax", "0.7"),
    ("cg_painBlendScale", "7.0"),
    ("cg_painBlendUpRate", "10.0"),
    ("cg_painBlendZoom", "0.65"),
    ("cg_projectileNudge", "1"),
    ("cg_runpitch", "0.002"),
    ("cg_runroll", "0.005"),
    ("cg_shadows", "1"),
    ("cg_showmiss", "0"),
    ("cg_smoothClients", "0"),
    ("cg_sprintToggle", "0"),
    ("cg_staticDeathCam", "0"),
    ("cg_stats", "0"),
    ("cg_stereoSeparation", "0.4"),
    ("cg_stickySpec", "1"),
    ("cg_swingSpeed", "0.3"),
    ("cg_teamChatsOnly", "0"),
    ("cg_teamOverlayMaxPlayers", "8"),
    ("cg_teamOverlaySortMode", "1"),
    ("cg_teslaTrailTime", "250"),
    ("cg_thirdPerson", "0"),
    ("cg_thirdPersonAngle", "0"),
    ("cg_thirdPersonPitchFollow", "0"),
    ("cg_thirdPersonRange", "75"),
    ("cg_thirdPersonShoulderViewMode", "1"),
    ("cg_timescaleFadeEnd", "1"),
    ("cg_timescaleFadeSpeed", "0"),
    ("cg_tracerchance", "0.4"),
    ("cg_tracerlength", "100"),
    ("cg_tracerwidth", "1"),
    ("cg_tutorial", "1"),
    ("cg_unlagged", "1"),
    ("cg_viewsize", "100"),
    ("cg_wwSmoothTime", "300"),
    ("cl_freelook", "1"),
    ("cl_paused", "0"),
    ("com_blood", "1"),
    ("com_cameraMode", "0"),
    ("g_synchronousClients", "0"),
    ("m_filter", "0"),
    ("m_pitch", "0.022"),
    ("pmove_fixed", "0"),
    ("pmove_msec", "8"),
    ("r_allowExtensions", "1"),
    ("r_colorbits", "0"),
    ("r_dynamiclight", "1"),
    ("r_ext_compressed_textures", "0"),
    ("r_fastsky", "0"),
    ("r_finish", "0"),
    ("r_fullscreen", "1"),
    ("r_gamma", "1"),
    ("r_height", "0"),
    ("r_inGameVideo", "0"),
    ("r_lodbias", "0"),
    ("r_mode", "-2"),
    ("r_picmip", "1"),
    ("r_texturebits", "0"),
    ("r_width", "0"),
    ("rate", "90000"),
    ("s_doppler", "1"),
    ("s_musicvolume", "0.25"),
    ("s_useOpenAL", "0"),
    ("s_volume", "0.8"),
    ("sensitivity", "5"),
    ("teamoverlay", "1"),
    ("timescale", "1"),
    ("ui_alienTeamVoteActive", "0"),
    ("ui_bigFont", "0.5"),
    ("ui_browserProtocol", "0"),
    ("ui_browserShowEmpty", "1"),
    ("ui_browserShowFull", "1"),
    ("ui_carriage", ""),
    ("ui_chatCommands", "1"),
    ("ui_currentClass", "0"),
    ("ui_dedicated", "0"),
    ("ui_developer", "0"),
    ("ui_dialog", "Text not set"),
    ("ui_findPlayer", ""),
    ("ui_humanTeamVoteActive", "0"),
    ("ui_lastServerRefresh_0", ""),
    ("ui_lastServerRefresh_0_time", ""),
    ("ui_lastServerRefresh_1", ""),
    ("ui_lastServerRefresh_1_time", ""),
    ("ui_lastServerRefresh_2", ""),
    ("ui_lastServerRefresh_2_time", ""),
    ("ui_lastServerRefresh_3", ""),
    ("ui_lastServerRefresh_3_time", ""),
    ("ui_netSource", "0"),
    ("ui_selectedMap", "0"),
    ("ui_serverStatusTimeOut", "7000"),
    ("ui_smallFont", "0.2"),
    ("ui_stages", "0 0"),
    ("ui_textWrapCache", "1"),
    ("ui_voteActive", "0"),
    ("ui_winner", ""),
    ("voice", "default"),
];
const ENGINE_CVARS: &[(&str, &str)] = &[
    ("name", "UnnamedPlayer"),
    ("sensitivity", "5"),
    ("m_filter", "1"),
    ("m_pitch", "0.022"),
    ("cl_freelook", "1"),
    ("rate", "90000"),
    ("s_volume", "0.8"),
    ("s_musicvolume", "0.25"),
    ("s_doppler", "1"),
    ("s_khz", "22"),
    ("s_useOpenAL", "0"),
    ("r_gamma", "1"),
    ("r_picmip", "1"),
    ("r_texturebits", "0"),
    ("r_colorbits", "0"),
    ("r_fullscreen", "1"),
    ("r_lodbias", "0"),
    ("r_vertexlight", "0"),
    ("r_dynamiclight", "1"),
    ("r_fastsky", "0"),
    ("r_finish", "0"),
    ("r_inGameVideo", "1"),
    ("r_allowExtensions", "1"),
    ("r_ext_compressed_textures", "0"),
    ("r_ext_texture_filter_anisotropic", "0"),
    ("r_texturemode", "GL_LINEAR_MIPMAP_NEAREST"),
    ("cg_viewsize", "100"),
    ("cg_lightFlare", "3"),
    ("ui_mousePitch", "0"),
];

fn bg_info(table: &[(&str, &'static str)], id: &str) -> &'static str {
    table.iter().find(|(n, _)| *n == id).map(|(_, i)| *i).unwrap_or("")
}

// =====================================================================
// ui/help.txt (UI_LoadHelp)
// =====================================================================

/// Botlib string escapes (`\n`, `\t`, `\\`, `\"`) as the precompiler
/// expands them in string tokens.
fn unescape(s: &str) -> String {
    let mut out = String::new();
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c == '\\' {
            match it.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some(o) => out.push(o),
                None => {}
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// `UI_LoadHelp`: `{ "title" { "text" ... } ... }` -> (title, "title\n\ntext").
pub fn parse_help(text: &str) -> Vec<(String, String)> {
    let mut toks = Vec::new();
    let mut defs = HashMap::new();
    if hud::lex(text, &mut defs, &mut |_| None, &mut toks, 0).is_err() || toks.first() != Some(&Tok::Open) {
        return Vec::new();
    }
    let word = |t: &Tok| match t {
        Tok::Word(w) => unescape(w),
        Tok::Str(w) => unescape(w),
        _ => String::new(),
    };
    let mut out = Vec::new();
    let mut title = String::new();
    let mut i = 1;
    while i < toks.len() {
        match &toks[i] {
            Tok::Close => break,
            Tok::Open => {
                let mut buffer = format!("{title}\n\n");
                i += 1;
                while i < toks.len() && toks[i] != Tok::Close {
                    buffer.push_str(&word(&toks[i]));
                    i += 1;
                }
                out.push((std::mem::take(&mut title), buffer));
            }
            t => title.push_str(&word(t)),
        }
        i += 1;
    }
    out
}

// =====================================================================
// uiInfo lists (ui_main.c menuItem_t)
// =====================================================================

/// `menuItem_t.type` + `.v`: what the info pane shows.
#[derive(Clone, Debug, PartialEq)]
pub enum Info {
    Text(String),
    Class(ClassId),
    Weapon(WeaponId),
    Upgrade(UpgradeId),
    Buildable(BuildableId),
}

/// `menuItem_t`: list text, command, info pane.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub text: String,
    /// the client command (`cmd ...` without the prefix)
    pub cmd: String,
    pub info: Info,
}

/// What the ui module reads through cvars (ui_currentClass, ui_carriage,
/// ui_stages) and the configstrings.
#[derive(Clone, Debug, Default)]
pub struct Player {
    pub team: Team,
    pub class: i32,
    pub credits: i32,
    pub stats: Vec<i32>,
    pub alien_stage: i32,
    pub human_stage: i32,
    /// A Tremulous 1.1.0 (protocol 69) game: the lists and info panes are
    /// 1.1.0 ui_main.c's over the 1.1 tables (dec041).
    pub vanilla: bool,
}

impl Player {
    pub fn from_world(w: &World, i: usize) -> Player {
        let c = &w.clients[i];
        Player { team: c.team, class: c.ps.stats[Stat::Class as usize], credits: c.credit, stats: c.ps.stats.to_vec(), alien_stage: w.level.alien_stage, human_stage: w.level.human_stage, vanilla: w.vanilla }
    }
    /// 1.1 counts alien frags where legacy counts credits
    /// (ALIEN_CREDITS_PER_KILL each, kernel::v11).
    fn frags11(&self) -> i32 {
        self.credits / k::ALIEN_CREDITS_PER_KILL
    }
}

/// `UI_LoadTeams`.
pub fn load_teams() -> Vec<Entry> {
    let e = |t: &str, c: &str, i: &str| Entry { text: t.into(), cmd: c.into(), info: Info::Text(i.into()) };
    vec![
        e(
            "Aliens",
            "team aliens",
            "The Alien Team\n\nThe Aliens' strengths are in movement and the ability to quickly construct new bases quickly. They possess a range of abilities including basic melee attacks, movement-crippling poisons and more.",
        ),
        e(
            "Humans",
            "team humans",
            "The Human Team\n\nThe humans are the masters of technology. Although their bases take long to construct, their automated defense ensures they stay built. A wide range of upgrades and weapons are available to the humans, each contributing to eradicate the alien threat.",
        ),
        e("Spectate", "team spectate", "Watch the game without playing."),
        e("Auto select", "team auto", "Join the team with the least players."),
    ]
}

/// `UI_AddClass`.
fn class_entry(c: ClassId) -> Entry {
    Entry { text: c.cold().display_name.into(), cmd: format!("class {}", c.id()), info: Info::Class(c) }
}

/// `UI_LoadAlienClasses` (1.1.0's `UI_LoadTremAlienClasses` in a 1.1 game).
pub fn load_alien_classes(p: &Player) -> Vec<Entry> {
    if p.vanilla {
        let allowed = |c: Class11Id| bg11::allowed_in_stage(c.def().stages, p.alien_stage);
        return vec![class_entry(ClassId::Level0), class_entry(if allowed(Class11Id::Builderupg) { ClassId::Builderupg } else { ClassId::Builder })];
    }
    let mut v = vec![class_entry(ClassId::Level0)];
    if bg::class_allowed_in_stage(ClassId::Builderupg, p.alien_stage) {
        v.push(class_entry(ClassId::Builderupg));
    } else {
        v.push(class_entry(ClassId::Builder));
    }
    v
}

/// `UI_AddItem`.
fn weapon_entry(w: WeaponId, verb: &str) -> Entry {
    Entry { text: w.cold().display_name.into(), cmd: format!("{verb} {}", w.id()), info: Info::Weapon(w) }
}
fn upgrade_entry(u: UpgradeId, verb: &str) -> Entry {
    Entry { text: u.cold().display_name.into(), cmd: format!("{verb} {}", u.id()), info: Info::Upgrade(u) }
}

/// `UI_LoadHumanItems` (`cmd class rifle` / `cmd class ckit`); in a 1.1
/// game 1.1.0's `UI_LoadTremHumanItems`: the advanced construction kit
/// instead of the kit once its stage is reached.
pub fn load_human_items(p: &Player) -> Vec<Entry> {
    if p.vanilla && bg11::allowed_in_stage(Weapon11Id::Ackit.def().stages, p.human_stage) {
        return vec![weapon_entry(WeaponId::Rifle, "class"), weapon_entry(WeaponId::Ackit, "class")];
    }
    vec![weapon_entry(WeaponId::Rifle, "class"), weapon_entry(WeaponId::Ckit, "class")]
}

/// `UI_ParseCarriageList`: the weapons / upgrades held (cgame's ui_carriage).
fn carriage(p: &Player) -> (Vec<WeaponId>, Vec<UpgradeId>) {
    let ws = WeaponId::ALL.into_iter().filter(|w| bg::inventory_contains_weapon(*w as i32, &p.stats)).collect();
    let us = UpgradeId::ALL.into_iter().filter(|u| bg::inventory_contains_upgrade(*u as i32, &p.stats)).collect();
    (ws, us)
}

/// `UI_LoadHumanArmouryBuys` (1.1.0's `UI_LoadTremHumanArmouryBuys` over
/// the 1.1 tables, with its "retrigger", in a 1.1 game).
pub fn load_armoury_buys(p: &Player) -> Vec<Entry> {
    let (ws, us) = carriage(p);
    if p.vanilla {
        let ws11: Vec<Weapon11Id> = ws.iter().filter_map(|&w| v11::weapon11_of(w)).collect();
        let us11: Vec<Upgrade11Id> = us.iter().filter_map(|&u| v11::upgrade11_of(u)).collect();
        let slots = ws11.iter().map(|w| w.def().slots).chain(us11.iter().map(|u| u.def().slots)).fold(0, |a, b| a | b);
        let mut out = Vec::new();
        for w in Weapon11Id::ALL {
            let d = w.def();
            if d.team == Team::Humans && d.purchasable && bg11::allowed_in_stage(d.stages, p.human_stage) && d.slots & slots == 0 && !ws11.contains(&w) {
                out.push(weapon_entry(d.legacy, "buy").retrigger());
            }
        }
        for u in Upgrade11Id::ALL {
            let d = u.def();
            if d.team == Team::Humans && d.purchasable && bg11::allowed_in_stage(d.stages, p.human_stage) && d.slots & slots == 0 && !us11.contains(&u) {
                out.push(upgrade_entry(d.legacy, "buy").retrigger());
            }
        }
        return out;
    }
    let mut slots = 0u32;
    for w in &ws {
        slots |= w.def().slots;
    }
    for u in &us {
        slots |= u.def().slots;
    }
    let mut out = Vec::new();
    for w in WeaponId::ALL {
        let d = w.def();
        if d.team == Team::Humans && d.purchasable && bg::weapon_allowed_in_stage(w, p.human_stage) && d.slots & slots == 0 && !ws.contains(&w) {
            out.push(weapon_entry(w, "buy"));
        }
    }
    for u in UpgradeId::ALL {
        let d = u.def();
        if d.team == Team::Humans && d.purchasable && bg::upgrade_allowed_in_stage(u, p.human_stage) && d.slots & slots == 0 && !us.contains(&u) {
            out.push(upgrade_entry(u, "buy"));
        }
    }
    out
}

/// `UI_LoadHumanArmourySells` (with 1.1's "retrigger" in a 1.1 game).
pub fn load_armoury_sells(p: &Player) -> Vec<Entry> {
    let (ws, us) = carriage(p);
    if p.vanilla {
        return ws.into_iter().map(|w| weapon_entry(w, "sell").retrigger()).chain(us.into_iter().map(|u| upgrade_entry(u, "sell").retrigger())).collect();
    }
    ws.into_iter().map(|w| weapon_entry(w, "sell")).chain(us.into_iter().map(|u| upgrade_entry(u, "sell"))).collect()
}

/// `UI_LoadAlienUpgrades` (ui_currentClass = class, credits); in a 1.1
/// game `UI_LoadTremAlienUpgrades` over the 1.1 evolution tree and costs.
pub fn load_alien_upgrades(p: &Player) -> Vec<Entry> {
    if p.vanilla {
        let from = v11::class_to_11(p.class);
        return Class11Id::ALL
            .into_iter()
            .filter(|c| *c != Class11Id::Spectator && bg11::class_can_evolve_from_to(from, c.num() as i32, p.frags11(), 0) >= 0 && bg11::allowed_in_stage(c.def().stages, p.alien_stage))
            .map(|c| class_entry(c.def().legacy))
            .collect();
    }
    let from = bg::class_id(p.class);
    ClassId::ALL.into_iter().filter(|&c| c != ClassId::Spectator && bg::class_can_evolve_from_to(from, c, p.credits, p.alien_stage, 0) >= 0).map(class_entry).collect()
}

/// `UI_LoadAlienBuilds` / `UI_LoadHumanBuilds`.
pub fn load_builds(p: &Player, team: Team) -> Vec<Entry> {
    let (ws, _) = carriage(p);
    let stage = if team == Team::Aliens { p.alien_stage } else { p.human_stage };
    if p.vanilla {
        // 1.1.0's UI_LoadTremAlienBuilds / UI_LoadTremHumanBuilds (the hovel,
        // the advanced kit's structures)
        let ws11: Vec<Weapon11Id> = ws.iter().filter_map(|&w| v11::weapon11_of(w)).collect();
        return Buildable11Id::ALL
            .into_iter()
            .filter(|b| b.def().team == team && b.def().build_weapons.iter().any(|w| ws11.contains(w)) && bg11::allowed_in_stage(b.def().stages, stage))
            .map(|b| Entry { text: b.cold().display_name.into(), cmd: format!("build {}", b.id()), info: Info::Buildable(b.def().legacy) })
            .collect();
    }
    BuildableId::ALL
        .into_iter()
        .filter(|b| b.def().team == team && b.def().build_weapons.iter().any(|w| ws.contains(w)) && bg::buildable_allowed_in_stage(*b, stage))
        .map(|b| Entry { text: b.cold().display_name.into(), cmd: format!("build {}", b.id()), info: Info::Buildable(b) })
        .collect()
}

/// `UI_DrawInfoPane`'s text for an entry.
pub fn info_pane_text(e: &Entry, p: &Player) -> String {
    if p.vanilla {
        if let Some(t) = info_pane_text11(e, p) {
            return t;
        }
    }
    match &e.info {
        Info::Text(t) => t.clone(),
        Info::Class(c) => {
            let cost = bg::class_can_evolve_from_to(bg::class_id(p.class), *c, p.credits, p.alien_stage, 0);
            let value = (cost + k::ALIEN_CREDITS_PER_KILL - 1) / k::ALIEN_CREDITS_PER_KILL;
            let (name, info) = (c.cold().display_name, bg_info(CLASS_INFO, c.id()));
            if value < 1 {
                format!("{name}\n\n{info}")
            } else {
                format!("{name}\n\n{info}\n\nFrags: {value}")
            }
        }
        Info::Weapon(w) => {
            let price = w.def().price;
            let p = if price == 0 { "Free".to_string() } else { price.to_string() };
            format!("{}\n\n{}\n\nCredits: {p}", w.cold().display_name, bg_info(WEAPON_INFO, w.id()))
        }
        Info::Upgrade(u) => {
            let price = u.def().price;
            let p = if price == 0 { "Free".to_string() } else { price.to_string() };
            format!("{}\n\n{}\n\nCredits: {p}", u.cold().display_name, bg_info(UPGRADE_INFO, u.id()))
        }
        Info::Buildable(b) => {
            let d = b.def();
            let what = match d.team {
                Team::Aliens => "Sentience",
                Team::Humans => "Power",
                Team::None => "",
            };
            let (name, info) = (b.cold().display_name, bg_info(BUILDABLE_INFO, b.id()));
            if d.build_points == 0 {
                format!("{name}\n\n{info}")
            } else {
                format!("{name}\n\n{info}\n\n{what}: {}", d.build_points)
            }
        }
    }
}

/// The info pane of a 1.1 game: 1.1's prices, frag costs and build points
/// (infopanes.def of data-1.1.0.pk3 shows the bg_misc.c values).
fn info_pane_text11(e: &Entry, p: &Player) -> Option<String> {
    let money = |price: i32| if price == 0 { "Free".to_string() } else { price.to_string() };
    Some(match &e.info {
        Info::Class(c) => {
            let c11 = v11::class11_of(*c)?;
            let cost = bg11::class_can_evolve_from_to(v11::class_to_11(p.class), c11.num() as i32, p.frags11(), 0);
            let (name, info) = (c11.cold().display_name, bg_info(CLASS_INFO, c.id()));
            if cost < 1 {
                format!("{name}\n\n{info}")
            } else {
                format!("{name}\n\n{info}\n\nFrags: {cost}")
            }
        }
        Info::Weapon(w) => {
            let w11 = v11::weapon11_of(*w)?;
            format!("{}\n\n{}\n\nCredits: {}", w11.cold().display_name, bg_info(WEAPON_INFO, w.id()), money(w11.def().price))
        }
        Info::Upgrade(u) => {
            let u11 = v11::upgrade11_of(*u)?;
            format!("{}\n\n{}\n\nCredits: {}", u11.cold().display_name, bg_info(UPGRADE_INFO, u.id()), money(u11.def().price))
        }
        Info::Buildable(b) => {
            let b11 = v11::buildable11_of(*b)?;
            let d = b11.def();
            let what = match d.team {
                Team::Aliens => "Sentience",
                Team::Humans => "Power",
                Team::None => "",
            };
            let (name, info) = (b11.cold().display_name, bg_info(BUILDABLE_INFO, b.id()));
            if d.build_points == 0 {
                format!("{name}\n\n{info}")
            } else {
                format!("{name}\n\n{info}\n\n{what}: {}", d.build_points)
            }
        }
        Info::Text(_) => return None,
    })
}

impl Entry {
    /// 1.1.0's armoury commands end in "retrigger": the server reopens the
    /// armoury menu (Cmd_Buy_f / Cmd_Sell_f).
    fn retrigger(mut self) -> Entry {
        self.cmd.push_str(" retrigger");
        self
    }
}

// =====================================================================
// CG_Menu (cg_servercmds.c): G_TriggerMenu messages
// =====================================================================

/// `dialogType_t`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DialogType {
    /// type 0: no cg_disable*Dialogs cvar applies
    Untyped,
    ArmouryEvolve,
    Build,
    Command,
}

/// What `CG_Menu` does for one menu id: open a menu, or show `longMsg`
/// in the team dialog (else print `shortMsg`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CgMenu {
    Open(&'static str),
    Dialog(Option<&'static str>, DialogType),
}

/// `CG_Menu` for the message this port's simulation prints where the game
/// calls `G_TriggerMenu` (the server sends the menu id, the sim its text).
pub fn cg_menu(msg: &str, mark_deconstruct: bool) -> Option<CgMenu> {
    use DialogType::{ArmouryEvolve, Build, Command, Untyped};
    let d = |l: &'static str, t: DialogType| Some(CgMenu::Dialog(Some(l), t));
    match msg.trim() {
        "Choose a team" => Some(CgMenu::Open("tremulous_teamselect")),
        "Choose a class to spawn as" => Some(CgMenu::Open("tremulous_alienclass")),
        "Choose an item to spawn with" => Some(CgMenu::Open("tremulous_humanitem")),
        "There is no room to build here" => d("There is no room to build here. Move until the structure turns translucent green, indicating a valid build location.", Build),
        "Cannot build on this surface" => d("Cannot build on this surface. The surface is too steep or unsuitable for building. Please choose another site for this structure.", Build),
        "You cannot build that structure" => Some(CgMenu::Dialog(None, Build)),
        "This action would remove your last spawn" | "You may not deconstruct the last spawn" => d("This action would remove your team's last spawn point, which often quickly results in a loss. Try building more spawns.", Untyped),
        "Building is not allowed during Sudden Death" => d("Neither team has prevailed after a certain time and the game has entered Sudden Death. During Sudden Death building is not allowed.", Build),
        "Building is denied to you, as your team has admitted defeat" => d("Your team has decided to admit defeat and concede the game:traitors and cowards are not allowed to build.", Untyped),
        "There is not enough power left" => {
            if mark_deconstruct {
                d("There is no power remaining. Free up power by marking existing buildable objects.", Build)
            } else {
                d("There is no power remaining. Free up power by deconstructing existing buildable objects.", Build)
            }
        }
        "This Armoury is not powered" => d("This buildable is not powered. Build a Reactor and/or Repeater in order to power it.", Build),
        "There can only be one Reactor" => d("There can only be one Reactor. Deconstruct the existing one if you wish to move it.", Build),
        "There is no power here" => d("There is no power here. If available, a Repeater may be used to transmit power to this location.", Build),
        "A Defence Computer is required" => d("There is no Defense Computer. A Defense Computer is needed to build this.", Build),
        "There is already power here" => d("This area already has power. A Repeater is not required here.", Build),
        "You have no room for that item" => d("You have no room to carry this. Please sell any conflicting upgrades before purchasing this item.", ArmouryEvolve),
        "You don't have enough credits" => d("Insufficient funds. You do not have enough credits to perform this action.", ArmouryEvolve),
        "You already hold this item" => d("You already hold this item. It is not possible to carry multiple items of the same type.", ArmouryEvolve),
        "You must be near a powered Armoury" => d("You must be near a powered Armoury in order to purchase weapons, upgrades or ammunition.", ArmouryEvolve),
        "You can't buy energy ammo here" => d("You must be near a Reactor or a powered Armoury or Repeater in order to purchase energy ammunition.", ArmouryEvolve),
        "Not enough room here to put on a Battle Suit" => d("There is not enough room here to put on a Battle Suit. Make sure you have enough head room to climb in.", ArmouryEvolve),
        "Not enough room here to take off your Battle Suit" => d("There is not enough room here to take off your Battle Suit. Make sure you have enough head room to climb out.", ArmouryEvolve),
        "You cannot sell until build timer expires" => d("You are not allowed to buy or sell weapons until your build timer has expired.", ArmouryEvolve),
        "You need creep to build here" => d("There is no creep here. You must build near existing Eggs or the Overmind. Alien structures will not support themselves.", Build),
        "There is no Overmind" => d("There is no Overmind. An Overmind must be built to control the structure you tried to place.", Build),
        "There can only be one Overmind" => d("There can only be one Overmind. Deconstruct the existing one if you wish to move it.", Build),
        "The Overmind cannot control any more structures" => d("The Overmind cannot control any more structures. Deconstruct existing structures to build more.", Build),
        "Not enough room here to evolve" => d("There is no room to evolve here. Move away from walls or other nearby objects and try again.", ArmouryEvolve),
        "Too close to the enemy to evolve" => d("This location is too close to the enemy to evolve. Move away from the enemy's presence and try again.", ArmouryEvolve),
        "You cannot evolve until build timer expires" => d("You cannot evolve until your build timer has expired.", ArmouryEvolve),
        "You cannot evolve while wallwalking" | "This class is not yet available" | "You cannot spawn with that class" => Some(CgMenu::Dialog(None, ArmouryEvolve)),
        "Cheats are not enabled on this server" | "Join a team first" | "You can only use this command when spectating" | "You must be dead to use the class command" => Some(CgMenu::Dialog(None, Command)),
        _ => None,
    }
}

// =====================================================================
// game snapshot (what the ui/cgame VMs read through traps)
// =====================================================================

/// One connected client (CS_PLAYERS configstring: name, team, ignore list).
#[derive(Clone, Debug, Default)]
pub struct ClientView {
    pub num: usize,
    pub name: String,
    pub team: Team,
    pub ignored: Vec<usize>,
}

/// One scoreboard row (cg.scores[], from ScoreboardMessage).
#[derive(Clone, Debug)]
pub struct ScoreRow {
    pub client: usize,
    pub team: Team,
    pub score: i32,
    pub time: i32,
    pub ping: i32,
    pub weapon: i32,
    pub upgrade: Option<UpgradeId>,
}

/// What cgame's scoreboard reads (cg.scores, cgs.clientinfo, configstrings).
#[derive(Clone, Debug, Default)]
pub struct Scoreboard {
    pub rows: Vec<ScoreRow>,
    pub names: Vec<String>,
    pub my_team: Team,
    /// pm_type spectator / noclip / following
    pub spectating: bool,
    pub intermission: bool,
    pub ready: Vec<bool>,
    pub alien_stage: i32,
    pub human_stage: i32,
    pub alien_next: i32,
    pub alien_credits: i32,
    pub human_next: i32,
    pub human_credits: i32,
    /// cg.spectatorList (CG_BuildSpectatorString)
    pub spectators: String,
    /// CS_WINNER
    pub winner: String,
}

/// The simulation as the ui module sees it for one frame.
#[derive(Clone, Debug, Default)]
pub struct GameView {
    pub player: Player,
    pub me: usize,
    pub clients: Vec<ClientView>,
    pub scores: Scoreboard,
    /// cgs.voteTime[ TEAM_NONE / ALIENS / HUMANS ] != 0
    pub votes: [bool; 3],
    /// CS_SERVERINFO keys UI_ServerInfo reads
    pub server_info: Vec<(String, String)>,
    pub mark_deconstruct: bool,
    /// the local client's latest printed message (time, text)
    pub message: Option<(i32, String)>,
}

impl GameView {
    /// Snapshot of the world for local client `me`.
    pub fn from_world(w: &World, me: usize) -> GameView {
        let mut clients = Vec::new();
        let mut rows: Vec<ScoreRow> = Vec::new();
        let mut names = vec![String::new(); MAX_CLIENTS];
        let mut ready = vec![false; MAX_CLIENTS];
        let mut specs = String::new();
        let my_team = w.clients[me].team;
        for i in 0..MAX_CLIENTS {
            let c = &w.clients[i];
            if !c.connected {
                continue;
            }
            clients.push(ClientView { num: i, name: c.name.clone(), team: c.team, ignored: c.ignored.clone() });
            names[i] = c.name.clone();
            ready[i] = c.ready_to_exit;
            if c.team == Team::None {
                // CG_BuildSpectatorString
                specs.push_str(&format!("^7{}     ", c.name));
            }
            // ScoreboardMessage: weapon / upgrade only for team mates (or to spectators)
            let (weapon, upgrade) = if c.spectator == 0 && (my_team == Team::None || c.team == my_team) {
                let order = [UpgradeId::Bsuit, UpgradeId::Jetpack, UpgradeId::Battpack, UpgradeId::Helmet, UpgradeId::Larmour];
                (c.ps.weapon, order.into_iter().find(|u| bg::inventory_contains_upgrade(*u as i32, &c.ps.stats)))
            } else {
                (0, None)
            };
            // ScoreboardMessage: ps.ping capped at 999 (0 in the single-process game)
            rows.push(ScoreRow { client: i, team: c.team, score: c.score, time: (w.level.time - c.enter_time).max(0) / 60000, ping: c.ps.ping.min(999), weapon, upgrade });
        }
        // level.sortedClients (CalculateRanks: by score)
        rows.sort_by_key(|r| -r.score);
        let l = &w.level;
        let me_c = &w.clients[me];
        let winner = match (l.intermission_queued != 0 || l.intermission_time != 0, l.winner) {
            (true, Some(Team::Aliens)) => "Aliens Win",
            (true, Some(Team::Humans)) => "Humans Win",
            (true, _) => "Stalemate",
            _ => "",
        };
        let scores = Scoreboard {
            rows,
            names,
            my_team,
            spectating: me_c.spectator != 0 || me_c.team == Team::None,
            intermission: l.intermission_time != 0,
            ready,
            alien_stage: l.alien_stage,
            human_stage: l.human_stage,
            alien_next: l.alien_next_stage,
            alien_credits: l.alien_credits,
            human_next: l.human_next_stage,
            human_credits: l.human_credits,
            spectators: specs,
            winner: winner.into(),
        };
        let vt = |i: usize| l.votes[i].time != 0;
        let server_info = vec![
            ("mapname".to_string(), w.map_name.clone()),
            ("timelimit".into(), l.timelimit.to_string()),
            ("g_suddenDeathTime".into(), l.sudden_death_time.to_string()),
            ("sv_hostname".into(), "noname".into()),
            ("sv_maxclients".into(), MAX_CLIENTS.to_string()),
            ("version".into(), "tremulous-legacy (Godot + Rust port)".into()),
            ("g_unlagged".into(), "1".into()),
            ("g_friendlyFire".into(), (l.friendly_fire as i32).to_string()),
            ("g_friendlyBuildableFire".into(), "0".into()),
            ("sv_allowdownload".into(), "0".into()),
        ];
        GameView {
            player: Player::from_world(w, me),
            me,
            clients,
            scores,
            votes: [vt(0), vt(1), vt(2)],
            server_info,
            mark_deconstruct: l.mark_deconstruct != 0,
            message: me_c.message.clone(),
        }
    }
    fn info(&self, key: &str) -> String {
        self.server_info.iter().find(|(k_, _)| k_.eq_ignore_ascii_case(key)).map(|(_, v)| v.clone()).unwrap_or_default()
    }
}

// =====================================================================
// runtime menus (menuDef_t / itemDef_t state)
// =====================================================================

/// `listBoxDef_t` runtime fields.
#[derive(Clone, Copy, Debug, Default)]
pub struct ListState {
    pub start: i32,
    pub end: i32,
    /// listPtr->cursorPos: the row under the mouse
    pub cursor: i32,
    pub last_count: i32,
    pub no_scrollbar: bool,
}

/// `editFieldDef_t`.
#[derive(Clone, Copy, Debug, Default)]
pub struct EditState {
    pub min: f32,
    pub max: f32,
    pub def: f32,
    pub max_chars: i32,
    pub max_paint_chars: i32,
    pub max_field_width: i32,
    pub paint_offset: i32,
}

/// `itemDef_t`: the parsed definition plus its window state.
#[derive(Clone, Debug)]
pub struct UiItem {
    pub def: ItemDef,
    /// legacy ITEM_TYPE_*
    pub ty: i32,
    pub feeder: i32,
    pub flags: u32,
    /// window.rectClient (aspect compensated, relative to the menu)
    pub rect_client: R,
    /// window.rect (screen)
    pub rect: R,
    pub text_align_x: f32,
    pub fore: Rgba,
    pub back: Rgba,
    pub border_color: Rgba,
    pub background: Option<String>,
    pub text_rect: R,
    pub cursor_pos: i32,
    pub list: ListState,
    pub edit: EditState,
    pub cycle_cursor: i32,
    pub cvar_flags: u32,
    pub enable_cvar: Vec<String>,
    /// window.nextTime / offsetTime (fades, transitions, orbits, model rotation)
    pub next_time: i32,
    pub offset_time: i32,
    /// window.rectEffects / rectEffects2 (transition target and steps, orbit centre)
    pub rect_effects: R,
    pub rect_effects2: R,
    /// modelDef_t.angle (advanced by rotationSpeed)
    pub model_angle: i32,
}

impl UiItem {
    /// `Item_Init` + the type data `Item_Parse` fills (ItemParse_type,
    /// _cvar, _cvarFloat, _maxChars, _enableCvar ...).
    fn new(mut def: ItemDef, no_scrollbar: bool) -> UiItem {
        // PC_String_Parse: the precompiler's string escapes (joinserver's
        // legend has a "\n")
        if let Some(t) = def.text.as_mut().filter(|t| t.contains('\\')) {
            *t = unescape(t);
        }
        // ItemParse_ownerdraw sets ITEM_TYPE_OWNERDRAW (legacy numbering)
        let ty = def.item_type;
        let mut flags = 0;
        if def.visible {
            flags |= WINDOW_VISIBLE;
        }
        if def.decoration {
            flags |= WINDOW_DECORATION;
        }
        if def.wrapped || def.autowrapped {
            flags |= WINDOW_WRAPPED;
        }
        if def.fore_set {
            flags |= WINDOW_FORECOLORSET;
        }
        if def.back_set {
            flags |= WINDOW_BACKCOLORSET;
        }
        let mut edit = EditState::default();
        if matches!(ty, IT_EDITFIELD | IT_SAYFIELD) {
            edit.max_paint_chars = MAX_EDITFIELD;
        }
        if def.max_paint_chars != 0 {
            edit.max_paint_chars = def.max_paint_chars;
        }
        edit.max_chars = def.max_chars;
        edit.max_field_width = def.max_field_width;
        if def.cvar.is_some() && is_edit_data(ty) {
            edit.min = -1.0;
            edit.max = -1.0;
            edit.def = -1.0;
        }
        if let Some([d, mn, mx]) = def.cvar_float {
            edit.def = d;
            edit.min = mn;
            edit.max = mx;
        }
        let (mut cvar_flags, mut enable_cvar) = (0, Vec::new());
        for (key, vals) in &def.cvar_flags {
            cvar_flags = match key.as_str() {
                "enablecvar" => CVAR_ENABLE,
                "disablecvar" => CVAR_DISABLE,
                "showcvar" => CVAR_SHOW,
                _ => CVAR_HIDE,
            };
            enable_cvar = vals.clone();
        }
        UiItem {
            ty,
            feeder: def.feeder as i32,
            flags,
            rect_client: def.rect,
            rect: def.rect,
            text_align_x: def.text_align_x,
            fore: def.fore,
            back: def.back,
            border_color: def.border_color,
            background: def.background.clone(),
            text_rect: [0.0; 4],
            cursor_pos: 0,
            list: ListState { no_scrollbar, ..Default::default() },
            edit,
            cycle_cursor: 0,
            cvar_flags,
            enable_cvar,
            next_time: 0,
            offset_time: 0,
            rect_effects: [0.0; 4],
            rect_effects2: [0.0; 4],
            model_angle: def.model_angle,
            def,
        }
    }
}

/// `Item_DataType( item ) == TYPE_EDIT`.
fn is_edit_data(ty: i32) -> bool {
    matches!(ty, IT_EDITFIELD | IT_NUMERICFIELD | IT_SAYFIELD | IT_YESNO | IT_BIND | IT_SLIDER | IT_TEXT)
}

/// `Item_IsEditField`.
fn is_edit_field(ty: i32) -> bool {
    matches!(ty, IT_EDITFIELD | IT_NUMERICFIELD | IT_SAYFIELD)
}

/// `Item_IsListBox`.
fn is_list_box(ty: i32) -> bool {
    matches!(ty, IT_LISTBOX | IT_COMBOBOX)
}

/// `menuDef_t`.
#[derive(Clone, Debug)]
pub struct UiMenu {
    /// the parsed menu (items moved into `items`)
    pub def: MenuDef,
    pub rect: R,
    pub flags: u32,
    pub cursor_item: i32,
    pub items: Vec<UiItem>,
    /// fadeAmount / fadeClamp / fadeCycle (`Menu_Init`: the assetGlobalDef
    /// values unless the menu sets its own)
    pub fade: (f32, f32, i32),
}

impl UiMenu {
    /// `Menu_Init` + `Menu_Parse` + `Menu_PostParse` (listboxes pushed to the end).
    fn new(mut def: MenuDef, no_scrollbar: &[bool]) -> UiMenu {
        let defs = std::mem::take(&mut def.items);
        let mut items: Vec<UiItem> = defs.into_iter().enumerate().map(|(i, d)| UiItem::new(d, no_scrollbar.get(i).copied().unwrap_or(false))).collect();
        if def.fullscreen {
            def.rect = [0.0, 0.0, 640.0, 480.0];
        }
        // push lists to the end of the array as they can potentially be drawn
        // on top of other elements
        // (as in the C, the item shifted into slot i is not looked at again)
        for i in 0..items.len() {
            if is_list_box(items[i].ty) {
                let it = items.remove(i);
                items.push(it);
            }
        }
        let mut flags = 0;
        if def.visible {
            flags |= WINDOW_VISIBLE;
        }
        if def.popup {
            flags |= WINDOW_POPUP;
        }
        if def.oob_click {
            flags |= WINDOW_OOB_CLICK;
        }
        let rect = def.rect;
        let fade = (def.fade_amount, def.fade_clamp, def.fade_cycle);
        let mut m = UiMenu { def, rect, flags, cursor_item: -1, items, fade };
        m.layout(1.0);
        m
    }

    pub fn name(&self) -> &str {
        &self.def.name
    }

    /// `Menu_AspectCompensate` + `Menu_UpdatePosition`, recomputed from the
    /// parsed rects for display aspect `aspect` (DC->aspectScale).
    pub fn layout(&mut self, aspect: f32) {
        let mut rect = self.def.rect;
        if self.def.aspect_bias != ASPECT_NONE {
            hud::aspectise_rect(self.def.aspect_bias, &mut rect, aspect);
            for it in &mut self.items {
                it.rect_client = it.def.rect;
                it.rect_client[0] *= aspect;
                it.rect_client[2] *= aspect;
                it.text_align_x = it.def.text_align_x * aspect;
            }
        } else {
            for it in &mut self.items {
                it.rect_client = it.def.rect;
                hud::aspectise_rect(it.def.aspect_bias, &mut it.rect_client, aspect);
                it.text_align_x = if it.def.aspect_bias != ASPECT_NONE { it.def.text_align_x * aspect } else { it.def.text_align_x };
            }
        }
        self.rect = rect;
        self.update_position();
    }

    /// `Menu_UpdatePosition` (`Item_SetScreenCoords` for every item).
    fn update_position(&mut self) {
        let (mut x, mut y) = (self.rect[0], self.rect[1]);
        if self.def.border != 0 {
            x += self.def.border_size;
            y += self.def.border_size;
        }
        for it in &mut self.items {
            let (mut ix, mut iy) = (x, y);
            if it.def.border != 0 {
                ix += it.def.border_size;
                iy += it.def.border_size;
            }
            it.rect = [ix + it.rect_client[0], iy + it.rect_client[1], it.rect_client[2], it.rect_client[3]];
            // force the text rects to recompute
            it.text_rect[2] = 0.0;
            it.text_rect[3] = 0.0;
        }
    }

    /// `Menu_FindItemByName`.
    fn find_item(&self, name: &str) -> Option<usize> {
        self.items.iter().position(|i| i.def.name.eq_ignore_ascii_case(name))
    }

    /// `Menu_ItemsMatchingGroup` / `Menu_GetMatchingItemByNumber`: items whose
    /// name or group is `name`.
    fn matching(&self, name: &str) -> Vec<usize> {
        (0..self.items.len()).filter(|&i| self.items[i].def.name.eq_ignore_ascii_case(name) || self.items[i].def.group.as_deref().is_some_and(|g| g.eq_ignore_ascii_case(name))).collect()
    }
}

// =====================================================================
// loading (UI_Load / UI_LoadMenus / UI_ParseMenu)
// =====================================================================

/// The `noscrollbar` flag of every itemDef of a menu file, in file order
/// (the shared parser accepts the keyword without storing it).
fn scan_noscrollbar(text: &str, read: &mut dyn FnMut(&str) -> Option<String>) -> Vec<bool> {
    let mut toks = Vec::new();
    let mut defs = HashMap::new();
    if hud::lex(text, &mut defs, read, &mut toks, 0).is_err() {
        return Vec::new();
    }
    let mut out: Vec<bool> = Vec::new();
    let mut in_item = false;
    for t in &toks {
        if let Tok::Word(w) = t {
            if w.eq_ignore_ascii_case("itemdef") {
                out.push(false);
                in_item = true;
            } else if w.eq_ignore_ascii_case("menudef") {
                in_item = false;
            } else if in_item && w.eq_ignore_ascii_case("noscrollbar") {
                if let Some(l) = out.last_mut() {
                    *l = true;
                }
            }
        }
    }
    out
}

/// `UI_ParseMenu` for each file: the menus (in order) and the menudef.h
/// defines (feeder / ownerdraw numbers).
pub fn load_menu_files(files: &[String], read: &mut dyn FnMut(&str) -> Option<String>) -> Result<(Vec<UiMenu>, HashMap<String, String>), String> {
    let mut menus = Vec::new();
    let mut defines = HashMap::new();
    for f in files {
        let text = read(f).ok_or_else(|| format!("{f}: not found"))?;
        let mut set = MenuSet::default();
        hud::parse_menus_into(&mut set, &text, read).map_err(|e| format!("{f}: {e}"))?;
        let noscroll = scan_noscrollbar(&text, read);
        let mut k_ = 0;
        for m in set.menus {
            let n = m.items.len();
            let flags: Vec<bool> = (0..n).map(|i| noscroll.get(k_ + i).copied().unwrap_or(false)).collect();
            k_ += n;
            menus.push(UiMenu::new(m, &flags));
        }
        defines.extend(set.defines);
    }
    Ok((menus, defines))
}

/// The files of the menu lists (`UI_LoadMenus` -> `loadMenu { ... }`).
pub fn menu_list_files(read: &mut dyn FnMut(&str) -> Option<String>) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    for l in MENU_LISTS {
        let text = read(l).ok_or_else(|| format!("menu list '{l}' not found"))?;
        out.extend(hud::parse_menu_list(&text));
    }
    Ok(out)
}

/// `scripts/*.arena` (`UI_LoadArenasFromFile` / `UI_ParseInfos`): (map, longname).
pub fn parse_arenas(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut toks = Vec::new();
    let mut defs = HashMap::new();
    if hud::lex(text, &mut defs, &mut |_| None, &mut toks, 0).is_err() {
        return out;
    }
    let s = |t: &Tok| match t {
        Tok::Word(w) | Tok::Str(w) => w.clone(),
        _ => String::new(),
    };
    let mut i = 0;
    while i < toks.len() {
        if toks[i] == Tok::Open {
            let (mut map, mut long) = (String::new(), String::new());
            i += 1;
            while i + 1 < toks.len() && toks[i] != Tok::Close {
                let key = s(&toks[i]).to_ascii_lowercase();
                let val = s(&toks[i + 1]);
                match key.as_str() {
                    "map" => map = val,
                    "longname" => long = val,
                    _ => {}
                }
                i += 2;
            }
            if !map.is_empty() {
                out.push((map, long));
            }
        }
        i += 1;
    }
    out
}

/// The FEEDER_* lists ui_main.c (and cgame for the team lists) feed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Feeder {
    Maps,
    AlienTeamList,
    HumanTeamList,
    TeamList,
    PlayerList,
    Teams,
    AlienClasses,
    HumanItems,
    ArmouryBuy,
    ArmourySell,
    AlienUpgrade,
    AlienBuild,
    HumanBuild,
    IgnoreList,
    HelpList,
    Resolutions,
    Servers,
    News,
    Mods,
    Demos,
    ServerStatus,
    FindPlayer,
    Cinematics,
}

const FEEDERS: &[(&str, Feeder)] = &[
    ("FEEDER_MAPS", Feeder::Maps),
    ("FEEDER_ALIENTEAM_LIST", Feeder::AlienTeamList),
    ("FEEDER_HUMANTEAM_LIST", Feeder::HumanTeamList),
    ("FEEDER_TEAM_LIST", Feeder::TeamList),
    ("FEEDER_PLAYER_LIST", Feeder::PlayerList),
    ("FEEDER_TREMTEAMS", Feeder::Teams),
    ("FEEDER_TREMALIENCLASSES", Feeder::AlienClasses),
    ("FEEDER_TREMHUMANITEMS", Feeder::HumanItems),
    ("FEEDER_TREMHUMANARMOURYBUY", Feeder::ArmouryBuy),
    ("FEEDER_TREMHUMANARMOURYSELL", Feeder::ArmourySell),
    ("FEEDER_TREMALIENUPGRADE", Feeder::AlienUpgrade),
    ("FEEDER_TREMALIENBUILD", Feeder::AlienBuild),
    ("FEEDER_TREMHUMANBUILD", Feeder::HumanBuild),
    ("FEEDER_IGNORE_LIST", Feeder::IgnoreList),
    ("FEEDER_HELP_LIST", Feeder::HelpList),
    ("FEEDER_RESOLUTIONS", Feeder::Resolutions),
    ("FEEDER_SERVERS", Feeder::Servers),
    ("FEEDER_NEWS", Feeder::News),
    ("FEEDER_MODS", Feeder::Mods),
    ("FEEDER_DEMOS", Feeder::Demos),
    ("FEEDER_SERVERSTATUS", Feeder::ServerStatus),
    ("FEEDER_FINDPLAYER", Feeder::FindPlayer),
    ("FEEDER_CINEMATICS", Feeder::Cinematics),
];

/// The ownerdraws of the loaded menus (ui_main.c `UI_OwnerDraw`, cg_draw.c `CG_OwnerDraw`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Od {
    InfoPane(Feeder),
    SelectedMapPreview,
    SelectedMapName,
    GlInfo,
    KeyBindStatus,
    AliensScoreLabel,
    HumansScoreLabel,
    StageReport,
    Spectators,
    NetSource,
    NetMapPreview,
    NetMapCinematic,
    ServerRefreshDate,
    ServerMotd,
}

const OWNERDRAWS: &[(&str, Od)] = &[
    ("UI_TEAMINFOPANE", Od::InfoPane(Feeder::Teams)),
    ("UI_ACLASSINFOPANE", Od::InfoPane(Feeder::AlienClasses)),
    ("UI_AUPGRADEINFOPANE", Od::InfoPane(Feeder::AlienUpgrade)),
    ("UI_HITEMINFOPANE", Od::InfoPane(Feeder::HumanItems)),
    ("UI_HBUYINFOPANE", Od::InfoPane(Feeder::ArmouryBuy)),
    ("UI_HSELLINFOPANE", Od::InfoPane(Feeder::ArmourySell)),
    ("UI_ABUILDINFOPANE", Od::InfoPane(Feeder::AlienBuild)),
    ("UI_HBUILDINFOPANE", Od::InfoPane(Feeder::HumanBuild)),
    ("UI_HELPINFOPANE", Od::InfoPane(Feeder::HelpList)),
    ("UI_SELECTEDMAPPREVIEW", Od::SelectedMapPreview),
    ("UI_SELECTEDMAPNAME", Od::SelectedMapName),
    ("UI_GLINFO", Od::GlInfo),
    ("UI_KEYBINDSTATUS", Od::KeyBindStatus),
    ("CG_ALIENS_SCORE_LABEL", Od::AliensScoreLabel),
    ("CG_HUMANS_SCORE_LABEL", Od::HumansScoreLabel),
    ("CG_STAGE_REPORT_TEXT", Od::StageReport),
    ("CG_SPECTATORS", Od::Spectators),
    ("UI_NETSOURCE", Od::NetSource),
    ("UI_NETMAPPREVIEW", Od::NetMapPreview),
    ("UI_NETMAPCINEMATIC", Od::NetMapCinematic),
    ("UI_SERVERREFRESHDATE", Od::ServerRefreshDate),
    ("UI_SERVERMOTD", Od::ServerMotd),
];

fn define_int(defines: &HashMap<String, String>, name: &str) -> Option<i32> {
    let set = MenuSet { defines: defines.clone(), ..Default::default() };
    set.define_int(name)
}

pub fn feeder_table(defines: &HashMap<String, String>) -> HashMap<i32, Feeder> {
    FEEDERS.iter().filter_map(|(n, f)| define_int(defines, n).map(|id| (id, *f))).collect()
}
pub fn ownerdraw_table(defines: &HashMap<String, String>) -> HashMap<i32, Od> {
    OWNERDRAWS.iter().filter_map(|(n, o)| define_int(defines, n).map(|id| (id, *o))).collect()
}

// =====================================================================
// the display context and menu state (ui_shared.c statics + uiInfo)
// =====================================================================

/// An item: menu set (0 = ui, 1 = cgame), menu, item.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IRef {
    pub set: usize,
    pub menu: usize,
    pub item: usize,
}

/// What the menus ask the engine to do.
#[derive(Clone, Debug, PartialEq)]
pub enum Out {
    /// `trap_Cmd_ExecuteText` text (console commands, `cmd ...`)
    Exec(String),
    /// `trap_S_StartLocalSound`
    Sound(String),
    /// `trap_Cvar_Set` (name, value)
    Cvar(String, String),
    /// `trap_Key_SetBinding` (keycodes.h key number, command)
    Bind(i32, String),
    /// `DC->startBackgroundTrack( intro, loop )` (`trap_S_StartBackgroundTrack`)
    BackgroundTrack(String, String),
    /// `DC->stopBackgroundTrack` (`trap_S_StopBackgroundTrack`)
    StopBackgroundTrack,
    /// `Display_CloseCinematics` / `Menu_CloseCinematics` /
    /// `Window_CloseCinematic` (DC->stopCinematic: `UI_StopCinematic`,
    /// `trap_CIN_StopCinematic`)
    CloseCinematics,
    /// `Com_Printf`
    Print(String),
}

/// The capture functions (`UI_InstallCaptureFunc`).
#[derive(Clone, Copy, Debug)]
enum Capture {
    None,
    /// `Scroll_ListBox_AutoFunc`
    ListAuto { item: IRef, key: K, next_scroll: i32, next_adjust: i32, adjust: i32 },
    /// `Scroll_ListBox_ThumbFunc`
    ListThumb { item: IRef, key: K, y_start: f32, next_scroll: i32, next_adjust: i32, adjust: i32 },
    /// `Scroll_Slider_ThumbFunc`
    Slider { item: IRef },
    /// `UI_ArmouryRefreshCb`
    ArmouryRefresh,
}

/// `UI_BuildPlayerList` results.
#[derive(Clone, Debug, Default)]
struct PlayerList {
    player_number: usize,
    my_index: usize,
    names: Vec<String>,
    client_nums: Vec<usize>,
    ignore: Vec<Vec<usize>>,
    team_names: Vec<String>,
    team_client_nums: Vec<usize>,
}

/// One `serverInfo_t` of the client's LAN lists (cl_ui.c `LAN_GetServerInfo`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ServerInfo {
    pub addr: String,
    pub host_name: String,
    pub map_name: String,
    pub game: String,
    pub clients: i32,
    pub max_clients: i32,
    pub ping: i32,
    pub visible: bool,
    /// the server's protocol (dec040): 71 legacy 1.3, 69 vanilla 1.1.0, 0 unknown
    pub protocol: i32,
}

/// The browser's tag of a server's protocol (dec040).
pub fn protocol_tag(protocol: i32) -> &'static str {
    match protocol {
        69 => "1.1",
        71 => "1.3",
        _ => "",
    }
}

/// The client's server lists behind `trap_LAN_*` (cl_main.c / cl_ui.c):
/// a mirror of the network client's lists (`TremGame::lan_sync`, dec035);
/// favorites are kept here (servercache.dat -> `favorites` the Godot side
/// saves) and handed to the client for pings.
#[derive(Clone, Debug, Default)]
pub struct Lan {
    pub local: Vec<ServerInfo>,
    /// cls.numglobalservers: -1 while waiting for a master's answer
    pub global_count: i32,
    pub global: Vec<ServerInfo>,
    pub favorites: Vec<ServerInfo>,
    /// a favorite was added / removed (LAN_SaveServersToCache)
    pub favorites_changed: bool,
    /// the game has a network host (dec033): the lists mirror its client's
    pub net: bool,
    /// `LAN_UpdateVisiblePings` asked for this source; the host pings
    pub want_pings: Option<i32>,
    /// ping requests still out (cl_pinglist)
    pub outstanding: usize,
    /// `LAN_MarkServerVisible( source, -1, visible )` marks every slot of
    /// the C's fixed arrays, the ones not filled yet too: the visible flag a
    /// server arriving later gets, per source (local, global, favorites)
    pub slot_visible: [Option<bool>; 3],
}

impl Lan {
    fn slot(source: i32) -> usize {
        match source {
            AS_LOCAL => 0,
            AS_FAVORITES => 2,
            _ => 1,
        }
    }
    /// The visible flag of a slot not in the list yet (`slot_visible`).
    pub fn new_slot_visible(&self, source: i32) -> Option<bool> {
        self.slot_visible.get(Self::slot(source)).copied().flatten()
    }
    fn list(&self, source: i32) -> &[ServerInfo] {
        match source {
            AS_LOCAL => &self.local,
            AS_FAVORITES => &self.favorites,
            _ => &self.global,
        }
    }
    fn list_mut(&mut self, source: i32) -> &mut Vec<ServerInfo> {
        match source {
            AS_LOCAL => &mut self.local,
            AS_FAVORITES => &mut self.favorites,
            _ => &mut self.global,
        }
    }
    /// `trap_LAN_GetServerCount` (`LAN_GetServerCount`).
    pub fn server_count(&self, source: i32) -> i32 {
        match source {
            AS_LOCAL => self.local.len() as i32,
            AS_FAVORITES => self.favorites.len() as i32,
            _ => self.global_count,
        }
    }
    /// `trap_LAN_GetServerPing` (`LAN_GetServerPing`).
    fn server_ping(&self, source: i32, n: usize) -> i32 {
        self.list(source).get(n).map(|s| s.ping).unwrap_or(-1)
    }
    /// `trap_LAN_MarkServerVisible` (`LAN_MarkServerVisible`) (n < 0: every server).
    fn mark_server_visible(&mut self, source: i32, n: i32, visible: bool) {
        if n < 0 {
            if let Some(v) = self.slot_visible.get_mut(Self::slot(source)) {
                *v = Some(visible);
            }
        }
        let l = self.list_mut(source);
        if n < 0 {
            for s in l.iter_mut() {
                s.visible = visible;
            }
        } else if let Some(s) = l.get_mut(n as usize) {
            s.visible = visible;
        }
    }
    /// `trap_LAN_ServerIsVisible` (`LAN_ServerIsVisible`).
    fn server_is_visible(&self, source: i32, n: usize) -> bool {
        self.list(source).get(n).is_some_and(|s| s.visible)
    }
    /// `trap_LAN_ResetPings` (`LAN_ResetPings`).
    fn reset_pings(&mut self, source: i32) {
        for s in self.list_mut(source).iter_mut() {
            s.ping = -1;
        }
    }
    /// `trap_LAN_UpdateVisiblePings` (`CL_UpdateVisiblePings_f`): the host
    /// sends the pings (`TremGame::lan_sync`); true while servers wait
    /// for one or answers are out. Without a network host nothing is sent.
    fn update_visible_pings(&mut self, source: i32) -> bool {
        if !self.net {
            return false;
        }
        self.want_pings = Some(source);
        self.outstanding > 0 || self.list(source).iter().any(|s| s.visible && s.ping == -1)
    }
    /// `trap_LAN_AddServer` (`LAN_AddServer`): 1 added, 0 already there, -1 list full.
    pub fn add_server(&mut self, source: i32, name: &str, addr: &str) -> i32 {
        let l = self.list_mut(source);
        if l.len() >= MAX_OTHER_SERVERS {
            return -1;
        }
        if l.iter().any(|s| s.addr.eq_ignore_ascii_case(addr)) {
            return 0;
        }
        l.push(ServerInfo { addr: addr.to_string(), host_name: name.to_string(), visible: true, ..Default::default() });
        if source == AS_FAVORITES {
            self.favorites_changed = true;
        }
        1
    }
    /// `trap_LAN_RemoveServer` (`LAN_RemoveServer`).
    pub fn remove_server(&mut self, source: i32, addr: &str) {
        let l = self.list_mut(source);
        let n = l.len();
        l.retain(|s| !s.addr.eq_ignore_ascii_case(addr));
        if source == AS_FAVORITES && l.len() != n {
            self.favorites_changed = true;
        }
    }
}

/// `uiInfo.serverStatus` (the browser's display list) and the news /
/// find-player / server-status state of `uiInfo`.
#[derive(Clone, Debug, Default)]
pub struct BrowserState {
    pub refresh_active: bool,
    pub refresh_time: i32,
    pub next_display_refresh: i32,
    pub display_servers: Vec<usize>,
    pub num_players_on_servers: i32,
    pub sort_key: i32,
    pub sort_dir: bool,
    pub sorted: bool,
    pub motd: String,
    pub current_server: i32,
    pub current_server_preview: Option<String>,
    /// `uiInfo.newsInfo`
    pub news: Vec<String>,
    pub news_active: bool,
    pub news_refresh_time: i32,
    /// `uiInfo.foundPlayerServerNames` / addresses
    pub found_names: Vec<String>,
    pub found_addrs: Vec<String>,
    pub current_found: i32,
    pub next_find_player_refresh: i32,
    /// `uiInfo.serverStatusInfo` lines (4 columns)
    pub status_lines: Vec<[String; 4]>,
    pub status_address: String,
    pub next_server_status_refresh: i32,
}

/// `Q_CleanStr`: colour codes and non-printable characters removed.
fn clean_str(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = String::new();
    let mut i = 0;
    while i < b.len() {
        if is_color_string(b, i) {
            i += 2;
            continue;
        }
        if (0x20..0x7f).contains(&b[i]) {
            out.push(b[i] as char);
        }
        i += 1;
    }
    out
}

fn c_atof(s: &str) -> f32 {
    let s = s.trim_start();
    let end = s.char_indices().take_while(|(i, c)| c.is_ascii_digit() || *c == '.' || ((*c == '-' || *c == '+') && *i == 0)).map(|(i, c)| i + c.len_utf8()).last().unwrap_or(0);
    s[..end].parse::<f32>().unwrap_or(0.0)
}

pub struct Ui {
    /// `Menus[]` of the ui VM (0) and cgame (1: teamscore_menu)
    pub sets: [Vec<UiMenu>; 2],
    /// `menuStack` (openMenuCount = len)
    pub stack: Vec<usize>,
    pub fonts: std::rc::Rc<Fonts>,
    feeders: HashMap<i32, Feeder>,
    ods: HashMap<i32, Od>,
    pub cvars: HashMap<String, String>,
    pub view: GameView,
    pub out: Vec<Out>,
    /// DC->aspectScale
    pub aspect: f32,
    /// DC->realTime
    pub real_time: i32,
    /// DC->cursorx / cursory
    pub cursor: (f32, f32),
    /// g_editingField + g_editItem
    editing: Option<IRef>,
    combo: Option<IRef>,
    capture: Capture,
    capture_expiry: i32,
    item_capture: Option<IRef>,
    last_list_click: i32,
    overstrike: bool,
    // uiInfo
    lists: HashMap<Feeder, Vec<Entry>>,
    sel: HashMap<Feeder, i32>,
    pub help: Vec<(String, String)>,
    /// scripts/*.arena (map, longname)
    pub arenas: Vec<(String, String)>,
    map_list: Vec<(String, String)>,
    players: PlayerList,
    player_refresh: i32,
    chat_team: bool,
    carriage: (Vec<WeaponId>, Vec<UpgradeId>),
    /// weapon.cfg `icon` by weapon number (cg_weapons[].weaponIcon)
    pub weapon_icons: Vec<Option<String>>,
    /// UI_GLINFO text
    pub gl_info: String,
    /// r_width / r_height
    pub resolution: (i32, i32),
    // cgame
    pub show_scores: bool,
    scores_first: bool,
    spectator_time: i32,
    spectator_offset: f32,
    /// the client's bind table (`DC->getBindingBuf`), refreshed every frame
    pub key_bindings: Vec<String>,
    /// ui_shared.c `g_bindings[]` with bind1 / bind2
    bindings: Vec<BindT>,
    /// g_waitingForKey / g_bindItem
    waiting_for_key: bool,
    bind_item: Option<IRef>,
    /// the keycodes.h number of the key event being handled
    keynum: i32,
    /// `trap_LAN_*`
    pub lan: Lan,
    pub browser: BrowserState,
    /// `UI_LoadMods` (name, description), `UI_LoadDemos`, `UI_LoadMovies`
    pub mods: Vec<(String, String)>,
    pub demos: Vec<String>,
    pub movies: Vec<String>,
    /// the files `trap_FS_GetFileList` lists (pk3 contents and user://),
    /// and the mod folders `$modlist` lists (name, description)
    pub fs_files: Vec<String>,
    pub mod_dirs: Vec<(String, String)>,
    /// `UI_ParseResolutions` (r_availableModes)
    pub resolutions: Vec<(i32, i32)>,
    /// assetGlobalDef fadeAmount / fadeClamp / fadeCycle
    pub asset_fade: (f32, f32, i32),
    /// uiInfo.mapList[].cinematic: -1 not tried, -2 none (`<map>.roq`)
    map_cinematic: HashMap<usize, i32>,
    /// uiInfo.previewMovie / movieIndex / modIndex / demoIndex
    preview_movie: bool,
    /// `trap_RealTime`: seconds and the local (month 0-11, day, year, hour, minute)
    pub unix_time: i64,
    pub local_time: [i32; 5],
}

/// Default menu cvars, then the cvarTable defaults.
fn default_cvars() -> HashMap<String, String> {
    let mut c = HashMap::new();
    for (n, v) in CVAR_DEFAULTS.iter().chain(ENGINE_CVARS.iter()) {
        c.entry(n.to_ascii_lowercase()).or_insert_with(|| v.to_string());
    }
    c
}

impl Ui {
    pub fn new(fonts: Fonts, ui: Vec<UiMenu>, cg: Vec<UiMenu>, defines: &HashMap<String, String>) -> Ui {
        let mut u = Ui {
            sets: [ui, cg],
            stack: Vec::new(),
            fonts: std::rc::Rc::new(fonts),
            feeders: feeder_table(defines),
            ods: ownerdraw_table(defines),
            cvars: default_cvars(),
            view: GameView::default(),
            out: Vec::new(),
            aspect: 1.0,
            real_time: 0,
            cursor: (320.0, 240.0),
            editing: None,
            combo: None,
            capture: Capture::None,
            capture_expiry: 0,
            item_capture: None,
            last_list_click: 0,
            overstrike: false,
            lists: HashMap::new(),
            sel: HashMap::new(),
            help: Vec::new(),
            arenas: Vec::new(),
            map_list: Vec::new(),
            players: PlayerList::default(),
            player_refresh: 0,
            chat_team: false,
            carriage: (Vec::new(), Vec::new()),
            weapon_icons: Vec::new(),
            gl_info: String::new(),
            resolution: (640, 480),
            show_scores: false,
            scores_first: true,
            spectator_time: 0,
            spectator_offset: 0.0,
            key_bindings: Vec::new(),
            bindings: G_BINDINGS.iter().map(|&(command, d1)| BindT { command, default1: d1, bind1: -1, bind2: -1 }).collect(),
            waiting_for_key: false,
            bind_item: None,
            keynum: 0,
            lan: Lan::default(),
            browser: BrowserState { current_server: 0, ..Default::default() },
            mods: Vec::new(),
            demos: Vec::new(),
            movies: Vec::new(),
            fs_files: Vec::new(),
            mod_dirs: Vec::new(),
            resolutions: Vec::new(),
            asset_fade: (0.1, 1.0, 1),
            map_cinematic: HashMap::new(),
            preview_movie: false,
            unix_time: 0,
            local_time: [0, 1, 1970, 0, 0],
        };
        // UI_Init: Menus_CloseAll after loading (main.menu is `visible`)
        u.menus_close_all();
        u.out.clear();
        u
    }

    /// Re-apply `Menu_AspectCompensate` for a new display aspect
    /// (aspectScale = (640 * vidHeight) / (480 * vidWidth)).
    pub fn set_aspect(&mut self, aspect: f32) {
        if (aspect - self.aspect).abs() < 1e-6 {
            return;
        }
        self.aspect = aspect;
        for set in self.sets.iter_mut() {
            for m in set.iter_mut() {
                m.layout(aspect);
            }
        }
    }

    // ------------------------------------------------------------ access

    fn it(&self, r: IRef) -> &UiItem {
        &self.sets[r.set][r.menu].items[r.item]
    }
    fn it_mut(&mut self, r: IRef) -> &mut UiItem {
        &mut self.sets[r.set][r.menu].items[r.item]
    }
    fn menu_of(&self, r: IRef) -> &UiMenu {
        &self.sets[r.set][r.menu]
    }
    fn iref(set: usize, menu: usize, item: usize) -> IRef {
        IRef { set, menu, item }
    }

    /// `Menus_FindByName` (ui menus).
    pub fn find_menu(&self, name: &str) -> Option<usize> {
        self.sets[0].iter().position(|m| m.name().eq_ignore_ascii_case(name))
    }

    pub fn is_open(&self) -> bool {
        !self.stack.is_empty()
    }

    /// The menu `Menu_GetFocused` returns, for tests / hud.gd.
    pub fn top_menu(&self) -> Option<&str> {
        self.stack.last().map(|&m| self.sets[0][m].name())
    }

    // ------------------------------------------------------------ cvars

    /// `DC->getCVarString`.
    pub fn cvar(&self, name: &str) -> String {
        self.cvars.get(&name.to_ascii_lowercase()).cloned().unwrap_or_default()
    }
    /// `DC->getCVarValue`.
    pub fn cvar_value(&self, name: &str) -> f32 {
        c_atof(&self.cvar(name))
    }
    /// `DC->setCVar`.
    pub fn set_cvar(&mut self, name: &str, value: &str) {
        self.cvars.insert(name.to_ascii_lowercase(), value.to_string());
        self.out.push(Out::Cvar(name.to_string(), value.to_string()));
    }

    fn play(&mut self, s: &str) {
        self.out.push(Out::Sound(s.to_string()));
    }
    fn exec(&mut self, s: &str) {
        self.out.push(Out::Exec(s.to_string()));
    }

    // ------------------------------------------------------------ menus

    /// `Menus_Activate`.
    fn menus_activate(&mut self, mi: usize) {
        let on_top = self.stack.last() == Some(&mi);
        self.sets[0][mi].flags |= WINDOW_HASFOCUS | WINDOW_VISIBLE;
        if !on_top {
            let on_open = self.sets[0][mi].def.on_open.clone();
            self.run_script(0, mi, None, &on_open);
            // the menu's `soundLoop` (`MenuParse_soundLoop`): DC->startBackgroundTrack
            if let Some(sl) = self.sets[0][mi].def.sound_loop.clone().filter(|s| !s.is_empty()) {
                self.out.push(Out::BackgroundTrack(sl.clone(), sl));
            }
            self.display_close_cinematics();
            let (cx, cy) = self.cursor;
            // force the item under the cursor to focus
            self.menu_handle_mouse_move(0, mi, cx, cy);
            // reset selection in listboxes when opened
            for ii in 0..self.sets[0][mi].items.len() {
                let r = Self::iref(0, mi, ii);
                let ty = self.it(r).ty;
                if is_list_box(ty) {
                    let f = self.it(r).feeder;
                    let c = self.feeder_initialise(f);
                    self.it_mut(r).cursor_pos = c;
                    self.list_set_start_pos(r, 0);
                    self.feeder_selection(f, c);
                } else if ty == IT_CYCLE {
                    let f = self.it(r).feeder;
                    let c = self.feeder_initialise(f);
                    self.it_mut(r).cycle_cursor = c;
                }
            }
            if self.stack.len() < 16 {
                self.stack.push(mi);
            }
        }
    }

    /// `Menus_ActivateByName`.
    pub fn menus_activate_by_name(&mut self, name: &str) -> Option<usize> {
        let m = self.find_menu(name);
        if let Some(mi) = m {
            self.menus_activate(mi);
        }
        for (i, menu) in self.sets[0].iter_mut().enumerate() {
            if Some(i) != m && !menu.name().eq_ignore_ascii_case(name) {
                menu.flags &= !WINDOW_HASFOCUS;
            }
        }
        m
    }

    /// `Menus_Close` (`Menu_RunCloseScript` first).
    fn menus_close(&mut self, mi: usize) {
        if self.sets[0][mi].flags & WINDOW_VISIBLE != 0 && !self.sets[0][mi].def.on_close.is_empty() {
            let s = self.sets[0][mi].def.on_close.clone();
            self.run_script(0, mi, None, &s);
        }
        self.sets[0][mi].flags &= !(WINDOW_VISIBLE | WINDOW_HASFOCUS);
        // (the C pops the top of the stack whichever menu closes)
        self.stack.pop();
        if let Some(&top) = self.stack.last() {
            self.menus_activate(top);
        }
    }

    /// `Menus_CloseByName`.
    pub fn menus_close_by_name(&mut self, name: &str) {
        if let Some(mi) = self.find_menu(name) {
            self.menus_close(mi);
        }
    }

    /// `Menus_CloseAll`.
    pub fn menus_close_all(&mut self) {
        while let Some(&top) = self.stack.last() {
            self.menus_close(top);
        }
        for mi in 0..self.sets[0].len() {
            let before = self.stack.len();
            self.menus_close(mi);
            // closing a menu that is not open must not pop anything here
            debug_assert!(before == 0 || self.stack.len() <= before);
        }
        self.stack.clear();
        self.editing = None;
        self.waiting_for_key = false;
        self.combo = None;
    }

    /// `Menu_GetFocused`.
    fn menu_get_focused(&self) -> Option<usize> {
        self.sets[0].iter().position(|m| m.flags & WINDOW_HASFOCUS != 0 && m.flags & WINDOW_VISIBLE != 0)
    }

    /// `Menus_AnyFullScreenVisible`.
    fn any_fullscreen_visible(&self) -> bool {
        self.sets[0].iter().any(|m| m.flags & WINDOW_VISIBLE != 0 && m.def.fullscreen)
    }

    /// `Menu_ClearFocus`: returns the item that had it.
    fn menu_clear_focus(&mut self, set: usize, mi: usize) -> Option<usize> {
        let mut ret = None;
        for ii in 0..self.sets[set][mi].items.len() {
            if self.sets[set][mi].items[ii].flags & WINDOW_HASFOCUS != 0 {
                ret = Some(ii);
            }
            self.sets[set][mi].items[ii].flags &= !WINDOW_HASFOCUS;
            let lf = self.sets[set][mi].items[ii].def.leave_focus.clone();
            if !lf.is_empty() {
                self.run_script(set, mi, Some(ii), &lf);
            }
        }
        ret
    }

    /// `Menu_ShowItemByName`.
    fn menu_show_item_by_name(&mut self, set: usize, mi: usize, name: &str, show: bool) {
        for ii in self.sets[set][mi].matching(name) {
            let it = &mut self.sets[set][mi].items[ii];
            if show {
                it.flags |= WINDOW_VISIBLE;
            } else {
                it.flags &= !WINDOW_VISIBLE;
            }
        }
    }

    // ------------------------------------------------------------ scripts

    /// `Item_RunScript`: the commands of `commandList` and, for any other
    /// word, `UI_RunMenuScript` (`uiScript name` reaches it that way).
    fn run_script(&mut self, set: usize, mi: usize, item: Option<usize>, script: &Script) {
        let mut toks: Vec<String> = Vec::new();
        for c in script {
            toks.extend(c.iter().cloned());
            toks.push(";".into());
        }
        let mut p = 0usize;
        let next = |p: &mut usize| -> Option<String> {
            let t = toks.get(*p).cloned();
            *p += 1;
            t
        };
        let float = |p: &mut usize| -> Option<f32> {
            let t = toks.get(*p).cloned();
            *p += 1;
            t.and_then(|t| t.parse::<f32>().ok())
        };
        loop {
            // menus may have closed / reopened under us; stop if gone
            if mi >= self.sets[set].len() {
                return;
            }
            let Some(cmd) = next(&mut p) else { return };
            if cmd == ";" {
                continue;
            }
            match cmd.to_ascii_lowercase().as_str() {
                "close" => {
                    if let Some(n) = next(&mut p) {
                        self.menus_close_by_name(&n);
                    }
                }
                "conditionalopen" => {
                    if let (Some(cv), Some(n1), Some(n2)) = (next(&mut p), next(&mut p), next(&mut p)) {
                        let name = if self.cvar_value(&cv) == 0.0 { n2 } else { n1 };
                        self.menus_activate_by_name(&name);
                    }
                }
                "exec" => {
                    if let Some(v) = next(&mut p) {
                        self.exec(&format!("{v} ; "));
                    }
                }
                "show" => {
                    if let Some(n) = next(&mut p) {
                        self.menu_show_item_by_name(set, mi, &n, true);
                    }
                }
                "hide" => {
                    if let Some(n) = next(&mut p) {
                        self.menu_show_item_by_name(set, mi, &n, false);
                    }
                }
                // `Script_FadeIn` / `Script_FadeOut`
                "fadein" | "fadeout" => {
                    if let Some(n) = next(&mut p) {
                        self.menu_fade_item_by_name(set, mi, &n, cmd.eq_ignore_ascii_case("fadeout"));
                    }
                }
                "open" => {
                    if let Some(n) = next(&mut p) {
                        self.menus_activate_by_name(&n);
                    }
                }
                "orbit" => {
                    // `Script_Orbit`: name x y cx cy time
                    if let Some(n) = next(&mut p) {
                        let v: Vec<Option<f32>> = (0..5).map(|_| float(&mut p)).collect();
                        if let [Some(x), Some(y), Some(cx), Some(cy), Some(t)] = v[..] {
                            self.menu_orbit_item_by_name(set, mi, &n, x, y, cx, cy, t as i32);
                        }
                    }
                }
                "play" => {
                    if let Some(v) = next(&mut p) {
                        self.play(&v);
                    }
                }
                "playlooped" => {
                    // `Script_playLooped`
                    if let Some(v) = next(&mut p) {
                        self.out.push(Out::StopBackgroundTrack);
                        self.out.push(Out::BackgroundTrack(v.clone(), v));
                    }
                }
                // `Script_SetAsset` / `Script_SetPlayerModel` / `Script_SetPlayerHead`
                // do nothing in legacy
                "setasset" | "setplayerhead" | "setplayermodel" => {
                    next(&mut p);
                }
                "reset" => {
                    if let Some(n) = next(&mut p) {
                        if let Some(ii) = self.sets[set][mi].find_item(&n) {
                            let r = Self::iref(set, mi, ii);
                            if is_list_box(self.it(r).ty) {
                                let f = self.it(r).feeder;
                                let c = self.feeder_initialise(f);
                                self.it_mut(r).cursor_pos = c;
                                self.list_set_start_pos(r, 0);
                                self.feeder_selection(f, c);
                            }
                        }
                    }
                }
                "setbackground" => {
                    if let (Some(n), Some(ii)) = (next(&mut p), item) {
                        self.sets[set][mi].items[ii].background = Some(n);
                    }
                }
                "setcolor" => {
                    let which = next(&mut p).unwrap_or_default().to_ascii_lowercase();
                    let mut c = [0.0; 4];
                    let mut ok = true;
                    for v in c.iter_mut() {
                        match float(&mut p) {
                            Some(f) => *v = f,
                            None => ok = false,
                        }
                    }
                    if let (true, Some(ii)) = (ok, item) {
                        let it = &mut self.sets[set][mi].items[ii];
                        match which.as_str() {
                            "backcolor" => {
                                it.back = c;
                                it.flags |= WINDOW_BACKCOLORSET;
                            }
                            "forecolor" => {
                                it.fore = c;
                                it.flags |= WINDOW_FORECOLORSET;
                            }
                            "bordercolor" => it.border_color = c,
                            _ => {}
                        }
                    }
                }
                "setcvar" => {
                    if let (Some(c), Some(v)) = (next(&mut p), next(&mut p)) {
                        self.set_cvar(&c, &v);
                    }
                }
                "setfocus" => {
                    if let Some(n) = next(&mut p) {
                        self.script_set_focus(set, mi, &n);
                    }
                }
                "setitemcolor" => {
                    let (name, which) = (next(&mut p).unwrap_or_default(), next(&mut p).unwrap_or_default().to_ascii_lowercase());
                    let mut c = [0.0; 4];
                    let mut ok = true;
                    for v in c.iter_mut() {
                        match float(&mut p) {
                            Some(f) => *v = f,
                            None => ok = false,
                        }
                    }
                    if ok {
                        for ii in self.sets[set][mi].matching(&name) {
                            let it = &mut self.sets[set][mi].items[ii];
                            match which.as_str() {
                                "backcolor" => it.back = c,
                                "forecolor" => {
                                    it.fore = c;
                                    it.flags |= WINDOW_FORECOLORSET;
                                }
                                "bordercolor" => it.border_color = c,
                                _ => {}
                            }
                        }
                    }
                }
                "transition" => {
                    // `Script_Transition`: name rectFrom rectTo time amt
                    if let Some(n) = next(&mut p) {
                        let v: Vec<Option<f32>> = (0..10).map(|_| float(&mut p)).collect();
                        if v.iter().all(|x| x.is_some()) {
                            let v: Vec<f32> = v.into_iter().flatten().collect();
                            self.menu_transition_item_by_name(set, mi, &n, [v[0], v[1], v[2], v[3]], [v[4], v[5], v[6], v[7]], v[8] as i32, v[9]);
                        }
                    }
                }
                _ => {
                    // DC->runScript
                    let Some(name) = next(&mut p) else { return };
                    let arg = if name.eq_ignore_ascii_case("update") {
                        next(&mut p)
                    } else if name.eq_ignore_ascii_case("serversort") || name.eq_ignore_ascii_case("initserverlist") {
                        // Int_Parse( args, &sortColumn )
                        match toks.get(p).filter(|t| t.parse::<i32>().is_ok()) {
                            Some(t) => {
                                p += 1;
                                Some(t.clone())
                            }
                            None => None,
                        }
                    } else {
                        None
                    };
                    self.run_menu_script(&name, arg.as_deref());
                }
            }
        }
    }

    /// `Script_SetFocus`.
    fn script_set_focus(&mut self, set: usize, mi: usize, name: &str) {
        let Some(ii) = self.sets[set][mi].find_item(name) else { return };
        if self.sets[set][mi].items[ii].flags & WINDOW_DECORATION != 0 {
            return;
        }
        self.menu_clear_focus(set, mi);
        self.sets[set][mi].items[ii].flags |= WINDOW_HASFOCUS;
        let of = self.sets[set][mi].items[ii].def.on_focus.clone();
        if !of.is_empty() {
            self.run_script(set, mi, Some(ii), &of);
        }
        // edit fields get activated too
        if is_edit_field(self.sets[set][mi].items[ii].ty) {
            self.editing = Some(Self::iref(set, mi, ii));
        }
        self.play(ITEM_FOCUS_SOUND);
    }

    fn list(&self, f: Feeder) -> &[Entry] {
        self.lists.get(&f).map(|v| v.as_slice()).unwrap_or(&[])
    }

    fn selected_cmd(&self, f: Feeder) -> Option<String> {
        let i = *self.sel.get(&f).unwrap_or(&0);
        self.list(f).get(i.max(0) as usize).map(|e| e.cmd.clone())
    }

    /// `UI_ParseCarriageList` (ui_carriage), remembered for the refresh callback.
    fn parse_carriage(&mut self) {
        self.carriage = carriage(&self.view.player);
    }

    /// `UI_RunMenuScript`.
    fn run_menu_script(&mut self, name: &str, arg: Option<&str>) {
        let p = self.view.player.clone();
        let exec_sel = |ui: &mut Ui, f: Feeder| {
            if let Some(c) = ui.selected_cmd(f) {
                ui.exec(&format!("cmd {c}\n"));
            }
        };
        match name.to_ascii_lowercase().as_str() {
            "loadarenas" => {
                // `UI_LoadArenas` (sorted by long name: `UI_MapNameCompare`) +
                // `Menu_SetFeederSelection`( NULL, FEEDER_MAPS, 0, "createserver" )
                let mut l: Vec<(String, String)> = self.arenas.clone();
                l.sort_by_key(|(_, n)| n.to_ascii_lowercase());
                self.map_list = l;
                self.map_cinematic.clear();
                self.menu_set_feeder_selection(None, Feeder::Maps, 0, Some("createserver"));
            }
            "startserver" => {
                let d = (self.cvar_value("ui_dedicated") as i32).clamp(0, 2);
                self.set_cvar("dedicated", &d.to_string());
                // the server the menu creates is a network (listen) server (dec033)
                self.set_cvar("net_enabled", "1");
                let s = self.cvar_value("ui_selectedMap") as i32;
                if let Some(m) = self.map_list.get(s.max(0) as usize).map(|m| m.0.clone()) {
                    self.exec(&format!("wait ; wait ; map {m}\n"));
                }
            }
            "getnews" => self.update_news(true),
            "downloadignore" => self.set_cvar("com_downloadPrompt", "1"),
            "downloadcurl" => self.set_cvar("com_downloadPrompt", "2"),
            "downloadudp" => self.set_cvar("com_downloadPrompt", "4"),
            "refreshservers" => {
                self.start_server_refresh(true);
                self.build_server_display_list(1);
            }
            "initserverlist" => {
                let time = self.unix_time;
                // set up default sorting
                if let (false, Some(c)) = (self.browser.sorted, arg.and_then(|a| a.parse::<i32>().ok())) {
                    self.browser.sort_key = c;
                    self.browser.sort_dir = false;
                }
                // refresh if older than 3 days or if list is empty
                let src = self.net_source();
                let last = c_atof(&self.cvar(&format!("ui_lastServerRefresh_{src}_time"))) as i64;
                if self.lan.server_count(src) < 1 || time - last > 3600 {
                    self.start_server_refresh(true);
                    self.build_server_display_list(1);
                }
            }
            "refreshfilter" => {
                self.start_server_refresh(false);
                self.build_server_display_list(1);
            }
            "loaddemos" => self.load_demos(),
            "loadmovies" => self.load_movies(),
            "loadmods" => self.load_mods(),
            // the port's voice page (dec053): the microphone list again,
            // "Default" if the chosen device is gone, and the capture
            // restarted on it (TremGame's voip_refreshdevices)
            "voiprefreshdevices" => {
                let pairs = voice_device_pairs(&crate::voipsnd::VoipSound::input_devices());
                for it in self.sets[0].iter_mut().flat_map(|m| m.items.iter_mut()) {
                    if it.def.cvar.as_deref().is_some_and(|c| c.eq_ignore_ascii_case("s_voipInputDevice")) {
                        it.def.multi = pairs.clone();
                    }
                }
                if !pairs.iter().any(|(_, v)| *v == self.cvar("s_voipInputDevice")) {
                    self.set_cvar("s_voipInputDevice", "Default");
                }
                self.exec("voip_refreshdevices\n");
            }
            "playmovie" => {
                if let Some(m) = self.movies.get(self.selection(Feeder::Cinematics).max(0) as usize).cloned() {
                    self.preview_movie = false;
                    self.exec(&format!("cinematic {m}.roq 2\n"));
                }
            }
            "runmod" => {
                if let Some(m) = self.mods.get(self.selection(Feeder::Mods).max(0) as usize).map(|m| m.0.clone()) {
                    self.set_cvar("fs_game", &m);
                    self.exec("vid_restart;");
                }
            }
            "rundemo" => {
                if let Some(d) = self.demos.get(self.selection(Feeder::Demos).max(0) as usize).cloned() {
                    self.exec(&format!("demo {d}\n"));
                }
            }
            "tremulous" => {
                self.set_cvar("fs_game", "");
                self.exec("vid_restart;");
            }
            "closejoin" => {
                if self.browser.refresh_active {
                    self.stop_server_refresh();
                    self.browser.next_display_refresh = 0;
                    self.browser.next_server_status_refresh = 0;
                    self.browser.next_find_player_refresh = 0;
                    self.build_server_display_list(1);
                } else {
                    self.menus_close_by_name("joinserver");
                    self.menus_activate_by_name("main");
                }
            }
            "stoprefresh" => {
                self.stop_server_refresh();
                self.browser.next_display_refresh = 0;
                self.browser.next_server_status_refresh = 0;
                self.browser.next_find_player_refresh = 0;
            }
            "updatefilter" => {
                if self.net_source() == AS_LOCAL {
                    self.start_server_refresh(true);
                }
                self.build_server_display_list(1);
                self.feeder_selection_f(Feeder::Servers, 0);
            }
            "serverstatus" => {
                self.browser.status_address = self.current_server_info().map(|s| s.addr).unwrap_or_default();
                self.build_server_status(true);
            }
            "foundplayerserverstatus" => {
                let i = self.browser.current_found.max(0) as usize;
                self.browser.status_address = self.browser.found_addrs.get(i).cloned().unwrap_or_default();
                self.build_server_status(true);
                self.menu_set_feeder_selection(None, Feeder::FindPlayer, 0, None);
            }
            "findplayer" => {
                self.build_find_player_list(true);
                // clear the displayed server status info
                self.browser.status_lines.clear();
                self.menu_set_feeder_selection(None, Feeder::FindPlayer, 0, None);
            }
            "joinserver" => {
                let c = self.browser.current_server;
                if c >= 0 && (c as usize) < self.browser.display_servers.len() {
                    if let Some(si) = self.current_server_info() {
                        // the server's own protocol (dec040): "-69" / "-71"
                        match si.protocol {
                            69 | 71 => self.exec(&format!("connect {} -{}\n", si.addr, si.protocol)),
                            _ => self.exec(&format!("connect {}\n", si.addr)),
                        }
                    }
                }
            }
            "foundplayerjoinserver" => {
                let c = self.browser.current_found;
                if c >= 0 && (c as usize) < self.browser.found_addrs.len() {
                    let a = self.browser.found_addrs[c as usize].clone();
                    self.exec(&format!("connect {a}\n"));
                }
            }
            "serversort" => {
                if let Some(col) = arg.and_then(|a| a.parse::<i32>().ok()) {
                    // if same column we're already sorting on then flip the direction
                    if col == self.browser.sort_key {
                        self.browser.sort_dir = !self.browser.sort_dir;
                    }
                    // make sure we sort again
                    self.servers_sort(col, true);
                    self.browser.sorted = true;
                }
            }
            "addfavorite" => {
                if self.net_source() != AS_FAVORITES {
                    if let Some(si) = self.current_server_info() {
                        if !si.host_name.is_empty() && !si.addr.is_empty() {
                            let res = self.lan.add_server(AS_FAVORITES, &si.host_name, &si.addr);
                            self.favorite_result(res, &si.addr);
                        }
                    }
                }
            }
            "deletefavorite" => {
                if self.net_source() == AS_FAVORITES {
                    if let Some(si) = self.current_server_info().filter(|s| !s.addr.is_empty()) {
                        self.lan.remove_server(AS_FAVORITES, &si.addr);
                    }
                }
            }
            "createfavorite" => {
                if self.net_source() == AS_FAVORITES {
                    let (n, a) = (self.cvar("ui_favoriteName"), self.cvar("ui_favoriteAddress"));
                    if !n.is_empty() && !a.is_empty() {
                        let res = self.lan.add_server(AS_FAVORITES, &n, &a);
                        self.favorite_result(res, &a);
                    }
                }
            }
            "loadserverinfo" => {
                // `UI_ServerInfo`
                for (cv, key) in [
                    ("ui_serverinfo_mapname", "mapname"),
                    ("ui_serverinfo_timelimit", "timelimit"),
                    ("ui_serverinfo_sd", "g_suddenDeathTime"),
                    ("ui_serverinfo_hostname", "sv_hostname"),
                    ("ui_serverinfo_maxclients", "sv_maxclients"),
                    ("ui_serverinfo_version", "version"),
                    ("ui_serverinfo_unlagged", "g_unlagged"),
                    ("ui_serverinfo_friendlyFire", "g_friendlyFire"),
                    ("ui_serverinfo_friendlyBuildableFire", "g_friendlyBuildableFire"),
                    ("ui_serverinfo_allowdl", "sv_allowdownload"),
                ] {
                    let v = self.view.info(key);
                    self.set_cvar(cv, &v);
                }
            }
            "loadteams" => {
                self.lists.insert(Feeder::Teams, load_teams());
            }
            "jointeam" => exec_sel(self, Feeder::Teams),
            "loadhumanitems" => {
                self.lists.insert(Feeder::HumanItems, load_human_items(&p));
            }
            "spawnwithhumanitem" => exec_sel(self, Feeder::HumanItems),
            "loadalienclasses" => {
                self.lists.insert(Feeder::AlienClasses, load_alien_classes(&p));
            }
            "spawnasalienclass" => exec_sel(self, Feeder::AlienClasses),
            "loadhumanarmourybuys" => {
                self.parse_carriage();
                self.lists.insert(Feeder::ArmouryBuy, load_armoury_buys(&p));
            }
            "buyfromarmoury" => {
                exec_sel(self, Feeder::ArmouryBuy);
                self.install_capture(Capture::ArmouryRefresh, 1000);
            }
            "loadhumanarmourysells" => {
                self.parse_carriage();
                self.lists.insert(Feeder::ArmourySell, load_armoury_sells(&p));
            }
            "selltoarmoury" => {
                exec_sel(self, Feeder::ArmourySell);
                self.install_capture(Capture::ArmouryRefresh, 1000);
            }
            "loadalienupgrades" => {
                // ui_currentClass "class credits" (set by CG_Menu MN_A_INFEST)
                let l = load_alien_upgrades(&p);
                let empty = l.is_empty();
                self.lists.insert(Feeder::AlienUpgrade, l);
                // disallow the menu if it would be empty
                if empty {
                    self.menus_close_all();
                }
            }
            "upgradetonewclass" => exec_sel(self, Feeder::AlienUpgrade),
            "loadalienbuilds" => {
                self.parse_carriage();
                self.lists.insert(Feeder::AlienBuild, load_builds(&p, Team::Aliens));
            }
            "buildalienbuildable" => exec_sel(self, Feeder::AlienBuild),
            "loadhumanbuilds" => {
                self.parse_carriage();
                self.lists.insert(Feeder::HumanBuild, load_builds(&p, Team::Humans));
            }
            "buildhumanbuildable" => exec_sel(self, Feeder::HumanBuild),
            "say" => {
                let buffer = self.cvar("ui_sayBuffer");
                if buffer.is_empty() {
                } else if self.cvar_value("ui_chatCommands") != 0.0 && (buffer.starts_with('/') || buffer.starts_with('\\')) {
                    self.exec(&format!("{}\n", &buffer[1..]));
                } else if self.chat_team {
                    // `say_team "%s"`: this port's command tokenizer keeps
                    // quotes, so the text goes unquoted
                    self.exec(&format!("say_team {buffer}\n"));
                } else {
                    self.exec(&format!("say {buffer}\n"));
                }
            }
            "saykeydown" => {
                if self.cvar_value("ui_chatCommands") != 0.0 {
                    let buffer = self.cvar("ui_sayBuffer");
                    let to = if buffer.starts_with('/') || buffer.starts_with('\\') {
                        "say_command"
                    } else if self.chat_team {
                        "say_team"
                    } else {
                        "say"
                    };
                    self.menus_replace_active_by_name(to);
                }
            }
            "quit" => self.exec("quit"),
            "leave" => {
                // trap_Key_SetCatcher( KEYCATCH_UI ) + the main menu
                self.exec("disconnect\n");
                self.menus_close_all();
                self.menus_activate_by_name("main");
            }
            "closeingame" => self.menus_close_all(),
            "votemap" | "votenextmap" => {
                let s = self.cvar_value("ui_selectedMap") as i32;
                if s >= 0 && (s as usize) < self.map_list.len() {
                    let what = if name.eq_ignore_ascii_case("votemap") { "map" } else { "nextmap" };
                    let m = self.map_list[s as usize].0.clone();
                    self.exec(&format!("callvote {what} {m}\n"));
                }
            }
            "votekick" | "votemute" | "voteunmute" => {
                let i = *self.sel.get(&Feeder::PlayerList).unwrap_or(&0);
                if i >= 0 && (i as usize) < self.players.client_nums.len() {
                    let n = self.players.client_nums[i as usize];
                    let reason = self.cvar("ui_reason");
                    match name.to_ascii_lowercase().as_str() {
                        "votekick" => self.exec(&format!("callvote kick {n} {reason}\n")),
                        "votemute" => self.exec(&format!("callvote mute {n} {reason}\n")),
                        _ => self.exec(&format!("callvote unmute {n}\n")),
                    }
                    if !name.eq_ignore_ascii_case("voteunmute") {
                        self.set_cvar("ui_reason", "");
                    }
                }
            }
            "voteteamkick" | "voteteamdenybuild" | "voteteamallowbuild" => {
                let i = *self.sel.get(&Feeder::TeamList).unwrap_or(&0);
                if i >= 0 && (i as usize) < self.players.team_client_nums.len() {
                    let n = self.players.team_client_nums[i as usize];
                    let reason = self.cvar("ui_reason");
                    match name.to_ascii_lowercase().as_str() {
                        "voteteamkick" => self.exec(&format!("callteamvote kick {n} {reason}\n")),
                        "voteteamdenybuild" => self.exec(&format!("callteamvote denybuild {n} {reason}\n")),
                        _ => self.exec(&format!("callteamvote allowbuild {n}\n")),
                    }
                    if !name.eq_ignore_ascii_case("voteteamallowbuild") {
                        self.set_cvar("ui_reason", "");
                    }
                }
            }
            "glcustom" => self.set_cvar("ui_glCustom", "4"),
            "update" => {
                if let Some(a) = arg {
                    self.ui_update(a);
                }
            }
            "initignorelist" => self.build_player_list(),
            "toggleignore" | "ignoreplayer" | "unignoreplayer" => {
                let i = *self.sel.get(&Feeder::IgnoreList).unwrap_or(&0);
                if i >= 0 && (i as usize) < self.players.client_nums.len() {
                    let n = self.players.client_nums[i as usize];
                    let me = self.players.my_index;
                    let has = self.players.ignore.get(me).is_some_and(|l| l.contains(&n));
                    let op = name.to_ascii_lowercase();
                    if has && (op == "toggleignore" || op == "unignoreplayer") {
                        if let Some(l) = self.players.ignore.get_mut(me) {
                            l.retain(|&x| x != n);
                        }
                        self.exec(&format!("unignore {n}\n"));
                    } else if !has && (op == "toggleignore" || op == "ignoreplayer") {
                        if let Some(l) = self.players.ignore.get_mut(me) {
                            l.push(n);
                        }
                        self.exec(&format!("ignore {n}\n"));
                    }
                }
            }
            "savecontrols" => self.controls_set_config(),
            "loadcontrols" => self.controls_get_config(),
            "resetdefaults" => {
                self.exec("exec default.cfg\n");
                self.exec("cvar_restart\n");
                self.controls_set_defaults();
                self.set_cvar("com_introPlayed", "1");
                self.exec("vid_restart\n");
            }
            "clearerror" => self.set_cvar("com_errorMessage", ""),
            other => godot_print_unknown(other),
        }
    }

    /// `UI_Update` (the cvars this port keeps; renderer presets only change
    /// the stored values).
    fn ui_update(&mut self, name: &str) {
        let val = self.cvar_value(name) as i32;
        match name.to_ascii_lowercase().as_str() {
            "ui_setname" => {
                let n = self.cvar("ui_Name");
                self.set_cvar("name", &n);
            }
            "ui_getname" => {
                let n = self.cvar("name");
                self.set_cvar("ui_Name", &n);
            }
            "r_colorbits" => match val {
                0 => {
                    self.set_cvar("r_depthbits", "0");
                    self.set_cvar("r_stencilbits", "0");
                }
                16 => {
                    self.set_cvar("r_depthbits", "16");
                    self.set_cvar("r_stencilbits", "0");
                }
                32 => self.set_cvar("r_depthbits", "24"),
                _ => {}
            },
            "r_lodbias" => match val {
                0 => self.set_cvar("r_subdivisions", "4"),
                1 => self.set_cvar("r_subdivisions", "12"),
                2 => self.set_cvar("r_subdivisions", "20"),
                _ => {}
            },
            "ui_mousepitch" => {
                let v = if val == 0 { "0.022" } else { "-0.022" };
                self.set_cvar("m_pitch", v);
            }
            "ui_setrate" => {
                let rate = self.cvar_value("rate");
                let (mp, dup) = if rate >= 5000.0 {
                    ("30", "1")
                } else if rate >= 4000.0 {
                    // favor less prediction errors when there's packet loss
                    ("15", "2")
                } else {
                    // favor lower bandwidth
                    ("15", "1")
                };
                self.set_cvar("cl_maxpackets", mp);
                self.set_cvar("cl_packetdup", dup);
            }
            "ui_glcustom" => {
                // the Video Quality presets
                let preset: &[(&str, &str)] = match val {
                    // high quality
                    0 => &[("r_subdivisions", "4"), ("r_vertexlight", "0"), ("r_lodbias", "0"), ("r_colorbits", "32"), ("r_depthbits", "24"), ("r_picmip", "0"), ("r_texturebits", "32"), ("r_fastSky", "0"), ("r_inGameVideo", "1"), ("cg_shadows", "1"), ("cg_bounceParticles", "1"), ("r_texturemode", "GL_LINEAR_MIPMAP_LINEAR")],
                    // normal
                    1 => &[("r_subdivisions", "12"), ("r_vertexlight", "0"), ("r_lodbias", "0"), ("r_colorbits", "0"), ("r_depthbits", "24"), ("r_picmip", "1"), ("r_texturebits", "0"), ("r_fastSky", "0"), ("r_inGameVideo", "1"), ("r_texturemode", "GL_LINEAR_MIPMAP_LINEAR"), ("cg_shadows", "0"), ("cg_bounceParticles", "0")],
                    // fast
                    2 => &[("r_subdivisions", "8"), ("r_vertexlight", "0"), ("r_lodbias", "1"), ("r_colorbits", "0"), ("r_depthbits", "0"), ("r_picmip", "1"), ("r_texturebits", "0"), ("cg_shadows", "0"), ("r_fastSky", "1"), ("r_inGameVideo", "0"), ("cg_bounceParticles", "0"), ("r_texturemode", "GL_LINEAR_MIPMAP_NEAREST")],
                    // fastest
                    3 => &[("r_subdivisions", "20"), ("r_vertexlight", "1"), ("r_lodbias", "2"), ("r_colorbits", "16"), ("r_depthbits", "16"), ("r_picmip", "2"), ("r_texturebits", "16"), ("cg_shadows", "0"), ("r_fastSky", "1"), ("r_inGameVideo", "0"), ("cg_bounceParticles", "0"), ("r_texturemode", "GL_LINEAR_MIPMAP_NEAREST")],
                    _ => &[],
                };
                for (n, v) in preset {
                    self.set_cvar(n, v);
                }
            }
            _ => {}
        }
    }

    /// `Menus_ReplaceActiveByName` / `Menus_ReplaceActive`: swap the top menu
    /// for one with the same item layout, keeping list positions.
    fn menus_replace_active_by_name(&mut self, name: &str) -> bool {
        let Some(mi) = self.find_menu(name) else { return false };
        let Some(&active) = self.stack.last() else { return false };
        let af = self.sets[0][active].flags;
        if af & WINDOW_HASFOCUS == 0 || af & WINDOW_VISIBLE == 0 || mi == active {
            return false;
        }
        let (n, n2) = (self.sets[0][mi].items.len(), self.sets[0][active].items.len());
        if n != n2 || (0..n).any(|i| self.sets[0][mi].items[i].ty != self.sets[0][active].items[i].ty) {
            return false;
        }
        self.sets[0][active].flags &= !(WINDOW_FADINGOUT | WINDOW_VISIBLE);
        self.sets[0][mi].flags |= WINDOW_HASFOCUS | WINDOW_VISIBLE;
        *self.stack.last_mut().unwrap() = mi;
        let on_open = self.sets[0][mi].def.on_open.clone();
        self.run_script(0, mi, None, &on_open);
        for i in 0..n {
            let a = self.sets[0][active].items[i].clone();
            let it = &mut self.sets[0][mi].items[i];
            it.cursor_pos = a.cursor_pos;
            it.feeder = a.feeder;
            it.list.start = a.list.start;
            it.list.cursor = a.list.cursor;
            it.cycle_cursor = a.cycle_cursor;
        }
        // the edit item follows (g_editItem pointed into the old menu)
        if let Some(e) = self.editing {
            if e.set == 0 && e.menu == active {
                self.editing = Some(Self::iref(0, mi, e.item));
                self.sets[0][mi].items[e.item].flags |= WINDOW_HASFOCUS;
            }
        }
        true
    }

    /// `UI_BuildPlayerList`.
    fn build_player_list(&mut self) {
        let me = self.view.me;
        let team = self.view.clients.iter().find(|c| c.num == me).map(|c| c.team).unwrap_or(Team::None);
        let mut pl = PlayerList { player_number: me, ..Default::default() };
        for c in &self.view.clients {
            pl.ignore.push(c.ignored.clone());
            pl.names.push(clean_str(&c.name));
            pl.client_nums.push(c.num);
            if c.num == me {
                pl.my_index = pl.names.len() - 1;
            }
            if c.team == team {
                pl.team_names.push(clean_str(&c.name));
                pl.team_client_nums.push(c.num);
            }
        }
        self.players = pl;
    }

    // ------------------------------------------------------------ feeders

    /// `UI_FeederCount` / `CG_FeederCount`.
    pub fn feeder_count(&mut self, id: i32) -> i32 {
        let Some(&f) = self.feeders.get(&id) else { return 0 };
        match f {
            Feeder::Maps => self.map_list.len() as i32,
            Feeder::PlayerList | Feeder::TeamList => {
                if self.real_time > self.player_refresh {
                    self.player_refresh = self.real_time + 3000;
                    self.build_player_list();
                }
                if f == Feeder::PlayerList {
                    self.players.names.len() as i32
                } else {
                    self.players.team_names.len() as i32
                }
            }
            Feeder::IgnoreList => self.players.names.len() as i32,
            Feeder::HelpList => self.help.len() as i32,
            Feeder::Resolutions => {
                let n = self.resolutions.len() as i32;
                if self.feeder_initialise(id) == n {
                    n + 1
                } else {
                    n
                }
            }
            Feeder::Servers => self.browser.display_servers.len() as i32,
            Feeder::ServerStatus => self.browser.status_lines.len() as i32,
            Feeder::News => self.browser.news.len() as i32,
            Feeder::FindPlayer => self.browser.found_names.len() as i32,
            Feeder::Mods => self.mods.len() as i32,
            Feeder::Demos => self.demos.len() as i32,
            Feeder::Cinematics => self.movies.len() as i32,
            Feeder::AlienTeamList | Feeder::HumanTeamList => {
                let t = if f == Feeder::AlienTeamList { Team::Aliens } else { Team::Humans };
                self.view.scores.rows.iter().filter(|r| r.team == t).count() as i32
            }
            _ => self.list(f).len() as i32,
        }
    }

    /// `UI_FeederItemText` / `CG_FeederItemText`: (text, optional image).
    pub fn feeder_item_text(&self, id: i32, index: i32, column: usize) -> (String, Option<String>) {
        let Some(&f) = self.feeders.get(&id) else { return (String::new(), None) };
        let ix = index.max(0) as usize;
        let s = |v: Option<&String>| v.cloned().unwrap_or_default();
        if index < 0 {
            return (String::new(), None);
        }
        let text = match f {
            Feeder::Maps => self.map_list.get(ix).map(|m| m.1.clone()).unwrap_or_default(),
            Feeder::PlayerList => s(self.players.names.get(ix)),
            Feeder::TeamList => s(self.players.team_names.get(ix)),
            Feeder::IgnoreList => {
                if ix >= self.players.names.len() {
                    return (String::new(), None);
                }
                let me = self.players.my_index;
                match column {
                    // am I ignoring him
                    1 => (if self.players.ignore.get(me).is_some_and(|l| l.contains(&self.players.client_nums[ix])) { "X" } else { "" }).to_string(),
                    // is he ignoring me
                    2 => (if self.players.ignore.get(ix).is_some_and(|l| l.contains(&self.players.player_number)) { "X" } else { "" }).to_string(),
                    _ => self.players.names[ix].clone(),
                }
            }
            Feeder::HelpList => self.help.get(ix).map(|h| h.0.clone()).unwrap_or_default(),
            Feeder::Resolutions => match self.resolutions.get(ix) {
                Some(&(w, h)) => format!("{w}x{h} ({})", display_aspect_string(w, h)),
                None => format!("Custom ({}x{})", self.cvar_value("r_width") as i32, self.cvar_value("r_height") as i32),
            },
            Feeder::Servers => return (self.server_item_text(ix, column), None),
            Feeder::ServerStatus => self.browser.status_lines.get(ix).and_then(|l| l.get(column)).cloned().unwrap_or_default(),
            Feeder::News => self.browser.news.get(ix).cloned().unwrap_or_default(),
            Feeder::FindPlayer => self.browser.found_names.get(ix).cloned().unwrap_or_default(),
            Feeder::Mods => self.mods.get(ix).map(|m| if m.1.is_empty() { m.0.clone() } else { m.1.clone() }).unwrap_or_default(),
            Feeder::Demos => self.demos.get(ix).cloned().unwrap_or_default(),
            Feeder::Cinematics => self.movies.get(ix).cloned().unwrap_or_default(),
            Feeder::AlienTeamList | Feeder::HumanTeamList => return self.cg_feeder_item_text(f, index, column),
            _ => self.list(f).get(ix).map(|e| e.text.clone()).unwrap_or_default(),
        };
        (text, None)
    }

    /// `CG_FeederItemText` (scoreboard rows; `CG_InfoFromScoreIndex`).
    fn cg_feeder_item_text(&self, f: Feeder, index: i32, column: usize) -> (String, Option<String>) {
        let s = &self.view.scores;
        let team = if f == Feeder::AlienTeamList { Team::Aliens } else { Team::Humans };
        let Some(sp) = s.rows.iter().filter(|r| r.team == team).nth(index.max(0) as usize).or_else(|| s.rows.get(index.max(0) as usize)) else { return (String::new(), None) };
        let ready = s.intermission && s.ready.get(sp.client).copied().unwrap_or(false);
        let show_icons = if ready { false } else { s.spectating || team == s.my_team || s.intermission };
        let none = (String::new(), None);
        match column {
            0 => {
                if show_icons && sp.weapon > 0 {
                    (String::new(), self.weapon_icons.get(sp.weapon as usize).cloned().flatten())
                } else {
                    none
                }
            }
            1 => {
                if !show_icons {
                    none
                } else if sp.team == Team::Humans {
                    (String::new(), sp.upgrade.and_then(|u| u.cold().icon.map(|s| s.to_string())))
                } else if [WeaponId::Abuildupg, WeaponId::Level1upg, WeaponId::Level2upg, WeaponId::Level3upg].iter().any(|w| *w as i32 == sp.weapon) {
                    (String::new(), Some("icons/icona_upgrade".to_string()))
                } else {
                    none
                }
            }
            2 => (if ready { "Ready".to_string() } else { String::new() }, None),
            3 => (format!("^7{}", s.names.get(sp.client).cloned().unwrap_or_default()), None),
            4 => (sp.score.to_string(), None),
            5 => (format!("{:4}", sp.time), None),
            6 => (if sp.ping == -1 { String::new() } else { format!("{:4}", sp.ping) }, None),
            _ => none,
        }
    }

    /// `UI_FeederItemImage` (FEEDER_MAPS levelshots).
    fn feeder_item_image(&self, id: i32, index: i32) -> Option<String> {
        match self.feeders.get(&id) {
            Some(Feeder::Maps) => self.map_list.get(index.max(0) as usize).map(|m| format!("levelshots/{}", m.0)),
            _ => None,
        }
    }

    /// `UI_FeederSelection` / `CG_FeederSelection`.
    pub fn feeder_selection(&mut self, id: i32, index: i32) {
        let Some(&f) = self.feeders.get(&id) else { return };
        match f {
            Feeder::Maps => {
                // the old map's cinematic stops, the new one's is tried
                let old = self.cvar_value("ui_selectedMap") as i32;
                if self.map_cinematic.get(&(old.max(0) as usize)).is_some_and(|&c| c >= 0) {
                    self.map_cinematic.insert(old.max(0) as usize, -1);
                }
                self.set_cvar("ui_selectedMap", &index.to_string());
                self.map_cinematic.insert(index.max(0) as usize, -1);
            }
            Feeder::Servers => {
                self.browser.current_server = index;
                let si = self.current_server_info();
                self.browser.current_server_preview = si.as_ref().map(|s| format!("levelshots/{}", s.map_name));
            }
            Feeder::FindPlayer => {
                self.browser.current_found = index;
                if (index as usize + 1) < self.browser.found_names.len() {
                    // build a new server status for this server
                    self.browser.status_address = self.browser.found_addrs.get(index.max(0) as usize).cloned().unwrap_or_default();
                    self.menu_set_feeder_selection(None, Feeder::ServerStatus, 0, None);
                    self.build_server_status(true);
                }
            }
            Feeder::Cinematics => self.preview_movie = false,
            Feeder::Resolutions => {
                if let Some(&(w, h)) = self.resolutions.get(index.max(0) as usize).filter(|_| index >= 0) {
                    self.set_cvar("r_width", &w.to_string());
                    self.set_cvar("r_height", &h.to_string());
                }
            }
            _ => {}
        }
        self.sel.insert(f, index);
    }

    /// `UI_FeederItemText` for FEEDER_SERVERS (host, game, map, clients, ping).
    fn server_item_text(&self, index: usize, column: usize) -> String {
        let src = self.net_source();
        let Some(si) = self.browser.display_servers.get(index).and_then(|&n| self.lan.list(src).get(n)) else { return String::new() };
        match column {
            // SORT_HOST
            0 => {
                if si.ping <= 0 {
                    si.addr.clone()
                } else if src == AS_LOCAL {
                    format!("{} [UDP]", si.host_name)
                } else {
                    si.host_name.trim_start().to_string()
                }
            }
            // the game and, once known, the protocol: "base 1.1" (dec040)
            1 => match protocol_tag(si.protocol) {
                "" => si.game.clone(),
                t => format!("{} {t}", si.game),
            },
            2 => si.map_name.clone(),
            3 => format!("{} ({})", si.clients, si.max_clients),
            4 => {
                if si.ping <= 0 {
                    "...".into()
                } else {
                    si.ping.to_string()
                }
            }
            _ => String::new(),
        }
    }

    /// `UI_FeederInitialise`.
    fn feeder_initialise(&self, id: i32) -> i32 {
        match self.feeders.get(&id) {
            // the mode r_width x r_height is, else the "Custom" entry
            Some(Feeder::Resolutions) => {
                let (w, h) = (self.cvar_value("r_width") as i32, self.cvar_value("r_height") as i32);
                self.resolutions.iter().position(|&(x, y)| x == w && y == h).unwrap_or(self.resolutions.len()) as i32
            }
            _ => 0,
        }
    }

    /// The selected entry of a uiInfo list.
    pub fn selected(&self, f: Feeder) -> Option<&Entry> {
        let i = *self.sel.get(&f).unwrap_or(&0);
        self.list(f).get(i.max(0) as usize)
    }

    pub fn selection(&self, f: Feeder) -> i32 {
        *self.sel.get(&f).unwrap_or(&0)
    }

    /// `UI_OwnerDrawVisible`.
    fn owner_draw_visible(&self, mut flags: i32) -> bool {
        let mut vis = true;
        let team = self.view.player.team;
        let cv = |n: &str| self.cvar_value(n) != 0.0;
        while flags != 0 {
            if flags & UI_SHOW_NOTSPECTATING != 0 {
                if team == Team::None {
                    vis = false;
                }
                flags &= !UI_SHOW_NOTSPECTATING;
            }
            if flags & UI_SHOW_VOTEACTIVE != 0 {
                if !cv("ui_voteActive") {
                    vis = false;
                }
                flags &= !UI_SHOW_VOTEACTIVE;
            }
            if flags & UI_SHOW_CANVOTE != 0 {
                if cv("ui_voteActive") {
                    vis = false;
                }
                flags &= !UI_SHOW_CANVOTE;
            }
            if flags & UI_SHOW_TEAMVOTEACTIVE != 0 {
                if (team == Team::Aliens && !cv("ui_alienTeamVoteActive")) || (team == Team::Humans && !cv("ui_humanTeamVoteActive")) {
                    vis = false;
                }
                flags &= !UI_SHOW_TEAMVOTEACTIVE;
            }
            if flags & UI_SHOW_CANTEAMVOTE != 0 {
                if (team == Team::Aliens && cv("ui_alienTeamVoteActive")) || (team == Team::Humans && cv("ui_humanTeamVoteActive")) {
                    vis = false;
                }
                flags &= !UI_SHOW_CANTEAMVOTE;
            }
            if flags & UI_SHOW_FAVORITESERVERS != 0 {
                // this assumes you only put this type of display flag on something showing in the proper context
                if self.net_source() != AS_FAVORITES {
                    vis = false;
                }
                flags &= !UI_SHOW_FAVORITESERVERS;
            }
            if flags & UI_SHOW_NOTFAVORITESERVERS != 0 {
                if self.net_source() == AS_FAVORITES {
                    vis = false;
                }
                flags &= !UI_SHOW_NOTFAVORITESERVERS;
            } else {
                flags = 0;
            }
        }
        vis
    }

    /// `UI_OwnerDrawText`.
    fn owner_draw_text(&self, id: i32) -> Option<String> {
        match self.ods.get(&id) {
            Some(Od::NetSource) => Some(NET_SOURCES[self.net_source() as usize].to_string()),
            Some(Od::KeyBindStatus) => Some(if self.key_bind_pending() { "Waiting for new key... Press ESCAPE to cancel" } else { "Press ENTER or CLICK to change, Press BACKSPACE to clear" }.into()),
            Some(Od::ServerRefreshDate) => Some(if self.browser.refresh_active {
                const MAX_DOTS: i32 = 5;
                let num_servers = self.lan.server_count(self.net_source());
                let dots = ".".repeat(((self.real_time / 500) % (MAX_DOTS + 1)).max(0) as usize);
                if num_servers < 0 {
                    format!("Waiting for response{dots}")
                } else {
                    format!("Getting info for {num_servers} servers (ESC to cancel){dots}")
                }
            } else {
                format!("Refresh Time: {}", self.cvar(&format!("ui_lastServerRefresh_{}", self.net_source())))
            }),
            Some(Od::ServerMotd) => Some(self.browser.motd.clone()),
            _ => None,
        }
    }

    /// `Item_EnableShowViaCvar`.
    fn enable_show_via_cvar(&self, r: IRef, flag: u32) -> bool {
        let it = self.it(r);
        if it.enable_cvar.is_empty() {
            return true;
        }
        let Some(test) = it.def.cvar_test.as_deref().filter(|t| !t.is_empty()) else { return true };
        let buff = self.cvar(test);
        for val in &it.enable_cvar {
            if val == ";" {
                continue;
            }
            if buff.eq_ignore_ascii_case(val) {
                // enable it if any of the values are true, disable if any are
                return it.cvar_flags & flag != 0;
            }
        }
        it.cvar_flags & flag == 0
    }
}

// =====================================================================
// the main menu side of ui_main.c: UI_SetActiveMenu, the server browser
// (UI_StartServerRefresh .. UI_BuildServerDisplayList on trap_LAN_*),
// news, find player, server status, mods / demos / movies, and the
// ui_shared.c effects (Fade, transitions, orbits)
// =====================================================================

/// `uiMenuCommand_t` of `UI_SetActiveMenu`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UiMenuCommand {
    None,
    Main,
    Ingame,
}

impl Ui {
    /// `Menu_Init`'s fadeAmount / fadeClamp / fadeCycle for the menus whose
    /// script does not set its own.
    pub fn apply_asset_fades(&mut self) {
        let (a, c, y) = self.asset_fade;
        for set in self.sets.iter_mut() {
            for m in set.iter_mut() {
                if m.def.fade_amount == 0.0 {
                    m.fade.0 = a;
                }
                if m.def.fade_clamp == 0.0 {
                    m.fade.1 = c;
                }
                if m.def.fade_cycle == 0 {
                    m.fade.2 = y;
                }
            }
        }
    }

    /// `UI_SetActiveMenu`.
    pub fn set_active_menu(&mut self, menu: UiMenuCommand) {
        match menu {
            UiMenuCommand::None => self.menus_close_all(),
            UiMenuCommand::Main => {
                self.set_cvar("sv_killserver", "1");
                self.menus_close_all();
                self.menus_activate_by_name("main");
                if !self.cvar("com_errorMessage").is_empty() {
                    if self.cvar_value("com_errorCode") as i32 == 5 {
                        // ERR_SERVERDISCONNECT
                        self.menus_activate_by_name("drop_popmenu");
                    } else {
                        self.menus_activate_by_name("error_popmenu");
                    }
                }
            }
            UiMenuCommand::Ingame => self.open_ingame(),
        }
    }

    /// `UI_IsFullscreen`.
    pub fn is_fullscreen(&self) -> bool {
        self.any_fullscreen_visible()
    }

    /// ui_netSource.integer (out of range reads as AS_GLOBAL, as `UI_OwnerDrawText` fixes it).
    fn net_source(&self) -> i32 {
        let n = self.cvar_value("ui_netSource") as i32;
        if (0..NET_SOURCES.len() as i32).contains(&n) {
            n
        } else {
            AS_GLOBAL
        }
    }

    /// `trap_LAN_GetServerInfo` of the browser's current server.
    fn current_server_info(&self) -> Option<ServerInfo> {
        let c = self.browser.current_server;
        let n = *self.browser.display_servers.get(c.max(0) as usize)?;
        self.lan.list(self.net_source()).get(n).cloned()
    }

    fn favorite_result(&mut self, res: i32, addr: &str) {
        let m = match res {
            // server already in the list
            0 => "Favorite already in list\n".to_string(),
            // list full
            -1 => "Favorite list full\n".to_string(),
            // successfully added
            _ => format!("Added favorite server {addr}\n"),
        };
        self.out.push(Out::Print(m));
    }

    /// `UI_StartServerRefresh`.
    fn start_server_refresh(&mut self, full: bool) {
        let src = self.net_source();
        let [mon, mday, year, hour, min] = self.local_time;
        let t = self.unix_time;
        self.set_cvar(&format!("ui_lastServerRefresh_{src}_time"), &t.to_string());
        self.set_cvar(&format!("ui_lastServerRefresh_{src}"), &format!("{}-{}, {} at {}:{:02}", MONTH_ABBREV[mon.clamp(0, 11) as usize], mday, year, hour, min));
        if !full {
            // UI_UpdatePendingPings
            self.lan.reset_pings(src);
            self.browser.refresh_active = true;
            self.browser.refresh_time = self.real_time + 1000;
            return;
        }
        self.browser.refresh_active = true;
        self.browser.next_display_refresh = self.real_time + 1000;
        // clear number of displayed servers
        self.browser.display_servers.clear();
        self.browser.num_players_on_servers = 0;
        // mark all servers as visible so we store ping updates for them
        self.lan.mark_server_visible(src, -1, true);
        // reset all the pings
        self.lan.reset_pings(src);
        if src == AS_LOCAL {
            self.exec("localservers\n");
            self.browser.refresh_time = self.real_time + 1000;
            return;
        }
        self.browser.refresh_time = self.real_time + 5000;
        if src == AS_GLOBAL || src == AS_MPLAYER {
            self.exec(&format!("globalservers {} full empty\n", if src == AS_GLOBAL { 0 } else { 1 }));
        }
    }

    /// `UI_StopServerRefresh`.
    fn stop_server_refresh(&mut self) {
        if !self.browser.refresh_active {
            // not currently refreshing
            return;
        }
        self.browser.refresh_active = false;
        let n = self.browser.display_servers.len() as i32;
        self.out.push(Out::Print(format!("{} servers listed in browser with {} players.\n", n, self.browser.num_players_on_servers)));
        let count = self.lan.server_count(self.net_source());
        if count - n > 0 {
            let mp = self.cvar_value("cl_maxPing") as i32;
            self.out.push(Out::Print(format!("{} servers not listed due to packet loss, invalid info, or pings higher than {}\n", count - n, mp)));
        }
    }

    /// `UI_DoServerRefresh`.
    fn do_server_refresh(&mut self) {
        if !self.browser.refresh_active {
            return;
        }
        let src = self.net_source();
        let mut wait = false;
        if src != AS_FAVORITES {
            if src == AS_LOCAL {
                // the C waits only while nobody answered; the port's servers
                // answer at their next frame (up to 50 ms after a tremded
                // would), so the first answer would end the scan before the
                // others arrive: the LAN scan keeps its whole second (dec040)
                wait = self.lan.server_count(src) == 0 || self.real_time < self.browser.refresh_time;
            } else if self.lan.server_count(src) < 0 {
                wait = true;
            }
        }
        if self.real_time < self.browser.refresh_time && wait {
            return;
        }
        // if still trying to retrieve pings
        if self.lan.update_visible_pings(src) {
            self.browser.refresh_time = self.real_time + 1000;
        } else if !wait {
            // get the last servers in the list
            self.build_server_display_list(2);
            // stop the refresh
            self.stop_server_refresh();
        }
        self.build_server_display_list(0);
    }

    /// `trap_LAN_CompareServers` (cl_ui.c `LAN_CompareServers`), the
    /// comparison `UI_ServersQsortCompare` sorts with.
    fn compare_servers(&self, a: usize, b: usize) -> i32 {
        let l = self.lan.list(self.net_source());
        let (Some(s1), Some(s2)) = (l.get(a), l.get(b)) else { return 0 };
        let stricmp = |x: &str, y: &str| match x.to_ascii_lowercase().cmp(&y.to_ascii_lowercase()) {
            std::cmp::Ordering::Less => -1,
            std::cmp::Ordering::Equal => 0,
            std::cmp::Ordering::Greater => 1,
        };
        let num = |x: i32, y: i32| (x > y) as i32 - (x < y) as i32;
        let res = match self.browser.sort_key {
            0 => {
                let alpha = |s: &str| s.chars().filter(|c| c.is_ascii_alphabetic()).collect::<String>();
                stricmp(&alpha(&s1.host_name), &alpha(&s2.host_name))
            }
            1 => stricmp(&s1.game, &s2.game),
            2 => stricmp(&s1.map_name, &s2.map_name),
            3 => num(s1.clients, s2.clients),
            4 => num(s1.ping, s2.ping),
            _ => 0,
        };
        if self.browser.sort_dir {
            -res
        } else {
            res
        }
    }

    /// `UI_BinaryServerInsertion` (+ `UI_InsertServerIntoDisplayList`).
    fn binary_server_insertion(&mut self, num: usize) {
        let mut len = self.browser.display_servers.len();
        let mut mid = len;
        let mut offset = 0;
        let mut res = 0;
        while mid > 0 {
            mid = len >> 1;
            res = self.compare_servers(num, self.browser.display_servers[(offset + mid).min(self.browser.display_servers.len() - 1)]);
            if res == 0 {
                self.browser.display_servers.insert(offset + mid, num);
                return;
            } else if res == 1 {
                offset += mid;
                len -= mid;
            } else {
                len -= mid;
            }
        }
        if res == 1 {
            offset += 1;
        }
        let at = offset.min(self.browser.display_servers.len());
        self.browser.display_servers.insert(at, num);
    }

    /// `UI_BuildServerDisplayList` (force 2: no reset).
    fn build_server_display_list(&mut self, force: i32) {
        if !(force != 0 || self.real_time > self.browser.next_display_refresh) {
            return;
        }
        let force = force == 1;
        // do motd updates here too
        self.browser.motd = self.cvar("cl_motdString");
        let src = self.net_source();
        if force {
            // clear number of displayed servers
            self.browser.display_servers.clear();
            self.browser.num_players_on_servers = 0;
            // set list box index to zero
            self.menu_set_feeder_selection(None, Feeder::Servers, 0, None);
            // mark all servers as visible so we store ping updates for them
            self.lan.mark_server_visible(src, -1, true);
        }
        // get the server count (comes from the master)
        let count = self.lan.server_count(src);
        if count == -1 || (src == AS_LOCAL && count == 0) {
            // still waiting on a response from the master
            self.browser.display_servers.clear();
            self.browser.num_players_on_servers = 0;
            self.browser.next_display_refresh = self.real_time + 500;
            return;
        }
        for i in 0..count.max(0) as usize {
            // if we already got info for this server
            if !self.lan.server_is_visible(src, i) {
                continue;
            }
            // get the ping for this server
            let ping = self.lan.server_ping(src, i);
            if ping > 0 || src == AS_FAVORITES {
                let Some(si) = self.lan.list(src).get(i).cloned() else { continue };
                self.browser.num_players_on_servers += si.clients;
                if self.cvar_value("ui_browserShowEmpty") == 0.0 && si.clients == 0 {
                    self.lan.mark_server_visible(src, i as i32, false);
                    continue;
                }
                if self.cvar_value("ui_browserShowFull") == 0.0 && si.clients == si.max_clients {
                    self.lan.mark_server_visible(src, i as i32, false);
                    continue;
                }
                // the protocol filter (dec040): ui_browserProtocol 71 or 69
                let want = self.cvar_value("ui_browserProtocol") as i32;
                if (want == 69 || want == 71) && si.protocol != want {
                    self.lan.mark_server_visible(src, i as i32, false);
                    continue;
                }
                // make sure we never add a favorite server twice
                // (`UI_RemoveServerFromDisplayList`)
                if src == AS_FAVORITES {
                    self.browser.display_servers.retain(|&x| x != i);
                }
                // insert the server into the list
                self.binary_server_insertion(i);
                // done with this server
                if ping > 0 {
                    self.lan.mark_server_visible(src, i as i32, false);
                }
            }
        }
        self.browser.refresh_time = self.real_time;
    }

    /// `UI_ServersSort`.
    fn servers_sort(&mut self, column: i32, force: bool) {
        if !force && self.browser.sort_key == column {
            return;
        }
        self.browser.sort_key = column;
        let mut l = std::mem::take(&mut self.browser.display_servers);
        l.sort_by(|&a, &b| self.compare_servers(a, b).cmp(&0));
        self.browser.display_servers = l;
    }

    /// `UI_NetSource_HandleKey` (UI_NETSOURCE's ownerdraw key handler).
    fn net_source_handle_key(&mut self, key: K) -> bool {
        if !matches!(key, K::Mouse1 | K::Mouse2 | K::Enter | K::KpEnter) {
            return false;
        }
        let mut n = self.net_source();
        if key == K::Mouse2 {
            n -= 1;
            if n == AS_MPLAYER {
                n -= 1;
            }
        } else {
            n += 1;
            if n == AS_MPLAYER {
                n += 1;
            }
        }
        let count = NET_SOURCES.len() as i32;
        if n >= count {
            n = 0;
        } else if n < 0 {
            n = count - 1;
        }
        self.set_cvar("ui_netSource", &n.to_string());
        self.build_server_display_list(1);
        if n != AS_GLOBAL {
            self.start_server_refresh(true);
        }
        true
    }

    /// `UI_UpdateNews`: this port has no cURL (`GetNews` without USE_CURL).
    fn update_news(&mut self, begin: bool) {
        if begin && !self.browser.news_active {
            self.browser.news_refresh_time = self.real_time + 10000;
            self.browser.news_active = true;
        } else if !self.browser.news_active {
            // do nothing
            return;
        } else if self.real_time > self.browser.news_refresh_time {
            self.browser.news = vec!["^1Error: Timed out while contacting the server.".into()];
            return;
        }
        // start the news fetching (`trap_GetNews`)
        let finished = true;
        self.set_cvar("cl_newsString", "^1You must compile your client with CURL support to use this feature");
        let news = self.cvar("cl_newsString");
        // parse what comes back. Parse newlines and otherwise chop when necessary
        let wrapped = item_text_wrap(&self.fonts, &news, 0.25, 325.0 * self.aspect, self.aspect);
        let mut lines: Vec<String> = vec![String::new()];
        for c in wrapped.chars() {
            let line_len = lines.last().map(|l| l.len()).unwrap_or(0);
            if line_len == MAX_NEWS_LINEWIDTH - 1 || c == '\n' {
                if lines.len() == MAX_NEWS_LINES {
                    break;
                }
                lines.push(String::new());
                if c != '\n' {
                    lines.last_mut().unwrap().push(c);
                }
            } else if c.is_ascii_graphic() || c == ' ' {
                lines.last_mut().unwrap().push(c);
            }
        }
        self.browser.news = lines;
        if finished {
            self.browser.news_active = false;
        }
    }

    /// `UI_BuildFindPlayerList`: with no servers in the browser the search
    /// ends at once ("0 servers found with player ...").
    fn build_find_player_list(&mut self, force: bool) {
        if !force {
            if self.browser.next_find_player_refresh == 0 || self.browser.next_find_player_refresh > self.real_time {
                return;
            }
        } else {
            self.browser.found_names.clear();
            self.browser.found_addrs.clear();
            self.browser.current_found = 0;
            let name = clean_str(&self.cvar("ui_findPlayer"));
            // should have a string of some length
            if name.is_empty() {
                self.browser.next_find_player_refresh = 0;
                return;
            }
            let resend = ((self.cvar_value("ui_serverStatusTimeOut") as i32) / 2 - 10).max(50);
            self.set_cvar("cl_serverStatusResendTime", &resend.to_string());
            self.browser.found_names.push("searching 0...".into());
        }
        // trap_LAN_ServerStatus answers nothing: every request times out and
        // there is nothing in the display list to ask
        let name = clean_str(&self.cvar("ui_findPlayer"));
        let n = self.browser.found_names.len();
        if n == 0 {
            self.browser.found_names.push("no servers found".into());
        } else {
            self.browser.found_names[n - 1] = format!("{} server{} found with player {}", n - 1, if n == 2 { "" } else { "s" }, name);
        }
        self.browser.next_find_player_refresh = 0;
        // show the server status info for the selected server
        let c = self.browser.current_found;
        self.feeder_selection_f(Feeder::FindPlayer, c);
    }

    /// `UI_BuildServerStatus`: no server can answer a status request here.
    fn build_server_status(&mut self, force: bool) {
        if self.browser.next_find_player_refresh != 0 {
            return;
        }
        if !force {
            if self.browser.next_server_status_refresh == 0 || self.browser.next_server_status_refresh > self.real_time {
                return;
            }
        } else {
            self.menu_set_feeder_selection(None, Feeder::ServerStatus, 0, None);
            self.browser.status_lines.clear();
        }
        let c = self.browser.current_server;
        if c < 0 || c as usize > self.browser.display_servers.len() || self.browser.display_servers.is_empty() {
            return;
        }
        // UI_GetServerStatusInfo fails: try again in 500 ms
        self.browser.next_server_status_refresh = self.real_time + 500;
    }

    /// `trap_FS_GetFileList( dir, ext )` over the pk3s and user files.
    fn fs_file_list(&self, dir: &str, ext: &str) -> Vec<String> {
        let prefix = format!("{}/", dir.trim_end_matches('/').to_ascii_lowercase());
        let ext = ext.trim_start_matches('.').to_ascii_lowercase();
        let mut out: Vec<String> = Vec::new();
        for f in &self.fs_files {
            let l = f.to_ascii_lowercase();
            if let Some(rest) = l.strip_prefix(&prefix) {
                if !rest.contains('/') && rest.rsplit_once('.').is_some_and(|(_, e)| e == ext) {
                    let name = f[prefix.len()..].to_string();
                    if !out.iter().any(|o| o.eq_ignore_ascii_case(&name)) {
                        out.push(name);
                    }
                }
            }
        }
        out
    }

    /// `UI_LoadMods` ($modlist: the game folders besides base, with their
    /// description.txt).
    fn load_mods(&mut self) {
        self.mods = self.mod_dirs.iter().take(MAX_MODS).cloned().collect();
    }

    /// `UI_LoadMovies` (video/*.roq, extension stripped, upper case).
    fn load_movies(&mut self) {
        let l = self.fs_file_list("video", "roq");
        self.movies = l.into_iter().take(MAX_MOVIES).map(|m| m[..m.len() - 4].to_ascii_uppercase()).collect();
    }

    /// `UI_LoadDemos` (demos/*.dm_<protocol>, extension stripped).
    fn load_demos(&mut self) {
        let ext = format!("dm_{PROTOCOL_VERSION}");
        let l = self.fs_file_list("demos", &ext);
        let dot = format!(".{ext}");
        self.demos = l.into_iter().take(MAX_DEMOS).map(|d| if d.to_ascii_lowercase().ends_with(&dot) { d[..d.len() - dot.len()].to_string() } else { d }).collect();
    }

    /// `UI_ParseResolutions` (r_availableModes "WxH WxH ...").
    pub fn parse_resolutions(&mut self) {
        self.resolutions.clear();
        for m in self.cvar("r_availableModes").split_whitespace() {
            let Some((w, h)) = m.split_once('x') else { return };
            self.resolutions.push((c_atof(w) as i32, c_atof(h) as i32));
        }
    }

    /// `Menu_SetFeederSelection`: the listbox of `feeder` in `menu` (or the
    /// focused menu) selects `index`.
    fn menu_set_feeder_selection(&mut self, menu: Option<usize>, feeder: Feeder, index: i32, name: Option<&str>) {
        let mi = match (menu, name) {
            (Some(m), _) => Some(m),
            (None, Some(n)) => self.find_menu(n),
            (None, None) => self.menu_get_focused(),
        };
        let Some(id) = self.feeders.iter().find(|(_, f)| **f == feeder).map(|(id, _)| *id) else { return };
        if let Some(mi) = mi {
            for ii in 0..self.sets[0][mi].items.len() {
                let r = Self::iref(0, mi, ii);
                if self.it(r).feeder == id && is_list_box(self.it(r).ty) {
                    if index == 0 {
                        self.it_mut(r).list.cursor = 0;
                        self.list_set_start_pos(r, 0);
                    }
                    self.it_mut(r).cursor_pos = index;
                    self.feeder_selection(id, index);
                    return;
                }
            }
        }
    }

    /// `UI_FeederSelection` by feeder (the C calls it with the FEEDER_ id).
    fn feeder_selection_f(&mut self, f: Feeder, index: i32) {
        match self.feeders.iter().find(|(_, x)| **x == f).map(|(id, _)| *id) {
            Some(id) => self.feeder_selection(id, index),
            None => {
                self.sel.insert(f, index);
            }
        }
    }

    /// `Display_CloseCinematics`.
    fn display_close_cinematics(&mut self) {
        self.preview_movie = false;
        for v in self.map_cinematic.values_mut() {
            if *v >= 0 {
                *v = -1;
            }
        }
        self.out.push(Out::CloseCinematics);
    }

    /// `Menu_FadeItemByName`.
    fn menu_fade_item_by_name(&mut self, set: usize, mi: usize, name: &str, fade_out: bool) {
        for ii in self.sets[set][mi].matching(name) {
            let it = &mut self.sets[set][mi].items[ii];
            if fade_out {
                it.flags |= WINDOW_FADINGOUT | WINDOW_VISIBLE;
                it.flags &= !WINDOW_FADINGIN;
            } else {
                it.flags |= WINDOW_VISIBLE | WINDOW_FADINGIN;
                it.flags &= !WINDOW_FADINGOUT;
            }
        }
    }

    /// `Item_UpdatePosition` (`Item_SetScreenCoords` under the menu).
    fn item_update_position(&mut self, r: IRef) {
        let m = &self.sets[r.set][r.menu];
        let (mut x, mut y) = (m.rect[0], m.rect[1]);
        if m.def.border != 0 {
            x += m.def.border_size;
            y += m.def.border_size;
        }
        let it = &mut self.sets[r.set][r.menu].items[r.item];
        if it.def.border != 0 {
            x += it.def.border_size;
            y += it.def.border_size;
        }
        it.rect = [x + it.rect_client[0], y + it.rect_client[1], it.rect_client[2], it.rect_client[3]];
        // force the text rects to recompute
        it.text_rect[2] = 0.0;
        it.text_rect[3] = 0.0;
    }

    /// `Menu_TransitionItemByName`.
    #[allow(clippy::too_many_arguments)]
    fn menu_transition_item_by_name(&mut self, set: usize, mi: usize, name: &str, from: R, to: R, time: i32, amt: f32) {
        for ii in self.sets[set][mi].matching(name) {
            let it = &mut self.sets[set][mi].items[ii];
            it.flags |= WINDOW_INTRANSITION | WINDOW_VISIBLE;
            it.offset_time = time;
            it.rect_client = from;
            it.rect_effects = to;
            // abs() of the C is the integer one
            let step = |a: f32, b: f32| ((b - a) as i32).abs() as f32 / amt;
            it.rect_effects2 = [step(from[0], to[0]), step(from[1], to[1]), step(from[2], to[2]), step(from[3], to[3])];
            self.item_update_position(Self::iref(set, mi, ii));
        }
    }

    /// `Menu_OrbitItemByName`.
    #[allow(clippy::too_many_arguments)]
    fn menu_orbit_item_by_name(&mut self, set: usize, mi: usize, name: &str, x: f32, y: f32, cx: f32, cy: f32, time: i32) {
        for ii in self.sets[set][mi].matching(name) {
            let it = &mut self.sets[set][mi].items[ii];
            it.flags |= WINDOW_ORBITING | WINDOW_VISIBLE;
            it.offset_time = time;
            it.rect_effects[0] = cx;
            it.rect_effects[1] = cy;
            it.rect_client[0] = x;
            it.rect_client[1] = y;
            self.item_update_position(Self::iref(set, mi, ii));
        }
    }

    /// `Item_Paint`'s orbit and transition steps.
    fn item_effects(&mut self, r: IRef) {
        let now = self.real_time;
        let flags = self.it(r).flags;
        if flags & WINDOW_ORBITING != 0 && now > self.it(r).next_time {
            let it = self.it_mut(r);
            it.next_time = now + it.offset_time;
            // translate
            let (w, h) = (it.rect_client[2] / 2.0, it.rect_client[3] / 2.0);
            let rx = it.rect_client[0] + w - it.rect_effects[0];
            let ry = it.rect_client[1] + h - it.rect_effects[1];
            let a = 3.0 * std::f32::consts::PI / 180.0;
            let (c, s) = (a.cos(), a.sin());
            it.rect_client[0] = (rx * c - ry * s) + it.rect_effects[0] - w;
            it.rect_client[1] = (rx * s + ry * c) + it.rect_effects[1] - h;
            self.item_update_position(r);
        }
        if flags & WINDOW_INTRANSITION != 0 && now > self.it(r).next_time {
            let it = self.it_mut(r);
            it.next_time = now + it.offset_time;
            // transition the x, y, w, h
            let mut done = 0;
            for k in 0..4 {
                let (cur, to, step) = (it.rect_client[k], it.rect_effects[k], it.rect_effects2[k]);
                if cur == to {
                    done += 1;
                } else if cur < to {
                    it.rect_client[k] = cur + step;
                    if it.rect_client[k] > to {
                        it.rect_client[k] = to;
                        done += 1;
                    }
                } else {
                    it.rect_client[k] = cur - step;
                    if it.rect_client[k] < to {
                        it.rect_client[k] = to;
                        done += 1;
                    }
                }
            }
            self.item_update_position(r);
            if done == 4 {
                self.it_mut(r).flags &= !WINDOW_INTRANSITION;
            }
        }
    }

    /// `Fade` on an item's forecolor alpha (Item_TextColor,
    /// Item_OwnerDraw_Paint) or its backcolor alpha (Window_Paint).
    fn item_fade(&mut self, r: IRef, back: bool) {
        let (amount, clamp, cycle) = self.menu_of(r).fade;
        let now = self.real_time;
        let it = self.it_mut(r);
        let mut a = if back { it.back[3] } else { it.fore[3] };
        fade(&mut it.flags, &mut a, clamp, &mut it.next_time, cycle, true, amount, now);
        if back {
            it.back[3] = a;
        } else {
            it.fore[3] = a;
        }
    }
}

/// `Fade`.
#[allow(clippy::too_many_arguments)]
pub fn fade(flags: &mut u32, f: &mut f32, clamp: f32, next_time: &mut i32, offset_time: i32, b_flags: bool, fade_amount: f32, now: i32) {
    if *flags & (WINDOW_FADINGOUT | WINDOW_FADINGIN) != 0 && now > *next_time {
        *next_time = now + offset_time;
        if *flags & WINDOW_FADINGOUT != 0 {
            *f -= fade_amount;
            if b_flags && *f <= 0.0 {
                *flags &= !(WINDOW_FADINGOUT | WINDOW_VISIBLE);
            }
        } else {
            *f += fade_amount;
            if *f >= clamp {
                *f = clamp;
                if b_flags {
                    *flags &= !WINDOW_FADINGIN;
                }
            }
        }
    }
}

/// assetGlobalDef `fadeClamp` / `fadeCycle` / `fadeAmount` (`Asset_Parse`):
/// (amount, clamp, cycle) of the first menu file that sets them.
pub fn scan_asset_fades(text: &str, read: &mut dyn FnMut(&str) -> Option<String>) -> Option<(f32, f32, i32)> {
    let mut toks = Vec::new();
    let mut defs = HashMap::new();
    if hud::lex(text, &mut defs, read, &mut toks, 0).is_err() {
        return None;
    }
    let word = |t: &Tok| match t {
        Tok::Word(w) | Tok::Str(w) => Some(w.clone()),
        _ => None,
    };
    let start = toks.iter().position(|t| word(t).is_some_and(|w| w.eq_ignore_ascii_case("assetglobaldef")))?;
    let (mut amount, mut clamp, mut cycle) = (None, None, None);
    let mut depth = 0;
    let mut i = start + 1;
    while i < toks.len() {
        match &toks[i] {
            Tok::Open => depth += 1,
            Tok::Close => {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
            t => {
                let v = toks.get(i + 1).and_then(word).and_then(|w| w.parse::<f32>().ok());
                match word(t).map(|w| w.to_ascii_lowercase()).as_deref() {
                    Some("fadeclamp") => clamp = v,
                    Some("fadecycle") => cycle = v.map(|x| x as i32),
                    Some("fadeamount") => amount = v,
                    _ => {}
                }
            }
        }
        i += 1;
    }
    Some((amount.unwrap_or(0.0), clamp.unwrap_or(0.0), cycle.unwrap_or(0)))
}

/// `GCD` + `UI_DisplayAspectString` ("16:9", 8:5 named 16:10).
pub fn display_aspect_string(w: i32, h: i32) -> String {
    let (mut a, mut b) = (w, h);
    while b != 0 {
        let c = a % b;
        a = b;
        b = c;
    }
    let g = a.max(1);
    let (mut w, mut h) = (w / g, h / g);
    // For some reason 8:5 is usually referred to as 16:10
    if w == 8 && h == 5 {
        w = 16;
        h = 10;
    }
    format!("{w}:{h}")
}

fn godot_print_unknown(name: &str) {
    // Com_Printf( "unknown UI script %s\n", name )
    if cfg!(test) {
        eprintln!("unknown UI script {name}");
    } else {
        godot_print!("unknown UI script {name}");
    }
}

// =====================================================================
// input (ui_shared.c focus / mouse / key handling)
// =====================================================================

impl Ui {
    fn arrow_w(&self) -> f32 {
        SCROLLBAR_ARROW_SIZE * self.aspect
    }
    /// SCROLLBAR_SLIDER_X
    fn sb_slider_x(&self, r: R) -> f32 {
        r[0] + r[2] - self.arrow_w() - self.aspect
    }
    /// SCROLLBAR_Y / SCROLLBAR_SLIDER_Y / SCROLLBAR_SLIDER_HEIGHT / SCROLLBAR_X
    fn sb_y(r: R) -> f32 {
        r[1] + 1.0
    }
    fn sb_slider_y(r: R) -> f32 {
        Self::sb_y(r) + SCROLLBAR_ARROW_SIZE
    }
    fn sb_slider_h(r: R) -> f32 {
        r[3] - SCROLLBAR_ARROW_SIZE * 2.0 - 2.0
    }
    fn sb_x(&self, r: R) -> f32 {
        r[0] + self.aspect
    }

    /// The rect a combobox has while it is cast to a listbox
    /// (`Item_ComboBox_MaybeCastToListBox`).
    fn list_rect(&self, r: IRef) -> (R, i32) {
        let it = self.it(r);
        if it.ty == IT_COMBOBOX && self.combo.is_some() {
            let h = it.def.element_height * it.def.drop_items as f32 + 2.0;
            ([it.rect[0], it.rect[1] + it.rect[3], it.rect[2], h], IT_LISTBOX)
        } else {
            (it.rect, it.ty)
        }
    }

    /// `Item_ListBox_NumItemsForItemHeight`.
    fn list_num_items(&self, r: IRef) -> i32 {
        let it = self.it(r);
        let (rect, ty) = self.list_rect(r);
        if ty == IT_COMBOBOX {
            it.def.drop_items
        } else if it.def.element_height > 0.0 {
            ((rect[3] - 2.0) / it.def.element_height) as i32
        } else {
            0
        }
    }

    /// `Item_ListBox_MaxScroll`.
    fn list_max_scroll(&mut self, r: IRef) -> i32 {
        let f = self.it(r).feeder;
        let total = self.feeder_count(f);
        (total - self.list_num_items(r)).max(0)
    }

    /// `Item_ListBox_SetStartPos`.
    fn list_set_start_pos(&mut self, r: IRef, start: i32) {
        let f = self.it(r).feeder;
        let total = self.feeder_count(f);
        let max = self.list_max_scroll(r);
        let n = self.list_num_items(r);
        let l = &mut self.it_mut(r).list;
        l.start = if start < 0 {
            0
        } else if start > max {
            max
        } else {
            start
        };
        l.end = l.start + (total - l.start).min(n);
    }

    /// `Item_ListBox_ThumbPosition`.
    fn list_thumb_position(&mut self, r: IRef) -> f32 {
        let (rect, _) = self.list_rect(r);
        let max = self.list_max_scroll(r) as f32;
        let size = Self::sb_slider_h(rect);
        let pos = if max > 0.0 { (size - SCROLLBAR_ARROW_SIZE) / max } else { 0.0 };
        Self::sb_slider_y(rect) + pos * self.it(r).list.start as f32
    }

    /// `Item_ListBox_ThumbDrawPosition`.
    fn list_thumb_draw_position(&mut self, r: IRef) -> f32 {
        if self.item_capture == Some(r) {
            let (rect, _) = self.list_rect(r);
            let min = Self::sb_slider_y(rect);
            let max = min + Self::sb_slider_h(rect) - SCROLLBAR_ARROW_SIZE;
            let half = SCROLLBAR_ARROW_SIZE / 2.0;
            let cy = self.cursor.1;
            if cy >= min + half && cy <= max + half {
                return cy - half;
            }
        }
        self.list_thumb_position(r)
    }

    /// `Item_ListBox_OverLB`.
    fn list_over_lb(&mut self, r: IRef, x: f32, y: f32) -> u32 {
        let (rect, _) = self.list_rect(r);
        let mut t = [self.sb_slider_x(rect), Self::sb_y(rect), self.arrow_w(), SCROLLBAR_ARROW_SIZE];
        if contains(t, x, y) {
            return WINDOW_LB_UPARROW;
        }
        t[1] = Self::sb_slider_y(rect) + Self::sb_slider_h(rect);
        if contains(t, x, y) {
            return WINDOW_LB_DOWNARROW;
        }
        let thumb = self.list_thumb_position(r) as i32 as f32;
        t[1] = thumb;
        if contains(t, x, y) {
            return WINDOW_LB_THUMB;
        }
        t[1] = Self::sb_slider_y(rect);
        t[3] = thumb - t[1];
        if contains(t, x, y) {
            return WINDOW_LB_PGUP;
        }
        t[1] = thumb + SCROLLBAR_ARROW_SIZE;
        t[3] = (Self::sb_slider_y(rect) + Self::sb_slider_h(rect)) - t[1];
        if contains(t, x, y) {
            return WINDOW_LB_PGDN;
        }
        0
    }

    /// `Item_ListBox_MouseEnter`.
    fn list_mouse_enter(&mut self, r: IRef, x: f32, y: f32) {
        let f = self.it(r).feeder;
        let total = self.feeder_count(f);
        self.it_mut(r).flags &= !LB_FLAGS;
        let over = self.list_over_lb(r, x, y);
        self.it_mut(r).flags |= over;
        if self.it(r).flags & LB_FLAGS == 0 {
            let (rect, _) = self.list_rect(r);
            let eh = self.it(r).def.element_height;
            let n = self.list_num_items(r);
            let t = [self.sb_x(rect), Self::sb_y(rect), self.sb_slider_x(rect) - self.sb_x(rect), eh * n.min(total) as f32];
            let l = &mut self.it_mut(r).list;
            if contains(t, x, y) {
                l.cursor = ((y - t[1]) / eh) as i32 + l.start;
                if l.cursor >= l.end {
                    l.cursor = l.end - 1;
                }
            } else {
                l.cursor = -1;
            }
        }
    }

    /// `Item_CorrectedTextRect`.
    fn corrected_text_rect(&self, r: IRef) -> R {
        let mut t = self.it(r).text_rect;
        if t[2] != 0.0 {
            t[1] -= t[3];
        }
        t
    }

    /// `Item_MouseEnter`.
    fn item_mouse_enter(&mut self, r: IRef, x: f32, y: f32) {
        let mut t = self.it(r).text_rect;
        t[1] -= t[3];
        let cf = self.it(r).cvar_flags;
        if cf & (CVAR_ENABLE | CVAR_DISABLE) != 0 && !self.enable_show_via_cvar(r, CVAR_ENABLE) {
            return;
        }
        if cf & (CVAR_SHOW | CVAR_HIDE) != 0 && !self.enable_show_via_cvar(r, CVAR_SHOW) {
            return;
        }
        let run = |ui: &mut Ui, s: Script| {
            if !s.is_empty() {
                ui.run_script(r.set, r.menu, Some(r.item), &s);
            }
        };
        if contains(t, x, y) {
            if self.it(r).flags & WINDOW_MOUSEOVERTEXT == 0 {
                let s = self.it(r).def.mouse_enter_text.clone();
                run(self, s);
                self.it_mut(r).flags |= WINDOW_MOUSEOVERTEXT;
            }
            if self.it(r).flags & WINDOW_MOUSEOVER == 0 {
                let s = self.it(r).def.mouse_enter.clone();
                run(self, s);
                self.it_mut(r).flags |= WINDOW_MOUSEOVER;
            }
        } else {
            if self.it(r).flags & WINDOW_MOUSEOVERTEXT != 0 {
                let s = self.it(r).def.mouse_exit_text.clone();
                run(self, s);
                self.it_mut(r).flags &= !WINDOW_MOUSEOVERTEXT;
            }
            if self.it(r).flags & WINDOW_MOUSEOVER == 0 {
                let s = self.it(r).def.mouse_enter.clone();
                run(self, s);
                self.it_mut(r).flags |= WINDOW_MOUSEOVER;
            }
            if self.list_rect(r).1 == IT_LISTBOX {
                self.list_mouse_enter(r, x, y);
            }
        }
    }

    /// `Item_MouseLeave`.
    fn item_mouse_leave(&mut self, r: IRef) {
        if self.it(r).flags & WINDOW_MOUSEOVERTEXT != 0 {
            let s = self.it(r).def.mouse_exit_text.clone();
            self.run_script(r.set, r.menu, Some(r.item), &s);
            self.it_mut(r).flags &= !WINDOW_MOUSEOVERTEXT;
        }
        let s = self.it(r).def.mouse_exit.clone();
        if !s.is_empty() {
            self.run_script(r.set, r.menu, Some(r.item), &s);
        }
        self.it_mut(r).flags &= !(WINDOW_LB_DOWNARROW | WINDOW_LB_UPARROW);
    }

    /// `Item_SetFocus`.
    fn item_set_focus(&mut self, r: IRef, x: f32, y: f32) -> bool {
        let fl = self.it(r).flags;
        if fl & WINDOW_DECORATION != 0 || fl & WINDOW_HASFOCUS != 0 || fl & WINDOW_VISIBLE == 0 {
            return false;
        }
        let cf = self.it(r).cvar_flags;
        if cf & (CVAR_ENABLE | CVAR_DISABLE) != 0 && !self.enable_show_via_cvar(r, CVAR_ENABLE) {
            return false;
        }
        if cf & (CVAR_SHOW | CVAR_HIDE) != 0 && !self.enable_show_via_cvar(r, CVAR_SHOW) {
            return false;
        }
        let old = self.menu_clear_focus(r.set, r.menu);
        let mut sound = None;
        if self.it(r).ty == IT_TEXT {
            let mut t = self.it(r).text_rect;
            t[1] -= t[3];
            if contains(t, x, y) {
                self.it_mut(r).flags |= WINDOW_HASFOCUS;
                sound = Some(self.it(r).def.focus_sound.clone().unwrap_or_else(|| ITEM_FOCUS_SOUND.into()));
            } else if let Some(o) = old {
                self.sets[r.set][r.menu].items[o].flags |= WINDOW_HASFOCUS;
                let of = self.sets[r.set][r.menu].items[o].def.on_focus.clone();
                if !of.is_empty() {
                    self.run_script(r.set, r.menu, Some(o), &of);
                }
            }
        } else {
            self.it_mut(r).flags |= WINDOW_HASFOCUS;
            let of = self.it(r).def.on_focus.clone();
            if !of.is_empty() {
                self.run_script(r.set, r.menu, Some(r.item), &of);
            }
            sound = Some(self.it(r).def.focus_sound.clone().unwrap_or_else(|| ITEM_FOCUS_SOUND.into()));
        }
        if let Some(s) = sound {
            self.play(&s);
        }
        self.sets[r.set][r.menu].cursor_item = r.item as i32;
        true
    }

    /// `Item_HandleMouseMove` + `Menu_HandleMouseMove`.
    fn menu_handle_mouse_move(&mut self, set: usize, mi: usize, x: f32, y: f32) {
        if self.sets[set][mi].flags & (WINDOW_VISIBLE | WINDOW_FORCED) == 0 {
            return;
        }
        if self.item_capture.is_some() || self.editing.is_some() || self.waiting_for_key {
            return;
        }
        let mut focus_set = false;
        if let Some(c) = self.combo {
            self.item_set_focus(c, x, y);
            focus_set = true;
        }
        for pass in 0..2 {
            for ii in 0..self.sets[set][mi].items.len() {
                if mi >= self.sets[set].len() || ii >= self.sets[set][mi].items.len() {
                    return;
                }
                let r = Self::iref(set, mi, ii);
                if self.it(r).flags & (WINDOW_VISIBLE | WINDOW_FORCED) == 0 {
                    continue;
                }
                let cf = self.it(r).cvar_flags;
                if cf & (CVAR_ENABLE | CVAR_DISABLE) != 0 && !self.enable_show_via_cvar(r, CVAR_ENABLE) {
                    continue;
                }
                if cf & (CVAR_SHOW | CVAR_HIDE) != 0 && !self.enable_show_via_cvar(r, CVAR_SHOW) {
                    continue;
                }
                let (rect, ty) = self.list_rect(r);
                let result = if contains(rect, x, y) {
                    if pass == 1 {
                        let it = self.it(r);
                        let skip = ty == IT_TEXT && it.def.text.is_some() && !contains(self.corrected_text_rect(r), x, y);
                        if !skip && it.flags & WINDOW_VISIBLE != 0 && it.flags & WINDOW_FADINGOUT == 0 {
                            self.item_mouse_enter(r, x, y);
                            if !focus_set {
                                focus_set = self.item_set_focus(r, x, y);
                            }
                        }
                    }
                    true
                } else {
                    false
                };
                if !result && self.it(r).flags & WINDOW_MOUSEOVER != 0 {
                    self.item_mouse_leave(r);
                    self.it_mut(r).flags &= !WINDOW_MOUSEOVER;
                }
            }
        }
    }

    /// `UI_MouseEvent` / `UI_SetMousePosition` -> `Display_MouseMove`.
    pub fn mouse_move(&mut self, x: f32, y: f32) {
        self.cursor = (x.clamp(0.0, 640.0), y.clamp(0.0, 480.0));
        let (x, y) = self.cursor;
        if let Some(m) = self.menu_get_focused() {
            if self.sets[0][m].flags & WINDOW_POPUP != 0 {
                self.menu_handle_mouse_move(0, m, x, y);
                return;
            }
        }
        for mi in 0..self.sets[0].len() {
            self.menu_handle_mouse_move(0, mi, x, y);
        }
    }

    /// `Menu_SetNextCursorItem` / `Menu_SetPrevCursorItem`.
    fn menu_step_cursor_item(&mut self, mi: usize, forward: bool) -> Option<usize> {
        let n = self.sets[0][mi].items.len() as i32;
        let old = self.sets[0][mi].cursor_item;
        let mut c = old;
        let mut wrapped = false;
        if forward && c == -1 {
            c = 0;
            wrapped = true;
        } else if !forward && c < 0 {
            c = n - 1;
            wrapped = true;
        }
        let (cx, cy) = self.cursor;
        loop {
            if forward {
                if c >= n {
                    break;
                }
                c += 1;
                if c >= n && !wrapped {
                    wrapped = true;
                    c = 0;
                }
            } else {
                if c <= -1 {
                    break;
                }
                c -= 1;
                if c < 0 && !wrapped {
                    wrapped = true;
                    c = n - 1;
                }
            }
            if c < 0 || c >= n {
                break;
            }
            self.sets[0][mi].cursor_item = c;
            let r = Self::iref(0, mi, c as usize);
            if self.item_set_focus(r, cx, cy) {
                let rr = self.it(r).rect;
                self.menu_handle_mouse_move(0, mi, rr[0] + 1.0, rr[1] + 1.0);
                return Some(c as usize);
            }
        }
        self.sets[0][mi].cursor_item = old;
        None
    }

    /// `UI_InstallCaptureFunc`.
    fn install_capture(&mut self, c: Capture, timeout: i32) {
        self.capture = c;
        self.capture_expiry = if timeout > 0 { self.real_time + timeout } else { 0 };
    }

    /// `UI_RemoveCaptureFunc`.
    fn remove_capture(&mut self) {
        self.capture = Capture::None;
        self.capture_expiry = 0;
    }

    /// `Item_StartCapture`.
    fn item_start_capture(&mut self, r: IRef, key: K) {
        if !matches!(self.capture, Capture::None) {
            return;
        }
        let (cx, cy) = self.cursor;
        match self.it(r).ty {
            IT_LISTBOX | IT_COMBOBOX => {
                let flags = self.list_over_lb(r, cx, cy);
                if flags & (WINDOW_LB_UPARROW | WINDOW_LB_DOWNARROW) != 0 {
                    let t = self.real_time;
                    self.install_capture(Capture::ListAuto { item: r, key, next_scroll: t + SCROLL_TIME_START, next_adjust: t + SCROLL_TIME_ADJUST, adjust: SCROLL_TIME_START }, 0);
                    self.item_capture = Some(r);
                } else if flags & WINDOW_LB_THUMB != 0 {
                    self.install_capture(Capture::ListThumb { item: r, key, y_start: cy, next_scroll: 0, next_adjust: 0, adjust: 0 }, 0);
                    self.item_capture = Some(r);
                }
            }
            IT_SLIDER => {
                if self.slider_over(r, cx, cy) {
                    self.install_capture(Capture::Slider { item: r }, 0);
                    self.item_capture = Some(r);
                }
            }
            _ => {}
        }
    }

    /// The capture function of this frame (`Menu_PaintAll`).
    fn run_capture(&mut self) {
        if matches!(self.capture, Capture::None) {
            return;
        }
        if self.capture_expiry > 0 && self.real_time > self.capture_expiry {
            self.remove_capture();
            return;
        }
        let t = self.real_time;
        match self.capture {
            Capture::None => {}
            Capture::ListAuto { item, key, mut next_scroll, mut next_adjust, mut adjust } => {
                if t > next_scroll {
                    self.list_handle_key(item, key, true, false);
                    next_scroll = t + adjust;
                }
                if t > next_adjust {
                    next_adjust = t + SCROLL_TIME_ADJUST;
                    if adjust > SCROLL_TIME_FLOOR {
                        adjust -= SCROLL_TIME_ADJUSTOFFSET;
                    }
                }
                if let Capture::ListAuto { .. } = self.capture {
                    self.capture = Capture::ListAuto { item, key, next_scroll, next_adjust, adjust };
                }
            }
            Capture::ListThumb { item, key, mut y_start, mut next_scroll, mut next_adjust, mut adjust } => {
                let cy = self.cursor.1;
                if cy != y_start {
                    let rect = self.list_rect(item).0;
                    let ry = rect[1] + SCROLLBAR_ARROW_SIZE + 1.0;
                    let rh = rect[3] - SCROLLBAR_ARROW_SIZE * 2.0 - 2.0;
                    let max = self.list_max_scroll(item);
                    let pos = (((cy - ry - SCROLLBAR_ARROW_SIZE / 2.0) * max as f32) / (rh - SCROLLBAR_ARROW_SIZE)) as i32;
                    self.list_set_start_pos(item, pos.clamp(0, max));
                    y_start = cy;
                }
                if t > next_scroll {
                    self.list_handle_key(item, key, true, false);
                    next_scroll = t + adjust;
                }
                if t > next_adjust {
                    next_adjust = t + SCROLL_TIME_ADJUST;
                    if adjust > SCROLL_TIME_FLOOR {
                        adjust -= SCROLL_TIME_ADJUSTOFFSET;
                    }
                }
                if let Capture::ListThumb { .. } = self.capture {
                    self.capture = Capture::ListThumb { item, key, y_start, next_scroll, next_adjust, adjust };
                }
            }
            Capture::Slider { item } => {
                let x = self.slider_x0(item);
                let w = 96.0 * self.aspect;
                let cx = self.cursor.0.clamp(x, x + w);
                let e = self.it(item).edit;
                let v = (cx - x) / w * (e.max - e.min) + e.min;
                if let Some(c) = self.it(item).def.cvar.clone() {
                    self.set_cvar(&c, &format!("{v:.6}"));
                }
            }
            Capture::ArmouryRefresh => {
                // UI_ArmouryRefreshCb
                let old = self.carriage.clone();
                self.parse_carriage();
                if self.carriage != old {
                    let p = self.view.player.clone();
                    self.lists.insert(Feeder::ArmouryBuy, load_armoury_buys(&p));
                    self.lists.insert(Feeder::ArmourySell, load_armoury_sells(&p));
                    self.remove_capture();
                }
            }
        }
    }

    /// Slider value x origin (`textRect` right + ITEM_VALUE_OFFSET, else rect.x).
    fn slider_x0(&self, r: IRef) -> f32 {
        let it = self.it(r);
        if it.def.text.is_some() {
            it.text_rect[0] + it.text_rect[2] + ITEM_VALUE_OFFSET
        } else {
            it.rect[0]
        }
    }

    /// `Item_Slider_ThumbPosition`.
    fn slider_thumb_position(&self, r: IRef) -> f32 {
        let x = self.slider_x0(r);
        let it = self.it(r);
        let Some(c) = &it.def.cvar else { return x };
        let e = it.edit;
        let v = self.cvar_value(c).clamp(e.min, e.max);
        let range = e.max - e.min;
        x + if range != 0.0 { (v - e.min) / range } else { 0.0 } * 96.0 * self.aspect
    }

    /// `Item_Slider_VScale`.
    fn slider_vscale(&self, r: IRef) -> f32 {
        let h = self.it(r).rect[3];
        if SLIDER_THUMB_HEIGHT > h {
            h / SLIDER_THUMB_HEIGHT
        } else {
            1.0
        }
    }

    /// `Item_Slider_OverSlider`.
    fn slider_over(&self, r: IRef, x: f32, y: f32) -> bool {
        let vs = self.slider_vscale(r);
        let tr = self.it(r).text_rect;
        let tw = 12.0 * self.aspect;
        let t = [self.slider_thumb_position(r) - tw / 2.0, tr[1] - tr[3] + (tr[3] - SLIDER_THUMB_HEIGHT * vs) / 2.0, tw, SLIDER_THUMB_HEIGHT * vs];
        contains(t, x, y)
    }

    /// `Item_ListBox_HandleKey`.
    fn list_handle_key(&mut self, r: IRef, key: K, _down: bool, force: bool) -> bool {
        let f = self.it(r).feeder;
        let count = self.feeder_count(f);
        let (rect, _) = self.list_rect(r);
        let (cx, cy) = self.cursor;
        if !(force || (contains(rect, cx, cy) && self.it(r).flags & WINDOW_HASFOCUS != 0)) {
            return false;
        }
        let viewmax = self.list_num_items(r);
        let fl = self.it(r).flags;
        let start = self.it(r).list.start;
        let notsel = self.it(r).def.not_selectable;
        match key {
            K::Mouse1 | K::Mouse2 => {
                if fl & WINDOW_LB_UPARROW != 0 {
                    self.list_set_start_pos(r, start - 1);
                } else if fl & WINDOW_LB_DOWNARROW != 0 {
                    self.list_set_start_pos(r, start + 1);
                } else if fl & WINDOW_LB_PGUP != 0 {
                    self.list_set_start_pos(r, start - viewmax);
                } else if fl & WINDOW_LB_PGDN != 0 {
                    self.list_set_start_pos(r, start + viewmax);
                } else if fl & WINDOW_LB_THUMB != 0 {
                    // handled by the capture function
                } else {
                    let lc = self.it(r).list.cursor;
                    // mouse isn't over an item
                    if lc < 0 {
                        return true;
                    }
                    if self.it(r).cursor_pos != lc {
                        self.it_mut(r).cursor_pos = lc;
                        self.feeder_selection(f, lc);
                    }
                    let dc = self.it(r).def.double_click.clone();
                    let mut run = self.real_time < self.last_list_click && !dc.is_empty();
                    self.last_list_click = self.real_time + DOUBLE_CLICK_DELAY;
                    // made a selection, so close combobox
                    if self.combo.is_some() {
                        if !dc.is_empty() {
                            run = true;
                        }
                        self.combo = None;
                    }
                    if run {
                        self.run_script(r.set, r.menu, Some(r.item), &dc);
                    }
                }
            }
            K::WheelUp => self.list_set_start_pos(r, start - 1),
            K::WheelDown => self.list_set_start_pos(r, start + 1),
            K::Enter => {
                // invoke the doubleClick handler when enter is pressed
                let dc = self.it(r).def.double_click.clone();
                if !dc.is_empty() {
                    self.run_script(r.set, r.menu, Some(r.item), &dc);
                }
            }
            K::PgUp | K::PgDn => {
                if notsel {
                    let d = if key == K::PgUp { -viewmax } else { viewmax };
                    self.list_set_start_pos(r, start + d);
                } else {
                    let mut c = self.it(r).list.cursor;
                    if key == K::PgUp {
                        c = (c - viewmax).max(0);
                        self.it_mut(r).list.cursor = c;
                        if c < start {
                            self.list_set_start_pos(r, c);
                        }
                    } else {
                        c += viewmax;
                        self.it_mut(r).list.cursor = c;
                        if c < start {
                            self.list_set_start_pos(r, c);
                        }
                        if c >= count {
                            c = count - 1;
                        }
                        self.it_mut(r).list.cursor = c;
                    }
                    let start = self.it(r).list.start;
                    if c >= start + viewmax {
                        self.list_set_start_pos(r, c - viewmax + 1);
                    }
                    self.it_mut(r).cursor_pos = c;
                    self.feeder_selection(f, c);
                }
            }
            _ => return false,
        }
        true
    }

    /// `Item_YesNo_HandleKey`.
    fn yesno_handle_key(&mut self, r: IRef, key: K) -> bool {
        let (cx, cy) = self.cursor;
        let it = self.it(r);
        if contains(it.rect, cx, cy) && it.flags & WINDOW_HASFOCUS != 0 {
            if let Some(c) = it.def.cvar.clone() {
                if matches!(key, K::Mouse1 | K::Enter | K::Mouse2 | K::Mouse3) {
                    let v = (self.cvar_value(&c) == 0.0) as i32;
                    self.set_cvar(&c, &v.to_string());
                    return true;
                }
            }
        }
        false
    }

    /// `Item_Multi_FindCvarByValue`.
    fn multi_find(&self, r: IRef) -> usize {
        let it = self.it(r);
        let Some(c) = &it.def.cvar else { return 0 };
        if it.def.multi_str {
            let buff = self.cvar(c);
            it.def.multi.iter().position(|(_, v)| v.eq_ignore_ascii_case(&buff)).unwrap_or(0)
        } else {
            let value = self.cvar_value(c);
            it.def.multi.iter().position(|(_, v)| v.parse::<f32>().ok() == Some(value)).unwrap_or(0)
        }
    }

    /// `Item_Multi_Setting`.
    fn multi_setting(&self, r: IRef) -> String {
        let it = self.it(r);
        let Some(c) = &it.def.cvar else { return String::new() };
        let found = if it.def.multi_str {
            let buff = self.cvar(c);
            it.def.multi.iter().find(|(_, v)| v.eq_ignore_ascii_case(&buff))
        } else {
            let value = self.cvar_value(c);
            it.def.multi.iter().find(|(_, v)| v.parse::<f32>().ok() == Some(value))
        };
        found.map(|(l, _)| l.clone()).unwrap_or_default()
    }

    /// `Item_Multi_HandleKey`.
    fn multi_handle_key(&mut self, r: IRef, key: K) -> bool {
        let (cx, cy) = self.cursor;
        let it = self.it(r);
        let over = contains(it.rect, cx, cy);
        let max = it.def.multi.len();
        let Some(c) = it.def.cvar.clone() else { return false };
        if it.flags & WINDOW_HASFOCUS == 0 || max == 0 {
            return false;
        }
        let current = if (over && key == K::Mouse1) || matches!(key, K::Enter | K::Right | K::Down) {
            (self.multi_find(r) + 1) % max
        } else if (over && key == K::Mouse2) || matches!(key, K::Left | K::Up) {
            (self.multi_find(r) + max - 1) % max
        } else {
            return false;
        };
        let (_, v) = self.it(r).def.multi[current].clone();
        if self.it(r).def.multi_str {
            self.set_cvar(&c, &v);
        } else {
            let f = v.parse::<f32>().unwrap_or(0.0);
            if f == f as i32 as f32 {
                self.set_cvar(&c, &(f as i32).to_string());
            } else {
                self.set_cvar(&c, &format!("{f:.6}"));
            }
        }
        true
    }

    /// `Item_Slider_HandleKey`.
    fn slider_handle_key(&mut self, r: IRef, key: K) -> bool {
        let (cx, cy) = self.cursor;
        let it = self.it(r);
        let Some(c) = it.def.cvar.clone() else { return false };
        if it.flags & WINDOW_HASFOCUS == 0 || !contains(it.rect, cx, cy) || !matches!(key, K::Enter | K::Mouse1 | K::Mouse2 | K::Mouse3) {
            return false;
        }
        let x = self.slider_x0(r);
        let w = 96.0 * self.aspect;
        let half = 12.0 * self.aspect / 2.0;
        let mut t = self.it(r).rect;
        t[0] = x - half;
        t[2] = w + half;
        if contains(t, cx, cy) {
            let e = self.it(r).edit;
            let v = (cx - x) / w * (e.max - e.min) + e.min;
            self.set_cvar(&c, &format!("{v:.6}"));
            return true;
        }
        false
    }

    /// `Item_HandleKey`.
    fn item_handle_key(&mut self, r: IRef, key: K, down: bool) -> bool {
        if self.item_capture.is_some() {
            self.item_capture = None;
            self.remove_capture();
        } else if down && key.is_mouse() {
            self.item_start_capture(r, key);
        }
        if !down {
            return false;
        }
        let ty = self.it(r).ty;
        if is_edit_field(ty) {
            return false;
        }
        match ty {
            IT_LISTBOX => self.list_handle_key(r, key, down, false),
            IT_COMBOBOX => {
                if self.combo.is_some() {
                    let res = self.list_handle_key(r, key, down, false);
                    if !res {
                        self.combo = None;
                    }
                    res
                } else {
                    let (cx, cy) = self.cursor;
                    if contains(self.it(r).rect, cx, cy) && self.it(r).flags & WINDOW_HASFOCUS != 0 && matches!(key, K::Mouse1 | K::Mouse2) {
                        self.combo = Some(r);
                        true
                    } else {
                        false
                    }
                }
            }
            IT_YESNO => self.yesno_handle_key(r, key),
            IT_MULTI => self.multi_handle_key(r, key),
            IT_SLIDER => self.slider_handle_key(r, key),
            IT_BIND => self.bind_handle_key(r, key, down),
            // Item_OwnerDraw_HandleKey -> UI_OwnerDrawHandleKey (returns qfalse)
            IT_OWNERDRAW => {
                if self.ods.get(&self.it(r).def.ownerdraw) == Some(&Od::NetSource) {
                    self.net_source_handle_key(key);
                }
                false
            }
            _ => false,
        }
    }

    /// `Item_TextField_CalcPaintOffset`.
    fn text_field_calc_paint_offset(&mut self, r: IRef, buff: &str) {
        let it = self.it(r);
        let (cursor, scale) = (it.cursor_pos, it.def.text_scale);
        let mut e = it.edit;
        if cursor < e.paint_offset {
            e.paint_offset = cursor;
        } else if e.max_field_width > 0 {
            let b = buff.as_bytes();
            let w = |off: i32, end: usize| self.fonts.width(&String::from_utf8_lossy(&b[(off.max(0) as usize).min(end)..end]), scale, self.aspect);
            let lim = (e.max_field_width - EDIT_CURSOR_WIDTH) as f32;
            let end = ((cursor + 1).max(0) as usize).min(b.len());
            // if the cursor is at the end of the string, maximise the amount
            // of the string that's visible
            if cursor as usize + 1 >= b.len() {
                while w(e.paint_offset, b.len()) <= lim && e.paint_offset > 0 {
                    e.paint_offset -= 1;
                }
            }
            // shift paintOffset so that the cursor is visible
            while w(e.paint_offset, end) > lim && (e.paint_offset as usize) < end {
                e.paint_offset += 1;
            }
        }
        self.it_mut(r).edit = e;
    }

    /// `Item_TextField_HandleKey`: false when the field releases the focus.
    fn text_field_handle_key(&mut self, r: IRef, key: K) -> bool {
        let Some(cvar) = self.it(r).def.cvar.clone() else {
            return false;
        };
        let mut buff: Vec<u8> = self.cvar(&cvar).into_bytes();
        let mut len = buff.len() as i32;
        if len < self.it(r).cursor_pos {
            self.it_mut(r).cursor_pos = len;
        }
        let e = self.it(r).edit;
        if e.max_chars != 0 && len > e.max_chars {
            len = e.max_chars;
        }
        let ty = self.it(r).ty;
        let release;
        let mut cur = self.it(r).cursor_pos;
        'exit: {
            match key {
                K::Char(c) => {
                    if c == 8 {
                        // ctrl-h is backspace
                        if cur > 0 {
                            buff.remove((cur - 1) as usize);
                            cur -= 1;
                        }
                        self.set_cvar(&cvar, &String::from_utf8_lossy(&buff));
                    } else if c < 32 {
                        // ignore any non printable chars
                        release = false;
                        break 'exit;
                    } else if ty == IT_NUMERICFIELD && !c.is_ascii_digit() {
                        release = false;
                        break 'exit;
                    } else {
                        if !self.overstrike {
                            if len == MAX_EDITFIELD - 1 || (e.max_chars != 0 && len >= e.max_chars) {
                                release = false;
                                break 'exit;
                            }
                            buff.insert((cur as usize).min(buff.len()), c);
                        } else {
                            if e.max_chars != 0 && cur >= e.max_chars {
                                release = false;
                                break 'exit;
                            }
                            if (cur as usize) < buff.len() {
                                buff[cur as usize] = c;
                            } else {
                                buff.push(c);
                            }
                        }
                        self.set_cvar(&cvar, &String::from_utf8_lossy(&buff));
                        if cur < len + 1 {
                            cur += 1;
                        }
                    }
                }
                K::Del => {
                    if cur < len {
                        buff.remove(cur as usize);
                        self.set_cvar(&cvar, &String::from_utf8_lossy(&buff));
                    }
                }
                K::Right => {
                    if cur < len {
                        cur += 1;
                    }
                }
                K::Left => {
                    if cur > 0 {
                        cur -= 1;
                    }
                }
                K::Home => cur = 0,
                K::End => cur = len,
                K::Ins => self.overstrike = !self.overstrike,
                K::Tab | K::Down | K::Up => {
                    // ignore these keys from the say field
                    if ty != IT_SAYFIELD {
                        self.it_mut(r).cursor_pos = cur;
                        let n = if r.set == 0 { self.menu_step_cursor_item(r.menu, true) } else { None };
                        match n {
                            Some(ni) if is_edit_field(self.sets[0][r.menu].items[ni].ty) => {
                                self.editing = Some(Self::iref(0, r.menu, ni));
                            }
                            _ => {
                                release = true;
                                break 'exit;
                            }
                        }
                    }
                }
                K::Mouse1 | K::Mouse2 | K::Mouse3 if ty == IT_SAYFIELD => {}
                K::Mouse1 | K::Mouse2 | K::Mouse3 | K::Enter | K::KpEnter | K::Escape => {
                    release = true;
                    break 'exit;
                }
                _ => {}
            }
            release = false;
        }
        self.it_mut(r).cursor_pos = cur;
        let s = String::from_utf8_lossy(&buff).into_owned();
        self.text_field_calc_paint_offset(r, &s);
        !release
    }

    /// `Menus_HandleOOBClick`.
    fn menus_handle_oob_click(&mut self, mi: usize, key: K, down: bool) {
        if down && self.sets[0][mi].flags & WINDOW_OOB_CLICK != 0 {
            self.menus_close(mi);
        }
        let (cx, cy) = self.cursor;
        for i in 0..self.sets[0].len() {
            if self.menu_over_active_item(i, cx, cy) {
                self.menus_close(mi);
                self.menus_activate(i);
                self.menu_handle_mouse_move(0, i, cx, cy);
                self.menu_handle_key(i, key, down);
            }
        }
    }

    /// `Menu_OverActiveItem`.
    fn menu_over_active_item(&self, mi: usize, x: f32, y: f32) -> bool {
        let m = &self.sets[0][mi];
        if m.flags & (WINDOW_VISIBLE | WINDOW_FORCED) == 0 || !contains(m.rect, x, y) {
            return false;
        }
        for (ii, it) in m.items.iter().enumerate() {
            if it.flags & (WINDOW_VISIBLE | WINDOW_FORCED) == 0 || it.flags & WINDOW_DECORATION != 0 {
                continue;
            }
            if contains(it.rect, x, y) {
                if it.ty == IT_TEXT && it.def.text.is_some() {
                    if contains(self.corrected_text_rect(Self::iref(0, mi, ii)), x, y) {
                        return true;
                    }
                    continue;
                }
                return true;
            }
        }
        false
    }

    /// `Menu_HandleKey`.
    fn menu_handle_key(&mut self, mi: usize, key: K, down: bool) {
        if self.waiting_for_key && down {
            if let Some(b) = self.bind_item {
                self.bind_handle_key(b, key, down);
            }
            return;
        }
        if let (Some(e), true) = (self.editing, down) {
            if !self.text_field_handle_key(e, key) {
                self.editing = None;
                let s = self.it(e).def.on_text_entry.clone();
                self.run_script(e.set, e.menu, Some(e.item), &s);
                return;
            } else {
                // (onCharEntry runs on the item being edited, which a
                // Menus_ReplaceActive may have moved)
                let e = self.editing.unwrap_or(e);
                let s = self.it(e).def.on_char_entry.clone();
                self.run_script(e.set, e.menu, Some(e.item), &s);
            }
        }
        if mi >= self.sets[0].len() {
            return;
        }
        let (cx, cy) = self.cursor;
        // see if the mouse is within the window bounds and if so is this a mouse click
        if down && self.sets[0][mi].flags & WINDOW_POPUP == 0 && !contains(self.sets[0][mi].rect, cx, cy) && key.is_mouse() {
            self.menus_handle_oob_click(mi, key, down);
            return;
        }
        let item = match self.combo {
            None => self.sets[0][mi].items.iter().rposition(|i| i.flags & WINDOW_HASFOCUS != 0).map(|ii| Self::iref(0, mi, ii)),
            Some(c) => Some(c),
        };
        if let Some(r) = item {
            if self.item_handle_key(r, key, down) {
                let a = self.it(r).def.action.clone();
                self.run_script(r.set, r.menu, Some(r.item), &a);
                return;
            }
        }
        if !down {
            return;
        }
        match key {
            K::Up => {
                self.menu_step_cursor_item(mi, false);
            }
            K::Escape => {
                let s = self.sets[0][mi].def.on_esc.clone();
                if !self.waiting_for_key && !s.is_empty() {
                    self.run_script(0, mi, None, &s);
                }
            }
            K::Tab | K::Down => {
                self.menu_step_cursor_item(mi, true);
            }
            K::Mouse1 | K::Mouse2 => {
                if let Some(r) = item {
                    let it = self.it(r);
                    if it.ty == IT_TEXT {
                        if contains(self.corrected_text_rect(r), cx, cy) {
                            let a = it.def.action.clone();
                            self.run_script(r.set, r.menu, Some(r.item), &a);
                        }
                    } else if is_edit_field(it.ty) {
                        if contains(it.rect, cx, cy) {
                            self.begin_edit(r);
                        }
                    } else if contains(it.rect, cx, cy) {
                        let a = it.def.action.clone();
                        self.run_script(r.set, r.menu, Some(r.item), &a);
                    }
                }
            }
            K::Enter | K::KpEnter => {
                if let Some(r) = item {
                    if is_edit_field(self.it(r).ty) {
                        self.begin_edit(r);
                    } else {
                        let a = self.it(r).def.action.clone();
                        self.run_script(r.set, r.menu, Some(r.item), &a);
                    }
                }
            }
            _ => {}
        }
    }

    /// The edit-field activation of `Menu_HandleKey` (cursor at the end).
    fn begin_edit(&mut self, r: IRef) {
        let buffer = self.it(r).def.cvar.as_deref().map(|c| self.cvar(c)).unwrap_or_default();
        self.it_mut(r).cursor_pos = buffer.len() as i32;
        self.text_field_calc_paint_offset(r, &buffer);
        self.editing = Some(r);
    }

    /// `UI_KeyEvent` for a keycodes.h key number.
    pub fn key_event_num(&mut self, keynum: i32, down: bool) {
        self.keynum = keynum;
        self.key_event(k_from_keynum(keynum), down);
    }

    /// `UI_KeyEvent`.
    pub fn key_event(&mut self, key: K, down: bool) {
        if !matches!(key, K::Char(_)) && k_from_keynum(self.keynum) != key {
            self.keynum = keynum_from_k(key);
        }
        match self.menu_get_focused() {
            Some(m) => {
                if key == K::Escape && down && !self.any_fullscreen_visible() {
                    self.menus_close_all();
                } else {
                    self.menu_handle_key(m, key, down);
                }
            }
            None => {
                // the key catcher goes: nothing is open
                self.stack.clear();
            }
        }
    }

    /// `CG_scrollScoresUp_f` / `CG_scrollScoresDown_f`: `Menu_ScrollFeeder`
    /// (an Up / Down arrow on the list) for both team lists.
    pub fn scroll_scores(&mut self, down: bool) {
        if !self.show_scores {
            return;
        }
        let Some(mi) = self.sets[1].iter().position(|m| m.name().eq_ignore_ascii_case(SCORE_MENU)) else { return };
        for f in [Feeder::AlienTeamList, Feeder::HumanTeamList] {
            let feeders = &self.feeders;
            if let Some(ii) = self.sets[1][mi].items.iter().position(|it| feeders.get(&it.feeder) == Some(&f)) {
                let r = Self::iref(1, mi, ii);
                self.list_handle_key(r, if down { K::Down } else { K::Up }, true, true);
            }
        }
    }

    /// `UI_MessageMode_f` (messagemode / messagemode2).
    pub fn message_mode(&mut self, team: bool) {
        self.set_cvar("ui_sayBuffer", "");
        self.chat_team = team;
        self.menus_close_by_name("say");
        self.menus_close_by_name("say_team");
        self.menus_activate_by_name(if team { "say_team" } else { "say" });
    }

    /// `UI_SetActiveMenu( UIMENU_INGAME )`.
    pub fn open_ingame(&mut self) {
        self.build_player_list();
        self.menus_close_all();
        self.menus_activate_by_name("ingame");
    }

    /// `CG_Menu` (the dialog/menu a G_TriggerMenu message opens). Returns
    /// true when a menu opened.
    pub fn cg_menu(&mut self, m: CgMenu) -> bool {
        let dialog = match self.view.player.team {
            Team::Aliens => "tremulous_alien_dialog",
            Team::Humans => "tremulous_human_dialog",
            Team::None => "tremulous_default_dialog",
        };
        match m {
            CgMenu::Open(name) => self.menus_activate_by_name(name).is_some(),
            CgMenu::Dialog(long, ty) => {
                let off = |n: &str| self.cvar_value(n) != 0.0;
                if (ty == DialogType::ArmouryEvolve && off("cg_disableUpgradeDialogs")) || (ty == DialogType::Build && off("cg_disableBuildDialogs")) || (ty == DialogType::Command && off("cg_disableCommandDialogs")) {
                    return false;
                }
                match long {
                    Some(l) if self.cvar_value("cg_disableWarningDialogs") == 0.0 => {
                        self.set_cvar("ui_dialog", l);
                        self.menus_activate_by_name(dialog).is_some()
                    }
                    // the short message is printed by the game already
                    _ => false,
                }
            }
        }
    }

    /// One frame: game snapshot in, `Menu_UpdateAll` + capture function.
    pub fn frame(&mut self, real_time: i32, view: GameView) {
        self.real_time = real_time;
        self.view = view;
        // cvars cgame sets from configstrings
        let v = self.view.votes;
        for (n, on) in [("ui_voteActive", v[0]), ("ui_alienTeamVoteActive", v[1]), ("ui_humanTeamVoteActive", v[2])] {
            let s = if on { "1" } else { "0" };
            if self.cvar(n) != s {
                self.cvars.insert(n.to_ascii_lowercase(), s.into());
            }
        }
        let w = self.view.scores.winner.clone();
        self.cvars.insert("ui_winner".into(), w);
        let p = &self.view.player;
        let cc = format!("{} {}", p.class, p.credits);
        self.cvars.insert("ui_currentclass".into(), cc);
        // Menu_UpdateAll
        for i in 0..self.stack.len() {
            let mi = self.stack[i];
            self.menu_update(0, mi);
        }
        // UI_Refresh: the browser, server status, find player and news
        if !self.sets[0].is_empty() {
            self.do_server_refresh();
            self.build_server_status(false);
            self.build_find_player_list(false);
            self.update_news(false);
        }
    }

    /// `Menu_Update` (`Item_ListBox_Update`).
    fn menu_update(&mut self, set: usize, mi: usize) {
        for ii in 0..self.sets[set][mi].items.len() {
            let r = Self::iref(set, mi, ii);
            if !is_list_box(self.it(r).ty) {
                continue;
            }
            let f = self.it(r).feeder;
            let n = self.feeder_count(f);
            if self.it(r).list.last_count != n {
                if self.it(r).def.reset_on_feeder_change {
                    let c = self.feeder_initialise(f);
                    self.it_mut(r).cursor_pos = c;
                    self.list_set_start_pos(r, 0);
                    self.feeder_selection(f, c);
                } else {
                    let s = self.it(r).list.start;
                    self.list_set_start_pos(r, s);
                    if self.it(r).cursor_pos >= n {
                        self.it_mut(r).cursor_pos = n - 1;
                    }
                }
            }
            self.it_mut(r).list.last_count = n;
        }
    }

    /// `CG_SetScoreSelection` (on the first frame the scoreboard shows).
    fn cg_set_score_selection(&mut self, mi: usize) {
        let s = &self.view.scores;
        let (alien, human) = (s.rows.iter().filter(|r| r.team == Team::Aliens).count() as i32, s.rows.iter().filter(|r| r.team == Team::Humans).count() as i32);
        let mine = s.rows.iter().find(|r| r.client == self.view.me).map(|r| r.team);
        let (feeder, i) = if mine == Some(Team::Humans) { ("FEEDER_HUMANTEAM_LIST", human) } else { ("FEEDER_ALIENTEAM_LIST", alien) };
        let Some(id) = self.feeders.iter().find(|(_, f)| **f == if feeder == "FEEDER_HUMANTEAM_LIST" { Feeder::HumanTeamList } else { Feeder::AlienTeamList }).map(|(id, _)| *id) else { return };
        // Menu_SetFeederSelection( menu, feeder, i, NULL )
        if let Some(ii) = self.sets[1][mi].items.iter().position(|it| it.feeder == id) {
            let r = Self::iref(1, mi, ii);
            if is_list_box(self.it(r).ty) && i == 0 {
                self.it_mut(r).list.cursor = 0;
                self.list_set_start_pos(r, 0);
            }
            self.it_mut(r).cursor_pos = i;
            self.feeder_selection(id, i);
        }
    }
}

// =====================================================================
// painting (Menu_Paint / Item_Paint and the item painters)
// =====================================================================

/// The windowDef_t fields `Window_Paint` / `Border_Paint` use.
struct Win<'a> {
    rect: R,
    style: i32,
    border: i32,
    border_size: f32,
    flags: u32,
    fore: Rgba,
    back: Rgba,
    border_color: Rgba,
    background: Option<&'a str>,
    /// window.cinematicName (WINDOW_STYLE_CINEMATIC)
    cinematic: Option<&'a str>,
}

/// `Window_Paint` (the Fade of a filled shader background is the caller's,
/// `Item_Paint` / `Menu_Paint`, which own the window).
fn window_paint(p: &mut Painter, w: &Win) {
    if w.style == 0 && w.border == 0 {
        return;
    }
    let mut fr = w.rect;
    if w.border != 0 {
        fr[0] += w.border_size;
        fr[1] += w.border_size;
        fr[2] -= w.border_size + 1.0;
        fr[3] -= w.border_size + 1.0;
    }
    match w.style {
        STYLE_FILLED => match w.background {
            // box, but possible a shader that needs filled
            Some(bg) => p.pic(bg, fr, w.back),
            None => p.fill(fr, w.back),
        },
        STYLE_GRADIENT => p.gradient_bar(fr, w.back),
        STYLE_SHADER => {
            if let Some(bg) = w.background {
                p.pic(bg, fr, if w.flags & WINDOW_FORECOLORSET != 0 { w.fore } else { WHITE });
            }
        }
        STYLE_CINEMATIC => {
            // DC->playCinematic / runCinematicFrame / drawCinematic
            if let Some(c) = w.cinematic.filter(|c| !c.is_empty()) {
                p.out.push(Cmd::Cinematic { name: crate::cinematic::cinematic_path(c), r: fr });
            }
        }
        _ => {}
    }
}

/// `Border_Paint`.
fn border_paint(p: &mut Painter, w: &Win) {
    if w.style == 0 && w.border == 0 {
        return;
    }
    match w.border {
        BORDER_FULL => p.draw_rect(w.rect, w.border_size, w.border_color),
        BORDER_HORZ => p.draw_top_bottom(w.rect, w.border_size, w.border_color),
        BORDER_VERT => p.draw_sides(w.rect, w.border_size, w.border_color),
        BORDER_KCGRADIENT => {
            let mut r = w.rect;
            r[3] = w.border_size;
            p.gradient_bar(r, w.border_color);
            r[1] = w.rect[1] + w.rect[3] - 1.0;
            p.gradient_bar(r, w.border_color);
        }
        _ => {}
    }
}

impl UiItem {
    fn win(&self) -> Win<'_> {
        Win {
            rect: self.rect,
            style: self.def.style,
            border: self.def.border,
            border_size: self.def.border_size,
            flags: self.flags,
            fore: self.fore,
            back: self.back,
            border_color: self.border_color,
            background: self.background.as_deref(),
            cinematic: self.def.cinematic.as_deref(),
        }
    }
}

/// `CG_AlignText`.
fn cg_align_text(p: &Painter, r: R, text: &str, scale: f32, align: i32, valign: i32) -> (f32, f32) {
    let (w, h) = (p.width(text, scale), p.height(text, scale));
    let tx = match align {
        ALIGN_RIGHT => r[2] - w,
        ALIGN_CENTER => (r[2] - w) / 2.0,
        _ => 0.0,
    };
    let ty = match valign {
        VALIGN_TOP => h,
        VALIGN_CENTER => h + (r[3] - h) / 2.0,
        3 => 0.0,
        _ => r[3],
    };
    (r[0] + tx, r[1] + ty)
}

impl Ui {
    /// `Menu_PaintAll` (ui menus of the stack) and cgame's
    /// `CG_DrawScoreboard` (`teamscore_menu`, force painted).
    pub fn paint(&mut self) -> Vec<Cmd> {
        let fonts = self.fonts.clone();
        let mut p = Painter::new(&fonts, self.aspect, self.real_time);
        if self.show_scores {
            if let Some(mi) = self.sets[1].iter().position(|m| m.name().eq_ignore_ascii_case(SCORE_MENU)) {
                self.sets[1][mi].flags &= !WINDOW_FORCED;
                if self.scores_first {
                    self.spectator_time = self.real_time;
                    self.cg_set_score_selection(mi);
                    self.scores_first = false;
                }
                self.menu_update(1, mi);
                self.menu_paint(&mut p, 1, mi, true);
            }
        } else {
            self.scores_first = true;
        }
        // ui_hideCursor
        let hide = if self.editing.is_some() || self.waiting_for_key { "1" } else { "0" };
        self.cvars.insert("ui_hidecursor".into(), hide.into());
        self.run_capture();
        for i in 0..self.stack.len() {
            if let Some(&mi) = self.stack.get(i) {
                self.menu_paint(&mut p, 0, mi, false);
            }
        }
        p.out
    }

    /// `Menu_Paint`.
    fn menu_paint(&mut self, p: &mut Painter, set: usize, mi: usize, force: bool) {
        let m = &self.sets[set][mi];
        if m.flags & WINDOW_VISIBLE == 0 && !force {
            return;
        }
        if m.def.ownerdraw_flag != 0 && !self.owner_draw_visible(m.def.ownerdraw_flag) {
            return;
        }
        if force {
            self.sets[set][mi].flags |= WINDOW_FORCED;
        }
        let m = &self.sets[set][mi];
        if m.def.fullscreen {
            if let Some(bg) = &m.def.background {
                p.pic(bg, [0.0, 0.0, 640.0, 480.0], WHITE);
            }
        }
        let w = Win {
            rect: m.rect,
            style: m.def.style,
            border: m.def.border,
            border_size: m.def.border_size,
            flags: m.flags,
            fore: m.def.fore,
            back: m.def.back,
            border_color: m.def.border_color,
            background: m.def.background.as_deref(),
            cinematic: m.def.cinematic.as_deref(),
        };
        window_paint(p, &w);
        border_paint(p, &w);
        for ii in 0..self.sets[set][mi].items.len() {
            self.item_paint(p, Self::iref(set, mi, ii));
        }
    }

    /// `Item_Paint`.
    fn item_paint(&mut self, p: &mut Painter, r: IRef) {
        self.item_effects(r);
        let odf = self.it(r).def.ownerdraw_flag;
        if odf != 0 {
            if self.owner_draw_visible(odf) {
                self.it_mut(r).flags |= WINDOW_VISIBLE;
            } else {
                self.it_mut(r).flags &= !WINDOW_VISIBLE;
            }
        }
        if self.it(r).cvar_flags & (CVAR_SHOW | CVAR_HIDE) != 0 && !self.enable_show_via_cvar(r, CVAR_SHOW) {
            return;
        }
        if self.it(r).flags & WINDOW_VISIBLE == 0 {
            return;
        }
        if self.it(r).def.style == STYLE_FILLED && self.it(r).background.is_some() {
            self.item_fade(r, true);
        }
        window_paint(p, &self.it(r).win());
        match self.it(r).ty {
            IT_OWNERDRAW => self.owner_draw_paint(p, r),
            IT_TEXT | IT_BUTTON => self.item_text_paint(p, r, None),
            IT_CYCLE => {
                let f = self.it(r).feeder;
                let c = self.it(r).cycle_cursor;
                let t = self.feeder_item_text(f, c, 0).0;
                self.value_paint(p, r, &t);
            }
            IT_LISTBOX => self.list_box_paint(p, r),
            IT_COMBOBOX => self.combo_box_paint(p, r),
            IT_MODEL => self.item_model_paint(p, r),
            IT_YESNO => {
                let v = self.it(r).def.cvar.as_deref().map(|c| self.cvar_value(c)).unwrap_or(0.0);
                self.value_paint(p, r, if v != 0.0 { "Yes" } else { "No" });
            }
            IT_MULTI => {
                let t = self.multi_setting(r);
                self.value_paint(p, r, &t);
            }
            IT_BIND => self.bind_paint(p, r),
            IT_SLIDER => self.slider_paint(p, r),
            t if is_edit_field(t) => self.text_field_paint(p, r),
            _ => {}
        }
        border_paint(p, &self.it(r).win());
    }

    /// `Item_Model_Paint`: the refdef rect (the window inset by one pixel),
    /// fovs and the yaw `model_rotation` turns one degree per that many ms.
    fn item_model_paint(&mut self, p: &mut Painter, r: IRef) {
        let now = self.real_time;
        let it = self.it_mut(r);
        let Some(asset) = it.def.asset_model.clone() else { return };
        if it.def.model_rotation != 0 && now > it.next_time {
            it.next_time = now + it.def.model_rotation;
            it.model_angle = (it.model_angle + 1) % 360;
        }
        let rr = [it.rect[0] + 1.0, it.rect[1] + 1.0, it.rect[2] - 2.0, it.rect[3] - 2.0];
        p.out.push(Cmd::Model { asset, r: rr, fov: it.def.model_fov, angle: it.model_angle as f32, time: now });
    }

    /// The focus colour or the item's forecolor (the value painters).
    fn value_color(&self, r: IRef) -> Rgba {
        if self.it(r).flags & WINDOW_HASFOCUS != 0 {
            self.menu_of(r).def.focus_color
        } else {
            self.it(r).fore
        }
    }

    /// `Item_YesNo_Paint` / `Item_Multi_Paint` / `Item_Cycle_Paint`: the
    /// label, then the value ITEM_VALUE_OFFSET after it.
    fn value_paint(&mut self, p: &mut Painter, r: IRef, value: &str) {
        let color = self.value_color(r);
        let it = self.it(r);
        let (scale, style) = (it.def.text_scale, it.def.text_style);
        if it.def.text.is_some() {
            self.item_text_paint(p, r, None);
            let tr = self.it(r).text_rect;
            let off = if self.it(r).def.text.as_deref().is_some_and(|t| !t.is_empty()) || self.it(r).ty != IT_YESNO { ITEM_VALUE_OFFSET } else { 0.0 };
            p.text(tr[0] + tr[2] + off, tr[1], scale, color, value, 0.0, 0, style);
        } else {
            let tr = it.text_rect;
            p.text(tr[0], tr[1], scale, color, value, 0.0, 0, style);
        }
    }

    /// `Item_SetTextExtents`.
    fn set_text_extents(&mut self, r: IRef, text: &str) {
        let aspect = self.aspect;
        let it = self.it(r);
        let cvar_content = it.def.cvar.is_some() && it.def.text_align != ALIGN_LEFT && it.ty == IT_TEXT;
        if !(it.text_rect[2] == 0.0 || cvar_content || (it.ty == IT_OWNERDRAW && it.def.text_align != ALIGN_LEFT)) {
            return;
        }
        let scale = it.def.text_scale;
        let label = it.def.text.clone().unwrap_or_default();
        let mut original = 0.0;
        if it.def.text_align == ALIGN_CENTER || it.def.text_align == ALIGN_RIGHT {
            original = if cvar_content {
                let buff = self.cvar(it.def.cvar.as_deref().unwrap_or(""));
                self.fonts.width(&label, scale, aspect) + self.fonts.width(&buff, scale, aspect)
            } else {
                self.fonts.width(&label, scale, aspect)
            };
        }
        let w = self.fonts.width(text, scale, aspect);
        let h = self.fonts.height(text, scale);
        let mut t = [0.0, 0.0, w, h];
        t[1] = match it.def.text_valign {
            VALIGN_CENTER => it.def.text_align_y + (h + it.rect[3]) / 2.0,
            VALIGN_TOP => it.def.text_align_y + h,
            VALIGN_BOTTOM => it.def.text_align_y + it.rect[3],
            _ => it.text_rect[1],
        };
        t[0] = match it.def.text_align {
            ALIGN_LEFT => it.text_align_x,
            ALIGN_CENTER => it.text_align_x + (it.rect[2] - original) / 2.0,
            ALIGN_RIGHT => it.text_align_x + it.rect[2] - original,
            _ => it.text_rect[0],
        };
        // ToWindowCoords
        let bs = if it.def.border != 0 { it.def.border_size } else { 0.0 };
        t[0] += bs + it.rect[0];
        t[1] += bs + it.rect[1];
        self.it_mut(r).text_rect = t;
    }

    /// `Item_TextColor`.
    fn item_text_color(&self, r: IRef) -> Rgba {
        let it = self.it(r);
        let parent = self.menu_of(r);
        let mut c = if it.flags & WINDOW_HASFOCUS != 0 {
            parent.def.focus_color
        } else if it.def.text_style == TEXTSTYLE_BLINK && (self.real_time / BLINK_DIVISOR) & 1 == 0 {
            let low = [0.8 * it.fore[0], 0.8 * it.fore[1], 0.8 * it.fore[2], 0.8 * it.fore[3]];
            lerp_color(it.fore, low, 0.5 + 0.5 * (self.real_time as f32 / PULSE_DIVISOR).sin())
        } else {
            it.fore
        };
        if !it.enable_cvar.is_empty() && it.def.cvar_test.as_deref().is_some_and(|t| !t.is_empty()) && it.cvar_flags & (CVAR_ENABLE | CVAR_DISABLE) != 0 && !self.enable_show_via_cvar(r, CVAR_ENABLE) {
            c = parent.def.disable_color;
        }
        c
    }

    /// `Item_Text_Paint` (`text` overrides item->text, as Item_OwnerDraw_Paint does).
    fn item_text_paint(&mut self, p: &mut Painter, r: IRef, text: Option<&str>) {
        // Item_TextColor's Fade
        self.item_fade(r, false);
        if self.it(r).flags & WINDOW_WRAPPED != 0 {
            self.item_text_wrapped_paint(p, r);
            return;
        }
        let it = self.it(r);
        let t = match (text, &it.def.text, &it.def.cvar) {
            (Some(t), _, _) => t.to_string(),
            (None, Some(t), _) => t.clone(),
            (None, None, Some(c)) => self.cvar(c),
            _ => return,
        };
        // this needs to go here as it sets extents for cvar types as well
        self.set_text_extents(r, &t);
        if t.is_empty() {
            return;
        }
        let color = self.item_text_color(r);
        let it = self.it(r);
        p.text(it.text_rect[0], it.text_rect[1], it.def.text_scale, color, &t, 0.0, 0, it.def.text_style);
    }

    /// `Item_Text_Wrapped_Paint` (`UI_DrawTextBlock` without an item).
    fn item_text_wrapped_paint(&mut self, p: &mut Painter, r: IRef) {
        let it = self.it(r);
        let text = match (&it.def.text, &it.def.cvar) {
            (Some(t), _) => t.clone(),
            (None, Some(c)) => self.cvar(c),
            _ => return,
        };
        if text.is_empty() {
            return;
        }
        let color = self.item_text_color(r);
        let it = self.it(r);
        text_block(p, it.rect, it.text_align_x, it.def.text_align_y, color, it.def.text_scale, it.def.text_align, it.def.text_valign, it.def.text_style, &text, it.def.border, it.def.border_size);
    }

    /// `Item_TextField_Paint`.
    fn text_field_paint(&mut self, p: &mut Painter, r: IRef) {
        let offset = if self.it(r).def.text.as_deref().is_some_and(|t| !t.is_empty()) { ITEM_VALUE_OFFSET } else { 0.0 };
        let editing = self.it(r).flags & WINDOW_HASFOCUS != 0 && self.editing == Some(r);
        let cursor_width = if editing { EDIT_CURSOR_WIDTH } else { 0 };
        let cursor = if self.overstrike { b'|' } else { b'_' };
        self.item_text_paint(p, r, None);
        let mut buff = self.it(r).def.cvar.as_deref().map(|c| self.cvar(c)).unwrap_or_default();
        let (tr, rect) = (self.it(r).text_rect, self.it(r).rect);
        // maxFieldWidth hasn't been set, so use the item's rect
        if self.it(r).edit.max_field_width == 0 {
            let w = (rect[2] - (tr[2] + offset + (tr[0] - rect[0]))) as i32;
            self.it_mut(r).edit.max_field_width = w.max(MIN_FIELD_WIDTH);
        }
        if !editing {
            self.it_mut(r).edit.paint_offset = 0;
        }
        let e = self.it(r).edit;
        let scale = self.it(r).def.text_scale;
        let po = (e.paint_offset.max(0) as usize).min(buff.len());
        // shorten string to max viewable
        while p.width(&buff[po.min(buff.len())..], scale) > (e.max_field_width - cursor_width) as f32 && !buff.is_empty() {
            buff.pop();
        }
        let color = self.value_color(r);
        let style = self.it(r).def.text_style;
        let shown = &buff[po.min(buff.len())..];
        let limit = e.max_paint_chars.max(0) as usize;
        if editing {
            let cp = self.it(r).cursor_pos - e.paint_offset;
            p.text_generic(tr[0] + tr[2] + offset, tr[1], scale, 0.0, shown, color, style, limit, None, cp, cursor);
        } else {
            p.text(tr[0] + tr[2] + offset, tr[1], scale, color, shown, 0.0, limit, style);
        }
    }

    /// `Item_Slider_Paint`.
    fn slider_paint(&mut self, p: &mut Painter, r: IRef) {
        let color = self.value_color(r);
        let vs = self.slider_vscale(r);
        let (x, y);
        if self.it(r).def.text.is_some() {
            self.item_text_paint(p, r, None);
            let tr = self.it(r).text_rect;
            x = tr[0] + tr[2] + ITEM_VALUE_OFFSET;
            y = tr[1] - tr[3] + (tr[3] - SLIDER_HEIGHT * vs) / 2.0;
        } else {
            x = self.it(r).rect[0];
            y = self.it(r).rect[1];
        }
        p.pic(ASSET_SLIDER_BAR, [x, y, 96.0 * self.aspect, SLIDER_HEIGHT * vs], color);
        let tr = self.it(r).text_rect;
        let y = tr[1] - tr[3] + (tr[3] - SLIDER_THUMB_HEIGHT * vs) / 2.0;
        let x = self.slider_thumb_position(r);
        let tw = 12.0 * self.aspect;
        p.pic(ASSET_SLIDER_THUMB, [x - tw / 2.0, y, tw, SLIDER_THUMB_HEIGHT * vs], color);
    }

    /// `Item_Bind_Paint`: the bound keys (`BindingFromName`), "Press key"
    /// pulsing while it waits for one.
    fn bind_paint(&mut self, p: &mut Painter, r: IRef) {
        let it = self.it(r);
        let max_chars = it.edit.max_paint_chars.max(0) as usize;
        let (scale, style) = (it.def.text_scale, it.def.text_style);
        let focus = self.menu_of(r).def.focus_color;
        let color = if it.flags & WINDOW_HASFOCUS != 0 {
            if self.bind_item == Some(r) {
                let low = [0.8 * focus[0], 0.8 * focus[1], 0.8 * focus[2], 0.8 * focus[3]];
                lerp_color(focus, low, 0.5 + 0.5 * (self.real_time as f32 / PULSE_DIVISOR).sin())
            } else {
                focus
            }
        } else {
            it.fore
        };
        if it.def.text.is_some() {
            let name = if self.bind_item == Some(r) && self.waiting_for_key { "Press key".to_string() } else { self.binding_from_name(it.def.cvar.as_deref().unwrap_or("")) };
            self.item_text_paint(p, r, None);
            let tr = self.it(r).text_rect;
            p.text(tr[0] + tr[2] + ITEM_VALUE_OFFSET, tr[1], scale, color, &name, 0.0, max_chars, style);
        } else {
            let tr = it.text_rect;
            p.text(tr[0], tr[1], scale, color, "FIXME", 0.0, max_chars, style);
        }
    }

    /// `Item_ListBox_Paint`.
    fn list_box_paint(&mut self, p: &mut Painter, r: IRef) {
        let f = self.it(r).feeder;
        let count = self.feeder_count(f);
        let (rect, _) = self.list_rect(r);
        let scrollbar = !self.it(r).list.no_scrollbar && count > self.list_num_items(r);
        if scrollbar {
            let x = self.sb_slider_x(rect);
            let y = Self::sb_y(rect);
            let thumb = self.list_thumb_draw_position(r);
            let aw = self.arrow_w();
            p.pic(ASSET_SCROLLBAR_ARROWUP, [x, y, aw, SCROLLBAR_ARROW_SIZE], WHITE);
            let y = Self::sb_slider_y(rect);
            let size = Self::sb_slider_h(rect);
            p.pic(ASSET_SCROLLBAR, [x, y, aw, size], WHITE);
            p.pic(ASSET_SCROLLBAR_ARROWDOWN, [x, y + size, aw, SCROLLBAR_ARROW_SIZE], WHITE);
            p.pic(ASSET_SCROLL_THUMB, [x, thumb, aw, SCROLLBAR_ARROW_SIZE], WHITE);
        }
        let (start, end) = (self.it(r).list.start, self.it(r).list.end);
        for i in start..end {
            self.list_row_paint(p, r, rect, i, i - start, true, scrollbar);
        }
    }

    /// `Item_ListBoxRow_Paint`.
    #[allow(clippy::too_many_arguments)]
    fn list_row_paint(&mut self, p: &mut Painter, r: IRef, rect: R, row: i32, render_pos: i32, highlight: bool, scrollbar: bool) {
        let a = self.aspect;
        let (one, two) = (a, 2.0 * a);
        let it = self.it(r);
        let menu_bias = self.menu_of(r).def.aspect_bias;
        let x = self.sb_x(rect);
        let eh = it.def.element_height;
        let y = Self::sb_y(rect) + eh * render_pos as f32;
        let mut w = rect[2] - two * it.def.border_size;
        if scrollbar {
            w -= self.arrow_w();
        }
        let biased = menu_bias != ASPECT_NONE || it.def.aspect_bias != ASPECT_NONE;
        let (scale, fore, style, f) = (it.def.text_scale, it.fore, it.def.text_style, it.feeder);
        if it.def.element_type == LISTBOX_IMAGE {
            let ew = it.def.element_width;
            p.clip = Some([x, y, ew, eh]);
            if let Some(img) = self.feeder_item_image(f, row) {
                p.pic(&img, [x + one, y + 1.0, ew - two, eh - 2.0], WHITE);
            }
            if highlight && row == self.it(r).cursor_pos {
                let it = self.it(r);
                p.draw_rect([x, y, ew, eh], it.def.border_size, it.border_color);
            }
            p.clip = None;
            return;
        }
        let m = p.fonts.em_height(scale);
        let columns = it.def.columns.clone();
        if !columns.is_empty() {
            for (j, col) in columns.iter().enumerate() {
                let (pos, cw, align) = (col[0] as f32, col[1] as f32, col[3]);
                let (column_pos, width) = if biased { ((pos + 4.0) * a, cw * a) } else { (pos + 4.0, cw) };
                let height = cw;
                let y_off = y + (eh - height) / 2.0;
                let (text, img) = self.feeder_item_text(f, row, j);
                p.clip = Some([x + column_pos, y_off, width, height]);
                if let Some(img) = img {
                    p.pic(&img, [x + column_pos, y_off, width, height], WHITE);
                } else if !text.is_empty() {
                    let tw = p.width(&text, scale);
                    let off = match align {
                        ALIGN_RIGHT => width - tw,
                        ALIGN_CENTER => width / 2.0 - tw / 2.0,
                        _ => 0.0,
                    };
                    p.text(x + column_pos + off, y + m + (eh - m) / 2.0, scale, fore, &text, 0.0, 0, style);
                }
                p.clip = None;
            }
        } else {
            let offset = if biased { 4.0 * a } else { 4.0 };
            let (text, img) = self.feeder_item_text(f, row, 0);
            p.clip = Some([x, y, w, eh]);
            if let Some(img) = img {
                p.pic(&img, [x + offset, y, eh, eh], WHITE);
            } else if !text.is_empty() {
                p.text(x + offset, y + m + (eh - m) / 2.0, scale, fore, &text, 0.0, 0, style);
            }
            p.clip = None;
        }
        if highlight && row == self.it(r).cursor_pos {
            let oc = self.it(r).def.outline_color;
            p.fill([x, y, w, eh], oc);
        }
    }

    /// `Item_ComboBox_Paint`.
    fn combo_box_paint(&mut self, p: &mut Painter, r: IRef) {
        let rect = self.it(r).rect;
        let x = self.sb_slider_x(rect);
        let y = Self::sb_y(rect);
        p.pic(ASSET_SCROLLBAR_ARROWDOWN, [x, y, self.arrow_w(), rect[3] - 2.0], WHITE);
        let c = self.it(r).cursor_pos;
        self.list_row_paint(p, r, rect, c, 0, false, true);
        if self.combo == Some(r) {
            self.list_box_paint(p, r);
        }
    }

    /// `Item_OwnerDraw_Paint`.
    fn owner_draw_paint(&mut self, p: &mut Painter, r: IRef) {
        self.item_fade(r, false);
        let it = self.it(r);
        let parent = self.menu_of(r);
        let mut color = it.fore;
        if !it.def.color_ranges.is_empty() {
            // DC->getValue (`UI_GetValue`) is 0 for every ownerdraw of the ui module
            if let Some((_, _, c)) = it.def.color_ranges.iter().find(|(lo, hi, _)| 0.0 >= *lo && 0.0 <= *hi) {
                color = *c;
            }
        }
        if it.flags & WINDOW_HASFOCUS != 0 {
            color = parent.def.focus_color;
        } else if it.def.text_style == TEXTSTYLE_BLINK && (self.real_time / BLINK_DIVISOR) & 1 == 0 {
            let low = [0.8 * it.fore[0], 0.8 * it.fore[1], 0.8 * it.fore[2], 0.8 * it.fore[3]];
            color = lerp_color(it.fore, low, 0.5 + 0.5 * (self.real_time as f32 / PULSE_DIVISOR).sin());
        }
        if it.cvar_flags & (CVAR_ENABLE | CVAR_DISABLE) != 0 && !self.enable_show_via_cvar(r, CVAR_ENABLE) {
            color = parent.def.disable_color;
        }
        if let Some(text) = self.owner_draw_text(it.def.ownerdraw) {
            if it.def.text.as_deref().is_some_and(|t| !t.is_empty()) {
                self.item_text_paint(p, r, None);
                let it = self.it(r);
                let tr = it.text_rect;
                p.text(tr[0] + tr[2] + ITEM_VALUE_OFFSET, tr[1], it.def.text_scale, color, &text, 0.0, 0, it.def.text_style);
            } else {
                self.item_text_paint(p, r, Some(&text));
            }
        } else {
            self.owner_draw(p, r, color);
        }
    }

    /// `UI_OwnerDraw` / `CG_OwnerDraw` for the ownerdraws of these menus.
    fn owner_draw(&mut self, p: &mut Painter, r: IRef, color: Rgba) {
        let it = self.it(r);
        let Some(&od) = self.ods.get(&it.def.ownerdraw) else { return };
        let rect = it.rect;
        let (tx, ty, scale, align, valign, style) = (it.text_align_x, it.def.text_align_y, it.def.text_scale, it.def.text_align, it.def.text_valign, it.def.text_style);
        match od {
            Od::InfoPane(f) => {
                // UI_DrawInfoPane
                let text = if f == Feeder::HelpList {
                    let i = self.selection(f).max(0) as usize;
                    self.help.get(i).map(|h| h.1.clone()).unwrap_or_default()
                } else {
                    // (uiInfo.*List[ index ]: an empty list shows its stale first entry)
                    match self.selected(f) {
                        Some(e) => info_pane_text(e, &self.view.player),
                        None => return,
                    }
                };
                text_block(p, rect, tx, ty, color, scale, align, valign, style, &text, 0, 0.0);
            }
            Od::SelectedMapPreview => {
                // `UI_DrawSelectedMapPreview`: <map>.roq when the data has one,
                // else the levelshot
                let mut s = self.cvar_value("ui_selectedMap") as i32;
                if s < 0 || s as usize > self.map_list.len() {
                    self.set_cvar("ui_selectedMap", "0");
                    s = 0;
                }
                let Some((map, _)) = self.map_list.get(s as usize).cloned() else {
                    p.pic("gfx/2d/load_screen", rect, WHITE);
                    return;
                };
                let roq = format!("video/{map}.roq");
                let cin = *self.map_cinematic.entry(s as usize).or_insert(-1);
                if cin >= -1 && self.fs_files.iter().any(|f| f.eq_ignore_ascii_case(&roq)) {
                    self.map_cinematic.insert(s as usize, 0);
                    p.out.push(Cmd::Cinematic { name: roq, r: rect });
                } else {
                    self.map_cinematic.insert(s as usize, -2);
                    p.pic(&format!("levelshots/{map}"), rect, WHITE);
                }
            }
            Od::NetMapPreview => {
                // `UI_DrawServerMapPreview`
                match self.browser.current_server_preview.clone().filter(|_| !self.browser.display_servers.is_empty()) {
                    Some(img) => p.pic(&img, rect, WHITE),
                    None => p.pic("gfx/2d/load_screen", rect, WHITE),
                }
            }
            Od::NetMapCinematic | Od::NetSource | Od::ServerRefreshDate | Od::ServerMotd => {}
            Od::SelectedMapName => {
                let s = self.cvar_value("ui_selectedMap") as i32;
                if s >= 0 && (s as usize) < self.map_list.len() {
                    let n = self.map_list[s as usize].1.clone();
                    p.text(rect[0], rect[1], scale, color, &n, 0.0, 0, style);
                }
            }
            Od::GlInfo => {
                let t = self.gl_info.clone();
                text_block(p, rect, tx, ty, color, scale, align, valign, style, &t, 0, 0.0);
            }
            Od::KeyBindStatus => {}
            Od::AliensScoreLabel | Od::HumansScoreLabel => {
                // CG_DrawTeamLabel
                let s = &self.view.scores;
                let (t, stage) = if od == Od::AliensScoreLabel { ("Aliens", s.alien_stage) } else { ("Humans", s.human_stage) };
                let stage = if s.intermission { format!("(Stage {})", stage + 1) } else { String::new() };
                let text = if align == ALIGN_RIGHT { format!("{stage} {t}") } else { format!("{t} {stage}") };
                let (x, y) = cg_align_text(p, rect, &text, scale, align, valign);
                p.text(tx + x, ty + y, scale, color, &text, 0.0, 0, style);
            }
            Od::StageReport => {
                // CG_DrawStageReport
                let s = &self.view.scores;
                if s.intermission {
                    return;
                }
                let text = match s.my_team {
                    Team::None => return,
                    Team::Aliens => {
                        let kills = (((s.alien_next - s.alien_credits) as f32) / k::ALIEN_CREDITS_PER_KILL as f32).ceil().max(0.0) as i32;
                        if s.alien_next < 0 {
                            format!("Stage {}", s.alien_stage + 1)
                        } else if kills == 1 {
                            format!("Stage {}, 1 frag for next stage", s.alien_stage + 1)
                        } else {
                            format!("Stage {}, {kills} frags for next stage", s.alien_stage + 1)
                        }
                    }
                    Team::Humans => {
                        let credits = (s.human_next - s.human_credits).max(0);
                        if s.human_next < 0 {
                            format!("Stage {}", s.human_stage + 1)
                        } else if credits == 1 {
                            format!("Stage {}, 1 credit for next stage", s.human_stage + 1)
                        } else {
                            format!("Stage {}, {credits} credits for next stage", s.human_stage + 1)
                        }
                    }
                };
                let (x, y) = cg_align_text(p, rect, &text, scale, align, valign);
                p.text(tx + x, ty + y, scale, color, &text, 0.0, 0, style);
            }
            Od::Spectators => {
                // CG_DrawTeamSpectators
                let text = self.view.scores.spectators.clone();
                let tw = p.width(&text, scale);
                let (_, y) = cg_align_text(p, rect, &text, scale, ALIGN_LEFT, valign);
                if tw > rect[2] {
                    // the text is too wide to fit, so scroll it
                    let now = self.real_time;
                    let delta = now - self.spectator_time;
                    p.clip = Some(rect);
                    p.text(rect[0] - self.spectator_offset, y, scale, color, &text, 0.0, 0, 0);
                    p.text(rect[0] + tw - self.spectator_offset, y, scale, color, &text, 0.0, 0, 0);
                    p.clip = None;
                    self.spectator_offset += (delta as f32 / 1000.0) * SPECTATORS_PIXELS_PER_SECOND;
                    while self.spectator_offset > tw {
                        self.spectator_offset -= tw;
                    }
                    self.spectator_time = now;
                } else {
                    p.text(rect[0], y, scale, color, &text, 0.0, 0, 0);
                }
            }
        }
    }
}

/// `UI_DrawTextBlock` -> `Item_Text_Wrapped_Paint` (one VALIGN_TOP line
/// item per wrapped line).
#[allow(clippy::too_many_arguments)]
fn text_block(p: &mut Painter, rect: R, text_x: f32, text_y: f32, color: Rgba, scale: f32, align: i32, valign: i32, style: i32, text: &str, border: i32, border_size: f32) {
    let fonts = p.fonts;
    let font_height = fonts.em_height(scale);
    let line_spacing = font_height * 0.4;
    let line_height = font_height + line_spacing;
    let (x, y) = (rect[0] + text_x, rect[1] + text_y);
    let (w, h) = (rect[2] - 2.0 * text_x, rect[3] - 2.0 * text_y);
    let wrapped = item_text_wrap(fonts, text, scale, w, p.aspect);
    let total_lines = wrapped.bytes().filter(|&c| c == b'\n').count() as i32;
    let paint_lines = (((h + line_spacing) / line_height).floor() as i32).min(total_lines);
    let text_height = paint_lines as f32 * line_height - line_spacing;
    let paint_y = match valign {
        VALIGN_CENTER => y + (h - text_height) / 2.0,
        VALIGN_TOP => y,
        _ => y + (h - text_height),
    };
    let bs = if border != 0 { border_size } else { 0.0 };
    for (line_num, line) in wrapped.split('\n').take(paint_lines.max(0) as usize).enumerate() {
        if line.len() >= MAX_STRING_CHARS - 1 {
            break;
        }
        // Item_SetTextExtents for the line item (VALIGN_TOP, textalignx/y 0)
        let lr = [x, paint_y + line_num as f32 * line_height, w, line_height];
        let tw = p.width(line, scale);
        let th = p.height(line, scale);
        let tx = match align {
            ALIGN_CENTER => (lr[2] - tw) / 2.0,
            ALIGN_RIGHT => lr[2] - tw,
            _ => 0.0,
        };
        p.text(lr[0] + tx + bs, lr[1] + th + bs, scale, color, line, 0.0, 0, style);
    }
}

/// ui_shared.c `bind_t`.
#[derive(Clone, Debug)]
struct BindT {
    command: &'static str,
    default1: i32,
    bind1: i32,
    bind2: i32,
}

/// ui_shared.c `g_bindings[]`: (command, defaultbind1); defaultbind2 is -1
/// for all of them.
const G_BINDINGS: &[(&str, i32)] = {
    use crate::keys::*;
    const F1: i32 = K_F1;
    &[
        ("+scores", K_TAB),
        ("+button2", K_ENTER),
        ("+speed", K_SHIFT),
        // human dodging
        ("+button6", b'z' as i32),
        ("+button8", b'x' as i32),
        ("+forward", K_UPARROW),
        ("+back", K_DOWNARROW),
        ("+moveleft", b',' as i32),
        ("+moveright", b'.' as i32),
        ("+moveup", K_SPACE),
        ("+movedown", b'c' as i32),
        ("+left", K_LEFTARROW),
        ("+right", K_RIGHTARROW),
        ("+strafe", K_ALT),
        ("+lookup", K_PGDN),
        ("+lookdown", K_DEL),
        ("+mlook", b'/' as i32),
        ("centerview", K_END),
        ("+zoom", -1),
        ("weapon 1", b'1' as i32),
        ("weapon 2", b'2' as i32),
        ("weapon 3", b'3' as i32),
        ("weapon 4", b'4' as i32),
        ("weapon 5", b'5' as i32),
        ("weapon 6", b'6' as i32),
        ("weapon 7", b'7' as i32),
        ("weapon 8", b'8' as i32),
        ("weapon 9", b'9' as i32),
        ("weapon 10", b'0' as i32),
        ("weapon 11", -1),
        ("weapon 12", -1),
        ("weapon 13", -1),
        ("+attack", K_MOUSE1),
        // secondary attack
        ("+button5", K_MOUSE2),
        ("reload", b'r' as i32),
        ("buy ammo", b'b' as i32),
        // use medkit
        ("itemact medkit", b'm' as i32),
        // buildable use
        ("+button7", b'q' as i32),
        // buildable destroy
        ("deconstruct", b'e' as i32),
        ("weapprev", b'[' as i32),
        ("weapnext", b']' as i32),
        ("+button3", K_MOUSE3),
        ("+button4", K_MOUSE4),
        ("vote yes", F1),
        ("vote no", F1 + 1),
        ("teamvote yes", F1 + 2),
        ("teamvote no", F1 + 3),
        ("scoresUp", K_KP_PGUP),
        ("scoresDown", K_KP_PGDN),
        ("screenshotJPEG", -1),
        ("messagemode", -1),
        ("messagemode2", -1),
        // the port's microphone toggle (clientcmd.rs PORT_DEFAULT_CFG, dec052)
        ("voip_toggle", b'k' as i32),
    ]
};

/// keycodes.h number -> the K the menu code distinguishes.
fn k_from_keynum(k: i32) -> K {
    use crate::keys::*;
    match k {
        K_TAB => K::Tab,
        K_ENTER => K::Enter,
        K_KP_ENTER => K::KpEnter,
        K_ESCAPE => K::Escape,
        K_BACKSPACE => K::Backspace,
        K_UPARROW | K_KP_UPARROW => K::Up,
        K_DOWNARROW | K_KP_DOWNARROW => K::Down,
        K_LEFTARROW | K_KP_LEFTARROW => K::Left,
        K_RIGHTARROW | K_KP_RIGHTARROW => K::Right,
        K_HOME | K_KP_HOME => K::Home,
        K_END | K_KP_END => K::End,
        K_PGUP | K_KP_PGUP => K::PgUp,
        K_PGDN | K_KP_PGDN => K::PgDn,
        K_DEL | K_KP_DEL => K::Del,
        K_INS | K_KP_INS => K::Ins,
        K_MOUSE1 => K::Mouse1,
        K_MOUSE2 => K::Mouse2,
        K_MOUSE3 => K::Mouse3,
        K_MWHEELUP => K::WheelUp,
        K_MWHEELDOWN => K::WheelDown,
        _ => K::Other,
    }
}

/// The key number of a K (0 for K::Other: no key).
fn keynum_from_k(k: K) -> i32 {
    use crate::keys::*;
    match k {
        K::Char(c) => c as i32,
        K::Tab => K_TAB,
        K::Enter => K_ENTER,
        K::KpEnter => K_KP_ENTER,
        K::Escape => K_ESCAPE,
        K::Backspace => K_BACKSPACE,
        K::Up => K_UPARROW,
        K::Down => K_DOWNARROW,
        K::Left => K_LEFTARROW,
        K::Right => K_RIGHTARROW,
        K::Home => K_HOME,
        K::End => K_END,
        K::PgUp => K_PGUP,
        K::PgDn => K_PGDN,
        K::Del => K_DEL,
        K::Ins => K_INS,
        K::Mouse1 => K_MOUSE1,
        K::Mouse2 => K_MOUSE2,
        K::Mouse3 => K_MOUSE3,
        K::WheelUp => K_MWHEELUP,
        K::WheelDown => K_MWHEELDOWN,
        K::Other => 0,
    }
}

impl Ui {
    /// `DC->getBindingBuf`.
    fn get_binding_buf(&self, k: i32) -> &str {
        self.key_bindings.get(k as usize).map(|s| s.as_str()).unwrap_or("")
    }

    /// `DC->setBinding` (`trap_Key_SetBinding`): applied to the local copy
    /// and sent to the client.
    fn set_binding(&mut self, k: i32, cmd: &str) {
        if k < 0 {
            return;
        }
        if let Some(b) = self.key_bindings.get_mut(k as usize) {
            *b = cmd.to_string();
        }
        self.out.push(Out::Bind(k, cmd.to_string()));
    }

    /// `Controls_GetKeyAssignment`: the first two keys (of 256) bound to command.
    fn controls_get_key_assignment(&self, command: &str) -> [i32; 2] {
        let mut two = [-1, -1];
        let mut count = 0;
        for j in 0..crate::keys::K_LAST_KEY {
            let b = self.get_binding_buf(j);
            if b.is_empty() {
                continue;
            }
            if b.eq_ignore_ascii_case(command) {
                two[count] = j;
                count += 1;
                if count == 2 {
                    break;
                }
            }
        }
        two
    }

    /// `Controls_GetConfig`: bind1 / bind2 of every `g_bindings` command.
    pub fn controls_get_config(&mut self) {
        for i in 0..self.bindings.len() {
            let [a, b] = self.controls_get_key_assignment(self.bindings[i].command);
            self.bindings[i].bind1 = a;
            self.bindings[i].bind2 = b;
        }
    }

    /// `Controls_SetConfig`: bind every command's keys ("in_restart" after).
    fn controls_set_config(&mut self) {
        for i in 0..self.bindings.len() {
            let BindT { command, bind1, bind2, .. } = self.bindings[i].clone();
            if bind1 != -1 {
                self.set_binding(bind1, command);
                if bind2 != -1 {
                    self.set_binding(bind2, command);
                }
            }
        }
        self.exec("in_restart\n");
    }

    /// `Controls_SetDefaults`.
    fn controls_set_defaults(&mut self) {
        for b in self.bindings.iter_mut() {
            b.bind1 = b.default1;
            b.bind2 = -1;
        }
    }

    /// `BindingIDFromName`.
    fn binding_id_from_name(&self, name: &str) -> Option<usize> {
        self.bindings.iter().position(|b| b.command.eq_ignore_ascii_case(name))
    }

    /// `BindingFromName`: "KEY", "KEY or KEY2" (upper case), or "???".
    fn binding_from_name(&self, cvar: &str) -> String {
        let up = |k: i32| crate::keys::key_keynum_to_string(k).to_uppercase();
        if let Some(b) = self.bindings.iter().find(|b| b.command.eq_ignore_ascii_case(cvar)) {
            if b.bind1 != -1 {
                return if b.bind2 != -1 { format!("{} or {}", up(b.bind1), up(b.bind2)) } else { up(b.bind1) };
            }
        }
        "???".into()
    }

    /// `Display_KeyBindPending`.
    pub fn key_bind_pending(&self) -> bool {
        self.waiting_for_key
    }

    /// `Item_Bind_HandleKey`: Enter / click starts waiting for a key; the
    /// next key becomes the command's first or second key (taken off any
    /// other command), Backspace clears, Escape cancels.
    fn bind_handle_key(&mut self, r: IRef, key: K, down: bool) -> bool {
        let (cx, cy) = self.cursor;
        let key_n = self.keynum;
        if contains(self.it(r).rect, cx, cy) && !self.waiting_for_key {
            if down && matches!(key, K::Mouse1 | K::Enter) {
                self.waiting_for_key = true;
                self.bind_item = Some(r);
            }
            return true;
        }
        if !self.waiting_for_key || self.bind_item.is_none() {
            return true;
        }
        // K_CHAR_FLAG events
        if matches!(key, K::Char(_)) {
            return true;
        }
        match key {
            K::Escape => {
                self.waiting_for_key = false;
                return true;
            }
            K::Backspace => {
                let cvar = self.it(r).def.cvar.clone().unwrap_or_default();
                if let Some(id) = self.binding_id_from_name(&cvar) {
                    self.bindings[id].bind1 = -1;
                    self.bindings[id].bind2 = -1;
                }
                self.controls_set_config();
                self.waiting_for_key = false;
                self.bind_item = None;
                return true;
            }
            _ => {}
        }
        // '`' (the console key) and keys without a number are ignored
        if key_n == b'`' as i32 || key_n <= 0 || key_n == crate::keys::K_CONSOLE {
            return true;
        }
        for b in self.bindings.iter_mut() {
            if b.bind2 == key_n {
                b.bind2 = -1;
            }
            if b.bind1 == key_n {
                b.bind1 = b.bind2;
                b.bind2 = -1;
            }
        }
        let cvar = self.it(r).def.cvar.clone().unwrap_or_default();
        if let Some(id) = self.binding_id_from_name(&cvar) {
            let (b1, b2) = (self.bindings[id].bind1, self.bindings[id].bind2);
            if b1 == -1 {
                self.bindings[id].bind1 = key_n;
            } else if b1 != key_n && b2 == -1 {
                self.bindings[id].bind2 = key_n;
            } else {
                self.set_binding(b1, "");
                self.set_binding(b2, "");
                self.bindings[id].bind1 = key_n;
                self.bindings[id].bind2 = -1;
            }
        }
        self.controls_set_config();
        self.waiting_for_key = false;
        true
    }
}

// =====================================================================
// loading from the game data
// =====================================================================

/// The port's enhanced-audio page (fmodaudio.rs cvars), opened from the
/// options' Sound panel; built in, never read from (or added to) the pk3s.
pub const PORT_AUDIO_MENU_FILE: &str = "ui/port_audio.menu";
pub const PORT_AUDIO_MENU: &str = r#"// numbers, not ui/menudef.h names: not every data set defines them;
// text placed the legacy ui's way (textalignx an offset, textvalign 1 centre)
{
  menuDef
  {
    name "port_audio"
    visible 0
    fullScreen 0
    outOfBoundsClick
    rect 240 70 340 290
    focusColor 1 .75 0 1
    onESC { close port_audio }

    itemDef
    {
      name frame
      style 1
      rect 0 0 340 290
      backcolor 0 0 0 .9
      visible 1
      decoration
      border 4
      borderSize 2.0
      borderColor 0.5 0.5 0.5 1
    }
    itemDef
    {
      name title
      type 1
      text "Enhanced Audio"
      style 0
      rect 0 8 340 20
      textalign 1
      textvalign 1
      textscale .3
      forecolor 1 1 1 1
      visible 1
      decoration
    }
    itemDef
    {
      name note
      type 1
      text "Applies in the enhanced mode (F10); vanilla sound is unchanged"
      style 0
      rect 0 30 340 14
      textalign 1
      textvalign 1
      textscale .2
      forecolor .7 .7 .7 1
      visible 1
      decoration
    }
@ROWS@
    itemDef
    {
      name back
      type 1
      text "BACK"
      style 0
      rect 132 258 75 20
      textalign 1
      textvalign 1
      textscale .25
      forecolor 1 1 1 1
      visible 1
      action
      {
        play "sound/misc/menu1.wav";
        close port_audio
      }
    }
  }
}
"#;

/// The rows of the enhanced-audio page: (label, cvar, value list; None = yes / no).
const PORT_AUDIO_ROWS: &[(&str, &str, Option<&str>)] = &[
    ("Output:", "s_hrtf", Some(r#""Headphones (3D)" 1 "Speakers" 0"#)),
    ("Occlusion:", "s_occlusion", Some(r#""Auto" -1 "On" 1 "Off" 0"#)),
    ("Sound Pathing:", "s_pathing", None),
    ("Reflections:", "s_reverb", None),
    ("Air Absorption:", "s_airAbsorption", None),
    ("Wall Thickness:", "s_wallThickness", None),
    ("HD Sounds:", "s_hdSounds", None),
    ("Doppler:", "s_doppler", None),
];

/// PORT_AUDIO_MENU with its rows.
fn port_audio_menu() -> String {
    port_page_rows(PORT_AUDIO_MENU, PORT_AUDIO_ROWS.iter().map(|&(l, c, v)| (l, c, v.map(|v| format!("cvarFloatList {{ {v} }}")))))
}

/// The port's voice-chat page (dec053), opened from the options' Sound
/// panel: built like the enhanced-audio page, with the capture devices
/// AudioServer lists in `devices` ("Default" first).
pub const PORT_VOICE_MENU_FILE: &str = "ui/port_voice.menu";

/// The rows of the voice-chat page: (label, cvar, cvarFloatList; None = yes / no);
/// the microphone row (`s_voipInputDevice`, a cvarStrList) is second.
const PORT_VOICE_ROWS: &[(&str, &str, Option<&str>)] = &[
    ("Voice Chat:", "cl_voip", None),
    ("Mic Volume:", "cl_voipCaptureMult", Some(r#""Quiet" 1 "Normal" 2 "Loud" 3 "Very Loud" 4"#)),
    ("Sound While Talking:", "cl_voipGainDuringCapture", Some(r#""Muted" 0 "20%" 0.2 "50%" 0.5 "Full" 1"#)),
    ("Voice Activation:", "cl_voipUseVAD", None),
    ("Voice HUD:", "cl_voipHud", None),
];

/// The microphone row's (label, value) pairs: the label <= 24 characters
/// (the value column), the value the full name (`"` read as `'`).
fn voice_device_pairs(devices: &[String]) -> Vec<(String, String)> {
    devices.iter().map(|d| d.replace('"', "'")).map(|d| (d.chars().take(24).collect(), d)).collect()
}

/// The voice page's Refresh button, left of the microphone row (whose
/// rect starts after it): `uiScript voipRefreshDevices`.
const PORT_VOICE_REFRESH: &str = r#"    itemDef
    {
      name refreshmic
      type 1
      text "Refresh"
      style 0
      rect 20 77 70 20
      textalign 1
      textvalign 1
      textscale .22
      forecolor 1 1 1 1
      visible 1
      action
      {
        play "sound/misc/menu1.wav";
        uiScript voipRefreshDevices
      }
    }
"#;

fn port_voice_menu(devices: &[String]) -> String {
    let page = PORT_AUDIO_MENU
        .replace("port_audio", "port_voice")
        .replace("\"Enhanced Audio\"", "\"Voice Chat\"")
        .replace("\"Applies in the enhanced mode (F10); vanilla sound is unchanged\"", "\"Toggle the microphone in game with the Voice Chat Mic key (K)\"");
    let list: String = voice_device_pairs(devices).iter().map(|(l, v)| format!("\"{l}\", \"{v}\", ")).collect();
    let mut rows: Vec<(&str, &str, Option<String>)> = PORT_VOICE_ROWS.iter().map(|&(l, c, v)| (l, c, v.map(|v| format!("cvarFloatList {{ {v} }}")))).collect();
    rows.insert(1, ("Microphone:", "s_voipInputDevice", Some(format!("cvarStrList {{ {} }}", list.trim_end().trim_end_matches(',')))));
    // the microphone row starts right of the Refresh button; its label
    // still ends at x + w + textalignx = 170 like the other rows
    port_page_rows(&page, rows)
        .replacen("rect 20 77 300 20", "rect 100 77 220 20", 1)
        .replacen("    itemDef\n    {\n      name back", &format!("{PORT_VOICE_REFRESH}    itemDef\n    {{\n      name back"), 1)
}

/// A port page's template with its rows (`@ROWS@`): multi items for a
/// value list ("cvarFloatList { .. }" / "cvarStrList { .. }"), else yes / no.
fn port_page_rows<'a, I: IntoIterator<Item = (&'a str, &'a str, Option<String>)>>(page: &str, list: I) -> String {
    let mut rows = String::new();
    for (i, (label, cvar, list)) in list.into_iter().enumerate() {
        let y = 52 + i * 25;
        // the legacy menudef.h numbers the menus are read with (1.1.0's
        // menudef.h has YESNO 11 / MULTI 12: those are an ownerdraw and a
        // numeric field here)
        let kind = match list {
            Some(l) => format!("type {IT_MULTI}\n      cvar \"{cvar}\"\n      {l}"),
            None => format!("type {IT_YESNO}\n      cvar \"{cvar}\""),
        };
        rows += &format!(
            "    itemDef\n    {{\n      name row{i}\n      {kind}\n      text \"{label}\"\n      rect 20 {y} 300 20\n      textalign 2\n      textalignx -150\n      textvalign 1\n      textscale .25\n      forecolor 1 1 1 1\n      visible 1\n      action\n      {{\n        play \"sound/misc/menu1.wav\"\n      }}\n    }}\n"
        );
    }
    page.replace("@ROWS@", &rows)
}

/// The options' Controls / Misc panel: a "Voice Chat Mic:" bind row for
/// the port's `voip_toggle` (dec052) on the row after Screenshot, a copy
/// of that item. A menu without the Screenshot row is left as it is.
fn patch_voip_bind(text: &str) -> String {
    let Some(k) = text.find("\"screenshotJPEG\"") else { return text.to_string() };
    let Some(start) = text[..k].rfind("itemDef") else { return text.to_string() };
    let Some(open) = text[start..].find('{').map(|o| start + o) else { return text.to_string() };
    let mut depth = 0;
    let mut end = None;
    for (i, ch) in text[open..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    end = Some(open + i + 1);
                    break;
                }
            }
            _ => {}
        }
    }
    let Some(end) = end else { return text.to_string() };
    let item = &text[start..end];
    // the row: rect SCONTENT_X (SCONTENT_Y+(<n>*ELEM_H)) ...
    let Some(r) = item.find("(SCONTENT_Y+(") else { return text.to_string() };
    let num_at = r + "(SCONTENT_Y+(".len();
    let num_len = item[num_at..].find(|c: char| !c.is_ascii_digit()).unwrap_or(0);
    let Ok(row) = item[num_at..num_at + num_len].parse::<i32>() else { return text.to_string() };
    let copy = format!("{}{}{}", &item[..num_at], row + 1, &item[num_at + num_len..])
        .replacen("\"screenshotJPEG\"", "\"voip_toggle\"", 1)
        .replacen("\"Screenshot:\"", "\"Voice Chat Mic:\"", 1);
    format!("{}\n\n    {}{}", &text[..end], copy, &text[end..])
}

/// The byte range of the `itemDef { .. }` holding `needle`.
fn item_range(text: &str, needle: &str) -> Option<(usize, usize)> {
    let k = text.find(needle)?;
    let start = text[..k].rfind("itemDef")?;
    let open = text[start..].find('{').map(|o| start + o)?;
    let mut depth = 0;
    for (i, ch) in text[open..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some((start, open + i + 1));
                }
            }
            _ => {}
        }
    }
    None
}

/// A panel row (the item of `cvar`) as a button labelled `label` that
/// opens `menu`: its rect, alignment and group stay, so it sits in the
/// panel like the other rows; `row_delta` moves it that many rows down
/// (rect "SCONTENT_X (SCONTENT_Y+(<n>*ELEM_H)) ..." of the legacy menus).
fn row_button(item: &str, cvar: &str, label: &str, menu: &str, row_delta: i32) -> Option<String> {
    let mut b = item.to_string();
    if row_delta != 0 {
        let r = b.find("(SCONTENT_Y+(")?;
        let at = r + "(SCONTENT_Y+(".len();
        let len = b[at..].find(|c: char| !c.is_ascii_digit())?;
        let row: i32 = b[at..at + len].parse().ok()?;
        b.replace_range(at..at + len, &(row + row_delta).to_string());
    }
    // the type: a button (ITEM_TYPE_BUTTON, 2 in the legacy menudef.h)
    let ty = b.find("type ")?;
    let ty_end = b[ty..].find('\n').map(|n| ty + n)?;
    b.replace_range(ty..ty_end, "type ITEM_TYPE_BUTTON");
    // no cvar line, the label, and the page to open
    let c = b.find(&format!("cvar \"{cvar}\""))?;
    let line_start = b[..c].rfind('\n').map(|n| n + 1).unwrap_or(c);
    let line_end = b[c..].find('\n').map(|n| c + n + 1).unwrap_or(b.len());
    b.replace_range(line_start..line_end, "");
    let tx = b.find("text \"")?;
    let tx_end = b[tx + 6..].find('"').map(|n| tx + 6 + n + 1)?;
    b.replace_range(tx..tx_end, &format!("text \"{label}\""));
    match b.find("play \"sound/misc/menu1.wav\";") {
        Some(pl) => b.insert_str(pl + "play \"sound/misc/menu1.wav\";".len(), &format!("\n        open {menu}")),
        None => {
            let close = b.rfind('}')?;
            b.insert_str(close, &format!("  action\n      {{\n        play \"sound/misc/menu1.wav\";\n        open {menu}\n      }}\n    "));
        }
    }
    Some(b)
}

/// Create Server (ui/createserver.menu): every setting gets a short help text on hover, in
/// the free strip between the two arrows: what it does in plain words, then
/// its official (console) name. Gameplay values are not touched. A menu
/// that lacks an item is left as it is for that item.
fn patch_createserver(text: &str) -> String {
    // (anchor in the item, up to three plain lines, official name)
    const HELP: &[(&str, &[&str], &str)] = &[
        ("cvar \"sv_hostname\"", &["The name players see for your server in the list."], "sv_hostname"),
        ("feeder FEEDER_", &["The map the server starts on.", "Pick one in the list, then press Start server."], "map"),
        ("cvar \"timelimit\"", &["Minutes until the match ends and the next map loads.", "0 = no time limit."], "timelimit"),
        ("cvar \"sv_maxclients\"", &["How many players can be connected at once."], "sv_maxclients"),
        ("cvar \"g_needpassword\"", &["Yes = players must know the password to join.", "Type the password in the field below."], "g_needpassword"),
        ("cvar \"g_password\"", &["The password players type to join, when it is required."], "g_password"),
        ("cvar \"sv_pure\"", &["Yes = only players with the same game files can join.", "Keeps out modified game files (pure server)."], "sv_pure"),
        (
            "cvar \"ui_dedicated\"",
            &["No = you play on this PC while you host.", "LAN / Internet = server only (dedicated), no game window.", "Internet is also listed online. Players use UDP port 30720."],
            "ui_dedicated",
        ),
        ("cvar \"sv_allowdownload\"", &["Yes = players missing a map or file can download it", "from your server (over the same UDP port 30720)."], "sv_allowdownload"),
        ("cvar \"g_allowvote\"", &["Yes = players can call votes (kick a player, change the map)."], "g_allowvote"),
        ("cvar \"sv_minping\"", &["Players with a ping below this are refused.", "0 = no minimum. Ping is in milliseconds (ms)."], "sv_minping"),
        ("cvar \"sv_maxping\"", &["Players with a ping above this are refused.", "0 = no limit. Ping is in milliseconds (ms)."], "sv_maxping"),
        (
            "cvar \"g_synchronousclients\"",
            &["Yes = server and players update in lock step (demo recording).", "Leave on No for normal play."],
            "g_synchronousclients",
        ),
        ("cvar \"sv_maxrate\"", &["The most data per second the server sends to one player.", "In bytes per second; 0 = no limit."], "sv_maxrate"),
        ("cvar \"sv_zombietime\"", &["Seconds the server waits after a player leaves", "before it frees the slot (zombie time)."], "sv_zombietime"),
        ("cvar \"sv_reconnectlimit\"", &["The shortest wait, in seconds, between connection", "attempts from the same player (reconnect limit)."], "sv_reconnectlimit"),
    ];
    let mut t = text.to_string();
    let mut helpers = String::new();
    for (n, (anchor, lines, cvar)) in HELP.iter().enumerate() {
        let Some(k) = t.find(anchor) else { continue };
        let eol = t[k..].find('\n').map_or(t.len(), |o| k + o);
        let names: Vec<String> = (0..4).map(|i| format!("help{n}_{i}")).collect();
        let shown = |verb: &str| names.iter().map(|x| format!("        {verb} {x}")).collect::<Vec<_>>().join(";\n");
        t.insert_str(eol, &format!("\n      mouseEnter\n      {{\n{}\n      }}\n      mouseExit\n      {{\n{}\n      }}", shown("show"), shown("hide")));
        for (i, name) in names.iter().enumerate() {
            let (s, color) = match lines.get(i) {
                Some(l) if i < 3 => (l.to_string(), "1 1 1 1"),
                _ if i == 3 => (format!("Official name: {cvar}"), ".7 .7 .7 1"),
                _ => continue,
            };
            let y = 422 + 14 * i;
            helpers.push_str(&format!(
                "    itemDef\n    {{\n      name {name}\n      rect 80 {y} 420 14\n      type ITEM_TYPE_TEXT\n      decoration\n      visible MENU_FALSE\n      text \"{s}\"\n      textalign ALIGN_CENTER\n      textvalign VALIGN_CENTER\n      textalignx 0\n      textscale .27\n      forecolor {color}\n    }}\n\n"
            ));
        }
    }
    if !helpers.is_empty() {
        if let Some((s, _)) = item_range(&t, "name back\n") {
            t.insert_str(s, &helpers);
        }
    }
    t
}

/// The options' Sound panel: a "Voice Chat..." button to the port's
/// voice-chat page on the row after Doppler Sound. A menu without that
/// row is left as it is.
fn patch_voice_button(text: &str) -> String {
    let Some((start, end)) = item_range(text, "\"s_doppler\"") else { return text.to_string() };
    let Some(b) = row_button(&text[start..end], "s_doppler", "Voice Chat...", "port_voice", 1) else { return text.to_string() };
    format!("{}\n\n    {}{}", &text[..end], b, &text[end..])
}

/// The options' Sound panel: its OpenAL switch (meaningless in the port)
/// becomes the button to the enhanced-audio page, on the same row.
fn patch_sound_options(text: &str) -> String {
    let Some((start, end)) = item_range(text, "\"s_useOpenAL\"") else { return text.to_string() };
    let Some(b) = row_button(&text[start..end], "s_useOpenAL", "Enhanced Audio...", "port_audio", 0) else { return text.to_string() };
    format!("{}{}{}", &text[..start], b, &text[end..])
}

/// `UI_Init`'s loading: menus (ui + cgame scoreboard), fonts, help,
/// arenas, weapon icons.
fn load_ui(lib: &mut Library) -> Result<Ui, String> {
    // the port's enhanced-audio page and its button in the options; if
    // they fail to load with this data set, the original menus alone
    let mut files = menu_list_files(&mut |p: &str| lib.text(p))?;
    let mut with_port = files.clone();
    with_port.push(PORT_AUDIO_MENU_FILE.to_string());
    with_port.push(PORT_VOICE_MENU_FILE.to_string());
    let devices = crate::voipsnd::VoipSound::input_devices();
    let port = load_menu_files(&with_port, &mut |p: &str| match p {
        PORT_AUDIO_MENU_FILE => Some(port_audio_menu()),
        PORT_VOICE_MENU_FILE => Some(port_voice_menu(&devices)),
        "ui/createserver.menu" => lib.text(p).map(|t| patch_createserver(&t)),
        "ui/ingame_options.menu" => lib.text(p).map(|t| patch_voice_button(&patch_voip_bind(&patch_sound_options(&t)))),
        _ => lib.text(p),
    });
    let (ui, defines) = match port {
        Ok(r) => {
            files = with_port;
            r
        }
        Err(e) => {
            godot_warn!("TremMenus: the enhanced-audio page did not load ({e}); the options keep their original Sound panel");
            load_menu_files(&files, &mut |p: &str| lib.text(p))?
        }
    };
    let _ = &files;
    let mut read = |p: &str| lib.text(p);
    let cg_files: Vec<String> = CG_MENU_FILES.iter().map(|s| s.to_string()).collect();
    let (cg, _) = load_menu_files(&cg_files, &mut read)?;
    let help = read("ui/help.txt").map(|t| parse_help(&t)).unwrap_or_default();
    let mut font = |size: i32| -> Result<QFont, String> {
        let d = lib.pk3.read(&format!("fonts/fontImage_{size}.dat")).ok_or_else(|| format!("fonts/fontImage_{size}.dat missing"))?;
        parse_font_dat(d.as_slice()).ok_or_else(|| format!("fonts/fontImage_{size}.dat: bad font"))
    };
    let fonts = Fonts::new(font(20)?, font(26)?, font(34)?);
    let mut u = Ui::new(fonts, ui, cg, &defines);
    u.help = help;
    // AssetCache / Asset_Parse: the fades every Menu_Init starts from
    let mut read = |p: &str| lib.text(p);
    if let Some(f) = files.iter().find_map(|f| read(f).and_then(|t| scan_asset_fades(&t, &mut read))) {
        u.asset_fade = f;
    }
    u.apply_asset_fades();
    let arena_files: Vec<String> = lib.pk3.files().filter(|f| f.to_ascii_lowercase().starts_with("scripts/") && f.to_ascii_lowercase().ends_with(".arena")).cloned().collect();
    for f in arena_files {
        if let Some(t) = lib.text(&f) {
            u.arenas.extend(parse_arenas(&t));
        }
    }
    let mut weapon_icons = vec![None; WeaponId::COUNT + 1];
    for wid in WeaponId::ALL {
        let Some(cfg) = lib.text(&format!("models/weapons/{}/weapon.cfg", wid.id())) else { continue };
        let mut depth = 0;
        for line in cfg.lines() {
            let line = line.split("//").next().unwrap_or("").trim();
            let t: Vec<&str> = line.split_whitespace().collect();
            if depth == 0 && t.len() >= 2 && t[0].eq_ignore_ascii_case("icon") {
                weapon_icons[wid as usize] = Some(t[1].to_string());
            }
            depth += line.matches('{').count() as i32 - line.matches('}').count() as i32;
        }
    }
    u.weapon_icons = weapon_icons;
    Ok(u)
}

// =====================================================================
// Godot node
// =====================================================================

enum Op {
    Pic { tex: Gd<Texture2D>, r: R, uv: [f32; 4], c: Rgba },
    Fill { r: R, c: Rgba },
    /// a premultiplied-alpha image (an ITEM_TYPE_MODEL view: its additive
    /// stages add to what is under them, as on the C's framebuffer)
    Premul { tex: Gd<Texture2D>, r: R },
}

/// `CIN_DrawCinematic`'s upload: the frame into its texture when dirty.
fn upload_frame(q: &mut Roq, tex: &mut Option<Gd<ImageTexture>>) -> Option<Gd<Texture2D>> {
    if let Some((w, h, px, dirty)) = q.frame() {
        if dirty || tex.is_none() {
            let img = Image::create_from_data(w as i32, h as i32, false, godot::classes::image::Format::RGBA8, &PackedByteArray::from(px));
            if let Some(img) = img {
                match tex.as_mut() {
                    Some(t) if t.get_width() == w as i32 && t.get_height() == h as i32 => t.update(&img),
                    _ => *tex = ImageTexture::create_from_image(&img),
                }
            }
        }
    }
    tex.clone().map(|t| t.upcast::<Texture2D>())
}

/// `user://<dir>/*` as "dir/name" (the home path of FS_GetFileList).
fn user_files(dirs: &[&str]) -> Vec<String> {
    let mut out = Vec::new();
    for d in dirs {
        let path = godot::classes::ProjectSettings::singleton().globalize_path(&format!("user://{d}")).to_string();
        if let Ok(rd) = std::fs::read_dir(&path) {
            for e in rd.flatten() {
                if e.path().is_file() {
                    out.push(format!("{d}/{}", e.file_name().to_string_lossy()));
                }
            }
        }
    }
    out
}

/// `FS_GetModList` ($modlist): the folders next to the data folders
/// (user://base, <exe>/base, res://base) other than base that hold pk3s,
/// with their description.txt.
fn mod_dirs() -> Vec<(String, String)> {
    let mut roots: Vec<std::path::PathBuf> = Vec::new();
    for p in crate::game::find_pk3s() {
        if let Some(r) = std::path::Path::new(&p).parent().and_then(|b| b.parent()) {
            if !roots.contains(&r.to_path_buf()) {
                roots.push(r.to_path_buf());
            }
        }
    }
    let mut out: Vec<(String, String)> = Vec::new();
    for r in roots {
        let Ok(rd) = std::fs::read_dir(&r) else { continue };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if !e.path().is_dir() || name.eq_ignore_ascii_case("base") || out.iter().any(|(n, _)| n == &name) {
                continue;
            }
            let has_pk3 = std::fs::read_dir(e.path()).map(|d| d.flatten().any(|f| f.file_name().to_string_lossy().to_ascii_lowercase().ends_with(".pk3"))).unwrap_or(false);
            if has_pk3 {
                let desc = std::fs::read_to_string(e.path().join("description.txt")).map(|t| t.lines().next().unwrap_or("").trim().to_string()).unwrap_or_default();
                out.push((name, desc));
            }
        }
    }
    out
}

/// The favorites of `trap_LAN_LoadCachedServers` (user://favorites.txt: one
/// "address<TAB>name" per line; the port's stand-in for servercache.dat).
fn load_favorites() -> Vec<ServerInfo> {
    let path = godot::classes::ProjectSettings::singleton().globalize_path("user://favorites.txt").to_string();
    std::fs::read_to_string(path)
        .map(|t| {
            t.lines()
                .filter_map(|l| {
                    let (a, n) = l.split_once('\t')?;
                    Some(ServerInfo { addr: a.to_string(), host_name: n.to_string(), visible: true, ..Default::default() })
                })
                .take(MAX_OTHER_SERVERS)
                .collect()
        })
        .unwrap_or_default()
}

/// `trap_LAN_SaveCachedServers` (the favorites).
fn save_favorites(f: &[ServerInfo]) {
    let path = godot::classes::ProjectSettings::singleton().globalize_path("user://favorites.txt").to_string();
    let text: String = f.iter().map(|s| format!("{}\t{}\n", s.addr, s.host_name)).collect();
    if std::fs::write(&path, text).is_err() {
        godot_warn!("TremMenus: couldn't write {path}");
    }
}

/// The local player (TremGame's client), else the viewed one.
fn local_client(w: &World, me: Option<usize>, view: Option<usize>) -> Option<usize> {
    me.filter(|&m| w.clients[m].connected).or(view)
}

/// The legacy ui menus drawn over the whole viewport in 640x480 virtual
/// coordinates. `open(kind)` returns false when the original data is not
/// loaded (hud.gd then shows its own menus).
#[derive(GodotClass)]
#[class(base=Control)]
pub struct TremMenus {
    base: Base<Control>,
    /// Path to the TremGame node.
    #[export]
    game_path: NodePath,
    /// Pop up the team dialog for refusals (the cg_disable*Dialogs cvars at 0).
    #[export]
    auto_dialogs: bool,
    /// Screenshots/tests only: when >= 0, the credits the feeders see
    /// (the evolve list without earning frags; no cheats in this port).
    #[export]
    test_credits: i32,
    /// `cinematic splash.RoQ` before the first main menu (Com_Init without
    /// startup commands).
    #[export]
    play_intro: bool,
    ui: Option<Ui>,
    tried_load: bool,
    tex: HashMap<String, Option<Gd<Texture2D>>>,
    ops: Vec<Op>,
    player: Option<Gd<AudioStreamPlayer>>,
    last_msg: Option<Option<(i32, String)>>,
    cursor_shape: Option<(i32, i32, bool)>,
    was_open: bool,
    /// ITEM_TYPE_MODEL scenes by asset
    models: HashMap<String, MenuModel>,
    shaders: ShaderScripts,
    /// ui cinematics (CIN_loop | CIN_silent) by file: None = could not play
    cins: HashMap<String, Option<(Roq, Option<Gd<ImageTexture>>, bool)>>,
    /// CL_handle: the full-screen `cinematic` (clc.state CA_CINEMATIC)
    system_cin: Option<(Roq, Option<Gd<ImageTexture>>)>,
    cin_audio: Option<Gd<AudioStreamPlayer>>,
    /// the background track (S_StartBackgroundTrack) and its file
    music: Option<Gd<AudioStreamPlayer>>,
    music_track: String,
    vidsnd: VidSnd,
    /// the client cvars mirrored into the ui every frame
    synced: Vec<String>,
    /// clc.state was CA_ACTIVE last frame
    connected: Option<bool>,
    intro_started: bool,
    /// the canvas items the ops are drawn into, in order (a premultiplied
    /// model view needs its own blend mode)
    layers: Vec<Rid>,
    premul: Option<Gd<godot::classes::CanvasItemMaterial>>,
}

#[godot_api]
impl IControl for TremMenus {
    fn init(base: Base<Control>) -> Self {
        TremMenus {
            base,
            game_path: NodePath::from("../../Game"),
            auto_dialogs: true,
            test_credits: -1,
            play_intro: true,
            ui: None,
            tried_load: false,
            tex: HashMap::new(),
            ops: Vec::new(),
            player: None,
            last_msg: None,
            cursor_shape: None,
            was_open: false,
            models: HashMap::new(),
            shaders: ShaderScripts::default(),
            cins: HashMap::new(),
            system_cin: None,
            cin_audio: None,
            music: None,
            music_track: String::new(),
            vidsnd: VidSnd::default(),
            synced: Vec::new(),
            connected: None,
            intro_started: false,
            layers: Vec::new(),
            premul: None,
        }
    }

    fn ready(&mut self) {
        {
            let mut b = self.base_mut();
            b.set_anchors_preset(LayoutPreset::FULL_RECT);
            b.set_mouse_filter(MouseFilter::IGNORE);
            // above hud.gd's labels, which are added to the same CanvasLayer later
            b.set_z_index(1);
        }
        let mut p = AudioStreamPlayer::new_alloc();
        p.set_name("MenuSound");
        self.base_mut().add_child(&p);
        self.player = Some(p);
        // S_StartBackgroundTrack's stream (the Music bus: s_musicvolume)
        let mut m = AudioStreamPlayer::new_alloc();
        m.set_name("MenuMusic");
        m.set_bus("Music");
        self.base_mut().add_child(&m);
        self.music = Some(m);
        // S_RawSamples of the cinematics (22050 Hz stereo)
        let mut c = AudioStreamPlayer::new_alloc();
        c.set_name("CinematicSound");
        let mut g = AudioStreamGenerator::new_gd();
        g.set_mix_rate(cinematic::ROQ_SAMPLE_RATE as f32);
        g.set_buffer_length(2.0);
        c.set_stream(&g);
        self.base_mut().add_child(&c);
        self.cin_audio = Some(c);
    }

    fn process(&mut self, _delta: f64) {
        self.ops.clear();
        self.ensure_loaded();
        if self.ui.is_none() {
            return;
        }
        self.engine_cvars();
        self.wtb_joy_keys(); // wt-b: joystick keys for the ui key catcher
        self.frame();
        self.watch_messages();
        self.connection_state();
        let cmds = {
            let ui = self.ui.as_mut().unwrap();
            if ui.is_open() || ui.show_scores {
                ui.paint()
            } else {
                Vec::new()
            }
        };
        self.flush();
        for m in self.models.values_mut() {
            m.used = false;
        }
        for c in self.cins.values_mut().flatten() {
            c.2 = false;
        }
        self.resolve(cmds);
        for m in self.models.values_mut() {
            if !m.used {
                m.idle();
            }
        }
        // a ui cinematic not drawn this frame is closed
        self.cins.retain(|_, c| c.as_ref().is_none_or(|c| c.2));
        self.system_cinematic();
        self.update_cursor_image();
        self.submit();
    }

    fn exit_tree(&mut self) {
        let mut rs = RenderingServer::singleton();
        for l in self.layers.drain(..) {
            rs.free_rid(l);
        }
    }

    fn input(&mut self, event: Gd<InputEvent>) {
        if self.system_cin.is_some() {
            // CL_KeyDownEvent: during a cinematic a key (< 128) or mouse1
            // is an escape: CL_Disconnect_f stops it, then UIMENU_MAIN
            let k = if let Ok(kev) = event.clone().try_cast::<InputEventKey>() {
                let code = if kev.get_keycode() != Key::NONE { kev.get_keycode() } else { kev.get_physical_keycode() };
                (kev.is_pressed() && !kev.is_echo()).then(|| crate::keys::keynum_from_godot_key(code))
            } else if let Ok(b) = event.clone().try_cast::<InputEventMouseButton>() {
                b.is_pressed().then(|| crate::keys::keynum_from_godot_mouse(b.get_button_index()))
            } else {
                None
            };
            if let Some(k) = k {
                if (0..128).contains(&k) || k == crate::keys::keynum_from_godot_mouse(godot::global::MouseButton::LEFT) {
                    self.stop_system_cinematic();
                }
                if let Some(mut vp) = self.base().get_viewport() {
                    vp.set_input_as_handled();
                }
            }
            return;
        }
        if !self.ui.as_ref().is_some_and(|u| u.is_open()) {
            return;
        }
        let size = self.base().get_size();
        let gp = self.base().get_global_position();
        if size.x <= 0.0 || size.y <= 0.0 {
            return;
        }
        let virt = |p: Vector2| ((p.x - gp.x) * 640.0 / size.x, (p.y - gp.y) * 480.0 / size.y);
        let mut handled = false;
        if let Ok(m) = event.clone().try_cast::<InputEventMouseMotion>() {
            let (x, y) = virt(m.get_position());
            self.ui.as_mut().unwrap().mouse_move(x, y);
        } else if let Ok(b) = event.clone().try_cast::<InputEventMouseButton>() {
            let (x, y) = virt(b.get_position());
            let ui = self.ui.as_mut().unwrap();
            ui.mouse_move(x, y);
            let key = crate::keys::keynum_from_godot_mouse(b.get_button_index());
            if key > 0 {
                ui.key_event_num(key, b.is_pressed());
            }
            handled = true;
        } else if let Ok(kev) = event.try_cast::<InputEventKey>() {
            let down = kev.is_pressed();
            let code = if kev.get_keycode() != Key::NONE { kev.get_keycode() } else { kev.get_physical_keycode() };
            let keynum = crate::keys::keynum_from_godot_key(code);
            let key = map_key(code);
            let ui = self.ui.as_mut().unwrap();
            // the console key never reaches the ui (CL_KeyDownEvent)
            if keynum != crate::keys::K_CONSOLE {
                ui.key_event_num(keynum, down);
            }
            if down && ui.is_open() {
                // the K_CHAR_FLAG character event
                let u = kev.get_unicode();
                if key == K::Backspace {
                    ui.key_event(K::Char(8), true);
                } else if (32..256).contains(&u) && u != 127 {
                    ui.key_event(K::Char(u as u8), true);
                }
            }
            handled = true;
        }
        self.flush();
        if handled {
            if let Some(mut vp) = self.base().get_viewport() {
                vp.set_input_as_handled();
            }
        }
    }
}

/// Godot key -> keycodes.h K_*.
fn map_key(k: Key) -> K {
    match k {
        Key::TAB => K::Tab,
        Key::ENTER => K::Enter,
        Key::KP_ENTER => K::KpEnter,
        Key::ESCAPE => K::Escape,
        Key::BACKSPACE => K::Backspace,
        Key::UP | Key::KP_8 => K::Up,
        Key::DOWN | Key::KP_2 => K::Down,
        Key::LEFT | Key::KP_4 => K::Left,
        Key::RIGHT | Key::KP_6 => K::Right,
        Key::HOME | Key::KP_7 => K::Home,
        Key::END | Key::KP_1 => K::End,
        Key::PAGEUP | Key::KP_9 => K::PgUp,
        Key::PAGEDOWN | Key::KP_3 => K::PgDn,
        Key::DELETE | Key::KP_PERIOD => K::Del,
        Key::INSERT | Key::KP_0 => K::Ins,
        _ => K::Other,
    }
}

#[godot_api]
impl TremMenus {
    /// An interactive menu closed (Esc, Cancel, OK, ...): hud.gd recaptures the mouse.
    #[signal]
    fn menu_closed();
    /// A menu opened on its own (a refusal dialog, a G_TriggerMenu menu):
    /// hud.gd releases the mouse.
    #[signal]
    fn menu_opened(kind: GString);
    /// An engine command a menu ran: "quit" or "disconnect".
    #[signal]
    fn ui_command(cmd: GString);

    /// Open the original menu for kind = ingame | team | spawn | evolve |
    /// armoury | build | say | say_team | scores | dialog. False when the
    /// original data is not loaded (the caller then shows its own menu).
    /// True with `is_open()` false when the original would not open one (no
    /// evolution available, not a builder).
    #[func]
    fn open(&mut self, kind: GString) -> bool {
        if !self.ensure_loaded() {
            return false;
        }
        self.frame();
        let kind = kind.to_string();
        let ui = self.ui.as_mut().unwrap();
        let p = ui.view.player.clone();
        match kind.as_str() {
            // UI_SetActiveMenu( UIMENU_MAIN ), after the intro cinematic the
            // first time (`cinematic splash.RoQ`)
            "main" => {
                if self.play_intro && !self.intro_started {
                    self.intro_started = true;
                    if self.play_system_cinematic("splash.RoQ", "") {
                        self.flush();
                        return true;
                    }
                }
                self.intro_started = true;
                self.ui.as_mut().unwrap().set_active_menu(UiMenuCommand::Main);
            }
            "scores" => ui.show_scores = true,
            // UI_SetActiveMenu( UIMENU_INGAME )
            "ingame" => ui.set_active_menu(UiMenuCommand::Ingame),
            // messagemode / messagemode2
            "say" => ui.message_mode(false),
            "say_team" => ui.message_mode(true),
            // CG_Menu( MN_TEAM / MN_A_CLASS / MN_H_SPAWN ...)
            "team" => {
                ui.menus_activate_by_name("tremulous_teamselect");
            }
            "spawn" => {
                let name = match p.team {
                    Team::Aliens => "tremulous_alienclass",
                    Team::Humans => "tremulous_humanitem",
                    Team::None => "tremulous_teamselect",
                };
                ui.menus_activate_by_name(name);
            }
            "evolve" => {
                // MN_A_INFEST: ui_currentClass "class credits"
                if p.team == Team::Aliens {
                    ui.set_cvar("ui_currentClass", &format!("{} {}", p.class, p.credits));
                    ui.menus_activate_by_name("tremulous_alienupgrade");
                }
            }
            "armoury" => {
                ui.menus_activate_by_name("tremulous_humanarmoury");
            }
            "build" => {
                // MN_A_BUILD / MN_H_BUILD: only for builders
                if p.team != Team::None && !load_builds(&p, p.team).is_empty() {
                    ui.menus_activate_by_name(if p.team == Team::Aliens { "tremulous_alienbuild" } else { "tremulous_humanbuild" });
                }
            }
            "dialog" => {
                let name = match p.team {
                    Team::Aliens => "tremulous_alien_dialog",
                    Team::Humans => "tremulous_human_dialog",
                    Team::None => "tremulous_default_dialog",
                };
                ui.menus_activate_by_name(name);
            }
            _ => return false,
        }
        self.flush();
        // hud.gd tracks this open itself
        self.was_open = self.ui.as_ref().is_some_and(|u| u.is_open());
        true
    }

    /// `UI_IsFullscreen`: a fullscreen menu (main, joinserver, ...) is up.
    #[func]
    fn is_fullscreen(&self) -> bool {
        self.ui.as_ref().is_some_and(|u| u.is_fullscreen())
    }

    /// True while the full-screen cinematic plays (clc.state CA_CINEMATIC).
    #[func]
    fn in_cinematic(&self) -> bool {
        self.system_cin.is_some()
    }

    /// The console's `cinematic <name> [1|2]` (CL_PlayCinematic_f).
    #[func]
    fn cinematic(&mut self, name: GString, arg: GString) -> bool {
        if !self.ensure_loaded() {
            return false;
        }
        let ok = self.play_system_cinematic(&name.to_string(), &arg.to_string());
        self.flush();
        ok
    }

    /// Close every interactive menu (Menus_CloseAll); the scoreboard stays
    /// until `hide_scores()`. No `menu_closed` signal (the caller closes).
    #[func]
    fn close(&mut self) {
        if let Some(ui) = self.ui.as_mut() {
            if ui.is_open() {
                ui.menus_close_all();
            }
        }
        self.flush();
        self.was_open = false;
    }

    /// cgame `CG_scrollScoresUp_f` / `CG_scrollScoresDown_f` (scoresUp /
    /// scoresDown): `Menu_ScrollFeeder` of both team lists while the
    /// scoreboard shows.
    #[func]
    fn scroll_scores(&mut self, down: bool) {
        if let Some(ui) = self.ui.as_mut() {
            ui.scroll_scores(down);
        }
    }

    /// The centre of an item (by name, cvar or text) of a ui menu in 640x480
    /// virtual coordinates, (-1, -1) when there is none (tests click it).
    #[func]
    fn item_center(&self, menu: GString, item: GString) -> Vector2 {
        let (m, i) = (menu.to_string(), item.to_string());
        let Some(ui) = self.ui.as_ref() else { return Vector2::new(-1.0, -1.0) };
        let Some(mi) = ui.find_menu(&m) else { return Vector2::new(-1.0, -1.0) };
        let eq = |s: Option<&str>| s.is_some_and(|c| c.eq_ignore_ascii_case(&i));
        let found = ui.sets[0][mi].items.iter().find(|it| eq(Some(&it.def.name)) || eq(it.def.cvar.as_deref()) || eq(it.def.text.as_deref()));
        match found {
            Some(it) => Vector2::new(it.rect[0] + it.rect[2] / 2.0, it.rect[1] + it.rect[3] / 2.0),
            None => Vector2::new(-1.0, -1.0),
        }
    }

    /// `Display_KeyBindPending`: a bind item waits for a key.
    #[func]
    fn key_bind_pending(&self) -> bool {
        self.ui.as_ref().is_some_and(|u| u.key_bind_pending())
    }

    /// Hide the scoreboard only (Tab released).
    #[func]
    fn hide_scores(&mut self) {
        if let Some(ui) = self.ui.as_mut() {
            ui.show_scores = false;
        }
    }

    /// True when the original menus are loaded (the original data is present).
    #[func]
    fn has_menus(&mut self) -> bool {
        self.ensure_loaded()
    }

    /// True while an interactive menu (not the scoreboard) is open.
    #[func]
    fn is_open(&self) -> bool {
        self.ui.as_ref().is_some_and(|u| u.is_open())
    }

    /// Name of the top menu ("" when none).
    #[func]
    fn current_menu(&self) -> GString {
        self.ui.as_ref().and_then(|u| u.top_menu()).map(GString::from).unwrap_or_default()
    }

    /// Show `text` in the team's dialog menu (cgame: ui_dialog + menu
    /// tremulous_<team>_dialog).
    #[func]
    fn show_dialog(&mut self, text: GString) -> bool {
        if !self.ensure_loaded() {
            return false;
        }
        self.ui.as_mut().unwrap().set_cvar("ui_dialog", &text.to_string());
        self.open(GString::from("dialog"))
    }

    /// A ui cvar (`trap_Cvar_VariableStringBuffer`), e.g. cg_drawCrosshair.
    #[func]
    fn get_cvar(&self, name: GString) -> GString {
        self.ui.as_ref().map(|u| GString::from(u.cvar(&name.to_string()).as_str())).unwrap_or_default()
    }

    /// Set a ui cvar.
    #[func]
    fn set_cvar(&mut self, name: GString, value: GString) {
        if self.ensure_loaded() {
            self.ui.as_mut().unwrap().set_cvar(&name.to_string(), &value.to_string());
            self.flush();
        }
    }
}

impl TremMenus {
    /// The frame's draw ops into the canvas items: runs of ordinary ops
    /// share one item, each premultiplied run gets one with a
    /// PREMULT_ALPHA material; the draw index keeps the order.
    fn submit(&mut self) {
        let size = self.base().get_size();
        let (xs, ys) = (size.x / 640.0, size.y / 480.0);
        let parent = self.base().get_canvas_item();
        let mut rs = RenderingServer::singleton();
        let premul_rid = self
            .premul
            .get_or_insert_with(|| {
                let mut m = godot::classes::CanvasItemMaterial::new_gd();
                m.set_blend_mode(godot::classes::canvas_item_material::BlendMode::PREMULT_ALPHA);
                m
            })
            .get_rid();
        let col = |c: &Rgba| Color::from_rgba(c[0], c[1], c[2], c[3].clamp(0.0, 1.0));
        let px = |r: &R| Rect2::new(Vector2::new(r[0] * xs, r[1] * ys), Vector2::new(r[2] * xs, r[3] * ys));
        let ops = std::mem::take(&mut self.ops);
        let mut layer = 0usize;
        let mut i = 0;
        while i < ops.len() {
            let premul = matches!(ops[i], Op::Premul { .. });
            if layer == self.layers.len() {
                let item = rs.canvas_item_create();
                rs.canvas_item_set_parent(item, parent);
                self.layers.push(item);
            }
            let item = self.layers[layer];
            rs.canvas_item_clear(item);
            rs.canvas_item_set_draw_index(item, layer as i32);
            rs.canvas_item_set_material(item, if premul { premul_rid } else { Rid::Invalid });
            while i < ops.len() && matches!(ops[i], Op::Premul { .. }) == premul {
                match &ops[i] {
                    Op::Pic { tex, r, uv, c } => {
                        let tex = &crate::hdtex::ui(tex);
                        let ts = tex.get_size();
                        let src = Rect2::new(Vector2::new(uv[0] * ts.x, uv[1] * ts.y), Vector2::new((uv[2] - uv[0]) * ts.x, (uv[3] - uv[1]) * ts.y));
                        rs.canvas_item_add_texture_rect_region_ex(item, px(r), tex.get_rid(), src).modulate(col(c)).done();
                    }
                    Op::Fill { r, c } => rs.canvas_item_add_rect(item, px(r), col(c)),
                    Op::Premul { tex, r } => {
                        let ts = tex.get_size();
                        rs.canvas_item_add_texture_rect_region(item, px(r), tex.get_rid(), Rect2::new(Vector2::ZERO, ts));
                    }
                }
                i += 1;
            }
            layer += 1;
        }
        for &l in &self.layers[layer..] {
            rs.canvas_item_clear(l);
        }
    }

    /// wt-b: the joystick's keys while the ui has the key catcher
    /// (cl_keys.c `CL_KeyDownEvent` / `CL_KeyUpEvent` -> `UI_KEY_EVENT`;
    /// ui_shared.c `Menu_HandleKey` / `Item_HandleKey` take K_JOY* like any
    /// other key: bind items bind them, the stick's arrow keys move the
    /// cursor and the sliders, K_JOY1-4 have no default action).
    fn wtb_joy_keys(&mut self) {
        let Some(mut g) = self.game() else { return };
        let keys = g.bind_mut().wtb_take_joy_keys(false);
        let Some(ui) = self.ui.as_mut() else { return };
        if keys.is_empty() || !ui.is_open() {
            return;
        }
        for (k, down) in keys {
            ui.key_event_num(k, down);
        }
        self.flush();
    }

    fn game(&self) -> Option<Gd<TremGame>> {
        self.base().get_node_or_null(&self.game_path)?.try_cast::<TremGame>().ok()
    }

    fn ensure_loaded(&mut self) -> bool {
        if self.ui.is_some() {
            return true;
        }
        if self.tried_load {
            return false;
        }
        let Some(mut game) = self.game() else { return false };
        let mut g = game.bind_mut();
        let Some(lib) = g.hud_library() else { return false };
        self.tried_load = true;
        match load_ui(lib) {
            Ok(mut u) => {
                let rs = RenderingServer::singleton();
                u.gl_info = format!("VENDOR: {}\nVERSION: {}\nPIXELFORMAT: color(32-bits) Z(24-bits) stencil(8-bits)\n", rs.get_video_adapter_vendor(), rs.get_video_adapter_name());
                if !self.auto_dialogs {
                    for c in ["cg_disableBuildDialogs", "cg_disableUpgradeDialogs", "cg_disableCommandDialogs"] {
                        u.cvars.insert(c.to_ascii_lowercase(), "1".into());
                    }
                }
                // trap_FS_GetFileList / $modlist / servercache favorites
                u.fs_files = lib.pk3.files().cloned().collect();
                u.fs_files.extend(user_files(&["demos", "video"]));
                u.mod_dirs = mod_dirs();
                u.lan.favorites = load_favorites();
                self.ui = Some(u);
            }
            Err(e) => godot_warn!("TremMenus: {e}; the original menus are disabled"),
        }
        drop(g);
        if self.ui.is_some() {
            // the engine cvars: registered in the client (autogen.cfg keeps
            // them), applied (renderer / sound start-up), mirrored in the ui
            self.vidsnd.window_cvars = game.bind().main_menu_scene();
            let windowed = godot::classes::DisplayServer::singleton().window_get_mode() == godot::classes::display_server::WindowMode::WINDOWED;
            {
                let mut gm = game.bind_mut();
                vidsnd::register(gm.client_cvars(), windowed);
            }
            let ui = self.ui.as_mut().unwrap();
            let names: Vec<String> = ui.cvars.keys().cloned().chain(vidsnd::VIDEO_CVARS.iter().chain(vidsnd::SOUND_CVARS).chain(vidsnd::CLIENT_CVARS).chain(vidsnd::UI_CVARS).map(|(n, _)| n.to_ascii_lowercase())).chain(vidsnd::SERVER_CVARS.iter().map(|(n, _, _)| n.to_ascii_lowercase())).chain(crate::fmodaudio::FMOD_CVARS.iter().map(|(n, _)| n.to_ascii_lowercase())).chain(["name", "rate", "r_availablemodes", "cl_motdstring"].map(String::from)).collect();
            let gm = game.bind();
            let c = gm.client_cvars_ref();
            let mut synced: Vec<String> = names.into_iter().filter(|n| c.find(n).is_some()).collect();
            // the voice chat page's cvars (dec053; registered with the
            // first frame, maybe after this): copied once they exist
            synced.extend(trem_sim::kernel::voip::CL_VOIP_CVARS.iter().map(|(n, _, _)| n.to_ascii_lowercase()).chain(["cl_voiphud", "s_voipinputdevice"].map(String::from)));
            synced.sort();
            synced.dedup();
            self.synced = synced;
        }
        self.ui.is_some()
    }

    /// `UI_UpdateCvars`: client cvars -> the ui's copy, and the engine cvars' effects
    /// (VidSnd: the first call is the vid / snd start-up).
    fn engine_cvars(&mut self) {
        let Some(mut game) = self.game() else { return };
        let mut parent = self.base().clone().upcast::<godot::classes::Node>();
        {
            let mut g = game.bind_mut();
            let first = !self.vidsnd_started();
            self.vidsnd.frame(g.client_cvars(), &mut parent);
            let c = g.client_cvars();
            let ui = self.ui.as_mut().unwrap();
            for n in &self.synced {
                if let Some(v) = c.find(n) {
                    if ui.cvars.get(n) != Some(&v.string) {
                        ui.cvars.insert(n.clone(), v.string.clone());
                    }
                }
            }
            if first {
                // UI_Init's UI_ParseResolutions (after GLimp's r_availableModes)
                ui.parse_resolutions();
                // ui_mousePitch from m_pitch
                let mp = if c.value("m_pitch") >= 0.0 { "0" } else { "1" };
                ui.cvars.insert("ui_mousepitch".into(), mp.into());
            }
        }
    }

    fn vidsnd_started(&self) -> bool {
        self.vidsnd.started()
    }

    /// clc.state changes: a map came up (CL_InitCGame closes the ui and the
    /// background track) or went away (CL_Frame: disconnected with nothing
    /// open brings up UIMENU_MAIN).
    fn connection_state(&mut self) {
        let Some(game) = self.game() else { return };
        let now = game.bind().has_world();
        let was = self.connected.replace(now);
        if now {
            // the intro only plays at a start without a map (Com_Init); a
            // later disconnect goes straight to the main menu
            self.intro_started = true;
        }
        if was == Some(true) && !now {
            // wt-a (dec037): demos recorded in the session show in the Demos
            // menu (FS_GetFileList reads the folder each time)
            let ui = self.ui.as_mut().unwrap();
            ui.fs_files.retain(|f| !f.starts_with("demos/"));
            ui.fs_files.extend(user_files(&["demos"]));
        }
        if was == Some(false) && now {
            self.ui.as_mut().unwrap().set_active_menu(UiMenuCommand::None);
            self.stop_background_track();
            self.flush();
            self.signals().ui_command().emit(&GString::from("connected"));
        }
        // Com_Error( ERR_DROP ) of the network session: com_errorMessage,
        // then UIMENU_MAIN shows error_popmenu
        // wt-a (dec037): UI_DrawConnectScreen at CA_CONNECTED: com_downloadPrompt
        // DLP_SHOW opens download_popmenu (the ui answers with downloadUDP ...)
        let prompt = if now { None } else { self.game().and_then(|mut g| g.bind_mut().take_download_prompt()) };
        if let Some(t) = prompt {
            godot_print!("Opening download prompt...");
            let ui = self.ui.as_mut().unwrap();
            ui.cvars.insert("com_downloadprompttext".into(), t);
            ui.cvars.insert("com_downloadprompt".into(), "0".into());
            ui.menus_activate_by_name("download_popmenu");
            self.flush();
        }
        let err = if now { None } else { self.game().and_then(|mut g| g.bind_mut().take_net_error()) };
        if let Some(e) = err {
            self.intro_started = true;
            let ui = self.ui.as_mut().unwrap();
            ui.set_cvar("com_errorMessage", e.trim());
            ui.set_active_menu(UiMenuCommand::Main);
            self.flush();
            return;
        }
        // CL_MapLoading: a map loading (our own server's too, from Create
        // Server or the console) closes the ui (cls.keyCatchers = 0) so the
        // loading screen shows instead of the menu it was started from
        let loading = self.game().is_some_and(|g| g.bind().is_loading());
        if loading && !now && self.ui.as_ref().unwrap().is_open() {
            self.ui.as_mut().unwrap().set_active_menu(UiMenuCommand::None);
            self.flush();
        }
        // CL_Frame tests clc.state == CA_DISCONNECTED: while connecting (and
        // downloading) or loading there is no map yet, but the connect /
        // loading screen shows
        let connecting = loading || self.game().is_some_and(|g| g.bind().is_connecting());
        if !now && !connecting && self.intro_started && self.system_cin.is_none() && !self.ui.as_ref().unwrap().is_open() {
            self.stop_ui_sound();
            self.ui.as_mut().unwrap().set_active_menu(UiMenuCommand::Main);
            self.flush();
        }
    }

    fn stop_ui_sound(&mut self) {
        // S_StopAllSounds
        if let Some(p) = self.player.as_mut() {
            p.stop();
        }
    }

    /// Snapshot the game, aspect and time into the ui (`UI_Refresh`'s start).
    fn frame(&mut self) {
        let size = self.base().get_size();
        let view = {
            let Some(game) = self.game() else { return };
            let g = game.bind();
            // disconnected (the main menu): an empty snapshot; while
            // connecting, the gamestate's CS_SERVERINFO (wt-a dec037: the
            // download popup's loadServerInfo)
            match g.hud_world().and_then(|w| local_client(w, g.hud_me(), g.hud_view_client()).map(|me| (w, me))) {
                Some((w, me)) => GameView::from_world(w, me),
                None => GameView { server_info: g.connecting_serverinfo(), ..GameView::default() },
            }
        };
        // the client's server lists behind trap_LAN_* (dec033)
        if let Some(mut g) = self.game() {
            if let Some(ui) = self.ui.as_mut() {
                g.bind_mut().lan_sync(&mut ui.lan);
            }
        }
        let mut view = view;
        if self.test_credits >= 0 {
            view.player.credits = self.test_credits;
        }
        // DC->getBindingBuf: the client's bind table; Controls_GetConfig at
        // the first frame (Init_Display)
        let binds = self.game().map(|g| g.bind().hud_bindings()).unwrap_or_default();
        let t = Time::singleton().get_ticks_msec() as i32;
        let time = Time::singleton();
        let d = time.get_datetime_dict_from_system();
        let gi = |k: &str| d.get(k).and_then(|v| v.try_to::<i32>().ok()).unwrap_or(0);
        let local = [gi("month") - 1, gi("day"), gi("year"), gi("hour"), gi("minute")];
        let unix = time.get_unix_time_from_system() as i64;
        let ui = self.ui.as_mut().unwrap();
        ui.unix_time = unix;
        ui.local_time = local;
        if size.x > 0.0 && size.y > 0.0 {
            ui.set_aspect((640.0 * size.y) / (480.0 * size.x));
            ui.resolution = (size.x as i32, size.y as i32);
        }
        let first = ui.key_bindings.is_empty();
        ui.key_bindings = binds;
        if first {
            ui.controls_get_config();
        }
        ui.frame(t, view);
    }

    /// cgame's `CG_Menu` for the local client's latest message (the
    /// G_TriggerMenu refusals / "choose a team" prompts of the simulation).
    fn watch_messages(&mut self) {
        let ui = self.ui.as_mut().unwrap();
        let msg = ui.view.message.clone();
        let first = self.last_msg.is_none();
        if self.last_msg.as_ref() == Some(&msg) {
            return;
        }
        self.last_msg = Some(msg.clone());
        if first {
            return;
        }
        if let Some((_, text)) = msg {
            if let Some(m) = cg_menu(&text, ui.view.mark_deconstruct) {
                ui.cg_menu(m);
            }
        }
        self.flush();
    }

    /// Run what the menus asked for and signal open/close changes.
    fn flush(&mut self) {
        let Some(ui) = self.ui.as_mut() else { return };
        let outs = std::mem::take(&mut ui.out);
        let mut sound = None;
        let mut engine = Vec::new();
        for o in outs {
            match o {
                Out::Sound(s) => sound = Some(s),
                Out::Bind(k, c) => {
                    // trap_Key_SetBinding
                    if let Some(mut g) = self.game() {
                        g.bind_mut().set_binding(k, &c);
                    }
                }
                Out::Cvar(n, v) => {
                    // trap_Cvar_Set: the client's cvar (and the server's)
                    if let Some(mut g) = self.game() {
                        g.bind_mut().ui_set_cvar(&n, &v);
                    }
                }
                Out::BackgroundTrack(intro, looped) => self.background_track(&intro, &looped),
                Out::StopBackgroundTrack => self.stop_background_track(),
                Out::CloseCinematics => self.cins.clear(),
                Out::Print(t) => godot_print!("{}", t.trim_end()),
                Out::Exec(text) => {
                    for line in text.split([';', '\n']) {
                        let line = line.trim();
                        if line.is_empty() {
                            continue;
                        }
                        let w0 = line.split_whitespace().next().unwrap_or("").to_ascii_lowercase();
                        let rest = line[w0.len()..].trim().to_string();
                        match w0.as_str() {
                            "cmd" => self.command(line[3..].trim()),
                            "quit" => engine.push(w0.clone()),
                            "disconnect" => {
                                // CL_Disconnect + SV_Shutdown
                                if let Some(mut g) = self.game() {
                                    g.bind_mut().shutdown_map();
                                }
                                self.stop_ui_sound();
                                engine.push(w0.clone());
                            }
                            // the listen server: map / devmap / map_restart
                            "map" | "devmap" | "map_restart" => {
                                self.stop_background_track();
                                let out = self.game().map(|mut g| g.call("exec", &[GString::from(line).to_variant()]).try_to::<GString>().map(|s| s.to_string()).unwrap_or_default()).unwrap_or_default();
                                let connected = self.game().is_some_and(|g| g.bind().has_world());
                                if !connected {
                                    // Com_Error( ERR_DROP ): com_errorMessage, then
                                    // UIMENU_MAIN shows error_popmenu
                                    if let Some(ui) = self.ui.as_mut() {
                                        ui.set_cvar("com_errorMessage", out.trim());
                                        ui.set_active_menu(UiMenuCommand::Main);
                                    }
                                }
                            }
                            "cinematic" => {
                                let mut a = rest.split_whitespace();
                                let (n, b) = (a.next().unwrap_or("").to_string(), a.next().unwrap_or("").to_string());
                                self.play_system_cinematic(&n, &b);
                            }
                            "vid_restart" => self.vid_restart(),
                            "snd_restart" => {
                                // CL_Snd_Restart_f: S_Shutdown, S_Init, CL_Vid_Restart_f
                                if let Some(mut g) = self.game() {
                                    self.vidsnd.snd_restart(g.bind_mut().client_cvars());
                                }
                                self.vid_restart();
                            }
                            "cvar_restart" => self.cvar_restart(),
                            // cl_main.c's network commands run on the game's network host (dec033; demo: dec037)
                            "localservers" | "globalservers" | "connect" | "reconnect" | "ping" | "demo" => {
                                if matches!(w0.as_str(), "connect" | "reconnect" | "demo") {
                                    self.stop_background_track();
                                }
                                let out = self.game().map(|mut g| g.call("exec", &[GString::from(line).to_variant()]).try_to::<GString>().map(|s| s.to_string()).unwrap_or_default()).unwrap_or_default();
                                if !out.trim().is_empty() {
                                    godot_print!("{}", out.trim_end());
                                }
                            }
                            "messagemode" | "messagemode2" => {
                                if let Some(ui) = self.ui.as_mut() {
                                    ui.message_mode(w0 == "messagemode2");
                                }
                            }
                            "in_restart" | "screenshot" | "screenshotjpeg" | "wait" => {}
                            // the client's own commands (resetDefaults: exec default.cfg)
                            "exec" | "execq" | "bind" | "unbind" | "unbindall" | "set" | "seta" | "setu" | "sets" | "toggle" | "vstr" => self.exec_client(line),
                            // the voice page's Refresh (the game's own command)
                            "voip_refreshdevices" => self.exec_client(line),
                            // anything else is forwarded to the server
                            _ => self.command(line),
                        }
                    }
                }
            }
        }
        if let Some(s) = sound {
            self.play(&s);
        }
        if self.ui.as_ref().is_some_and(|u| u.lan.favorites_changed) {
            // LAN_SaveServersToCache
            let ui = self.ui.as_mut().unwrap();
            ui.lan.favorites_changed = false;
            save_favorites(&ui.lan.favorites);
        }
        let open = self.ui.as_ref().is_some_and(|u| u.is_open());
        if open != self.was_open {
            self.was_open = open;
            if open {
                let kind = GString::from(self.ui.as_ref().and_then(|u| u.top_menu()).unwrap_or(""));
                self.signals().menu_opened().emit(&kind);
            } else {
                self.signals().menu_closed().emit();
            }
        }
        for c in engine {
            let c = GString::from(c.as_str());
            self.signals().ui_command().emit(&c);
        }
    }

    /// `trap_Cmd_ExecuteText` of a client command (TremGame.exec).
    fn exec_client(&mut self, line: &str) {
        let Some(mut game) = self.game() else { return };
        game.call("exec", &[GString::from(line).to_variant()]);
    }

    fn command(&mut self, line: &str) {
        let Some(mut game) = self.game() else { return };
        let r = game.call("command", &[GString::from(line).to_variant()]);
        let r = r.try_to::<GString>().map(|s| s.to_string()).unwrap_or_default();
        if !r.is_empty() {
            godot_print!("{}", r);
        }
    }

    fn play(&mut self, path: &str) {
        let Some(mut game) = self.game() else { return };
        let stream = {
            let mut g = game.bind_mut();
            let Some(lib) = g.hud_library() else { return };
            lib.sound(path)
        };
        if let (Some(s), Some(p)) = (stream, self.player.as_mut()) {
            p.set_stream(&s);
            p.play();
        }
    }

    /// Resolve the draw commands' images through the original data.
    fn resolve(&mut self, cmds: Vec<Cmd>) {
        if cmds.is_empty() {
            return;
        }
        let Some(mut game) = self.game() else { return };
        let mut g = game.bind_mut();
        let Some(lib) = g.hud_library() else { return };
        let size = self.base().get_size();
        let (xs, ys) = (size.x / 640.0, size.y / 480.0);
        let now = Time::singleton().get_ticks_msec() as i64;
        let mut parent = self.base().clone().upcast::<godot::classes::Node>();
        for c in cmds {
            match c {
                Cmd::Pic { img, r, uv, c } => {
                    let tex = self.tex.entry(img.to_lowercase()).or_insert_with(|| lib.texture(&img)).clone();
                    if let Some(tex) = tex {
                        self.ops.push(Op::Pic { tex, r, uv, c });
                    }
                }
                Cmd::Fill { r, c } => self.ops.push(Op::Fill { r, c }),
                Cmd::Model { asset, r, fov, angle, time } => {
                    // `Item_Model_Paint`: `UI_AdjustFrom640`, `trap_R_RegisterModel`,
                    // `trap_R_ModelBounds`, `trap_R_ClearScene`,
                    // `trap_R_AddRefEntityToScene`, `trap_R_RenderScene`
                    if !self.models.contains_key(&asset) {
                        let Some(m) = MenuModel::new(lib, &asset, &mut self.shaders, &mut parent) else { continue };
                        self.models.insert(asset.clone(), m);
                    }
                    let m = self.models.get_mut(&asset).unwrap();
                    m.paint(r[2] * xs, r[3] * ys, fov, angle, time);
                    if let Some(tex) = m.texture() {
                        self.ops.push(Op::Premul { tex, r });
                    }
                }
                Cmd::Cinematic { name, r } => {
                    // `UI_PlayCinematic` (`trap_CIN_PlayCinematic`, CIN_loop |
                    // CIN_silent), `UI_RunCinematicFrame` (`trap_CIN_RunCinematic`),
                    // `UI_DrawCinematic` (`trap_CIN_SetExtents` + `trap_CIN_DrawCinematic`)
                    let key = name.to_ascii_lowercase();
                    if !self.cins.contains_key(&key) {
                        let roq = lib.pk3.read(&name).and_then(|d| Roq::play(&name, d.to_vec(), r, cinematic::CIN_LOOP | cinematic::CIN_SILENT, now));
                        let roq = roq.map(|mut q| {
                            q.first_frame(now);
                            (q, None, true)
                        });
                        self.cins.insert(key.clone(), roq);
                    }
                    if let Some(Some((q, tex, used))) = self.cins.get_mut(&key) {
                        *used = true;
                        q.run(now);
                        if let Some(t) = upload_frame(q, tex) {
                            self.ops.push(Op::Pic { tex: t, r, uv: [0.0, 0.0, 1.0, 1.0], c: WHITE });
                        }
                    }
                }
            }
        }
    }

    /// `CL_PlayCinematic_f` (`cinematic <name> [1|2]`): CIN_system closes
    /// the ui (UIMENU_NONE) and the client is in CA_CINEMATIC until it ends
    /// or a key stops it. False when the file is missing / not a RoQ.
    fn play_system_cinematic(&mut self, arg: &str, s: &str) -> bool {
        if self.system_cin.is_some() {
            self.stop_system_cinematic();
        }
        let bits = cinematic::play_cinematic_bits(arg, s);
        // S_StopAllSounds
        self.stop_background_track();
        self.stop_ui_sound();
        let path = cinematic::cinematic_path(arg);
        let now = Time::singleton().get_ticks_msec() as i64;
        let data = {
            let Some(mut game) = self.game() else { return false };
            let mut g = game.bind_mut();
            let Some(lib) = g.hud_library() else { return false };
            lib.pk3.read(&path).map(|d| d.to_vec())
        };
        let Some(data) = data else { return false };
        let Some(mut roq) = Roq::play(&path, data, [0.0, 0.0, 640.0, 480.0], bits, now) else { return false };
        if let Some(ui) = self.ui.as_mut() {
            ui.set_active_menu(UiMenuCommand::None);
        }
        // wait for first frame (load codebook and sound)
        roq.first_frame(now);
        if let Some(p) = self.cin_audio.as_mut() {
            p.play();
        }
        self.system_cin = Some((roq, None));
        true
    }

    /// `SCR_StopCinematic` (+ `CL_Disconnect_f`'s UIMENU_MAIN when no map runs).
    fn stop_system_cinematic(&mut self) {
        if self.system_cin.take().is_none() {
            return;
        }
        if let Some(p) = self.cin_audio.as_mut() {
            p.stop();
        }
        // connected or connecting (the connect screen shows): no main menu
        let connected = self.game().is_some_and(|g| g.bind().has_world() || g.bind().is_connecting());
        if !connected {
            if let Some(ui) = self.ui.as_mut() {
                ui.set_active_menu(UiMenuCommand::Main);
            }
            self.flush();
        }
    }

    /// `SCR_RunCinematic` + `SCR_DrawCinematic` (full screen, over a black
    /// screen) and its S_RawSamples.
    fn system_cinematic(&mut self) {
        let now = Time::singleton().get_ticks_msec() as i64;
        let done = {
            let Some((q, tex)) = self.system_cin.as_mut() else { return };
            q.run(now);
            self.ops.push(Op::Fill { r: [0.0, 0.0, 640.0, 480.0], c: [0.0, 0.0, 0.0, 1.0] });
            if let Some(t) = upload_frame(q, tex) {
                self.ops.push(Op::Pic { tex: t, r: [0.0, 0.0, 640.0, 480.0], uv: [0.0, 0.0, 1.0, 1.0], c: WHITE });
            }
            if !q.audio.is_empty() {
                if let Some(mut pb) = self.cin_audio.as_mut().and_then(|p| p.get_stream_playback()).and_then(|pb| pb.try_cast::<AudioStreamGeneratorPlayback>().ok()) {
                    let n = (pb.get_frames_available().max(0) as usize).min(q.audio.len() / 2);
                    if n > 0 {
                        let frames: PackedVector2Array = q.audio[..n * 2].chunks(2).map(|c| Vector2::new(c[0] as f32 / 32768.0, c[1] as f32 / 32768.0)).collect();
                        pb.push_buffer(&frames);
                        q.audio.drain(..n * 2);
                    }
                }
            }
            q.finished()
        };
        if done {
            // RoQShutdown: clc.state = CA_DISCONNECTED; CL_Frame brings up the main menu
            self.stop_system_cinematic();
        }
    }

    /// `S_StartBackgroundTrack( intro, loop )` (the loop file repeats).
    fn background_track(&mut self, _intro: &str, looped: &str) {
        if looped.eq_ignore_ascii_case(&self.music_track) && self.music.as_ref().is_some_and(|m| m.is_playing()) {
            return;
        }
        let stream = {
            let Some(mut game) = self.game() else { return };
            let mut g = game.bind_mut();
            let Some(lib) = g.hud_library() else { return };
            lib.sound(looped)
        };
        let Some(stream) = stream else { return };
        let stream = match stream.clone().try_cast::<AudioStreamWav>() {
            Ok(w) => {
                let mut w2 = w.duplicate_resource();
                let bytes = w2.get_data().len() as i32;
                let per = if w2.is_stereo() { 4 } else { 2 };
                w2.set_loop_mode(godot::classes::audio_stream_wav::LoopMode::FORWARD);
                w2.set_loop_begin(0);
                w2.set_loop_end(bytes / per);
                w2.upcast::<godot::classes::AudioStream>()
            }
            Err(s) => s,
        };
        self.music_track = looped.to_string();
        if let Some(m) = self.music.as_mut() {
            m.set_stream(&stream);
            m.play();
        }
    }

    /// `S_StopBackgroundTrack`.
    fn stop_background_track(&mut self) {
        self.music_track.clear();
        if let Some(m) = self.music.as_mut() {
            m.stop();
        }
    }

    /// `vid_restart` (CL_Vid_Restart_f: the renderer and the ui start again
    /// with the latched cvars; UI_ParseResolutions).
    fn vid_restart(&mut self) {
        let Some(mut game) = self.game() else { return };
        let mut parent = self.base().clone().upcast::<godot::classes::Node>();
        let mut g = game.bind_mut();
        self.vidsnd.vid_restart(g.client_cvars(), &mut parent);
        let modes = g.client_cvars().string("r_availableModes");
        if let Some(ui) = self.ui.as_mut() {
            ui.cvars.insert("r_availablemodes".into(), modes);
            ui.parse_resolutions();
        }
    }

    /// `Cvar_Restart_f` for the engine cvars of the menus (back to their
    /// reset values).
    fn cvar_restart(&mut self) {
        let Some(mut game) = self.game() else { return };
        let mut g = game.bind_mut();
        let c = g.client_cvars();
        let names: Vec<&str> = vidsnd::VIDEO_CVARS.iter().chain(vidsnd::SOUND_CVARS).chain(vidsnd::CLIENT_CVARS).chain(vidsnd::UI_CVARS).map(|(n, _)| *n).collect();
        for n in names {
            if let Some(r) = c.find(n).map(|v| v.reset_string.clone()) {
                c.set(n, &r);
            }
        }
    }

    /// The assetGlobalDef cursor as the mouse cursor while a menu is open
    /// (UI_Refresh draws it 32*aspectScale x 32 virtual units, centred;
    /// hidden while an edit field has the keys: ui_hideCursor).
    fn update_cursor_image(&mut self) {
        let size = self.base().get_size();
        let (open, hide, aspect) = self.ui.as_ref().map(|u| (u.is_open(), u.cvar_value("ui_hideCursor") != 0.0, u.aspect)).unwrap_or((false, false, 1.0));
        let want = if !open || size.x <= 0.0 { None } else { Some(((32.0 * aspect * size.x / 640.0).round().clamp(8.0, 256.0) as i32, (32.0 * size.y / 480.0).round().clamp(8.0, 256.0) as i32, hide)) };
        if want == self.cursor_shape {
            return;
        }
        self.cursor_shape = want;
        let mut input = Input::singleton();
        let Some((w, h, hide)) = want else {
            input.set_custom_mouse_cursor(Option::<&Gd<godot::classes::Resource>>::None);
            return;
        };
        let img: Option<Gd<Image>> = if hide {
            Image::create_empty(w, h, false, godot::classes::image::Format::RGBA8)
        } else {
            let tex = self.tex.get(&CURSOR.to_lowercase()).cloned().flatten().or_else(|| {
                let mut game = self.game()?;
                let mut g = game.bind_mut();
                g.hud_library()?.texture(CURSOR)
            });
            tex.and_then(|t| t.get_image()).map(|mut i| {
                if i.is_compressed() {
                    i.decompress();
                }
                i.resize(w, h);
                i
            })
        };
        let Some(img) = img else { return };
        if let Some(t) = ImageTexture::create_from_image(&img) {
            input.set_custom_mouse_cursor_ex(&t).hotspot(Vector2::new(w as f32 / 2.0, h as f32 / 2.0)).done();
        }
    }
}

#[godot_api(secondary)]
impl TremMenus {
    /// wt-b (tests): the focused menu's item with WINDOW_HASFOCUS as
    /// "name|text", "" without one.
    #[func]
    fn focused_item(&self) -> GString {
        let Some(ui) = self.ui.as_ref() else { return GString::new() };
        let Some(mi) = ui.menu_get_focused() else { return GString::new() };
        let it = ui.sets[0][mi].items.iter().rfind(|it| it.flags & WINDOW_HASFOCUS != 0);
        GString::from(it.map(|it| format!("{}|{}", it.def.name, it.def.text.clone().unwrap_or_default())).unwrap_or_default().as_str())
    }
}

// =====================================================================
// tests (legacy data: /tmp/claude-0/datalegacy)
// =====================================================================

#[cfg(test)]
mod tests {
    use super::*;

    const DATA: &str = "/tmp/claude-0/datalegacy";

    fn read(p: &str) -> Option<String> {
        std::fs::read_to_string(std::path::Path::new(DATA).join(p)).ok()
    }

    fn have_data() -> bool {
        let ok = std::path::Path::new(DATA).join("ui/ingame.txt").exists();
        if !ok {
            eprintln!("{DATA} missing: skipping");
        }
        ok
    }

    #[test]
    fn port_audio_page_loads_and_options_link_to_it() {
        // the 1.1.0 data's ui/ (unzipped from data-1.1.0.pk3)
        const D110: &str = "/tmp/claude-0/data110";
        let read = |p: &str| std::fs::read_to_string(std::path::Path::new(D110).join(p)).ok();
        let Some(opts) = read("ui/ingame_options.menu") else {
            eprintln!("{D110} missing: skipping");
            return;
        };
        let patched = patch_sound_options(&opts);
        assert!(!patched.contains("s_useOpenAL") && patched.contains("open port_audio"));
        let mut r = |p: &str| match p {
            PORT_AUDIO_MENU_FILE => Some(port_audio_menu()),
            "ui/ingame_options.menu" => Some(patched.clone()),
            _ => read(p),
        };
        let files = vec!["ui/ingame_options.menu".to_string(), PORT_AUDIO_MENU_FILE.to_string()];
        let (menus, _) = load_menu_files(&files, &mut r).unwrap();
        let page = menus.iter().find(|m| m.name() == "port_audio").expect("port_audio parsed");
        let txt = format!("{page:?}");
        for (_, cvar, _) in PORT_AUDIO_ROWS {
            assert!(txt.contains(cvar), "{cvar} missing from the page");
        }
        assert!(menus.iter().any(|m| m.name() == "ingame_options"));
    }

    #[test]
    fn voice_chat_page_and_its_button() {
        let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../re/source/assets");
        let read = |p: &str| std::fs::read_to_string(repo.join(p)).ok();
        let opts = read("ui/ingame_options.menu").expect("legacy ingame_options.menu");
        let patched = patch_voice_button(&patch_voip_bind(&patch_sound_options(&opts)));
        assert_eq!(patched.matches("\"s_doppler\"").count(), 1, "the copy has no cvar");
        assert!(patched.contains("type ITEM_TYPE_BUTTON\n      text \"Voice Chat...\"\n      rect SCONTENT_X (SCONTENT_Y+(6*ELEM_H))"), "the row after Doppler");
        assert!(patched.contains("open port_voice"));
        assert_eq!(patch_voice_button("menuDef { }"), "menuDef { }");
        let devices = vec!["Default".to_string(), "Headset Microphone (USB \"Pro\" Audio Device)".to_string()];
        let page = port_voice_menu(&devices);
        let mut r = |p: &str| match p {
            PORT_VOICE_MENU_FILE => Some(page.clone()),
            "ui/ingame_options.menu" => Some(patched.clone()),
            _ => read(p),
        };
        let files = vec!["ui/ingame_options.menu".to_string(), PORT_VOICE_MENU_FILE.to_string()];
        let (menus, _) = load_menu_files(&files, &mut r).unwrap();
        let m = menus.iter().find(|m| m.name() == "port_voice").expect("port_voice parsed");
        let txt = format!("{m:?}");
        for cvar in ["cl_voip", "s_voipInputDevice", "cl_voipCaptureMult", "cl_voipGainDuringCapture", "cl_voipUseVAD", "cl_voipHud"] {
            assert!(txt.contains(cvar), "{cvar} missing from the page");
        }
        // the device's full name is the value, its label cut to 24 characters
        assert!(txt.contains("(\"Headset Microphone (USB \", \"Headset Microphone (USB 'Pro' Audio Device)\")"), "the device row's list");
        assert!(page.contains("\"Headset Microphone (USB \", \"Headset"));
        // the Refresh button left of the microphone row, which starts after it
        assert!(txt.contains("refreshmic") && txt.contains("voipRefreshDevices"), "the Refresh button");
        assert!(page.contains("rect 20 77 70 20") && page.contains("rect 100 77 220 20"));
        assert_eq!(page.matches("rect 20 77 300 20").count(), 0);
    }

    #[test]
    fn createserver_settings_have_help() {
        // the legacy menus as re/source/assets has them (data-legacy-assets.pk3)
        let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../re/source/assets");
        let read = |p: &str| std::fs::read_to_string(repo.join(p)).ok();
        let Some(orig) = read("ui/createserver.menu") else {
            eprintln!("re/source/assets has no ui/createserver.menu: skipping");
            return;
        };
        let patched = patch_createserver(&orig);
        // every setting shows its help on hover and hides it again
        for cvar in ["sv_hostname", "timelimit", "sv_maxclients", "g_needpassword", "g_password", "sv_pure", "ui_dedicated", "sv_allowdownload", "g_allowvote", "sv_minping", "sv_maxping", "g_synchronousclients", "sv_maxrate", "sv_zombietime", "sv_reconnectlimit"] {
            let (s, e) = item_range(&patched, &format!("cvar \"{cvar}\"")).unwrap();
            let it = &patched[s..e];
            assert!(it.contains("mouseEnter") && it.contains("mouseExit"), "{cvar}");
            assert!(patched.contains(&format!("Official name: {cvar}")), "{cvar} help");
        }
        assert!(patched.contains("Official name: map"));
        // the gameplay values are the file's own: only the added blocks differ
        assert_eq!(patched.matches("cvar \"").count(), orig.matches("cvar \"").count());
        // it parses, and the help items start hidden
        let mut r = |p: &str| if p == "ui/createserver.menu" { Some(patched.clone()) } else { read(p) };
        let (menus, _) = load_menu_files(&["ui/createserver.menu".to_string()], &mut r).unwrap();
        let m = menus.iter().find(|m| m.name() == "createserver").expect("createserver parsed");
        let txt = format!("{m:?}");
        assert!(txt.contains("help3_0"));
        assert_eq!(patch_createserver("menuDef { }"), "menuDef { }");
    }

    #[test]
    fn controls_have_the_voip_toggle_row() {
        // the legacy menus as re/source/assets has them (data-legacy-assets.pk3)
        let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../re/source/assets");
        let read = |p: &str| std::fs::read_to_string(repo.join(p)).ok();
        let opts = read("ui/ingame_options.menu").expect("legacy ingame_options.menu");
        let patched = patch_voip_bind(&patch_sound_options(&opts));
        assert_eq!(patched.matches("\"voip_toggle\"").count(), 1);
        assert!(patched.contains("text \"Voice Chat Mic:\"\n      cvar \"voip_toggle\"\n      rect SCONTENT_X (SCONTENT_Y+(13*ELEM_H))"), "the row after Screenshot");
        // a menu without the Screenshot row is unchanged
        assert_eq!(patch_voip_bind("menuDef { }"), "menuDef { }");
        let mut r = |p: &str| if p == "ui/ingame_options.menu" { Some(patched.clone()) } else { read(p) };
        let (menus, _) = load_menu_files(&["ui/ingame_options.menu".to_string()], &mut r).unwrap();
        let m = menus.iter().find(|m| m.name() == "ingame_options").expect("ingame_options parsed");
        assert!(format!("{m:?}").contains("voip_toggle"));
        assert!(G_BINDINGS.iter().any(|&(c, k)| c == "voip_toggle" && k == b'k' as i32));
    }

    fn fonts() -> Fonts {
        let f = |n: i32| parse_font_dat(&std::fs::read(format!("{DATA}/fonts/fontImage_{n}.dat")).unwrap()).unwrap();
        Fonts::new(f(20), f(26), f(34))
    }

    /// The ui as UI_Init loads it, with a human (or `team`) local player.
    fn ui(team: Team) -> Ui {
        ui_from(team, &mut |p: &str| read(p))
    }

    /// `ui` over another file reader.
    fn ui_from(team: Team, r: &mut dyn FnMut(&str) -> Option<String>) -> Ui {
        let mut r = |p: &str| r(p);
        let files = menu_list_files(&mut r).unwrap();
        let (menus, defines) = load_menu_files(&files, &mut r).unwrap();
        let (cg, _) = load_menu_files(&["ui/teamscore.menu".to_string()], &mut r).unwrap();
        let mut u = Ui::new(fonts(), menus, cg, &defines);
        u.help = parse_help(&read("ui/help.txt").unwrap());
        u.arenas = vec![("niveus".into(), "Niveus".into()), ("atcs".into(), "ATCS".into())];
        let mut view = GameView { me: 1, ..Default::default() };
        view.player = Player { team, class: if team == Team::Aliens { ClassId::Level0 as i32 } else { ClassId::HumanBase as i32 }, credits: 0, stats: vec![0; 16], alien_stage: 0, human_stage: 0, vanilla: false };
        view.player.stats[Stat::Team as usize] = team as i32;
        view.clients = vec![
            ClientView { num: 0, name: "^1Granger".into(), team: Team::Aliens, ignored: vec![] },
            ClientView { num: 1, name: "Me".into(), team, ignored: vec![] },
            ClientView { num: 3, name: "Bob".into(), team, ignored: vec![1] },
        ];
        u.frame(1000, view);
        u
    }

    fn mref(u: &Ui, menu: &str, item: &str) -> IRef {
        let m = u.find_menu(menu).unwrap_or_else(|| panic!("menu {menu}"));
        let i = u.sets[0][m].find_item(item).unwrap_or_else(|| panic!("item {item}"));
        IRef { set: 0, menu: m, item: i }
    }

    /// Click (down + up) at the centre of an item, after moving there.
    fn click(u: &mut Ui, r: IRef) {
        let rect = u.it(r).rect;
        let (x, y) = (rect[0] + rect[2] / 2.0, rect[1] + rect[3] / 2.0);
        click_at(u, x, y);
    }
    fn click_at(u: &mut Ui, x: f32, y: f32) {
        u.mouse_move(x, y);
        u.key_event(K::Mouse1, true);
        u.key_event(K::Mouse1, false);
    }
    fn execs(u: &mut Ui) -> Vec<String> {
        u.out.drain(..).filter_map(|o| if let Out::Exec(e) = o { Some(e) } else { None }).collect()
    }

    #[test]
    fn menus_legacy_lists_load() {
        if !have_data() {
            return;
        }
        let mut r = |p: &str| read(p);
        let files = menu_list_files(&mut r).unwrap();
        // ui/menus.txt, ui/ingame.txt, ui/tremulous.txt (UI_Init's order)
        assert_eq!(files.len(), 33, "{files:?}");
        assert_eq!(files[0], "ui/main.menu");
        assert_eq!(files[17], "ui/ingame.menu");
        assert!(files.contains(&"ui/say.menu".to_string()) && files.contains(&"ui/tremulous_dialogs.menu".to_string()));
        let (menus, defines) = load_menu_files(&files, &mut r).unwrap();
        let names: Vec<&str> = menus.iter().map(|m| m.name()).collect();
        for n in [
            "main",
            "joinserver",
            "createserver",
            "simple_options",
            "mod",
            "demo",
            "news_menu",
            "quit_popmenu",
            "quitCredit",
            "error_popmenu",
            "ingame",
            "ingame_game",
            "ingame_options",
            "ingame_leave",
            "ingame_help",
            "tremulous_teamselect",
            "tremulous_alienclass",
            "tremulous_humanitem",
            "tremulous_alienbuild",
            "tremulous_humanbuild",
            "tremulous_humanarmoury",
            "tremulous_default_dialog",
            "tremulous_human_dialog",
            "tremulous_alien_dialog",
            "tremulous_alienupgrade",
            "say",
            "say_team",
            "say_command",
        ] {
            assert!(names.contains(&n), "menu {n} in {names:?}");
        }
        // every feeder / ownerdraw this module drives is a menudef.h name
        let f = feeder_table(&defines);
        assert_eq!(f.len(), FEEDERS.len());
        assert_eq!(define_int(&defines, "FEEDER_TREMTEAMS"), Some(12));
        assert_eq!(define_int(&defines, "ITEM_TYPE_OWNERDRAW"), Some(IT_OWNERDRAW));
        assert_eq!(define_int(&defines, "ITEM_TYPE_SAYFIELD"), Some(IT_SAYFIELD));
        assert_eq!(define_int(&defines, "ITEM_TYPE_BIND"), Some(IT_BIND));
        assert_eq!(define_int(&defines, "ASPECT_NONE"), Some(ASPECT_NONE));
        assert_eq!(define_int(&defines, "VALIGN_TOP"), Some(VALIGN_TOP));
        let od = ownerdraw_table(&defines);
        assert_eq!(od.len(), OWNERDRAWS.len());
    }

    #[test]
    fn menus_items_post_parse() {
        if !have_data() {
            return;
        }
        let u = ui(Team::Humans);
        // ownerdraw items are ITEM_TYPE_OWNERDRAW (not the 1.1 number 8 = LISTBOX)
        let pane = mref(&u, "tremulous_teamselect", "infopane");
        assert_eq!(u.it(pane).ty, IT_OWNERDRAW);
        assert_eq!(u.ods[&u.it(pane).def.ownerdraw], Od::InfoPane(Feeder::Teams));
        // Menu_PostParse pushes listboxes to the end
        let ts = &u.sets[0][pane.menu];
        assert_eq!(ts.items.last().unwrap().def.name, "list");
        assert_eq!(ts.items.last().unwrap().ty, IT_LISTBOX);
        assert_eq!(u.feeders[&ts.items.last().unwrap().feeder], Feeder::Teams);
        assert!(ts.flags & WINDOW_POPUP != 0 && ts.flags & WINDOW_OOB_CLICK != 0);
        // say field
        let say = mref(&u, "say", "say_field");
        assert_eq!(u.it(say).ty, IT_SAYFIELD);
        assert_eq!(u.it(say).edit.max_chars, 128);
        assert_eq!(u.it(say).edit.max_paint_chars, MAX_EDITFIELD);
        assert_eq!(u.it(say).def.text_style, TEXTSTYLE_SHADOWED);
        // cvarFloat slider
        let opt = u.find_menu("ingame_options").unwrap();
        let slider = u.sets[0][opt].items.iter().find(|i| i.ty == IT_SLIDER && i.def.cvar.as_deref() == Some("sensitivity")).unwrap();
        assert_eq!((slider.edit.def, slider.edit.min, slider.edit.max), (5.0, 1.0, 30.0));
        let multi = u.sets[0][opt].items.iter().find(|i| i.def.cvar.as_deref() == Some("cg_drawCrosshair")).unwrap();
        assert_eq!(multi.ty, IT_MULTI);
        assert_eq!(multi.def.multi.len(), 3);
        // the scoreboard's lists: notselectable + noscrollbar, legacy columns
        let sc = &u.sets[1][0];
        assert_eq!(sc.name(), SCORE_MENU);
        let ll = sc.items.iter().find(|i| i.def.name == "leftlist").unwrap();
        assert!(ll.def.not_selectable && ll.list.no_scrollbar);
        assert_eq!(ll.def.columns.len(), 7);
        assert_eq!(ll.def.columns[3], [45, 80, 0, ALIGN_LEFT]);
        assert_eq!(ll.def.columns[6], [210, 20, 0, ALIGN_RIGHT]);
        let help = mref(&u, "ingame_help", "list");
        assert!(!u.it(help).list.no_scrollbar);
    }

    #[test]
    fn menus_aspect_compensation() {
        if !have_data() {
            return;
        }
        let mut u = ui(Team::Humans);
        let bar = u.find_menu("ingame").unwrap();
        let game = mref(&u, "ingame", "game");
        let ts = u.find_menu("tremulous_teamselect").unwrap();
        assert_eq!(u.sets[0][bar].rect, [0.0, 0.0, 1280.0, 56.0]);
        assert_eq!(u.it(game).rect, [25.0, 0.0, 62.5, 56.0]);
        // 16:9 -> aspectScale 0.75
        u.set_aspect((640.0 * 720.0) / (480.0 * 1280.0));
        assert!((u.aspect - 0.75).abs() < 1e-6);
        // ingame: aspectBias ALIGN_LEFT
        assert_eq!(u.sets[0][bar].rect, [0.0, 0.0, 960.0, 56.0]);
        assert_eq!(u.it(game).rect, [18.75, 0.0, 46.875, 56.0]);
        // menus default to ALIGN_CENTER
        let r = u.sets[0][ts].rect;
        assert!((r[0] - (95.0 * 0.75 + 80.0)).abs() < 1e-4 && (r[2] - 450.0 * 0.75).abs() < 1e-4, "{r:?}");
        // back to 4:3 restores the parsed layout
        u.set_aspect(1.0);
        assert_eq!(u.it(game).rect, [25.0, 0.0, 62.5, 56.0]);
        // the hit-testing uses the compensated rects: the Game button opens
        // ingame_game at 16:9 too
        u.set_aspect(0.75);
        u.open_ingame();
        click(&mut u, game);
        assert_eq!(u.top_menu(), Some("ingame_game"));
    }

    #[test]
    fn menus_text_extents() {
        if !have_data() {
            return;
        }
        let mut u = ui(Team::Humans);
        u.set_aspect(0.75);
        u.open_ingame();
        u.paint();
        let game = mref(&u, "ingame", "game");
        let it = u.it(game);
        // textalign ALIGN_CENTER, textvalign VALIGN_CENTER, textscale .4
        let w = u.fonts.width("Game", 0.4, 0.75);
        let h = u.fonts.height("Game", 0.4);
        let tr = it.text_rect;
        assert!((tr[0] - (it.rect[0] + (it.rect[2] - w) / 2.0)).abs() < 1e-3, "{tr:?}");
        assert!((tr[1] - (it.rect[1] + (h + it.rect[3]) / 2.0)).abs() < 1e-3, "{tr:?}");
        assert_eq!((tr[2], tr[3]), (w, h));
        // the aspect squeezes text horizontally only
        assert!((u.fonts.width("Game", 0.4, 1.0) * 0.75 - w).abs() < 1e-3);
        // UI_FontForScale: .4 is the medium font, .2 the small one, .5 the big one
        assert!(std::ptr::eq(u.fonts.pick(0.4), &u.fonts.text));
        assert!(std::ptr::eq(u.fonts.pick(0.2), &u.fonts.small));
        assert!(std::ptr::eq(u.fonts.pick(0.5), &u.fonts.big));
    }

    #[test]
    fn menus_team_select_flow() {
        if !have_data() {
            return;
        }
        let mut u = ui(Team::None);
        u.menus_activate_by_name("tremulous_teamselect");
        assert_eq!(u.top_menu(), Some("tremulous_teamselect"));
        // onOpen: uiScript LoadTeams; setFocus list
        let list = mref(&u, "tremulous_teamselect", "list");
        assert_eq!(u.lists[&Feeder::Teams].len(), 4);
        assert!(u.it(list).flags & WINDOW_HASFOCUS != 0);
        assert_eq!(u.selection(Feeder::Teams), 0);
        let text = info_pane_text(u.selected(Feeder::Teams).unwrap(), &u.view.player);
        assert!(text.starts_with("The Alien Team\n\n"));
        // the second row ("Humans") under the mouse, then a click selects it
        let rect = u.it(list).rect;
        let eh = u.it(list).def.element_height;
        click_at(&mut u, rect[0] + 20.0, rect[1] + 1.0 + eh * 1.5);
        assert_eq!(u.selection(Feeder::Teams), 1);
        assert_eq!(u.it(list).cursor_pos, 1);
        assert!(execs(&mut u).is_empty());
        // a second click within DOUBLE_CLICK_DELAY runs doubleClick
        click_at(&mut u, rect[0] + 20.0, rect[1] + 1.0 + eh * 1.5);
        assert_eq!(execs(&mut u), vec!["cmd team humans\n".to_string()]);
        assert!(!u.is_open());
        // keyboard: Enter on the focused list runs doubleClick too
        u.menus_activate_by_name("tremulous_teamselect");
        u.key_event(K::Enter, true);
        assert_eq!(execs(&mut u), vec!["cmd team aliens\n".to_string()]);
        // Esc closes everything (UI_KeyEvent)
        u.menus_activate_by_name("tremulous_teamselect");
        u.key_event(K::Escape, true);
        assert!(!u.is_open());
    }

    #[test]
    fn menus_ingame_game_vote_and_ignore() {
        if !have_data() {
            return;
        }
        let mut u = ui(Team::Humans);
        u.open_ingame();
        let g = mref(&u, "ingame", "game");
        click(&mut u, g);
        assert_eq!(u.top_menu(), Some("ingame_game"));
        assert_eq!(u.stack.len(), 2);
        let gm = u.find_menu("ingame_game").unwrap();
        let m = &u.sets[0][gm];
        // onOpen: hide gameGrp; show vote; show mapvote; setitemcolor voteBtn ...
        let vis = |name: &str| m.items.iter().filter(|i| i.def.name == name).all(|i| i.flags & WINDOW_VISIBLE != 0);
        let hid = |name: &str| m.items.iter().filter(|i| i.def.name == name).all(|i| i.flags & WINDOW_VISIBLE == 0);
        assert!(vis("vote") && vis("mapvote") && hid("ignore") && hid("info") && hid("playervote"));
        assert_eq!(m.items[m.find_item("voteBtn").unwrap()].fore, [0.2, 0.2, 0.2, 1.0]);
        // loadArenas / loadServerInfo
        let maps = *u.feeders.iter().find(|(_, f)| **f == Feeder::Maps).unwrap().0;
        assert_eq!(u.feeder_item_text(maps, 0, 0).0, "ATCS");
        assert_eq!(u.cvar("ui_serverinfo_hostname"), "");
        assert_eq!(u.map_list[1].1, "Niveus");
        // "Begin Sudden Death": exec "cmd callvote sudden_death"; uiScript closeingame
        let sd = u.sets[0][gm].items.iter().position(|i| i.def.text.as_deref() == Some("Begin Sudden Death")).unwrap();
        click(&mut u, IRef { set: 0, menu: gm, item: sd });
        assert_eq!(execs(&mut u), vec!["cmd callvote sudden_death ; ".to_string()]);
        assert!(!u.is_open());
        // the Ignore tab and FEEDER_IGNORE_LIST
        u.open_ingame();
        let g = mref(&u, "ingame", "game");
        click(&mut u, g);
        let ib = mref(&u, "ingame_game", "ignoreBtn");
        click(&mut u, ib);
        let m = &u.sets[0][gm];
        assert!(m.items.iter().filter(|i| i.def.name == "ignore").all(|i| i.flags & WINDOW_VISIBLE != 0));
        assert!(m.items.iter().filter(|i| i.def.name == "vote").all(|i| i.flags & WINDOW_VISIBLE == 0));
        let il = u.sets[0][gm].items.iter().position(|i| u.feeders.get(&i.feeder) == Some(&Feeder::IgnoreList)).unwrap();
        let fid = u.sets[0][gm].items[il].feeder;
        assert_eq!(u.feeder_count(fid), 3);
        assert_eq!(u.feeder_item_text(fid, 0, 0).0, "Granger");
        // Bob ignores me
        assert_eq!(u.feeder_item_text(fid, 2, 2).0, "X");
        assert_eq!(u.feeder_item_text(fid, 2, 1).0, "");
        // select Bob, "Ignore Player"
        let lr = u.sets[0][gm].items[il].rect;
        let eh = u.sets[0][gm].items[il].def.element_height;
        click_at(&mut u, lr[0] + 20.0, lr[1] + 1.0 + eh * 2.5);
        assert_eq!(u.selection(Feeder::IgnoreList), 2);
        let ign = u.sets[0][gm].items.iter().position(|i| i.def.text.as_deref() == Some("Ignore Player")).unwrap();
        click(&mut u, IRef { set: 0, menu: gm, item: ign });
        assert_eq!(execs(&mut u), vec!["ignore 3\n".to_string()]);
        assert_eq!(u.feeder_item_text(fid, 2, 1).0, "X");
    }

    #[test]
    fn menus_help_and_leave() {
        if !have_data() {
            return;
        }
        let help = parse_help(&read("ui/help.txt").unwrap());
        assert!(help.len() > 5);
        assert_eq!(help[0].0, "^3Welcome^7");
        assert!(help[0].1.starts_with("^3Welcome^7\n\nIn this section"), "{:?}", &help[0].1[..40]);
        assert!(help[1].1.contains("\n\nWatch the health cross"));
        let mut u = ui(Team::Humans);
        u.open_ingame();
        let bar = u.find_menu("ingame").unwrap();
        let hb = u.sets[0][bar].items.iter().position(|i| i.def.text.as_deref() == Some("Help")).unwrap();
        click(&mut u, IRef { set: 0, menu: bar, item: hb });
        assert_eq!(u.top_menu(), Some("ingame_help"));
        let fid = u.it(mref(&u, "ingame_help", "list")).feeder;
        assert_eq!(u.feeder_count(fid), help.len() as i32);
        assert_eq!(u.feeder_item_text(fid, 1, 0).0, "Alien Healing");
        // UI_HELPINFOPANE paints the selected topic
        let cmds = u.paint();
        assert!(cmds.iter().any(|c| matches!(c, Cmd::Pic { img, .. } if img.starts_with("fonts/"))));
        // Exit -> ingame_leave -> Quit -> Yes: uiScript quit
        let exit = u.sets[0][bar].items.iter().position(|i| i.def.text.as_deref() == Some("Exit")).unwrap();
        click(&mut u, IRef { set: 0, menu: bar, item: exit });
        assert_eq!(u.top_menu(), Some("ingame_leave"));
        let lv = u.find_menu("ingame_leave").unwrap();
        let quit = u.sets[0][lv].items.iter().position(|i| i.def.text.as_deref() == Some("Quit")).unwrap();
        click(&mut u, IRef { set: 0, menu: lv, item: quit });
        let yes = u.sets[0][lv].items.iter().position(|i| i.def.name == "quitConfirm" && i.def.text.as_deref() == Some("Yes")).unwrap();
        assert!(u.sets[0][lv].items[yes].flags & WINDOW_VISIBLE != 0);
        click(&mut u, IRef { set: 0, menu: lv, item: yes });
        assert_eq!(execs(&mut u), vec!["quit".to_string()]);
    }

    #[test]
    fn menus_say_field() {
        if !have_data() {
            return;
        }
        let mut u = ui(Team::Humans);
        u.message_mode(false);
        assert_eq!(u.top_menu(), Some("say"));
        assert!(u.editing.is_some());
        for c in b"hi there" {
            u.key_event(K::Other, true);
            u.key_event(K::Char(*c), true);
        }
        u.key_event(K::Char(8), true);
        assert_eq!(u.cvar("ui_sayBuffer"), "hi ther");
        // the say field ignores arrows / tab
        u.key_event(K::Up, true);
        assert_eq!(u.top_menu(), Some("say"));
        let cmds = u.paint();
        assert!(!cmds.is_empty());
        u.key_event(K::Enter, true);
        assert_eq!(execs(&mut u), vec!["say hi ther\n".to_string()]);
        assert!(!u.is_open());
        // team chat; a leading '/' turns it into a command (say_command)
        u.message_mode(true);
        assert_eq!(u.top_menu(), Some("say_team"));
        for c in b"/callvote draw" {
            u.key_event(K::Char(*c), true);
        }
        assert_eq!(u.top_menu(), Some("say_command"));
        u.key_event(K::Enter, true);
        assert_eq!(execs(&mut u), vec!["callvote draw\n".to_string()]);
        u.message_mode(true);
        u.key_event(K::Char(b'x'), true);
        u.key_event(K::Enter, true);
        assert_eq!(execs(&mut u), vec!["say_team x\n".to_string()]);
    }

    #[test]
    fn menus_armoury_and_dialogs() {
        if !have_data() {
            return;
        }
        let mut u = ui(Team::Humans);
        u.view.player.stats[Stat::Weapon as usize] = WeaponId::Rifle as i32;
        u.menus_activate_by_name("tremulous_humanarmoury");
        let buys: Vec<String> = u.lists[&Feeder::ArmouryBuy].iter().map(|e| e.cmd.clone()).collect();
        assert!(buys.contains(&"buy larmour".to_string()) && !buys.contains(&"buy rifle".to_string()));
        let sells: Vec<String> = u.lists[&Feeder::ArmourySell].iter().map(|e| e.cmd.clone()).collect();
        assert_eq!(sells, vec!["sell blaster".to_string(), "sell rifle".to_string()]);
        // the info pane: name, bg_misc.c info, price
        let e = u.lists[&Feeder::ArmouryBuy].iter().find(|e| e.cmd == "buy larmour").unwrap().clone();
        let t = info_pane_text(&e, &u.view.player);
        assert!(t.starts_with("Light Armour\n\nProtective armour") && t.ends_with("Credits: 70"), "{t}");
        // Buy runs the selected entry, then waits for the carriage to change
        let buy = mref(&u, "tremulous_humanarmoury", "Buy");
        click(&mut u, buy);
        assert_eq!(execs(&mut u), vec![format!("cmd {}\n", u.lists[&Feeder::ArmouryBuy][0].cmd)]);
        assert!(matches!(u.capture, Capture::ArmouryRefresh));
        bg::add_upgrade(UpgradeId::Larmour as i32, &mut u.view.player.stats);
        u.paint();
        assert!(matches!(u.capture, Capture::None));
        assert!(u.lists[&Feeder::ArmourySell].iter().any(|e| e.cmd == "sell larmour"));
        // a refusal: CG_Menu opens the team dialog over the armoury
        let m = cg_menu("You don't have enough credits", false).unwrap();
        assert!(u.cg_menu(m));
        assert_eq!(u.top_menu(), Some("tremulous_human_dialog"));
        assert!(u.cvar("ui_dialog").starts_with("Insufficient funds."));
        u.paint();
        // OK closes it and the armoury is back on top
        let ok = u.find_menu("tremulous_human_dialog").unwrap();
        let okb = u.sets[0][ok].items.iter().position(|i| i.def.text.as_deref() == Some("OK")).unwrap();
        click(&mut u, IRef { set: 0, menu: ok, item: okb });
        assert_eq!(u.top_menu(), Some("tremulous_humanarmoury"));
        // cg_disableUpgradeDialogs suppresses ARMOURYEVOLVE dialogs
        u.set_cvar("cg_disableUpgradeDialogs", "1");
        assert!(!u.cg_menu(m));
        assert_eq!(cg_menu("Choose a class to spawn as", false), Some(CgMenu::Open("tremulous_alienclass")));
        assert_eq!(cg_menu("Join a team first", false), Some(CgMenu::Dialog(None, DialogType::Command)));
        assert_eq!(cg_menu("Unknown item foo", false), None);
    }

    #[test]
    fn menus_feeder_contents() {
        let base = Player { team: Team::Humans, class: ClassId::HumanBase as i32, credits: 0, stats: vec![0; 16], alien_stage: 0, human_stage: 0, vanilla: false };
        let with = |p: &Player, w: WeaponId| {
            let mut p = p.clone();
            p.stats[Stat::Team as usize] = p.team as i32;
            p.stats[Stat::Weapon as usize] = w as i32;
            p
        };
        let unarmed = with(&base, WeaponId::Blaster);
        let buys: Vec<String> = load_armoury_buys(&unarmed).into_iter().map(|e| e.cmd).collect();
        assert!(buys.contains(&"buy shotgun".to_string()) && buys.contains(&"buy rifle".to_string()), "{buys:?}");
        assert!(!buys.contains(&"buy lcannon".to_string()));
        let builds: Vec<String> = load_builds(&with(&base, WeaponId::Ckit), Team::Humans).into_iter().map(|e| e.cmd).collect();
        assert!(builds.contains(&"build telenode".to_string()) && builds.contains(&"build reactor".to_string()), "{builds:?}");
        let alien = Player { team: Team::Aliens, class: ClassId::Level0 as i32, ..base.clone() };
        assert!(load_alien_upgrades(&alien).is_empty());
        let rich = Player { credits: 10 * k::ALIEN_CREDITS_PER_KILL, ..alien.clone() };
        let up = load_alien_upgrades(&rich);
        let l1 = up.iter().find(|e| e.cmd == "class level1").expect("level1");
        // UI_DrawInfoPane: frags needed
        let t = info_pane_text(l1, &rich);
        assert!(t.contains("\n\nA support class able to crawl") && t.contains("Frags: "), "{t}");
        assert_eq!(load_alien_classes(&alien)[0].cmd, "class level0");
        let items = load_human_items(&base);
        assert_eq!(items.iter().map(|e| e.cmd.as_str()).collect::<Vec<_>>(), vec!["class rifle", "class ckit"]);
        assert!(info_pane_text(&items[0], &base).ends_with("Credits: Free"));
        let b = load_builds(&with(&base, WeaponId::Ckit), Team::Humans).into_iter().find(|e| e.cmd == "build reactor").unwrap();
        assert!(info_pane_text(&b, &base).contains("All structures except the telenode rely on a reactor"));
    }

    /// A Tremulous 1.1.0 game (dec041): 1.1.0 ui_main.c's lists over the
    /// 1.1 tables, with the hovel, the advanced construction kit, 1.1 prices
    /// and frag costs.
    #[test]
    fn menus_feeder_contents_vanilla() {
        let base = Player { team: Team::Humans, class: ClassId::HumanBase as i32, credits: 0, stats: vec![0; 16], alien_stage: 0, human_stage: 0, vanilla: true };
        let with = |p: &Player, w: WeaponId| {
            let mut p = p.clone();
            p.stats[Stat::Team as usize] = p.team as i32;
            p.stats[Stat::Weapon as usize] = w as i32;
            p
        };
        let cmds = |v: Vec<Entry>| v.into_iter().map(|e| e.cmd).collect::<Vec<_>>();
        assert_eq!(cmds(load_human_items(&base)), vec!["class rifle", "class ckit"]);
        let s2 = Player { human_stage: 1, alien_stage: 2, ..base.clone() };
        assert_eq!(cmds(load_human_items(&s2)), vec!["class rifle", "class ackit"]);
        // the advanced kit builds the defence computer, the kit does not
        let ackit = cmds(load_builds(&with(&s2, WeaponId::Ackit), Team::Humans));
        let ckit = cmds(load_builds(&with(&s2, WeaponId::Ckit), Team::Humans));
        assert!(ackit.contains(&"build dcc".to_string()) && !ckit.contains(&"build dcc".to_string()), "{ackit:?} {ckit:?}");
        // the hovel for grangers at stage 3
        let granger = Player { team: Team::Aliens, class: ClassId::Builder as i32, ..s2.clone() };
        let builds = cmds(load_builds(&with(&granger, WeaponId::Abuild), Team::Aliens));
        assert!(builds.contains(&"build hovel".to_string()), "{builds:?}");
        assert!(!cmds(load_builds(&with(&Player { alien_stage: 1, ..granger.clone() }, WeaponId::Abuild), Team::Aliens)).contains(&"build hovel".to_string()));
        // 1.1's armoury: "retrigger", its prices (pulse rifle 400, legacy 450)
        let buys = load_armoury_buys(&with(&s2, WeaponId::Blaster));
        let prifle = buys.iter().find(|e| e.cmd == "buy prifle retrigger").expect("prifle");
        assert!(info_pane_text(prifle, &s2).ends_with("Credits: 400"), "{}", info_pane_text(prifle, &s2));
        // evolving costs 1.1 frags (one frag is ALIEN_CREDITS_PER_KILL legacy credits)
        let dretch = Player { team: Team::Aliens, class: ClassId::Level0 as i32, credits: k::ALIEN_CREDITS_PER_KILL, ..base.clone() };
        let up = load_alien_upgrades(&dretch);
        let l1 = up.iter().find(|e| e.cmd == "class level1").expect("level1");
        assert!(info_pane_text(l1, &dretch).ends_with("Frags: 1"), "{}", info_pane_text(l1, &dretch));
    }

    #[test]
    fn menus_text_wrap() {
        if !have_data() {
            return;
        }
        let f = fonts();
        let text = "^3Welcome^7 to the wrapped text block of the legacy ui, which breaks lines at blanks.\n\nForced break.";
        let w = item_text_wrap(&f, text, 0.33, 150.0, 1.0);
        let lines: Vec<&str> = w.split('\n').collect();
        assert!(lines.len() > 4, "{w:?}");
        assert!(w.ends_with('\n'));
        for l in &lines {
            assert!(f.width(l, 0.33, 1.0) <= 150.0 + 12.0, "{l:?}");
        }
        // the colour of a broken line carries over
        assert!(lines[1].starts_with("^7") || lines[0].contains("^7"));
        assert!(w.contains("\n\nForced break.\n"));
        assert_eq!(clean_str("^1Gr^7anger\x01"), "Granger");
    }

    #[test]
    fn menus_bind_items() {
        if !have_data() {
            return;
        }
        use crate::keys::{key_string_to_keynum as kn, Keys, K_ENTER, K_MOUSE1};
        let mut u = ui(Team::Humans);
        let mut keys = Keys::default();
        for (k, c) in [("x", "+button8"), ("SHIFT", "+speed"), ("MOUSE1", "+attack"), ("CTRL", "+attack")] {
            keys.set_binding(kn(k), c);
        }
        u.key_bindings = keys.bindings().to_vec();
        u.controls_get_config();
        assert_eq!(u.binding_from_name("+attack"), "CTRL or MOUSE1");
        assert_eq!(u.binding_from_name("+button6"), "???");
        let m = u.find_menu("ingame_options").unwrap();
        let ii = u.sets[0][m].items.iter().position(|it| it.ty == IT_BIND && it.def.cvar.as_deref() == Some("+button8")).unwrap();
        let r = IRef { set: 0, menu: m, item: ii };
        let rect = u.it(r).rect;
        // Enter on the item: waiting for a key
        u.cursor = (rect[0] + 1.0, rect[1] + 1.0);
        u.keynum = K_ENTER;
        assert!(u.bind_handle_key(r, K::Enter, true));
        assert!(u.key_bind_pending());
        assert_eq!(u.owner_draw_text(*u.ods.iter().find(|(_, o)| **o == Od::KeyBindStatus).unwrap().0).unwrap(), "Waiting for new key... Press ESCAPE to cancel");
        // k becomes the second key
        u.cursor = (0.0, 0.0);
        u.keynum = b'k' as i32;
        u.bind_handle_key(r, K::Other, true);
        assert!(!u.key_bind_pending());
        assert_eq!(u.binding_from_name("+button8"), "X or K");
        let outs: Vec<Out> = u.out.drain(..).collect();
        assert!(outs.contains(&Out::Bind(b'k' as i32, "+button8".into())));
        assert!(outs.contains(&Out::Exec("in_restart\n".into())));
        assert_eq!(u.key_bindings[b'k' as usize], "+button8");
        // a third key replaces both
        u.waiting_for_key = true;
        u.keynum = b'j' as i32;
        u.bind_handle_key(r, K::Other, true);
        assert_eq!(u.binding_from_name("+button8"), "J");
        assert_eq!(u.key_bindings[b'x' as usize], "");
        // a key taken from another command
        u.waiting_for_key = true;
        u.keynum = K_MOUSE1;
        u.bind_handle_key(r, K::Mouse1, true);
        assert_eq!(u.binding_from_name("+button8"), "J or MOUSE1");
        assert_eq!(u.binding_from_name("+attack"), "CTRL");
        // Escape cancels, char events are ignored
        u.waiting_for_key = true;
        u.keynum = b'q' as i32;
        u.bind_handle_key(r, K::Char(b'q'), true);
        assert!(u.key_bind_pending());
        u.keynum = crate::keys::K_ESCAPE;
        u.bind_handle_key(r, K::Escape, true);
        assert!(!u.key_bind_pending());
        assert_eq!(u.binding_from_name("+button8"), "J or MOUSE1");
        // Backspace clears the command's keys (the binds stay, as in the C)
        u.waiting_for_key = true;
        u.keynum = crate::keys::K_BACKSPACE;
        u.bind_handle_key(r, K::Backspace, true);
        assert_eq!(u.binding_from_name("+button8"), "???");
        // defaults (Controls_SetDefaults): x sprint, z dodge
        u.controls_set_defaults();
        assert_eq!(u.binding_from_name("+button8"), "X");
        assert_eq!(u.binding_from_name("+button6"), "Z");
        assert_eq!(u.binding_from_name("vote no"), "F2");
    }

    /// wt-b: joystick keys in the ui: Item_Bind_HandleKey binds K_JOY1 like
    /// any key; Menu_HandleKey has no default action for K_JOY1-4 (the
    /// focus stays), the stick's arrow keys move it.
    #[test]
    fn menus_joystick_keys() {
        if !have_data() {
            return;
        }
        use crate::keys::{key_string_to_keynum as kn, K_DOWNARROW, K_JOY1};
        const K_JOY4: i32 = K_JOY1 + 3;
        let mut u = ui(Team::Humans);
        u.key_bindings = crate::keys::Keys::default().bindings().to_vec();
        u.controls_get_config();
        let m = u.find_menu("ingame_options").unwrap();
        let ii = u.sets[0][m].items.iter().position(|it| it.ty == IT_BIND && it.def.cvar.as_deref() == Some("+button8")).unwrap();
        let r = IRef { set: 0, menu: m, item: ii };
        u.waiting_for_key = true;
        u.bind_item = Some(r);
        u.keynum = K_JOY1;
        u.bind_handle_key(r, k_from_keynum(K_JOY1), true);
        assert!(!u.key_bind_pending());
        assert_eq!(u.binding_from_name("+button8"), "JOY1");
        assert_eq!(u.key_bindings[kn("JOY1") as usize], "+button8");
        // the main menu: K_JOY4 does nothing, the stick's down arrow moves the focus
        u.menus_activate_by_name("main");
        let mi = u.menu_get_focused().unwrap();
        let focus = |u: &Ui| u.sets[0][mi].items.iter().position(|it| it.flags & WINDOW_HASFOCUS != 0);
        let before = focus(&u);
        u.key_event_num(K_JOY4, true);
        u.key_event_num(K_JOY4, false);
        assert_eq!(focus(&u), before);
        u.key_event_num(K_DOWNARROW, true);
        u.key_event_num(K_DOWNARROW, false);
        assert_ne!(focus(&u), before);
    }

    #[test]
    fn menus_scoreboard() {
        if !have_data() {
            return;
        }
        let mut u = ui(Team::Humans);
        u.view.scores = Scoreboard {
            rows: vec![
                ScoreRow { client: 1, team: Team::Humans, score: 7, time: 3, ping: 0, weapon: WeaponId::Rifle as i32, upgrade: Some(UpgradeId::Larmour) },
                ScoreRow { client: 0, team: Team::Aliens, score: 2, time: 1, ping: -1, weapon: WeaponId::Level1upg as i32, upgrade: None },
            ],
            names: (0..4).map(|i| format!("p{i}")).collect(),
            my_team: Team::Humans,
            ready: vec![false; 4],
            alien_next: 400,
            alien_credits: 0,
            human_next: 100,
            human_credits: 99,
            spectators: "^7spec     ".into(),
            ..Default::default()
        };
        let sc = &u.sets[1][0];
        let hl = sc.items.iter().find(|i| i.def.name == "rightlist").unwrap().feeder;
        let al = sc.items.iter().find(|i| i.def.name == "leftlist").unwrap().feeder;
        assert_eq!(u.feeder_count(hl), 1);
        assert_eq!(u.feeder_item_text(hl, 0, 3).0, "^7p1");
        assert_eq!(u.feeder_item_text(hl, 0, 4).0, "7");
        assert_eq!(u.feeder_item_text(hl, 0, 5).0, "   3");
        assert_eq!(u.feeder_item_text(hl, 0, 1).1.as_deref(), Some("icons/iconu_larmour"));
        // enemy team: no icons (not spectating); ping -1 shows nothing
        assert_eq!(u.feeder_item_text(al, 0, 1).1, None);
        assert_eq!(u.feeder_item_text(al, 0, 6).0, "");
        u.show_scores = true;
        let cmds = u.paint();
        assert!(cmds.iter().any(|c| matches!(c, Cmd::Fill { .. })));
        // CG_SetScoreSelection sets the team count as selection, then
        // Menu_Update clamps it to the team's last row
        let rl = u.sets[1][0].items.iter().find(|i| i.def.name == "rightlist").unwrap();
        assert_eq!(rl.cursor_pos, 0);
        let ll = u.sets[1][0].items.iter().find(|i| i.def.name == "leftlist").unwrap();
        assert_eq!(ll.cursor_pos, 0);
    }

    #[test]
    fn menus_options_cvars() {
        if !have_data() {
            return;
        }
        let mut u = ui(Team::Humans);
        u.set_aspect(0.75);
        u.open_ingame();
        let opt = mref(&u, "ingame", "options");
        click(&mut u, opt);
        assert_eq!(u.top_menu(), Some("ingame_options"));
        let om = u.find_menu("ingame_options").unwrap();
        // the HUD tab: cg_drawCrosshair is an ITEM_TYPE_MULTI over cvarFloatList
        let hud_btn = mref(&u, "ingame_options", "hudBtn");
        click(&mut u, hud_btn);
        let ii = u.sets[0][om].items.iter().position(|i| i.def.cvar.as_deref() == Some("cg_drawCrosshair")).unwrap();
        let r = IRef { set: 0, menu: om, item: ii };
        assert!(u.it(r).flags & WINDOW_VISIBLE != 0);
        assert_eq!(u.cvar("cg_drawCrosshair"), "2");
        assert_eq!(u.multi_setting(r), "Always");
        u.paint();
        click(&mut u, r);
        assert_eq!(u.cvar("cg_drawCrosshair"), "0");
        assert_eq!(u.multi_setting(r), "Never");
        assert!(u.out.iter().any(|o| *o == Out::Cvar("cg_drawCrosshair".into(), "0".into())));
        // every tab paints (binds, sliders, yes/no, the resolution combobox)
        for b in ["PlayerBtn", "controlsBtn", "systemBtn"] {
            let r = mref(&u, "ingame_options", b);
            click(&mut u, r);
            assert!(!u.paint().is_empty());
        }
        // no bind table snapshot here: every command shows "???"
        assert_eq!(u.binding_from_name("+attack"), "???");
    }

    fn by_text(u: &Ui, menu: &str, text: &str) -> IRef {
        let m = u.find_menu(menu).unwrap_or_else(|| panic!("menu {menu}"));
        let i = u.sets[0][m].items.iter().position(|it| it.def.text.as_deref().is_some_and(|t| t.eq_ignore_ascii_case(text))).unwrap_or_else(|| panic!("item {text}"));
        IRef { set: 0, menu: m, item: i }
    }
    fn outs(u: &mut Ui) -> Vec<Out> {
        u.out.drain(..).collect()
    }

    #[test]
    fn menus_main_menu_and_splash_model() {
        if !have_data() {
            return;
        }
        let mut u = ui(Team::None);
        assert!(!u.is_open(), "UI_Init closes every menu (main.menu is visible)");
        u.set_active_menu(UiMenuCommand::Main);
        assert_eq!(u.top_menu(), Some("main"));
        assert!(u.is_fullscreen());
        // onOpen: uiScript stopRefresh ; playlooped "sound/ui/heartbeat.wav"
        let o = outs(&mut u);
        assert!(o.contains(&Out::BackgroundTrack("sound/ui/heartbeat.wav".into(), "sound/ui/heartbeat.wav".into())), "{o:?}");
        let cmds = u.paint();
        // the background, then the ITEM_TYPE_MODEL splash in its rect inset by a pixel
        assert!(matches!(&cmds[0], Cmd::Pic { img, .. } if img == "ui/assets/mainmenu.jpg"));
        let m = cmds.iter().find_map(|c| if let Cmd::Model { asset, r, fov, angle, .. } = c { Some((asset.clone(), *r, *fov, *angle)) } else { None }).expect("model");
        assert_eq!(m.0, "models/splash/splash_screen.md3");
        assert_eq!(m.1, [1.0, 1.0, 638.0, 478.0]);
        assert_eq!((m.2, m.3), ([32.0, 24.0], 180.0));
        // the buttons draw after it; Esc on the fullscreen main runs onEsc
        u.key_event(K::Escape, true);
        assert_eq!(u.top_menu(), Some("quit_popmenu"));
        // YES -> quitCredit -> a click quits
        let r_ = by_text(&u, "quit_popmenu", "YES");
        click(&mut u, r_);
        assert_eq!(u.top_menu(), Some("quitCredit"));
        u.out.clear();
        let r_ = mref(&u, "quitCredit", "exitclickbox");
        click(&mut u, r_);
        assert!(execs(&mut u).contains(&"quit".to_string()));
    }

    #[test]
    fn menus_create_server_flow() {
        if !have_data() {
            return;
        }
        let mut u = ui(Team::None);
        u.fs_files = vec!["levelshots/atcs.jpg".into(), "video/splash.roq".into()];
        u.set_active_menu(UiMenuCommand::Main);
        // Play -> joinserver -> Create Server
        let r_ = by_text(&u, "main", "Play");
        click(&mut u, r_);
        assert_eq!(u.top_menu(), Some("joinserver"));
        let r_ = mref(&u, "joinserver", "createServer");
        click(&mut u, r_);
        assert_eq!(u.top_menu(), Some("createserver"));
        // loadArenas: FEEDER_MAPS sorted by long name, selection 0
        let list = mref(&u, "createserver", "maplist");
        let f = u.it(list).feeder;
        assert_eq!(u.feeder_count(f), 2);
        assert_eq!(u.feeder_item_text(f, 0, 0).0, "ATCS");
        assert_eq!(u.feeder_item_text(f, 1, 0).0, "Niveus");
        assert_eq!(u.feeder_item_image(f, 1).as_deref(), Some("levelshots/niveus"));
        assert_eq!(u.cvar("ui_selectedMap"), "0");
        // pick Niveus: the preview is its levelshot (no niveus.roq)
        let rect = u.it(list).rect;
        let eh = u.it(list).def.element_height;
        click_at(&mut u, rect[0] + 30.0, rect[1] + 1.0 + eh * 1.5);
        assert_eq!(u.cvar("ui_selectedMap"), "1");
        let cmds = u.paint();
        assert!(cmds.iter().any(|c| matches!(c, Cmd::Pic { img, .. } if img == "levelshots/niveus")));
        assert!(!cmds.iter().any(|c| matches!(c, Cmd::Cinematic { .. })));
        // the server options are cvars: Time Limit
        u.set_cvar("timelimit", "20");
        u.out.clear();
        u.run_menu_script("StartServer", None);
        let e = execs(&mut u);
        assert_eq!(e, vec!["wait ; wait ; map niveus\n".to_string()]);
        assert_eq!(u.cvar("dedicated"), "0");
    }

    #[test]
    fn menus_server_browser_offline() {
        if !have_data() {
            return;
        }
        let mut u = ui(Team::None);
        u.local_time = [9, 2, 2026, 7, 5];
        u.unix_time = 1_790_000_000;
        u.set_active_menu(UiMenuCommand::Main);
        u.out.clear();
        let r_ = by_text(&u, "main", "Play");
        click(&mut u, r_);
        // the legend's "\n" is a newline (PC_String_Parse escapes)
        let lg = mref(&u, "joinserver", "legend");
        assert_eq!(u.it(lg).def.text.as_deref(), Some("[official] = Official Server\n[featured] = Featured Server"));
        // InitServerList 3: AS_GLOBAL asks the master (no network: nothing comes)
        let e = execs(&mut u);
        assert!(e.iter().any(|x| x.starts_with("globalservers 0 full empty")), "{e:?}");
        assert_eq!(u.browser.sort_key, 3);
        assert!(u.browser.refresh_active);
        let od = u.ods.iter().find(|(_, o)| **o == Od::ServerRefreshDate).map(|(i, _)| *i).unwrap();
        assert!(u.owner_draw_text(od).unwrap().starts_with("Getting info for 0 servers (ESC to cancel)"));
        let fs = u.feeders.iter().find(|(_, f)| **f == Feeder::Servers).map(|(i, _)| *i).unwrap();
        assert_eq!(u.feeder_count(fs), 0);
        // UI_DoServerRefresh stops after the 5 s refresh time
        let v = u.view.clone();
        u.frame(7000, v);
        assert!(!u.browser.refresh_active);
        assert_eq!(u.owner_draw_text(od).unwrap(), "Refresh Time: Oct-2, 2026 at 7:05");
        assert_eq!(u.cvar("ui_lastServerRefresh_0_time"), "1790000000");
        // Source: Internet -> LAN (Mplayer skipped) -> Favorites
        let src = u.ods.iter().find(|(_, o)| **o == Od::NetSource).map(|(i, _)| *i).unwrap();
        assert_eq!(u.owner_draw_text(src).unwrap(), "Internet");
        assert!(u.net_source_handle_key(K::Mouse1));
        assert_eq!(u.owner_draw_text(src).unwrap(), "LAN");
        assert!(execs(&mut u).iter().any(|x| x.starts_with("localservers")));
        u.net_source_handle_key(K::Mouse1);
        assert_eq!(u.owner_draw_text(src).unwrap(), "Favorites");
        // createFavorite: favorites list without pings shows the address
        u.set_cvar("ui_favoriteName", "my server");
        u.set_cvar("ui_favoriteAddress", "10.0.0.2:30720");
        u.run_menu_script("CreateFavorite", None);
        assert!(u.lan.favorites_changed);
        u.run_menu_script("UpdateFilter", None);
        assert_eq!(u.feeder_count(fs), 1);
        assert_eq!(u.feeder_item_text(fs, 0, 0).0, "10.0.0.2:30720");
        assert_eq!(u.feeder_item_text(fs, 0, 4).0, "...");
        u.out.clear();
        u.run_menu_script("JoinServer", None);
        assert_eq!(execs(&mut u), vec!["connect 10.0.0.2:30720\n".to_string()]);
        u.run_menu_script("DeleteFavorite", None);
        u.run_menu_script("UpdateFilter", None);
        assert_eq!(u.feeder_count(fs), 0);
        // closeJoin: the LAN refresh never ends (no answers): the first
        // stops it, the second goes back to main
        assert!(u.browser.refresh_active);
        u.run_menu_script("closeJoin", None);
        assert!(!u.browser.refresh_active);
        assert_eq!(u.top_menu(), Some("joinserver"));
        u.run_menu_script("closeJoin", None);
        assert_eq!(u.top_menu(), Some("main"));
        // find a friend: no servers to ask
        u.set_cvar("ui_findPlayer", "^1Bob");
        u.run_menu_script("FindPlayer", None);
        assert_eq!(u.browser.found_names, vec!["0 servers found with player Bob".to_string()]);
        // news: no cURL in this port
        u.run_menu_script("getNews", None);
        // (wrapped at 325 * aspectScale like UI_UpdateNews)
        assert!(!u.browser.news.is_empty());
        assert!(u.browser.news.concat().contains("CURL support"), "{:?}", u.browser.news);
    }

    #[test]
    fn menus_server_browser_protocols() {
        // dec040: the browser tags vanilla 1.1 / legacy 1.3 servers, filters
        // them (ui_browserProtocol, the joinserver menu's "Version:") and
        // joins each with its own protocol
        if !have_data() {
            return;
        }
        // the menus as re/source/assets/ui has them (packed into
        // data-legacy-assets.pk3), the rest from the data folder
        let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../re/source/assets");
        let mut u = ui_from(Team::None, &mut |p: &str| std::fs::read_to_string(repo.join(p)).ok().or_else(|| read(p)));
        let vp = mref(&u, "joinserver", "viewProtocol");
        assert_eq!(u.it(vp).def.cvar.as_deref(), Some("ui_browserProtocol"));
        let srv = |addr: &str, name: &str, protocol: i32| ServerInfo { addr: addr.into(), host_name: name.into(), map_name: "atcs".into(), game: "base".into(), clients: 1, max_clients: 8, ping: 40, visible: true, protocol };
        u.lan.favorites = vec![srv("10.0.0.1:30720", "vanilla", 69), srv("10.0.0.2:30720", "legacy", 71)];
        u.set_cvar("ui_netSource", &AS_FAVORITES.to_string());
        let fs = u.feeders.iter().find(|(_, f)| **f == Feeder::Servers).map(|(i, _)| *i).unwrap();
        u.run_menu_script("UpdateFilter", None);
        assert_eq!(u.feeder_count(fs), 2);
        let games: Vec<String> = (0..2).map(|i| u.feeder_item_text(fs, i, 1).0).collect();
        assert!(games.contains(&"base 1.1".to_string()) && games.contains(&"base 1.3".to_string()), "{games:?}");
        for (want, name) in [("69", "vanilla"), ("71", "legacy")] {
            u.set_cvar("ui_browserProtocol", want);
            for s in u.lan.favorites.iter_mut() {
                s.visible = true;
            }
            u.run_menu_script("UpdateFilter", None);
            assert_eq!(u.feeder_count(fs), 1, "filter {want}");
            assert_eq!(u.feeder_item_text(fs, 0, 0).0, name);
            u.browser.current_server = 0;
            u.out.clear();
            u.run_menu_script("JoinServer", None);
            let addr = if want == "69" { "10.0.0.1:30720" } else { "10.0.0.2:30720" };
            assert_eq!(execs(&mut u), vec![format!("connect {addr} -{want}\n")]);
        }
        // the options menu's network mode
        let np = by_text(&u, "simple_options", "Server Protocol:");
        assert_eq!(u.it(np).def.cvar.as_deref(), Some("cl_netProtocol"));
    }

    #[test]
    fn menus_mods_demos_movies_resolutions() {
        if !have_data() {
            return;
        }
        let mut u = ui(Team::None);
        u.fs_files = vec!["video/splash.roq".into(), "video/intro.RoQ".into(), "demos/duel.dm_71".into(), "demos/old.dm_68".into(), "maps/atcs.bsp".into()];
        u.mod_dirs = vec![("mymod".into(), "My Mod".into())];
        u.run_menu_script("loadMovies", None);
        assert_eq!(u.movies, vec!["SPLASH".to_string(), "INTRO".to_string()]);
        u.run_menu_script("LoadDemos", None);
        assert_eq!(u.demos, vec!["duel".to_string()]);
        u.run_menu_script("loadMods", None);
        let fm = u.feeders.iter().find(|(_, f)| **f == Feeder::Mods).map(|(i, _)| *i).unwrap();
        assert_eq!(u.feeder_item_text(fm, 0, 0).0, "My Mod");
        u.out.clear();
        u.run_menu_script("RunMod", None);
        let o = outs(&mut u);
        assert!(o.contains(&Out::Cvar("fs_game".into(), "mymod".into())) && o.contains(&Out::Exec("vid_restart;".into())), "{o:?}");
        u.run_menu_script("RunDemo", None);
        assert_eq!(execs(&mut u), vec!["demo duel\n".to_string()]);
        u.run_menu_script("playMovie", None);
        assert_eq!(execs(&mut u), vec!["cinematic SPLASH.roq 2\n".to_string()]);
        // FEEDER_RESOLUTIONS from r_availableModes; r_width x r_height not in
        // the list is the extra "Custom" row
        u.set_cvar("r_availableModes", "1280x720 1920x1080 1024x768");
        u.parse_resolutions();
        assert_eq!(u.resolutions, vec![(1280, 720), (1920, 1080), (1024, 768)]);
        let fr = u.feeders.iter().find(|(_, f)| **f == Feeder::Resolutions).map(|(i, _)| *i).unwrap();
        u.set_cvar("r_width", "800");
        u.set_cvar("r_height", "600");
        assert_eq!(u.feeder_count(fr), 4);
        assert_eq!(u.feeder_item_text(fr, 1, 0).0, "1920x1080 (16:9)");
        assert_eq!(u.feeder_item_text(fr, 3, 0).0, "Custom (800x600)");
        u.feeder_selection(fr, 2);
        assert_eq!((u.cvar("r_width"), u.cvar("r_height")), ("1024".to_string(), "768".to_string()));
        assert_eq!(u.feeder_initialise(fr), 2);
        assert_eq!(u.feeder_count(fr), 3);
        assert_eq!(display_aspect_string(1680, 1050), "16:10");
        assert_eq!(display_aspect_string(1024, 768), "4:3");
        // ui_glCustom presets (UI_Update)
        u.set_cvar("ui_glCustom", "3");
        u.ui_update("ui_glCustom");
        assert_eq!(u.cvar("r_picmip"), "2");
        assert_eq!(u.cvar("r_texturemode"), "GL_LINEAR_MIPMAP_NEAREST");
        u.set_cvar("ui_glCustom", "0");
        u.ui_update("ui_glCustom");
        assert_eq!((u.cvar("r_picmip").as_str(), u.cvar("r_texturemode").as_str()), ("0", "GL_LINEAR_MIPMAP_LINEAR"));
    }

    #[test]
    fn menus_fades_transitions_orbits() {
        if !have_data() {
            return;
        }
        // Fade: clamp 1.0, cycle 1 ms, 0.1 a step (main.menu's assetGlobalDef)
        let (mut flags, mut a, mut next) = (WINDOW_VISIBLE | WINDOW_FADINGIN, 0.0f32, 0);
        for t in 1..=12 {
            fade(&mut flags, &mut a, 1.0, &mut next, 1, true, 0.1, t * 2);
        }
        assert_eq!(a, 1.0);
        assert_eq!(flags & WINDOW_FADINGIN, 0, "done fading in");
        flags |= WINDOW_FADINGOUT;
        for t in 30..60 {
            fade(&mut flags, &mut a, 1.0, &mut next, 1, true, 0.1, t * 2);
        }
        assert_eq!(flags & (WINDOW_VISIBLE | WINDOW_FADINGOUT), 0, "faded out and hidden");
        let mut r = |p: &str| read(p);
        assert_eq!(scan_asset_fades(&read("ui/main.menu").unwrap(), &mut r), Some((0.1, 1.0, 1)));
        let mut u = ui(Team::None);
        u.asset_fade = (0.1, 1.0, 1);
        u.apply_asset_fades();
        let mi = u.find_menu("main").unwrap();
        assert_eq!(u.sets[0][mi].fade, (0.1, 1.0, 1));
        u.set_active_menu(UiMenuCommand::Main);
        // fadeout / fadein scripts (Menu_FadeItemByName on the group)
        let s: Script = vec![vec!["fadeout".into(), "copyright".into()]];
        u.run_script(0, mi, None, &s);
        let c = mref(&u, "main", "copyright");
        assert_ne!(u.it(c).flags & WINDOW_FADINGOUT, 0);
        for t in 0..40 {
            u.real_time = 2000 + t * 5;
            u.paint();
        }
        assert_eq!(u.it(c).flags & WINDOW_VISIBLE, 0, "the text faded away");
        let s: Script = vec![vec!["fadein".into(), "copyright".into()]];
        u.run_script(0, mi, None, &s);
        assert_ne!(u.it(c).flags & WINDOW_VISIBLE, 0);
        // transition copyright from 0 0 10 10 to 100 50 30 20 in 10 steps of 5 ms
        let s: Script = vec!["transition copyright 0 0 10 10 100 50 30 20 5 10".split(' ').map(String::from).collect()];
        u.run_script(0, mi, None, &s);
        assert_ne!(u.it(c).flags & WINDOW_INTRANSITION, 0);
        assert_eq!(u.it(c).rect_client, [0.0, 0.0, 10.0, 10.0]);
        assert_eq!(u.it(c).rect_effects2, [10.0, 5.0, 2.0, 1.0]);
        for t in 0..30 {
            u.real_time = 3000 + t * 6;
            u.paint();
        }
        assert_eq!(u.it(c).rect_client, [100.0, 50.0, 30.0, 20.0]);
        assert_eq!(u.it(c).flags & WINDOW_INTRANSITION, 0);
        // orbit: 3 degrees a step around (cx, cy) keeps the distance
        let s: Script = vec!["orbit copyright 200 100 320 240 5".split(' ').map(String::from).collect()];
        u.run_script(0, mi, None, &s);
        let d0 = ((u.it(c).rect_client[0] + 15.0 - 320.0).powi(2) + (u.it(c).rect_client[1] + 10.0 - 240.0).powi(2)).sqrt();
        for t in 0..10 {
            u.real_time = 4000 + t * 6;
            u.paint();
        }
        let rc = u.it(c).rect_client;
        let d1 = ((rc[0] + 15.0 - 320.0).powi(2) + (rc[1] + 10.0 - 240.0).powi(2)).sqrt();
        assert!((d0 - d1).abs() < 0.01 && rc[0] != 200.0, "orbited: {rc:?}");
        assert_ne!(u.it(c).flags & WINDOW_ORBITING, 0);
    }
}
