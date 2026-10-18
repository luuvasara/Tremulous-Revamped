//! The cgame HUD of Tremulous (legacy): the menu-script HUDs of `ui/hud.txt`
//! (tremulous_human_hud / alien_builder_hud / alien_general_hud /
//! default_hud .menu, which include tremulous_common_hud.h) drawn by a
//! `TremHud` Control in cgame's 640x480 virtual screen.
//!
//! * [`parse_menus`] reads the `.menu` language of legacy ui_shared.c
//!   (`menuDef` / `itemDef` blocks, botlib `#include` / `#define` / `enum`,
//!   PC_Expression_Parse arithmetic) from menudef.h and the HUD files.
//! * `TremHud` picks the menu as `CG_Draw2D` does (the viewed client's class
//!   `hudName` when alive and not spectating, else `default_hud`), applies
//!   `Menu_AspectCompensate` for the window's aspectScale, paints it as
//!   `Menu_Paint` / `Item_Paint` / `Window_Paint` do, lays text out as
//!   `Item_SetTextExtents` / `Item_Text_Wrapped_Paint` with the original
//!   bitmap fonts (`UI_Text_Paint`), and implements the ownerdraws of
//!   cg_draw.c, cg_weapons.c (item select), cg_scanner.c and cg_tutorial.c,
//!   plus `CG_DrawBuildableStatus` (cg_buildable.c).
//!
//! Each frame is built in two phases: the simulation is read through a
//! shared `bind()` of TremGame into a list of draw commands naming images,
//! then the names are resolved through the original-data `Library`
//! (`bind_mut()`), and `draw()` replays the list.
use std::collections::HashMap;

use godot::classes::control::{LayoutPreset, MouseFilter};
use godot::classes::{Control, IControl, Texture2D};
use godot::prelude::*;

use crate::game::TremGame;
use crate::menus::{parse_font_dat, Glyph, QFont};
use crate::models::Library;
use trem_sim::gen::buildables::BuildableId;
use trem_sim::gen::classes::ClassId;
use trem_sim::gen::constants as k;
use trem_sim::gen::enums::{ability, contents, Pers, Stat, Team};
use trem_sim::gen::upgrades::UpgradeId;
use trem_sim::gen::weapons::WeaponId;
use trem_sim::kernel::bg;
use trem_sim::kernel::qmath::{angle_vectors, cross, dot, length, ma, normalize, project_point_on_plane, rotate_point_around_vector, sub, Vec3};
use trem_sim::kernel::types::PlayerState;
use trem_sim::kernel::world::{EType, World, MAX_CLIENTS};

// =====================================================================
// .menu parsing (ui_shared.c subset)
// =====================================================================

// legacy menudef.h enum values (ITEM_TYPE_NONE is 0: an item without a
// `type` paints no text, as in the legacy Item_Paint)
pub const ITEM_TYPE_TEXT: i32 = 1;
pub const ITEM_TYPE_BUTTON: i32 = 2;
#[allow(dead_code)]
pub const ITEM_TYPE_LISTBOX: i32 = 8;
pub const ITEM_TYPE_OWNERDRAW: i32 = 11;
pub const WINDOW_STYLE_FILLED: i32 = 1;
pub const WINDOW_STYLE_SHADER: i32 = 3;
pub const WINDOW_BORDER_FULL: i32 = 1;
pub const ITEM_ALIGN_LEFT: i32 = 0;
pub const ITEM_ALIGN_CENTER: i32 = 1;
pub const ITEM_ALIGN_RIGHT: i32 = 2;

/// A menu script (`action { play "x"; uiScript Foo; close m }`): one
/// command (keyword + arguments) per `;`-separated statement.
pub type Script = Vec<Vec<String>>;

/// `columnInfo_t`: pos, width, maxChars, align.
pub type Column = [i32; 4];

/// `itemDef`: the window/item fields of ui_shared.c the HUD and the
/// in-game menus (team select, spawn, armoury, build, dialogs, scores) set.
#[derive(Clone, Debug)]
pub struct ItemDef {
    pub name: String,
    pub rect: [f32; 4],
    pub visible: bool,
    pub style: i32,
    pub item_type: i32,
    pub fore: [f32; 4],
    /// WINDOW_FORECOLORSET
    pub fore_set: bool,
    pub back: [f32; 4],
    /// backcolor given in the script (no ui_shared flag; used to pick the
    /// 1.1-era HH/AH alpha pair for ownerdraws that lerp back->fore).
    pub back_set: bool,
    pub background: Option<String>,
    pub ownerdraw: i32,
    pub align: i32,
    pub text_align_x: f32,
    pub text_align_y: f32,
    pub text_scale: f32,
    pub text: Option<String>,
    pub cvar: Option<String>,
    /// `textalign` (item->textalignment; `align` is item->alignment).
    pub text_align: i32,
    pub text_style: i32,
    pub border: i32,
    pub border_size: f32,
    pub border_color: [f32; 4],
    pub outline_color: [f32; 4],
    /// WINDOW_DECORATION: never takes focus.
    pub decoration: bool,
    /// WINDOW_AUTOWRAPPED / WINDOW_WRAPPED
    pub autowrapped: bool,
    pub wrapped: bool,
    pub not_selectable: bool,
    /// listbox `feeder` id (item->special)
    pub feeder: f32,
    pub element_width: f32,
    pub element_height: f32,
    pub element_type: i32,
    pub columns: Vec<Column>,
    pub action: Script,
    pub double_click: Script,
    pub on_focus: Script,
    pub leave_focus: Script,
    pub mouse_enter: Script,
    pub mouse_exit: Script,
    /// `cvarTest` + `showCvar`/`hideCvar`/`enableCvar`/`disableCvar` (keyword, values).
    pub cvar_test: Option<String>,
    pub cvar_flags: Vec<(String, Vec<String>)>,
    /// legacy ui_shared.c additions
    /// `aspectBias` (ALIGN_LEFT/CENTER/RIGHT, ASPECT_NONE = default for items)
    pub aspect_bias: i32,
    /// `textvalign` (VALIGN_BOTTOM default, VALIGN_CENTER, VALIGN_TOP)
    pub text_valign: i32,
    pub group: Option<String>,
    pub asset_shader: Option<String>,
    pub asset_model: Option<String>,
    pub model_origin: [f32; 3],
    pub model_fov: [f32; 2],
    pub model_rotation: i32,
    pub model_angle: i32,
    pub ownerdraw_flag: i32,
    pub max_chars: i32,
    pub max_paint_chars: i32,
    pub max_field_width: i32,
    /// `cvarFloat cvar def min max`
    pub cvar_float: Option<[f32; 3]>,
    /// `cvarStrList` (str_list = true) / `cvarFloatList`: (label, value) pairs
    pub multi: Vec<(String, String)>,
    pub multi_str: bool,
    pub drop_items: i32,
    pub reset_on_feeder_change: bool,
    pub focus_sound: Option<String>,
    pub on_text_entry: Script,
    pub on_char_entry: Script,
    pub mouse_enter_text: Script,
    pub mouse_exit_text: Script,
    /// `addColorRange low high r g b a`
    pub color_ranges: Vec<(f32, f32, [f32; 4])>,
    pub cinematic: Option<String>,
}

impl Default for ItemDef {
    /// `Item_Init` / `Window_Init`.
    fn default() -> Self {
        ItemDef {
            name: String::new(),
            rect: [0.0; 4],
            visible: false,
            style: 0,
            item_type: 0,
            fore: [1.0; 4],
            fore_set: false,
            back: [0.0; 4],
            back_set: false,
            background: None,
            ownerdraw: 0,
            align: 0,
            text_align_x: 0.0,
            text_align_y: 0.0,
            text_scale: 0.55,
            text: None,
            cvar: None,
            text_align: 0,
            text_style: 0,
            border: 0,
            border_size: 1.0,
            border_color: [0.0; 4],
            outline_color: [0.0; 4],
            decoration: false,
            autowrapped: false,
            wrapped: false,
            not_selectable: false,
            feeder: 0.0,
            element_width: 0.0,
            element_height: 0.0,
            element_type: 0,
            columns: Vec::new(),
            action: Vec::new(),
            double_click: Vec::new(),
            on_focus: Vec::new(),
            leave_focus: Vec::new(),
            mouse_enter: Vec::new(),
            mouse_exit: Vec::new(),
            cvar_test: None,
            cvar_flags: Vec::new(),
            aspect_bias: ASPECT_NONE,
            text_valign: VALIGN_BOTTOM,
            group: None,
            asset_shader: None,
            asset_model: None,
            model_origin: [0.0; 3],
            model_fov: [0.0; 2],
            model_rotation: 0,
            model_angle: 0,
            ownerdraw_flag: 0,
            max_chars: 0,
            max_paint_chars: 0,
            max_field_width: 0,
            cvar_float: None,
            multi: Vec::new(),
            multi_str: false,
            drop_items: 0,
            reset_on_feeder_change: false,
            focus_sound: None,
            on_text_entry: Vec::new(),
            on_char_entry: Vec::new(),
            mouse_enter_text: Vec::new(),
            mouse_exit_text: Vec::new(),
            color_ranges: Vec::new(),
            cinematic: None,
        }
    }
}

/// legacy menudef.h enum values
pub const ASPECT_NONE: i32 = 3;
pub const VALIGN_BOTTOM: i32 = 0;
pub const VALIGN_CENTER: i32 = 1;
pub const VALIGN_TOP: i32 = 2;

/// `menuDef`.
#[derive(Clone, Debug)]
pub struct MenuDef {
    pub name: String,
    pub rect: [f32; 4],
    pub items: Vec<ItemDef>,
    pub visible: bool,
    pub fullscreen: bool,
    pub style: i32,
    pub border: i32,
    pub border_size: f32,
    pub fore: [f32; 4],
    pub back: [f32; 4],
    pub border_color: [f32; 4],
    pub focus_color: [f32; 4],
    pub disable_color: [f32; 4],
    pub background: Option<String>,
    /// WINDOW_POPUP
    pub popup: bool,
    /// WINDOW_OOB_CLICK: a click outside the rect closes the menu.
    pub oob_click: bool,
    pub on_open: Script,
    pub on_close: Script,
    pub on_esc: Script,
    /// legacy: `aspectBias` (ALIGN_CENTER by default for menus), `font`,
    /// `outlinecolor`, `cinematic`, `soundLoop`, fades, `ownerdraw(Flag)`
    pub aspect_bias: i32,
    pub font: Option<String>,
    pub outline_color: [f32; 4],
    pub cinematic: Option<String>,
    pub sound_loop: Option<String>,
    pub fade_clamp: f32,
    pub fade_cycle: i32,
    pub fade_amount: f32,
    pub ownerdraw: i32,
    pub ownerdraw_flag: i32,
}

impl Default for MenuDef {
    /// `Menu_Init` / `Window_Init`.
    fn default() -> Self {
        MenuDef {
            name: String::new(),
            rect: [0.0; 4],
            items: Vec::new(),
            visible: false,
            fullscreen: false,
            style: 0,
            border: 0,
            border_size: 1.0,
            fore: [1.0; 4],
            back: [0.0; 4],
            border_color: [0.0; 4],
            focus_color: [1.0; 4],
            disable_color: [0.0; 4],
            background: None,
            popup: false,
            oob_click: false,
            on_open: Vec::new(),
            on_close: Vec::new(),
            on_esc: Vec::new(),
            aspect_bias: 1, // ALIGN_CENTER
            font: None,
            outline_color: [0.0; 4],
            cinematic: None,
            sound_loop: None,
            fade_clamp: 0.0,
            fade_cycle: 0,
            fade_amount: 0.0,
            ownerdraw: 0,
            ownerdraw_flag: 0,
        }
    }
}

/// `Menu_AspectiseRect`.
pub fn aspectise_rect(bias: i32, r: &mut [f32; 4], scale: f32) {
    match bias {
        0 => {
            r[0] *= scale;
            r[2] *= scale;
        }
        1 => {
            r[0] = r[0] * scale + (320.0 - 320.0 * scale);
            r[2] *= scale;
        }
        2 => {
            r[0] = 640.0 - (640.0 - r[0]) * scale;
            r[2] *= scale;
        }
        _ => {}
    }
}

impl MenuDef {
    /// `Menu_AspectCompensate` for `scale` = cgDC/uiDC aspectScale
    /// ((640 * vidHeight) / (480 * vidWidth)). Item rects are `rectClient`
    /// (relative to the menu, as parsed): a biased menu has its own rect
    /// aspectised and every item's client x/w scaled, so the items follow
    /// the menu's new origin (Item_SetScreenCoords adds it when painting).
    pub fn aspect_compensated(&self, scale: f32) -> MenuDef {
        let mut m = self.clone();
        if (scale - 1.0).abs() < 1e-6 {
            return m;
        }
        if m.aspect_bias != ASPECT_NONE {
            aspectise_rect(m.aspect_bias, &mut m.rect, scale);
            for it in &mut m.items {
                it.rect[0] *= scale;
                it.rect[2] *= scale;
                it.text_align_x *= scale;
            }
        } else {
            for it in &mut m.items {
                aspectise_rect(it.aspect_bias, &mut it.rect, scale);
                if it.aspect_bias != ASPECT_NONE {
                    it.text_align_x *= scale;
                }
            }
        }
        m
    }
}

/// The menus of one or more files plus the `#define`s seen (menudef.h).
#[derive(Clone, Debug, Default)]
pub struct MenuSet {
    pub menus: Vec<MenuDef>,
    pub defines: HashMap<String, String>,
    /// keywords the parser skipped (diagnostics / tests).
    pub unknown: Vec<String>,
}

impl MenuSet {
    pub fn menu(&self, name: &str) -> Option<&MenuDef> {
        self.menus.iter().find(|m| m.name.eq_ignore_ascii_case(name))
    }
    /// Integer value of a `#define` (decimal or 0x hex).
    pub fn define_int(&self, name: &str) -> Option<i32> {
        let toks = expand_tokens(name, &self.defines, 0);
        if toks.len() == 1 {
            return parse_int(&toks[0]);
        }
        num_value(&toks).map(|f| f as i32)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Tok {
    Word(String),
    Str(String),
    Open,
    Close,
}

pub(crate) fn parse_int(s: &str) -> Option<i32> {
    let s = s.trim();
    if let Some(h) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        return i64::from_str_radix(h, 16).ok().map(|v| v as i32);
    }
    s.parse::<i32>().ok().or_else(|| s.parse::<f32>().ok().map(|f| f as i32))
}

fn parse_float(s: &str) -> Option<f32> {
    let s = s.trim().trim_end_matches('f');
    if s.starts_with("0x") || s.starts_with("0X") {
        return parse_int(s).map(|v| v as f32);
    }
    s.parse::<f32>().ok()
}

/// Split a word into botlib precompiler tokens (numbers, names, single
/// punctuation characters) and expand names through the #defines.
pub(crate) fn expand_tokens(word: &str, defines: &HashMap<String, String>, depth: u32) -> Vec<String> {
    let b = word.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c.is_ascii_whitespace() {
            i += 1;
        } else if c.is_ascii_digit() || (c == b'.' && b.get(i + 1).is_some_and(|d| d.is_ascii_digit())) {
            let s = i;
            if c == b'0' && matches!(b.get(i + 1), Some(b'x') | Some(b'X')) {
                i += 2;
                while i < b.len() && b[i].is_ascii_hexdigit() {
                    i += 1;
                }
            } else {
                while i < b.len() && (b[i].is_ascii_digit() || b[i] == b'.') {
                    i += 1;
                }
                if i < b.len() && b[i] == b'f' {
                    i += 1;
                }
            }
            out.push(word[s..i].to_string());
        } else if c.is_ascii_alphabetic() || c == b'_' {
            let s = i;
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                i += 1;
            }
            let name = &word[s..i];
            match defines.get(name) {
                Some(v) if depth < 16 => out.extend(expand_tokens(v, defines, depth + 1)),
                _ => out.push(name.to_string()),
            }
        } else {
            out.push((c as char).to_string());
            i += 1;
        }
    }
    out
}

/// `PC_Float_Parse`: a number, `-` number, or `(` expression `)`.
pub(crate) fn num_value(toks: &[String]) -> Option<f32> {
    match toks.first().map(String::as_str) {
        Some("(") => expression(&toks[1..]),
        Some("-") => toks.get(1).and_then(|t| parse_float(t)).map(|f| -f),
        Some(t) if toks.len() == 1 => parse_float(t),
        _ => None,
    }
}

/// `PC_Expression_Parse` (ui_shared.c): shunting-yard over the tokens after
/// the opening '(', with the original's precedences (* 4, / 3, + 2, - 1) and
/// its strict-less popping rule.
fn expression(toks: &[String]) -> Option<f32> {
    fn prec(op: char) -> i32 {
        match op {
            '*' => 4,
            '/' => 3,
            '+' => 2,
            '-' => 1,
            '(' => 0,
            _ => -1,
        }
    }
    enum E {
        Op(char),
        Val(f32),
    }
    let mut stack: Vec<char> = Vec::new();
    let mut fifo: Vec<E> = Vec::new();
    let mut unmatched = 0;
    let mut expecting = true;
    let mut i = 0;
    while i < toks.len() {
        let mut t = toks[i].as_str();
        i += 1;
        if unmatched == 0 && t == ")" {
            break;
        }
        let mut neg = false;
        if expecting && t == "-" {
            t = toks.get(i)?.as_str();
            i += 1;
            neg = true;
        }
        if let Some(v) = parse_float(t).filter(|_| t.as_bytes()[0].is_ascii_digit() || t.starts_with('.')) {
            if !expecting {
                return None;
            }
            expecting = false;
            fifo.push(E::Val(if neg { -v } else { v }));
            continue;
        }
        let op = t.chars().next()?;
        match op {
            '(' => {
                unmatched += 1;
                stack.push('(');
            }
            ')' => {
                unmatched -= 1;
                if unmatched < 0 {
                    return None;
                }
                while let Some(&o) = stack.last() {
                    if o == '(' {
                        break;
                    }
                    fifo.push(E::Op(stack.pop()?));
                }
                stack.pop()?;
            }
            '*' | '/' | '+' | '-' => {
                if expecting {
                    return None;
                }
                expecting = true;
                while let Some(&o) = stack.last() {
                    if prec(op) < prec(o) {
                        fifo.push(E::Op(stack.pop()?));
                    } else {
                        break;
                    }
                }
                stack.push(op);
            }
            _ => return None,
        }
    }
    while let Some(o) = stack.pop() {
        fifo.push(E::Op(o));
    }
    let mut vals: Vec<f32> = Vec::new();
    for e in fifo {
        match e {
            E::Val(v) => vals.push(v),
            E::Op(op) => {
                let b = vals.pop()?;
                let a = vals.pop()?;
                vals.push(match op {
                    '*' => a * b,
                    '/' => a / b,
                    '+' => a + b,
                    '-' => a - b,
                    _ => return None,
                });
            }
        }
    }
    vals.pop()
}

/// Lexer with the botlib precompiler features the menus use: `//` and
/// `/* */` comments, quoted strings, `#include "file"` and `#define NAME value`.
pub(crate) fn lex(text: &str, defines: &mut HashMap<String, String>, include: &mut dyn FnMut(&str) -> Option<String>, out: &mut Vec<Tok>, depth: u32) -> Result<(), String> {
    let b = text.as_bytes();
    let mut i = 0;
    let mut line_start = true;
    while i < b.len() {
        let c = b[i];
        if c == b'\n' {
            line_start = true;
            i += 1;
            continue;
        }
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        if c == b'/' && b.get(i + 1) == Some(&b'/') {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if c == b'/' && b.get(i + 1) == Some(&b'*') {
            i += 2;
            while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') {
                i += 1;
            }
            i = (i + 2).min(b.len());
            continue;
        }
        if c == b'#' && line_start {
            let end = text[i..].find('\n').map(|n| i + n).unwrap_or(b.len());
            let line = text[i + 1..end].split("//").next().unwrap_or("").trim();
            i = end;
            let mut parts = line.splitn(2, char::is_whitespace);
            let dir = parts.next().unwrap_or("");
            let rest = parts.next().unwrap_or("").trim();
            match dir {
                "include" => {
                    let path = rest.trim_matches(|c| c == '"' || c == '<' || c == '>');
                    if depth > 8 {
                        return Err(format!("#include nesting too deep at {path}"));
                    }
                    let inc = include(path).ok_or_else(|| format!("#include {path}: not found"))?;
                    lex(&inc, defines, include, out, depth + 1)?;
                }
                "define" => {
                    let mut p = rest.splitn(2, char::is_whitespace);
                    if let Some(name) = p.next().filter(|n| !n.is_empty()) {
                        let val = p.next().unwrap_or("").trim().trim_matches('"').to_string();
                        defines.insert(name.to_string(), val);
                    }
                }
                "undef" => {
                    defines.remove(rest.split_whitespace().next().unwrap_or(""));
                }
                _ => {} // #ifdef etc.: not used by the HUD menus
            }
            continue;
        }
        line_start = false;
        match c {
            b'{' => {
                out.push(Tok::Open);
                i += 1;
            }
            b'}' => {
                out.push(Tok::Close);
                i += 1;
            }
            b'"' => {
                let s = i + 1;
                let mut e = s;
                while e < b.len() && b[e] != b'"' {
                    e += 1;
                }
                // Parse_ReadToken: strings behind each other are concatenated
                if let Some(Tok::Str(prev)) = out.last_mut() {
                    prev.push_str(&text[s..e]);
                } else {
                    out.push(Tok::Str(text[s..e].to_string()));
                }
                i = (e + 1).min(b.len());
            }
            _ => {
                let s = i;
                while i < b.len() && !b[i].is_ascii_whitespace() && b[i] != b'{' && b[i] != b'}' && b[i] != b'"' {
                    i += 1;
                }
                let w = &text[s..i];
                if w == "enum" {
                    // Parse_ReadEnumeration: `enum { A = -1, B, C };` adds defines
                    let open = text[i..].find('{').map(|n| i + n).ok_or("enum: expected {")?;
                    let close = text[open..].find('}').map(|n| open + n).ok_or("enum: expected }")?;
                    let body: String = text[open + 1..close]
                        .lines()
                        .map(|l| l.split("//").next().unwrap_or(""))
                        .collect::<Vec<_>>()
                        .join("\n");
                    let mut value = 0i32;
                    for entry in body.split(',') {
                        let entry = entry.trim();
                        if entry.is_empty() {
                            continue;
                        }
                        let mut kv = entry.splitn(2, '=');
                        let name = kv.next().unwrap_or("").trim();
                        if let Some(v) = kv.next() {
                            value = parse_int(&v.split_whitespace().collect::<String>()).ok_or_else(|| format!("enum {name}: expecting integer"))?;
                        }
                        defines.insert(name.to_string(), value.to_string());
                        value += 1;
                    }
                    i = close + 1;
                    // ignore trailing semicolon
                    let rest = text[i..].trim_start();
                    if rest.starts_with(';') {
                        i = b.len() - rest.len() + 1;
                    }
                    continue;
                }
                // botlib expands macros as it reads, so a later #define or
                // #undef only affects what follows it
                let has_define = w
                    .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                    .any(|n| !n.is_empty() && !n.as_bytes()[0].is_ascii_digit() && defines.contains_key(n));
                if has_define {
                    out.push(Tok::Word(expand_tokens(w, defines, 0).join(" ")));
                } else {
                    out.push(Tok::Word(w.to_string()));
                }
            }
        }
    }
    Ok(())
}

const ITEM_KEYS: &[&str] = &[
    "name", "group", "text", "cvar", "background", "type", "rect", "style", "border", "bordersize", "decoration", "notselectable", "visible", "ownerdraw", "ownerdrawflag",
    "align", "textalign", "textvalign", "textalignx", "textaligny", "textscale", "textstyle", "forecolor", "backcolor", "bordercolor", "outlinecolor", "autowrapped", "wrapped",
    "feeder", "elementwidth", "elementheight", "elementtype", "columns", "action", "doubleclick", "onfocus", "leavefocus", "mouseenter", "mouseexit", "mouseentertext",
    "mouseexittext", "cvartest", "showcvar", "hidecvar", "enablecvar", "disablecvar", "horizontalscroll", "noscrollbar", "special", "asset_shader", "asset_model",
    "model_origin", "model_fovx", "model_fovy", "model_rotation", "model_angle", "aspectbias", "resetonfeederchange", "dropitems", "maxchars", "maxpaintchars",
    "maxfieldwidth", "focussound", "cinematic", "ontextentry", "oncharentry", "cvarfloat", "cvarstrlist", "cvarfloatlist", "addcolorrange", "ownerdrawflag",
];
const MENU_KEYS: &[&str] = &[
    "name", "fullscreen", "visible", "rect", "style", "background", "border", "bordersize", "forecolor", "backcolor", "bordercolor", "focuscolor", "disablecolor",
    "fadeclamp", "fadecycle", "fadeamount", "ownerdraw", "outofboundsclick", "popup", "itemdef", "onopen", "onclose", "onesc", "soundloop", "ownerdrawflag",
    "aspectbias", "font", "outlinecolor", "cinematic",
];

struct Parser<'a> {
    toks: Vec<Tok>,
    pos: usize,
    defines: &'a HashMap<String, String>,
    unknown: Vec<String>,
}

