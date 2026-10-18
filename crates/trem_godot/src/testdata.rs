//! Game data for the unit tests (cfg(test) only).
//!
//! Tests used to read an unpacked data folder at a hard-coded `/tmp` path that
//! only existed on one machine, and quietly `return` when it was not there, so
//! anywhere else they passed without testing anything.
//!
//! Now the data is found, or the test fails with a message that says what to
//! do. It is never skipped:
//!
//! * `TREM_TEST_DATA_LEGACY=<dir>`: the folder the game loads with the legacy
//!   menus: data-1.1.0.pk3 unpacked, then data-legacy-assets.pk3 over it.
//! * `TREM_TEST_DATA_110=<dir>`: data-1.1.0.pk3 unpacked, nothing else.
//! * Unset: the same files read straight from the pk3s in `godot/base/`
//!   (both are tracked in git, and byte-identical to the unpacked folders),
//!   later archives overriding earlier ones.
//!
//! A variable that is set but points at a folder without `ui/` is an error,
//! not a reason to fall back: a typo must not turn into a different test.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

pub(crate) enum Data {
    Dir(PathBuf),
    Paks(Mutex<Vec<(PathBuf, zip::ZipArchive<std::fs::File>)>>),
}

fn open(var: &str, paks: &[&str]) -> Data {
    if let Some(dir) = std::env::var_os(var).filter(|d| !d.is_empty()) {
        let dir = PathBuf::from(dir);
        assert!(dir.join("ui").is_dir(), "{var}={} has no ui/ folder: point it at an unpacked data folder (or unset it to read godot/base/*.pk3)", dir.display());
        return Data::Dir(dir);
    }
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../godot/base");
    let v = paks
        .iter()
        .map(|p| {
            let path = base.join(p);
            let f = std::fs::File::open(&path).unwrap_or_else(|e| panic!("test data {} is missing ({e}); restore it from git, or set {var}=<unpacked data folder>", path.display()));
            let z = zip::ZipArchive::new(f).unwrap_or_else(|e| panic!("{} is not a readable pk3 ({e}); restore it from git, or set {var}=<unpacked data folder>", path.display()));
            (path, z)
        })
        .collect();
    Data::Paks(Mutex::new(v))
}

/// data-1.1.0.pk3 with the legacy assets over it (what the legacy menus see).
pub(crate) fn legacy() -> &'static Data {
    static D: OnceLock<Data> = OnceLock::new();
    D.get_or_init(|| open("TREM_TEST_DATA_LEGACY", &["data-1.1.0.pk3", "data-legacy-assets.pk3"]))
}

/// data-1.1.0.pk3 alone.
pub(crate) fn d110() -> &'static Data {
    static D: OnceLock<Data> = OnceLock::new();
    D.get_or_init(|| open("TREM_TEST_DATA_110", &["data-1.1.0.pk3"]))
}

impl Data {
    pub(crate) fn read(&self, name: &str) -> Option<Vec<u8>> {
        match self {
            Data::Dir(d) => std::fs::read(d.join(name)).ok(),
            Data::Paks(v) => {
                let mut v = v.lock().unwrap_or_else(|e| e.into_inner());
                // later archives win
                for (_, z) in v.iter_mut().rev() {
                    if let Ok(mut f) = z.by_name(name) {
                        let mut out = Vec::new();
                        f.read_to_end(&mut out).ok()?;
                        return Some(out);
                    }
                }
                None
            }
        }
    }

    pub(crate) fn text(&self, name: &str) -> Option<String> {
        self.read(name).map(|b| String::from_utf8_lossy(&b).into_owned())
    }

    /// Like `text`, but a missing file fails the test.
    pub(crate) fn must_text(&self, name: &str) -> String {
        self.text(name).unwrap_or_else(|| panic!("test data file {name} not found (see testdata.rs: TREM_TEST_DATA_LEGACY / TREM_TEST_DATA_110)"))
    }

    pub(crate) fn must_read(&self, name: &str) -> Vec<u8> {
        self.read(name).unwrap_or_else(|| panic!("test data file {name} not found (see testdata.rs: TREM_TEST_DATA_LEGACY / TREM_TEST_DATA_110)"))
    }

    /// Files directly inside `dir` (no trailing slash) ending in one of `exts`.
    pub(crate) fn list(&self, dir: &str, exts: &[&str]) -> Vec<String> {
        let want = |n: &str| exts.iter().any(|e| n.ends_with(e));
        let mut out: Vec<String> = match self {
            Data::Dir(d) => std::fs::read_dir(d.join(dir)).map(|rd| rd.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).filter(|n| want(n)).map(|n| format!("{dir}/{n}")).collect()).unwrap_or_default(),
            Data::Paks(v) => {
                let v = v.lock().unwrap_or_else(|e| e.into_inner());
                let pre = format!("{dir}/");
                v.iter().flat_map(|(_, z)| z.file_names().map(str::to_string).collect::<Vec<_>>()).filter(|n| n.strip_prefix(pre.as_str()).is_some_and(|r| !r.contains('/') && want(r))).collect()
            }
        };
        out.sort();
        out.dedup();
        out
    }
}
