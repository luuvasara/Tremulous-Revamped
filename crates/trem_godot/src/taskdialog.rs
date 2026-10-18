//! The firewall question as a native Windows TaskDialog (dec076).
//!
//! `TaskDialogIndirect` (comctl32 v6) shows the verdict as the main
//! instruction (warning icon), the status rows as content (two links:
//! "What does this mean?" and "How to forward a port"), the long text
//! behind "Show details" (expanded information), a short footer, the
//! answers as command links with a note line, and "Don't ask again when I
//! host" as the verification checkbox (ticked when the window was opened
//! with `hosting_status` while net_hostWindow is 0). The warning icon
//! when a step is left, else the information icon. It is called straight from Rust through a hand-written
//! binding (the one function and two structs; no new crate): the build has no
//! windows-sys/windows crate and this adds none. comctl32 is loaded at run
//! time with LoadLibrary/GetProcAddress, so a missing function (no v6
//! manifest, very old Windows, Wine without it) is a plain `None` and the
//! caller falls back to Godot's own dialog. The Godot exe's manifest
//! already requests Microsoft.Windows.Common-Controls 6.0.0.0, so no
//! activation-context code is needed.
//!
//! The call blocks until the player answers: run it on a thread. While it
//! is open, `Refresh::set` replaces its main line and text (a router or
//! network-check answer that came after it opened); the dialog's own timer
//! callback picks the new text up on its thread.

/// The only addresses the dialog's links open (https only, fixed).
#[cfg(windows)]
pub const LINKS: [&str; 2] = crate::hoststatus::LINKS;

/// New (main line, text) for a dialog that is still open.
#[derive(Clone, Default)]
pub struct Refresh(std::sync::Arc<std::sync::Mutex<Option<(String, String)>>>);

impl Refresh {
    pub fn set(&self, main: &str, content: &str) {
        if let Ok(mut g) = self.0.lock() {
            *g = Some((main.to_string(), content.to_string()));
        }
    }
    /// The pending new text, once.
    pub fn take(&self) -> Option<(String, String)> {
        self.0.lock().ok().and_then(|mut g| g.take())
    }
}

/// Shows the dialog and returns the chosen button id and whether the
/// checkbox was ticked; `default` is the id that has the focus. Err(reason)
/// when the dialog could not be shown (not Windows, comctl32 v6 missing, the
/// call failed with this HRESULT): the caller says why and falls back.
/// Closing the window (X, Esc) gives `default`. Setting the environment
/// variable `TREM_TASKDIALOG_FAIL` makes it fail at once (a test hook for the
/// fallback path).
pub fn show(p: &crate::firewall::Prompt, default: i64, refresh: &Refresh) -> Result<(i64, bool), String> {
    if std::env::var_os(FAIL_HOOK).is_some() {
        return Err(format!("forced to fail by {FAIL_HOOK}"));
    }
    #[cfg(windows)]
    {
        win::show(p, default, refresh)
    }
    #[cfg(not(windows))]
    {
        let _ = (p, default, refresh);
        Err("not Windows".into())
    }
}

/// The test hook's environment variable.
pub const FAIL_HOOK: &str = "TREM_TASKDIALOG_FAIL";

#[cfg(windows)]
mod win {
    use super::Refresh;
    use std::ffi::c_void;

    const TDF_ENABLE_HYPERLINKS: u32 = 0x0001;
    /// TDN_TIMER about every 200 ms (the refresh check)
    const TDF_CALLBACK_TIMER: u32 = 0x0800;
    const TDN_TIMER: u32 = 4;
    /// WM_USER + 108; wParam TDE_CONTENT / TDE_MAIN_INSTRUCTION
    const TDM_SET_ELEMENT_TEXT: u32 = 0x0400 + 108;
    const TDE_CONTENT: usize = 0;
    const TDE_MAIN_INSTRUCTION: usize = 3;

    /// What the callback reads (callback_data): the refresh, and every text
    /// handed to the dialog, kept alive until it closes.
    struct Live {
        refresh: Refresh,
        texts: Vec<Vec<u16>>,
    }
    const TDF_ALLOW_DIALOG_CANCELLATION: u32 = 0x0008;
    /// TDN_HYPERLINK_CLICKED (lParam is the href as UTF-16)
    const TDN_HYPERLINK_CLICKED: u32 = 3;
    /// command links with their arrow (the mockup's look)
    const TDF_USE_COMMAND_LINKS: u32 = 0x0010;
    /// TD_WARNING_ICON: MAKEINTRESOURCEW(-1); TD_INFORMATION_ICON: MAKEINTRESOURCEW(-3)
    const TD_WARNING_ICON: usize = 0xFFFF;
    const TD_INFORMATION_ICON: usize = 0xFFFD;
    /// the verification checkbox starts ticked
    const TDF_VERIFICATION_FLAG_CHECKED: u32 = 0x0100;
    /// answer ids are offset: 0-2 are Windows' own IDOK/IDCANCEL ids (2 is what Esc and the X return)
    const BASE: i32 = 1000;

