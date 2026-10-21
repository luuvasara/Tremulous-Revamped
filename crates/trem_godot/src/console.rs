//! The client console: cl_console.c (`CL_ConsolePrint`, `Con_Linefeed`,
//! `Con_CheckResize`, `Con_Clear_f`, `Con_Dump_f`, `Con_PageUp` /
//! `Con_PageDown` / `Con_Top` / `Con_Bottom`, `Con_RunConsole`,
//! `Con_DrawSolidConsole`, `Con_DrawInput`), cl_keys.c's edit field and
//! history (`Field_KeyDownEvent`, `Field_CharEvent`, `Field_Paste`,
//! `Field_VariableSizeDraw`, `Console_Key`, `CL_LoadConsoleHistory`,
//! `CL_SaveConsoleHistory`: user://q3history) and common.c's tab
//! completion (`Field_AutoComplete`, `Field_CompleteCommand`,
//! `Field_Complete`, `FindMatches`, `PrintMatches`, `PrintCvarMatches`,
//! `Field_CompleteKeyname`, `Field_CompleteFilename`) with the commands'
//! completion functions (cmd.c `Cmd_CompleteArgument`,
//! `Cmd_CompleteCfgName`, cvar.c `Cvar_CompleteCvarName`, cl_keys.c
//! `Key_CompleteBind` / `Key_CompleteUnbind`, cl_main.c `CL_CompleteRcon`,
//! cl_console.c `Cmd_CompleteTxtName`).
//!
//! The logic is pure Rust (`Console`, unit tested); `TremConsole` is the
//! Godot node that draws it like the original (the `console` shader over
//! the top half, the red line, the red version string, 8x16 characters
//! from gfx/2d/bigchars) and feeds it the keys while it is down.

use crate::keys::*;
use godot::classes::{Control, IControl, InputEvent, InputEventKey};
use godot::prelude::*;

/// cl_console.c CON_TEXTSIZE
pub const CON_TEXTSIZE: usize = 163840;
/// keys.h COMMAND_HISTORY
pub const COMMAND_HISTORY: usize = 32;
/// qcommon.h MAX_EDIT_LINE
pub const MAX_EDIT_LINE: usize = 256;
/// cl_console.c g_console_field_width
pub const CONSOLE_FIELD_WIDTH: usize = 78;
/// cl_keys.c CONSOLE_HISTORY_FILE
pub const CONSOLE_HISTORY_FILE: &str = "q3history";
/// cl_keys.c MAX_CONSOLE_SAVE_BUFFER
const MAX_CONSOLE_SAVE_BUFFER: usize = 1024;
/// q_shared.h SMALLCHAR_WIDTH / SMALLCHAR_HEIGHT
pub const SMALLCHAR_WIDTH: i32 = 8;
pub const SMALLCHAR_HEIGHT: i32 = 16;
/// q_shared.h SCREEN_WIDTH / SCREEN_HEIGHT
const SCREEN_WIDTH: i32 = 640;
const SCREEN_HEIGHT: i32 = 480;
/// q_shared.h Q3_VERSION (PRODUCT_NAME " " PRODUCT_VERSION)
pub const Q3_VERSION: &str = "tremulous 1.1.0";
/// q_shared.h INDENT_MARKER
const INDENT_MARKER: u8 = 0x0b;
/// q_shared.h COLOR_WHITE's index
const COLOR_WHITE: u16 = 7;
/// q_shared.h COLOR_RED's index
const COLOR_RED: usize = 1;

/// q_math.c `g_color_table`.
pub const COLOR_TABLE: [[f32; 4]; 8] = [[0.2, 0.2, 0.2, 1.0], [1.0, 0.0, 0.0, 1.0], [0.0, 1.0, 0.0, 1.0], [1.0, 1.0, 0.0, 1.0], [0.0, 0.0, 1.0, 1.0], [0.0, 1.0, 1.0, 1.0], [1.0, 0.0, 1.0, 1.0], [1.0, 1.0, 1.0, 1.0]];

/// `Q_IsColorString` at `b[i]`.
fn is_color_string(b: &[u8], i: usize) -> bool {
    b.get(i) == Some(&b'^') && b.get(i + 1).is_some_and(|c| c.is_ascii_alphanumeric())
}

/// `ColorIndex( c )`.
fn color_index(c: u8) -> u16 {
    (c.wrapping_sub(b'0') & 7) as u16
}

/// Latin-1 bytes of a Rust string (the console works on chars 0..255).
fn bytes_of(s: &str) -> Vec<u8> {
    s.chars().map(|c| if (c as u32) < 256 { c as u32 as u8 } else { b'?' }).collect()
}

fn string_of(b: &[u8]) -> String {
    b.iter().map(|&c| c as char).collect()
}

// =====================================================================
// console_t
// =====================================================================

/// cl_console.c `console_t`: the scrollback ring of colour-tagged chars.
#[derive(Clone, Debug)]
pub struct ConText {
    text: Vec<u16>,
    /// line where next message will be printed
    pub current: i32,
    /// offset in current line for next print
    pub x: i32,
    /// bottom of console displays this line
    pub display: i32,
    /// characters across screen
    pub linewidth: i32,
    /// total lines in console scrollback
    pub totallines: i32,
    /// aproaches finalFrac at scr_conspeed
    pub display_frac: f32,
    /// 0.0 to 1.0 lines of console to display
    pub final_frac: f32,
    /// in scanlines
    pub vislines: i32,
}

impl Default for ConText {
    fn default() -> Self {
        let mut c = ConText { text: vec![(COLOR_WHITE << 8) | b' ' as u16; CON_TEXTSIZE], current: 0, x: 0, display: 0, linewidth: -1, totallines: 0, display_frac: 0.0, final_frac: 0.0, vislines: 0 };
        c.check_resize();
        c
    }
}

impl ConText {
    /// `Con_CheckResize`: the width is SCREEN_WIDTH / SMALLCHAR_WIDTH - 2
    /// (the console text keeps to the 640 virtual columns).
    pub fn check_resize(&mut self) {
        let width = SCREEN_WIDTH / SMALLCHAR_WIDTH - 2;
        if width == self.linewidth {
            return;
        }
        let (oldwidth, oldtotallines) = (self.linewidth, self.totallines);
        self.linewidth = width;
        self.totallines = CON_TEXTSIZE as i32 / width;
        let blank = (COLOR_WHITE << 8) | b' ' as u16;
        if oldwidth < 1 {
            self.text.iter_mut().for_each(|c| *c = blank);
        } else {
            let numlines = oldtotallines.min(self.totallines);
            let numchars = oldwidth.min(self.linewidth);
            let tbuf = std::mem::replace(&mut self.text, vec![blank; CON_TEXTSIZE]);
            for i in 0..numlines {
                for j in 0..numchars {
                    self.text[((self.totallines - 1 - i) * self.linewidth + j) as usize] = tbuf[(((self.current - i + oldtotallines) % oldtotallines) * oldwidth + j) as usize];
                }
            }
        }
        self.current = self.totallines - 1;
        self.display = self.current;
    }

    fn row(&self, row: i32) -> &[u16] {
        let r = row.rem_euclid(self.totallines) as usize * self.linewidth as usize;
        &self.text[r..r + self.linewidth as usize]
    }

    /// `Con_Clear_f`.
    pub fn clear(&mut self) {
        self.text.iter_mut().for_each(|c| *c = (COLOR_WHITE << 8) | b' ' as u16);
        self.bottom();
    }