impl Parser<'_> {
    fn next(&mut self) -> Option<Tok> {
        let t = self.toks.get(self.pos).cloned();
        self.pos += 1;
        t
    }
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos)
    }
    fn expect_open(&mut self, what: &str) -> Result<(), String> {
        match self.next() {
            Some(Tok::Open) => Ok(()),
            t => Err(format!("{what}: expected '{{', got {t:?}")),
        }
    }
    fn string(&mut self, what: &str) -> Result<String, String> {
        match self.next() {
            Some(Tok::Word(w)) | Some(Tok::Str(w)) => Ok(w),
            t => Err(format!("{what}: expected a value, got {t:?}")),
        }
    }
    /// The next value as botlib precompiler tokens: macro-expanded through the
    /// #defines, split into numbers / names / punctuation, and, when it opens
    /// a parenthesis, extended over the following words until it closes.
    fn num_tokens(&mut self, what: &str) -> Result<Vec<String>, String> {
        let first = self.string(what)?;
        let mut toks = expand_tokens(&first, self.defines, 0);
        let depth = |t: &[String]| t.iter().fold(0i32, |d, x| d + (x == "(") as i32 - (x == ")") as i32);
        if toks.first().map(|t| t == "(").unwrap_or(false) {
            while depth(&toks) > 0 {
                match self.peek() {
                    Some(Tok::Word(_)) => {
                        let w = self.string(what)?;
                        toks.extend(expand_tokens(&w, self.defines, 0));
                    }
                    _ => break,
                }
            }
        }
        Ok(toks)
    }
    fn float(&mut self, what: &str) -> Result<f32, String> {
        let toks = self.num_tokens(what)?;
        num_value(&toks).ok_or_else(|| format!("{what}: bad number {:?}", toks.concat()))
    }
    fn int(&mut self, what: &str) -> Result<i32, String> {
        let toks = self.num_tokens(what)?;
        // PC_Int_Parse: expressions truncate, plain tokens use the int value
        if toks.first().map(|t| t == "(").unwrap_or(false) {
            return num_value(&toks).map(|f| f as i32).ok_or_else(|| format!("{what}: bad integer {:?}", toks.concat()));
        }
        let (neg, rest) = if toks.first().map(|t| t == "-").unwrap_or(false) { (true, &toks[1..]) } else { (false, &toks[..]) };
        let v = rest.first().and_then(|t| parse_int(t)).ok_or_else(|| format!("{what}: bad integer {:?}", toks.concat()))?;
        Ok(if neg { -v } else { v })
    }
    fn floats4(&mut self, what: &str) -> Result<[f32; 4], String> {
        Ok([self.float(what)?, self.float(what)?, self.float(what)?, self.float(what)?])
    }
    /// PC_Script_Parse: a `{ ... }` block split into `;`-separated commands.
    fn script(&mut self, what: &str) -> Result<Script, String> {
        self.expect_open(what)?;
        let mut out: Script = Vec::new();
        let mut cur: Vec<String> = Vec::new();
        let mut depth = 1;
        loop {
            match self.next() {
                None => return Err(format!("{what}: unexpected end of script")),
                Some(Tok::Open) => depth += 1,
                Some(Tok::Close) => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                Some(Tok::Str(s)) => cur.push(s),
                Some(Tok::Word(w)) => {
                    let mut parts = w.split(';').peekable();
                    while let Some(p) = parts.next() {
                        if !p.is_empty() {
                            cur.push(p.to_string());
                        }
                        if parts.peek().is_some() && !cur.is_empty() {
                            out.push(std::mem::take(&mut cur));
                        }
                    }
                }
            }
        }
        if !cur.is_empty() {
            out.push(cur);
        }
        Ok(out)
    }
    /// `cvarTest`-style list argument: `"a"` or `{ "a" "b" }`.
    fn string_list(&mut self, what: &str) -> Result<Vec<String>, String> {
        if self.peek() == Some(&Tok::Open) {
            self.pos += 1;
            let mut v = Vec::new();
            loop {
                match self.next() {
                    Some(Tok::Close) => return Ok(v),
                    Some(Tok::Word(w)) | Some(Tok::Str(w)) => {
                        for p in w.split(';').filter(|p| !p.is_empty()) {
                            v.push(p.to_string());
                        }
                    }
                    _ => return Err(format!("{what}: bad list")),
                }
            }
        }
        Ok(vec![self.string(what)?])
    }
    fn skip_block(&mut self) {
        let mut depth = 0;
        while let Some(t) = self.next() {
            match t {
                Tok::Open => depth += 1,
                Tok::Close => {
                    depth -= 1;
                    if depth <= 0 {
                        return;
                    }
                }
                _ => {}
            }
        }
    }
    /// Skip an unsupported keyword's arguments: up to the next known keyword
    /// or the end of the enclosing block (nested blocks skipped whole).
    fn skip_unknown(&mut self, key: &str, known: &[&str]) {
        self.unknown.push(key.to_string());
        loop {
            match self.peek() {
                None | Some(Tok::Close) => return,
                Some(Tok::Open) => self.skip_block(),
                Some(Tok::Word(w)) if known.contains(&w.to_ascii_lowercase().as_str()) => return,
                _ => self.pos += 1,
            }
        }
    }

    fn item(&mut self) -> Result<ItemDef, String> {
        self.expect_open("itemDef")?;
        let mut it = ItemDef::default();
        loop {
            let key = match self.next() {
                Some(Tok::Close) => return Ok(it),
                Some(Tok::Word(w)) | Some(Tok::Str(w)) => w,
                Some(Tok::Open) => {
                    self.pos -= 1;
                    self.skip_block();
                    continue;
                }
                None => return Err(format!("itemDef {:?}: unexpected end of file", it.name)),
            };
            let lk = key.to_ascii_lowercase();
            match lk.as_str() {
                "name" => it.name = self.string("name")?,
                "group" => it.group = Some(self.string(&lk)?),
                "asset_shader" => it.asset_shader = Some(self.string(&lk)?),
                "asset_model" => it.asset_model = Some(self.string(&lk)?),
                "model_origin" => it.model_origin = [self.float(&lk)?, self.float(&lk)?, self.float(&lk)?],
                "model_fovx" => it.model_fov[0] = self.float(&lk)?,
                "model_fovy" => it.model_fov[1] = self.float(&lk)?,
                "model_rotation" => it.model_rotation = self.int(&lk)?,
                "model_angle" => it.model_angle = self.int(&lk)?,
                "aspectbias" => it.aspect_bias = self.int(&lk)?,
                "resetonfeederchange" => it.reset_on_feeder_change = true,
                "dropitems" => it.drop_items = self.int(&lk)?,
                "maxchars" => it.max_chars = self.int(&lk)?,
                "maxpaintchars" => it.max_paint_chars = self.int(&lk)?,
                "maxfieldwidth" => it.max_field_width = self.int(&lk)?,
                "focussound" => it.focus_sound = Some(self.string(&lk)?),
                "cinematic" => it.cinematic = Some(self.string(&lk)?),
                "ontextentry" => it.on_text_entry = self.script("onTextEntry")?,
                "oncharentry" => it.on_char_entry = self.script("onCharEntry")?,
                "cvarfloat" => {
                    it.cvar = Some(self.string(&lk)?);
                    it.cvar_float = Some([self.float(&lk)?, self.float(&lk)?, self.float(&lk)?]);
                }
                "cvarstrlist" | "cvarfloatlist" => {
                    // ItemParse_cvarStrList / ItemParse_cvarFloatList
                    it.multi_str = lk == "cvarstrlist";
                    it.multi.clear();
                    self.expect_open(&lk)?;
                    let mut label: Option<String> = None;
                    loop {
                        match self.next() {
                            None => return Err(format!("{lk}: end of file inside menu item")),
                            Some(Tok::Close) => break,
                            Some(Tok::Open) => return Err(format!("{lk}: unexpected '{{'")),
                            Some(Tok::Word(w)) | Some(Tok::Str(w)) => {
                                if w == "," || w == ";" {
                                    continue;
                                }
                                let w = w.trim_end_matches([',', ';']).to_string();
                                match label.take() {
                                    None => label = Some(w),
                                    Some(l) => {
                                        let v = if it.multi_str { w } else { num_value(&expand_tokens(&w, self.defines, 0)).map(|f| f.to_string()).ok_or_else(|| format!("{lk}: bad number {w:?}"))? };
                                        it.multi.push((l, v));
                                    }
                                }
                            }
                        }
                    }
                }
                "addcolorrange" => {
                    let lo = self.float(&lk)?;
                    let hi = self.float(&lk)?;
                    let c = self.floats4(&lk)?;
                    it.color_ranges.push((lo, hi, c));
                }
                "text" => it.text = Some(self.string("text")?),
                "cvar" => it.cvar = Some(self.string("cvar")?),
                "background" => it.background = Some(self.string("background")?),
                "type" => it.item_type = self.int("type")?,
                "rect" => it.rect = self.floats4("rect")?,
                "style" => it.style = self.int("style")?,
                "border" => it.border = self.int("border")?,
                "textalign" => it.text_align = self.int("textalign")?,
                "textstyle" => it.text_style = self.int("textstyle")?,
                "ownerdrawflag" => it.ownerdraw_flag = self.int(&lk)?,
                "textvalign" => it.text_valign = self.int(&lk)?,
                "special" => {
                    self.int(&lk)?;
                }
                "bordersize" => it.border_size = self.float("bordersize")?,
                "decoration" => it.decoration = true,
                "notselectable" => it.not_selectable = true,
                "autowrapped" => it.autowrapped = true,
                "wrapped" => it.wrapped = true,
                "horizontalscroll" | "noscrollbar" => {}
                "visible" => it.visible = self.int("visible")? != 0,
                "ownerdraw" => {
                    it.ownerdraw = self.int("ownerdraw")?;
                    it.item_type = ITEM_TYPE_OWNERDRAW;
                }
                "align" => it.align = self.int("align")?,
                "textalignx" => it.text_align_x = self.float("textalignx")?,
                "textaligny" => it.text_align_y = self.float("textaligny")?,
                "textscale" => it.text_scale = self.float("textscale")?,
                "forecolor" => {
                    it.fore = self.floats4("forecolor")?;
                    it.fore_set = true;
                }
                "backcolor" => {
                    it.back = self.floats4("backcolor")?;
                    it.back_set = true;
                }
                "bordercolor" => it.border_color = self.floats4("bordercolor")?,
                "outlinecolor" => it.outline_color = self.floats4("outlinecolor")?,
                "feeder" => it.feeder = self.float("feeder")?,
                "elementwidth" => it.element_width = self.float("elementwidth")?,
                "elementheight" => it.element_height = self.float("elementheight")?,
                "elementtype" => it.element_type = self.int("elementtype")?,
                "columns" => {
                    // legacy ItemParse_columns: pos width align (no maxChars;
                    // stored as [pos, width, 0, align])
                    let n = self.int("columns")?.clamp(0, 16);
                    for _ in 0..n {
                        let (pos, width, align) = (self.int("columns")?, self.int("columns")?, self.int("columns")?);
                        it.columns.push([pos, width, 0, align]);
                    }
                }
                "action" => it.action = self.script("action")?,
                "doubleclick" => it.double_click = self.script("doubleclick")?,
                "onfocus" => it.on_focus = self.script("onFocus")?,
                "leavefocus" => it.leave_focus = self.script("leaveFocus")?,
                "mouseenter" => it.mouse_enter = self.script("mouseEnter")?,
                "mouseexit" => it.mouse_exit = self.script("mouseExit")?,
                "mouseentertext" => it.mouse_enter_text = self.script(&lk)?,
                "mouseexittext" => it.mouse_exit_text = self.script(&lk)?,
                "cvartest" => it.cvar_test = Some(self.string("cvarTest")?),
                "showcvar" | "hidecvar" | "enablecvar" | "disablecvar" => {
                    let v = self.string_list(&lk)?;
                    it.cvar_flags.push((lk, v));
                }
                _ => self.skip_unknown(&key, ITEM_KEYS),
            }
        }
    }

    fn menu(&mut self) -> Result<MenuDef, String> {
        self.expect_open("menuDef")?;
        let mut m = MenuDef::default();
        loop {
            let key = match self.next() {
                Some(Tok::Close) => return Ok(m),
                Some(Tok::Word(w)) | Some(Tok::Str(w)) => w,
                Some(Tok::Open) => {
                    self.pos -= 1;
                    self.skip_block();
                    continue;
                }
                None => return Err(format!("menuDef {:?}: unexpected end of file", m.name)),
            };
            let lk = key.to_ascii_lowercase();
            match lk.as_str() {
                "name" => m.name = self.string("name")?,
                "rect" => m.rect = self.floats4("rect")?,
                "itemdef" => {
                    let it = self.item()?;
                    m.items.push(it);
                }
                "fullscreen" => m.fullscreen = self.int(&lk)? != 0,
                "visible" => m.visible = self.int(&lk)? != 0,
                "style" => m.style = self.int(&lk)?,
                "border" => m.border = self.int(&lk)?,
                "fadecycle" => m.fade_cycle = self.int(&lk)?,
                "ownerdraw" => m.ownerdraw = self.int(&lk)?,
                "ownerdrawflag" => m.ownerdraw_flag = self.int(&lk)?,
                "bordersize" => m.border_size = self.float(&lk)?,
                "fadeclamp" => m.fade_clamp = self.float(&lk)?,
                "fadeamount" => m.fade_amount = self.float(&lk)?,
                "background" => m.background = Some(self.string(&lk)?),
                "soundloop" => m.sound_loop = Some(self.string(&lk)?),
                "aspectbias" => m.aspect_bias = self.int(&lk)?,
                "font" => m.font = Some(self.string(&lk)?),
                "outlinecolor" => m.outline_color = self.floats4(&lk)?,
                "cinematic" => m.cinematic = Some(self.string(&lk)?),
                "forecolor" => m.fore = self.floats4(&lk)?,
                "backcolor" => m.back = self.floats4(&lk)?,
                "bordercolor" => m.border_color = self.floats4(&lk)?,
                "focuscolor" => m.focus_color = self.floats4(&lk)?,
                "disablecolor" => m.disable_color = self.floats4(&lk)?,
                "outofboundsclick" => m.oob_click = true,
                "popup" => m.popup = true,
                "onopen" => m.on_open = self.script("onOpen")?,
                "onclose" => m.on_close = self.script("onClose")?,
                "onesc" => m.on_esc = self.script("onESC")?,
                _ => self.skip_unknown(&key, MENU_KEYS),
            }
        }
    }
}

/// Parse a .menu file (`{ menuDef { ... } ... }`), resolving `#include`s
/// through `include` (path relative to the data root, e.g. "ui/menudef.h").
/// Defines accumulate into `set` so later files can use earlier ones.
pub fn parse_menus_into(set: &mut MenuSet, text: &str, include: &mut dyn FnMut(&str) -> Option<String>) -> Result<usize, String> {
    let mut toks = Vec::new();
    lex(text, &mut set.defines, include, &mut toks, 0)?;
    let mut p = Parser { toks, pos: 0, defines: &set.defines, unknown: Vec::new() };
    let mut menus = Vec::new();
    while let Some(t) = p.next() {
        if let Tok::Word(w) = &t {
            if w.eq_ignore_ascii_case("menudef") {
                menus.push(p.menu()?);
            }
        }
    }
    let unknown = std::mem::take(&mut p.unknown);
    let n = menus.len();
    set.menus.extend(menus);
    set.unknown.extend(unknown);
    Ok(n)
}

#[cfg(test)]
pub fn parse_menus(text: &str, include: &mut dyn FnMut(&str) -> Option<String>) -> Result<MenuSet, String> {
    let mut set = MenuSet::default();
    parse_menus_into(&mut set, text, include)?;
    Ok(set)
}

/// The `loadMenu { "file" ... }` entries of a menu list (ui/hud.txt).
pub fn parse_menu_list(text: &str) -> Vec<String> {
    let mut toks = Vec::new();
    let mut defs = HashMap::new();
    if lex(text, &mut defs, &mut |_| None, &mut toks, 0).is_err() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut i = 0;
    while i < toks.len() {
        if matches!(&toks[i], Tok::Word(w) if w.eq_ignore_ascii_case("loadmenu")) && toks.get(i + 1) == Some(&Tok::Open) {
            i += 2;
            while i < toks.len() && toks[i] != Tok::Close {
                if let Tok::Str(s) | Tok::Word(s) = &toks[i] {
                    out.push(s.clone());
                }
                i += 1;
            }
        }
        i += 1;
    }
    out
}

/// One stage of a 2D shader: image, `rgbgen const`/`alphagen const` colour
/// (None = vertex colour) and `tcmod rotate` speed.
#[derive(Clone, Debug, PartialEq)]
pub struct Stage2D {
    pub image: String,
    pub color: Option<[f32; 4]>,
    pub rotate: f32,
}

/// The stages of shader `name` in a shader script (only what 2D HUD
/// shaders like gfx/2d/crosshair-*_s use).
pub fn shader_stages_2d(script: &str, name: &str) -> Option<Vec<Stage2D>> {
    let mut depth = 0;
    let mut cur: Option<String> = None;
    let mut stages = Vec::new();
    let mut st: Option<Stage2D> = None;
    let (mut rgb, mut alpha): (Option<[f32; 3]>, Option<f32>) = (None, None);
    for raw in script.lines() {
        let line = raw.split("//").next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        for tok in line.split_inclusive(['{', '}']) {
            let body = tok.trim_end_matches(['{', '}']).trim();
            let toks: Vec<&str> = body.split_whitespace().collect();
            if !toks.is_empty() {
                let lk = toks[0].to_ascii_lowercase();
                if depth == 0 {
                    cur = Some(toks[0].to_string());
                } else if depth == 2 && cur.as_deref().is_some_and(|c| c.eq_ignore_ascii_case(name)) {
                    let s = st.get_or_insert(Stage2D { image: String::new(), color: None, rotate: 0.0 });
                    match lk.as_str() {
                        "map" | "clampmap" => s.image = toks.get(1).unwrap_or(&"").to_string(),
                        "rgbgen" if toks.get(1).is_some_and(|t| t.eq_ignore_ascii_case("const")) => {
                            let n: Vec<f32> = toks[2..].iter().filter_map(|t| t.trim_matches(['(', ')']).parse().ok()).collect();
                            if n.len() >= 3 {
                                rgb = Some([n[0], n[1], n[2]]);
                            }
                        }
                        "alphagen" if toks.get(1).is_some_and(|t| t.eq_ignore_ascii_case("const")) => alpha = toks.get(2).and_then(|t| t.parse().ok()),
                        "tcmod" if toks.get(1).is_some_and(|t| t.eq_ignore_ascii_case("rotate")) => s.rotate = toks.get(2).and_then(|t| t.parse().ok()).unwrap_or(0.0),
                        _ => {}
                    }
                }
            }
            if tok.ends_with('{') {
                depth += 1;
                if depth == 2 {
                    st = None;
                    rgb = None;
                    alpha = None;
                }
            } else if tok.ends_with('}') {
                if depth == 2 {
                    if let Some(mut s) = st.take() {
                        if rgb.is_some() || alpha.is_some() {
                            let c = rgb.unwrap_or([1.0; 3]);
                            s.color = Some([c[0], c[1], c[2], alpha.unwrap_or(1.0)]);
                        }
                        if !s.image.is_empty() {
                            stages.push(s);
                        }
                    }
                }
                depth -= 1;
                if depth == 0 {
                    if cur.as_deref().is_some_and(|c| c.eq_ignore_ascii_case(name)) {
                        return Some(stages);
                    }
                    cur = None;
                }
            }
        }
    }
    None
}

// =====================================================================
// ownerdraw ids (resolved by name through menudef.h)
// =====================================================================

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Od {
    Health,
    HealthCross,
    Stamina(u8),
    StaminaBolt,
    BoostBolt,
    ClipsRing,
    BuildTimerRing,
    CreditsValue,
    CreditsValueNopad,
    CreditsFraction,
    AmmoValue,
    ClipsValue,
    BuildTimer,
    WeaponIcon,
    Select,
    SelectText,
    Wallclimbing,
    Boosted,
    PoisonBarbs,
    AlienSense,
    HumanScanner,
    UsableBuildable,
    ChargeBarBg,
    ChargeBar,
    Crosshair,
    CrosshairNames,
    Location,
    Follow,
    TeamOverlay,
    Fps,
    Timer,
    Clock,
    Speedometer,
    Console,
    Tutorial,
    LoadLevelshot,
    LoadMedia,
    LoadMediaLabel,
    LoadBuildables,
    LoadBuildablesLabel,
    LoadCharModel,
    LoadCharModelLabel,
    LoadOverall,
    LoadLevelname,
    LoadMotd,
    LoadHostname,
}

const OWNERDRAWS: &[(&str, Od)] = &[
    ("CG_PLAYER_HEALTH", Od::Health),
    ("CG_PLAYER_HEALTH_CROSS", Od::HealthCross),
    ("CG_PLAYER_STAMINA_1", Od::Stamina(1)),
    ("CG_PLAYER_STAMINA_2", Od::Stamina(2)),
    ("CG_PLAYER_STAMINA_3", Od::Stamina(3)),
    ("CG_PLAYER_STAMINA_4", Od::Stamina(4)),
    ("CG_PLAYER_STAMINA_BOLT", Od::StaminaBolt),
    ("CG_PLAYER_BOOST_BOLT", Od::BoostBolt),
    ("CG_PLAYER_CLIPS_RING", Od::ClipsRing),
    ("CG_PLAYER_BUILD_TIMER_RING", Od::BuildTimerRing),
    ("CG_PLAYER_CREDITS_VALUE", Od::CreditsValue),
    ("CG_PLAYER_CREDITS_VALUE_NOPAD", Od::CreditsValueNopad),
    ("CG_PLAYER_CREDITS_FRACTION", Od::CreditsFraction),
    ("CG_PLAYER_AMMO_VALUE", Od::AmmoValue),
    ("CG_PLAYER_CLIPS_VALUE", Od::ClipsValue),
    ("CG_PLAYER_BUILD_TIMER", Od::BuildTimer),
    ("CG_PLAYER_WEAPONICON", Od::WeaponIcon),
    ("CG_PLAYER_SELECT", Od::Select),
    ("CG_PLAYER_SELECTTEXT", Od::SelectText),
    ("CG_PLAYER_WALLCLIMBING", Od::Wallclimbing),
    ("CG_PLAYER_BOOSTED", Od::Boosted),
    ("CG_PLAYER_POISON_BARBS", Od::PoisonBarbs),
    ("CG_PLAYER_ALIEN_SENSE", Od::AlienSense),
    ("CG_PLAYER_HUMAN_SCANNER", Od::HumanScanner),
    ("CG_PLAYER_USABLE_BUILDABLE", Od::UsableBuildable),
    ("CG_PLAYER_CHARGE_BAR_BG", Od::ChargeBarBg),
    ("CG_PLAYER_CHARGE_BAR", Od::ChargeBar),
    ("CG_PLAYER_CROSSHAIR", Od::Crosshair),
    ("CG_PLAYER_CROSSHAIRNAMES", Od::CrosshairNames),
    ("CG_PLAYER_LOCATION", Od::Location),
    ("CG_FOLLOW", Od::Follow),
    ("CG_TEAMOVERLAY", Od::TeamOverlay),
    ("CG_FPS", Od::Fps),
    ("CG_TIMER", Od::Timer),
    ("CG_CLOCK", Od::Clock),
    ("CG_SPEEDOMETER", Od::Speedometer),
    ("CG_CONSOLE", Od::Console),
    ("CG_TUTORIAL", Od::Tutorial),
    // ui/loading.menu (CG_DrawLoadingScreen)
    ("CG_LOAD_LEVELSHOT", Od::LoadLevelshot),
    ("CG_LOAD_MEDIA", Od::LoadMedia),
    ("CG_LOAD_MEDIA_LABEL", Od::LoadMediaLabel),
    ("CG_LOAD_BUILDABLES", Od::LoadBuildables),
    ("CG_LOAD_BUILDABLES_LABEL", Od::LoadBuildablesLabel),
    ("CG_LOAD_CHARMODEL", Od::LoadCharModel),
    ("CG_LOAD_CHARMODEL_LABEL", Od::LoadCharModelLabel),
    ("CG_LOAD_OVERALL", Od::LoadOverall),
    ("CG_LOAD_LEVELNAME", Od::LoadLevelname),
    ("CG_LOAD_MOTD", Od::LoadMotd),
    ("CG_LOAD_HOSTNAME", Od::LoadHostname),
    // Not drawn (engine/online only): CG_LAGOMETER, CG_SNAPSHOT,
    // CG_DEMO_PLAYBACK, CG_DEMO_RECORDING. CG_ALIENS/HUMANS_SCORE_LABEL,
    // CG_STAGE_REPORT_TEXT and CG_SPECTATORS belong to teamscore_menu, which
    // TremMenus draws.
];

fn ownerdraw_table(set: &MenuSet) -> HashMap<i32, Od> {
    OWNERDRAWS.iter().filter_map(|(n, od)| set.define_int(n).map(|id| (id, *od))).collect()
}

// legacy menudef.h enums used by the drawing code
const ALIGN_NONE: i32 = 3;
const VALIGN_NONE: i32 = 3;
const WINDOW_STYLE_GRADIENT: i32 = 2;
const WINDOW_BORDER_HORZ: i32 = 2;
const WINDOW_BORDER_VERT: i32 = 3;
const ITEM_TEXTSTYLE_NORMAL: i32 = 0;
const ITEM_TEXTSTYLE_BLINK: i32 = 1;
const ITEM_TEXTSTYLE_SHADOWED: i32 = 3;
const ITEM_TEXTSTYLE_SHADOWEDMORE: i32 = 6;
const ITEM_TEXTSTYLE_NEON: i32 = 7;
/// ui_shared.h BLINK_DIVISOR / PULSE_DIVISOR
const BLINK_DIVISOR: u64 = 200;
const PULSE_DIVISOR: f32 = 75.0;
/// q_shared.h INDENT_MARKER
const INDENT_MARKER: u8 = 0x0b;

// =====================================================================
// cgame cvars (legacy cg_main.c defaults)
// =====================================================================

/// The cgame cvars the HUD ownerdraws read, with the defaults of legacy
/// cg_main.c `cvarTable` (the ones TremHud exports can be changed there).
#[derive(Clone, Copy, Debug)]
struct Cvars {
    draw_fps: i32,
    draw_timer: i32,
    draw_clock: i32,
    draw_speed: i32,
    draw_charge_bar: i32,
    draw_crosshair: i32,
    crosshair_size: f32,
    draw_crosshair_names: i32,
    draw_team_overlay: i32,
    team_overlay_sort_mode: i32,
    team_overlay_max_players: i32,
    team_overlay_userinfo: i32,
    console_latency: i32,
    tutorial: i32,
    draw_gun: i32,
    viewsize: i32,
    pain_blend_down_rate: f32,
    pain_blend_max: f32,
    pain_blend_scale: f32,
    pain_blend_up_rate: f32,
    pain_blend_zoom: f32,
}

impl Default for Cvars {
    fn default() -> Self {
        Cvars {
            draw_fps: 1,
            draw_timer: 1,
            draw_clock: 0,
            draw_speed: 0,
            draw_charge_bar: 1,
            draw_crosshair: 2,
            crosshair_size: 1.0,
            draw_crosshair_names: 1,
            draw_team_overlay: 1,
            team_overlay_sort_mode: 1,
            team_overlay_max_players: 8,
            team_overlay_userinfo: 1,
            console_latency: 3000,
            tutorial: 1,
            draw_gun: 1,
            viewsize: 100,
            pain_blend_down_rate: 0.5,
            pain_blend_max: 0.7,
            pain_blend_scale: 7.0,
            pain_blend_up_rate: 10.0,
            pain_blend_zoom: 0.65,
        }
    }
}

/// Shader handle 0 (tr.defaultShader): a name no data defines.
const DEFAULT_SHADER: &str = "*default";
/// cg_centertime (cg_main.c default 3 seconds)
const CG_CENTERTIME: f32 = 3.0;
/// cg_local.h BIGCHAR_HEIGHT
const BIGCHAR_HEIGHT: i32 = 16;
/// cg_draw.c FOLLOWING_STRING / CHASING_STRING
const FOLLOWING_STRING: &str = "following ";
const CHASING_STRING: &str = "chasing ";
/// cg_local.h CROSSHAIR_ALWAYSOFF / CROSSHAIR_RANGEDONLY
const CROSSHAIR_ALWAYSOFF: i32 = 0;
const CROSSHAIR_RANGEDONLY: i32 = 1;
/// cg_draw.c speedometer
const SPEEDOMETER_NUM_SAMPLES: usize = 160;
const SPEEDOMETER_DRAW_TEXT: i32 = 0x1;
const SPEEDOMETER_DRAW_GRAPH: i32 = 0x2;
const SPEEDOMETER_IGNORE_Z: i32 = 0x4;
const SPEEDOMETER_MIN_RANGE: f32 = 900.0;
const SPEED_MED: f32 = 1000.0;
const SPEED_FAST: f32 = 1600.0;
const FPS_FRAMES: usize = 20;
const CHARGE_BAR_FADE_RATE: f32 = 0.002;
const CHARGE_BAR_CAP_SIZE: f32 = 3.0;
/// cg_local.h MAX_CONSOLE_LINES
const MAX_CONSOLE_LINES: usize = 32;
/// cg_buildable.c
const STATUS_FADE_TIME: i32 = 200;
const STATUS_MAX_VIEW_DIST: f32 = 900.0;
const STATUS_PEEK_DIST: f32 = 20.0;

// =====================================================================
// fonts and text (ui_shared.c UI_Text_*)
// =====================================================================

/// smallFont / font / bigFont of loading.menu's assetGlobalDef
/// (fonts/fontImage_{20,26,34}.dat), as `CG_Asset_Parse` registers them.
pub struct HudFonts {
    pub small: QFont,
    pub text: QFont,
    pub big: QFont,
}

/// legacy ui_main.c ui_smallFont / ui_bigFont defaults (cgDC.small/bigFontScale).
const SMALL_FONT_SCALE: f32 = 0.2;
const BIG_FONT_SCALE: f32 = 0.5;

impl HudFonts {
    /// `UI_FontForScale`.
    fn pick(&self, scale: f32) -> &QFont {
        if scale <= SMALL_FONT_SCALE {
            &self.small
        } else if scale >= BIG_FONT_SCALE {
            &self.big
        } else {
            &self.text
        }
    }
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

/// `UI_Parse_Indent`: a leading `<pixels>\v` indent marker; (pixels, bytes).
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

/// q_shared.h MAX_EMOTICON_NAME_LEN / MAX_EMOTICONS
const MAX_EMOTICON_NAME_LEN: usize = 16;
const MAX_EMOTICONS: usize = 64;

/// `emoticon_t`: name, width in glyph heights, and its image.
#[derive(Clone, Debug, PartialEq)]
pub struct Emoticon {
    pub name: String,
    pub width: i32,
    pub shader: String,
}

/// `BG_LoadEmoticons`: emoticons/<name>_<N>x1.tga files (N in 1..9) from
/// `trap_FS_GetFileList( "emoticons", "x1.tga" )`, at most MAX_EMOTICONS;
/// the cgame registers each as emoticons/<name>_<N>x1.tga (CG_AssetCache).
pub fn load_emoticons<'a>(files: impl Iterator<Item = &'a str>) -> Vec<Emoticon> {
    let mut out = Vec::new();
    for f in files {
        if out.len() >= MAX_EMOTICONS {
            break;
        }
        let lower = f.to_ascii_lowercase();
        let Some(name) = lower.strip_prefix("emoticons/").and_then(|_| f.get(10..)) else { continue };
        if name.contains('/') || !name.to_ascii_lowercase().ends_with("x1.tga") {
            continue;
        }
        let b = name.as_bytes();
        let n = b.len();
        if n < 9 || b[n - 8] != b'_' || !(b'1'..=b'9').contains(&b[n - 7]) {
            continue; // skipping invalidly named emoticon
        }
        if n - 8 > MAX_EMOTICON_NAME_LEN {
            continue; // emoticon file name too long
        }
        let width = (b[n - 7] - b'0') as i32;
        // Q_strncpyz( name, file, fileLen - 8 + 1 ) keeps at most 15 characters
        let base: String = name[..n - 8].chars().take(MAX_EMOTICON_NAME_LEN - 1).collect();
        out.push(Emoticon { shader: format!("emoticons/{base}_{width}x1.tga"), name: base, width });
    }
    out
}

/// `UI_Text_IsEmoticon` at byte `s`: Some((escaped, length, index)).
fn is_emoticon(emoticons: &[Emoticon], b: &[u8], s: usize) -> Option<(bool, usize, usize)> {
    if b.get(s) != Some(&b'[') {
        return None;
    }
    let mut p = s + 1;
    let escaped = b.get(p) == Some(&b'[');
    if escaped {
        p += 1;
    }
    let mut length = 0;
    loop {
        match b.get(p + length) {
            Some(b']') => break,
            None | Some(0) => return None,
            _ if length == MAX_EMOTICON_NAME_LEN - 1 => return None,
            _ => length += 1,
        }
    }
    let name = &b[p..p + length];
    let i = emoticons.iter().position(|e| e.name.as_bytes().eq_ignore_ascii_case(name))?;
    Some((escaped, length + 2 + escaped as usize, i))
}