    #[repr(C, packed(1))]
    pub(super) struct TaskDialogButton {
        id: i32,
        text: *const u16,
    }

    /// commctrl.h declares both structs between pshpack1.h and poppack.h
    #[repr(C, packed(1))]
    pub(super) struct TaskDialogConfig {
        cb_size: u32,
        hwnd_parent: *mut c_void,
        h_instance: *mut c_void,
        flags: u32,
        common_buttons: u32,
        window_title: *const u16,
        main_icon: *const u16,
        main_instruction: *const u16,
        content: *const u16,
        c_buttons: u32,
        buttons: *const TaskDialogButton,
        default_button: i32,
        c_radio_buttons: u32,
        radio_buttons: *const TaskDialogButton,
        default_radio_button: i32,
        verification_text: *const u16,
        expanded_information: *const u16,
        expanded_control_text: *const u16,
        collapsed_control_text: *const u16,
        footer_icon: *const u16,
        footer: *const u16,
        callback: *const c_void,
        callback_data: isize,
        cx_width: u32,
    }

    extern "system" {
        fn LoadLibraryW(name: *const u16) -> *mut c_void;
        fn GetProcAddress(module: *mut c_void, name: *const u8) -> *const c_void;
        fn SendMessageW(hwnd: *mut c_void, msg: u32, w: usize, l: isize) -> isize;
    }

    type TaskDialogIndirect =
        unsafe extern "system" fn(*const TaskDialogConfig, *mut i32, *mut i32, *mut i32) -> i32;

    /// Opens the one fixed help page when its link is clicked; any other
    /// href is ignored (the text of the link is never user data).
    unsafe extern "system" fn callback(
        hwnd: *mut c_void,
        msg: u32,
        _w: usize,
        lparam: isize,
        data: isize,
    ) -> i32 {
        if msg == TDN_TIMER && data != 0 {
            // SAFETY: data is the Live on show()'s stack, alive while the dialog is
            let live = &mut *(data as *mut Live);
            if let Some((main, content)) = live.refresh.take() {
                for (el, s) in [(TDE_MAIN_INSTRUCTION, main), (TDE_CONTENT, content)] {
                    let w = wide(&s);
                    SendMessageW(hwnd, TDM_SET_ELEMENT_TEXT, el, w.as_ptr() as isize);
                    live.texts.push(w);
                }
            }
            return 0;
        }
        if msg == TDN_HYPERLINK_CLICKED && lparam != 0 {
            let mut n = 0usize;
            let p = lparam as *const u16;
            while *p.add(n) != 0 && n < 256 {
                n += 1;
            }
            let href = String::from_utf16_lossy(std::slice::from_raw_parts(p, n));
            if super::LINKS.contains(&href.as_str()) {
                open_url(&href);
            }
        }
        0
    }

