//! Kernel `files` — the pure-server half of qcommon/files.c and md4.c
//! (dec037): `Com_BlockChecksum` (MD4, md4.c), `FS_LoadZipFile`'s two pak
//! checksums over the CRCs of the zip's central directory (the plain
//! `checksum` and the `pure_checksum` keyed with `fs_checksumFeed`), the
//! search path of pk3s per game directory (`FS_AddGameDirectory`'s order),
//! pak references (`FS_FOpenFileRead`'s FS_GENERAL_REF / FS_CGAME_REF /
//! FS_UI_REF), `FS_LoadedPakChecksums` / `Names` / `PureChecksums`,
//! `FS_ReferencedPakChecksums` / `Names` / `PureChecksums`,
//! `FS_PureServerSetLoadedPaks` / `SetReferencedPaks`, `FS_ComparePaks`,
//! `FS_CompareZipChecksum`, `FS_FileIsInPAK`, `FS_ClearPakReferences`,
//! `FS_Restart` and `FS_CheckDirTraversal`.
//!
//! Only the zip's central directory is read (names, sizes, CRCs); the
//! contents stay with the host's own pk3 reader (Godot's ZipReader, the
//! dedicated example's zip code). Checked against the original unzip.c +
//! md4.c in `re/harness/pak_ref.c` (re/traces/pak.sha256, tests/pure.rs).

use std::path::{Path, PathBuf};

/// `BASEGAME`.
pub const BASEGAME: &str = "base";
/// `MAX_SEARCH_PATHS`.
pub const MAX_SEARCH_PATHS: usize = 4096;
/// Reference flags (`FS_GENERAL_REF`, `FS_UI_REF`, `FS_CGAME_REF`).
pub const FS_GENERAL_REF: i32 = 0x01;
pub const FS_UI_REF: i32 = 0x02;
pub const FS_CGAME_REF: i32 = 0x04;

/// The files that stand for the client modules in the pure checks
/// (SV_VerifyPaks_f's `FS_FileIsInPAK( "vm/cgame.qvm" )`, the client's
/// FS_CGAME_REF / FS_UI_REF): the qvm when a pak holds one (an original
/// install's vms pk3), else the module's first data file, since the port's
/// cgame and ui are built in (dec037).
pub const CGAME_MODULE: [&str; 2] = ["vm/cgame.qvm", "ui/hud.txt"];
pub const UI_MODULE: [&str; 2] = ["vm/ui.qvm", "ui/menus.txt"];
/// The server's game module (`VM_Create( "qagame" )` reads vm/game.qvm).
pub const GAME_MODULE: [&str; 1] = ["vm/game.qvm"];

// ===================================================================== md4

/// md4.c `struct mdfour`.
struct Md4 {
    a: u32,
    b: u32,
    c: u32,
    d: u32,
    total_n: u32,
}

impl Md4 {
    /// `mdfour64`: one 64-byte block.
    fn block(&mut self, x: &[u32; 16]) {
        let f = |x: u32, y: u32, z: u32| (x & y) | (!x & z);
        let g = |x: u32, y: u32, z: u32| (x & y) | (x & z) | (y & z);
        let h = |x: u32, y: u32, z: u32| x ^ y ^ z;
        let (mut a, mut b, mut c, mut d) = (self.a, self.b, self.c, self.d);
        let (aa, bb, cc, dd) = (a, b, c, d);
        for &i in &[0usize, 4, 8, 12] {
            a = a.wrapping_add(f(b, c, d)).wrapping_add(x[i]).rotate_left(3);
            d = d.wrapping_add(f(a, b, c)).wrapping_add(x[i + 1]).rotate_left(7);
            c = c.wrapping_add(f(d, a, b)).wrapping_add(x[i + 2]).rotate_left(11);
            b = b.wrapping_add(f(c, d, a)).wrapping_add(x[i + 3]).rotate_left(19);
        }
        for &i in &[0usize, 1, 2, 3] {
            a = a.wrapping_add(g(b, c, d)).wrapping_add(x[i]).wrapping_add(0x5a82_7999).rotate_left(3);
            d = d.wrapping_add(g(a, b, c)).wrapping_add(x[i + 4]).wrapping_add(0x5a82_7999).rotate_left(5);
            c = c.wrapping_add(g(d, a, b)).wrapping_add(x[i + 8]).wrapping_add(0x5a82_7999).rotate_left(9);
            b = b.wrapping_add(g(c, d, a)).wrapping_add(x[i + 12]).wrapping_add(0x5a82_7999).rotate_left(13);
        }
        for &i in &[0usize, 2, 1, 3] {
            a = a.wrapping_add(h(b, c, d)).wrapping_add(x[i]).wrapping_add(0x6ed9_eba1).rotate_left(3);
            d = d.wrapping_add(h(a, b, c)).wrapping_add(x[i + 8]).wrapping_add(0x6ed9_eba1).rotate_left(9);
            c = c.wrapping_add(h(d, a, b)).wrapping_add(x[i + 4]).wrapping_add(0x6ed9_eba1).rotate_left(11);
            b = b.wrapping_add(h(c, d, a)).wrapping_add(x[i + 12]).wrapping_add(0x6ed9_eba1).rotate_left(15);
        }
        self.a = a.wrapping_add(aa);
        self.b = b.wrapping_add(bb);
        self.c = c.wrapping_add(cc);
        self.d = d.wrapping_add(dd);
    }