/// `UI_EscapeEmoticons`: "[" before every emoticon so it prints as text.
fn escape_emoticons(emoticons: &[Emoticon], src: &str) -> String {
    let b = src.as_bytes();
    let mut out = Vec::with_capacity(b.len() + 8);
    for i in 0..b.len() {
        if matches!(is_emoticon(emoticons, b, i), Some((false, _, _))) {
            out.push(b'[');
        }
        out.push(b[i]);
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `Q_CleanStr`: drop colour codes and non-printable characters.
fn clean_str(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if is_color_string(b, i) {
            i += 1;
        } else if (0x20..=0x7e).contains(&b[i]) {
            out.push(b[i]);
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `Q_ParseNewlines`: the two characters `\n` become a newline (a
/// backslash before anything else is dropped).
fn parse_newlines(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' {
            i += 1;
            match b.get(i) {
                Some(b'n') => out.push(b'\n'),
                Some(&c) => out.push(c),
                None => break,
            }
        } else {
            out.push(b[i]);
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// GL_DST_COLOR GL_SRC_COLOR of `detail` over `base`, both stretched over
/// the same rect: 2 x base x detail per channel, at the base's resolution.
fn detail_blend(base: &Gd<Texture2D>, detail: &Gd<Texture2D>) -> Option<Gd<Texture2D>> {
    use godot::classes::image::{Format, Interpolation};
    use godot::classes::{Image, ImageTexture};
    let mut a = base.get_image()?;
    let mut d = detail.get_image()?;
    for img in [&mut a, &mut d] {
        if img.is_compressed() {
            img.decompress();
        }
        img.clear_mipmaps();
        img.convert(Format::RGBA8);
    }
    let (w, h) = (a.get_width(), a.get_height());
    d.resize_ex(w, h).interpolation(Interpolation::BILINEAR).done();
    let (ab, db) = (a.get_data(), d.get_data());
    let (ab, db) = (ab.as_slice(), db.as_slice());
    let mut out = vec![0u8; ab.len()];
    for i in 0..ab.len().min(db.len()) {
        out[i] = if i % 4 == 3 { ab[i] } else { ((2 * ab[i] as u32 * db[i] as u32) / 255).min(255) as u8 };
    }
    let mut img = Image::create_from_data(w, h, false, Format::RGBA8, &PackedByteArray::from(out.as_slice()))?;
    img.generate_mipmaps();
    ImageTexture::create_from_image(&img).map(|t| t.upcast::<Texture2D>())
}

/// Text measurement with the HUD fonts at cgDC.aspectScale.
#[derive(Clone, Copy)]
struct TextMetrics<'a> {
    fonts: &'a HudFonts,
    aspect: f32,
    /// cgDC.Assets.emoticons
    emoticons: &'a [Emoticon],
}

impl<'a> TextMetrics<'a> {
    /// `UI_Char_Width`. Note the original advances before it looks up the
    /// glyph, so it measures the character *after* the current one (the
    /// terminating NUL for the last one); text widths keep that quirk.
    fn char_width(&self, b: &[u8], i: &mut usize, scale: f32) -> f32 {
        if is_color_string(b, *i) {
            *i += 2;
            return 0.0;
        }
        if b.get(*i) == Some(&INDENT_MARKER) {
            *i += 1;
            return 0.0;
        }
        let f = self.fonts.pick(scale);
        if let Some((escaped, len, n)) = is_emoticon(self.emoticons, b, *i) {
            if escaped {
                *i += 1;
            } else {
                *i += len;
                return self.emoticons[n].width as f32 * self.emoticon_width(f, scale);
            }
        }
        *i += 1;
        let g = &f.glyphs[b.get(*i).copied().unwrap_or(0) as usize];
        g.xskip as f32 * self.aspect * scale * f.glyph_scale
    }

    /// `UI_EmoticonHeight`.
    fn emoticon_height(&self, f: &QFont, scale: f32) -> f32 {
        f.glyphs[b'[' as usize].height as f32 * scale * f.glyph_scale
    }

    /// `UI_EmoticonWidth`.
    fn emoticon_width(&self, f: &QFont, scale: f32) -> f32 {
        self.emoticon_height(f, scale) * self.aspect
    }

    /// `UI_Text_Width`.
    fn width(&self, text: &str, scale: f32) -> f32 {
        let b = text.as_bytes();
        let (indent, mut i) = parse_indent(b);
        let mut out = 0.0;
        while i < b.len() {
            out += self.char_width(b, &mut i, scale);
        }
        out + indent
    }

    /// `UI_Text_Height`.
    fn height(&self, text: &str, scale: f32) -> f32 {
        let f = self.fonts.pick(scale);
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

    /// `Item_Text_Wrap`: break `text` into lines of at most `width`, at
    /// blanks when possible, carrying the last colour code over.
    fn wrap(&self, text: &[u8], scale: f32, width: f32) -> Vec<u8> {
        fn skip_color_codes(b: &[u8], q: &mut usize, c: &mut [u8; 2]) {
            while is_color_string(b, *q) {
                *c = [b[*q], b[*q + 1]];
                *q += 2;
            }
        }
        let b = text;
        let eos = b.len();
        let mut out: Vec<u8> = Vec::with_capacity(eos + 16);
        let mut c = [0u8; 2];
        let mut p = 0;
        let mut indent_width = 0.0f32;
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
                text_width += self.char_width(b, &mut q, scale);
            }
            // no split has taken place: split mid-word (never stall)
            if eol == p {
                eol = q.max(p + 1).min(eos);
            }
            out.extend_from_slice(&b[p..eol]);
            p = eol;
            if out.last() == Some(&b'\n') {
                // deliberately broken line: clear the colour and indent
                c[0] = 0;
                indent_width = 0.0;
            } else {
                out.push(b'\n');
                if indent_width > 0.0 {
                    out.extend_from_slice(format!("{indent_width:.6}").as_bytes());
                    out.push(INDENT_MARKER);
                }
                // SkipWhiteSpace: leading blanks (and colour codes) of the next line
                while p < eos {
                    skip_color_codes(b, &mut p, &mut c);
                    if p < eos && b[p] != b'\n' && b[p].is_ascii_whitespace() {
                        p += 1;
                    } else {
                        break;
                    }
                }
            }
            if c[0] != 0 {
                out.extend_from_slice(&c);
            }
        }
        out
    }
}

// =====================================================================
// buildstat.cfg (cg_buildable.c)
// =====================================================================

/// `buildStat_t`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BuildStat {
    pub frame_shader: Option<String>,
    pub overlay_shader: Option<String>,
    pub no_power_shader: Option<String>,
    pub marked_shader: Option<String>,
    pub health_severe_color: Rgba,
    pub health_high_color: Rgba,
    pub health_elevated_color: Rgba,
    pub health_guarded_color: Rgba,
    pub health_low_color: Rgba,
    pub fore_color: Rgba,
    pub back_color: Rgba,
    pub frame_height: i32,
    pub frame_width: i32,
    pub health_padding: i32,
    pub overlay_height: i32,
    pub overlay_width: i32,
    pub vertical_margin: f32,
    pub horizontal_margin: f32,
    pub loaded: bool,
}

/// `CG_BuildableStatusParse`: ui/assets/{alien,human}/buildstat.cfg.
/// An unknown token leaves the config unloaded (no status display).
pub fn buildable_status_parse(text: &str) -> BuildStat {
    let mut bs = BuildStat::default();
    let mut toks = Vec::new();
    let mut defs = HashMap::new();
    if lex(text, &mut defs, &mut |_| None, &mut toks, 0).is_err() {
        return bs;
    }
    let word = |t: &Tok| match t {
        Tok::Word(w) | Tok::Str(w) => Some(w.clone()),
        _ => None,
    };
    let mut i = 0;
    let mut next = |i: &mut usize| -> Option<String> {
        let t = toks.get(*i).and_then(word);
        *i += 1;
        t
    };
    // PC_Float_Parse / PC_Int_Parse: an optional leading '-' token
    fn num(next: &mut dyn FnMut(&mut usize) -> Option<String>, i: &mut usize) -> Option<f32> {
        let t = next(i)?;
        if t == "-" {
            return next(i).and_then(|v| parse_float(&v)).map(|f| -f);
        }
        parse_float(&t)
    }
    // trap_R_RegisterShader("") is 0: no shader
    let shader = |s: Option<String>| s.filter(|s| !s.is_empty());
    while let Some(tok) = next(&mut i) {
        match tok.to_ascii_lowercase().as_str() {
            "frameshader" => bs.frame_shader = shader(next(&mut i)),
            "overlayshader" => bs.overlay_shader = shader(next(&mut i)),
            "nopowershader" => bs.no_power_shader = shader(next(&mut i)),
            "markedshader" => bs.marked_shader = shader(next(&mut i)),
            k @ ("healthseverecolor" | "healthhighcolor" | "healthelevatedcolor" | "healthguardedcolor" | "healthlowcolor" | "forecolor" | "backcolor") => {
                let mut c = [0.0f32; 4];
                let mut ok = true;
                for v in c.iter_mut() {
                    match num(&mut next, &mut i) {
                        Some(f) => *v = f,
                        None => ok = false,
                    }
                }
                if ok {
                    *match k {
                        "healthseverecolor" => &mut bs.health_severe_color,
                        "healthhighcolor" => &mut bs.health_high_color,
                        "healthelevatedcolor" => &mut bs.health_elevated_color,
                        "healthguardedcolor" => &mut bs.health_guarded_color,
                        "healthlowcolor" => &mut bs.health_low_color,
                        "forecolor" => &mut bs.fore_color,
                        _ => &mut bs.back_color,
                    } = c;
                }
            }
            k @ ("frameheight" | "framewidth" | "healthpadding" | "overlayheight" | "overlaywidth") => {
                if let Some(v) = num(&mut next, &mut i) {
                    *match k {
                        "frameheight" => &mut bs.frame_height,
                        "framewidth" => &mut bs.frame_width,
                        "healthpadding" => &mut bs.health_padding,
                        "overlayheight" => &mut bs.overlay_height,
                        _ => &mut bs.overlay_width,
                    } = v as i32;
                }
            }
            "verticalmargin" => {
                if let Some(v) = num(&mut next, &mut i) {
                    bs.vertical_margin = v;
                }
            }
            "horizontalmargin" => {
                if let Some(v) = num(&mut next, &mut i) {
                    bs.horizontal_margin = v;
                }
            }
            _ => {
                bs.loaded = false;
                return bs;
            }
        }
    }
    bs.loaded = true;
    bs
}

// =====================================================================
// cg_tutorial.c key names
// =====================================================================

/// cg_tutorial.c `bindings[]` (command, humanName).
const TUTORIAL_BINDINGS: &[(&str, &str)] = &[
    ("+button2", "Activate Upgrade"),
    ("+speed", "Run/Walk"),
    ("+button6", "Dodge"),
    ("+button8", "Sprint"),
    ("+moveup", "Jump"),
    ("+movedown", "Crouch"),
    ("+attack", "Primary Attack"),
    ("+button5", "Secondary Attack"),
    ("reload", "Reload"),
    ("buy ammo", "Buy Ammo"),
    ("itemact medkit", "Use Medkit"),
    ("+button7", "Use Structure/Evolve"),
    ("deconstruct", "Deconstruct Structure"),
    ("weapprev", "Previous Upgrade"),
    ("weapnext", "Next Upgrade"),
];

/// `CG_GetBindings` + `CG_KeyNameForCommand` over the client's bind table
/// (`trap_Key_GetBindingBuf` for keys below K_LAST_KEY): "KEY", "KEY or
/// KEY2", or `"Human Name" (unbound)`.
fn key_name_for_command(binds: &[String], command: &str) -> String {
    for (cmd, human) in TUTORIAL_BINDINGS {
        if !cmd.eq_ignore_ascii_case(command) {
            continue;
        }
        let keys: Vec<i32> = (0..crate::keys::K_LAST_KEY.min(binds.len() as i32)).filter(|&j| !binds[j as usize].is_empty() && binds[j as usize].eq_ignore_ascii_case(cmd)).take(2).collect();
        return match keys.as_slice() {
            [] => format!("\"{human}\" (unbound)"),
            [a] => crate::keys::key_keynum_to_string(*a).to_uppercase(),
            [a, b, ..] => format!("{} or {}", crate::keys::key_keynum_to_string(*a).to_uppercase(), crate::keys::key_keynum_to_string(*b).to_uppercase()),
        };
    }
    String::new()
}

/// `CG_KeyBinding`: the name of the first key bound to exactly `bind`,
/// else "\\" + bind.
fn cg_key_binding(binds: &[String], bind: &str) -> String {
    (0..crate::keys::K_LAST_KEY.min(binds.len() as i32))
        .find(|&j| binds[j as usize].eq_ignore_ascii_case(bind))
        .map(|j| crate::keys::key_keynum_to_string(j))
        .unwrap_or_else(|| format!("\\{bind}"))
}

/// `CG_ChargeProgress` for the viewed weapon, STAT_STATE and STAT_MISC.
fn charge_progress(weapon: i32, state: i32, misc: i32) -> f32 {
    let (min, max) = match weapon_id(weapon) {
        Some(WeaponId::Level3) => (k::LEVEL3_POUNCE_TIME_MIN, k::LEVEL3_POUNCE_TIME),
        Some(WeaponId::Level3upg) => (k::LEVEL3_POUNCE_TIME_MIN, k::LEVEL3_POUNCE_TIME_UPG),
        Some(WeaponId::Level4) => {
            if state & k::SS_CHARGING != 0 {
                (0, k::LEVEL4_TRAMPLE_DURATION)
            } else {
                (k::LEVEL4_TRAMPLE_CHARGE_MIN, k::LEVEL4_TRAMPLE_CHARGE_MAX)
            }
        }
        Some(WeaponId::Lcannon) => (k::LCANNON_CHARGE_TIME_MIN, k::LCANNON_CHARGE_TIME_MAX),
        _ => (0, 0),
    };
    if max - min <= 0 {
        return 0.0;
    }
    ((misc as f32 - min as f32) / (max - min) as f32).clamp(0.0, 1.0)
}

// =====================================================================
// per-frame drawing (cgame logic -> draw commands in 640x480 space)
// =====================================================================

type Rgba = [f32; 4];
const WHITE: Rgba = [1.0; 4];
const UV_FULL: [f32; 4] = [0.0, 0.0, 1.0, 1.0];

/// A draw command in virtual 640x480 coordinates: `trap_R_DrawStretchPic`
/// (image, rect, colour, texture coordinates s1 t1 s2 t2, plus the rotation
/// of a `tcmod rotate` stage) or a white-shader fill.
enum Cmd {
    Pic { img: String, r: [f32; 4], c: Rgba, rot: f32, uv: [f32; 4] },
    Fill { r: [f32; 4], c: Rgba },
    /// a stretch pic with per-corner texture coordinates (tcMod'ed stages):
    /// top-left, top-right, bottom-right, bottom-left
    Quad { img: String, r: [f32; 4], c: Rgba, st: [[f32; 2]; 4] },
    /// a pic with a second pic drawn over it with blendFunc GL_DST_COLOR
    /// GL_SRC_COLOR (2 x dst x src), both stretched over the rect
    PicDetail { img: String, detail: String, r: [f32; 4] },
}

/// Resolved draw operation (textures looked up).
enum Op {
    Pic { tex: Gd<Texture2D>, r: [f32; 4], c: Rgba, rot: f32, uv: [f32; 4] },
    Fill { r: [f32; 4], c: Rgba },
    Quad { tex: Gd<Texture2D>, r: [f32; 4], c: Rgba, st: [[f32; 2]; 4] },
}

#[derive(Clone, Debug, Default)]
struct WeaponHud {
    icon: Option<String>,
    /// crossHairSize and the shader's stages.
    crosshair: Option<(f32, Vec<Stage2D>)>,
}

/// Loaded HUD data (`CG_LoadHudMenu` + the media of `CG_RegisterGraphics` /
/// `CG_RegisterWeapon` used here).
struct HudData {
    menus: MenuSet,
    od: HashMap<i32, Od>,
    weapons: Vec<WeaponHud>,
    /// healthCross, 2X, 3X, Medkit, Poisoned (absent ones fall back to the
    /// plain cross: neither data set ships the others).
    cross: [String; 5],
    fonts: Option<HudFonts>,
    /// cgs.alienBuildStat / cgs.humanBuildStat
    alien_build_stat: BuildStat,
    human_build_stat: BuildStat,
    /// cgDC.Assets.emoticons (cg_emoticons 1)
    emoticons: Vec<Emoticon>,
}

/// cg_scanner.c entityPositions.
#[derive(Default)]
struct EntityPositions {
    origin: Vec3,
    vangles: Vec3,
    last_update: i32,
    alien_buildables: Vec<Vec3>,
    human_buildables: Vec<Vec3>,
    alien_clients: Vec<Vec3>,
    human_clients: Vec<Vec3>,
}

/// cg_draw.c NO_CREDITS_TIME / BUILD_DELAY_TIME.
const NO_CREDITS_TIME: i32 = 2000;
const BUILD_DELAY_TIME: i32 = 2000;

/// The cg.* state the ownerdraws keep across frames.
struct HudState {
    /// cg.centerPrint (wrapped), cg.centerPrintTime, cg.centerPrintLines
    center_print: String,
    center_print_time: i32,
    center_print_lines: i32,
    /// levelshots/<map> is in the data (else `CG_DrawLevelShot` shows gfx/2d/load_screen)
    levelshot: bool,
    /// cg.lastEvolveAttempt / cg.lastBuildAttempt (level time)
    last_evolve_attempt: i32,
    last_build_attempt: i32,
    last_cross: Option<String>,
    cross_fade: f32,
    weapon_select: i32,
    weapon_select_time: i32,
    crosshair_client: i32,
    crosshair_client_time: i32,
    crosshair_buildable: i32,
    /// cg.nearUsableBuildable
    near_usable_buildable: Option<BuildableId>,
    /// cg.chargeMeterAlpha / cg.chargeMeterValue
    charge_meter_alpha: f32,
    charge_meter_value: f32,
    /// speedSamples / oldestSpeedSample / maxSpeedSample
    speed_samples: Vec<f32>,
    oldest_speed_sample: usize,
    max_speed_sample: usize,
    /// CG_DrawFPS statics
    fps_times: [i32; FPS_FRAMES],
    fps_index: usize,
    fps_previous: u64,
    /// cg.consoleLines (time, text incl. the newline)
    console: Vec<(i32, String)>,
    /// how many TremGame feed lines were already added
    feed_seen: u64,
    /// centity_t.buildableStatus (visible, lastTime) by entity number
    buildable_status: HashMap<usize, (bool, i32)>,
    positions: EntityPositions,
    last_time: i32,
    /// TremHud.scanner_without_helmet
    force_scanner: bool,
    /// Menu_AspectCompensate'd copies of the HUD menus and their aspectScale
    compensated: Option<(f32, Vec<MenuDef>)>,
    /// cg.lastHealth / cg.painBlendValue / cg.painBlendTarget
    last_health: i32,
    pain_blend_value: f32,
    pain_blend_target: f32,
    /// (PERS_SPAWN_COUNT, clientNum, class) at the last CG_ResetPainBlend:
    /// CG_Respawn, a follow change and EV_ALIEN_EVOLVE reset the blend
    pain_reset_key: Option<(i32, i32, i32)>,
}

impl Default for HudState {
    fn default() -> Self {
        HudState {
            center_print: String::new(),
            center_print_time: 0,
            center_print_lines: 0,
            levelshot: false,
            last_evolve_attempt: 0,
            last_build_attempt: 0,
            last_cross: None,
            cross_fade: 0.0,
            weapon_select: 0,
            weapon_select_time: 0,
            crosshair_client: 0,
            crosshair_client_time: 0,
            crosshair_buildable: -1,
            near_usable_buildable: None,
            charge_meter_alpha: 0.0,
            charge_meter_value: 0.0,
            speed_samples: vec![0.0; SPEEDOMETER_NUM_SAMPLES],
            oldest_speed_sample: 0,
            max_speed_sample: 0,
            fps_times: [0; FPS_FRAMES],
            fps_index: 0,
            fps_previous: 0,
            console: Vec::new(),
            feed_seen: 0,
            buildable_status: HashMap::new(),
            positions: EntityPositions::default(),
            last_time: 0,
            force_scanner: false,
            compensated: None,
            last_health: 0,
            pain_blend_value: 0.0,
            pain_blend_target: 0.0,
            pain_reset_key: None,
        }
    }
}

impl HudState {
    /// `CG_AddNotifyText` for every TremGame feed line (chat, prints,
    /// obituaries) not added yet; `feed` holds the last lines and `total`
    /// counts every line ever added to it.
    fn add_notify_lines(&mut self, feed: &[(f64, String)], total: u64, time: i32) {
        if total < self.feed_seen {
            // a new TremGame
            self.feed_seen = 0;
        }
        let new = ((total - self.feed_seen) as usize).min(feed.len());
        self.feed_seen = total;
        for (_, line) in &feed[feed.len() - new..] {
            if self.console.len() == MAX_CONSOLE_LINES {
                self.console.remove(0);
            }
            self.console.push((time, format!("{line}\n")));
        }
    }

    /// CG_DrawActiveFrame's `CG_RemoveNotifyLine` after cg_consoleLatency.
    fn expire_notify_lines(&mut self, time: i32, latency: i32) {
        for l in &mut self.console {
            // a new level restarts level.time
            if l.0 > time {
                l.0 = time;
            }
        }
        if latency > 0 && self.console.first().is_some_and(|l| l.0 + latency < time) {
            self.console.remove(0);
        }
    }
}

const HUMAN_SCANNER_UPDATE_PERIOD: i32 = 700;
const WEAPON_SELECT_TIME: i32 = 1400;
const CROSSHAIR_CLIENT_TIMEOUT: i32 = 1000;
const FADE_TIME: i32 = 200;
const PIES: [&str; 8] = [
    "ui/assets/neutral/1_5pie",
    "ui/assets/neutral/3_0pie",
    "ui/assets/neutral/4_5pie",
    "ui/assets/neutral/6_0pie",
    "ui/assets/neutral/7_5pie",
    "ui/assets/neutral/9_0pie",
    "ui/assets/neutral/10_5pie",
    "ui/assets/neutral/12_0pie",
];
const NUMBERS: [&str; 11] = [
    "gfx/2d/numbers/zero_32b",
    "gfx/2d/numbers/one_32b",
    "gfx/2d/numbers/two_32b",
    "gfx/2d/numbers/three_32b",
    "gfx/2d/numbers/four_32b",
    "gfx/2d/numbers/five_32b",
    "gfx/2d/numbers/six_32b",
    "gfx/2d/numbers/seven_32b",
    "gfx/2d/numbers/eight_32b",
    "gfx/2d/numbers/nine_32b",
    "gfx/2d/numbers/minus_32b",
];
/// cgs.media.teamOverlayShader / upgradeClassIconShader
const TEAM_OVERLAY_SHADER: &str = "gfx/2d/teamoverlay";
const UPGRADE_CLASS_ICON: &str = "icons/icona_upgrade.tga";
/// assetGlobalDef gradientBar (loading.menu)
const GRADIENT_BAR: &str = "ui/assets/gradientbar2.tga";

fn lerp4(t: f32, a: &Rgba, b: &Rgba) -> Rgba {
    [a[0] + t * (b[0] - a[0]), a[1] + t * (b[1] - a[1]), a[2] + t * (b[2] - a[2]), a[3] + t * (b[3] - a[3])]
}

fn is_builder_weapon(wp: i32) -> bool {
    // (+ Tremulous 1.1.0's advanced construction kit, dec041)
    wp == WeaponId::Abuild as i32 || wp == WeaponId::Abuildupg as i32 || wp == WeaponId::Ckit as i32 || wp == WeaponId::Ackit as i32
}

fn weapon_id(n: i32) -> Option<WeaponId> {
    if n > 0 && n < 256 {
        WeaponId::from_num(n as u8)
    } else {
        None
    }
}

/// `BG_GetPlayerWeapon`.
fn player_weapon(ps: &PlayerState) -> i32 {
    let nw = ps.pers(Pers::Newweapon);
    if nw != 0 {
        nw
    } else {
        ps.weapon
    }
}

/// `CG_DrawStrlen`: length without colour codes.
fn draw_strlen(s: &str) -> usize {
    let b = s.as_bytes();
    let (mut i, mut n) = (0, 0);
    while i < b.len() {
        if is_color_string(b, i) {
            i += 2;
        } else {
            n += 1;
            i += 1;
        }
    }
    n
}

/// cg.refdef: view origin, axis and the tangents of the half field of view.
#[derive(Clone, Copy, Debug)]
struct Refdef {
    vieworg: Vec3,
    axis: [Vec3; 3],
    /// tan(fov_x / 2), tan(fov_y / 2)
    px: f32,
    py: f32,
}

impl Refdef {
    /// From a camera (Quake origin, view angles, fov_x and fov_y in degrees).
    fn new(vieworg: Vec3, viewangles: Vec3, fov_x: f32, fov_y: f32) -> Refdef {
        let (f, r, u) = angle_vectors(&viewangles);
        let px = (fov_x.to_radians() / 2.0).tan();
        let py = (fov_y.to_radians() / 2.0).tan();
        // viewaxis[1] points left
        Refdef { vieworg, axis: [f, [-r[0], -r[1], -r[2]], u], px, py }
    }
}

/// Wall-clock values (trap_Milliseconds, trap_RealTime).
#[derive(Clone, Copy, Debug, Default)]
struct RealTime {
    ms: u64,
    hour: i32,
    min: i32,
    sec: i32,
}

struct Frame<'a> {
    w: &'a World,
    /// cg.snap->ps / cg.predictedPlayerState (the local client's; a copy of
    /// the followed player's with PMF_FOLLOW while following)
    ps: &'a PlayerState,
    /// cg.clientNum
    me: usize,
    /// cg.snap->ps.clientNum: the client whose view is shown
    view: usize,
    time: i32,
    frametime: i32,
    /// cgDC.aspectScale
    aspect: f32,
    xs: f32,
    ys: f32,
    /// cg.renderingThirdPerson
    third_person: bool,
    /// cg.chaseFollow
    chase_follow: bool,
    /// the loading screen's facts (`CG_DrawLoadingScreen`)
    loading: Option<&'a crate::game::LoadInfo>,
    refdef: Refdef,
    cvars: Cvars,
    real: RealTime,
    data: &'a HudData,
    st: &'a mut HudState,
    /// the client's bind table (`trap_Key_GetBindingBuf`)
    binds: &'a [String],
    out: Vec<Cmd>,
}

impl<'a> Frame<'a> {
    fn pic(&mut self, img: &str, x: f32, y: f32, w: f32, h: f32, c: Rgba) {
        self.pic_uv(img, x, y, w, h, c, UV_FULL);
    }
    /// `trap_R_DrawStretchPic` with texture coordinates.
    #[allow(clippy::too_many_arguments)]
    fn pic_uv(&mut self, img: &str, x: f32, y: f32, w: f32, h: f32, c: Rgba, uv: [f32; 4]) {
        // (an alpha-0 draw still shows a missing image: the default shader ignores the colour)
        if !img.is_empty() {
            self.out.push(Cmd::Pic { img: img.to_string(), r: [x, y, w, h], c, rot: 0.0, uv });
        }
    }
    /// `CG_DrawPic` of cgs.media.whiteShader / `CG_FillRect`.
    fn fill(&mut self, x: f32, y: f32, w: f32, h: f32, c: Rgba) {
        if c[3] > 0.0 && w != 0.0 && h != 0.0 {
            self.out.push(Cmd::Fill { r: [x, y, w, h], c });
        }
    }
    fn stat(&self, s: Stat) -> i32 {
        self.ps.stats[s as usize]
    }
    fn team(&self) -> i32 {
        self.stat(Stat::Team)
    }
    fn intermission_started(&self) -> bool {
        self.w.level.intermission_time != 0
    }
    fn metrics(&self) -> Option<TextMetrics<'a>> {
        let data: &'a HudData = self.data;
        data.fonts.as_ref().map(|fonts| TextMetrics { fonts, aspect: self.aspect, emoticons: &data.emoticons })
    }
    fn text_width(&self, text: &str, scale: f32) -> f32 {
        self.metrics().map(|m| m.width(text, scale)).unwrap_or(0.0)
    }
    fn text_height(&self, text: &str, scale: f32) -> f32 {
        self.metrics().map(|m| m.height(text, scale)).unwrap_or(0.0)
    }

    // ------------------------------------------------------------ text
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
        if c[3] > 0.0 && w > 0.0 && h > 0.0 && !g.shader.is_empty() {
            self.out.push(Cmd::Pic { img: g.shader.clone(), r: [x, y, w, h], c, rot: 0.0, uv: [g.s, g.t, g.s2, g.t2] });
        }
    }

    /// `UI_Text_Paint_Generic` (with the emoticons of `BG_LoadEmoticons`).
    #[allow(clippy::too_many_arguments)]
    fn text_paint_generic(&mut self, mut x: f32, y: f32, scale: f32, adjust: f32, text: &str, color: Rgba, style: i32, limit: usize, mut max_x: Option<&mut f32>) {
        let Some(m) = self.metrics() else { return };
        let f = m.fonts.pick(scale);
        let use_scale = scale * f.glyph_scale;
        let b = text.as_bytes();
        let len = if limit > 0 && b.len() > limit { limit } else { b.len() };
        let mut new_color = color;
        let (indent, mut s) = parse_indent(b);
        x += indent;
        let mut count = 0;
        while s < b.len() && count < len {
            let mut t = s;
            let char_width = m.char_width(b, &mut t, scale);
            let g = &f.glyphs[b[s] as usize];
            if let Some(mx) = max_x.as_deref_mut() {
                if char_width + x > *mx {
                    *mx = 0.0;
                    break;
                }
            }
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
            if let Some((escaped, elen, n)) = is_emoticon(m.emoticons, b, s) {
                if escaped {
                    s += 1;
                } else {
                    let e = &m.emoticons[n];
                    let yadj = use_scale * g.top as f32;
                    let (ew, eh) = (m.emoticon_width(f, scale), m.emoticon_height(f, scale));
                    // DC->setColor( NULL ): drawn uncoloured
                    self.pic(&e.shader, x, y - yadj, ew * e.width as f32, eh, WHITE);
                    x += ew * e.width as f32 + adjust;
                    s += elen;
                    count += e.width as usize;
                    continue;
                }
            }
            let mut main = new_color;
            if style == ITEM_TEXTSTYLE_SHADOWED || style == ITEM_TEXTSTYLE_SHADOWEDMORE {
                let ofs = if style == ITEM_TEXTSTYLE_SHADOWED { 1.0 } else { 2.0 };
                self.paint_char(x + ofs, y + ofs, use_scale, g, 0.0, [0.0, 0.0, 0.0, new_color[3]]);
            } else if style == ITEM_TEXTSTYLE_NEON {
                let glow = [new_color[0], new_color[1], new_color[2], new_color[3] * 0.2];
                self.paint_char(x, y, use_scale, g, 6.0, glow);
                self.paint_char(x, y, use_scale, g, 4.0, glow);
                self.paint_char(x, y, use_scale, g, 2.0, new_color);
                main = WHITE;
            }
            self.paint_char(x, y, use_scale, g, 0.0, main);
            x += g.xskip as f32 * self.aspect * use_scale + adjust;
            s += 1;
            count += 1;
        }
        if let Some(mx) = max_x {
            *mx = x;
        }
    }

    /// `UI_Text_Paint`.
    fn text_paint(&mut self, x: f32, y: f32, scale: f32, color: Rgba, text: &str, style: i32) {
        self.text_paint_generic(x, y, scale, 0.0, text, color, style, 0, None);
    }

    /// `UI_Text_Paint_Limit`.
    fn text_paint_limit(&mut self, max_x: &mut f32, x: f32, y: f32, scale: f32, color: Rgba, text: &str) {
        self.text_paint_generic(x, y, scale, 0.0, text, color, ITEM_TEXTSTYLE_NORMAL, 0, Some(max_x));
    }

    /// `CG_AlignText`: the baseline position of `text` aligned in `r`
    /// (`w`/`h` are used instead of the text's extents when `scale` <= 0).
    #[allow(clippy::too_many_arguments)]
    fn align_text(&self, r: [f32; 4], text: &str, scale: f32, mut w: f32, mut h: f32, align: i32, valign: i32) -> (f32, f32) {
        if scale > 0.0 {
            w = self.text_width(text, scale);
            h = self.text_height(text, scale);
        }
        let tx = match align {
            ITEM_ALIGN_RIGHT => r[2] - w,
            ITEM_ALIGN_CENTER => (r[2] - w) / 2.0,
            _ => 0.0,
        };
        let ty = match valign {
            VALIGN_TOP => h,
            VALIGN_CENTER => h + (r[3] - h) / 2.0,
            VALIGN_NONE => 0.0,
            _ => r[3],
        };
        (r[0] + tx, r[1] + ty)
    }

    /// `Item_SetTextExtents`: the text rect (x, baseline y, w, h) of `text`
    /// in an item at `r` with the given alignment; `item_text` is the item's
    /// own text (the original centres/right-aligns by its width).
    #[allow(clippy::too_many_arguments)]
    fn text_extents(&self, r: [f32; 4], item_text: Option<&str>, text: &str, scale: f32, align: i32, valign: i32, align_x: f32, align_y: f32) -> [f32; 4] {
        let original_width = if align == ITEM_ALIGN_CENTER || align == ITEM_ALIGN_RIGHT { item_text.map(|t| self.text_width(t, scale)).unwrap_or(0.0) } else { 0.0 };
        let w = self.text_width(text, scale);
        let h = self.text_height(text, scale);
        let y = match valign {
            VALIGN_BOTTOM => align_y + r[3],
            VALIGN_CENTER => align_y + (h + r[3]) / 2.0,
            VALIGN_TOP => align_y + h,
            _ => 0.0,
        };
        let x = match align {
            ITEM_ALIGN_LEFT => align_x,
            ITEM_ALIGN_CENTER => align_x + (r[2] - original_width) / 2.0,
            ITEM_ALIGN_RIGHT => align_x + r[2] - original_width,
            _ => 0.0,
        };
        [r[0] + x, r[1] + y, w, h]
    }

    /// `Item_Text_Wrapped_Paint` (without ui_textWrapCache, which only
    /// caches the same result).
    #[allow(clippy::too_many_arguments)]
    fn text_wrapped_paint(&mut self, r: [f32; 4], align_x: f32, align_y: f32, scale: f32, style: i32, align: i32, valign: i32, color: Rgba, text: &str) {
        let Some(m) = self.metrics() else { return };
        if text.is_empty() {
            return;
        }
        let font_height = m.height("M", scale);
        let line_spacing = font_height * 0.4;
        let line_height = font_height + line_spacing;
        let (x, y, w, h) = (r[0] + align_x, r[1] + align_y, r[2] - 2.0 * align_x, r[3] - 2.0 * align_y);
        let wrapped = m.wrap(text.as_bytes(), scale, w);
        let total_lines = wrapped.iter().filter(|&&c| c == b'\n').count() as i32;
        let paint_lines = (((h + line_spacing) / line_height).floor() as i32).min(total_lines);
        let text_height = paint_lines as f32 * line_height - line_spacing;
        let paint_y = match valign {
            VALIGN_CENTER => y + (h - text_height) / 2.0,
            VALIGN_TOP => y,
            _ => y + (h - text_height),
        };
        let mut p = 0;
        let mut line_num = 0;
        for i in 0..wrapped.len() {
            if line_num >= paint_lines {
                break;
            }
            if wrapped[i] == b'\n' {
                let line = String::from_utf8_lossy(&wrapped[p..i]).into_owned();
                p = i + 1;
                let lr = [x, paint_y + line_num as f32 * line_height, w, line_height];
                let ext = self.text_extents(lr, Some(&line), &line, scale, align, VALIGN_TOP, 0.0, 0.0);
                self.text_paint(ext[0], ext[1], scale, color, &line, style);
                line_num += 1;
            }
        }
    }

    /// `UI_DrawTextBlock`.
    #[allow(clippy::too_many_arguments)]
    fn draw_text_block(&mut self, r: [f32; 4], text_x: f32, text_y: f32, color: Rgba, scale: f32, align: i32, valign: i32, style: i32, text: &str) {
        self.text_wrapped_paint(r, text_x, text_y, scale, style, align, valign, color, text);
    }

    /// `CG_DrawField`.
    #[allow(clippy::too_many_arguments)]
    fn field(&mut self, mut x: f32, y: f32, mut width: i32, cw: f32, ch: f32, mut value: i32, c: Rgba) {
        let cw = if cw == 0.0 { 32.0 } else { cw };
        let ch = if ch == 0.0 { 48.0 } else { ch };
        if width < 1 {
            return;
        }
        width = width.min(4);
        value = match width {
            1 => value.clamp(0, 9),
            2 => value.clamp(-9, 99),
            3 => value.clamp(-99, 999),
            _ => value.clamp(-999, 9999),
        };
        let num = value.to_string();
        let l = (num.len() as i32).min(width);
        x += 2.0 * self.aspect + cw * (width - l) as f32;
        for ch_ in num.chars().take(l as usize) {
            let frame = if ch_ == '-' { 10 } else { ch_ as usize - '0' as usize };
            self.pic(NUMBERS[frame], x, y, cw, ch, c);
            x += cw;
        }
    }

    /// `CG_DrawFieldPadded` (int coordinates, as the C signature truncates).
    #[allow(clippy::too_many_arguments)]
    fn field_padded(&mut self, x: i32, y: i32, mut width: i32, cw: i32, ch: i32, mut value: i32, c: Rgba) {
        let cw = if cw == 0 { 32 } else { cw };
        let ch = if ch == 0 { 48 } else { ch };
        if width < 1 {
            return;
        }
        width = width.min(4);
        value = match width {
            1 => value.clamp(0, 9),
            2 => value.clamp(-9, 99),
            3 => value.clamp(-99, 999),
            _ => value.clamp(-999, 9999),
        };
        let num: Vec<char> = value.to_string().chars().collect();
        let mut l = (num.len() as i32).min(width);
        let org_l = l;
        // x is an int in the C code: the aspect offset truncates
        let mut x = (x as f32 + 2.0 * self.aspect) as i32 as f32;
        let mut p = 0;
        while p < num.len() && l > 0 {
            if width > org_l {
                self.pic(NUMBERS[0], x, y as f32, cw as f32, ch as f32, c);
                width -= 1;
                x += cw as f32;
                continue;
            }
            let frame = if num[p] == '-' { 10 } else { num[p] as usize - '0' as usize };
            self.pic(NUMBERS[frame], x, y as f32, cw as f32, ch as f32, c);
            x += cw as f32;
            p += 1;
            l -= 1;
        }
    }

    // ------------------------------------------------------------ menu
    /// `Menu_Paint` (forced: CG_Draw2D paints the HUD with forcePaint).
    fn paint_menu(&mut self, menu: &MenuDef) {
        if menu.ownerdraw_flag != 0 {
            // cgDC.ownerDrawVisible (CG_OwnerDrawVisible) is always false
            return;
        }
        if menu.fullscreen {
            if let Some(bg) = &menu.background {
                self.pic(bg, 0.0, 0.0, 640.0, 480.0, WHITE);
            }
        }
        self.window_paint(menu.rect, menu.style, menu.border, menu.border_size, menu.fore, true, menu.back, menu.background.as_deref());
        self.border_paint(menu.rect, menu.style, menu.border, menu.border_size, menu.border_color);
        for it in &menu.items {
            self.item_paint(menu, it);
        }
    }

    /// `Window_Paint`.
    #[allow(clippy::too_many_arguments)]
    fn window_paint(&mut self, r: [f32; 4], style: i32, border: i32, border_size: f32, fore: Rgba, fore_set: bool, back: Rgba, background: Option<&str>) {
        if style == 0 && border == 0 {
            return;
        }
        let mut f = r;
        if border != 0 {
            f[0] += border_size;
            f[1] += border_size;
            f[2] -= border_size + 1.0;
            f[3] -= border_size + 1.0;
        }
        match style {
            WINDOW_STYLE_FILLED => match background {
                Some(bg) => self.pic(bg, f[0], f[1], f[2], f[3], back),
                None => self.fill(f[0], f[1], f[2], f[3], back),
            },
            // GradientBar_Paint
            WINDOW_STYLE_GRADIENT => self.pic(GRADIENT_BAR, f[0], f[1], f[2], f[3], back),
            WINDOW_STYLE_SHADER => {
                if let Some(bg) = background {
                    self.pic(bg, f[0], f[1], f[2], f[3], if fore_set { fore } else { WHITE });
                }
            }
            _ => {}
        }
    }

    /// `Border_Paint` (CG_DrawRect / CG_DrawTopBottom / CG_DrawSides).
    fn border_paint(&mut self, r: [f32; 4], style: i32, border: i32, size: f32, c: Rgba) {
        if style == 0 && border == 0 {
            return;
        }
        let [x, y, w, h] = r;
        if border == WINDOW_BORDER_FULL || border == WINDOW_BORDER_HORZ {
            self.fill(x, y, w, size, c);
            self.fill(x, y + h - size, w, size, c);
        }
        if border == WINDOW_BORDER_FULL || border == WINDOW_BORDER_VERT {
            self.fill(x, y, size, h, c);
            self.fill(x + w - size, y, size, h, c);
        }
    }

    /// `Item_Paint` for the item types the HUD menus use.
    fn item_paint(&mut self, menu: &MenuDef, it: &ItemDef) {
        // ownerDrawFlags: CG_OwnerDrawVisible is always false
        if it.ownerdraw_flag != 0 || !it.visible {
            return;
        }
        // (cvarTest show/hide: no HUD item uses it)
        let r = [it.rect[0] + menu.rect[0], it.rect[1] + menu.rect[1], it.rect[2], it.rect[3]];
        self.window_paint(r, it.style, it.border, it.border_size, it.fore, it.fore_set, it.back, it.background.as_deref());
        match it.item_type {
            ITEM_TYPE_OWNERDRAW => {
                if let Some(od) = self.data.od.get(&it.ownerdraw).copied() {
                    self.owner_draw_paint(it, r, od);
                }
            }
            ITEM_TYPE_TEXT | ITEM_TYPE_BUTTON => self.item_text_paint(it, r),
            _ => {}
        }
        self.border_paint(r, it.style, it.border, it.border_size, it.border_color);
    }

    /// `Item_TextColor` (no focus, fade or enableCvar in the HUD menus).
    fn item_text_color(&self, it: &ItemDef) -> Rgba {
        if it.text_style == ITEM_TEXTSTYLE_BLINK && (self.real.ms / BLINK_DIVISOR) & 1 == 0 {
            let f = it.fore;
            let low = [0.8 * f[0], 0.8 * f[1], 0.8 * f[2], 0.8 * f[3]];
            return lerp4(0.5 + 0.5 * (self.real.ms as f32 / PULSE_DIVISOR).sin(), &f, &low);
        }
        it.fore
    }

    /// `Item_Text_Paint` (cvar texts are empty: cgame has no such cvars here).
    fn item_text_paint(&mut self, it: &ItemDef, r: [f32; 4]) {
        let color = self.item_text_color(it);
        let text = it.text.clone().unwrap_or_default();
        if it.wrapped {
            self.text_wrapped_paint(r, it.text_align_x, it.text_align_y, it.text_scale, it.text_style, it.text_align, it.text_valign, color, &text);
            return;
        }
        if text.is_empty() {
            return;
        }
        let ext = self.text_extents(r, it.text.as_deref(), &text, it.text_scale, it.text_align, it.text_valign, it.text_align_x, it.text_align_y);
        self.text_paint(ext[0], ext[1], it.text_scale, color, &text, it.text_style);
    }

    /// `CG_GetValue` (for addColorRange items).
    fn get_value(&self, od: Od) -> f32 {
        let weapon = player_weapon(self.ps);
        match od {
            Od::AmmoValue if weapon != 0 => self.ps.ammo as f32,
            Od::ClipsValue if weapon != 0 => self.ps.clips as f32,
            Od::Health => self.stat(Stat::Health) as f32,
            _ => -1.0,
        }
    }

    /// `Item_OwnerDraw_Paint`: the item's colour (colour ranges, blink) and
    /// CG_OwnerDraw with the item's rect, text offsets and style.
    fn owner_draw_paint(&mut self, it: &ItemDef, r: [f32; 4], od: Od) {
        let mut color = it.fore;
        if !it.color_ranges.is_empty() {
            let f = self.get_value(od);
            if let Some((_, _, c)) = it.color_ranges.iter().find(|(lo, hi, _)| f >= *lo && f <= *hi) {
                color = *c;
            }
        }
        if it.text_style == ITEM_TEXTSTYLE_BLINK && (self.real.ms / BLINK_DIVISOR) & 1 == 0 {
            color = self.item_text_color(it);
        }
        self.owner_draw(od, it, r, color);
    }

    /// `CG_OwnerDraw`.
    fn owner_draw(&mut self, od: Od, it: &ItemDef, r: [f32; 4], fore: Rgba) {
        let back = it.back;
        let shader = it.background.clone().unwrap_or_default();
        let (scale, tx, ty, align, valign, style) = (it.text_scale, it.text_align_x, it.text_align_y, it.text_align, it.text_valign, it.text_style);
        match od {
            Od::CreditsValue => self.credits_value(r, fore, true),
            Od::CreditsFraction => self.credits_fraction(r, fore, &shader),
            Od::CreditsValueNopad => self.credits_value(r, fore, false),
            Od::Stamina(n) => self.stamina(n, r, back, fore, &shader),
            Od::StaminaBolt => self.stamina_bolt(r, back, fore, &shader),
            Od::AmmoValue => self.ammo_value(r, fore),
            Od::ClipsValue => self.clips_value(r, fore),
            Od::BuildTimer => self.build_timer(r, fore),
            Od::Health => self.health_value(r, fore),
            Od::HealthCross => self.health_cross(r, fore),
            Od::ChargeBarBg => self.charge_bar_bg(r, fore, &shader),
            Od::ChargeBar => self.charge_bar(r, fore, &shader),
            Od::ClipsRing => self.clips_ring(r, back, fore, &shader),
            Od::BuildTimerRing => self.build_timer_ring(r, back, fore, &shader),
            Od::Wallclimbing => self.wallclimbing(r, back, fore, &shader),
            Od::Boosted => self.boosted(r, back, fore, &shader),
            Od::BoostBolt => self.booster_bolt(r, back, fore, &shader),
            Od::PoisonBarbs => self.poison_barbs(r, fore, &shader),
            Od::AlienSense => self.draw_alien_sense(r),
            Od::HumanScanner => self.draw_human_scanner(r, &shader, fore),
            Od::UsableBuildable => self.usable_buildable(r, &shader, fore),
            Od::Select => self.item_select(r),
            Od::WeaponIcon => self.weapon_icon(r, fore),
            Od::SelectText => self.item_select_text(r, scale, style),
            Od::Location => self.location(r, scale, align, fore),
            Od::Follow => self.follow(r, tx, ty, fore, scale, align, valign, style),
            Od::CrosshairNames => self.crosshair_names(r, scale, style),
            Od::Crosshair => self.crosshair(r, fore),
            Od::Fps => self.fps(r, tx, ty, scale, fore, align, valign, style),
            Od::Timer => self.timer(r, tx, ty, scale, fore, align, valign, style),
            Od::Clock => self.clock(r, tx, ty, scale, fore, align, valign, style),
            Od::TeamOverlay => self.team_overlay(r, scale, fore),
            Od::Speedometer => self.speed(r, scale, fore, back),
            Od::Console => self.console(r, tx, ty, fore, scale, align, valign, style),
            Od::Tutorial => self.tutorial(r, tx, ty, fore, scale, align, valign, style),
            Od::LoadLevelshot => self.level_shot(r),
            Od::LoadMedia | Od::LoadBuildables | Od::LoadCharModel | Od::LoadOverall => {
                let l = self.loading.cloned().unwrap_or_default();
                // `CG_DrawMediaProgress`, `CG_DrawBuildablesProgress`,
                // `CG_DrawCharModelProgress`, `CG_DrawOverallProgress`
                let progress = match od {
                    Od::LoadMedia => l.media,
                    Od::LoadBuildables => l.buildables,
                    Od::LoadCharModel => l.char_models,
                    _ => (l.char_models + l.buildables + l.media) / 3.0,
                };
                self.progress_bar(r, fore, scale, it.align, align, style, it.border_size, progress);
            }
            Od::LoadMediaLabel | Od::LoadBuildablesLabel | Od::LoadCharModelLabel => {
                let l = self.loading.cloned().unwrap_or_default();
                // `CG_DrawMediaProgressLabel`, `CG_DrawBuildablesProgressLabel`,
                // `CG_DrawCharModelProgressLabel`
                let (s, f) = match od {
                    Od::LoadMediaLabel => ("Map and Textures", l.media),
                    Od::LoadBuildablesLabel => ("Buildable Models", l.buildables),
                    _ => ("Character Models", l.char_models),
                };
                self.progress_label(r, tx, ty, fore, scale, align, valign, s, f);
            }
            Od::LoadLevelname => {
                // `CG_DrawLevelName`: CS_MESSAGE
                let s = self.loading.map(|l| l.message.clone()).unwrap_or_default();
                self.draw_text_block(r, tx, ty, fore, scale, align, valign, style, &s);
            }
            Od::LoadMotd => {
                // `CG_DrawMOTD`: CS_MOTD with Q_ParseNewlines
                let s = self.loading.map(|l| parse_newlines(&l.motd)).unwrap_or_default();
                self.draw_text_block(r, tx, ty, fore, scale, align, valign, style, &s);
            }
            Od::LoadHostname => {
                // `CG_DrawHostname`: sv_hostname, UI_EscapeEmoticons then Q_CleanStr
                let s = self.loading.map(|l| clean_str(&escape_emoticons(&self.data.emoticons, &l.hostname))).unwrap_or_default();
                self.draw_text_block(r, tx, ty, fore, scale, align, valign, style, &s);
            }
        }
    }

    // ------------------------------------------------------------ CG_Draw2D texts
    /// `CG_DrawVote` for the global vote (TEAM_NONE) or our team's. (The
    /// talk beep on a modified vote is not played here.)
    fn draw_vote(&mut self, team: Team) {
        let v = &self.w.level.votes[team as usize];
        if v.time == 0 || (team != Team::None && self.stat(Stat::Team) != team as i32) {
            return;
        }
        let sec = ((trem_sim::kernel::vote::VOTE_TIME - (self.time - v.time)) / 1000).max(0);
        let (mut yeskey, mut nokey) = (String::new(), String::new());
        let prefix = if team == Team::None { "" } else { "team" };
        if self.cvars.tutorial != 0 {
            yeskey = format!("[{}]", cg_key_binding(self.binds, &format!("{prefix}vote yes")));
            nokey = format!("[{}]", cg_key_binding(self.binds, &format!("{prefix}vote no")));
        }
        let offset = if team != Team::None { 80.0 } else { 0.0 };
        let (display, caller, yes, no) = (v.display.clone(), v.caller.clone(), v.yes, v.no);
        let s = format!("{}VOTE({sec}): {display}", if team == Team::None { "" } else { "TEAM" });
        self.text_paint(8.0, 300.0 + offset, 0.3, WHITE, &s, ITEM_TEXTSTYLE_NORMAL);
        let s = format!("  Called by: \"{caller}\"");
        self.text_paint(8.0, 320.0 + offset, 0.3, WHITE, &s, ITEM_TEXTSTYLE_NORMAL);
        let s = format!("  {yeskey}Yes:{yes} {nokey}No:{no}");
        self.text_paint(8.0, 340.0 + offset, 0.3, WHITE, &s, ITEM_TEXTSTYLE_NORMAL);
    }

    /// `CG_DrawWarmup`: "Warmup Time:" and the seconds left (cg.warmupTime
    /// is CS_WARMUP, `CG_ParseWarmup`), "FIGHT!" in the last one.
    fn draw_warmup(&mut self) {
        let warmup = self.w.level.cs_warmup;
        // !cg.warmupTime; CS_WARMUP "-1" (no warmup) never draws in the
        // original, whose server time is past a second by the first snapshot
        if warmup <= 0 {
            return;
        }
        // (integer division: "FIGHT!" stays up for the second after the start)
        let sec = (warmup - self.time) / 1000;
        if sec < 0 {
            return;
        }
        let size = 0.5;
        let text = "Warmup Time:";
        let w = self.text_width(text, size) as i32;
        let h = self.text_height(text, size) as i32;
        self.text_paint((320 - w / 2) as f32, 200.0, size, WHITE, text, ITEM_TEXTSTYLE_SHADOWED);
        let text = if sec != 0 { sec.to_string() } else { "FIGHT!".to_string() };
        let w = self.text_width(&text, size) as i32;
        self.text_paint((320 - w / 2) as f32, 200.0 + 1.5 * h as f32, size, WHITE, &text, ITEM_TEXTSTYLE_SHADOWED);
    }

    /// `CG_DrawQueue`: our place in the spawn queue and the spawns left.
    fn draw_queue(&mut self) -> bool {
        if self.ps.pm_flags & k::PMF_QUEUED == 0 {
            return false;
        }
        let position = self.ps.pers(Pers::Queuepos) + 1;
        if position < 1 {
            return false;
        }
        let ordinal = match position % 100 {
            11..=13 => "th",
            _ => match position % 10 {
                1 => "st",
                2 => "nd",
                3 => "rd",
                _ => "th",
            },
        };
        let buffer = format!("You are {position}{ordinal} in the spawn queue");
        let w = self.text_width(&buffer, 0.7);
        self.text_paint(320.0 - w / 2.0, 360.0, 0.7, WHITE, &buffer, ITEM_TEXTSTYLE_SHADOWED);
        let buffer = match self.ps.pers(Pers::Spawns) {
            0 => "There are no spawns remaining".to_string(),
            1 => "There is 1 spawn remaining".to_string(),
            n => format!("There are {n} spawns remaining"),
        };
        let w = self.text_width(&buffer, 0.7);
        self.text_paint(320.0 - w / 2.0, 400.0, 0.7, WHITE, &buffer, ITEM_TEXTSTYLE_SHADOWED);
        true
    }

    /// The "cp" server commands addressed to us since the last frame:
    /// `CG_CenterPrint( text, SCREEN_HEIGHT * 0.30, BIGCHAR_WIDTH )`.
    fn center_print_from_server(&mut self) {
        if self.time < self.st.center_print_time {
            self.st.center_print_time = 0; // a new level
        }
        let (me, team) = (self.me, self.w.clients[self.me].team);
        let after = self.st.center_print_time;
        let latest = self.w.center_prints.iter().rev().find(|(t, tm, cl, _)| *t > after && tm.is_none_or(|x| x == team) && cl.is_none_or(|x| x == me)).map(|c| (c.0, c.3.clone()));
        if let Some((t, text)) = latest {
            self.center_print(&text);
            self.st.center_print_time = t.max(1);
        }
    }

    /// `CG_CenterPrint`: Q_ParseNewlines, wrapped at 2/3 of the screen
    /// width at scale 0.5, lines counted for centring.
    fn center_print(&mut self, s: &str) {
        let max_width = (2.0f32 / 3.0 * 640.0) as i32 as f32;
        let parsed = parse_newlines(s);
        let wrapped = self.metrics().map(|m| String::from_utf8_lossy(&m.wrap(parsed.as_bytes(), 0.5, max_width)).into_owned()).unwrap_or(parsed);
        self.st.center_print_lines = 1 + wrapped.matches('\n').count() as i32;
        self.st.center_print = wrapped;
    }

    /// `CG_DrawCenterString`: the centre print, fading out at the end of
    /// cg_centertime (3 s), each line centred at scale 0.5.
    fn draw_center_string(&mut self) {
        if self.st.center_print_time == 0 {
            return;
        }
        let Some(alpha) = self.fade(self.st.center_print_time, (1000.0 * CG_CENTERTIME) as i32) else { return };
        let color = [1.0, 1.0, 1.0, alpha];
        let mut y = (480.0 * 0.30) as i32 - self.st.center_print_lines * BIGCHAR_HEIGHT / 2;
        let text = self.st.center_print.clone();
        for line in text.split('\n') {
            let w = self.text_width(line, 0.5) as i32;
            let h = self.text_height(line, 0.5) as i32;
            let x = (640 - w) / 2;
            self.text_paint(x as f32, (y + h) as f32, 0.5, color, line, ITEM_TEXTSTYLE_SHADOWEDMORE);
            y += h + 6;
        }
    }

    // ------------------------------------------------------------ loading screen
    /// `CG_DrawLevelShot`: levelshots/<mapname> (else gfx/2d/load_screen)
    /// with the gfx/misc/detail texture blended over it.
    fn level_shot(&mut self, r: [f32; 4]) {
        let map = self.loading.map(|l| l.mapname.clone()).unwrap_or_default();
        let shot = format!("levelshots/{map}.tga");
        let img = if self.st.levelshot { shot } else { "gfx/2d/load_screen".into() };
        // blend a detail texture over it (gfx/misc/detail: blendFunc GL_DST_COLOR GL_SRC_COLOR)
        self.out.push(Cmd::PicDetail { img, detail: "gfx/misc/detail".into(), r });
    }

    /// `CG_DrawProgressBar`.
    #[allow(clippy::too_many_arguments)]
    fn progress_bar(&mut self, r: [f32; 4], color: Rgba, scale: f32, align: i32, text_align: i32, text_style: i32, border_size: f32, progress: f32) {
        let rim = if border_size >= 0.0 { border_size } else { (r[3] / 20.0).max(0.6) };
        let progress = progress.clamp(0.0, 1.0);
        let done = (r[2] - 2.0 * rim) * progress;
        let left = (r[2] - 2.0 * rim) - done;
        // draw rim and bar
        if align == ITEM_ALIGN_RIGHT {
            self.fill(r[0], r[1], rim, r[3], color);
            self.fill(r[0] + rim, r[1], left, rim, color);
            self.fill(r[0] + rim, r[1] + r[3] - rim, left, rim, color);
            self.fill(r[0] + rim + left, r[1], rim + done, r[3], color);
        } else {
            self.fill(r[0], r[1], rim + done, r[3], color);
            self.fill(rim + r[0] + done, r[1], left, rim, color);
            self.fill(rim + r[0] + done, r[1] + r[3] - rim, left, rim, color);
            self.fill(r[0] + r[2] - rim, r[1], rim, r[3], color);
        }
        // draw text
        if scale > 0.0 {
            let text = format!("{}%", (progress * 100.0) as i32);
            let (tx, ty) = self.align_text(r, &text, scale, 0.0, 0.0, text_align, VALIGN_CENTER);
            self.text_paint(tx, ty, scale, color, &text, text_style);
        }
    }

    /// `CG_DrawProgressLabel`: white while loading, neon in the item's colour when done.
    #[allow(clippy::too_many_arguments)]
    fn progress_label(&mut self, r: [f32; 4], text_x: f32, text_y: f32, color: Rgba, scale: f32, align: i32, valign: i32, s: &str, fraction: f32) {
        let (tx, ty) = self.align_text(r, s, scale, 0.0, 0.0, align, valign);
        if fraction < 1.0 {
            self.text_paint(text_x + tx, text_y + ty, scale, WHITE, s, ITEM_TEXTSTYLE_NORMAL);
        } else {
            self.text_paint(text_x + tx, text_y + ty, scale, color, s, ITEM_TEXTSTYLE_NEON);
        }
    }

    // ------------------------------------------------------------ cg_draw.c ownerdraws
    /// `CG_DrawPlayerCreditsValue`.
    fn credits_value(&mut self, r: [f32; 4], c: Rgba, padding: bool) {
        let ent_weapon = self.w.ents[self.view].weapon;
        if (ent_weapon == WeaponId::Abuild as i32 || ent_weapon == WeaponId::Abuildupg as i32) && self.stat(Stat::Misc) != 0 {
            return;
        }
        let mut value = self.ps.pers(Pers::Credit);
        if value > -1 {
            let mut c = c;
            if self.team() == Team::Aliens as i32 {
                let dt = self.time - self.st.last_evolve_attempt;
                if !self.can_evolve(value) && dt <= NO_CREDITS_TIME && (dt / 300) & 1 != 0 {
                    c[3] = 0.0;
                }
                value /= k::ALIEN_CREDITS_PER_KILL;
            }
            if padding {
                self.field_padded(r[0] as i32, r[1] as i32, 4, (r[2] / 4.0) as i32, r[3] as i32, value, c);
            } else {
                self.field(r[0], r[1], 1, r[2], r[3], value, c);
            }
        }
    }

    /// `CG_DrawPlayerCreditsFraction`: the item's shader filled from the
    /// bottom by the fraction of an evo the alien has.
    fn credits_fraction(&mut self, r: [f32; 4], c: Rgba, shader: &str) {
        if self.team() != Team::Aliens as i32 {
            return;
        }
        let fraction = (self.ps.pers(Pers::Credit) % k::ALIEN_CREDITS_PER_KILL) as f32 / k::ALIEN_CREDITS_PER_KILL as f32;
        let height = r[3] * fraction;
        self.pic_uv(shader, r[0], r[1] - height + r[3], r[2], height, c, [0.0, 1.0 - fraction, 1.0, 1.0]);
    }

    /// `CG_DrawPlayerStamina`.
    fn stamina(&mut self, n: u8, r: [f32; 4], back: Rgba, fore: Rgba, shader: &str) {
        let stamina = self.stat(Stat::Stamina) as f32;
        let by3 = k::STAMINA_MAX as f32 / 3.0;
        let progress = match n {
            1 => (stamina - 2.0 * (by3 as i32) as f32) / by3,
            2 => (stamina - (by3 as i32) as f32) / by3,
            3 => stamina / by3,
            _ => (stamina + k::STAMINA_MAX as f32) / k::STAMINA_MAX as f32,
        }
        .clamp(0.0, 1.0);
        let c = lerp4(progress, &back, &fore);
        self.pic(shader, r[0], r[1], r[2], r[3], c);
    }

    /// `CG_DrawPlayerStaminaBolt`.
    fn stamina_bolt(&mut self, r: [f32; 4], back: Rgba, fore: Rgba, shader: &str) {
        let stamina = self.stat(Stat::Stamina);
        let c = if self.stat(Stat::State) & k::SS_SPEEDBOOST != 0 {
            let period = if stamina >= 0 { 150.0 } else { 2000.0 };
            lerp4(((self.time as f32 / period).sin() + 1.0) / 2.0, &back, &fore)
        } else if stamina < 0 {
            back
        } else {
            fore
        };
        self.pic(shader, r[0], r[1], r[2], r[3], c);
    }

    /// `CG_DrawPlayerClipsRing`.
    fn clips_ring(&mut self, r: [f32; 4], back: Rgba, fore: Rgba, shader: &str) {
        let wp = player_weapon(self.ps);
        let c = if is_builder_weapon(wp) {
            let max = self.max_build_time() as f32;
            let bt = (self.stat(Stat::Misc) as f32).min(max);
            lerp4((max - bt) / max, &back, &fore)
        } else if self.ps.weaponstate == trem_sim::gen::enums::WeaponState::Reloading as i32 {
            let max_delay = weapon_id(self.w.ents[self.view].weapon).map(|w| w.def().reload_time).unwrap_or(0) as f32;
            if max_delay > 0.0 {
                lerp4((max_delay - self.ps.weapon_time as f32) / max_delay, &back, &fore)
            } else {
                fore
            }
        } else {
            fore
        };
        self.pic(shader, r[0], r[1], r[2], r[3], c);
    }

    /// `CG_DrawPlayerBuildTimerRing`.
    fn build_timer_ring(&mut self, r: [f32; 4], back: Rgba, fore: Rgba, shader: &str) {
        let max = self.max_build_time() as f32;
        let bt = (self.stat(Stat::Misc) as f32).min(max);
        let p = (max - bt) / max;
        let c = lerp4(p, &back, &fore);
        self.pic(shader, r[0], r[1], r[2], r[3], c);
    }

    /// `CG_DrawPlayerBoosted`.
    fn boosted(&mut self, r: [f32; 4], back: Rgba, fore: Rgba, shader: &str) {
        let c = if self.stat(Stat::State) & k::SS_BOOSTED != 0 { fore } else { back };
        self.pic(shader, r[0], r[1], r[2], r[3], c);
    }

    /// `CG_DrawPlayerBoosterBolt`.
    fn booster_bolt(&mut self, r: [f32; 4], back: Rgba, fore: Rgba, shader: &str) {
        let s = self.stat(Stat::State);
        let c = if s & k::SS_BOOSTED != 0 && s & k::SS_BOOSTEDWARNING != 0 { lerp4(((self.time as f32 / 100.0).sin() + 1.0) / 2.0, &back, &fore) } else { fore };
        self.pic(shader, r[0], r[1], r[2], r[3], c);
    }

    /// MAXIMUM_BUILD_TIME; in a Tremulous 1.1.0 game the build delay of the
    /// builder's weapon (1.1.0 cg_draw.c's ABUILDER_*_DELAY / HBUILD*_DELAY
    /// are its bg_weapons buildDelay values, dec041).
    fn max_build_time(&self) -> i32 {
        if self.w.vanilla {
            if let Some(w11) = weapon_id(self.stat(Stat::Weapon)).and_then(trem_sim::kernel::v11::weapon11_of) {
                if w11.def().build_delay > 0 {
                    return w11.def().build_delay;
                }
            }
        }
        k::MAXIMUM_BUILD_TIME
    }

    /// (max ammo, infinite ammo) of a weapon: the legacy table, or 1.1's in a
    /// Tremulous 1.1.0 game (dec041).
    fn weapon_ammo_def(&self, wp: i32) -> Option<(i32, bool)> {
        let w = weapon_id(wp)?;
        if self.w.vanilla {
            if let Some(w11) = trem_sim::kernel::v11::weapon11_of(w) {
                return Some((w11.def().max_ammo, w11.def().infinite_ammo));
            }
        }
        Some((w.def().max_ammo, w.def().infinite_ammo))
    }

    /// `CG_DrawPlayerPoisonBarbs` (the icon is square in screen space).
    fn poison_barbs(&mut self, r: [f32; 4], c: Rgba, shader: &str) {
        let Some((max_ammo, _)) = self.weapon_ammo_def(self.ps.weapon) else { return };
        let (max_barbs, mut num) = (max_ammo, self.ps.ammo);
        if max_barbs <= 0 || num <= 0 {
            return;
        }
        let (mut x, mut y, width, height) = (r[0] * self.xs, r[1] * self.ys, r[2] * self.xs, r[3] * self.ys);
        let vertical = height > width;
        let iconsize = (if vertical { width } else { height }) as i32 as f32;
        let diff = if max_barbs != 1 { ((if vertical { height } else { width }) - iconsize) / (max_barbs - 1) as f32 } else { 0.0 };
        while num > 0 {
            self.pic(shader, x / self.xs, y / self.ys, iconsize / self.xs, iconsize / self.ys, c);
            if vertical {
                y += diff;
            } else {
                x += diff;
            }
            num -= 1;
        }
    }

    /// `CG_DrawPlayerWallclimbing`.
    fn wallclimbing(&mut self, r: [f32; 4], back: Rgba, fore: Rgba, shader: &str) {
        let c = if self.stat(Stat::State) & k::SS_WALLCLIMBING != 0 { fore } else { back };
        self.pic(shader, r[0], r[1], r[2], r[3], c);
    }

    /// `CG_DrawPlayerAmmoValue`.
    fn ammo_value(&mut self, r: [f32; 4], c: Rgba) {
        let wp = self.stat(Stat::Weapon);
        if wp == 0 || wp == WeaponId::Blaster as i32 {
            return;
        }
        let (mut value, mut marked, bp) = if is_builder_weapon(wp) { (self.ps.pers(Pers::Bp), self.ps.pers(Pers::Markedbp), true) } else { (self.ps.ammo, -1, false) };
        value = value.min(999);
        marked = marked.min(999);
        if value <= -1 {
            return;
        }
        if !bp {
            self.field(r[0] - 5.0, r[1], 4, r[2] / 4.0, r[3], value, c);
            return;
        }
        let text = if marked > 0 { format!("{value}+({marked})") } else { value.to_string() };
        let scale = match text.len() {
            0..=4 => 0.50,
            5 | 6 => 0.43,
            7 => 0.36,
            8 => 0.33,
            _ => 0.31,
        };
        let (tx, ty) = self.align_text(r, &text, scale, 0.0, 0.0, ITEM_ALIGN_RIGHT, VALIGN_CENTER);
        self.text_paint(tx + 1.0, ty, scale, c, &text, ITEM_TEXTSTYLE_NORMAL);
    }

    /// `CG_DrawAlienSense`.
    fn draw_alien_sense(&mut self, r: [f32; 4]) {
        if bg::class_has_ability(self.stat(Stat::Class), ability::ALIENSENSE) {
            self.alien_sense(r);
        }
    }

    /// `CG_DrawHumanScanner`.
    fn draw_human_scanner(&mut self, r: [f32; 4], shader: &str, c: Rgba) {
        if self.st.force_scanner || bg::inventory_contains_upgrade(UpgradeId::Helmet as i32, &self.ps.stats) {
            self.scanner(r, shader, c);
        }
    }

    /// The entity a MASK_SHOT trace 64 units along the view hits (used by
    /// CG_DrawUsableBuildable and CG_BuildableInRange).
    fn entity_in_front(&self) -> Option<usize> {
        let rd = self.refdef;
        let end = ma(&rd.vieworg, 64.0, &rd.axis[0]);
        let tr = self.w.trace(&rd.vieworg, None, None, &end, self.view as i32, k::MASK_SHOT);
        (tr.entity_num >= 0 && (tr.entity_num as usize) < self.w.ents.len()).then_some(tr.entity_num as usize)
    }

    /// `CG_DrawUsableBuildable` (sets cg.nearUsableBuildable).
    fn usable_buildable(&mut self, r: [f32; 4], shader: &str, c: Rgba) {
        let hit = self.entity_in_front().map(|i| &self.w.ents[i]).filter(|e| e.etype == EType::Buildable).and_then(|e| e.buildable);
        self.st.near_usable_buildable = None;
        let Some(b) = hit else { return };
        let bd = b.def();
        if !bd.usable || self.team() != bd.team as i32 {
            return;
        }
        if b == BuildableId::Reactor || b == BuildableId::Repeater {
            let wd = weapon_id(self.ps.weapon).map(|w| w.def());
            if !wd.is_some_and(|d| d.uses_energy && !d.infinite_ammo) {
                return;
            }
        }
        self.pic(shader, r[0], r[1], r[2], r[3], c);
        self.st.near_usable_buildable = Some(b);
    }

    /// `CG_DrawPlayerBuildTimer`.
    fn build_timer(&mut self, r: [f32; 4], c: Rgba) {
        let misc = self.stat(Stat::Misc);
        if misc <= 0 || !is_builder_weapon(self.stat(Stat::Weapon)) {
            return;
        }
        let index = (8 * (misc - 1) / self.max_build_time()).clamp(0, 7) as usize;
        let mut c = c;
        let dt = self.time - self.st.last_build_attempt;
        if dt <= BUILD_DELAY_TIME && (dt / 300) % 2 != 0 {
            c = [1.0, 0.0, 0.0, 1.0];
        }
        self.pic(PIES[index], r[0], r[1], r[2], r[3], c);
    }

    /// `CG_DrawPlayerClipsValue`.
    fn clips_value(&mut self, r: [f32; 4], c: Rgba) {
        let wp = self.stat(Stat::Weapon);
        if wp == 0 || wp == WeaponId::Blaster as i32 || is_builder_weapon(wp) {
            return;
        }
        let value = self.ps.clips;
        if value > -1 {
            self.field(r[0], r[1], 4, r[2] / 4.0, r[3], value, c);
        }
    }

    /// `CG_DrawPlayerHealthValue`.
    fn health_value(&mut self, r: [f32; 4], c: Rgba) {
        let h = self.stat(Stat::Health);
        self.field(r[0], r[1], 4, r[2] / 4.0, r[3], h, c);
    }

    /// `CG_DrawPlayerHealthCross`.
    fn health_cross(&mut self, r: [f32; 4], ref_color: Rgba) {
        let s = self.stat(Stat::State);
        let cr = &self.data.cross;
        let shader = if s & k::SS_HEALING_3X != 0 {
            cr[2].clone()
        } else if s & k::SS_HEALING_2X != 0 {
            if self.team() == Team::Aliens as i32 {
                cr[1].clone()
            } else {
                cr[3].clone()
            }
        } else if s & k::SS_POISONED != 0 {
            cr[4].clone()
        } else {
            cr[0].clone()
        };
        let mut color = ref_color;
        if self.team() == Team::Humans as i32 && self.stat(Stat::Health) < 10 {
            color = [1.0, 0.0, 0.0, color[3]];
        }
        let ref_alpha = if s & k::SS_HEALING_ACTIVE != 0 { 1.0 } else { ref_color[3] };
        let last = self.st.last_cross.get_or_insert_with(|| shader.clone()).clone();
        if last != shader {
            self.st.cross_fade += self.frametime as f32 / 500.0;
            if self.st.cross_fade > 1.0 {
                self.st.cross_fade = 0.0;
                self.st.last_cross = Some(shader.clone());
            } else {
                let f = self.st.cross_fade;
                self.pic(&shader, r[0], r[1], r[2], r[3], [color[0], color[1], color[2], ref_alpha * f]);
                self.pic(&last, r[0], r[1], r[2], r[3], [color[0], color[1], color[2], ref_alpha * (1.0 - f)]);
                return;
            }
        }
        color[3] = ref_alpha;
        self.pic(&shader, r[0], r[1], r[2], r[3], color);
    }

    /// `CG_DrawPlayerChargeBarBG`.
    fn charge_bar_bg(&mut self, r: [f32; 4], ref_color: Rgba, shader: &str) {
        if self.cvars.draw_charge_bar == 0 || self.st.charge_meter_alpha <= 0.0 {
            return;
        }
        let c = [ref_color[0], ref_color[1], ref_color[2], ref_color[3] * self.st.charge_meter_alpha];
        self.pic(shader, r[0], r[1], r[2], r[3], c);
    }

    /// `CG_DrawPlayerChargeBar`: pumps cg.chargeMeterAlpha and draws the
    /// meter with its end caps from the item's shader.
    fn charge_bar(&mut self, r: [f32; 4], ref_color: Rgba, shader: &str) {
        if self.cvars.draw_charge_bar == 0 {
            return;
        }
        let progress = charge_progress(self.ps.weapon, self.stat(Stat::State), self.stat(Stat::Misc));
        let ft = self.frametime as f32;
        if progress <= 0.0 {
            self.st.charge_meter_alpha -= CHARGE_BAR_FADE_RATE * ft;
            if self.st.charge_meter_alpha <= 0.0 {
                self.st.charge_meter_alpha = 0.0;
                return;
            }
        } else {
            self.st.charge_meter_value = progress;
            self.st.charge_meter_alpha = (self.st.charge_meter_alpha + CHARGE_BAR_FADE_RATE * ft).min(1.0);
        }
        let mut c = [ref_color[0], ref_color[1], ref_color[2], ref_color[3] * self.st.charge_meter_alpha];
        // flash red for the Lucifer Cannon warning
        if self.ps.weapon == WeaponId::Lcannon as i32 && self.stat(Stat::Misc) >= k::LCANNON_CHARGE_TIME_WARN && self.time & 128 != 0 {
            c = [1.0, 0.0, 0.0, c[3]];
        }
        let (x, y) = (r[0], r[1]);
        let cap = CHARGE_BAR_CAP_SIZE;
        let value = self.st.charge_meter_value;
        if r[2] >= r[3] {
            // horizontal
            let width = (r[2] - CHARGE_BAR_CAP_SIZE * 2.0) * value;
            let height = r[3];
            self.pic_uv(shader, x, y, cap, height, c, [0.0, 0.0, 1.0, 1.0]);
            self.pic_uv(shader, x + width + cap, y, cap, height, c, [1.0, 0.0, 0.0, 1.0]);
            self.pic_uv(shader, x + cap, y, width, height, c, [1.0, 0.0, 1.0, 1.0]);
        } else {
            // vertical
            let y = y + r[3];
            let width = r[2];
            let height = (r[3] - CHARGE_BAR_CAP_SIZE * 2.0) * value;
            self.pic_uv(shader, x, y - cap, width, cap, c, [0.0, 1.0, 1.0, 0.0]);
            self.pic_uv(shader, x, y - height - cap * 2.0, width, cap, c, [0.0, 0.0, 1.0, 1.0]);
            self.pic_uv(shader, x, y - height - cap, width, height, c, [0.0, 1.0, 1.0, 1.0]);
        }
    }

    /// `CG_DrawWeaponIcon`.
    fn weapon_icon(&mut self, r: [f32; 4], mut c: Rgba) {
        if self.stat(Stat::Health) <= 0 {
            return;
        }
        let wp = player_weapon(self.ps);
        let Some((max_ammo, infinite_ammo)) = self.weapon_ammo_def(wp) else { return };
        if self.ps.clips == 0 && !infinite_ammo && max_ammo > 0 && (self.ps.ammo as f32 / max_ammo as f32) < 0.33 {
            c[0] = 1.0;
            c[1] = 0.0;
            c[2] = 0.0;
        }
        if self.team() == Team::Aliens as i32 && !self.can_evolve(self.ps.pers(Pers::Credit)) {
            let dt = self.time - self.st.last_evolve_attempt;
            if dt <= NO_CREDITS_TIME && (dt / 300) % 2 != 0 {
                c[3] = 0.0;
            }
        }
        if let Some(icon) = self.data.weapons.get(wp as usize).and_then(|w| w.icon.clone()) {
            self.pic(&icon, r[0], r[1], r[2], r[3], c);
        }
    }

    /// `BG_AlienCanEvolve` for the viewed player.
    fn can_evolve(&self, credits: i32) -> bool {
        let class = bg::class_id(self.stat(Stat::Class));
        bg::alien_can_evolve(class, credits, self.w.level.alien_stage)
    }

    fn upgrade_icon(&self, u: UpgradeId) -> Option<String> {
        if u == UpgradeId::Gren {
            self.data.weapons.get(WeaponId::Grenade as usize).and_then(|w| w.icon.clone())
        } else {
            u.cold().icon.map(|s| s.to_string())
        }
    }

    /// cg_weapons.c `CG_DrawItemSelect` (the selection check runs in the
    /// client every frame: `WeaponSelect::validate`).
    fn item_select(&mut self, r: [f32; 4]) {
        if self.stat(Stat::Health) <= 0 {
            return;
        }
        let stats = self.ps.stats;
        let selected_id = self.st.weapon_select;
        let mut items: Vec<(Option<String>, u8)> = Vec::new();
        let mut selected = 0usize;
        for w in WeaponId::ALL {
            let n = w as i32;
            if !bg::inventory_contains_weapon(n, &stats) {
                continue;
            }
            let col = if self.ps.ammo == 0 && self.ps.clips == 0 && !w.def().infinite_ammo { 1 } else { 0 };
            if n == selected_id {
                selected = items.len();
            }
            items.push((self.data.weapons.get(n as usize).and_then(|x| x.icon.clone()), col));
        }
        for u in UpgradeId::ALL {
            if !bg::inventory_contains_upgrade(u as i32, &stats) {
                continue;
            }
            let col = if u.def().usable { 0 } else { 2 };
            if u as i32 == selected_id - 32 {
                selected = items.len();
            }
            items.push((self.upgrade_icon(u), col));
        }
        let (mut x, mut y) = (r[0], r[1]);
        let vertical = r[3] > r[2];
        let (iw, ih, length) = if vertical { (r[2] * self.aspect, r[2], (r[3] / (r[2] * self.aspect)) as i32) } else { (r[3] * self.aspect, r[3], (r[2] / (r[3] * self.aspect)) as i32) };
        for i in 0..length {
            let mut item = i - length / 2 + selected as i32;
            if item < 0 {
                item += length;
            } else if item >= length {
                item -= length;
            }
            if item >= 0 && (item as usize) < items.len() {
                let (icon, col) = &items[item as usize];
                // colorCyan / colorRed / colorMdGrey, alpha 0.5
                let c = match col {
                    0 => [0.0, 1.0, 1.0, 0.5],
                    1 => [1.0, 0.0, 0.0, 0.5],
                    _ => [0.5, 0.5, 0.5, 0.5],
                };
                if let Some(icon) = icon {
                    self.pic(icon, x, y, iw, ih, c);
                }
            }
            if vertical {
                y += ih;
            } else {
                x += iw;
            }
        }
    }

    /// `CG_FadeColor` alpha.
    fn fade(&self, start: i32, total: i32) -> Option<f32> {
        if start == 0 {
            return None;
        }
        let t = self.time - start;
        if t >= total {
            return None;
        }
        Some(if total - t < FADE_TIME { (total - t) as f32 / FADE_TIME as f32 } else { 1.0 })
    }

    /// The display name of cg.weaponSelect (weapon < 32, else upgrade).
    fn weapon_select_name(&self) -> Option<&'static str> {
        let sel = self.st.weapon_select;
        if sel < 32 {
            weapon_id(sel).map(|w| w.cold().display_name)
        } else {
            UpgradeId::from_num((sel - 32).clamp(0, 255) as u8).map(|u| u.cold().display_name)
        }
    }

    /// cg_weapons.c `CG_DrawItemSelectText`.
    fn item_select_text(&mut self, r: [f32; 4], scale: f32, style: i32) {
        let Some(a) = self.fade(self.st.weapon_select_time, WEAPON_SELECT_TIME) else { return };
        let sel = self.st.weapon_select;
        let held = if sel < 32 { bg::inventory_contains_weapon(sel, &self.ps.stats) } else { bg::inventory_contains_upgrade(sel - 32, &self.ps.stats) };
        if let Some(name) = self.weapon_select_name().filter(|_| held) {
            let w = self.text_width(name, scale) as i32;
            let x = (r[0] + r[2] / 2.0) as i32;
            self.text_paint((x - w / 2) as f32, r[1] + r[3], scale, [1.0, 1.0, 1.0, a], name, style);
        }
    }

    /// `CG_GetPlayerLocation`: the nearest location entity in the PVS of
    /// the predicted origin; its CS_LOCATIONS string (CS_LOCATIONS + 0 if none).
    fn player_location(&self) -> String {
        let w = self.w;
        let o = self.ps.origin;
        let mut best: Option<usize> = None;
        let mut bestlen = 3.0f32 * 8192.0 * 8192.0;
        let mut e = w.level.location_head;
        while let Some(l) = e {
            let d = sub(&o, &w.ents[l].origin);
            let len = dot(&d, &d);
            if len <= bestlen && w.in_pvs(&o, &w.ents[l].origin) {
                bestlen = len;
                best = Some(l);
            }
            e = w.ents[l].next_train;
        }
        let idx = best.map(|l| w.ents[l].generic1.max(0) as usize).unwrap_or(0);
        w.location_names.get(idx).cloned().unwrap_or_default()
    }

    /// `CG_DrawLocation`.
    fn location(&mut self, r: [f32; 4], scale: f32, align: i32, c: Rgba) {
        if self.intermission_started() {
            return;
        }
        let max_x = r[0] + r[2];
        let location = self.player_location();
        if self.text_width(&location, scale) < r[2] {
            let (tx, ty) = self.align_text(r, &location, scale, 0.0, 0.0, align, VALIGN_CENTER);
            self.text_paint(tx, ty, scale, c, &location, ITEM_TEXTSTYLE_NORMAL);
        } else {
            let (tx, ty) = self.align_text(r, &location, scale, 0.0, 0.0, ALIGN_NONE, VALIGN_CENTER);
            let mut mx = max_x;
            self.text_paint_limit(&mut mx, tx, ty, scale, c, &location);
        }
    }

    /// `CG_DrawFollow`: "following <name>", or "chasing <name>" in the
    /// chase cam (cg.chaseFollow).
    #[allow(clippy::too_many_arguments)]
    fn follow(&mut self, r: [f32; 4], text_x: f32, text_y: f32, c: Rgba, scale: f32, align: i32, valign: i32, style: i32) {
        if self.ps.pm_flags & k::PMF_FOLLOW == 0 {
            return;
        }
        let name = self.w.clients.get(self.ps.client_num.max(0) as usize).map(|c| c.name.clone()).unwrap_or_default();
        let buffer = format!("{}{name}", if self.chase_follow { CHASING_STRING } else { FOLLOWING_STRING });
        let (tx, ty) = self.align_text(r, &buffer, scale, 0.0, 0.0, align, valign);
        self.text_paint(text_x + tx, text_y + ty, scale, c, &buffer, style);
    }

    /// `CG_DrawFPS` (scalable text), averaged over FPS_FRAMES frames of
    /// trap_Milliseconds.
    #[allow(clippy::too_many_arguments)]
    fn fps(&mut self, r: [f32; 4], text_x: f32, text_y: f32, scale: f32, c: Rgba, align: i32, valign: i32, style: i32) {
        if self.cvars.draw_fps == 0 {
            return;
        }
        let t = self.real.ms;
        let frame_time = t.saturating_sub(self.st.fps_previous) as i32;
        self.st.fps_previous = t;
        self.st.fps_times[self.st.fps_index % FPS_FRAMES] = frame_time;
        self.st.fps_index += 1;
        if self.st.fps_index > FPS_FRAMES {
            let total = self.st.fps_times.iter().sum::<i32>().max(1);
            let fps = 1000 * FPS_FRAMES as i32 / total;
            let s = fps.to_string();
            let w = self.text_width("0", scale);
            let h = self.text_height("0", scale);
            let n = draw_strlen(&s);
            let total_width = self.text_width("fps", scale) + w * n as f32;
            let (tx, ty) = self.align_text(r, &s, 0.0, total_width, h, align, valign);
            for (i, ch) in s.chars().enumerate() {
                self.text_paint(text_x + tx + i as f32 * w, text_y + ty, scale, c, &ch.to_string(), style);
            }
            self.text_paint(text_x + tx + n as f32 * w, text_y + ty, scale, c, "fps", style);
        }
    }

    /// `CG_DrawTimer`.
    #[allow(clippy::too_many_arguments)]
    fn timer(&mut self, r: [f32; 4], text_x: f32, text_y: f32, scale: f32, c: Rgba, align: i32, valign: i32, style: i32) {
        if self.cvars.draw_timer == 0 {
            return;
        }
        let msec = self.time - self.w.level.start_time;
        let mut seconds = msec / 1000;
        let mins = seconds / 60;
        seconds -= mins * 60;
        let tens = seconds / 10;
        seconds -= tens * 10;
        let s = format!("{mins}:{tens}{seconds}");
        self.monospaced(r, text_x, text_y, scale, c, align, valign, style, &s);
    }

    /// CG_DrawTimer / CG_DrawClock's layout: the digits on a grid of "0"
    /// widths, aligned as a block.
    #[allow(clippy::too_many_arguments)]
    fn monospaced(&mut self, r: [f32; 4], text_x: f32, text_y: f32, scale: f32, c: Rgba, align: i32, valign: i32, style: i32, s: &str) {
        let w = self.text_width("0", scale);
        let h = self.text_height("0", scale);
        let total_width = w * draw_strlen(s) as f32;
        let (tx, ty) = self.align_text(r, s, 0.0, total_width, h, align, valign);
        for (i, ch) in s.chars().enumerate() {
            self.text_paint(text_x + tx + i as f32 * w, text_y + ty, scale, c, &ch.to_string(), style);
        }
    }

    /// `CG_DrawClock` (cg_drawClock 1: 12-hour, 2: 24-hour; default 0).
    #[allow(clippy::too_many_arguments)]
    fn clock(&mut self, r: [f32; 4], text_x: f32, text_y: f32, scale: f32, c: Rgba, align: i32, valign: i32, style: i32) {
        if self.cvars.draw_clock == 0 {
            return;
        }
        let rt = self.real;
        let sep = if rt.sec % 2 != 0 { ":" } else { " " };
        let s = if self.cvars.draw_clock == 2 {
            format!("{:02}{sep}{:02}", rt.hour, rt.min)
        } else {
            let (mut h, mut pm) = (rt.hour, "am");
            if h == 0 {
                h = 12;
            } else if h == 12 {
                pm = "pm";
            } else if h > 12 {
                h -= 12;
                pm = "pm";
            }
            format!("{h}{sep}{:02}{pm}", rt.min)
        };
        self.monospaced(r, text_x, text_y, scale, c, align, valign, style, &s);
    }

    /// `CG_AddSpeed`: append the snapshot speed to the sample history.
    fn add_speed(&mut self) {
        let mut vel = self.ps.velocity;
        if self.cvars.draw_speed & SPEEDOMETER_IGNORE_Z != 0 {
            vel[2] = 0.0;
        }
        let speed = length(&vel);
        let st = &mut *self.st;
        if speed > st.speed_samples[st.max_speed_sample] {
            st.max_speed_sample = st.oldest_speed_sample;
            st.speed_samples[st.oldest_speed_sample] = speed;
            st.oldest_speed_sample = (st.oldest_speed_sample + 1) % SPEEDOMETER_NUM_SAMPLES;
            return;
        }
        st.speed_samples[st.oldest_speed_sample] = speed;
        let was_max = st.max_speed_sample == st.oldest_speed_sample;
        st.oldest_speed_sample += 1;
        if was_max {
            // the old max was overwritten: find a new one
            st.max_speed_sample = 0;
            for i in 1..SPEEDOMETER_NUM_SAMPLES {
                if st.speed_samples[i] > st.speed_samples[st.max_speed_sample] {
                    st.max_speed_sample = i;
                }
            }
        }
        st.oldest_speed_sample %= SPEEDOMETER_NUM_SAMPLES;
    }

    /// `CG_DrawSpeedGraph`.
    fn speed_graph(&mut self, r: [f32; 4], fore: Rgba, back: Rgba) {
        let slow = [0.0, 0.0, 1.0];
        let medium = [0.0, 1.0, 0.0];
        let fast = [1.0, 0.0, 0.0];
        let max = self.st.speed_samples[self.st.max_speed_sample].max(SPEEDOMETER_MIN_RANGE);
        self.fill(r[0], r[1], r[2], r[3], back);
        let mut color = fore;
        let lerp3 = |t: f32, a: [f32; 3], b: [f32; 3]| [a[0] + t * (b[0] - a[0]), a[1] + t * (b[1] - a[1]), a[2] + t * (b[2] - a[2])];
        for i in 1..SPEEDOMETER_NUM_SAMPLES {
            let val = self.st.speed_samples[(self.st.oldest_speed_sample + i) % SPEEDOMETER_NUM_SAMPLES];
            let rgb = if val < SPEED_MED {
                lerp3(val / SPEED_MED, slow, medium)
            } else if val < SPEED_FAST {
                lerp3((val - SPEED_MED) / (SPEED_FAST - SPEED_MED), medium, fast)
            } else {
                fast
            };
            color[..3].copy_from_slice(&rgb);
            let top = r[1] + (1.0 - val / max) * r[3];
            let n = SPEEDOMETER_NUM_SAMPLES as f32;
            self.fill(r[0] + (i as f32 / n) * r[2], top, r[2] / n, val * r[3] / max, color);
        }
    }

    /// `CG_DrawSpeedText`.
    fn speed_text(&mut self, r: [f32; 4], scale: f32, fore: Rgba) {
        let color = [fore[0], fore[1], fore[2], 1.0];
        let val = if self.ps.client_num as usize == self.me {
            let mut vel = self.ps.velocity;
            if self.cvars.draw_speed & SPEEDOMETER_IGNORE_Z != 0 {
                vel[2] = 0.0;
            }
            length(&vel)
        } else if self.st.oldest_speed_sample == 0 {
            self.st.speed_samples[SPEEDOMETER_NUM_SAMPLES - 1]
        } else {
            self.st.speed_samples[self.st.oldest_speed_sample - 1]
        };
        let s = (val as i32).to_string();
        let x = r[0] + (r[2] - self.text_width(&s, scale)) / 2.0;
        let y = r[1] + (r[3] + self.text_height(&s, scale)) / 2.0;
        self.text_paint(x, y, scale, color, &s, ITEM_TEXTSTYLE_NORMAL);
    }

    /// `CG_DrawSpeed` (cg_drawSpeed defaults to 0: nothing).
    fn speed(&mut self, r: [f32; 4], scale: f32, fore: Rgba, back: Rgba) {
        if self.cvars.draw_speed & SPEEDOMETER_DRAW_GRAPH != 0 {
            self.speed_graph(r, fore, back);
        }
        if self.cvars.draw_speed & SPEEDOMETER_DRAW_TEXT != 0 {
            self.speed_text(r, scale, fore);
        }
    }

    /// `CG_DrawConsole`: the notify lines (cg.consoleText).
    #[allow(clippy::too_many_arguments)]
    fn console(&mut self, r: [f32; 4], text_x: f32, text_y: f32, c: Rgba, scale: f32, align: i32, valign: i32, style: i32) {
        let text: String = self.st.console.iter().map(|l| l.1.as_str()).collect();
        self.draw_text_block(r, text_x, text_y, c, scale, align, valign, style, &text);
    }

    /// `CG_DrawTutorial`.
    #[allow(clippy::too_many_arguments)]
    fn tutorial(&mut self, r: [f32; 4], text_x: f32, text_y: f32, c: Rgba, scale: f32, align: i32, valign: i32, style: i32) {
        if self.cvars.tutorial == 0 {
            return;
        }
        let text = self.tutorial_text();
        self.draw_text_block(r, text_x, text_y, c, scale, align, valign, style, &text);
    }

    /// `CG_GetColorCharForHealth`.
    fn color_char_for_health(&self, client: usize) -> char {
        let c = &self.w.clients[client];
        let mut max_health = if c.team == Team::Aliens { ClassId::from_num(c.ps.stat(Stat::Class).clamp(0, 255) as u8).map(|k| k.def().health).unwrap_or(1) } else { ClassId::HumanBase.def().health };
        if self.w.vanilla {
            // 1.1's class health (dec041)
            let class = if c.team == Team::Aliens { c.ps.stat(Stat::Class) } else { ClassId::HumanBase as i32 };
            max_health = trem_sim::kernel::bg11::health(trem_sim::kernel::v11::class_to_11(class));
        }
        let percent = (100.0 * c.ps.stat(Stat::Health).max(0) as f32 / max_health.max(1) as f32) as i32;
        if percent < 33 {
            '1'
        } else if percent < 67 {
            '3'
        } else {
            '2'
        }
    }

    /// `CG_DrawTeamOverlay` (cg_drawTeamOverlay 1 = all teammates, sorted by
    /// score), with the clientinfo the server's TeamplayInfoMessage sends:
    /// location, health, weapon (humans) or class (aliens), best upgrade.
    fn team_overlay(&mut self, r: [f32; 4], scale: f32, color: Rgba) {
        let mode = self.cvars.draw_team_overlay;
        if self.ps.pm_type == trem_sim::gen::enums::PmType::Spectator as i32 || mode == 0 || self.cvars.team_overlay_max_players == 0 {
            return;
        }
        if self.ps.pm_type == trem_sim::gen::enums::PmType::Intermission as i32 {
            return;
        }
        let w = self.w;
        let pci = (self.ps.client_num.max(0) as usize).min(MAX_CLIENTS - 1);
        let pteam = w.clients[pci].team;
        let mut display: Vec<usize> = Vec::new();
        if mode == 1 || mode == 2 {
            for i in 0..MAX_CLIENTS {
                let ci = &w.clients[i];
                if !ci.connected || i == pci || ci.team != pteam {
                    continue;
                }
                if mode == 1 {
                    display.push(i);
                } else {
                    // PCL_ALIEN_BUILDER0(_UPG), PCL_ALIEN_LEVEL1(_UPG) or WP_HBUILD
                    let support = [ClassId::Builder as i32, ClassId::Builderupg as i32, ClassId::Level1 as i32, ClassId::Level1upg as i32, WeaponId::Ckit as i32, WeaponId::Ackit as i32];
                    if support.contains(&self.weapon_class(i)) {
                        display.push(i);
                    }
                }
            }
        } else {
            // nearby: snapshot players of our team within HELMET_RANGE
            for i in 0..MAX_CLIENTS {
                let e = &w.ents[i];
                if !e.inuse || e.etype != EType::Player || w.clients[i].team != pteam || e.e_flags & k::EF_DEAD != 0 {
                    continue;
                }
                if !w.in_pvs(&self.refdef.vieworg, &w.clients[i].ps.origin) {
                    continue;
                }
                if length(&sub(&w.clients[i].ps.origin, &self.ps.origin)) < k::HELMET_RANGE {
                    display.push(i);
                }
            }
        }
        match self.cvars.team_overlay_sort_mode {
            1 => display.sort_by_key(|&i| -w.clients[i].score),
            2 => display.sort_by(|&a, &b| {
                // SortWeaponClass: grangers on top
                let (ca, cb) = (self.weapon_class(a), self.weapon_class(b));
                let mut out = cb - ca;
                let builders = [ClassId::Builder as i32, ClassId::Builderupg as i32];
                if w.clients[a].team == Team::Aliens && (builders.contains(&ca) || builders.contains(&cb)) {
                    out = -out;
                }
                out.cmp(&0)
            }),
            _ => {}
        }
        display.truncate(self.cvars.team_overlay_max_players.max(0) as usize);
        let x = r[0];
        let icon_size = r[3] / 8.0 * scale;
        let left_margin = 4.0 * scale;
        let icon_top_margin = 2.0 * scale;
        let mid_sep = 2.0 * scale;
        let background_width = r[2] * scale;
        let font_scale = 0.30 * scale;
        let name_width = 0.5 * r[2] * scale;
        let max_x = r[0] + r[2];
        let v_pad = (r[3] - display.len() as f32 * icon_size) / 2.0;
        let mut y = r[1] + v_pad;
        let tcolor = [1.0, 1.0, 1.0, color[3]];
        let my_team = self.team();
        for &i in &display {
            let ci = &w.clients[i];
            let name = format!("{}^7", ci.name);
            self.pic(TEAM_OVERLAY_SHADER, x, y, background_width, icon_size, color);
            let health = ci.ps.stat(Stat::Health).max(0);
            let cur = self.weapon_class(i);
            let s = if health <= 0 || cur == 0 {
                String::new()
            } else {
                let cur_weapon = if ci.team == Team::Humans { cur } else { ClassId::from_num(cur.clamp(0, 255) as u8).and_then(|c| c.def().start_weapon).map(|w| w as i32).unwrap_or(0) };
                if let Some(icon) = self.data.weapons.get(cur_weapon.max(0) as usize).and_then(|w| w.icon.clone()) {
                    self.pic(&icon, x + left_margin, y, icon_size, icon_size, tcolor);
                }
                if my_team == Team::Humans as i32 {
                    if let Some(icon) = self.team_upgrade(i).and_then(|u| self.upgrade_icon(u)) {
                        self.pic(&icon, x + icon_size + left_margin, y, icon_size, icon_size, tcolor);
                    }
                } else if [WeaponId::Abuildupg, WeaponId::Level1upg, WeaponId::Level2upg, WeaponId::Level3upg].iter().any(|w| *w as i32 == cur_weapon) {
                    self.pic(UPGRADE_CLASS_ICON, x + icon_size + left_margin, y, icon_size, icon_size, tcolor);
                }
                let loc = trem_sim::kernel::team::get_location(w, i).map(|l| w.ents[l].generic1.max(0) as usize).unwrap_or(0);
                let loc = w.location_names.get(loc).cloned().unwrap_or_default();
                format!(" [^{}{:3}^7] ^7{}", self.color_char_for_health(i), health, loc)
            };
            let name_max_x = x + 2.0 * icon_size + left_margin + mid_sep + name_width;
            let mut name_max_x_cp = name_max_x;
            self.text_paint_limit(&mut name_max_x_cp, x + 2.0 * icon_size + left_margin + mid_sep, y + icon_size - icon_top_margin, font_scale, tcolor, &name);
            let mut max_x_cp = max_x;
            self.text_paint_limit(&mut max_x_cp, name_max_x, y + icon_size - icon_top_margin, font_scale, tcolor, &s);
            y += icon_size;
        }
    }

    /// TeamplayInfoMessage's curWeaponClass: weapon for humans, class for
    /// aliens, none while not spawned.
    fn weapon_class(&self, i: usize) -> i32 {
        let c = &self.w.clients[i];
        if c.spectator != trem_sim::gen::enums::SpectatorState::Not as i32 {
            0
        } else if c.team == Team::Humans {
            c.ps.weapon
        } else if c.team == Team::Aliens {
            c.ps.stat(Stat::Class)
        } else {
            0
        }
    }

    /// TeamplayInfoMessage's upgrade: the best one a human carries.
    fn team_upgrade(&self, i: usize) -> Option<UpgradeId> {
        let c = &self.w.clients[i];
        if c.spectator != trem_sim::gen::enums::SpectatorState::Not as i32 || c.team != Team::Humans {
            return None;
        }
        [UpgradeId::Bsuit, UpgradeId::Jetpack, UpgradeId::Battpack, UpgradeId::Helmet, UpgradeId::Larmour].into_iter().find(|u| bg::inventory_contains_upgrade(*u as i32, &c.ps.stats))
    }

    /// `CG_ScanForCrosshairEntity`.
    fn scan_crosshair_entity(&mut self) {
        let rd = self.refdef;
        let end = ma(&rd.vieworg, 131072.0, &rd.axis[0]);
        let tr = self.w.trace(&rd.vieworg, Some(&[0.0; 3]), Some(&[0.0; 3]), &end, self.view as i32, (contents::SOLID | contents::BODY) as i32);
        if self.w.point_contents(&tr.endpos, 0) & contents::FOG as i32 != 0 {
            return;
        }
        let n = tr.entity_num;
        if n as usize >= MAX_CLIENTS || n < 0 {
            let e = self.w.ents.get(n.max(0) as usize).filter(|_| n >= 0);
            self.st.crosshair_buildable = if e.is_some_and(|e| e.etype == EType::Buildable && e.buildable.is_some_and(|b| b.def().team as i32 == self.team())) { n } else { -1 };
            return;
        }
        let team = self.w.clients[n as usize].team as i32;
        if self.team() != Team::None as i32 && team != self.team() {
            return;
        }
        self.st.crosshair_client = n;
        self.st.crosshair_client_time = self.time;
    }

    /// `CG_DrawCrosshairNames` (with the teamoverlay health suffix).
    fn crosshair_names(&mut self, r: [f32; 4], scale: f32, style: i32) {
        if self.cvars.draw_crosshair_names == 0 || self.third_person {
            return;
        }
        self.scan_crosshair_entity();
        let Some(a) = self.fade(self.st.crosshair_client_time, CROSSHAIR_CLIENT_TIMEOUT) else { return };
        let n = self.st.crosshair_client.clamp(0, MAX_CLIENTS as i32 - 1) as usize;
        let cl = &self.w.clients[n];
        let mut name = cl.name.clone();
        let health = cl.ps.stat(Stat::Health);
        if self.cvars.team_overlay_userinfo != 0 && self.team() != Team::None as i32 && health > 0 {
            name = format!("{name} ^7[^{}{}^7]", self.color_char_for_health(n), health);
        }
        let w = self.text_width(&name, scale);
        let x = r[0] + r[2] / 2.0;
        self.text_paint(x - w / 2.0, r[1] + r[3], scale, [1.0, 1.0, 1.0, a], &name, style);
    }

    /// `CG_DrawCrosshair`: the weapon's crosshair shader (its stages with
    /// `rgbGen vertex` take the item colour), crossHairSize x cg_crosshairSize,
    /// centred on the item rect, dimmed when aimed at a friend.
    fn crosshair(&mut self, r: [f32; 4], mut color: Rgba) {
        let wp = player_weapon(self.ps);
        if self.cvars.draw_crosshair == CROSSHAIR_ALWAYSOFF {
            return;
        }
        if self.cvars.draw_crosshair == CROSSHAIR_RANGEDONLY && !weapon_id(wp).is_some_and(|w| w.def().long_ranged) {
            return;
        }
        if self.ps.pers(Pers::Specstate) != 0 || self.third_person || self.ps.pm_type == trem_sim::gen::enums::PmType::Intermission as i32 {
            return;
        }
        let Some((size, stages)) = self.data.weapons.get(wp.max(0) as usize).and_then(|w| w.crosshair.clone()) else { return };
        let h = size * self.cvars.crosshair_size;
        let w = h * self.aspect;
        let (x, y) = (r[0] + r[2] / 2.0 - w / 2.0, r[1] + r[3] / 2.0 - h / 2.0);
        if self.time == self.st.crosshair_client_time || self.st.crosshair_buildable >= 0 {
            for c in color.iter_mut().take(3) {
                *c *= 0.5;
            }
        }
        for s in stages {
            let c = s.color.unwrap_or(color);
            let rot = s.rotate * self.time as f32 / 1000.0;
            if c[3] > 0.0 {
                self.out.push(Cmd::Pic { img: s.image.clone(), r: [x, y, w, h], c, rot, uv: UV_FULL });
            }
        }
    }

    /// `CG_ResetPainBlend`.
    /// `CG_TileClear` / `CG_TileClearBox`: cgs.media.backTileShader
    /// ("console") repeated as 64x64 pixel tiles around a sized-down view.
    /// The C works in screen pixels; the HUD draws in 640x480 units.
    fn tile_clear(&mut self, vrect: (i32, i32, i32, i32), screen: (f32, f32)) {
        let (w, h) = (screen.0 as i32, screen.1 as i32);
        let (x, y, rw, rh) = vrect;
        if rw <= 0 || rh <= 0 || (x == 0 && y == 0 && rw == w && rh == h) {
            return; // full screen rendering
        }
        let top = y;
        let bottom = top + rh - 1;
        let left = x;
        let right = left + rw - 1;
        let (sx, sy) = (640.0 / screen.0, 480.0 / screen.1);
        let mut box_ = |x: i32, y: i32, w: i32, h: i32| {
            let (s1, t1, s2, t2) = (x as f32 / 64.0, y as f32 / 64.0, (x + w) as f32 / 64.0, (y + h) as f32 / 64.0);
            if w > 0 && h > 0 {
                self.out.push(Cmd::Quad { img: "console".into(), r: [x as f32 * sx, y as f32 * sy, w as f32 * sx, h as f32 * sy], c: WHITE, st: [[s1, t1], [s2, t1], [s2, t2], [s1, t2]] });
            }
        };
        // clear above view screen
        box_(0, 0, w, top);
        // clear below view screen
        box_(0, bottom, w, h - bottom);
        // clear left of view screen
        box_(0, top, left, bottom - top + 1);
        // clear right of view screen
        box_(right, top, w - right, bottom - top + 1);
    }

    fn reset_pain_blend(&mut self) {
        self.st.pain_blend_value = 0.0;
        self.st.pain_blend_target = 0.0;
        self.st.last_health = self.stat(Stat::Health);
    }

    /// `CG_PainBlend` (with `CG_ScalePainBlendTCs`): the team-coloured
    /// gfx/damage/fullscreen_painblend border, its two stages rotating
    /// their texture coordinates (tcMod rotate 90 / -90).
    fn pain_blend(&mut self) {
        const PAINBLEND_BORDER: f32 = 0.15;
        let key = (self.ps.pers(Pers::SpawnCount), self.ps.client_num, self.stat(Stat::Class));
        if self.st.pain_reset_key != Some(key) {
            self.st.pain_reset_key = Some(key);
            self.reset_pain_blend();
        }
        if self.ps.pers(Pers::Specstate) != 0 || self.intermission_started() {
            return;
        }
        let damage = (self.st.last_health - self.stat(Stat::Health)).max(0);
        let frac = damage as f32 / self.stat(Stat::MaxHealth).max(1) as f32;
        self.st.last_health = self.stat(Stat::Health);
        let cv = &self.cvars;
        let (down, up, max, scale, zoom) = (cv.pain_blend_down_rate, cv.pain_blend_up_rate, cv.pain_blend_max, cv.pain_blend_scale, cv.pain_blend_zoom);
        self.st.pain_blend_value += frac * scale;
        if self.st.pain_blend_value > 0.0 {
            self.st.pain_blend_value -= (self.frametime as f32 / 1000.0) * down;
        }
        if self.st.pain_blend_value > 1.0 {
            self.st.pain_blend_value = 1.0;
        } else if self.st.pain_blend_value <= 0.0 {
            self.st.pain_blend_value = 0.0;
            return;
        }
        let mut color = [1.0f32; 4];
        if self.team() == Team::Aliens as i32 {
            color = [0.43, 0.8, 0.37, 1.0];
        } else if self.team() == Team::Humans as i32 {
            color = [0.8, 0.0, 0.0, 1.0];
        }
        if self.st.pain_blend_value > self.st.pain_blend_target {
            self.st.pain_blend_target += (self.frametime as f32 / 1000.0) * up;
        } else if self.st.pain_blend_value < self.st.pain_blend_target {
            self.st.pain_blend_target = self.st.pain_blend_value;
        }
        if self.st.pain_blend_target > max {
            self.st.pain_blend_target = max;
        }
        color[3] = self.st.pain_blend_target;
        let b = PAINBLEND_BORDER;
        // (x, y, w, h) in 640x480 and (s1, t1, s2, t2): left, right, top, bottom
        let parts = [
            ([0.0, 0.0, b * 640.0, 480.0], [0.0, 0.0, b, 1.0]),
            ([640.0 - b * 640.0, 0.0, b * 640.0, 480.0], [1.0 - b, 0.0, 1.0, 1.0]),
            ([b * 640.0, 0.0, 640.0 - 2.0 * b * 640.0, b * 480.0], [b, 0.0, 1.0 - b, b]),
            ([b * 640.0, 480.0 - b * 480.0, 640.0 - 2.0 * b * 640.0, b * 480.0], [b, 1.0 - b, 1.0 - b, 1.0]),
        ];
        let shader_time = self.time as f32 / 1000.0;
        for (r, tc) in parts {
            // CG_ScalePainBlendTCs
            let tc = tc.map(|v| (v - 0.5) * zoom + 0.5);
            let corners = [[tc[0], tc[1]], [tc[2], tc[1]], [tc[2], tc[3]], [tc[0], tc[3]]];
            for deg_per_sec in [90.0f32, -90.0] {
                // RB_CalcRotateTexCoords
                let degs = (-deg_per_sec * shader_time).to_radians();
                let (sn, cs) = degs.sin_cos();
                let st = corners.map(|[s, t]| [s * cs - t * sn + 0.5 - 0.5 * cs + 0.5 * sn, s * sn + t * cs + 0.5 - 0.5 * sn - 0.5 * cs]);
                self.out.push(Cmd::Quad { img: "gfx/damage/fullscreen_painblend".into(), r, c: color, st });
            }
        }
    }

    /// `CG_DrawLighting`.
    fn lighting(&mut self) {
        let s = self.stat(Stat::Stamina);
        if s < k::STAMINA_BLACKOUT_LEVEL && self.team() == Team::Humans as i32 {
            let a = 1.0 - (s + 1000) as f32 / 200.0;
            self.fill(0.0, 0.0, 640.0, 480.0, [0.0, 0.0, 0.0, a]);
        }
    }

    // ------------------------------------------------------------ cg_scanner.c
    /// `CG_UpdateEntityPositions`.
    fn update_entity_positions(&mut self) {
        let p = &mut self.st.positions;
        if self.time < p.last_update {
            p.last_update = 0;
        }
        if self.ps.stats[Stat::Team as usize] == Team::Humans as i32 && p.last_update + HUMAN_SCANNER_UPDATE_PERIOD > self.time {
            return;
        }
        p.origin = bg::client_view_origin(self.ps);
        p.vangles = self.ps.viewangles;
        p.last_update = self.time;
        p.alien_buildables.clear();
        p.human_buildables.clear();
        p.alien_clients.clear();
        p.human_clients.clear();
        let w = self.w;
        for i in 0..w.num_entities.min(w.ents.len()) {
            let e = &w.ents[i];
            if !e.inuse || i == self.view {
                continue;
            }
            // only what the snapshot carries: entities in the viewer's PVS
            // (SV_AddEntitiesToPacket; tested at the entity origin)
            let eo = if i < MAX_CLIENTS && e.client { w.clients[i].ps.origin } else { e.origin };
            if !w.in_pvs(&p.origin, &eo) {
                continue;
            }
            match e.etype {
                EType::Buildable if e.e_flags & k::EF_DEAD == 0 => match e.team {
                    Team::Aliens => p.alien_buildables.push(e.origin),
                    Team::Humans => p.human_buildables.push(e.origin),
                    _ => {}
                },
                EType::Player if i < MAX_CLIENTS => {
                    let o = w.clients[i].ps.origin;
                    match w.clients[i].team {
                        Team::Aliens => p.alien_clients.push(o),
                        Team::Humans => p.human_clients.push(o),
                        _ => {}
                    }
                }
                _ => {}
            }
        }
    }

    /// cg_scanner.c `CG_DrawBlips`.
    fn blip(&mut self, r: [f32; 4], origin: &Vec3, colour: Rgba) {
        let p = &self.st.positions;
        let tfrac = 1.0 - (self.time - p.last_update) as f32 / HUMAN_SCANNER_UPDATE_PERIOD as f32;
        let mut d = rotate_point_around_vector(&[0.0, 0.0, 1.0], origin, -p.vangles[1] - 90.0);
        d[0] /= 2.0 * k::HELMET_RANGE / r[2];
        d[1] /= 2.0 * k::HELMET_RANGE / r[3];
        d[2] /= 2.0 * k::HELMET_RANGE / r[2];
        let alpha_mod = 0.8 + ((d[1] + r[3] / 2.0) / r[3]) * (1.2 - 0.8);
        let mut c = colour;
        c[3] = (c[3] * alpha_mod * (0.5 + tfrac * 0.5)).clamp(0.0, 1.0);
        let stalk = 2.0 * self.aspect;
        let (blipx, blipy) = (16.0 * self.aspect, 8.0);
        let sx = r[0] + r[2] / 2.0 - stalk / 2.0 - d[0];
        if d[2] > 0.0 {
            self.pic("gfx/2d/stalk", sx, r[1] + r[3] / 2.0 + d[1] - d[2], stalk, d[2], c);
        } else {
            self.pic("gfx/2d/stalk", sx, r[1] + r[3] / 2.0 + d[1], stalk, -d[2], c);
        }
        self.pic("gfx/2d/blip", r[0] + r[2] / 2.0 - blipx / 2.0 - d[0], r[1] + r[3] / 2.0 - blipy / 2.0 + d[1] - d[2], blipx, blipy, c);
    }

    /// cg_scanner.c `CG_Scanner` (cg_disableScannerPlane 0).
    fn scanner(&mut self, r: [f32; 4], shader: &str, color: Rgba) {
        let o = self.st.positions.origin;
        let hab = [color[0], color[1], color[2], color[3] * 1.5];
        let hbe = color;
        let aab = [1.0, 0.0, 0.0, 0.75];
        let abe = [1.0, 0.0, 0.0, 0.5];
        let p = &self.st.positions;
        let lists: [(Vec<Vec3>, Rgba, Rgba); 4] = [
            (p.human_buildables.clone(), hbe, hab),
            (p.alien_buildables.clone(), abe, aab),
            (p.human_clients.clone(), hbe, hab),
            (p.alien_clients.clone(), abe, aab),
        ];
        for (list, below, _) in &lists {
            for pos in list {
                let rel = sub(pos, &o);
                if length(&rel) < k::HELMET_RANGE && rel[2] < 0.0 {
                    self.blip(r, &rel, *below);
                }
            }
        }
        self.pic(shader, r[0], r[1], r[2], r[3], color);
        for (list, _, above) in &lists {
            for pos in list {
                let rel = sub(pos, &o);
                if length(&rel) < k::HELMET_RANGE && rel[2] > 0.0 {
                    self.blip(r, &rel, *above);
                }
            }
        }
    }

    /// cg_scanner.c `CG_DrawDir`.
    fn dir(&mut self, r: [f32; 4], origin: &Vec3, colour: Rgba) {
        let normal = bg::client_normal(self.ps);
        let (view, _, _) = angle_vectors(&self.st.positions.vangles);
        let mut no_z_origin = project_point_on_plane(origin, &normal);
        let mut no_z_view = project_point_on_plane(&view, &normal);
        normalize(&mut no_z_origin);
        normalize(&mut no_z_view);
        let mut angle = dot(&no_z_origin, &no_z_view).clamp(-1.0, 1.0).acos().to_degrees();
        let mut anti = cross(&no_z_origin, &no_z_view);
        normalize(&mut anti);
        if length(&sub(&normal, &anti)) < 1.0 {
            angle = 360.0 - angle;
        }
        let d = rotate_point_around_vector(&[0.0, 0.0, 1.0], &[0.0, -1.0, 0.0], angle);
        let (bx, by) = (24.0 * self.aspect, 24.0);
        self.pic("gfx/2d/blip", r[0] + r[2] / 2.0 - bx / 2.0 - d[0] * (r[2] / 2.0), r[1] + r[3] / 2.0 - by / 2.0 + d[1] * (r[3] / 2.0), bx, by, colour);
    }

    /// cg_scanner.c `CG_AlienSense`.
    fn alien_sense(&mut self, r: [f32; 4]) {
        let o = self.st.positions.origin;
        let list: Vec<(Vec3, Rgba)> = self.st.positions.human_buildables.iter().map(|p| (*p, [1.0, 0.0, 0.0, 0.7])).chain(self.st.positions.human_clients.iter().map(|p| (*p, [0.0, 0.0, 1.0, 0.7]))).collect();
        for (pos, c) in list {
            let rel = sub(&pos, &o);
            if length(&rel) < k::ALIENSENSE_RANGE {
                self.dir(r, &rel, c);
            }
        }
    }

    // ------------------------------------------------------------ cg_buildable.c
    /// `CG_WorldToScreen` (cg_viewsize 100).
    fn world_to_screen(&self, point: &Vec3) -> Option<(f32, f32)> {
        let rd = &self.refdef;
        let trans = sub(point, &rd.vieworg);
        let xc = 640.0 * self.cvars.viewsize as f32 / 200.0;
        let yc = 480.0 * self.cvars.viewsize as f32 / 200.0;
        let z = dot(&trans, &rd.axis[0]);
        if z <= 0.001 {
            return None;
        }
        Some((320.0 - dot(&trans, &rd.axis[1]) * xc / (z * rd.px), 240.0 - dot(&trans, &rd.axis[2]) * yc / (z * rd.py)))
    }

    /// `CG_PlayerIsBuilder`: holding a build weapon of the buildable's team.
    fn player_is_builder(&self, b: BuildableId) -> bool {
        let wp = self.ps.weapon;
        is_builder_weapon(wp) && weapon_id(wp).is_some_and(|w| w.def().team == b.def().team)
    }

    /// `CG_DrawBuildableStatus`: the snapshot's buildables the player can
    /// build, farthest first.
    fn buildable_status(&mut self) {
        let w = self.w;
        let mut list: Vec<usize> = (MAX_CLIENTS..w.num_entities.min(w.ents.len()))
            .filter(|&i| {
                let e = &w.ents[i];
                e.inuse && e.etype == EType::Buildable && e.buildable.is_some_and(|b| self.player_is_builder(b)) && w.in_pvs(&self.refdef.vieworg, &e.origin)
            })
            .collect();
        let vo = self.refdef.vieworg;
        let dist = |i: &usize| length(&sub(&vo, &w.ents[*i].origin));
        list.sort_by(|a, b| dist(b).partial_cmp(&dist(a)).unwrap_or(std::cmp::Ordering::Equal));
        for i in list {
            self.buildable_status_display(i);
        }
    }

    /// `CG_BuildableStatusDisplay`: the health bar, no-power / marked icons
    /// and health number over a buildable, fading in and out with its
    /// visibility (traced from the eye and 20 units left and right).
    fn buildable_status_display(&mut self, n: usize) {
        let w = self.w;
        let e = &w.ents[n];
        let Some(b) = e.buildable else { return };
        let bd = b.def();
        let data: &'a HudData = self.data;
        let bs = if bd.team == Team::Aliens { &data.alien_build_stat } else { &data.human_build_stat };
        if !bs.loaded {
            return;
        }
        let rd = self.refdef;
        let d = length(&sub(&e.origin, &rd.vieworg));
        if d > STATUS_MAX_VIEW_DIST {
            return;
        }
        let mut color = bs.fore_color;
        // trace for the centre point
        let (mins, mut maxs) = (bd.mins, bd.maxs);
        let anim = e.torso_anim & !(k::ANIM_FORCEBIT | k::ANIM_TOGGLEBIT);
        if b == BuildableId::Barricade && (anim == trem_sim::gen::enums::BuildableAnim::Destroyed as i32 || e.e_flags & k::EF_B_SPAWNED == 0) {
            maxs[2] = (maxs[2] * k::BARRICADE_SHRINKPROP) as i32 as f32;
        }
        let mut origin = e.origin;
        origin[2] += mins[2];
        origin[2] += (mins[2].abs() as i32 + maxs[2].abs() as i32) as f32 / 2.0;
        let mut ent_num = self.view as i32;
        let mut visible = false;
        // if the first try fails, step right, then left
        for j in 0..3 {
            let mut tr_origin = rd.vieworg;
            let right = [-rd.axis[1][0], -rd.axis[1][1], -rd.axis[1][2]];
            match j {
                1 => tr_origin = ma(&tr_origin, STATUS_PEEK_DIST, &right),
                2 => tr_origin = ma(&tr_origin, -STATUS_PEEK_DIST, &right),
                _ => {}
            }
            // look through up to 3 players and/or transparent buildables
            for _ in 0..3 {
                let tr = w.trace(&tr_origin, None, None, &origin, ent_num, k::MASK_SHOT);
                if tr.entity_num == n as i32 {
                    visible = true;
                    break;
                }
                if tr.entity_num == k::ENTITYNUM_WORLD {
                    break;
                }
                let Some(hit) = w.ents.get(tr.entity_num.max(0) as usize).filter(|_| tr.entity_num >= 0) else { break };
                if (tr.entity_num as usize) < MAX_CLIENTS || (hit.etype == EType::Buildable && (e.e_flags & k::EF_B_SPAWNED == 0 || hit.buildable.is_some_and(|hb| hb.def().transparent_test))) {
                    ent_num = tr.entity_num;
                    tr_origin = tr.endpos;
                } else {
                    break;
                }
            }
            if visible {
                break;
            }
        }
        // hack to make the kit obscure view
        if self.cvars.draw_gun != 0 && visible && self.team() == Team::Humans as i32 {
            if let Some((x, y)) = self.world_to_screen(&origin) {
                if x > 450.0 && y > 290.0 {
                    visible = false;
                }
            }
        }
        let status = self.st.buildable_status.entry(n).or_insert((false, 0));
        if !visible && status.0 {
            *status = (false, self.time);
        } else if visible && !status.0 {
            *status = (true, self.time);
        }
        let (vis, last) = *status;
        if vis {
            // fade up
            if last + STATUS_FADE_TIME > self.time {
                color[3] = (self.time - last) as f32 / STATUS_FADE_TIME as f32;
            }
        } else if last + STATUS_FADE_TIME > self.time {
            // fade down
            color[3] = 1.0 - (self.time - last) as f32 / STATUS_FADE_TIME as f32;
        } else {
            return;
        }
        let health = e.generic1;
        let mut health_scale = health as f32 / bd.health as f32;
        if health > 0 && health_scale < 0.01 {
            health_scale = 0.01;
        } else {
            health_scale = health_scale.clamp(0.0, 1.0);
        }
        let Some((x, y)) = self.world_to_screen(&origin) else { return };
        let mut pic_h = bs.frame_height as f32;
        let mut pic_w = bs.frame_width as f32;
        // fudged to get the cfg's width/height more realistic
        let scale = (pic_h / d) * 3.0;
        let powered = e.e_flags & k::EF_B_POWERED != 0;
        let marked = e.e_flags & k::EF_B_MARKED != 0;
        pic_h *= scale;
        pic_w *= scale;
        let pic_x = x - pic_w * 0.5;
        let pic_y = y - pic_h * 0.5;
        // sub-elements such as icons and number
        let sub_h = pic_h - pic_h * bs.vertical_margin;
        let sub_y = pic_y + pic_h * 0.5 - sub_h * 0.5;
        // (CG_SetClipRegion to the view: the whole screen at cg_viewsize 100)
        let frame_color = [bs.back_color[0], bs.back_color[1], bs.back_color[2], color[3]];
        if let Some(f) = &bs.frame_shader {
            self.pic(f, pic_x, pic_y, pic_w, pic_h, frame_color);
        }
        if health > 0 {
            let pad = bs.health_padding as f32 * scale;
            let (hx, hy, hh, hw) = (pic_x + pad, pic_y + pad, pic_h - pad * 2.0, pic_w * health_scale - pad * 2.0);
            let mut hc = if health_scale == 1.0 {
                bs.health_low_color
            } else if health_scale >= 0.75 {
                bs.health_guarded_color
            } else if health_scale >= 0.50 {
                bs.health_elevated_color
            } else if health_scale >= 0.25 {
                bs.health_high_color
            } else {
                bs.health_severe_color
            };
            hc[3] = color[3];
            self.fill(hx, hy, hw, hh, hc);
        }
        if let Some(o) = &bs.overlay_shader {
            let (ow, oh) = (bs.overlay_width as f32 * scale, bs.overlay_height as f32 * scale);
            self.pic(o, x - ow * 0.5, y - oh * 0.5, ow, oh, frame_color);
        }
        // (unchecked in the C: handle 0 is the renderer's default shader)
        if !powered {
            let s = bs.no_power_shader.as_deref().unwrap_or(DEFAULT_SHADER);
            self.pic(s, pic_x + sub_h * bs.horizontal_margin, sub_y, sub_h, sub_h, color);
        }
        if marked {
            let s = bs.marked_shader.as_deref().unwrap_or(DEFAULT_SHADER);
            self.pic(s, pic_x + pic_w - sub_h * bs.horizontal_margin - sub_h, sub_y, sub_h, sub_h, color);
        }
        let health_max = bd.health;
        let mut health_points = (health_scale * health_max as f32) as i32;
        if health > 0 && health_points < 1 {
            health_points = 1;
        }
        let mut nx = pic_x + pic_w * 0.5 - 2.0 - (sub_h * 4.0) * 0.5;
        if health_points > 999 {
            nx -= 0.0;
        } else if health_points > 99 {
            nx -= sub_h * 0.5;
        } else if health_points > 9 {
            nx -= sub_h;
        } else {
            nx -= sub_h * 1.5;
        }
        self.field(nx, sub_y, 4, sub_h, sub_h, health_points, color);
    }

    // ------------------------------------------------------------ cg_tutorial.c
    /// `CG_BuildableInRange`: a buildable of our team 64 units ahead.
    fn buildable_in_range(&self) -> Option<&'a trem_sim::kernel::world::Entity> {
        let w: &'a World = self.w;
        let e = &w.ents[self.entity_in_front()?];
        (e.etype == EType::Buildable && e.buildable.is_some_and(|b| b.def().team as i32 == self.team())).then_some(e)
    }

    /// The "place / cancel / build" lines shared by CG_AlienBuilderText and
    /// CG_HumanCkitText, then the deconstruct line (`mark_suffix` differs).
    fn builder_lines(&self, text: &mut String, mark_suffix: &str, destroy: bool) {
        let buildable = self.stat(Stat::Buildable) & !k::SB_VALID_TOGGLEBIT;
        if let Some(b) = BuildableId::from_num(buildable.clamp(0, 255) as u8).filter(|_| buildable > 0) {
            let name = b.cold().display_name;
            text.push_str(&format!("Press {} to place the {name}\n", key_name_for_command(self.binds, "+attack")));
            text.push_str(&format!("Press {} to cancel placing the {name}\n", key_name_for_command(self.binds, "+button5")));
        } else {
            text.push_str(&format!("Press {} to build a structure\n", key_name_for_command(self.binds, "+attack")));
        }
        if let Some(e) = self.buildable_in_range() {
            let key = key_name_for_command(self.binds, "deconstruct");
            if self.w.level.mark_deconstruct != 0 {
                if e.e_flags & k::EF_B_MARKED != 0 {
                    text.push_str(&format!("Press {key} to unmark this structure{mark_suffix}\n"));
                } else {
                    text.push_str(&format!("Press {key} to mark this structure{mark_suffix}\n"));
                }
            } else if destroy {
                text.push_str(&format!("Press {key} to destroy this structure\n"));
            }
        }
    }

    /// `CG_AlienBuilderText`.
    fn alien_builder_text(&self, text: &mut String) {
        self.builder_lines(text, " for replacement", true);
        if self.stat(Stat::Buildable) & !k::SB_VALID_TOGGLEBIT == 0 {
            text.push_str(&format!("Press {} to swipe\n", key_name_for_command(self.binds, "+button5")));
        }
        if self.stat(Stat::Class) == ClassId::Builderupg as i32 {
            text.push_str(&format!("Press {} to launch a projectile\n", key_name_for_command(self.binds, "+button2")));
            text.push_str(&format!("Press {} to walk on walls\n", key_name_for_command(self.binds, "+movedown")));
        }
    }

    /// `CG_HumanText` (with `CG_HumanCkitText`).
    fn human_text(&self, text: &mut String) {
        let ps = self.ps;
        let sel = self.st.weapon_select;
        let upgrade = if sel >= 32 { UpgradeId::from_num((sel - 32).clamp(0, 255) as u8) } else { None };
        let name = self.weapon_select_name().unwrap_or("");
        let wp = weapon_id(ps.weapon);
        let wname = wp.map(|w| w.cold().display_name).unwrap_or("");
        if ps.ammo == 0 && ps.clips == 0 && !wp.is_some_and(|w| w.def().infinite_ammo) {
            match wp {
                Some(WeaponId::Rifle | WeaponId::Chaingun | WeaponId::Shotgun | WeaponId::Flamer) => text.push_str(&format!("Find an Armoury and press {} for more ammo\n", key_name_for_command(self.binds, "buy ammo"))),
                Some(WeaponId::Lgun | WeaponId::Prifle | WeaponId::Mdriver | WeaponId::Lcannon) => text.push_str(&format!("Find an Armoury, Reactor, or Repeater and press {} for more ammo\n", key_name_for_command(self.binds, "buy ammo"))),
                _ => {}
            }
        } else {
            match wp {
                Some(WeaponId::Blaster | WeaponId::Rifle | WeaponId::Shotgun | WeaponId::Lgun | WeaponId::Chaingun | WeaponId::Prifle | WeaponId::Flamer) => {
                    text.push_str(&format!("Press {} to fire the {wname}\n", key_name_for_command(self.binds, "+attack")));
                }
                Some(WeaponId::Mdriver) => {
                    text.push_str(&format!("Press {} to fire the {wname}\n", key_name_for_command(self.binds, "+attack")));
                    text.push_str(&format!("Hold {} to zoom\n", key_name_for_command(self.binds, "+button5")));
                }
                Some(WeaponId::Psaw) => text.push_str(&format!("Hold {} to activate the {wname}\n", key_name_for_command(self.binds, "+attack"))),
                Some(WeaponId::Lcannon) => {
                    text.push_str(&format!("Hold and release {} to fire a charged shot\n", key_name_for_command(self.binds, "+attack")));
                    text.push_str(&format!("Press {} to fire the {wname}\n", key_name_for_command(self.binds, "+button5")));
                }
                Some(WeaponId::Ckit) | Some(WeaponId::Ackit) => self.builder_lines(text, "", true),
                _ => {}
            }
        }
        text.push_str(&format!("Press {} and ", key_name_for_command(self.binds, "weapprev")));
        text.push_str(&format!("{} to select an upgrade\n", key_name_for_command(self.binds, "weapnext")));
        if sel < 32 || upgrade.is_some_and(|u| u.def().usable) {
            text.push_str(&format!("Press {} to use the {name}\n", key_name_for_command(self.binds, "+button2")));
        }
        if ps.stat(Stat::Health) <= 35 && bg::inventory_contains_upgrade(UpgradeId::Medkit as i32, &ps.stats) {
            text.push_str(&format!("Press {} to use your {}\n", key_name_for_command(self.binds, "itemact medkit"), UpgradeId::Medkit.cold().display_name));
        }
        let stamina = ps.stat(Stat::Stamina);
        if stamina <= k::STAMINA_BLACKOUT_LEVEL {
            text.push_str("You are blacking out. Stop sprinting to recover stamina\n");
        } else if stamina <= k::STAMINA_SLOW_LEVEL {
            text.push_str("Your stamina is low. Stop sprinting to recover\n");
        }
        match self.st.near_usable_buildable {
            Some(BuildableId::Arm) => text.push_str(&format!("Press {} to buy equipment upgrades at the {}. Sell your old weapon first!\n", key_name_for_command(self.binds, "+button7"), BuildableId::Arm.cold().display_name)),
            Some(b @ (BuildableId::Repeater | BuildableId::Reactor)) => text.push_str(&format!("Press {} to refill your energy weapon's ammo at the {}\n", key_name_for_command(self.binds, "+button7"), b.cold().display_name)),
            Some(b) => text.push_str(&format!("Press {} to use the {}\n", key_name_for_command(self.binds, "+button7"), b.cold().display_name)),
            None => {}
        }
        text.push_str(&format!("Press {} and any direction to sprint\n", key_name_for_command(self.binds, "+button8")));
        text.push_str(&format!("Press {} and back or strafe to dodge\n", key_name_for_command(self.binds, "+button6")));
    }

    /// `CG_SpectatorText` (no chase-cam: following is never chasing).
    fn spectator_text(&self, text: &mut String) {
        let ps = self.ps;
        if self.w.clients[self.me].team != Team::None {
            if ps.pm_flags & k::PMF_QUEUED != 0 {
                text.push_str(&format!("Press {} to leave spawn queue\n", key_name_for_command(self.binds, "+attack")));
            } else {
                text.push_str(&format!("Press {} to spawn\n", key_name_for_command(self.binds, "+attack")));
            }
        } else {
            text.push_str(&format!("Press {} to join a team\n", key_name_for_command(self.binds, "+attack")));
        }
        if ps.pm_flags & k::PMF_FOLLOW != 0 {
            text.push_str(&format!("Press {} to switch to chase-cam spectator mode\n", key_name_for_command(self.binds, "+button2")));
            text.push_str(&format!("Press {} or ", key_name_for_command(self.binds, "weapprev")));
            text.push_str(&format!("{} to change player\n", key_name_for_command(self.binds, "weapnext")));
        } else {
            text.push_str(&format!("Press {} to follow a player\n", key_name_for_command(self.binds, "+button2")));
        }
    }

    /// `CG_TutorialText`: context help for the current class / weapon.
    fn tutorial_text(&self) -> String {
        let ps = self.ps;
        let mut text = String::new();
        if !self.intermission_started() {
            if ps.pers(Pers::Specstate) != 0 || ps.pm_flags & k::PMF_FOLLOW != 0 {
                self.spectator_text(&mut text);
            } else if ps.stat(Stat::Health) > 0 {
                let say = |t: &mut String, s: String| t.push_str(&s);
                match ClassId::from_num(ps.stat(Stat::Class).clamp(0, 255) as u8) {
                    Some(ClassId::Builder | ClassId::Builderupg) => self.alien_builder_text(&mut text),
                    Some(ClassId::Level0) => {
                        // `CG_AlienLevel0Text`
                        text.push_str("Touch humans to damage them\n");
                        say(&mut text, format!("Press {} to walk on walls\n", key_name_for_command(self.binds, "+movedown")));
                    }
                    Some(c @ (ClassId::Level1 | ClassId::Level1upg)) => {
                        // `CG_AlienLevel1Text`
                        text.push_str("Touch humans to grab them\n");
                        say(&mut text, format!("Press {} to swipe\n", key_name_for_command(self.binds, "+attack")));
                        if c == ClassId::Level1upg {
                            say(&mut text, format!("Press {} to spray poisonous gas\n", key_name_for_command(self.binds, "+button5")));
                        }
                        say(&mut text, format!("Press {} to walk on walls\n", key_name_for_command(self.binds, "+movedown")));
                    }
                    Some(c @ (ClassId::Level2 | ClassId::Level2upg)) => {
                        // `CG_AlienLevel2Text`
                        say(&mut text, format!("Press {} to bite\n", key_name_for_command(self.binds, "+attack")));
                        if c == ClassId::Level2upg {
                            say(&mut text, format!("Press {} to invoke an electrical attack\n", key_name_for_command(self.binds, "+button5")));
                        }
                        say(&mut text, format!("Hold down {} then touch a wall to wall jump\n", key_name_for_command(self.binds, "+moveup")));
                    }
                    Some(c @ (ClassId::Level3 | ClassId::Level3upg)) => {
                        // `CG_AlienLevel3Text`
                        say(&mut text, format!("Press {} to bite\n", key_name_for_command(self.binds, "+attack")));
                        if c == ClassId::Level3upg {
                            say(&mut text, format!("Press {} to launch a projectile\n", key_name_for_command(self.binds, "+button2")));
                        }
                        say(&mut text, format!("Hold down and release {} to pounce\n", key_name_for_command(self.binds, "+button5")));
                    }
                    Some(ClassId::Level4) => {
                        // `CG_AlienLevel4Text`
                        say(&mut text, format!("Press {} to swipe\n", key_name_for_command(self.binds, "+attack")));
                        say(&mut text, format!("Hold down and release {} to trample\n", key_name_for_command(self.binds, "+button5")));
                    }
                    Some(ClassId::HumanBase | ClassId::HumanBsuit) => self.human_text(&mut text),
                    _ => {}
                }
                if ps.stat(Stat::Team) == Team::Aliens as i32 && self.can_evolve(ps.pers(Pers::Credit)) {
                    say(&mut text, format!("Press {} to evolve\n", key_name_for_command(self.binds, "+button7")));
                }
            }
        } else if !self.w.clients[self.ps.client_num.clamp(0, MAX_CLIENTS as i32 - 1) as usize].ready_to_exit {
            text.push_str(&format!("Press {} when ready to continue\n", key_name_for_command(self.binds, "+attack")));
        } else {
            text.push_str("Waiting for other players to be ready\n");
        }
        text.push_str("Press ESC for the menu");
        text
    }
}