    /// `Con_Linefeed`.
    fn linefeed(&mut self) {
        self.x = 0;
        if self.display == self.current {
            self.display += 1;
        }
        self.current += 1;
        let r = (self.current % self.totallines) as usize * self.linewidth as usize;
        let w = self.linewidth as usize;
        self.text[r..r + w].iter_mut().for_each(|c| *c = (COLOR_WHITE << 8) | b' ' as u16);
    }

    /// `CL_ConsolePrint`: colour escapes, word wrap, \n and \r; a leading
    /// "[skipnotify]" is dropped (it only keeps the line out of the notify
    /// area, which the HUD's CG_CONSOLE draws from its own feed).
    pub fn print(&mut self, txt: &str) {
        let b = bytes_of(txt);
        let mut i = if b.starts_with(b"[skipnotify]") { 12 } else { 0 };
        let mut color = COLOR_WHITE;
        while i < b.len() {
            let c = b[i];
            if c == 0 {
                break;
            }
            if is_color_string(&b, i) {
                color = color_index(b[i + 1]);
                i += 2;
                continue;
            }
            // count word length
            let mut l = 0;
            while l < self.linewidth {
                if b.get(i + l as usize).is_none_or(|&ch| ch <= b' ') {
                    break;
                }
                l += 1;
            }
            // word wrap
            if l != self.linewidth && self.x + l >= self.linewidth {
                self.linefeed();
            }
            i += 1;
            match c {
                INDENT_MARKER => {}
                b'\n' => self.linefeed(),
                b'\r' => self.x = 0,
                _ => {
                    // display character and advance
                    let y = (self.current % self.totallines) as usize;
                    self.text[y * self.linewidth as usize + self.x as usize] = (color << 8) | c as u16;
                    self.x += 1;
                    if self.x >= self.linewidth {
                        self.linefeed();
                    }
                }
            }
        }
    }

    /// `Con_PageUp`.
    pub fn page_up(&mut self) {
        self.display -= 2;
        if self.current - self.display >= self.totallines {
            self.display = self.current - self.totallines + 1;
        }
    }

    /// `Con_PageDown`.
    pub fn page_down(&mut self) {
        self.display += 2;
        if self.display > self.current {
            self.display = self.current;
        }
    }

    /// `Con_Top`.
    pub fn top(&mut self) {
        self.display = self.totallines;
        if self.current - self.display >= self.totallines {
            self.display = self.current - self.totallines + 1;
        }
    }

    /// `Con_Bottom`.
    pub fn bottom(&mut self) {
        self.display = self.current;
    }

    /// The text of line `row` without colours or trailing blanks.
    pub fn line_text(&self, row: i32) -> String {
        let s: Vec<u8> = self.row(row).iter().map(|c| (c & 0xff) as u8).collect();
        string_of(&s).trim_end_matches(' ').to_string()
    }

    /// `Con_Dump_f`'s text: the lines from the first non-empty one.
    pub fn dump(&self) -> String {
        let mut l = self.current - self.totallines + 1;
        while l <= self.current {
            if self.row(l).iter().any(|c| c & 0xff != b' ' as u16) {
                break;
            }
            l += 1;
        }
        let mut out = String::new();
        while l <= self.current {
            out.push_str(&self.line_text(l));
            out.push('\n');
            l += 1;
        }
        out
    }

    /// `Con_RunConsole`: scroll towards half a screen (console down) or
    /// nothing at scr_conspeed per second.
    pub fn run(&mut self, down: bool, conspeed: f32, real_frametime_ms: f32) {
        self.final_frac = if down { 0.5 } else { 0.0 };
        if self.final_frac < self.display_frac {
            self.display_frac -= conspeed * real_frametime_ms * 0.001;
            if self.final_frac > self.display_frac {
                self.display_frac = self.final_frac;
            }
        } else if self.final_frac > self.display_frac {
            self.display_frac += conspeed * real_frametime_ms * 0.001;
            if self.final_frac < self.display_frac {
                self.display_frac = self.final_frac;
            }
        }
    }
}

// =====================================================================
// field_t
// =====================================================================

/// qcommon.h `field_t`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Field {
    pub cursor: usize,
    pub scroll: usize,
    pub width_in_chars: usize,
    pub buffer: Vec<u8>,
}

impl Field {
    pub fn new() -> Self {
        Field { width_in_chars: CONSOLE_FIELD_WIDTH, ..Default::default() }
    }

    pub fn text(&self) -> String {
        string_of(&self.buffer)
    }

    pub fn set_text(&mut self, s: &str) {
        self.buffer = bytes_of(s);
        self.buffer.truncate(MAX_EDIT_LINE - 1);
        self.cursor = self.buffer.len();
        self.scroll = 0;
    }

    /// `Field_Clear`.
    pub fn clear(&mut self) {
        self.buffer.clear();
        self.cursor = 0;
        self.scroll = 0;
    }

    /// `Field_Paste`: sent as if typed, so insert / overstrike works.
    pub fn paste(&mut self, clipboard: &str, overstrike: bool) {
        for c in bytes_of(clipboard) {
            self.char_event(c as i32, overstrike, "");
        }
    }

    /// `Field_KeyDownEvent`: the non-printable editing keys.
    pub fn key_down_event(&mut self, key: i32, shift_down: bool, overstrike: &mut bool, clipboard: &str) {
        // shift-insert is paste
        if (key == K_INS || key == K_KP_INS) && shift_down {
            self.paste(clipboard, *overstrike);
            return;
        }
        let key = if (0..256).contains(&key) { (key as u8).to_ascii_lowercase() as i32 } else { key };
        let len = self.buffer.len();
        match key {
            K_DEL => {
                if self.cursor < len {
                    self.buffer.remove(self.cursor);
                }
            }
            K_RIGHTARROW => {
                if self.cursor < len {
                    self.cursor += 1;
                }
            }
            K_LEFTARROW => {
                if self.cursor > 0 {
                    self.cursor -= 1;
                }
            }
            K_HOME => self.cursor = 0,
            K_END => self.cursor = len,
            K_INS => *overstrike = !*overstrike,
            _ => {}
        }
        // Change scroll if cursor is no longer visible
        let len = self.buffer.len();
        if self.cursor < self.scroll {
            self.scroll = self.cursor;
        } else if self.cursor >= self.scroll + self.width_in_chars && self.cursor <= len {
            self.scroll = self.cursor + 1 - self.width_in_chars;
        }
    }

