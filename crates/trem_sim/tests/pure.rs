//! Pure-server checksums against the original C (dec037): the port's
//! files.rs prints what re/harness/pak_ref prints (Com_BlockChecksum test
//! vectors, then FS_LoadZipFile's checksum and pure checksums for a set of
//! feeds) for godot/base/data-1.1.0.pk3; the SHA-256 must equal
//! re/traces/pak.sha256.
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use trem_sim::kernel::files::{block_checksum, zip_directory, Pak, PakSet};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn oracle_lines(pk3s: &[PathBuf]) -> String {
    let feeds: [i32; 8] = [0, 1, -1, 12345, 0x7fffffff, i32::MIN, -559038737, 987654321];
    let vectors = ["", "a", "abc", "message digest", "abcdefghijklmnopqrstuvwxyz", "12345678901234567890123456789012345678901234567890123456789012345678901234567890"];
    let mut out = String::new();
    for v in vectors {
        out.push_str(&format!("md4 \"{v}\" {}\n", block_checksum(v.as_bytes())));
    }
    for p in pk3s {
        let data = std::fs::read(p).unwrap();
        let entries = zip_directory(&data).unwrap();
        let pak = Pak::from_entries(p, "base", &entries);
        out.push_str(&format!("{}.pk3 {} {}", pak.basename, entries.len(), pak.checksum));
        for f in feeds {
            out.push_str(&format!(" {}", pak.pure_checksum(f)));
        }
        out.push('\n');
    }
    out
}

#[test]
fn data_pk3_checksums_match_the_c_oracle() {
    let pk3 = root().join("godot/base/data-1.1.0.pk3");
    assert!(pk3.exists(), "{} is missing: it is tracked in git, restore it", pk3.display());
    let text = oracle_lines(&[pk3.clone()]);
    let want = std::fs::read_to_string(root().join("re/traces/pak.sha256")).unwrap();
    let got = format!("{:x}", Sha256::digest(text.as_bytes()));
    let plain = std::fs::read_to_string(root().join("re/traces/pak.txt")).unwrap_or_default();
    assert_eq!(got, want.trim(), "port:\n{text}\noracle:\n{plain}");
    // the file-reading path agrees with the in-memory one
    let p = Pak::load(&pk3, "base").unwrap();
    assert_eq!(p.checksum, Pak::from_entries(&pk3, "base", &zip_directory(&std::fs::read(&pk3).unwrap()).unwrap()).checksum);
    // Tremulous 1.1.0 keeps its qvms in vms-1.1.0.pk3 (not shipped): the
    // data pak holds the built-in modules' stand-in files
    assert!(p.contains("ui/hud.txt") && p.contains("ui/menus.txt") && !p.contains("vm/cgame.qvm"));
}

/// The server's pure strings and the client's "cp" reply over the real
/// base folder: what SV_VerifyPaks_f accepts.
#[test]
fn base_folder_pure_strings() {
    let base = root().join("godot/base");
    assert!(base.join("data-1.1.0.pk3").exists(), "{} is missing: it is tracked in git, restore it", base.join("data-1.1.0.pk3").display());
    let mut sv = PakSet::new(&[(base.clone(), "base".into())]);
    sv.restart(4242);
    assert!(sv.reference("maps/atcs.bsp"));
    sv.reference_module(&trem_sim::kernel::files::GAME_MODULE, 0);
    // SV_TouchCGame of a pure dedicated server
    assert!(sv.reference_module(&trem_sim::kernel::files::CGAME_MODULE, 0));
    let names = sv.referenced_pak_names();
    assert!(names.contains("base/map-atcs-1.1.0") && names.contains("base/data-legacy-assets"), "{names}");
    // a client with the same folder, restricted to the server's paks
    let mut cl = PakSet::new(&[(base, "base".into())]);
    cl.pure_server_set_loaded_paks(&sv.loaded_pak_checksums(), &sv.loaded_pak_names());
    cl.pure_server_set_referenced_paks(&sv.referenced_pak_checksums(), &sv.referenced_pak_names());
    cl.conditional_restart(4242);
    assert_eq!(cl.compare_paks(true, &|_| false), None);
    use trem_sim::kernel::files::{CGAME_MODULE, FS_CGAME_REF, FS_UI_REF, UI_MODULE};
    cl.reference_module(&CGAME_MODULE, FS_CGAME_REF);
    cl.reference_module(&UI_MODULE, FS_UI_REF);
    cl.reference("maps/atcs.bsp");
    let cp = cl.referenced_pak_pure_checksums();
    let args: Vec<i32> = cp.split_whitespace().filter(|a| *a != "@").map(|a| a.parse().unwrap()).collect();
    let cgame = sv.file_is_in_pak(sv.module_file(&CGAME_MODULE).unwrap()).unwrap();
    assert_eq!(args[0], cgame);
    assert_eq!(args[1], sv.file_is_in_pak(sv.module_file(&UI_MODULE).unwrap()).unwrap());
    let refs = &args[2..args.len() - 1];
    let mut x = 4242;
    for r in refs {
        x ^= r;
    }
    assert_eq!(x ^ refs.len() as i32, *args.last().unwrap());
}
