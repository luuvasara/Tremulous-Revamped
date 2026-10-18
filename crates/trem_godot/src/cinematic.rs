//! client/cl_cin.c: RoQ cinematics (the id vector-quantised video format
//! of `video/*.roq`), engine-independent. The data ships one, the startup
//! `cinematic splash.RoQ` of `Com_Init`; the ui can also play them in
//! `WINDOW_STYLE_CINEMATIC` windows / `cinematic` menu keywords, the map
//! previews (`<map>.roq`, none in the data) and `playMovie`.
//!
//! Only the 32-bit path is ported: `initRoQ` always sets samplesPerPixel 4
//! (blitVQQuad32fs, yuv_to_rgb24); the 16-bit / grey / half-size /
//! smoothed-double branches of `decodeCodeBook` are unreachable there. The
//! file is read whole (FS_Read of the next chunk is a cursor into it), the
//! qStatus pointers are byte offsets into `linbuf`, and `S_RawSamples` is
//! the `audio` queue (22050 Hz stereo, i16) the Godot side plays.

/// cl_cin.c / qcommon.h constants
pub const MAXSIZE: i64 = 8;
pub const MINSIZE: i64 = 4;
pub const DEFAULT_CIN_WIDTH: i64 = 512;
pub const DEFAULT_CIN_HEIGHT: i64 = 512;
pub const ROQ_QUAD_INFO: u32 = 0x1001;
pub const ROQ_CODEBOOK: u32 = 0x1002;
pub const ROQ_QUAD_VQ: u32 = 0x1011;
pub const ROQ_QUAD_JPEG: u32 = 0x1012;
pub const ROQ_QUAD_HANG: u32 = 0x1013;
pub const ROQ_PACKET: u32 = 0x1030;
pub const ZA_SOUND_MONO: u32 = 0x1020;
pub const ZA_SOUND_STEREO: u32 = 0x1021;
/// `CIN_system` / `CIN_loop` / `CIN_hold` / `CIN_silent` / `CIN_shader`
pub const CIN_SYSTEM: i32 = 1;
pub const CIN_LOOP: i32 = 2;
pub const CIN_HOLD: i32 = 4;
pub const CIN_SILENT: i32 = 8;
pub const CIN_SHADER: i32 = 16;
/// the raw sample rate of RoQ sound chunks
pub const ROQ_SAMPLE_RATE: i32 = 22050;

/// `e_status`
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Idle,
    Play,
    Eof,
    Looped,
}

/// The YUV tables of `ROQ_GenYUVTables` and the square table of
/// `RllSetupTable` (static in the C, shared by every handle).
struct Tables {
    yy: [i64; 256],
    ub: [i64; 256],
    ug: [i64; 256],
    vg: [i64; 256],
    vr: [i64; 256],
    sqr: [i16; 256],
}

impl Tables {
    /// `ROQ_GenYUVTables` + `RllSetupTable`.
    fn new() -> Tables {
        let t_ub = (1.77200f32 / 2.0) * (1 << 6) as f32 + 0.5;
        let t_vr = (1.40200f32 / 2.0) * (1 << 6) as f32 + 0.5;
        let t_ug = (0.34414f32 / 2.0) * (1 << 6) as f32 + 0.5;
        let t_vg = (0.71414f32 / 2.0) * (1 << 6) as f32 + 0.5;
        let mut t = Tables { yy: [0; 256], ub: [0; 256], ug: [0; 256], vg: [0; 256], vr: [0; 256], sqr: [0; 256] };
        for i in 0..256usize {
            let x = (2 * i as i32 - 255) as f32;
            t.ub[i] = (t_ub * x + (1 << 5) as f32) as i64;
            t.vr[i] = (t_vr * x + (1 << 5) as f32) as i64;
            t.ug[i] = (-t_ug * x) as i64;
            t.vg[i] = (-t_vg * x + (1 << 5) as f32) as i64;
            t.yy[i] = ((i << 6) | (i >> 2)) as i64;
        }
        for z in 0..128usize {
            t.sqr[z] = (z * z) as i16;
            t.sqr[z + 128] = -t.sqr[z];
        }
        t
    }