    /// `Field_CharEvent`.
    pub fn char_event(&mut self, ch: i32, overstrike: bool, clipboard: &str) {
        let ctrl = |c: u8| (c - b'a' + 1) as i32;
        if ch == ctrl(b'v') {
            // ctrl-v is paste
            self.paste(clipboard, overstrike);
            return;
        }
        if ch == ctrl(b'c') {
            // ctrl-c clears the field
            self.clear();
            return;
        }
        let len = self.buffer.len();
        if ch == ctrl(b'h') {
            // ctrl-h is backspace
            if self.cursor > 0 {
                self.buffer.remove(self.cursor - 1);
                self.cursor -= 1;
                if self.cursor < self.scroll {
                    self.scroll -= 1;
                }
            }
            return;
        }
        if ch == ctrl(b'a') {
            // ctrl-a is home
            self.cursor = 0;
            self.scroll = 0;
            return;
        }
        if ch == ctrl(b'e') {
            // ctrl-e is end
            self.cursor = len;
            self.scroll = self.cursor.saturating_sub(self.width_in_chars);
            return;
        }
        // ignore any other non printable chars
        if !(32..256).contains(&ch) {
            return;
        }
        let c = ch as u8;
        if overstrike {
            // - 2 to leave room for the leading slash and trailing \0
            if self.cursor == MAX_EDIT_LINE - 2 {
                return;
            }
            if self.cursor < len {
                self.buffer[self.cursor] = c;
            } else {
                self.buffer.push(c);
            }
            self.cursor += 1;
        } else {
            // insert mode
            if len == MAX_EDIT_LINE - 2 {
                return; // all full
            }
            self.buffer.insert(self.cursor, c);
            self.cursor += 1;
        }
        if self.cursor >= self.width_in_chars {
            self.scroll += 1;
        }
    }

    /// `Field_VariableSizeDraw`'s horizontal scrolling for small chars:
    /// (the visible text, the cursor column within it).
    pub fn visible(&mut self) -> (Vec<u8>, i32) {
        // - 1 so there is always a space for the cursor
        let mut draw_len = self.width_in_chars as i32 - 1;
        let len = self.buffer.len() as i32;
        let prestep = if len <= draw_len {
            0
        } else {
            if self.scroll as i32 + draw_len > len {
                self.scroll = (len - draw_len).max(0) as usize;
            }
            self.scroll as i32
        };
        if prestep + draw_len > len {
            draw_len = len - prestep;
        }
        let s = self.buffer[prestep as usize..(prestep + draw_len.max(0)) as usize].to_vec();
        let i = draw_len - s.len() as i32;
        (s, self.cursor as i32 - prestep - i)
    }
}

// =====================================================================
// tab completion (common.c)
// =====================================================================

/// What the completion functions enumerate (`Cmd_CommandCompletion`,
/// `Cvar_CommandCompletion`, `FS_FilenameCompletion`).
pub trait CompletionSource {
    /// every registered command name
    fn commands(&self) -> Vec<String>;
    /// every cvar (name, value)
    fn cvars(&self) -> Vec<(String, String)>;
    /// files under `dir` with extension `ext` (without the dir; `strip`: without the extension)
    fn files(&self, dir: &str, ext: &str, strip: bool) -> Vec<String>;
}

/// `completionString` / `shortestMatch` / `matchCount`.
struct Matches {
    completion: Vec<u8>,
    shortest: Vec<u8>,
    count: usize,
}

impl Matches {
    fn new(completion: &[u8]) -> Self {
        Matches { completion: completion.to_vec(), shortest: Vec::new(), count: 0 }
    }

    /// `FindMatches`.
    fn find(&mut self, s: &str) {
        let s = bytes_of(s);
        let n = self.completion.len();
        if s.len() < n || !s[..n].eq_ignore_ascii_case(&self.completion) {
            return;
        }
        self.count += 1;
        if self.count == 1 {
            self.shortest = s;
            return;
        }
        // cut shortestMatch to the amount common with s
        let mut i = 0;
        while i < self.shortest.len() {
            if i >= s.len() || !self.shortest[i].eq_ignore_ascii_case(&s[i]) {
                self.shortest.truncate(i);
                break;
            }
            i += 1;
        }
    }

    /// The `PrintMatches` test (`Q_stricmpn( s, shortestMatch, strlen )`).
    fn printed(&self, s: &str) -> bool {
        let s = bytes_of(s);
        let n = self.shortest.len();
        s.len() >= n && s[..n].eq_ignore_ascii_case(&self.shortest)
    }
}

/// `Com_SkipCharset`.
fn skip_charset(b: &[u8], mut p: usize, set: &[u8]) -> usize {
    while p < b.len() && set.contains(&b[p]) {
        p += 1;
    }
    p
}

/// `Com_SkipTokens`: the position after `num` tokens separated by `sep`, or `s`.
fn skip_tokens(b: &[u8], s: usize, num: usize, sep: &[u8]) -> usize {
    let at = |p: usize| b.get(p).copied().unwrap_or(0);
    let (mut count, mut p) = (0, s);
    while count < num {
        let c = at(p);
        p += 1;
        if sep.contains(&c) {
            count += 1;
            while sep.contains(&at(p)) {
                p += 1;
            }
        } else if at(p) == 0 {
            break;
        }
    }
    if count == num {
        p.min(b.len())
    } else {
        s
    }
}

/// The completion of one console line (`completionField` and the prints).
struct Completer<'a> {
    field: &'a mut Field,
    src: &'a dyn CompletionSource,
    out: &'a mut Vec<String>,
}