/// The inputs of one HUD frame besides the world.
struct FrameInput {
    me: usize,
    view: usize,
    refdef: Option<Refdef>,
    aspect: f32,
    xs: f32,
    ys: f32,
    third_person: bool,
    chase_follow: bool,
    /// cg.refdef x, y, width, height and the screen size (pixels)
    vrect: (i32, i32, i32, i32),
    screen: (f32, f32),
    cvars: Cvars,
    real: RealTime,
    /// the client's bind table
    binds: Vec<String>,
    /// cg.weaponSelect, cg.weaponSelectTime (the client's)
    weapon_select: (i32, i32),
}

/// One frame of `CG_Draw2D`'s HUD part (`CG_DrawLighting`, the class HUD
/// menu or default_hud, `CG_DrawBuildableStatus`, `CG_DrawIntermission`).
fn build_frame(data: &HudData, st: &mut HudState, w: &World, inp: &FrameInput) -> Vec<Cmd> {
    let (Some(_), Some(cl)) = (w.clients.get(inp.view), w.clients.get(inp.me)) else { return Vec::new() };
    let ps = &cl.ps;
    let time = w.level.time;
    let frametime = if st.last_time > 0 && time >= st.last_time { time - st.last_time } else { 0 };
    st.last_time = time;
    // weapon selection display (cg.weaponSelect / weaponSelectTime)
    (st.weapon_select, st.weapon_select_time) = inp.weapon_select;
    st.expire_notify_lines(time, inp.cvars.console_latency);
    // Menu_AspectCompensate'd menus for this aspectScale
    if st.compensated.as_ref().is_none_or(|(a, _)| (*a - inp.aspect).abs() > 1e-6) {
        st.compensated = Some((inp.aspect, data.menus.menus.iter().map(|m| m.aspect_compensated(inp.aspect)).collect()));
    }
    let menus = st.compensated.take().map(|c| c.1).unwrap_or_default();
    let find = |name: &str| menus.iter().find(|m| m.name.eq_ignore_ascii_case(name));
    let refdef = inp.refdef.unwrap_or_else(|| Refdef::new(bg::client_view_origin(ps), ps.viewangles, 90.0, 73.739_79));
    let third_person = inp.third_person || ps.stat(Stat::Health) <= 0;
    let mut f = Frame { w, ps, me: inp.me, view: inp.view, time, frametime, aspect: inp.aspect, xs: inp.xs, ys: inp.ys, third_person, chase_follow: inp.chase_follow, loading: None, refdef, cvars: inp.cvars, real: inp.real, data, st, binds: &inp.binds, out: Vec::new() };
    // CG_DrawActive: clear around the rendered view if sized down
    f.tile_clear(inp.vrect, inp.screen);
    // first person blend blobs before CG_Draw2D
    if !third_person {
        f.pain_blend();
    }
    f.add_speed();
    f.update_entity_positions();
    f.lighting();
    if ps.pm_type == trem_sim::gen::enums::PmType::Intermission as i32 {
        // CG_DrawIntermission
        if let Some(m) = find("default_hud") {
            f.paint_menu(m);
        }
    } else {
        let alive_player = ps.pers(Pers::Specstate) == 0 && ps.stat(Stat::Health) > 0;
        let mut menu = None;
        if alive_player {
            menu = ClassId::from_num(ps.stat(Stat::Class).clamp(0, 255) as u8).and_then(|c| c.cold().hud).and_then(find);
            f.buildable_status();
        }
        if let Some(m) = menu.or_else(|| find("default_hud")) {
            f.paint_menu(m);
        }
        f.draw_vote(Team::None);
        f.draw_vote(Team::from_raw(f.stat(Stat::Team) as u8).unwrap_or(Team::None));
        f.draw_warmup();
        f.draw_queue();
        // (the scoreboard is TremMenus': it is up during the intermission)
        f.center_print_from_server();
        f.draw_center_string();
    }
    let out = std::mem::take(&mut f.out);
    st.compensated = Some((inp.aspect, menus));
    out
}