    /// `yuv_to_rgb24`: RGBA8 bytes (LittleLong of r | g<<8 | b<<16 | 255<<24).
    fn yuv_to_rgb24(&self, y: usize, u: usize, v: usize) -> [u8; 4] {
        let yy = self.yy[y];
        let r = ((yy + self.vr[v]) >> 6).clamp(0, 255);
        let g = ((yy + self.ug[u] + self.vg[v]) >> 6).clamp(0, 255);
        let b = ((yy + self.ub[u]) >> 6).clamp(0, 255);
        [r as u8, g as u8, b as u8, 255]
    }
}

/// `RllDecodeStereoToStereo` (unsigned output): interleaved L/R samples.
pub fn rll_decode_stereo_to_stereo(sqr: &[i16; 256], from: &[u8], flag: u16, to: &mut Vec<i16>) -> usize {
    let mut prev_l = (flag & 0xff00) as i32;
    let mut prev_r = ((flag & 0x00ff) as i32) << 8;
    let mut z = 0;
    while z + 1 < from.len() {
        prev_l = (prev_l + sqr[from[z] as usize] as i32) as i16 as i32;
        prev_r = (prev_r + sqr[from[z + 1] as usize] as i32) as i16 as i32;
        to.push(prev_l as i16);
        to.push(prev_r as i16);
        z += 2;
    }
    from.len() >> 1
}

/// `RllDecodeMonoToStereo` (unsigned output): each sample twice.
pub fn rll_decode_mono_to_stereo(sqr: &[i16; 256], from: &[u8], flag: u16, to: &mut Vec<i16>) -> usize {
    let mut prev = flag as i32;
    for &b in from {
        prev = (prev + sqr[b as usize] as i32) as i16 as i32;
        to.push(prev as i16);
        to.push(prev as i16);
    }
    from.len()
}

/// One `cin_cache` entry plus the `cinematics_t` state it decodes into
/// (the C shares one `cin` between handles and resets it on a switch;
/// each handle owning its buffers is the same thing without the resets).
pub struct Roq {
    /// `fileName`
    #[allow(dead_code)]
    pub name: String,
    data: Vec<u8>,
    /// FS read offset of the open file, and where `cin.file` starts
    cursor: usize,
    file_base: usize,
    // cinematics_t
    linbuf: Vec<u8>,
    q_status: [Vec<Option<usize>>; 2],
    mcomp: [i64; 256],
    old: Option<(i64, i64, i64, i64)>,
    vq2: Vec<u8>,
    vq4: Vec<u8>,
    vq8: Vec<u8>,
    // cin_cache
    pub cin_width: i64,
    pub cin_height: i64,
    /// CIN_SetExtents (640x480 virtual)
    #[allow(dead_code)]
    pub extents: [f32; 4],
    pub looping: bool,
    pub hold_at_end: bool,
    pub dirty: bool,
    #[allow(dead_code)]
    pub alter_game_state: bool,
    pub silent: bool,
    pub shader: bool,
    pub status: Status,
    start_time: i64,
    last_time: i64,
    tfps: i64,
    roq_played: i64,
    roq_size: i64,
    roq_frame_size: u32,
    on_quad: usize,
    pub num_quads: i64,
    samples_per_line: i64,
    roq_id: u32,
    screen_delta: i64,
    samples_per_pixel: i64,
    xsize: i64,
    ysize: i64,
    in_memory: i64,
    normal_buffer0: i64,
    roq_flags: i64,
    roq_f0: i64,
    roq_f1: i64,
    t: [i64; 2],
    pub roq_fps: i64,
    /// `buf`: offset of the frame shown into `linbuf`
    buf: Option<usize>,
    /// `S_RawSamples`: interleaved stereo i16 at 22050 Hz not yet played
    pub audio: Vec<i16>,
    tables: std::rc::Rc<Tables>,
}