impl Completer<'_> {
    /// `Field_Complete`: false when several matches were listed.
    fn complete(&mut self, m: &Matches) -> bool {
        if m.count == 0 {
            return true;
        }
        let offset = self.field.buffer.len() - m.completion.len();
        self.field.buffer.truncate(offset);
        self.field.buffer.extend_from_slice(&m.shortest);
        self.field.buffer.truncate(MAX_EDIT_LINE - 1);
        self.field.cursor = self.field.buffer.len();
        if m.count == 1 {
            if self.field.buffer.len() < MAX_EDIT_LINE - 1 {
                self.field.buffer.push(b' ');
                self.field.cursor += 1;
            }
            return true;
        }
        self.out.push(format!("]{}\n", self.field.text()));
        false
    }

    /// `Field_CompleteKeyname`.
    fn complete_keyname(&mut self, completion: &[u8]) {
        let names = keyname_completion_list();
        let mut m = Matches::new(completion);
        names.iter().for_each(|n| m.find(n));
        if !self.complete(&m) {
            for n in names.iter().filter(|n| m.printed(n)) {
                self.out.push(format!("    {n}\n"));
            }
        }
    }

    /// `Field_CompleteFilename`.
    fn complete_filename(&mut self, completion: &[u8], dir: &str, ext: &str, strip: bool) {
        let files = self.src.files(dir, ext, strip);
        let mut m = Matches::new(completion);
        files.iter().for_each(|n| m.find(n));
        if !self.complete(&m) {
            for n in files.iter().filter(|n| m.printed(n)) {
                self.out.push(format!("    {n}\n"));
            }
        }
    }

    /// `Field_CompleteCommand` on the text from `cmd` (an index into the
    /// field's buffer, like the C pointer: it is not moved when a '\' is
    /// put in front of the buffer).
    fn complete_command(&mut self, cmd: usize, do_commands: bool, do_cvars: bool) {
        // Skip leading whitespace and quotes
        let cmd = skip_charset(&self.field.buffer, cmd, b" \"");
        let text = string_of(&self.field.buffer[cmd.min(self.field.buffer.len())..]);
        let argv = crate::cmd::tokenize_ignore_quotes(&text);
        let mut completion_argument = argv.len();
        // If there is trailing whitespace on the cmd
        let completion: Vec<u8> = if text.ends_with(' ') {
            completion_argument += 1;
            Vec::new()
        } else {
            argv.get(completion_argument.wrapping_sub(1)).map(|s| bytes_of(s)).unwrap_or_default()
        };
        // Unconditionally add a '\' to the start of the buffer
        if !self.field.buffer.is_empty() && self.field.buffer[0] != b'\\' {
            if self.field.buffer[0] != b'/' {
                // Buffer is full, refuse to complete
                if self.field.buffer.len() + 1 >= MAX_EDIT_LINE {
                    return;
                }
                self.field.buffer.insert(0, b'\\');
                self.field.cursor += 1;
            }
            self.field.buffer[0] = b'\\';
        }
        if completion_argument > 1 {
            let base = argv[0].trim_start_matches(['\\', '/']).to_ascii_lowercase();
            let b = &self.field.buffer;
            if let Some(p) = b[cmd.min(b.len())..].iter().position(|&c| c == b';') {
                // Compound command
                self.complete_command(cmd + p + 1, true, true);
            } else {
                self.complete_argument(&base, cmd, completion_argument, &completion);
            }
            return;
        }
        let completion = completion.strip_prefix(b"\\").or_else(|| completion.strip_prefix(b"/")).unwrap_or(&completion).to_vec();
        if completion.is_empty() {
            return;
        }
        let mut m = Matches::new(&completion);
        let commands = if do_commands { self.src.commands() } else { Vec::new() };
        let cvars = if do_cvars { self.src.cvars() } else { Vec::new() };
        commands.iter().for_each(|c| m.find(c));
        cvars.iter().for_each(|(c, _)| m.find(c));
        if !self.complete(&m) {
            // run through again, printing matches
            for c in commands.iter().filter(|c| m.printed(c)) {
                self.out.push(format!("    {c}\n"));
            }
            for (c, v) in cvars.iter().filter(|(c, _)| m.printed(c)) {
                // Com_TruncateLongString (TRUNCATE_LENGTH 64)
                let v = if v.chars().count() > 63 { format!("{} ... {}", v.chars().take(30).collect::<String>(), v.chars().skip(v.chars().count() - 30).collect::<String>()) } else { v.clone() };
                self.out.push(format!("    {c} = \"{v}\"\n"));
            }
        }
    }

    /// `Cmd_CompleteArgument`: the command's completion function.
    fn complete_argument(&mut self, base: &str, args: usize, arg_num: usize, completion: &[u8]) {
        let b = self.field.buffer.clone();
        match base {
            // Cmd_CompleteCfgName
            "exec" | "execq" if arg_num == 2 => self.complete_filename(completion, "", "cfg", false),
            // Cmd_CompleteTxtName
            "condump" if arg_num == 2 => self.complete_filename(completion, "", "txt", false),
            // Cvar_CompleteCvarName
            "vstr" | "toggle" | "set" | "sets" | "setu" | "seta" | "reset" | "unset" | "print" if arg_num == 2 => {
                // Skip "<cmd> "
                let p = skip_tokens(&b, args, 1, b" ");
                if p > args {
                    self.complete_command(p, false, true);
                }
            }
            // Key_CompleteUnbind / Key_CompleteBind
            "unbind" | "bind" if arg_num == 2 => {
                if skip_tokens(&b, args, 1, b" ") > args {
                    self.complete_keyname(completion);
                }
            }
            "bind" if arg_num >= 3 => {
                // Skip "bind <key> "
                let p = skip_tokens(&b, args, 2, b" ");
                if p > args {
                    self.complete_command(p, true, true);
                }
            }
            // CL_CompleteRcon
            "rcon" if arg_num == 2 => {
                let p = skip_tokens(&b, args, 1, b" ");
                if p > args {
                    self.complete_command(p, true, true);
                }
            }
            _ => {}
        }
    }
}

/// `Field_AutoComplete`: Tab expansion of `field`; the listed matches are
/// returned as console lines.
pub fn field_auto_complete(field: &mut Field, src: &dyn CompletionSource) -> Vec<String> {
    let mut out = Vec::new();
    Completer { field, src, out: &mut out }.complete_command(0, true, true);
    out
}

// =====================================================================
// the console (Console_Key and the history)
// =====================================================================

/// What a console key did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConsoleAction {
    None,
    /// `Cbuf_AddText` this (Enter, ctrl-L)
    Exec(String),
    /// Tab: complete against the source
    Complete,
    /// the history changed: write `CONSOLE_HISTORY_FILE`
    SaveHistory(String),
}

/// The console: text, the edit line (`g_consoleField`), the history
/// (`historyEditLines`, `nextHistoryLine`, `historyLine`) and
/// `key_overstrikeMode`.
#[derive(Clone, Debug)]
pub struct Console {
    pub con: ConText,
    pub field: Field,
    history: Vec<Field>,
    next_history_line: usize,
    history_line: usize,
    pub overstrike: bool,
}

impl Default for Console {
    fn default() -> Self {
        Console { con: ConText::default(), field: Field::new(), history: vec![Field::new(); COMMAND_HISTORY], next_history_line: 0, history_line: 0, overstrike: false }
    }
}

impl Console {
    /// `CL_ConsolePrint`.
    pub fn print(&mut self, s: &str) {
        self.con.print(s);
    }

    /// `Con_ToggleConsole_f` / `Con_Close`'s field reset.
    pub fn reset_field(&mut self) {
        self.field.clear();
        self.field.width_in_chars = CONSOLE_FIELD_WIDTH;
    }

    /// `Console_Key`: history, scrollback, Enter and Tab; the rest goes to
    /// `Field_KeyDownEvent`. `active` is clc.state == CA_ACTIVE.
    pub fn key(&mut self, key: i32, ctrl: bool, shift: bool, active: bool, clipboard: &str) -> Vec<ConsoleAction> {
        let lower = if (0..256).contains(&key) { (key as u8).to_ascii_lowercase() as i32 } else { key };
        // ctrl-L clears screen
        if key == b'l' as i32 && ctrl {
            return vec![ConsoleAction::Exec("clear\n".into())];
        }
        // enter finishes the line
        if key == K_ENTER || key == K_KP_ENTER {
            let b = &self.field.buffer;
            // if not in the game explicitly prepend a slash if needed
            if !active && !b.is_empty() && b[0] != b'\\' && b[0] != b'/' {
                self.field.buffer.insert(0, b'\\');
                self.field.buffer.truncate(MAX_EDIT_LINE - 2);
                self.field.cursor += 1;
            }
            let line = self.field.text();
            self.print(&format!("]{line}\n"));
            let mut out = Vec::new();
            // leading slash is an explicit command
            if line.starts_with('\\') || line.starts_with('/') {
                out.push(ConsoleAction::Exec(format!("{}\n", &line[1..])));
            } else if line.is_empty() {
                // empty lines just scroll the console without adding to history
                return out;
            } else {
                // other text will be chat messages
                out.push(ConsoleAction::Exec(format!("cmd say {line}\n")));
            }
            // copy line to history buffer
            self.history[self.next_history_line % COMMAND_HISTORY] = self.field.clone();
            self.next_history_line += 1;
            self.history_line = self.next_history_line;
            self.reset_field();
            out.push(ConsoleAction::SaveHistory(self.save_history()));
            return out;
        }
        // command completion
        if key == K_TAB {
            return vec![ConsoleAction::Complete];
        }
        // command history (ctrl-p ctrl-n for unix style)
        if (key == K_MWHEELUP && shift) || key == K_UPARROW || key == K_KP_UPARROW || (lower == b'p' as i32 && ctrl) {
            if self.next_history_line - self.history_line < COMMAND_HISTORY && self.history_line > 0 {
                self.history_line -= 1;
            }
            self.field = self.history[self.history_line % COMMAND_HISTORY].clone();
            return vec![ConsoleAction::None];
        }
        if (key == K_MWHEELDOWN && shift) || key == K_DOWNARROW || key == K_KP_DOWNARROW || (lower == b'n' as i32 && ctrl) {
            self.history_line += 1;
            if self.history_line >= self.next_history_line {
                self.history_line = self.next_history_line;
                self.reset_field();
                return vec![ConsoleAction::None];
            }
            self.field = self.history[self.history_line % COMMAND_HISTORY].clone();
            return vec![ConsoleAction::None];
        }
        // console scrolling
        match key {
            K_PGUP => self.con.page_up(),
            K_PGDN => self.con.page_down(),
            K_MWHEELUP => {
                // hold <ctrl> to accelerate scrolling
                for _ in 0..if ctrl { 3 } else { 1 } {
                    self.con.page_up();
                }
            }
            K_MWHEELDOWN => {
                for _ in 0..if ctrl { 3 } else { 1 } {
                    self.con.page_down();
                }
            }
            // ctrl-home = top of console, ctrl-end = bottom of console
            K_HOME if ctrl => self.con.top(),
            K_END if ctrl => self.con.bottom(),
            // pass to the normal editline routine
            _ => self.field.key_down_event(key, shift, &mut self.overstrike, clipboard),
        }
        vec![ConsoleAction::None]
    }