/// `CG_DrawLoadingScreen`: the "Loading" menu of ui/loading.menu with its
/// CG_LOAD_* ownerdraws (`Menu_Update` + `Menu_Paint`).
#[allow(clippy::too_many_arguments)]
fn build_loading_frame(data: &HudData, st: &mut HudState, w: &World, info: &crate::game::LoadInfo, aspect: f32, xs: f32, ys: f32, cvars: Cvars, real: RealTime) -> Vec<Cmd> {
    if st.compensated.as_ref().is_none_or(|(a, _)| (*a - aspect).abs() > 1e-6) {
        st.compensated = Some((aspect, data.menus.menus.iter().map(|m| m.aspect_compensated(aspect)).collect()));
    }
    let menus = st.compensated.take().map(|c| c.1).unwrap_or_default();
    let ps = w.clients[0].ps;
    let refdef = Refdef::new([0.0; 3], [0.0; 3], 90.0, 73.739_79);
    let mut f = Frame { w, ps: &ps, me: 0, view: 0, time: 0, frametime: 0, aspect, xs, ys, third_person: false, chase_follow: false, loading: Some(info), refdef, cvars, real, data, st, binds: &[], out: Vec::new() };
    if let Some(m) = menus.iter().find(|m| m.name.eq_ignore_ascii_case("Loading")) {
        f.paint_menu(m);
    }
    let out = std::mem::take(&mut f.out);
    st.compensated = Some((aspect, menus));
    out
}