impl Roq {
    /// `CIN_PlayCinematic` for one file's bytes (`systemBits` as given).
    /// None for an invalid RoQ id (`RoQShutdown`).
    pub fn play(name: &str, data: Vec<u8>, extents: [f32; 4], bits: i32, now: i64) -> Option<Roq> {
        let tables = std::rc::Rc::new(Tables::new());
        let roq_size = data.len() as i64;
        if roq_size <= 0 {
            return None;
        }
        let mut r = Roq {
            name: name.to_string(),
            data,
            cursor: 0,
            file_base: 0,
            linbuf: vec![0; (DEFAULT_CIN_WIDTH * DEFAULT_CIN_HEIGHT * 4 * 2) as usize],
            q_status: [Vec::new(), Vec::new()],
            mcomp: [0; 256],
            old: None,
            vq2: vec![0; 256 * 4 * 4],
            vq4: vec![0; 256 * 16 * 4],
            vq8: vec![0; 256 * 64 * 4],
            cin_width: DEFAULT_CIN_WIDTH,
            cin_height: DEFAULT_CIN_HEIGHT,
            extents,
            looping: bits & CIN_LOOP != 0,
            hold_at_end: bits & CIN_HOLD != 0,
            dirty: true,
            alter_game_state: bits & CIN_SYSTEM != 0,
            silent: bits & CIN_SILENT != 0,
            shader: bits & CIN_SHADER != 0,
            status: Status::Eof,
            start_time: now,
            last_time: now,
            tfps: 0,
            roq_played: 0,
            roq_size,
            roq_frame_size: 0,
            on_quad: 0,
            num_quads: -1,
            samples_per_line: 0,
            roq_id: 0,
            screen_delta: 0,
            // initRoQ
            samples_per_pixel: 4,
            xsize: 0,
            ysize: 0,
            in_memory: 0,
            normal_buffer0: 0,
            roq_flags: 0,
            roq_f0: 0,
            roq_f1: 0,
            t: [0; 2],
            roq_fps: 30,
            buf: None,
            audio: Vec::new(),
            tables,
        };
        r.fs_read(16);
        let id = r.byte(0) as u32 + r.byte(1) as u32 * 256;
        if id != 0x1084 {
            // trFMV::play(), invalid RoQ ID
            return None;
        }
        r.roq_init(now);
        r.status = Status::Play;
        Some(r)
    }

    /// `FS_Read( cin.file, n, iFile )`: `cin.file` is the next n bytes.
    fn fs_read(&mut self, n: usize) {
        self.file_base = self.cursor;
        self.cursor = (self.cursor + n).min(self.data.len());
    }

    /// `cin.file[ i ]` (0 past the end of the file).
    fn byte(&self, i: usize) -> u8 {
        self.data.get(self.file_base + i).copied().unwrap_or(0)
    }

    /// `RoQ_init`.
    fn roq_init(&mut self, now: i64) {
        self.start_time = now;
        self.last_time = now;
        self.roq_played = 24;
        // get frame rate
        self.roq_fps = self.byte(6) as i64 + self.byte(7) as i64 * 256;
        if self.roq_fps == 0 {
            self.roq_fps = 30;
        }
        self.num_quads = -1;
        self.roq_id = self.byte(8) as u32 + self.byte(9) as u32 * 256;
        self.roq_frame_size = self.byte(10) as u32 + self.byte(11) as u32 * 256 + self.byte(12) as u32 * 65536;
        self.roq_flags = self.byte(14) as i64 + self.byte(15) as i64 * 256;
    }

    /// `RoQReset`: reopen the file and start over (status FMV_LOOPED).
    fn roq_reset(&mut self, now: i64) {
        self.cursor = 0;
        self.fs_read(16);
        self.roq_init(now);
        self.status = Status::Looped;
    }

    /// `readQuadInfo`.
    fn read_quad_info(&mut self, q: usize) {
        self.xsize = self.byte(q) as i64 + self.byte(q + 1) as i64 * 256;
        self.ysize = self.byte(q + 2) as i64 + self.byte(q + 3) as i64 * 256;
        self.cin_height = self.ysize;
        self.cin_width = self.xsize;
        self.samples_per_line = self.cin_width * self.samples_per_pixel;
        self.screen_delta = self.cin_height * self.samples_per_line;
        self.t = [self.screen_delta, -self.screen_delta];
        let need = (self.screen_delta * 2).max(0) as usize;
        if self.linbuf.len() < need {
            self.linbuf.resize(need, 0);
        }
    }