    /// `Field_CharEvent` on the console field.
    pub fn char_event(&mut self, ch: i32, clipboard: &str) {
        // delete is not a printable character (CL_CharEvent)
        if ch == 127 {
            return;
        }
        self.field.char_event(ch, self.overstrike, clipboard);
    }

    /// Tab (`Field_AutoComplete( &g_consoleField )`): the listed matches are printed.
    pub fn complete(&mut self, src: &dyn CompletionSource) {
        for l in field_auto_complete(&mut self.field, src) {
            self.print(&l);
        }
    }

    /// The history lines, oldest first (tests).
    #[cfg(test)]
    pub fn history_lines(&self) -> Vec<String> {
        (0..self.next_history_line.min(COMMAND_HISTORY)).map(|i| self.history[i].text()).collect()
    }

    /// `CL_SaveConsoleHistory`'s file text: "cursor scroll length text "
    /// per line, newest first, within MAX_CONSOLE_SAVE_BUFFER.
    pub fn save_history(&self) -> String {
        let mut buf = String::new();
        let start = (self.next_history_line + COMMAND_HISTORY - 1) % COMMAND_HISTORY;
        let mut i = start;
        loop {
            let h = &self.history[i];
            if !h.buffer.is_empty() {
                let additional = h.buffer.len() + "999 999 999  ".len();
                if buf.len() + additional < MAX_CONSOLE_SAVE_BUFFER {
                    buf.push_str(&format!("{} {} {} {} ", h.cursor, h.scroll, h.buffer.len(), h.text()));
                } else {
                    break;
                }
            }
            i = (i + COMMAND_HISTORY - 1) % COMMAND_HISTORY;
            if i == start {
                break;
            }
        }
        buf
    }

    /// `CL_LoadConsoleHistory` from the file's text (false: "Couldn't read").
    pub fn load_history(&mut self, text: &str) -> bool {
        let b = bytes_of(text);
        if b.len() > MAX_CONSOLE_SAVE_BUFFER {
            return false;
        }
        let mut p = 0usize;
        // COM_Parse: skip whitespace, read a token
        let token = |p: &mut usize| -> Option<String> {
            while *p < b.len() && b[*p] <= b' ' {
                *p += 1;
            }
            let s = *p;
            while *p < b.len() && b[*p] > b' ' {
                *p += 1;
            }
            (s < *p).then(|| string_of(&b[s..*p]))
        };
        let mut lines: Vec<Field> = Vec::new();
        while lines.len() < COMMAND_HISTORY {
            let Some(cursor) = token(&mut p) else { break };
            let Some(scroll) = token(&mut p) else { break };
            let Some(n) = token(&mut p) else { break };
            let n = trem_sim::kernel::spawn::c_atoi(&n).max(0) as usize;
            p += 1;
            if n > b.len().saturating_sub(p) {
                // probable corrupt history
                break;
            }
            let mut f = Field::new();
            f.cursor = trem_sim::kernel::spawn::c_atoi(&cursor).max(0) as usize;
            f.scroll = trem_sim::kernel::spawn::c_atoi(&scroll).max(0) as usize;
            f.buffer = b[p..p + n].to_vec();
            f.cursor = f.cursor.min(f.buffer.len());
            p += n;
            lines.push(f);
        }
        // the file is newest first: historyEditLines[0] is the oldest
        lines.reverse();
        let num = lines.len();
        self.history = lines;
        self.history.resize(COMMAND_HISTORY, Field::new());
        self.history_line = num;
        self.next_history_line = num;
        true
    }
}

// =====================================================================
// drawing (Con_DrawSolidConsole / Con_DrawInput)
// =====================================================================

/// One draw call of the console.
#[derive(Clone, Debug, PartialEq)]
pub enum ConDraw {
    /// `SCR_DrawPic( ..., cls.consoleShader )` in 640x480 virtual units
    Background([f32; 4]),
    /// `SCR_FillRect` in 640x480 virtual units
    Fill([f32; 4], [f32; 4]),
    /// `SCR_DrawSmallChar` at real pixels with the colour
    Char(i32, i32, u8, [f32; 4]),
    /// wt-b: `SCR_DrawChar` in 640x480 virtual units (x, y, size)
    VirtChar(f32, f32, f32, u8, [f32; 4]),
}

/// wt-b VoIP: `SCR_DrawVoipMeter`'s `SCR_DrawStringExt( 320 - strlen( string )
/// * 4, 10, 8, string, g_color_table[7], qtrue, qfalse )` (drop shadow, then white).
pub fn draw_voip_meter(text: &str) -> Vec<ConDraw> {
    let x0 = 320.0 - text.len() as f32 * 4.0;
    let mut out = Vec::new();
    for (off, color) in [(2.0, [0.0, 0.0, 0.0, 1.0]), (0.0, [1.0, 1.0, 1.0, 1.0])] {
        for (i, ch) in text.bytes().enumerate() {
            if ch != b' ' {
                out.push(ConDraw::VirtChar(x0 + i as f32 * 8.0 + off, 10.0 + off, 8.0, ch, color));
            }
        }
    }
    out
}