/// `CG_LoadHudMenu` + `CG_RegisterGraphics` / `CG_RegisterWeapon` (HUD parts),
/// `CG_Asset_Parse` fonts and `CG_BuildableStatusParse` configs.
fn load_hud(lib: &mut Library) -> Option<HudData> {
    let list = lib.text("ui/hud.txt").map(|t| parse_menu_list(&t)).unwrap_or_default();
    let files: Vec<String> = if list.is_empty() {
        ["ui/tremulous_human_hud.menu", "ui/tremulous_alien_builder_hud.menu", "ui/tremulous_alien_general_hud.menu", "ui/tremulous_default_hud.menu"].iter().map(|s| s.to_string()).collect()
    } else {
        list
    };
    let mut menus = MenuSet::default();
    // teamscore.menu is drawn by TremMenus; loading.menu is CG_DrawLoadingScreen's
    for f in files.iter().filter(|f| f.contains("_hud") || f.to_ascii_lowercase().ends_with("loading.menu")) {
        let Some(text) = lib.text(f) else { continue };
        if let Err(e) = parse_menus_into(&mut menus, &text, &mut |p| lib.text(p)) {
            godot_warn!("TremHud: {f}: {e}");
        }
    }
    menus.menu("default_hud")?;
    let od = ownerdraw_table(&menus);
    // weapon.cfg icon / crosshair
    let mut scripts: Vec<String> = lib.pk3.files().filter(|f| f.starts_with("scripts/") && f.ends_with(".shader")).cloned().collect();
    scripts.sort();
    // later archives override: search the scripts last-to-first
    let script_texts: Vec<String> = scripts.iter().filter_map(|s| lib.text(s)).collect();
    let mut weapons = vec![WeaponHud::default(); WeaponId::COUNT + 1];
    for wid in WeaponId::ALL {
        let Some(cfg) = lib.text(&format!("models/weapons/{}/weapon.cfg", wid.id())) else { continue };
        let mut wh = WeaponHud::default();
        let mut depth = 0;
        for line in cfg.lines() {
            let line = line.split("//").next().unwrap_or("").trim();
            let t: Vec<&str> = line.split_whitespace().collect();
            if depth == 0 && t.len() >= 2 && t[0].eq_ignore_ascii_case("icon") {
                wh.icon = Some(t[1].to_string());
            }
            if depth == 0 && t.len() >= 3 && t[0].eq_ignore_ascii_case("crosshair") {
                let size = t[1].parse::<i32>().unwrap_or(0).max(0) as f32;
                let name = t[2];
                let stages = script_texts.iter().find_map(|s| shader_stages_2d(s, name)).unwrap_or_else(|| vec![Stage2D { image: name.to_string(), color: None, rotate: 0.0 }]);
                wh.crosshair = Some((size, stages));
            }
            depth += line.matches('{').count() as i32 - line.matches('}').count() as i32;
        }
        weapons[wid as usize] = wh;
    }
    let base_cross = "ui/assets/neutral/cross.tga".to_string();
    let mut cross = |n: &str| if lib.texture(n).is_some() { n.to_string() } else { base_cross.clone() };
    let cross = [
        base_cross.clone(),
        cross("ui/assets/neutral/cross2.tga"),
        cross("ui/assets/neutral/cross3.tga"),
        cross("ui/assets/neutral/cross_medkit.tga"),
        cross("ui/assets/neutral/cross_poison.tga"),
    ];
    // CG_Asset_Parse (loading.menu's assetGlobalDef): font 26, smallFont 20, bigFont 34
    let mut font = |size: i32| lib.pk3.read(&format!("fonts/fontImage_{size}.dat")).and_then(|d| parse_font_dat(d.as_slice()));
    let fonts = match (font(20), font(26), font(34)) {
        (Some(small), Some(text), Some(big)) => Some(HudFonts { small, text, big }),
        _ => {
            godot_warn!("TremHud: fonts/fontImage_{{20,26,34}}.dat missing: no HUD text");
            None
        }
    };
    let mut alien_build_stat = lib.text("ui/assets/alien/buildstat.cfg").map(|t| buildable_status_parse(&t)).unwrap_or_default();
    let mut human_build_stat = lib.text("ui/assets/human/buildstat.cfg").map(|t| buildable_status_parse(&t)).unwrap_or_default();
    // trap_R_RegisterShader returns 0 for an image that is not in the data;
    // CG_BuildableStatusDisplay skips a 0 frame / overlay shader (but draws
    // the noPower / marked ones unchecked: the default shader for 0)
    for bs in [&mut alien_build_stat, &mut human_build_stat] {
        for sh in [&mut bs.frame_shader, &mut bs.overlay_shader] {
            if sh.as_deref().is_some_and(|n| lib.texture(n).is_none() && !lib.shader_defined(n)) {
                *sh = None;
            }
        }
    }
    // CG_AssetCache: BG_LoadEmoticons (cg_emoticons defaults to 1)
    let mut files: Vec<String> = lib.pk3.files().cloned().collect();
    files.sort();
    let emoticons = load_emoticons(files.iter().map(|s| s.as_str()));
    Some(HudData { menus, od, weapons, cross, fonts, alien_build_stat, human_build_stat, emoticons })
}