    /// `copy64`.
    fn copy64(input: &[u8]) -> [u32; 16] {
        let mut m = [0u32; 16];
        for (i, w) in m.iter_mut().enumerate() {
            *w = u32::from_le_bytes([input[i * 4], input[i * 4 + 1], input[i * 4 + 2], input[i * 4 + 3]]);
        }
        m
    }

    /// `mdfour_tail`: the padding with the length as 32 bits (`b =
    /// m->totalN * 8`, copied with `copy4`).
    fn tail(&mut self, input: &[u8]) {
        let n = input.len();
        self.total_n = self.total_n.wrapping_add(n as u32);
        let b = self.total_n.wrapping_mul(8);
        let mut buf = [0u8; 128];
        buf[..n].copy_from_slice(input);
        buf[n] = 0x80;
        if n <= 55 {
            buf[56..60].copy_from_slice(&b.to_le_bytes());
            self.block(&Self::copy64(&buf[..64]));
        } else {
            buf[120..124].copy_from_slice(&b.to_le_bytes());
            self.block(&Self::copy64(&buf[..64]));
            self.block(&Self::copy64(&buf[64..]));
        }
    }
}

/// md4.c `mdfour` (`mdfour_begin`, `mdfour_update`, `mdfour_result`),
/// including its quirk of running the tail twice for an empty input.
pub fn mdfour(input: &[u8]) -> [u8; 16] {
    let mut m = Md4 { a: 0x6745_2301, b: 0xefcd_ab89, c: 0x98ba_dcfe, d: 0x1032_5476, total_n: 0 };
    let mut rest = input;
    if rest.is_empty() {
        m.tail(rest);
    }
    while rest.len() >= 64 {
        m.block(&Md4::copy64(&rest[..64]));
        rest = &rest[64..];
        m.total_n = m.total_n.wrapping_add(64);
    }
    m.tail(rest);
    let mut out = [0u8; 16];
    for (i, v) in [m.a, m.b, m.c, m.d].iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
    }
    out
}

/// `Com_BlockChecksum`: the four digest words xor'ed.
pub fn block_checksum(data: &[u8]) -> u32 {
    let d = mdfour(data);
    let w = |i: usize| u32::from_le_bytes([d[i * 4], d[i * 4 + 1], d[i * 4 + 2], d[i * 4 + 3]]);
    w(0) ^ w(1) ^ w(2) ^ w(3)
}

// ===================================================================== zip

/// One entry of a zip's central directory (`unz_file_info` subset).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ZipEntry {
    pub name: String,
    pub crc: u32,
    pub compressed_size: u32,
    pub uncompressed_size: u32,
}

/// unzip.c `unzOpen` + `unzGetCurrentFileInfo` over every entry: the
/// central directory in its order (`unzGoToFirstFile` / `unzGoToNextFile`).
pub fn zip_directory(data: &[u8]) -> Option<Vec<ZipEntry>> {
    // unzlocal_SearchCentralDir: the end of central directory record, in
    // the last 0xffff + 22 bytes
    if data.len() < 22 {
        return None;
    }
    let lo = data.len().saturating_sub(0xffff + 22);
    let mut eocd = None;
    let mut i = data.len() - 22;
    loop {
        if data[i..i + 4] == [0x50, 0x4b, 0x05, 0x06] {
            eocd = Some(i);
            break;
        }
        if i == lo {
            break;
        }
        i -= 1;
    }
    let e = eocd?;
    let u16at = |p: usize| u16::from_le_bytes([data[p], data[p + 1]]) as usize;
    let u32at = |p: usize| u32::from_le_bytes([data[p], data[p + 1], data[p + 2], data[p + 3]]);
    let number_entry = u16at(e + 10);
    let size_central_dir = u32at(e + 12) as usize;
    let offset_central_dir = u32at(e + 16) as usize;
    // byte_before_the_zipfile (self-extracting archives)
    let before = e.checked_sub(offset_central_dir + size_central_dir)?;
    let mut p = offset_central_dir + before;
    let mut out = Vec::with_capacity(number_entry);
    for _ in 0..number_entry {
        if p + 46 > data.len() || u32at(p) != 0x0201_4b50 {
            break; // UNZ_BADZIPFILE: FS_LoadZipFile stops at the error
        }
        let crc = u32at(p + 16);
        let compressed_size = u32at(p + 20);
        let uncompressed_size = u32at(p + 24);
        let name_len = u16at(p + 28);
        let extra_len = u16at(p + 30);
        let comment_len = u16at(p + 32);
        let name_end = (p + 46 + name_len).min(data.len());
        let name = String::from_utf8_lossy(&data[p + 46..name_end]).into_owned();
        out.push(ZipEntry { name, crc, compressed_size, uncompressed_size });
        p += 46 + name_len + extra_len + comment_len;
    }
    Some(out)
}