/// `Con_DrawSolidConsole( frac )` for a vid_w x vid_h window; `input`:
/// the edit line is drawn (`Con_DrawInput`'s check), `realtime` blinks the cursor.
pub fn draw_solid_console(c: &mut Console, frac: f32, vid_w: i32, vid_h: i32, input: bool, realtime: u32) -> Vec<ConDraw> {
    let mut out = Vec::new();
    let mut lines = (vid_h as f32 * frac) as i32;
    if lines <= 0 {
        return out;
    }
    lines = lines.min(vid_h);
    // on wide screens, we will center the text (SCR_AdjustFrom640 of 0: 0)
    let xadjust = 0;
    // draw the background
    let y = (frac * SCREEN_HEIGHT as f32) as i32;
    let y = if y < 1 { 0 } else { y };
    if y > 0 {
        out.push(ConDraw::Background([0.0, 0.0, SCREEN_WIDTH as f32, y as f32]));
    }
    out.push(ConDraw::Fill([0.0, y as f32, SCREEN_WIDTH as f32, 2.0], COLOR_TABLE[COLOR_RED]));
    // draw the version number
    let v = Q3_VERSION.as_bytes();
    let n = v.len() as i32;
    for (x, &ch) in v.iter().enumerate() {
        out.push(ConDraw::Char(vid_w - (n - x as i32 + 1) * SMALLCHAR_WIDTH, lines - SMALLCHAR_HEIGHT, ch, COLOR_TABLE[COLOR_RED]));
    }
    // draw the text
    let con = &mut c.con;
    con.vislines = lines;
    let mut rows = (lines - SMALLCHAR_HEIGHT) / SMALLCHAR_HEIGHT;
    let mut y = lines - SMALLCHAR_HEIGHT * 3;
    // draw from the bottom up
    if con.display != con.current {
        // draw arrows to show the buffer is backscrolled
        let mut x = 0;
        while x < con.linewidth {
            out.push(ConDraw::Char(xadjust + (x + 1) * SMALLCHAR_WIDTH, y, b'^', COLOR_TABLE[COLOR_RED]));
            x += 4;
        }
        y -= SMALLCHAR_HEIGHT;
        rows -= 1;
    }
    let mut row = con.display;
    if con.x == 0 {
        row -= 1;
    }
    for _ in 0..rows {
        if row < 0 {
            break;
        }
        if con.current - row < con.totallines {
            for (x, &t) in con.row(row).iter().enumerate() {
                if t & 0xff == b' ' as u16 {
                    continue;
                }
                out.push(ConDraw::Char(xadjust + (x as i32 + 1) * SMALLCHAR_WIDTH, y, (t & 0xff) as u8, COLOR_TABLE[((t >> 8) & 7) as usize]));
            }
        }
        y -= SMALLCHAR_HEIGHT;
        row -= 1;
    }
    // draw the input prompt, user text, and cursor if desired
    if input {
        let y = con.vislines - SMALLCHAR_HEIGHT * 2;
        let white = COLOR_TABLE[COLOR_WHITE as usize];
        out.push(ConDraw::Char(xadjust + SMALLCHAR_WIDTH, y, b']', white));
        let x0 = xadjust + 2 * SMALLCHAR_WIDTH;
        let (s, cur) = c.field.visible();
        // SCR_DrawSmallStringExt( noColorEscape ): colours apply, escapes are drawn
        let mut color = white;
        for (i, &ch) in s.iter().enumerate() {
            if is_color_string(&s, i) {
                color = COLOR_TABLE[color_index(s[i + 1]) as usize];
            }
            if ch != b' ' {
                out.push(ConDraw::Char(x0 + i as i32 * SMALLCHAR_WIDTH, y, ch, color));
            }
        }
        // draw the cursor (off blink every 256 ms)
        if (realtime >> 8) & 1 == 0 {
            let cursor_char = if c.overstrike { 11 } else { 10 };
            out.push(ConDraw::Char(x0 + cur * SMALLCHAR_WIDTH, y, cursor_char, white));
        }
    }
    out
}

// =====================================================================
// the Godot node
// =====================================================================

/// The drop-down console (hud.gd adds it; `open` / `close`). Draws the
/// `Console` of the TremGame node and sends its keys there while it is down.
#[derive(GodotClass)]
#[class(base=Control)]
pub struct TremConsole {
    base: Base<Control>,
    /// Path to the TremGame node.
    #[export]
    game_path: NodePath,
    /// the console has the keys (KEYCATCH_CONSOLE)
    down: bool,
    last_ms: u64,
    charset: Option<Gd<godot::classes::Texture2D>>,
    background: Option<Gd<godot::classes::Texture2D>>,
    tried_textures: bool,
    ops: Vec<ConDraw>,
}

#[godot_api]
impl IControl for TremConsole {
    fn init(base: Base<Control>) -> Self {
        TremConsole { base, game_path: NodePath::from("../../Game"), down: false, last_ms: 0, charset: None, background: None, tried_textures: false, ops: Vec::new() }
    }

    fn ready(&mut self) {
        let mut b = self.base_mut();
        // made from a script: the offsets too, or the rect stays empty
        b.set_anchors_and_offsets_preset(godot::classes::control::LayoutPreset::FULL_RECT);
        b.set_mouse_filter(godot::classes::control::MouseFilter::IGNORE);
        b.set_focus_mode(godot::classes::control::FocusMode::ALL);
    }

    fn process(&mut self, _delta: f64) {
        let now = godot::classes::Time::singleton().get_ticks_msec();
        let frametime = if self.last_ms == 0 { 0.0 } else { (now - self.last_ms) as f32 };
        self.last_ms = now;
        let Some(mut game) = self.game() else { return };
        let size = self.base().get_size();
        let (vw, vh) = (size.x as i32, size.y as i32);
        if !self.tried_textures {
            if let Some(lib) = game.bind_mut().hud_library() {
                self.tried_textures = true;
                self.charset = lib.texture("gfx/2d/bigchars");
                self.background = lib.texture("console");
            }
        }
        let down = self.down;
        let mut g = game.bind_mut();
        let speed = g.client_cvars_ref().find("scr_conspeed").map(|v| trem_sim::kernel::spawn::c_atof(&v.string) as f32).unwrap_or(3.0);
        // wt-b: joystick keys while the console has the keys (Console_Key)
        g.wtb_set_console_down(down);
        for (k, _) in g.wtb_take_joy_keys(true) {
            g.misc_console_key(k, false, false, "");
        }
        let meter = g.wtb_voip_meter();
        let voice = g.wtb_voip_hud();
        let c = g.misc_console();
        c.con.run(down, speed, frametime);
        let frac = c.con.display_frac;
        self.ops = if frac > 0.0 { draw_solid_console(c, frac, vw, vh, down, now as u32) } else { Vec::new() };
        drop(g);
        // SCR_DrawScreenField: the voip meter over the console
        if let Some(m) = meter.as_deref() {
            self.ops.extend(draw_voip_meter(m));
        }
        // the port's voice HUD (dec053)
        if let Some(h) = voice.as_ref() {
            self.ops.extend(crate::voiphud::draw(h));
        }
        let visible = frac > 0.0 || meter.is_some() || voice.is_some();
        self.base_mut().set_visible(visible);
        self.base_mut().queue_redraw();
    }