// =====================================================================
// Godot node
// =====================================================================

/// The original Tremulous HUD (menu-script HUD of cgame), drawn over the
/// whole viewport in 640x480 virtual coordinates. Draws nothing when the
/// original data is not available (the GDScript HUD is then the fallback).
#[derive(GodotClass)]
#[class(base=Control)]
pub struct TremHud {
    base: Base<Control>,
    /// Path to the TremGame node.
    #[export]
    game_path: NodePath,
    /// Draw the human scanner without a helmet (screenshots/tests only;
    /// cgame requires UP_HELMET).
    #[export]
    scanner_without_helmet: bool,
    /// cg_tutorial (default 1): context help (CG_TUTORIAL).
    #[export]
    cg_tutorial: i32,
    /// cg_drawClock (default 0; 1 = 12-hour, 2 = 24-hour).
    #[export]
    cg_draw_clock: i32,
    /// cg_drawSpeed (default 0; 1 text, 2 graph, 4 ignore z).
    #[export]
    cg_draw_speed: i32,
    /// cg_drawTeamOverlay (default 1 = all, 2 = support, 3 = nearby).
    #[export]
    cg_draw_team_overlay: i32,
    /// cg_consoleLatency (default 3000 ms).
    #[export]
    cg_console_latency: i32,
    data: Option<HudData>,
    tried_load: bool,
    st: HudState,
    tex: HashMap<String, Option<(Gd<Texture2D>, bool)>>,
    ops: Vec<Op>,
    /// a World for the ownerdraw context while no level exists (loading)
    dummy_world: Option<Box<World>>,
}

#[godot_api]
impl IControl for TremHud {
    fn init(base: Base<Control>) -> Self {
        let cv = Cvars::default();
        TremHud {
            base,
            game_path: NodePath::from("../../Game"),
            scanner_without_helmet: false,
            cg_tutorial: cv.tutorial,
            cg_draw_clock: cv.draw_clock,
            cg_draw_speed: cv.draw_speed,
            cg_draw_team_overlay: cv.draw_team_overlay,
            cg_console_latency: cv.console_latency,
            data: None,
            tried_load: false,
            st: HudState::default(),
            tex: HashMap::new(),
            ops: Vec::new(),
            dummy_world: None,
        }
    }

    fn ready(&mut self) {
        let mut b = self.base_mut();
        b.set_anchors_preset(LayoutPreset::FULL_RECT);
        b.set_mouse_filter(MouseFilter::IGNORE);
    }

    fn process(&mut self, _delta: f64) {
        self.ops.clear();
        self.update();
        self.base_mut().queue_redraw();
    }

    fn draw(&mut self) {
        let size = self.base().get_size();
        let (xs, ys) = (size.x / 640.0, size.y / 480.0);
        let ops = std::mem::take(&mut self.ops);
        let col = |c: &Rgba| Color::from_rgba(c[0], c[1], c[2], c[3].clamp(0.0, 1.0));
        for op in &ops {
            match op {
                Op::Pic { tex, r, c, rot, uv } => {
                    let tex = &crate::hdtex::ui(tex);
                    let rect = Rect2::new(Vector2::new(r[0] * xs, r[1] * ys), Vector2::new(r[2] * xs, r[3] * ys));
                    let mut b = self.base_mut();
                    if *rot != 0.0 {
                        let centre = rect.position + rect.size / 2.0;
                        b.draw_set_transform_ex(centre).rotation(-rot.to_radians()).done();
                        b.draw_texture_rect_ex(tex, Rect2::new(-rect.size / 2.0, rect.size), false).modulate(col(c)).done();
                        b.draw_set_transform_matrix(Transform2D::IDENTITY);
                    } else if *uv == UV_FULL {
                        b.draw_texture_rect_ex(tex, rect, false).modulate(col(c)).done();
                    } else {
                        // trap_R_DrawStretchPic with s1 t1 s2 t2 (may be mirrored)
                        let (p, s) = (rect.position, rect.size);
                        let points = PackedVector2Array::from(&[p, Vector2::new(p.x + s.x, p.y), p + s, Vector2::new(p.x, p.y + s.y)][..]);
                        let colors = PackedColorArray::from(&[col(c); 4][..]);
                        let uvs = PackedVector2Array::from(&[Vector2::new(uv[0], uv[1]), Vector2::new(uv[2], uv[1]), Vector2::new(uv[2], uv[3]), Vector2::new(uv[0], uv[3])][..]);
                        b.draw_primitive_ex(&points, &colors, &uvs).texture(tex).done();
                    }
                }
                Op::Quad { tex, r, c, st } => {
                    let tex = &crate::hdtex::ui(tex);
                    let rect = Rect2::new(Vector2::new(r[0] * xs, r[1] * ys), Vector2::new(r[2] * xs, r[3] * ys));
                    let (p, s) = (rect.position, rect.size);
                    let points = PackedVector2Array::from(&[p, Vector2::new(p.x + s.x, p.y), p + s, Vector2::new(p.x, p.y + s.y)][..]);
                    let colors = PackedColorArray::from(&[col(c); 4][..]);
                    let uvs = PackedVector2Array::from(&st.map(|[u, v]| Vector2::new(u, v))[..]);
                    self.base_mut().draw_primitive_ex(&points, &colors, &uvs).texture(tex).done();
                }
                Op::Fill { r, c } => {
                    // negative sizes (e.g. a pic drawn upward) are normalised
                    let (mut x, mut y, mut w, mut h) = (r[0], r[1], r[2], r[3]);
                    if w < 0.0 {
                        x += w;
                        w = -w;
                    }
                    if h < 0.0 {
                        y += h;
                        h = -h;
                    }
                    let rect = Rect2::new(Vector2::new(x * xs, y * ys), Vector2::new(w * xs, h * ys));
                    self.base_mut().draw_rect(rect, col(c));
                }
            }
        }
        self.ops = ops;
    }
}

/// Image names to textures (`trap_R_RegisterShader*`): an image that is in
/// no shader script and no file is drawn with the renderer's default
/// shader, i.e. the `R_CreateDefaultImage` box, opaque and uncoloured
/// (`RE_RegisterShader` returns handle 0 = tr.defaultShader for it).
fn resolve_cmds(cmds: Vec<Cmd>, cache: &mut HashMap<String, Option<(Gd<Texture2D>, bool)>>, lib: &mut Library, ops: &mut Vec<Op>) {
    type Cache = HashMap<String, Option<(Gd<Texture2D>, bool)>>;
    fn get(cache: &mut Cache, img: &str, lib: &mut Library) -> Option<(Gd<Texture2D>, bool)> {
        cache
            .entry(img.to_lowercase())
            .or_insert_with(|| match lib.texture(img) {
                Some(t) => Some((t, false)),
                None if !lib.shader_defined(img) => Some((lib.default_texture(), true)),
                None => None,
            })
            .clone()
    }
    for c in cmds {
        match c {
            Cmd::Pic { img, r, c, rot, uv } => {
                if let Some((tex, default)) = get(cache, &img, lib).filter(|(_, d)| *d || c[3] > 0.0) {
                    ops.push(Op::Pic { tex, r, c: if default { WHITE } else { c }, rot, uv });
                }
            }
            Cmd::Fill { r, c } => ops.push(Op::Fill { r, c }),
            Cmd::Quad { img, r, c, st } => {
                if let Some((tex, default)) = get(cache, &img, lib).filter(|(_, d)| *d || c[3] > 0.0) {
                    ops.push(Op::Quad { tex, r, c: if default { WHITE } else { c }, st });
                }
            }
            Cmd::PicDetail { img, detail, r } => {
                let key = format!("{}|{}", img.to_lowercase(), detail.to_lowercase());
                let base = get(cache, &img, lib);
                let det = get(cache, &detail, lib);
                let t = cache
                    .entry(key)
                    .or_insert_with(|| match (base.clone(), det) {
                        (Some((b, _)), Some((d, _))) => detail_blend(&b, &d).map(|t| (t, false)).or(base),
                        _ => base,
                    })
                    .clone();
                if let Some((tex, _)) = t {
                    ops.push(Op::Pic { tex, r, c: WHITE, rot: 0.0, uv: UV_FULL });
                }
            }
        }
    }
}