/// The central directory of a zip file on disk (only its tail is read).
pub fn zip_directory_file(path: &Path) -> Option<Vec<ZipEntry>> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    // read the tail for the end record, then the directory it points at
    let tail = len.min(0xffff + 22);
    f.seek(SeekFrom::Start(len - tail)).ok()?;
    let mut t = vec![0u8; tail as usize];
    f.read_exact(&mut t).ok()?;
    let mut e = None;
    if t.len() >= 22 {
        let mut i = t.len() - 22;
        loop {
            if t[i..i + 4] == [0x50, 0x4b, 0x05, 0x06] {
                e = Some(i);
                break;
            }
            if i == 0 {
                break;
            }
            i -= 1;
        }
    }
    let e = e?;
    let size = u32::from_le_bytes([t[e + 12], t[e + 13], t[e + 14], t[e + 15]]) as u64;
    let eocd_pos = len - tail + e as u64;
    let start = eocd_pos.checked_sub(size)?;
    f.seek(SeekFrom::Start(start)).ok()?;
    let mut buf = vec![0u8; (len - start) as usize];
    f.read_exact(&mut buf).ok()?;
    // rebase: the directory starts at 0 of `buf` (offset fixed below)
    let mut fixed = buf.clone();
    let off_pos = (eocd_pos - start) as usize + 16;
    fixed[off_pos..off_pos + 4].copy_from_slice(&0u32.to_le_bytes());
    zip_directory(&fixed)
}

// ===================================================================== paks

/// `pack_t` (the checksum part).
#[derive(Clone, Debug)]
pub struct Pak {
    /// `pakFilename`: the full path
    pub path: PathBuf,
    /// `pakBasename`: the file name without ".pk3"
    pub basename: String,
    /// `pakGamename`: the game directory ("base")
    pub gamename: String,
    /// lowercased file names (`Q_strlwr` in FS_LoadZipFile)
    pub names: Vec<String>,
    /// the CRCs of the entries with data (`uncompressed_size > 0`)
    pub crcs: Vec<u32>,
    /// `checksum`: Com_BlockChecksum over the CRCs
    pub checksum: i32,
    /// `referenced`: FS_*_REF flags
    pub referenced: i32,
}

impl Pak {
    /// `FS_LoadZipFile` from a central directory.
    pub fn from_entries(path: &Path, gamename: &str, entries: &[ZipEntry]) -> Pak {
        let file = path.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
        // strip .pk3 if needed
        let basename = if file.len() > 4 && file[file.len() - 4..].eq_ignore_ascii_case(".pk3") { file[..file.len() - 4].to_string() } else { file };
        let crcs: Vec<u32> = entries.iter().filter(|e| e.uncompressed_size > 0).map(|e| e.crc).collect();
        let mut bytes = Vec::with_capacity(crcs.len() * 4);
        for c in &crcs {
            bytes.extend_from_slice(&c.to_le_bytes());
        }
        Pak {
            path: path.to_path_buf(),
            basename,
            gamename: gamename.to_string(),
            names: entries.iter().map(|e| e.name.to_lowercase()).collect(),
            crcs,
            checksum: block_checksum(&bytes) as i32,
            referenced: 0,
        }
    }

    /// `FS_LoadZipFile` of a pk3 on disk.
    pub fn load(path: &Path, gamename: &str) -> Option<Pak> {
        zip_directory_file(path).map(|e| Pak::from_entries(path, gamename, &e))
    }

    /// `pure_checksum`: Com_BlockChecksum over the feed and the CRCs.
    pub fn pure_checksum(&self, feed: i32) -> i32 {
        let mut bytes = Vec::with_capacity(self.crcs.len() * 4 + 4);
        bytes.extend_from_slice(&feed.to_le_bytes());
        for c in &self.crcs {
            bytes.extend_from_slice(&c.to_le_bytes());
        }
        block_checksum(&bytes) as i32
    }

    pub fn contains(&self, name: &str) -> bool {
        let l = name.to_lowercase();
        self.names.iter().any(|n| *n == l)
    }
}

/// `FS_CheckDirTraversal`.
pub fn check_dir_traversal(checkdir: &str) -> bool {
    checkdir.contains("../") || checkdir.contains("..\\")
}

/// Longest downloaded pak name accepted ("<dir>/<name>.pk3" as the server
/// names it, with the extension).
pub const MAX_DOWNLOAD_NAME_LEN: usize = 128;