    /// `recurseQuad`.
    fn recurse_quad(&mut self, start_x: i64, start_y: i64, mut quad_size: i64, x_off: i64, y_off: i64) {
        let offset = self.screen_delta;
        let (lowx, lowy) = (0, 0);
        let bigx = self.xsize.min(self.cin_width);
        let bigy = self.ysize.min(self.cin_height);
        if start_x >= lowx && start_x + quad_size <= bigx && start_y + quad_size <= bigy && start_y >= lowy && quad_size <= MAXSIZE {
            let scroff = (start_y + ((self.cin_height - bigy) >> 1) + y_off) * self.samples_per_line + (start_x + x_off) * self.samples_per_pixel;
            let q = self.on_quad;
            if q < self.q_status[0].len() {
                self.q_status[0][q] = Some(scroff as usize);
                self.q_status[1][q] = Some((scroff + offset) as usize);
            }
            self.on_quad += 1;
        }
        if quad_size != MINSIZE {
            quad_size >>= 1;
            self.recurse_quad(start_x, start_y, quad_size, x_off, y_off);
            self.recurse_quad(start_x + quad_size, start_y, quad_size, x_off, y_off);
            self.recurse_quad(start_x, start_y + quad_size, quad_size, x_off, y_off);
            self.recurse_quad(start_x + quad_size, start_y + quad_size, quad_size, x_off, y_off);
        }
    }

    /// `setupQuad`.
    fn setup_quad(&mut self, x_off: i64, y_off: i64) {
        let key = (x_off, y_off, self.ysize, self.xsize);
        if self.old == Some(key) {
            return;
        }
        self.old = Some(key);
        let mut num = (self.xsize * self.ysize) / 16;
        num += num / 4;
        num += 64; // for overflow
        let n = num.max(64) as usize;
        self.q_status = [vec![None; n], vec![None; n]];
        self.on_quad = 0;
        let mut y = 0;
        while y < self.ysize {
            let mut x = 0;
            while x < self.xsize {
                self.recurse_quad(x, y, 16, x_off, y_off);
                x += 16;
            }
            y += 16;
        }
        // eoq
        for i in n - 64..n {
            self.q_status[0][i] = None;
            self.q_status[1][i] = None;
        }
    }

    /// `RoQPrepMcomp`.
    fn roq_prep_mcomp(&mut self, xoff: i64, yoff: i64) {
        let (mut i, mut j) = (self.samples_per_line, self.samples_per_pixel);
        if self.xsize == self.ysize * 4 {
            j += j;
            i += i;
        }
        for y in 0..16 {
            let temp2 = (y + yoff - 8) * i;
            for x in 0..16 {
                let temp = (x + xoff - 8) * j;
                self.mcomp[(x * 16 + y) as usize] = self.normal_buffer0 - (temp2 + temp);
            }
        }
    }

    /// `decodeCodeBook` (samplesPerPixel 4, normal height).
    fn decode_code_book(&mut self, input: usize, roq_flags: u16) {
        let (two, mut four) = if roq_flags == 0 {
            (256, 256)
        } else {
            let two = (roq_flags >> 8) as usize;
            (if two == 0 { 256 } else { two }, (roq_flags & 0xff) as usize)
        };
        four *= 2;
        let mut p = input;
        let tables = self.tables.clone();
        for i in 0..two {
            let y = [self.byte(p) as usize, self.byte(p + 1) as usize, self.byte(p + 2) as usize, self.byte(p + 3) as usize];
            let (cr, cb) = (self.byte(p + 4) as usize, self.byte(p + 5) as usize);
            p += 6;
            for (k, &yk) in y.iter().enumerate() {
                let px = tables.yuv_to_rgb24(yk, cr, cb);
                let o = (i * 4 + k) * 4;
                if o + 4 <= self.vq2.len() {
                    self.vq2[o..o + 4].copy_from_slice(&px);
                }
            }
        }
        // VQ2TO4 on 32-bit pixels: c (vq4) and d (vq8) run on across cells
        let (mut c, mut d) = (0usize, 0usize);
        for _ in 0..four {
            let mut a = self.byte(p) as usize * 4;
            let mut b = self.byte(p + 1) as usize * 4;
            p += 2;
            for _ in 0..2 {
                let px = |v: &Vec<u8>, k: usize| -> [u8; 4] { v.get(k * 4..k * 4 + 4).map(|s| [s[0], s[1], s[2], s[3]]).unwrap_or([0; 4]) };
                let (a0, a1, b0, b1) = (px(&self.vq2, a), px(&self.vq2, a + 1), px(&self.vq2, b), px(&self.vq2, b + 1));
                for v in [a0, a1, b0, b1] {
                    if c * 4 + 4 <= self.vq4.len() {
                        self.vq4[c * 4..c * 4 + 4].copy_from_slice(&v);
                    }
                    c += 1;
                }
                for v in [a0, a0, a1, a1, b0, b0, b1, b1, a0, a0, a1, a1, b0, b0, b1, b1] {
                    if d * 4 + 4 <= self.vq8.len() {
                        self.vq8[d * 4..d * 4 + 4].copy_from_slice(&v);
                    }
                    d += 1;
                }
                a += 2;
                b += 2;
            }
        }
    }