    fn draw(&mut self) {
        let size = self.base().get_size();
        let (xs, ys) = (size.x / SCREEN_WIDTH as f32, size.y / SCREEN_HEIGHT as f32);
        let ops = std::mem::take(&mut self.ops);
        let col = |c: &[f32; 4]| Color::from_rgba(c[0], c[1], c[2], c[3]);
        for op in &ops {
            match op {
                ConDraw::Background(r) => {
                    let rect = Rect2::new(Vector2::new(r[0] * xs, r[1] * ys), Vector2::new(r[2] * xs, r[3] * ys));
                    match &self.background {
                        Some(t) => {
                            let t = t.clone();
                            self.base_mut().draw_texture_rect_ex(&t, rect, false).done();
                        }
                        // the console shader of the data is black
                        None => self.base_mut().draw_rect(rect, Color::BLACK),
                    }
                }
                ConDraw::Fill(r, c) => {
                    let rect = Rect2::new(Vector2::new(r[0] * xs, r[1] * ys), Vector2::new(r[2] * xs, r[3] * ys));
                    self.base_mut().draw_rect(rect, col(c));
                }
                ConDraw::VirtChar(x, y, size, ch, c) => {
                    let rect = Rect2::new(Vector2::new(x * xs, y * ys), Vector2::new(size * xs, size * ys));
                    if let Some(t) = &self.charset {
                        let (row, colm) = ((*ch >> 4) as f32, (*ch & 15) as f32);
                        let t = &crate::hdtex::ui(t);
                        let src = Rect2::new(Vector2::new(colm * 0.0625, row * 0.0625) * t.get_size(), Vector2::new(0.0625, 0.0625) * t.get_size());
                        let t = t.clone();
                        self.base_mut().draw_texture_rect_region_ex(&t, rect, src).modulate(col(c)).done();
                    }
                }
                ConDraw::Char(x, y, ch, c) => {
                    let rect = Rect2::new(Vector2::new(*x as f32, *y as f32), Vector2::new(SMALLCHAR_WIDTH as f32, SMALLCHAR_HEIGHT as f32));
                    match &self.charset {
                        Some(t) => {
                            let (row, colm) = ((*ch >> 4) as f32, (*ch & 15) as f32);
                            let t = &crate::hdtex::ui(t);
                        let src = Rect2::new(Vector2::new(colm * 0.0625, row * 0.0625) * t.get_size(), Vector2::new(0.0625, 0.0625) * t.get_size());
                            let t = t.clone();
                            self.base_mut().draw_texture_rect_region_ex(&t, rect, src).modulate(col(c)).done();
                        }
                        None => {
                            let font = self.base().get_theme_default_font();
                            if let Some(f) = font {
                                let s = GString::from((*ch as char).to_string().as_str());
                                self.base_mut().draw_string_ex(&f, Vector2::new(*x as f32, (*y + 13) as f32), &s).font_size(14).modulate(col(c)).done();
                            }
                        }
                    }
                }
            }
        }
        self.ops = ops;
    }

    fn gui_input(&mut self, event: Gd<InputEvent>) {
        if !self.down {
            return;
        }
        let Ok(k) = event.try_cast::<InputEventKey>() else { return };
        if !k.is_pressed() {
            return;
        }
        let code = if k.get_keycode() != godot::global::Key::NONE { k.get_keycode() } else { k.get_physical_keycode() };
        let key = crate::keys::keynum_from_godot_key(code);
        let (ctrl, shift) = (k.is_ctrl_pressed(), k.is_shift_pressed());
        let uni = k.get_unicode() as i32;
        self.base_mut().accept_event();
        // the console key and escape close it (hud.gd's _input)
        if key == K_ESCAPE || key == b'`' as i32 || key == b'~' as i32 {
            return;
        }
        let clip = godot::classes::DisplayServer::singleton().clipboard_get().to_string();
        let Some(mut game) = self.game() else { return };
        let mut g = game.bind_mut();
        if key > 0 {
            g.misc_console_key(key, ctrl, shift, &clip);
        }
        // SE_CHAR: printable characters, backspace as ctrl-h and ctrl-<letter>
        let ch = if key == K_BACKSPACE {
            8
        } else if ctrl && (b'a' as i32..=b'z' as i32).contains(&key) {
            key - b'a' as i32 + 1
        } else {
            uni
        };
        if ch > 0 && !(key == K_TAB || key == K_ENTER || key == K_KP_ENTER) {
            g.misc_console().char_event(ch, &clip);
        }
    }
}

#[godot_api]
impl TremConsole {
    /// `Con_ToggleConsole_f` (down) with `prefix` in the field (messagemode
    /// without the original menus).
    #[func]
    fn open(&mut self, prefix: GString) {
        self.down = true;
        if let Some(mut game) = self.game() {
            let mut g = game.bind_mut();
            let c = g.misc_console();
            c.reset_field();
            c.field.set_text(&prefix.to_string());
        }
        self.base_mut().set_visible(true);
        self.base_mut().grab_focus();
    }

    /// `Con_Close` / toggling it up: the field is cleared, the console scrolls up.
    #[func]
    fn close(&mut self) {
        self.down = false;
        if let Some(mut game) = self.game() {
            game.bind_mut().misc_console().reset_field();
        }
        self.base_mut().release_focus();
    }

    /// The console has the keys.
    #[func]
    fn is_down(&self) -> bool {
        self.down
    }
}