/// Whether a downloaded pak's name is safe to turn into a path below the
/// download directory: a relative "<dir>/<name>.pk3", only
/// `[A-Za-z0-9_.+-]` and '/', no empty component, none starting (".", "..", hidden) or
/// ending in '.', no Windows device name, and a lower-case ".pk3" ending.
/// Everything else (absolute paths, drive letters, UNC, '\\', ':', '@',
/// NUL and control characters) is refused. This is the check of the
/// server-supplied name; `download_dest` is the one place that uses it.
pub fn safe_download_name(local: &str) -> bool {
    if local.len() > MAX_DOWNLOAD_NAME_LEN || !local.ends_with(".pk3") {
        return false;
    }
    if !local
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'+' | b'/'))
    {
        return false;
    }
    let mut parts = local.split('/').peekable();
    while let Some(c) = parts.next() {
        if c.is_empty() || c.starts_with('.') || c.ends_with('.') {
            return false;
        }
        // Windows opens CON, NUL, COM1 ... (with any extension) as devices
        let stem = c.split('.').next().unwrap_or("").to_ascii_uppercase();
        let device = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            || ((stem.starts_with("COM") || stem.starts_with("LPT"))
                && stem.len() == 4
                && stem.as_bytes()[3].is_ascii_digit());
        if device {
            return false;
        }
        // the file name needs a stem before ".pk3"
        if parts.peek().is_none() && c.len() <= 4 {
            return false;
        }
    }
    true
}

/// `safe_download_name` of "<name>.pk3", for a name as `sv_referencedPakNames`
/// lists it (no extension).
pub fn safe_pak_name(name: &str) -> bool {
    safe_download_name(&format!("{name}.pk3"))
}

/// The path a download of `local` is written to: `local` below `home`, or
/// None when the name is unsafe or any component of it is not a plain
/// name (so the join cannot replace or leave `home`).
pub fn download_dest(home: &Path, local: &str) -> Option<PathBuf> {
    if !safe_download_name(local) {
        return None;
    }
    let rel = Path::new(local);
    if !rel
        .components()
        .all(|c| matches!(c, std::path::Component::Normal(_)))
    {
        return None;
    }
    Some(home.join(rel))
}

/// `FS_IsExt` list of FS_FOpenFileRead: files that do not reference a pak.
fn unreferencing_ext(name: &str) -> bool {
    let l = name.to_lowercase();
    [".shader", ".txt", ".cfg", ".config", ".bot", ".arena", ".menu"].iter().any(|e| l.ends_with(e))
}

/// The pk3 search path with the pure state (`fs_searchpaths`,
/// `fs_checksumFeed`, `fs_serverPaks`, `fs_serverReferencedPaks`).
#[derive(Clone, Debug, Default)]
pub struct PakSet {
    /// game directories, lowest priority first: (path, gamename)
    pub dirs: Vec<(PathBuf, String)>,
    /// search order: the first pak is searched first
    pub paks: Vec<Pak>,
    /// `fs_checksumFeed`
    pub feed: i32,
    /// `fs_serverPaks` (empty: not connected to a pure server)
    pub server_paks: Vec<i32>,
    pub server_pak_names: Vec<String>,
    /// `fs_serverReferencedPaks` / `fs_serverReferencedPakNames`
    pub server_referenced_paks: Vec<i32>,
    pub server_referenced_pak_names: Vec<String>,
    /// `fs_reordered`
    pub reordered: bool,
}

impl PakSet {
    /// `FS_Startup` over game directories given lowest priority first
    /// (basepath, then homepath, like `FS_AddGameDirectory`'s calls).
    pub fn new(dirs: &[(PathBuf, String)]) -> PakSet {
        let mut s = PakSet { dirs: dirs.to_vec(), ..Default::default() };
        s.scan();
        s
    }

    /// A search path from the hosts' pk3 lists (every pk3 of their
    /// directories, highest priority directory first): the directories.
    pub fn from_files(paths: &[String]) -> PakSet {
        let mut dirs: Vec<(PathBuf, String)> = Vec::new();
        for p in paths {
            let dir = Path::new(p).parent().map(|d| d.to_path_buf()).unwrap_or_default();
            if !dirs.iter().any(|(d, _)| *d == dir) {
                // pakGamename: the folder name ("base")
                let game = dir.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_else(|| BASEGAME.into());
                dirs.push((dir, game));
            }
        }
        dirs.reverse();
        PakSet::new(&dirs)
    }