    /// blit8_32 / blit4_32 / blit2_32: `rows` rows of `width` bytes from a
    /// codebook into linbuf.
    fn blit(linbuf: &mut [u8], src: &[u8], so: usize, dst: usize, rows: usize, width: usize, spl: usize) {
        for r in 0..rows {
            let (s, d) = (so + r * width, dst + r * spl);
            if s + width <= src.len() && d + width <= linbuf.len() {
                linbuf[d..d + width].copy_from_slice(&src[s..s + width]);
            }
        }
    }

    /// move8_32 / move4_32: motion compensation inside linbuf.
    fn mv(linbuf: &mut [u8], src: i64, dst: usize, rows: usize, width: usize, spl: usize) {
        for r in 0..rows {
            let s = src + (r * spl) as i64;
            let d = dst + r * spl;
            if s >= 0 && (s as usize) + width <= linbuf.len() && d + width <= linbuf.len() {
                linbuf.copy_within(s as usize..s as usize + width, d);
            }
        }
    }

    /// `blitVQQuad32fs`.
    fn blit_vq_quad32fs(&mut self, which: usize, data: usize) {
        let spl = self.samples_per_line as usize;
        let status = std::mem::take(&mut self.q_status[which]);
        let mut data = data;
        let (mut newd, mut celdata, mut index) = (0u16, 0u16, 0usize);
        let st = |i: usize| status.get(i).copied().flatten();
        loop {
            if newd == 0 {
                newd = 7;
                celdata = self.byte(data) as u16 + self.byte(data + 1) as u16 * 256;
                data += 2;
            } else {
                newd -= 1;
            }
            let code = celdata & 0xc000;
            celdata <<= 2;
            match code {
                0x8000 => {
                    // vq code
                    if let Some(s) = st(index) {
                        let v = self.byte(data) as usize * 256;
                        Self::blit(&mut self.linbuf, &self.vq8, v, s, 8, 32, spl);
                    }
                    data += 1;
                    index += 5;
                }
                0xc000 => {
                    // drop: skip 8x8
                    index += 1;
                    for _ in 0..4 {
                        if newd == 0 {
                            newd = 7;
                            celdata = self.byte(data) as u16 + self.byte(data + 1) as u16 * 256;
                            data += 2;
                        } else {
                            newd -= 1;
                        }
                        let code = celdata & 0xc000;
                        celdata <<= 2;
                        let s = st(index);
                        match code {
                            0x8000 => {
                                // 4x4 vq code
                                if let Some(s) = s {
                                    let v = self.byte(data) as usize * 64;
                                    Self::blit(&mut self.linbuf, &self.vq4, v, s, 4, 16, spl);
                                }
                                data += 1;
                            }
                            0xc000 => {
                                // 2x2 vq codes
                                for off in [0, 8, spl * 2, spl * 2 + 8] {
                                    if let Some(s) = s {
                                        let v = self.byte(data) as usize * 16;
                                        Self::blit(&mut self.linbuf, &self.vq2, v, s + off, 2, 8, spl);
                                    }
                                    data += 1;
                                }
                            }
                            0x4000 => {
                                // motion compensation
                                if let Some(s) = s {
                                    let m = self.mcomp[self.byte(data) as usize];
                                    Self::mv(&mut self.linbuf, s as i64 + m, s, 4, 16, spl);
                                }
                                data += 1;
                            }
                            _ => {}
                        }
                        index += 1;
                    }
                }
                0x4000 => {
                    // motion compensation
                    if let Some(s) = st(index) {
                        let m = self.mcomp[self.byte(data) as usize];
                        Self::mv(&mut self.linbuf, s as i64 + m, s, 8, 32, spl);
                    }
                    data += 1;
                    index += 5;
                }
                _ => index += 5,
            }
            if st(index).is_none() {
                break;
            }
        }
        self.q_status[which] = status;
    }