impl TremConsole {
    fn game(&self) -> Option<Gd<crate::game::TremGame>> {
        self.base().get_node_or_null(&self.game_path)?.try_cast::<crate::game::TremGame>().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Src;
    impl CompletionSource for Src {
        fn commands(&self) -> Vec<String> {
            ["bind", "bindlist", "callvote", "condump", "exec", "set", "seta", "toggle", "rcon", "say", "status", "unbind"].map(String::from).to_vec()
        }
        fn cvars(&self) -> Vec<(String, String)> {
            [("cl_run", "1"), ("cl_freelook", "1"), ("cg_viewsize", "100"), ("sensitivity", "5"), ("g_warmup", "10")].map(|(a, b)| (a.to_string(), b.to_string())).to_vec()
        }
        fn files(&self, _dir: &str, ext: &str, _strip: bool) -> Vec<String> {
            if ext == "cfg" {
                vec!["autoexec.cfg".into(), "autogen.cfg".into(), "default.cfg".into()]
            } else {
                Vec::new()
            }
        }
    }

    fn typed(c: &mut Console, s: &str) {
        for ch in s.bytes() {
            c.char_event(ch as i32, "");
        }
    }

    fn tab(c: &mut Console) -> Vec<String> {
        field_auto_complete(&mut c.field, &Src)
    }

    #[test]
    fn print_wraps_words_and_keeps_colours() {
        let mut c = ConText::default();
        assert_eq!((c.linewidth, c.totallines), (78, 2100));
        c.print("^1red ^7white\nnext");
        assert_eq!(c.line_text(c.current - 1), "red white");
        assert_eq!(c.row(c.current - 1)[0] >> 8, 1);
        assert_eq!(c.row(c.current - 1)[4] >> 8, 7);
        assert_eq!(c.line_text(c.current), "next");
        // a word that does not fit moves to the next line
        let mut c = ConText::default();
        c.print(&format!("{} abcdefgh", "x".repeat(72)));
        assert_eq!(c.line_text(c.current), "abcdefgh");
        assert_eq!(c.dump(), format!("{} \nabcdefgh\n", "x".repeat(72)).replace(" \n", "\n"));
    }

    #[test]
    fn scrollback_pages_by_two_lines() {
        let mut c = ConText::default();
        for i in 0..10 {
            c.print(&format!("line {i}\n"));
        }
        let cur = c.current;
        c.page_up();
        assert_eq!(c.display, cur - 2);
        c.page_down();
        c.page_down();
        assert_eq!(c.display, cur);
        // Con_Top: line totallines, or the oldest line kept once the ring wrapped
        c.top();
        assert_eq!(c.display, c.totallines);
        for i in 0..3000 {
            c.print(&format!("more {i}\n"));
        }
        c.top();
        assert_eq!(c.display, c.current - c.totallines + 1);
        let cur = c.current;
        c.bottom();
        assert_eq!(c.display, cur);
    }

    #[test]
    fn field_editing_keys_and_chars() {
        let mut c = Console::default();
        typed(&mut c, "hello");
        c.key(K_LEFTARROW, false, false, true, "");
        c.key(K_LEFTARROW, false, false, true, "");
        typed(&mut c, "X");
        assert_eq!(c.field.text(), "helXlo");
        // backspace (ctrl-h), delete
        c.char_event(8, "");
        c.key(K_DEL, false, false, true, "");
        assert_eq!(c.field.text(), "helo");
        // insert toggles overstrike
        c.key(K_HOME, false, false, true, "");
        c.key(K_INS, false, false, true, "");
        typed(&mut c, "j");
        assert_eq!(c.field.text(), "jelo");
        c.key(K_INS, false, false, true, "");
        // ctrl-e end, ctrl-a home, ctrl-v paste, ctrl-c clear
        c.char_event(5, "");
        assert_eq!(c.field.cursor, 4);
        c.char_event(1, "");
        c.char_event(22, "ab");
        assert_eq!(c.field.text(), "abjelo");
        c.key(K_INS, false, true, true, "!");
        assert_eq!(c.field.text(), "ab!jelo");
        c.char_event(3, "");
        assert_eq!(c.field.text(), "");
        // a long line scrolls
        typed(&mut c, &"y".repeat(100));
        let (s, cur) = c.field.visible();
        assert_eq!(s.len(), 77);
        assert_eq!(cur, 77);
    }

    #[test]
    fn enter_runs_commands_or_says_and_keeps_history() {
        let mut c = Console::default();
        typed(&mut c, "/kill");
        let a = c.key(K_ENTER, false, false, true, "");
        assert_eq!(a[0], ConsoleAction::Exec("kill\n".into()));
        typed(&mut c, "hello there");
        let a = c.key(K_ENTER, false, false, true, "");
        assert_eq!(a[0], ConsoleAction::Exec("cmd say hello there\n".into()));
        // not in a game: a slash is put in front
        typed(&mut c, "map atcs");
        let a = c.key(K_ENTER, false, false, false, "");
        assert_eq!(a[0], ConsoleAction::Exec("map atcs\n".into()));
        assert!(c.con.dump().contains("]\\map atcs"));
        // empty lines only scroll
        assert!(c.key(K_ENTER, false, false, true, "").is_empty());
        // up / down walk the history
        c.key(K_UPARROW, false, false, true, "");
        assert_eq!(c.field.text(), "\\map atcs");
        c.key(K_UPARROW, false, false, true, "");
        c.key(K_UPARROW, false, false, true, "");
        assert_eq!(c.field.text(), "/kill");
        c.key(K_UPARROW, false, false, true, "");
        assert_eq!(c.field.text(), "/kill");
        c.key(b'n' as i32, true, false, true, "");
        assert_eq!(c.field.text(), "hello there");
        c.key(K_DOWNARROW, false, false, true, "");
        c.key(K_DOWNARROW, false, false, true, "");
        assert_eq!(c.field.text(), "");
        // ctrl-l clears the screen
        assert_eq!(c.key(b'l' as i32, true, false, true, ""), vec![ConsoleAction::Exec("clear\n".into())]);
    }

    #[test]
    fn history_file_round_trip() {
        let mut c = Console::default();
        for l in ["/bind x kill", "hi", "/seta name \"A B\""] {
            typed(&mut c, l);
            c.key(K_ENTER, false, false, true, "");
        }
        let text = c.save_history();
        assert_eq!(text, "16 0 16 /seta name \"A B\" 2 0 2 hi 12 0 12 /bind x kill ");
        let mut d = Console::default();
        assert!(d.load_history(&text));
        assert_eq!(d.history_lines(), vec!["/bind x kill", "hi", "/seta name \"A B\""]);
        d.key(K_UPARROW, false, false, true, "");
        assert_eq!(d.field.text(), "/seta name \"A B\"");
        assert_eq!(d.field.cursor, 16);
        // too big: "Couldn't read"
        assert!(!d.load_history(&"x".repeat(2000)));
    }

    #[test]
    fn tab_completes_commands_and_cvars() {
        let mut c = Console::default();
        // a single match: completed with a space, a '\' in front
        typed(&mut c, "callv");
        assert!(tab(&mut c).is_empty());
        assert_eq!(c.field.text(), "\\callvote ");
        // several: the common prefix, the matches listed
        c.reset_field();
        typed(&mut c, "cl_");
        let out = tab(&mut c);
        assert_eq!(c.field.text(), "\\cl_");
        assert_eq!(out, vec!["]\\cl_\n", "    cl_run = \"1\"\n", "    cl_freelook = \"1\"\n"]);
        c.reset_field();
        typed(&mut c, "/bi");
        let out = tab(&mut c);
        assert_eq!(c.field.text(), "\\bind");
        assert_eq!(out, vec!["]\\bind\n", "    bind\n", "    bindlist\n"]);
        // argument completion: cvars for set, keys for bind, commands after the key, cfg names for exec
        c.reset_field();
        typed(&mut c, "set sens");
        tab(&mut c);
        assert_eq!(c.field.text(), "\\set sensitivity ");
        c.reset_field();
        typed(&mut c, "bind mwheelu");
        tab(&mut c);
        assert_eq!(c.field.text(), "\\bind MWHEELUP ");
        c.reset_field();
        typed(&mut c, "bind x tog");
        tab(&mut c);
        assert_eq!(c.field.text(), "\\bind x toggle ");
        c.reset_field();
        typed(&mut c, "exec autoe");
        tab(&mut c);
        assert_eq!(c.field.text(), "\\exec autoexec.cfg ");
        // a compound command completes after the ;
        c.reset_field();
        typed(&mut c, "say hi;stat");
        tab(&mut c);
        assert_eq!(c.field.text(), "\\say hi;status ");
        // nothing matches: unchanged but for the '\'
        c.reset_field();
        typed(&mut c, "zzz");
        assert!(tab(&mut c).is_empty());
        assert_eq!(c.field.text(), "\\zzz");
    }

    #[test]
    fn solid_console_layout() {
        let mut c = Console::default();
        c.print("^3hello\n");
        typed(&mut c, "ab");
        let ops = draw_solid_console(&mut c, 0.5, 640, 480, true, 0);
        assert_eq!(ops[0], ConDraw::Background([0.0, 0.0, 640.0, 240.0]));
        assert_eq!(ops[1], ConDraw::Fill([0.0, 240.0, 640.0, 2.0], COLOR_TABLE[1]));
        // the version at the bottom right, in red
        assert_eq!(ops[2], ConDraw::Char(640 - 16 * 8, 240 - 16, b't', COLOR_TABLE[1]));
        // the text line above the input line, yellow
        assert!(ops.contains(&ConDraw::Char(8, 240 - 48, b'h', COLOR_TABLE[3])));
        // the prompt, the field and the cursor
        assert!(ops.contains(&ConDraw::Char(8, 240 - 32, b']', COLOR_TABLE[7])));
        assert!(ops.contains(&ConDraw::Char(16, 240 - 32, b'a', COLOR_TABLE[7])));
        assert!(ops.contains(&ConDraw::Char(32, 240 - 32, 10, COLOR_TABLE[7])));
        // backscrolled: a row of red carets
        c.con.page_up();
        let ops = draw_solid_console(&mut c, 0.5, 640, 480, false, 0);
        assert!(ops.contains(&ConDraw::Char(8, 240 - 48, b'^', COLOR_TABLE[1])));
    }
}