    /// `FS_AddGameDirectory` for every directory: its pk3s sorted, each
    /// put at the head of the search path.
    fn scan(&mut self) {
        let old: Vec<Pak> = std::mem::take(&mut self.paks);
        for (dir, game) in self.dirs.clone() {
            let Ok(rd) = std::fs::read_dir(&dir) else { continue };
            let mut names: Vec<String> = rd.filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().into_owned()).filter(|n| n.len() > 4 && n[n.len() - 4..].eq_ignore_ascii_case(".pk3")).collect();
            // FS_SortFileList: FS_PathCmp order
            names.sort_by(|a, b| path_cmp(a, b));
            for n in names {
                let path = dir.join(&n);
                // reuse a pak already read (same size and mtime would be
                // safer; the C re-reads, but the CRCs cannot change under us
                // except by a download, which writes a new file)
                let reuse = old.iter().find(|p| p.path == path).cloned();
                let pak = match reuse {
                    Some(mut p) => {
                        p.referenced = 0;
                        p
                    }
                    None => match Pak::load(&path, &game) {
                        Some(p) => p,
                        None => continue,
                    },
                };
                self.paks.insert(0, pak);
            }
        }
        self.reorder_pure_paks();
    }

    /// `FS_Restart( checksumFeed )`: rescan the directories (a download
    /// may have added a pk3), forget the references, set the feed.
    pub fn restart(&mut self, feed: i32) {
        self.feed = feed;
        self.scan();
        self.clear_pak_references(0);
    }

    /// `fs_game` (and the other game folders a pure server's paks come
    /// from, e.g. GrangerHub's "trem13" + "gpp"): those folders under
    /// fs_homepath join the search path before base, like FS_Startup's
    /// fs_game pass, so the paks the server made us download are found.
    /// True when the search path changed (the caller restarts it).
    pub fn set_games(&mut self, home: &Path, games: &[String]) -> bool {
        let mut want: Vec<(PathBuf, String)> = Vec::new();
        for g in games {
            let g = g.trim();
            let valid = !g.is_empty() && !g.eq_ignore_ascii_case(BASEGAME) && !g.contains(['/', '\\', ':']) && g != "." && g != "..";
            if valid && !want.iter().any(|(_, w)| w.eq_ignore_ascii_case(g)) {
                want.push((home.join(g), g.to_string()));
            }
        }
        let cur: Vec<(PathBuf, String)> = self.dirs.iter().filter(|(_, g)| !g.eq_ignore_ascii_case(BASEGAME)).cloned().collect();
        if cur == want {
            return false;
        }
        self.dirs.retain(|(_, g)| g.eq_ignore_ascii_case(BASEGAME));
        self.dirs.extend(want);
        true
    }

    /// The game folders of the server's referenced paks ("gpp/map-x" -> "gpp").
    pub fn referenced_games(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for n in &self.server_referenced_pak_names {
            if let Some((g, _)) = n.replace('\\', "/").split_once('/') {
                if !out.iter().any(|o| o.eq_ignore_ascii_case(g)) {
                    out.push(g.to_string());
                }
            }
        }
        out
    }

    /// The files of the paks of the fs_game folder (the host's asset search path).
    pub fn game_pak_paths(&self) -> Vec<String> {
        self.paks.iter().filter(|p| !p.gamename.eq_ignore_ascii_case(BASEGAME)).map(|p| p.path.to_string_lossy().into_owned()).collect()
    }

    /// `FS_ConditionalRestart` (without the fs_game switch).
    pub fn conditional_restart(&mut self, feed: i32) {
        if feed != self.feed {
            self.restart(feed);
        } else if !self.server_paks.is_empty() && !self.reordered {
            self.reorder_pure_paks();
        }
    }

    /// `FS_ReorderPurePaks`: the server's paks first, in its order.
    fn reorder_pure_paks(&mut self) {
        self.reordered = false;
        if self.server_paks.is_empty() {
            return;
        }
        let mut insert = 0usize;
        for sp in self.server_paks.clone() {
            if let Some(j) = (insert..self.paks.len()).find(|&j| self.paks[j].checksum == sp) {
                self.reordered = true;
                let p = self.paks.remove(j);
                self.paks.insert(insert, p);
                insert += 1;
            }
        }
    }

    /// The paks a pure server lets this client use (`fs_serverPaks`), or
    /// all of them.
    pub fn allowed(&self, p: &Pak) -> bool {
        self.server_paks.is_empty() || self.server_paks.contains(&p.checksum)
    }

    /// The pk3 paths in effect, lowest priority first (the order of the
    /// hosts' lists, where a later pk3 overrides an earlier one).
    pub fn effective_paths(&self) -> Vec<String> {
        self.paks.iter().rev().filter(|p| self.allowed(p)).map(|p| p.path.to_string_lossy().into_owned()).collect()
    }

    /// `FS_FOpenFileRead`'s search: the first allowed pak holding `name`.
    pub fn find(&self, name: &str) -> Option<usize> {
        let l = name.to_lowercase();
        self.paks.iter().position(|p| self.allowed(p) && p.names.iter().any(|n| *n == l))
    }

    /// The reference marking of `FS_FOpenFileRead`: true when found.
    pub fn reference(&mut self, name: &str) -> bool {
        let Some(i) = self.find(name) else { return false };
        let p = &mut self.paks[i];
        if p.referenced & FS_GENERAL_REF == 0 && !unreferencing_ext(name) {
            p.referenced |= FS_GENERAL_REF;
        }
        let l = name.to_lowercase();
        if l.contains("cgame.qvm") {
            p.referenced |= FS_CGAME_REF;
        }
        if l.contains("ui.qvm") {
            p.referenced |= FS_UI_REF;
        }
        true
    }

    /// The first file of `module` some allowed pak holds.
    pub fn module_file(&self, module: &[&'static str]) -> Option<&'static str> {
        module.iter().copied().find(|f| self.find(f).is_some())
    }

    /// Loading a module (`VM_Create`): its pak is referenced as the qvm's
    /// would be (FS_GENERAL_REF, a qvm is not an excluded extension) with
    /// `flag` (FS_CGAME_REF / FS_UI_REF).
    pub fn reference_module(&mut self, module: &[&'static str], flag: i32) -> bool {
        let Some(f) = self.module_file(module) else { return false };
        if let Some(i) = self.find(f) {
            self.paks[i].referenced |= FS_GENERAL_REF | flag;
        }
        true
    }

    /// `FS_FileIsInPAK`: the pure checksum of the pak holding `name`.
    pub fn file_is_in_pak(&self, name: &str) -> Option<i32> {
        if check_dir_traversal(name) {
            return None;
        }
        self.find(name).map(|i| self.paks[i].pure_checksum(self.feed))
    }

    /// `FS_ClearPakReferences` (0: all flags).
    pub fn clear_pak_references(&mut self, flags: i32) {
        let flags = if flags == 0 { -1 } else { flags };
        for p in &mut self.paks {
            p.referenced &= !flags;
        }
    }

    /// `FS_LoadedPakChecksums`.
    pub fn loaded_pak_checksums(&self) -> String {
        self.paks.iter().map(|p| format!("{} ", p.checksum)).collect()
    }

    /// `FS_LoadedPakNames`.
    pub fn loaded_pak_names(&self) -> String {
        self.paks.iter().map(|p| p.basename.as_str()).collect::<Vec<_>>().join(" ")
    }

    /// `FS_LoadedPakPureChecksums`.
    pub fn loaded_pak_pure_checksums(&self) -> String {
        self.paks.iter().map(|p| format!("{} ", p.pure_checksum(self.feed))).collect()
    }

    fn is_referenced(p: &Pak) -> bool {
        p.referenced != 0 || !p.gamename.to_lowercase().starts_with(BASEGAME)
    }

    /// `FS_ReferencedPakChecksums`.
    pub fn referenced_pak_checksums(&self) -> String {
        self.paks.iter().filter(|p| Self::is_referenced(p)).map(|p| format!("{} ", p.checksum)).collect()
    }

    /// `FS_ReferencedPakNames`: "game/basename" of the referenced paks.
    pub fn referenced_pak_names(&self) -> String {
        self.paks.iter().filter(|p| Self::is_referenced(p)).map(|p| format!("{}/{}", p.gamename, p.basename)).collect::<Vec<_>>().join(" ")
    }

    /// `FS_ReferencedPakPureChecksums`: "cgame ui @ ref1 ref2 ... n".
    pub fn referenced_pak_pure_checksums(&self) -> String {
        let mut info = String::new();
        let mut checksum = self.feed;
        let mut num_paks = 0;
        let mut flags = FS_CGAME_REF;
        while flags != 0 {
            if flags & FS_GENERAL_REF != 0 {
                // add a delimter between must haves and general refs
                info.push_str("@ ");
            }
            for p in &self.paks {
                // is the element a pak file and has it been referenced based on flag?
                if p.referenced & flags != 0 {
                    let pc = p.pure_checksum(self.feed);
                    info.push_str(&format!("{pc} "));
                    if flags & (FS_CGAME_REF | FS_UI_REF) != 0 {
                        break;
                    }
                    checksum ^= pc;
                    num_paks += 1;
                }
            }
            flags >>= 1;
        }
        // last checksum is the encoded number of referenced pk3s
        checksum ^= num_paks;
        info.push_str(&format!("{checksum} "));
        info
    }

    /// `FS_PureServerSetLoadedPaks`.
    pub fn pure_server_set_loaded_paks(&mut self, sums: &str, names: &str) {
        let c: Vec<i32> = sums.split_whitespace().take(MAX_SEARCH_PATHS).map(super::spawn::c_atoi).collect();
        self.server_paks = c;
        if self.server_paks.is_empty() && self.reordered {
            // force a restart to make sure the search order will be correct
            let f = self.feed;
            self.restart(f);
            return;
        }
        self.server_pak_names = names.split_whitespace().take(MAX_SEARCH_PATHS).map(|s| s.to_string()).collect();
    }

    /// `FS_PureServerSetReferencedPaks`.
    pub fn pure_server_set_referenced_paks(&mut self, sums: &str, names: &str) {
        let mut c: Vec<i32> = sums.split_whitespace().take(MAX_SEARCH_PATHS).map(super::spawn::c_atoi).collect();
        let mut n: Vec<String> = names.split_whitespace().map(|s| s.to_string()).collect();
        n.truncate(c.len());
        // ensure that there are as many checksums as there are pak names.
        c.truncate(n.len());
        self.server_referenced_paks = c;
        self.server_referenced_pak_names = n;
    }

    /// `FS_ComparePaks`: the referenced paks this client lacks, as the
    /// "@remote@local..." download list (`dlstring`) or a readable list;
    /// None when nothing is missing. `home_exists` is `FS_SV_FileExists`.
    pub fn compare_paks(&self, dlstring: bool, home_exists: &dyn Fn(&str) -> bool) -> Option<String> {
        if self.server_referenced_paks.is_empty() {
            return None; // Server didn't send any pack information along
        }
        let mut needed = String::new();
        for (sum, name) in self
            .server_referenced_paks
            .iter()
            .zip(self.server_referenced_pak_names.iter())
        {
            // Make sure the server cannot make us write to non-quake3 directories
            // (the original's ".." test is a subset of this).
            if check_dir_traversal(name) || !safe_pak_name(name) {
                continue;
            }
            let havepak = self.paks.iter().any(|p| p.checksum == *sum);
            if havepak || name.is_empty() {
                continue;
            }
            if dlstring {
                // Remote name
                needed.push_str(&format!("@{name}.pk3"));
                // Local name: do we have one with the same name?
                if home_exists(&format!("{name}.pk3")) {
                    // We already have one called this, we need to download it to another name
                    needed.push_str(&format!("@{name}.{:08x}.pk3", *sum as u32));
                } else {
                    needed.push_str(&format!("@{name}.pk3"));
                }
            } else {
                needed.push_str(&format!("{name}.pk3"));
                if home_exists(&format!("{name}.pk3")) {
                    needed.push_str(" (local file exists with wrong checksum)");
                }
                needed.push('\n');
            }
        }
        (!needed.is_empty()).then_some(needed)
    }

    /// `FS_SV_FOpenFileRead` of "game/name.pk3" (a download): the bytes of
    /// the pak of that game folder and file name.
    pub fn sv_read_pak(&self, name: &str) -> Option<Vec<u8>> {
        let n = name.replace('\\', "/");
        let (game, file) = n.rsplit_once('/')?;
        let p = self.paks.iter().find(|p| p.gamename.eq_ignore_ascii_case(game) && p.path.file_name().is_some_and(|f| f.to_string_lossy().eq_ignore_ascii_case(file)))?;
        std::fs::read(&p.path).ok()
    }

    /// `FS_CompareZipChecksum`: a downloaded zip is one the server referenced.
    pub fn compare_zip_checksum(&self, zipfile: &Path) -> bool {
        match Pak::load(zipfile, BASEGAME) {
            Some(p) => self.server_referenced_paks.contains(&p.checksum),
            None => false,
        }
    }
}

/// `FS_PathCmp`: case-insensitive, '/' and '\\' equal, as `FS_SortFileList`.
fn path_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let norm = |s: &str| s.to_lowercase().replace('\\', "/");
    norm(a).cmp(&norm(b))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// md4.c over the RFC 1320 vectors, as Com_BlockChecksum (the values
    /// re/harness/pak_ref prints).
    #[test]
    fn block_checksum_matches_md4_c() {
        assert_eq!(block_checksum(b""), 2556995263);
        assert_eq!(block_checksum(b"a"), 720146015);
        assert_eq!(block_checksum(b"abc"), 1570836014);
        assert_eq!(block_checksum(b"message digest"), 618399556);
        assert_eq!(block_checksum(b"abcdefghijklmnopqrstuvwxyz"), 929550956);
        // the full digest of RFC 1320 for "abc"
        let d = mdfour(b"abc");
        assert_eq!(d.iter().map(|b| format!("{b:02x}")).collect::<String>(), "a448017aaf21d8525fc10ae87aa6729d");
    }

    fn pak(crcs: &[u32], name: &str) -> Pak {
        let entries: Vec<ZipEntry> = crcs.iter().enumerate().map(|(i, c)| ZipEntry { name: format!("f{i}"), crc: *c, compressed_size: 1, uncompressed_size: 1 }).collect();
        Pak::from_entries(Path::new(&format!("/x/{name}.pk3")), "base", &entries)
    }

    #[test]
    fn referenced_pure_checksums_layout() {
        let mut s = PakSet { feed: 77, ..Default::default() };
        let mut a = pak(&[1, 2, 3], "data");
        a.names.push("vm/cgame.qvm".into());
        a.names.push("vm/ui.qvm".into());
        let b = pak(&[4, 5], "map");
        s.paks = vec![b, a];
        s.reference("vm/cgame.qvm");
        s.reference("vm/ui.qvm");
        s.reference("f0");
        let (pa, pb) = (s.paks[1].pure_checksum(77), s.paks[0].pure_checksum(77));
        // "cgame ui @ refs... (feed ^ refs ^ count)"
        let want = format!("{pa} {pa} @ {pb} {pa} {} ", 77 ^ pb ^ pa ^ 2);
        assert_eq!(s.referenced_pak_pure_checksums(), want);
        // names are game/basename, references include both
        assert_eq!(s.referenced_pak_names(), "base/map base/data");
    }

    #[test]
    fn compare_paks_builds_download_list() {
        let mut s = PakSet::default();
        s.paks = vec![pak(&[9], "data")];
        let mine = s.paks[0].checksum;
        s.pure_server_set_referenced_paks(&format!("{mine} 12345 99"), "base/data base/map-x ../evil");
        assert_eq!(s.server_referenced_paks.len(), 3);
        assert_eq!(s.compare_paks(true, &|_| false).as_deref(), Some("@base/map-x.pk3@base/map-x.pk3"));
        assert_eq!(s.compare_paks(true, &|n| n == "base/map-x.pk3").as_deref(), Some(&*format!("@base/map-x.pk3@base/map-x.{:08x}.pk3", 12345)));
        assert_eq!(s.compare_paks(false, &|_| true).as_deref(), Some("base/map-x.pk3 (local file exists with wrong checksum)\n"));
        // nothing referenced: nothing to compare
        s.pure_server_set_referenced_paks("", "");
        assert_eq!(s.compare_paks(true, &|_| false), None);
    }

    #[test]
    fn pure_restriction_and_reorder() {
        let mut s = PakSet::default();
        s.paks = vec![pak(&[1], "z"), pak(&[2], "a"), pak(&[3], "m")];
        let (z, a) = (s.paks[0].checksum, s.paks[1].checksum);
        s.pure_server_set_loaded_paks(&format!("{a} {z}"), "a z");
        s.conditional_restart(0);
        // the server's paks first in its order, the third one is not searched
        assert_eq!(s.paks[0].basename, "a");
        assert_eq!(s.paks[1].basename, "z");
        assert!(!s.allowed(&s.paks[2]));
        assert_eq!(s.effective_paths().len(), 2);
    }

    #[test]
    fn download_names_whitelist() {
        for ok in [
            "slacker/vms-1.1.0-bunker_220207-144309.pk3",
            "gpp/map-arachnid2-1.pk3",
            "base/data-x.pk3",
            "a.pk3",
            "slacker/x.0badf00d.pk3",
            "dir/sub/x+y.pk3",
        ] {
            assert!(safe_download_name(ok), "{ok}");
        }
        let long = format!("{}.pk3", "a".repeat(MAX_DOWNLOAD_NAME_LEN));
        let bad = [
            "/tmp/x.pk3",
            "C:\\x.pk3",
            "C:x.pk3",
            "\\\\host\\share\\x.pk3",
            "\\x.pk3",
            "//host/share/x.pk3",
            "a@b.txt@c.pk3",
            "x.dll",
            "x.PK3",
            "x.pk3.exe",
            "x",
            ".pk3",
            "a/.pk3",
            "a\0b.pk3",
            "a\x07b.pk3",
            "../x.pk3",
            "a/../x.pk3",
            "./x.pk3",
            "a//b.pk3",
            "a/b/.pk3",
            "a/.hidden.pk3",
            "x:stream.pk3",
            "con.pk3",
            "a/NUL.pk3",
            "com1.pk3",
            "lpt9.x.pk3",
            "a space.pk3",
            "",
            "dir/",
        ];
        for b in bad {
            assert!(!safe_download_name(b), "{b:?}");
        }
        assert!(!safe_download_name(&long));
        assert!(safe_pak_name("slacker/vms-1.1.0-bunker_220207-144309"));
        assert!(!safe_pak_name("a@b.txt@c"));
    }

    #[test]
    fn download_dest_stays_below_home() {
        let home = Path::new("/srv/game");
        assert_eq!(
            download_dest(home, "slacker/x-1.pk3"),
            Some(home.join("slacker").join("x-1.pk3"))
        );
        for bad in [
            "/tmp/x.pk3",
            "../x.pk3",
            "a/../../x.pk3",
            "C:\\x.pk3",
            "a@b.pk3",
            "x.dll",
        ] {
            assert_eq!(download_dest(home, bad), None, "{bad}");
        }
    }

    /// With the real Windows path rules (run for the windows target): the
    /// accepted names never turn into a rooted, prefixed or parent path, and
    /// the refused ones are the Windows forms of "outside the directory".
    #[cfg(windows)]
    #[test]
    fn download_dest_windows_semantics() {
        let home = Path::new("C:\\Users\\u\\tremulous");
        let d = download_dest(home, "slacker/x-1.pk3").unwrap();
        assert!(d.starts_with(home));
        for bad in [
            "C:\\x.pk3",
            "C:x.pk3",
            "\\\\host\\share\\x.pk3",
            "\\x.pk3",
            "//host/share/x.pk3",
            "/x.pk3",
            "x:stream.pk3",
            "con.pk3",
        ] {
            assert_eq!(download_dest(home, bad), None, "{bad}");
        }
    }
}