    /// The end of the file: hold, loop or EOF (`RoQInterrupt`'s two checks).
    fn at_end(&mut self, now: i64) -> bool {
        if self.roq_played >= self.roq_size {
            if !self.hold_at_end {
                if self.looping {
                    self.roq_reset(now);
                } else {
                    self.status = Status::Eof;
                }
            } else {
                self.status = Status::Idle;
            }
            return true;
        }
        false
    }

    /// `RoQInterrupt`: decode the pending chunk, read the next one's header.
    fn roq_interrupt(&mut self, now: i64) {
        self.fs_read(self.roq_frame_size as usize + 8);
        if self.at_end(now) {
            return;
        }
        let mut framedata = 0usize;
        loop {
            // new frame is ready
            match self.roq_id {
                ROQ_QUAD_VQ => {
                    if self.num_quads & 1 != 0 {
                        self.normal_buffer0 = self.t[1];
                        self.roq_prep_mcomp(self.roq_f0, self.roq_f1);
                        self.blit_vq_quad32fs(1, framedata);
                        self.buf = Some(self.screen_delta as usize);
                    } else {
                        self.normal_buffer0 = self.t[0];
                        self.roq_prep_mcomp(self.roq_f0, self.roq_f1);
                        self.blit_vq_quad32fs(0, framedata);
                        self.buf = Some(0);
                    }
                    if self.num_quads == 0 {
                        // first frame
                        let n = (self.samples_per_line * self.ysize) as usize;
                        let d = self.screen_delta as usize;
                        if d + n <= self.linbuf.len() {
                            self.linbuf.copy_within(0..n, d);
                        }
                    }
                    self.num_quads += 1;
                    self.dirty = true;
                }
                ROQ_CODEBOOK => self.decode_code_book(framedata, self.roq_flags as u16),
                ZA_SOUND_MONO | ZA_SOUND_STEREO => {
                    if !self.silent {
                        let s = self.file_base + framedata;
                        let e = (s + self.roq_frame_size as usize).min(self.data.len());
                        let from = &self.data[s.min(e)..e];
                        let sqr = &self.tables.sqr;
                        if self.roq_id == ZA_SOUND_MONO {
                            rll_decode_mono_to_stereo(sqr, from, self.roq_flags as u16, &mut self.audio);
                        } else {
                            rll_decode_stereo_to_stereo(sqr, from, self.roq_flags as u16, &mut self.audio);
                        }
                    }
                }
                ROQ_QUAD_INFO => {
                    if self.num_quads == -1 {
                        self.read_quad_info(framedata);
                        self.setup_quad(0, 0);
                        self.start_time = now;
                        self.last_time = now;
                    }
                    if self.num_quads != 1 {
                        self.num_quads = 0;
                    }
                }
                ROQ_PACKET => {
                    self.in_memory = self.roq_flags;
                    // for header
                    self.roq_frame_size = 0;
                }
                ROQ_QUAD_HANG => self.roq_frame_size = 0,
                ROQ_QUAD_JPEG => {}
                _ => self.status = Status::Eof,
            }
            // read in next frame data
            if self.at_end(now) {
                return;
            }
            framedata += self.roq_frame_size as usize;
            self.roq_id = self.byte(framedata) as u32 + self.byte(framedata + 1) as u32 * 256;
            self.roq_frame_size = self.byte(framedata + 2) as u32 + self.byte(framedata + 3) as u32 * 256 + self.byte(framedata + 4) as u32 * 65536;
            self.roq_flags = self.byte(framedata + 6) as i64 + self.byte(framedata + 7) as i64 * 256;
            self.roq_f0 = self.byte(framedata + 7) as i8 as i64;
            self.roq_f1 = self.byte(framedata + 6) as i8 as i64;
            if self.roq_frame_size > 65536 || self.roq_id == 0x1084 {
                self.status = Status::Eof;
                if self.looping {
                    self.roq_reset(now);
                }
                return;
            }
            if self.in_memory != 0 && self.status != Status::Eof {
                self.in_memory -= 1;
                framedata += 8;
                continue;
            }
            break;
        }
        // one more frame hits the dust
        self.roq_played += self.roq_frame_size as i64 + 8;
    }