    /// ShellExecuteW "open" (shell32 loaded at run time, like comctl32).
    unsafe fn open_url(url: &str) {
        let lib = LoadLibraryW(wide("shell32.dll").as_ptr());
        if lib.is_null() {
            return;
        }
        let f = GetProcAddress(lib, b"ShellExecuteW\0".as_ptr());
        if f.is_null() {
            return;
        }
        type Sh = unsafe extern "system" fn(
            *mut c_void,
            *const u16,
            *const u16,
            *const u16,
            *const u16,
            i32,
        ) -> *mut c_void;
        let f: Sh = std::mem::transmute(f);
        f(
            std::ptr::null_mut(),
            wide("open").as_ptr(),
            wide(url).as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            1,
        );
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    pub fn show(p: &crate::firewall::Prompt, default: i64, refresh: &Refresh) -> Result<(i64, bool), String> {
        let (title, main, content, footer, buttons) = (&p.title, &p.main, &p.content, &p.footer, &p.buttons);
        let mut live = Live { refresh: refresh.clone(), texts: Vec::new() };
        // SAFETY: plain Win32 calls; every pointer handed over lives until the call returns
        unsafe {
            let lib = LoadLibraryW(wide("comctl32.dll").as_ptr());
            if lib.is_null() {
                return Err("comctl32.dll could not be loaded".into());
            }
            let f = GetProcAddress(lib, b"TaskDialogIndirect\0".as_ptr());
            if f.is_null() {
                return Err("TaskDialogIndirect is not in comctl32.dll (no version 6)".into());
            }
            let f: TaskDialogIndirect = std::mem::transmute(f);
            let (t, m, c, ft) = (wide(title), wide(main), wide(content), wide(footer));
            let (det, vt) = (wide(&p.details), wide(&p.verify));
            let (show_d, hide_d) = (wide("Show details"), wide("Hide details"));
            let opt = |s: &str, w: &Vec<u16>| if s.is_empty() { std::ptr::null() } else { w.as_ptr() };
            let texts: Vec<Vec<u16>> = buttons.iter().map(|b| wide(&b.1)).collect();
            let bs: Vec<TaskDialogButton> = buttons
                .iter()
                .zip(&texts)
                .map(|(b, w)| TaskDialogButton {
                    id: BASE + b.0 as i32,
                    text: w.as_ptr(),
                })
                .collect();
            let cfg = TaskDialogConfig {
                cb_size: std::mem::size_of::<TaskDialogConfig>() as u32,
                hwnd_parent: std::ptr::null_mut(),
                h_instance: std::ptr::null_mut(),
                // links always on: a refreshed text may bring the help link
                flags: TDF_ALLOW_DIALOG_CANCELLATION
                    | TDF_USE_COMMAND_LINKS
                    | TDF_ENABLE_HYPERLINKS
                    | TDF_CALLBACK_TIMER
                    | if p.checked { TDF_VERIFICATION_FLAG_CHECKED } else { 0 },
                common_buttons: 0,
                window_title: t.as_ptr(),
                main_icon: (if p.warn { TD_WARNING_ICON } else { TD_INFORMATION_ICON }) as *const u16,
                main_instruction: m.as_ptr(),
                content: c.as_ptr(),
                c_buttons: bs.len() as u32,
                buttons: bs.as_ptr(),
                default_button: BASE + default as i32,
                c_radio_buttons: 0,
                radio_buttons: std::ptr::null(),
                default_radio_button: 0,
                verification_text: opt(&p.verify, &vt),
                expanded_information: opt(&p.details, &det),
                expanded_control_text: hide_d.as_ptr(),
                collapsed_control_text: show_d.as_ptr(),
                footer_icon: std::ptr::null(),
                footer: ft.as_ptr(),
                callback: callback as *const c_void,
                callback_data: &mut live as *mut Live as isize,
                cx_width: 0,
            };
            let mut pressed = 0i32;
            let mut ticked = 0i32;
            let hr = f(&cfg, &mut pressed, std::ptr::null_mut(), &mut ticked);
            if hr < 0 {
                return Err(format!("TaskDialogIndirect failed, HRESULT 0x{:08X}", hr as u32));
            }
            let id = if pressed >= BASE { (pressed - BASE) as i64 } else { default };
            Ok((id, ticked != 0))
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    #[cfg(not(windows))]
    fn not_windows_falls_back() {
        let p = crate::firewall::Prompt {
            title: "t".into(),
            main: "m".into(),
            content: "c".into(),
            footer: "f".into(),
            details: "d".into(),
            verify: "v".into(),
            buttons: vec![(1, "a".into())],
            ..Default::default()
        };
        let e = super::show(&p, 1, &super::Refresh::default()).unwrap_err();
        assert!(e.contains("not Windows") || e.contains(super::FAIL_HOOK), "{e}");
    }

    #[test]
    fn the_test_hook_forces_a_failure_with_a_reason() {
        let p = crate::firewall::Prompt::default();
        std::env::set_var(super::FAIL_HOOK, "1");
        let e = super::show(&p, 1, &super::Refresh::default()).unwrap_err();
        std::env::remove_var(super::FAIL_HOOK);
        assert!(e.contains("TREM_TASKDIALOG_FAIL"), "{e}");
    }

    #[test]
    fn a_refresh_is_taken_once() {
        let r = super::Refresh::default();
        assert_eq!(r.take(), None);
        r.clone().set("m", "c1");
        r.set("m", "c2");
        assert_eq!(r.take(), Some(("m".into(), "c2".into())));
        assert_eq!(r.take(), None);
    }

    #[cfg(windows)]
    #[test]
    fn struct_sizes_match_commctrl() {
        // x64, packed(1) as between pshpack1.h and poppack.h: 160 and 12 bytes
        assert_eq!(std::mem::size_of::<super::win::TaskDialogConfig>(), 160);
        assert_eq!(std::mem::size_of::<super::win::TaskDialogButton>(), 12);
    }
}