impl TremHud {
    /// wt-b VoIP: `CG_CrosshairPlayer` (cl_voipSendTarget "crosshair").
    pub(crate) fn crosshair_player(&self) -> i32 {
        if self.st.last_time > self.st.crosshair_client_time + 1000 {
            return -1;
        }
        self.st.crosshair_client
    }

    fn cvars(&self) -> Cvars {
        Cvars {
            tutorial: self.cg_tutorial,
            draw_clock: self.cg_draw_clock,
            draw_speed: self.cg_draw_speed,
            draw_team_overlay: self.cg_draw_team_overlay,
            console_latency: self.cg_console_latency,
            ..Cvars::default()
        }
    }

    fn update(&mut self) {
        let Some(node) = self.base().get_node_or_null(&self.game_path) else { return };
        let Ok(mut game) = node.try_cast::<TremGame>() else { return };
        let size = self.base().get_size();
        if size.x <= 0.0 || size.y <= 0.0 {
            return;
        }
        let (xs, ys) = (size.x / 640.0, size.y / 480.0);
        // cgDC.aspectScale
        let aspect = (640.0 * size.y) / (480.0 * size.x);
        // load once the original data is there
        if self.data.is_none() {
            let mut g = game.bind_mut();
            let Some(lib) = g.hud_library() else { return };
            if !self.tried_load {
                self.tried_load = true;
                self.data = load_hud(lib);
                if self.data.is_none() {
                    godot_warn!("TremHud: no HUD menus in the game data; the original HUD is disabled");
                }
            }
            if self.data.is_none() {
                return;
            }
        }
        let time = godot::classes::Time::singleton();
        let clock = time.get_time_dict_from_system();
        let field = |k: &str| clock.get(k).and_then(|v| v.try_to::<i32>().ok()).unwrap_or(0);
        let real = RealTime { ms: time.get_ticks_msec(), hour: field("hour"), min: field("minute"), sec: field("second") };
        let cvars = self.cvars();
        // CG_DrawActiveFrame while cg.infoScreenText is set: CG_DrawLoadingScreen
        let loading = game.bind().hud_loading().cloned();
        if let Some(info) = loading {
            let mut g = game.bind_mut();
            let Some(lib) = g.hud_library() else { return };
            self.st.levelshot = lib.texture(&format!("levelshots/{}.tga", info.mapname)).is_some();
            let dummy = self.dummy_world.get_or_insert_with(|| Box::new(World::new(trem_sim::gen::maps::MapId::Arena)));
            let Some(data) = self.data.as_ref() else { return };
            let cmds = build_loading_frame(data, &mut self.st, dummy, &info, aspect, xs, ys, cvars, real);
            resolve_cmds(cmds, &mut self.tex, lib, &mut self.ops);
            return;
        }
        // phase 1: read the simulation
        let cmds = {
            let g = game.bind();
            let (Some(w), Some(me), Some(view)) = (g.hud_world(), g.hud_me(), g.hud_view_client()) else { return };
            let Some(data) = self.data.as_ref() else { return };
            self.st.force_scanner = self.scanner_without_helmet;
            (self.st.last_evolve_attempt, self.st.last_build_attempt) = g.hud_attempts();
            let (feed, total) = g.hud_feed();
            self.st.add_notify_lines(feed, total, w.level.time);
            let refdef = g.hud_camera().map(|(org, ang, fov_x, fov_y)| Refdef::new(org, ang, fov_x, fov_y));
            let third_person = g.hud_rendering_third_person();
            let mut cvars = cvars;
            cvars.viewsize = g.hud_viewsize();
            let inp = FrameInput { me, view, refdef, aspect, xs, ys, third_person, chase_follow: g.hud_chase_follow(), vrect: g.hud_vrect(), screen: (size.x, size.y), cvars, real, binds: g.hud_bindings(), weapon_select: g.hud_weapon_select() };
            build_frame(data, &mut self.st, w, &inp)
        };
        // phase 2: resolve images through the library
        let mut g = game.bind_mut();
        let Some(lib) = g.hud_library() else { return };
        resolve_cmds(cmds, &mut self.tex, lib, &mut self.ops);
    }
}

// =====================================================================
// tests
// =====================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tutorial_key_names_come_from_the_bind_table() {
        let mut keys = crate::keys::Keys::default();
        for (k, c) in [("MOUSE1", "+attack"), ("CTRL", "+attack"), ("b", "buy ammo"), ("x", "+button8")] {
            keys.set_binding(crate::keys::key_string_to_keynum(k), c);
        }
        let b = keys.bindings();
        assert_eq!(key_name_for_command(b, "+attack"), "CTRL or MOUSE1");
        assert_eq!(key_name_for_command(b, "buy ammo"), "B");
        assert_eq!(key_name_for_command(b, "+button8"), "X");
        assert_eq!(key_name_for_command(b, "+button6"), "\"Dodge\" (unbound)");
        assert_eq!(key_name_for_command(b, "nosuch"), "");
    }

    /// what the game loads: data-1.1.0.pk3 + the legacy assets overlay
    fn load(name: &str) -> MenuSet {
        let d = crate::testdata::legacy();
        let text = d.must_text(&format!("ui/{name}"));
        parse_menus(&text, &mut |p| d.text(p)).expect("parse")
    }

    fn item<'a>(m: &'a MenuDef, name: &str) -> &'a ItemDef {
        m.items.iter().find(|i| i.name == name).unwrap_or_else(|| panic!("item {name}"))
    }

    /// Every menu the legacy menu lists load parses (1.1.0 data + the legacy
    /// assets overlay, re/harness/pack_assets.py).
    #[test]
    fn legacy_menus_parse() {
        let d = crate::testdata::legacy();
        let mut bad = Vec::new();
        for list in ["ui/hud.txt", "ui/menus.txt", "ui/ingame.txt", "ui/tremulous.txt"] {
            let t = d.must_text(list);
            for f in parse_menu_list(&t) {
                let text = d.must_text(&f);
                match parse_menus(&text, &mut |p| d.text(p)) {
                    Ok(set) if !set.unknown.is_empty() => bad.push(format!("{f}: unknown {:?}", set.unknown)),
                    Ok(_) => {}
                    Err(e) => bad.push(format!("{f}: {e}")),
                }
            }
        }
        assert!(bad.is_empty(), "{bad:#?}");
    }

    #[test]
    fn hud_menudef_defines() {
        let set = load("tremulous_default_hud.menu");
        assert_eq!(set.define_int("WINDOW_STYLE_SHADER"), Some(3));
        assert_eq!(set.define_int("ALIGN_RIGHT"), Some(2));
        assert_eq!(set.define_int("VALIGN_TOP"), Some(2));
        assert_eq!(set.define_int("ASPECT_NONE"), Some(ASPECT_NONE));
        assert_eq!(set.define_int("ITEM_TEXTSTYLE_SHADOWED"), Some(ITEM_TEXTSTYLE_SHADOWED));
        assert_eq!(set.define_int("ITEM_TEXTSTYLE_NEON"), Some(ITEM_TEXTSTYLE_NEON));
        // legacy ownerdraws are an enum starting at 0
        assert_eq!(set.define_int("CG_PLAYER_HEALTH"), Some(0));
        assert_eq!(set.define_int("CG_PLAYER_HUMAN_SCANNER"), Some(23));
        assert_eq!(set.define_int("CG_PLAYER_CROSSHAIR"), Some(27));
        assert_eq!(set.define_int("CG_PLAYER_CREDITS_FRACTION"), Some(30));
        assert_eq!(set.define_int("CG_FOLLOW"), Some(33));
        assert_eq!(set.define_int("CG_FPS"), Some(45));
        assert_eq!(set.define_int("CG_CLOCK"), Some(61));
        let od = ownerdraw_table(&set);
        assert_eq!(od.get(&0), Some(&Od::Health));
        assert_eq!(od.get(&1), Some(&Od::HealthCross));
        assert_eq!(od.get(&26), Some(&Od::ChargeBar));
        assert_eq!(od.get(&29), Some(&Od::TeamOverlay));
        assert_eq!(od.get(&52), Some(&Od::Speedometer));
        assert_eq!(od.get(&59), Some(&Od::Console));
        assert_eq!(od.get(&60), Some(&Od::Tutorial));
        // every ownerdraw this module implements exists in menudef.h
        assert_eq!(od.len(), OWNERDRAWS.len());
        // and every ownerdraw the HUD menus use is implemented, but the
        // engine-only ones
        for f in ["tremulous_human_hud.menu", "tremulous_alien_general_hud.menu", "tremulous_alien_builder_hud.menu", "tremulous_default_hud.menu"] {
            let s = load(f);
            let skip: Vec<i32> = ["CG_LAGOMETER", "CG_SNAPSHOT", "CG_DEMO_PLAYBACK", "CG_DEMO_RECORDING"].iter().map(|n| s.define_int(n).unwrap()).collect();
            for it in s.menus.iter().flat_map(|m| &m.items).filter(|i| i.item_type == ITEM_TYPE_OWNERDRAW) {
                assert!(od.contains_key(&it.ownerdraw) || skip.contains(&it.ownerdraw), "{f}: {} ownerdraw {}", it.name, it.ownerdraw);
            }
        }
    }

    #[test]
    fn hud_human_menu() {
        let set = load("tremulous_human_hud.menu");
        assert!(set.unknown.is_empty(), "unknown keywords {:?}", set.unknown);
        assert_eq!(set.menus.len(), 1);
        let m = set.menu("human_hud").unwrap();
        assert_eq!(m.rect, [0.0, 0.0, 640.0, 480.0]);
        assert_eq!(m.aspect_bias, ASPECT_NONE);
        // 14 items of tremulous_common_hud.h + 27 of the human HUD
        assert_eq!(m.items.len(), 41);
        let la = item(m, "left-arm");
        assert_eq!(la.rect, [68.25, 420.0, 94.5, 35.0]);
        assert_eq!(la.style, WINDOW_STYLE_SHADER);
        assert_eq!(la.aspect_bias, ITEM_ALIGN_LEFT);
        assert!(la.visible && la.fore_set);
        assert_eq!(la.fore, [0.0, 0.8, 1.0, 0.25]);
        assert_eq!(la.background.as_deref(), Some("ui/assets/human/left-arm.tga"));
        let sc = item(m, "scanner");
        assert_eq!(sc.item_type, ITEM_TYPE_OWNERDRAW);
        assert_eq!(sc.ownerdraw, 23);
        assert_eq!(sc.rect, [164.0, 340.0, 312.0, 72.0]);
        assert_eq!(sc.aspect_bias, ITEM_ALIGN_CENTER);
        let bolt = item(m, "bolt");
        assert_eq!((bolt.back, bolt.fore), ([0.0, 0.8, 1.0, 0.2], [0.0, 0.8, 1.0, 0.8]));
        assert_eq!(item(m, "health").ownerdraw, 0);
        assert_eq!(item(m, "health").fore, [0.0, 0.8, 1.0, 0.5]);
        assert_eq!(item(m, "charge").rect, [510.0, 384.0, 56.0, 8.0]);
        assert_eq!(item(m, "chargebg").background.as_deref(), Some("ui/assets/neutral/charge_bg_h.tga"));
        assert_eq!(item(m, "teamoverlay").rect, [10.0, 175.0, 200.0, 128.0]);
        assert_eq!(item(m, "teamoverlay").text_scale, 0.85);
        // the common items (W 640, BORDER 10, STAT_W 45, STAT_H 22)
        let c = item(m, "console");
        assert_eq!(c.rect, [10.0, 10.0, 565.0, 180.0]);
        assert_eq!((c.text_align, c.text_valign, c.text_style), (ITEM_ALIGN_LEFT, VALIGN_TOP, ITEM_TEXTSTYLE_SHADOWED));
        assert_eq!(c.fore, [0.93, 0.93, 0.92, 1.0]);
        let t = item(m, "tutorial");
        assert_eq!(t.rect, [10.0, 275.0, 620.0, 130.0]);
        assert_eq!((t.text_valign, t.text_scale), (VALIGN_CENTER, 0.3));
        let fps = item(m, "fps");
        assert_eq!(fps.rect, [585.0, 10.0, 45.0, 22.0]);
        assert_eq!((fps.text_align, fps.text_valign, fps.aspect_bias), (ITEM_ALIGN_RIGHT, VALIGN_CENTER, ITEM_ALIGN_RIGHT));
        assert_eq!(item(m, "timer").rect, [585.0, 42.0, 45.0, 22.0]);
        assert_eq!(item(m, "location").rect, [510.0, 138.0, 120.0, 22.0]);
        assert_eq!(item(m, "speedometer").rect, [297.5, 350.0, 45.0, 22.0]);
        let ch = item(m, "crosshair");
        assert_eq!((ch.rect, ch.fore), ([320.0, 240.0, 0.0, 0.0], [0.0, 0.8, 1.0, 0.35]));
        assert_eq!(item(m, "followtext").text_scale, 0.7);
    }

    #[test]
    fn hud_alien_menus() {
        let g = load("tremulous_alien_general_hud.menu");
        assert!(g.unknown.is_empty(), "unknown keywords {:?}", g.unknown);
        let m = g.menu("alien_general_hud").unwrap();
        assert_eq!(m.items.len(), 34);
        assert_eq!(item(m, "blob").ownerdraw, 21);
        assert_eq!(item(m, "blob").rect, [497.0, 419.0, 52.0, 18.0]);
        assert_eq!(item(m, "left-spikes").ownerdraw, 19);
        assert_eq!(item(m, "left-spikes").fore, [1.0, 0.0, 0.0, 1.0]);
        assert_eq!(item(m, "organs").ownerdraw, 6);
        let cf = item(m, "credits-background");
        assert_eq!((cf.ownerdraw, cf.rect), (30, [567.0, 410.0, 25.0, 25.0]));
        assert_eq!(item(m, "aliensense").rect, [20.0, 20.0, 600.0, 400.0]);
        // static circle: shader style, no ownerdraw
        let c = item(m, "left-arm-circle");
        assert_eq!((c.style, c.item_type), (WINDOW_STYLE_SHADER, 0));
        // the crosshair item takes the HUD colour
        assert_eq!(item(m, "crosshair").fore, [1.0, 0.0, 0.0, 0.35]);

        let b = load("tremulous_alien_builder_hud.menu");
        let m = b.menu("alien_builder_hud").unwrap();
        assert_eq!(m.items.len(), 35);
        assert!(m.items.iter().all(|i| i.name != "blob"));
        assert_eq!(item(m, "buildtimer").ownerdraw, 4);
        assert_eq!(item(m, "build-points").ownerdraw, 2);
        assert_eq!(item(m, "build-points").rect, [493.5, 421.5, 60.0, 15.0]);
    }

    #[test]
    fn hud_default_menu_and_list() {
        let d = load("tremulous_default_hud.menu");
        let m = d.menu("default_hud").unwrap();
        assert_eq!(m.items.len(), 15);
        assert_eq!(item(m, "playername").ownerdraw, 53);
        let st = item(m, "spectatortext");
        // no `type` keyword: ITEM_TYPE_NONE, which the legacy Item_Paint
        // does not paint text for
        assert_eq!((st.item_type, st.text.as_deref(), st.text_align, st.text_valign), (0, Some("SPECTATOR"), ITEM_ALIGN_CENTER, VALIGN_TOP));
        assert_eq!(st.rect, [200.0, 415.0, 240.0, 25.0]);
        assert_eq!(item(m, "fps").fore, [1.0, 1.0, 1.0, 1.0]);
        let list = parse_menu_list(&crate::testdata::legacy().must_text("ui/hud.txt"));
        assert_eq!(list, ["ui/tremulous_human_hud.menu", "ui/tremulous_alien_builder_hud.menu", "ui/tremulous_alien_general_hud.menu", "ui/tremulous_default_hud.menu", "ui/teamscore.menu", "ui/loading.menu"]);
        // the four HUDs into one set: the class hudName menus are all found
        let mut all = MenuSet::default();
        for f in &list[..4] {
            let t = crate::testdata::legacy().must_text(&f);
            parse_menus_into(&mut all, &t, &mut |p| crate::testdata::legacy().text(p)).unwrap();
        }
        for c in ClassId::ALL {
            if let Some(h) = c.cold().hud {
                assert!(all.menu(h).is_some(), "{h}");
            }
        }
    }

    /// Menu_AspectCompensate at 16:9 (aspectScale 0.75).
    #[test]
    fn hud_aspect_compensate() {
        let set = load("tremulous_human_hud.menu");
        let m = set.menu("human_hud").unwrap().aspect_compensated(0.75);
        // ASPECT_NONE menu: each item by its own bias
        assert_eq!(m.rect, [0.0, 0.0, 640.0, 480.0]);
        assert_eq!(item(&m, "left-arm").rect, [68.25 * 0.75, 420.0, 94.5 * 0.75, 35.0]);
        assert_eq!(item(&m, "right-arm").rect, [640.0 - (640.0 - 477.25) * 0.75, 420.0, 94.5 * 0.75, 35.0]);
        assert_eq!(item(&m, "scanner").rect, [164.0 * 0.75 + 80.0, 340.0, 312.0 * 0.75, 72.0]);
        // no aspectBias: untouched
        assert_eq!(item(&m, "teamoverlay").rect, [10.0, 175.0, 200.0, 128.0]);
        // a biased menu (teamscore_menu, ALIGN_CENTER by Menu_Init): the
        // menu rect is aspectised, item client rects scaled
        let ts = load("teamscore.menu");
        let t = ts.menu("teamscore_menu").unwrap();
        assert_eq!(t.rect, [70.0, 71.0, 500.0, 338.0]);
        let c = t.aspect_compensated(0.75);
        assert_eq!(c.rect, [70.0 * 0.75 + 80.0, 71.0, 375.0, 338.0]);
        let h = item(&c, "humanteamname");
        assert!((h.rect[0] - 1000.0 / 3.0 * 0.75).abs() < 1e-3 && (h.rect[2] - 500.0 / 3.0 * 0.75).abs() < 1e-3, "{:?}", h.rect);
        assert_eq!(h.text_align_x, -7.5);
        assert_eq!(t.aspect_compensated(1.0).rect, t.rect);
    }

    fn fonts() -> Option<HudFonts> {
        let f = |n: i32| parse_font_dat(&crate::testdata::legacy().must_read(&format!("fonts/fontImage_{n}.dat")));
        Some(HudFonts { small: f(20)?, text: f(26)?, big: f(34)? })
    }

    /// UI_Text_Width / UI_Text_Height with the legacy font choice and the
    /// original's one-glyph-ahead width quirk; Item_Text_Wrap.
    #[test]
    fn hud_text_metrics_and_wrap() {
        let Some(fonts) = fonts() else { return };
        let m = TextMetrics { fonts: &fonts, aspect: 1.0, emoticons: &[] };
        // UI_FontForScale: <= 0.2 small, >= 0.5 big
        assert!(std::ptr::eq(fonts.pick(0.2), &fonts.small));
        assert!(std::ptr::eq(fonts.pick(0.35), &fonts.text));
        assert!(std::ptr::eq(fonts.pick(0.5), &fonts.big));
        let f = &fonts.text;
        let us = 0.3 * f.glyph_scale;
        let xs = |c: u8| f.glyphs[c as usize].xskip as f32 * us;
        // "AB" measures B and the terminating NUL
        assert!((m.width("AB", 0.3) - (xs(b'B') + xs(0))).abs() < 1e-4);
        assert_eq!(m.width("^1AB", 0.3), m.width("AB", 0.3));
        assert!((TextMetrics { fonts: &fonts, aspect: 0.75, emoticons: &[] }.width("AB", 0.3) - 0.75 * m.width("AB", 0.3)).abs() < 1e-4);
        // emoticons: "[smile]" is two glyph heights wide, "[[smile]" is escaped text
        let emo = load_emoticons(["emoticons/smile_2x1.tga"].into_iter());
        let me = TextMetrics { fonts: &fonts, aspect: 1.0, emoticons: &emo };
        let h = fonts.text.glyphs[b'[' as usize].height as f32 * 0.3 * fonts.text.glyph_scale;
        assert!((me.width("[smile]", 0.3) - 2.0 * h).abs() < 1e-4);
        assert!((me.width("[[smile]", 0.3) - m.width("[smile]", 0.3)).abs() < 1e-4);
        assert!((me.width("[frown]", 0.3) - m.width("[frown]", 0.3)).abs() < 1e-4);
        assert_eq!(m.height("A^7", 0.3), f.glyphs[b'A' as usize].height as f32 * us);
        // forced breaks are kept, every line ends with a newline
        assert_eq!(m.wrap(b"one\ntwo", 0.3, 1000.0), b"one\ntwo\n");
        // long lines break at the last blank that fits; the colour carries over
        let w = m.width("alpha beta", 0.3) + 1.0;
        let out = String::from_utf8(m.wrap(b"^3alpha beta gamma delta", 0.3, w)).unwrap();
        let lines: Vec<&str> = out.split_terminator('\n').collect();
        assert!(lines.len() >= 2, "{out:?}");
        assert!(lines[0].starts_with("^3alpha"), "{out:?}");
        assert!(lines[1].starts_with("^3"), "{out:?}");
        assert_eq!(out.replace("^3", "").split_whitespace().collect::<Vec<_>>(), ["alpha", "beta", "gamma", "delta"]);
        for l in &lines {
            assert!(m.width(l, 0.3) <= w + m.width("M", 0.3), "{l:?} too wide in {out:?}");
        }
        // a word longer than the width is split mid-word without stalling
        let s = String::from_utf8(m.wrap(b"abcdefghijklmnop", 0.3, m.width("abcd", 0.3))).unwrap();
        assert!(s.lines().count() > 2 && s.replace('\n', "") == "abcdefghijklmnop", "{s:?}");
    }

    /// CG_BuildableStatusParse on the legacy buildstat.cfg files.
    #[test]
    fn loading_menu_ownerdraws() {
        let set = load("loading.menu");
        let od = ownerdraw_table(&set);
        let m = set.menu("Loading").expect("Loading menu");
        let kinds: Vec<Od> = m.items.iter().filter(|i| i.item_type == ITEM_TYPE_OWNERDRAW).filter_map(|i| od.get(&i.ownerdraw).copied()).collect();
        for k in [Od::LoadLevelshot, Od::LoadMedia, Od::LoadMediaLabel, Od::LoadBuildables, Od::LoadCharModel, Od::LoadOverall, Od::LoadLevelname, Od::LoadMotd, Od::LoadHostname] {
            assert!(kinds.contains(&k), "{k:?}");
        }
        let shot = m.items.iter().find(|i| i.name == "levelshot").unwrap();
        assert_eq!(shot.rect, [310.0, 10.0, 320.0, 240.0]);
    }

    #[test]
    fn loading_strings() {
        assert_eq!(parse_newlines("a\\nb\\xc"), "a\nbxc");
        assert_eq!(clean_str("^1Red^7 server\t!"), "Red server!");
        let emo = load_emoticons(["emoticons/smile_1x1.tga"].into_iter());
        assert_eq!(escape_emoticons(&emo, "hi [smile] [x]"), "hi [[smile] [x]");
    }

    #[test]
    fn emoticon_files_like_bg_loademoticons() {
        let files = ["emoticons/smile_1x1.tga", "emoticons/wide_3x1.tga", "emoticons/bad.tga", "emoticons/zero_0x1.tga", "emoticons/sub/x_1x1.tga", "emoticons/abcdefghijklmnopq_1x1.tga", "gfx/a_1x1.tga"];
        let e = load_emoticons(files.into_iter());
        assert_eq!(e.iter().map(|e| (e.name.as_str(), e.width)).collect::<Vec<_>>(), vec![("smile", 1), ("wide", 3)]);
        assert_eq!(e[1].shader, "emoticons/wide_3x1.tga");
        assert_eq!(is_emoticon(&e, b"a [Smile] b", 2), Some((false, 7, 0)));
        assert_eq!(is_emoticon(&e, b"[[wide]", 0), Some((true, 7, 1)));
        assert_eq!(is_emoticon(&e, b"[nope]", 0), None);
        assert_eq!(is_emoticon(&e, b"[smile", 0), None);
    }

    #[test]
    fn hud_buildstat_parse() {
        let h = crate::testdata::legacy().must_text("ui/assets/human/buildstat.cfg");
        let bs = buildable_status_parse(&h);
        assert!(bs.loaded);
        assert_eq!(bs.frame_shader.as_deref(), Some("ui/assets/human/buildstat/frame"));
        assert_eq!(bs.overlay_shader, None, "overlayShader \"\" registers no shader");
        assert_eq!(bs.marked_shader.as_deref(), Some("ui/assets/human/buildstat/mark"));
        assert_eq!(bs.no_power_shader.as_deref(), Some("ui/assets/human/buildstat/nopower"));
        assert_eq!((bs.frame_width, bs.frame_height, bs.health_padding, bs.overlay_width, bs.overlay_height), (150, 30, 2, 160, 40));
        assert_eq!(bs.health_severe_color, [0.83, 0.03, 0.02, 1.0]);
        assert_eq!(bs.health_low_color, [0.27, 0.49, 0.55, 1.0]);
        assert_eq!((bs.vertical_margin, bs.horizontal_margin), (0.5, 1.0));
        assert_eq!((bs.fore_color, bs.back_color), ([0.0, 0.0, 0.0, 1.0], [1.0, 1.0, 1.0, 1.0]));
        let a = buildable_status_parse(&crate::testdata::legacy().must_text("ui/assets/alien/buildstat.cfg"));
        assert!(a.loaded);
        assert_eq!(a.overlay_shader.as_deref(), Some("ui/assets/alien/buildstat/overlay"));
        assert_eq!((a.overlay_width, a.overlay_height), (156, 36));
        assert_eq!(a.health_guarded_color, [0.48, 0.08, 0.08, 1.0]);
        // unknown tokens make the whole config unloaded
        assert!(!buildable_status_parse("frameWidth 10\nbogus 1").loaded);
        assert_eq!(buildable_status_parse("verticalMargin -0.5").vertical_margin, -0.5);
    }

    #[test]
    fn hud_charge_progress() {
        assert_eq!(charge_progress(WeaponId::Rifle as i32, 0, 500), 0.0);
        assert_eq!(charge_progress(WeaponId::Level3 as i32, 0, 500), 0.5);
        assert_eq!(charge_progress(WeaponId::Level3 as i32, 0, 100), 0.0);
        assert_eq!(charge_progress(WeaponId::Lcannon as i32, 0, 5000), 1.0);
        assert_eq!(charge_progress(WeaponId::Level4 as i32, k::SS_CHARGING, 1500), 0.5);
        assert!((charge_progress(WeaponId::Level4 as i32, 0, 1000) - 1.0).abs() < 1e-6);
    }

    /// CG_AddNotifyText from the TremGame feed, CG_RemoveNotifyLine after
    /// cg_consoleLatency.
    #[test]
    fn hud_notify_lines() {
        let mut st = HudState::default();
        let mut feed = vec![(1.0, "a".to_string()), (1.0, "b".to_string())];
        st.add_notify_lines(&feed, 2, 100);
        st.add_notify_lines(&feed, 2, 150);
        // two more lines while the first one drops off the front
        feed.push((1.0, "c".into()));
        feed.push((2.0, "d".into()));
        feed.remove(0);
        st.add_notify_lines(&feed, 4, 200);
        let lines: Vec<&str> = st.console.iter().map(|l| l.1.as_str()).collect();
        assert_eq!(lines, ["a\n", "b\n", "c\n", "d\n"]);
        assert_eq!(st.console[2].0, 200);
        st.expire_notify_lines(3100, 3000);
        assert_eq!(st.console.len(), 4, "not older than the latency yet");
        st.expire_notify_lines(3101, 3000);
        assert_eq!(st.console.len(), 3, "one line per frame");
        for i in 0..40 {
            st.add_notify_lines(&[(10.0 + i as f64, format!("{i}"))], 5 + i, 4000);
        }
        assert_eq!(st.console.len(), MAX_CONSOLE_LINES);
        assert_eq!(st.console.last().unwrap().1, "39\n");
    }

    #[test]
    fn hud_parser_basics() {
        let src = "#define FOO 0x10\n#define BAR 2\n// c\n{ menuDef { name \"m\" rect 1 2 3 4 /* x */ onOpen { play \"x\" } itemDef { name i textScale .5 ownerdraw FOO style BAR visible 1 weird a b { c } forecolor 1 0 0 -1 } } }";
        let s = parse_menus(src, &mut |_| None).unwrap();
        let m = s.menu("m").unwrap();
        assert_eq!(m.rect, [1.0, 2.0, 3.0, 4.0]);
        let i = &m.items[0];
        assert_eq!((i.ownerdraw, i.style, i.item_type, i.text_scale), (16, 2, ITEM_TYPE_OWNERDRAW, 0.5));
        assert_eq!(i.fore, [1.0, 0.0, 0.0, -1.0]);
        assert_eq!(s.unknown, vec!["weird".to_string()]);
        assert_eq!(m.on_open, vec![vec!["play".to_string(), "x".to_string()]]);
        assert!(parse_menus("#include \"nope.h\"", &mut |_| None).is_err());
    }

    /// PC_Expression_Parse: legacy menus compute rects from #defines.
    #[test]
    fn menu_expressions() {
        let src = "#define W 640\n#define BORDER 10\n#define STAT_W (W/8)\n#define X (W-((3*BORDER)+STAT_W))\n\
                   { menuDef { name m rect 0 0 W 480 itemDef { name a rect X ( 320 - ( W / 2 ) ) -5 (8-2+1) } } }";
        let set = parse_menus(src, &mut |_| None).unwrap();
        let r = set.menus[0].items[0].rect;
        assert_eq!(r, [530.0, 0.0, -5.0, 5.0], "the original pops only on strictly lower precedence: 8-(2+1)");
        assert_eq!(set.menus[0].rect[2], 640.0);
    }

    #[test]
    fn hud_crosshair_shader() {
        let t = crate::testdata::legacy().must_text("scripts/crosshairs.shader");
        let r = shader_stages_2d(&t, "gfx/2d/crosshair-rifle_s").unwrap();
        // legacy crosshairs are `rgbGen vertex`: CG_DrawCrosshair colours them
        assert_eq!(r, vec![Stage2D { image: "gfx/2d/crosshair-rifle.tga".into(), color: None, rotate: 0.0 }]);
        let f = shader_stages_2d(&t, "gfx/2d/crosshair-flamer_s").unwrap();
        assert_eq!(f.len(), 2);
        assert_eq!((f[0].rotate, f[1].rotate), (5.0, -5.0));
        assert!(shader_stages_2d(&t, "nope").is_none());
    }

}