    /// `CIN_RunCinematic`: decode up to the frame due at `now` (ms,
    /// `CL_ScaledMilliseconds`). Returns the status; at FMV_EOF a looping
    /// video restarts and any other is done (`RoQShutdown`: Idle).
    pub fn run(&mut self, now: i64) -> Status {
        if self.status == Status::Eof || self.status == Status::Idle {
            return self.status;
        }
        let this_time = now;
        if self.shader && (this_time - self.last_time).abs() > 100 {
            self.start_time += this_time - self.last_time;
        }
        self.tfps = ((now - self.start_time) * 3) / 100;
        let mut start = self.start_time;
        let mut guard = 0;
        while self.tfps != self.num_quads && self.status == Status::Play {
            self.roq_interrupt(now);
            if start != self.start_time {
                self.tfps = ((now - self.start_time) * 3) / 100;
                start = self.start_time;
            }
            // (a file without VQ frames would spin forever in the C)
            guard += 1;
            if guard > 100_000 {
                self.status = Status::Eof;
            }
        }
        self.last_time = this_time;
        if self.status == Status::Looped {
            self.status = Status::Play;
        }
        if self.status == Status::Eof {
            if self.looping {
                self.roq_reset(now);
            } else {
                self.shutdown();
            }
        }
        self.status
    }

    /// `CL_PlayCinematic_f`'s wait for the first frame (the codebook and
    /// first sound chunks): decode chunks until a VQ frame is up.
    pub fn first_frame(&mut self, now: i64) {
        let mut guard = 0;
        while self.buf.is_none() && self.status == Status::Play && guard < 10_000 {
            self.roq_interrupt(now);
            guard += 1;
        }
        // the C spins until Sys_Milliseconds reaches the first frame's time
        // (tfps 1): the clock is where the first picture is due
        self.start_time = now - 34;
        self.last_time = now;
    }

    /// `RoQShutdown` / `CIN_StopCinematic`.
    pub fn shutdown(&mut self) {
        if self.status != Status::Idle {
            self.status = Status::Idle;
        }
    }

    /// True once the video has stopped (FMV_EOF / RoQShutdown).
    pub fn finished(&self) -> bool {
        matches!(self.status, Status::Idle | Status::Eof) && !self.hold_at_end
    }

    /// The frame `CIN_DrawCinematic` shows: (width, height, RGBA8 rows), and
    /// whether it changed since the last call (`dirty`).
    pub fn frame(&mut self) -> Option<(usize, usize, &[u8], bool)> {
        let b = self.buf?;
        let (w, h) = (self.cin_width.max(0) as usize, self.cin_height.max(0) as usize);
        let n = w * h * 4;
        if b + n > self.linbuf.len() {
            return None;
        }
        let dirty = std::mem::take(&mut self.dirty);
        Some((w, h, &self.linbuf[b..b + n], dirty))
    }
}

/// `CIN_PlayCinematic`'s name rule: a bare name is under `video/`.
pub fn cinematic_path(arg: &str) -> String {
    if arg.contains('/') || arg.contains('\\') {
        arg.to_string()
    } else {
        format!("video/{arg}")
    }
}

/// `CL_PlayCinematic_f`'s system bits for `cinematic <name> [1|2]`.
pub fn play_cinematic_bits(arg: &str, s: &str) -> i32 {
    let mut bits = CIN_SYSTEM;
    if s.starts_with('1') || arg.eq_ignore_ascii_case("demoend.roq") || arg.eq_ignore_ascii_case("end.roq") {
        bits |= CIN_HOLD;
    }
    if s.starts_with('2') {
        bits |= CIN_LOOP;
    }
    bits
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cin_yuv_tables_and_rll() {
        let t = Tables::new();
        // ROQ_YY_tab / UB / VR at the ends
        assert_eq!(t.yy[255], (255 << 6) | 63);
        assert_eq!(t.ub[0], (57.204f32 * -255.0 + 32.0) as i64);
        assert!(t.yuv_to_rgb24(0, 128, 128).iter().take(3).all(|&c| c <= 2));
        assert_eq!(t.yuv_to_rgb24(255, 128, 128)[3], 255);
        assert!(t.yuv_to_rgb24(255, 128, 128)[0] >= 250);
        assert_eq!(t.sqr[3], 9);
        assert_eq!(t.sqr[131], -9);
        let mut out = Vec::new();
        assert_eq!(rll_decode_stereo_to_stereo(&t.sqr, &[1, 2, 129, 130], 0x0000, &mut out), 2);
        assert_eq!(out, vec![1, 4, 0, 0]);
        out.clear();
        assert_eq!(rll_decode_mono_to_stereo(&t.sqr, &[2, 2], 10, &mut out), 2);
        assert_eq!(out, vec![14, 14, 18, 18]);
        assert_eq!(cinematic_path("splash.RoQ"), "video/splash.RoQ");
        assert_eq!(play_cinematic_bits("x.roq", "2"), CIN_SYSTEM | CIN_LOOP);
        assert_eq!(play_cinematic_bits("end.roq", ""), CIN_SYSTEM | CIN_HOLD);
    }

    #[test]
    fn cin_splash_decodes() {
        let data = crate::testdata::legacy().must_read("video/splash.roq");
        let total_sound: usize = {
            let mut p = 8;
            let mut n = 0;
            while p + 8 <= data.len() {
                let id = u16::from_le_bytes([data[p], data[p + 1]]) as u32;
                let sz = u32::from_le_bytes([data[p + 2], data[p + 3], data[p + 4], data[p + 5]]) as usize;
                if id == ZA_SOUND_STEREO {
                    n += sz;
                }
                p += 8 + sz;
            }
            n
        };
        let mut r = Roq::play("video/splash.roq", data, [0.0, 0.0, 640.0, 480.0], CIN_SYSTEM, 0).expect("roq");
        assert_eq!(r.roq_fps, 30);
        r.first_frame(0);
        assert!(r.frame().is_some(), "first frame up");
        assert_eq!((r.cin_width, r.cin_height), (512, 512));
        let mut t = 0;
        let mut lit = 0usize;
        while r.run(t) == Status::Play && t < 60_000 {
            if let Some((w, h, px, _)) = r.frame() {
                assert_eq!(px.len(), w * h * 4);
                lit = lit.max(px.chunks(4).filter(|p| p[0] as u32 + p[1] as u32 + p[2] as u32 > 60).count());
                if let (Ok(path), true) = (std::env::var("ROQ_DUMP"), (2000..2033).contains(&t)) {
                    // a binary PPM of the frame for eyeballing
                    let mut out = format!("P6 {w} {h} 255\n").into_bytes();
                    out.extend(px.chunks(4).flat_map(|p| [p[0], p[1], p[2]]));
                    let _ = std::fs::write(path, out);
                }
            }
            t += 33;
        }
        assert!(r.finished(), "plays to its end (status {:?})", r.status);
        assert!((3_000..6_000).contains(&t), "about 4 s at 30 fps: {t} ms");
        assert_eq!(r.num_quads, 121, "every VQ frame decoded");
        assert!(lit > 1000, "a mid-video frame has a picture: {lit} lit pixels");
        // S_RawSamples: every stereo byte pair becomes one L/R sample pair
        assert_eq!(r.audio.len(), total_sound);
    }
}
