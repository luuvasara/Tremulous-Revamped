//! Windows Firewall check for a hosted game (port addition, dec047).
//!
//! A listen server on Windows is reachable from its own PC whatever the
//! firewall says (traffic to the PC's own address is not filtered), so a
//! test on one PC passes while a friend on the same Wi-Fi sees nothing:
//! with no inbound allow rule for the game's exe, Windows drops every
//! packet from other PCs, the LAN scan's answers included. This asks
//! Windows (PowerShell, read only, on a thread) which inbound rules name
//! the running exe and which network categories are active, and tells the
//! host in one line. `firewall_allow` adds the rule through Windows' own
//! administrator prompt; the player approves or declines it there.

use std::process::Command;

/// What the firewall does with packets from other PCs for this exe.
#[derive(Clone, Debug, PartialEq)]
pub enum Verdict {
    /// every active network category has an enabled allow rule and no block
    Allowed(Vec<String>),
    /// an enabled block rule covers an active category
    Blocked(Vec<String>),
    /// no allow rule for these active categories
    NoRule(Vec<String>),
    /// allowed for these active categories only from the local subnet (the
    /// "Host for my home network only" rule): LAN players get in, internet
    /// players do not
    LanOnly(Vec<String>),
    /// the check itself failed (not Windows, PowerShell missing, ...)
    Unknown(String),
}

/// One inbound rule naming the exe: action, enabled, profiles.
#[derive(Clone, Debug, PartialEq)]
pub struct Rule {
    pub allow: bool,
    pub enabled: bool,
    pub profiles: Vec<String>,
    /// the rule only lets in remote addresses of the local network
    /// (RemoteAddress LocalSubnet or a list), not any address
    pub lan: bool,
}

/// The name of the rule the game adds (dec076; the old one is
/// `OLD_RULE_NAME`), and of the home-network-only one.
pub const RULE_NAME: &str = "Tremulous Revamped";
pub const RULE_NAME_LAN: &str = "Tremulous Revamped (LAN)";
pub const OLD_RULE_NAME: &str = "Tremulous (Godot port)";

/// Whether a rule's RemoteAddress field ("Any", "LocalSubnet", a list)
/// limits it to some addresses. An old check script prints no field: any.
fn remote_limited(field: &str) -> bool {
    let f = field.trim();
    !(f.is_empty() || f.eq_ignore_ascii_case("any") || f.eq_ignore_ascii_case("*"))
}

/// The categories a rule's `Profile` field covers ("Any", "Private, Public", ...).
fn profiles(field: &str) -> Vec<String> {
    let f = field.trim();
    if f.eq_ignore_ascii_case("any") || f.is_empty() {
        return vec!["Domain".into(), "Private".into(), "Public".into()];
    }
    f.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()
}

/// `DomainAuthenticated` (Get-NetConnectionProfile) is the `Domain` profile.
fn category(c: &str) -> String {
    let c = c.trim();
    if c.starts_with("Domain") {
        "Domain".into()
    } else {
        c.to_string()
    }
}

/// Parses the check script's output: `R|<Action>|<Enabled>|<Profile>|<RemoteAddress>`
/// per inbound rule naming the exe (the last field may be missing),
/// `N|<NetworkCategory>` per connected network.
pub fn parse(out: &str) -> (Vec<Rule>, Vec<String>) {
    let mut rules = Vec::new();
    let mut nets = Vec::new();
    for l in out.lines() {
        let p: Vec<&str> = l.trim().split('|').collect();
        match p.as_slice() {
            ["R", action, enabled, prof, rest @ ..] if rest.len() <= 1 => rules.push(Rule {
                allow: action.trim().eq_ignore_ascii_case("allow"),
                enabled: enabled.trim().eq_ignore_ascii_case("true"),
                profiles: profiles(prof),
                lan: rest.first().is_some_and(|r| remote_limited(r)),
            }),
            ["N", cat] => {
                let c = category(cat);
                if !c.is_empty() && !nets.contains(&c) {
                    nets.push(c);
                }
            }
            _ => {}
        }
    }
    (rules, nets)
}

/// Windows' rule: a matching enabled block rule wins over any allow rule.
pub fn judge(rules: &[Rule], nets: &[String]) -> Verdict {
    if nets.is_empty() {
        return Verdict::Unknown("no connected network".into());
    }
    let covers = |r: &Rule, n: &String| r.enabled && r.profiles.iter().any(|p| p.eq_ignore_ascii_case(n));
    let blocked: Vec<String> = nets.iter().filter(|n| rules.iter().any(|r| !r.allow && covers(r, n))).cloned().collect();
    if !blocked.is_empty() {
        return Verdict::Blocked(blocked);
    }
    // an allow rule for any address; a LAN-only one lets the home network in
    let missing: Vec<String> = nets.iter().filter(|n| !rules.iter().any(|r| r.allow && !r.lan && covers(r, n))).cloned().collect();
    if missing.is_empty() {
        return Verdict::Allowed(nets.to_vec());
    }
    if missing.iter().all(|n| rules.iter().any(|r| r.allow && r.lan && covers(r, n))) {
        return Verdict::LanOnly(missing);
    }
    if !missing.is_empty() {
        return Verdict::NoRule(missing);
    }
    Verdict::Allowed(nets.to_vec())
}

/// PowerShell single-quoted literal.
fn ps_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// The read-only check script for `exe`.
pub fn check_script(exe: &str) -> String {
    format!(
        "$e = {}; \
         Get-NetFirewallApplicationFilter -ErrorAction SilentlyContinue | Where-Object {{ $_.Program -and ([Environment]::ExpandEnvironmentVariables($_.Program) -ieq $e) }} | \
         Get-NetFirewallRule -ErrorAction SilentlyContinue | Where-Object {{ \"$($_.Direction)\" -eq 'Inbound' }} | \
         ForEach-Object {{ 'R|' + $_.Action + '|' + $_.Enabled + '|' + $_.Profile + '|' + ((($_ | Get-NetFirewallAddressFilter -ErrorAction SilentlyContinue).RemoteAddress) -join ',') }}; \
         Get-NetConnectionProfile -ErrorAction SilentlyContinue | ForEach-Object {{ 'N|' + $_.NetworkCategory }}",
        ps_quote(exe)
    )
}

fn powershell(script: &str) -> std::io::Result<std::process::Output> {
    let mut c = Command::new("powershell");
    c.args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", script]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    c.output()
}

/// The longest the firewall check may run: Windows' rule listing is slow on
/// a PC with many rules, but a PowerShell that never ends must not hold the
/// answer (and a process) forever.
pub const CHECK_TIMEOUT_SECS: u64 = 60;

/// Runs `cmd` to the end, or kills it after `limit`: its output, else the
/// reason. stdout/stderr are read on threads so a full pipe cannot stall it.
pub fn output_timeout(mut cmd: Command, limit: std::time::Duration) -> Result<std::process::Output, String> {
    use std::io::Read;
    use std::process::Stdio;
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| format!("could not start: {e}"))?;
    let mut out = child.stdout.take();
    let mut err = child.stderr.take();
    let t_out = std::thread::spawn(move || {
        let mut b = Vec::new();
        if let Some(o) = out.as_mut() {
            let _ = o.read_to_end(&mut b);
        }
        b
    });
    let t_err = std::thread::spawn(move || {
        let mut b = Vec::new();
        if let Some(o) = err.as_mut() {
            let _ = o.read_to_end(&mut b);
        }
        b
    });
    let start = std::time::Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Ok(s),
            Ok(None) if start.elapsed() >= limit => {
                let _ = child.kill();
                let _ = child.wait();
                break Err(format!("timed out after {} s", limit.as_secs()));
            }
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(50)),
            Err(e) => break Err(e.to_string()),
        }
    };
    // the pipes close when the process is gone; a grandchild holding them open must not hang us
    let join = |t: std::thread::JoinHandle<Vec<u8>>| {
        let d = std::time::Instant::now();
        while !t.is_finished() && d.elapsed() < std::time::Duration::from_secs(2) {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        if t.is_finished() { t.join().unwrap_or_default() } else { Vec::new() }
    };
    let (stdout, stderr) = (join(t_out), join(t_err));
    status.map(|status| std::process::Output { status, stdout, stderr })
}

fn powershell_limited(script: &str, limit: std::time::Duration) -> Result<std::process::Output, String> {
    let mut c = Command::new("powershell");
    c.args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", script]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    output_timeout(c, limit)
}

/// Runs the check for the running exe (blocking: call it on a thread).
pub fn check() -> Verdict {
    check_with(&check_script_for_running_exe(), std::time::Duration::from_secs(CHECK_TIMEOUT_SECS))
}

fn check_script_for_running_exe() -> Option<String> {
    std::env::current_exe().ok().map(|p| check_script(&p.to_string_lossy()))
}

fn check_with(script: &Option<String>, limit: std::time::Duration) -> Verdict {
    if !cfg!(windows) {
        return Verdict::Unknown("not Windows".into());
    }
    let Some(script) = script else { return Verdict::Unknown("the game's own path is unknown".into()) };
    match powershell_limited(script, limit) {
        Ok(o) => {
            let (rules, nets) = parse(&String::from_utf8_lossy(&o.stdout));
            judge(&rules, &nets)
        }
        Err(e) => Verdict::Unknown(e),
    }
}

/// The `netsh` arguments that allow `exe` in on UDP for `profiles`; `lan`:
/// only from the local subnet (the home network), under its own name.
pub fn allow_args(exe: &str, profiles: &[String], lan: bool) -> String {
    let prof = if profiles.is_empty() { "private".to_string() } else { profiles.iter().map(|p| p.to_ascii_lowercase()).collect::<Vec<_>>().join(",") };
    let (name, remote) = if lan { (RULE_NAME_LAN, " remoteip=localsubnet") } else { (RULE_NAME, "") };
    format!("advfirewall firewall add rule name=\"{name}\" dir=in action=allow enable=yes protocol=udp profile={prof}{remote} program=\"{exe}\"")
}

/// The game's own rules that go before a new one is added (dec076): those
/// named `RULE_NAME*` or `OLD_RULE_NAME` whose program is `$e` (replaced)
/// or a file that no longer exists (a moved or deleted copy of the game).
/// Rules with other names, and ours for copies that still exist, stay.
pub fn cleanup_script() -> String {
    format!(
        "Get-NetFirewallRule -DisplayName {},{} -ErrorAction SilentlyContinue | Where-Object {{ $p = ($_ | Get-NetFirewallApplicationFilter -ErrorAction SilentlyContinue).Program; \
         $p -and ($p -ne 'Any') -and ((([Environment]::ExpandEnvironmentVariables($p)) -ieq $e) -or -not (Test-Path -LiteralPath ([Environment]::ExpandEnvironmentVariables($p)))) }} | Remove-NetFirewallRule",
        ps_quote(&format!("{RULE_NAME}*")),
        ps_quote(OLD_RULE_NAME)
    )
}

/// The script the elevated PowerShell runs (dec054, dec076), all in one
/// administrator prompt: the game's own stale rules go (`cleanup_script`),
/// the inbound block rules naming `exe` go (Windows makes one when its own
/// "allow access" prompt is cancelled, and a block wins over any allow),
/// then the allow rule of `allow_args` is added.
pub fn fix_script(exe: &str, profiles: &[String], lan: bool) -> String {
    format!(
        "$e = {}; \
         {}; \
         Get-NetFirewallApplicationFilter -ErrorAction SilentlyContinue | Where-Object {{ $_.Program -and ([Environment]::ExpandEnvironmentVariables($_.Program) -ieq $e) }} | \
         Get-NetFirewallRule -ErrorAction SilentlyContinue | Where-Object {{ \"$($_.Direction)\" -eq 'Inbound' -and \"$($_.Action)\" -eq 'Block' }} | Remove-NetFirewallRule; \
         netsh {}; exit $LASTEXITCODE",
        ps_quote(exe),
        cleanup_script(),
        allow_args(exe, profiles, lan)
    )
}

/// `-EncodedCommand`'s argument: base64 of the UTF-16LE script (no
/// quoting through Start-Process's argument list).
pub fn encode_command(script: &str) -> String {
    const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let bytes: Vec<u8> = script.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for c in bytes.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= c.len() {
                out.push(B64[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Asks Windows to let the game in (blocking until the player answers:
/// call it on a thread): Windows shows its administrator prompt; nothing
/// changes unless the player approves it there.
pub fn request_allow(profiles: &[String], lan: bool) -> Result<(), String> {
    if !cfg!(windows) {
        return Err("only on Windows".into());
    }
    let exe = std::env::current_exe().map_err(|e| e.to_string())?.to_string_lossy().into_owned();
    let script = format!(
        "try {{ $p = Start-Process -FilePath powershell -Verb RunAs -Wait -PassThru -WindowStyle Hidden -ArgumentList '-NoProfile','-ExecutionPolicy','Bypass','-EncodedCommand','{}'; exit $p.ExitCode }} catch {{ exit 1223 }}",
        encode_command(&fix_script(&exe, profiles, lan))
    );
    let o = powershell(&script).map_err(|e| e.to_string())?;
    match o.status.code() {
        Some(0) => Ok(()),
        Some(1223) => Err("the administrator prompt was declined: other PCs still cannot reach the game".into()),
        c => Err(format!("Windows did not add the rule (exit {c:?})")),
    }
}

/// The ids of the answers (what `Prompt::buttons` pairs with its labels).
pub const ANSWER_ALLOW: i64 = 0;
/// Host without the rule (the default: closing the dialog means this).
pub const ANSWER_HOST: i64 = 1;
/// Host without it and never ask when hosting (net_hostWindow 0): the
/// fallback dialog's button; the native one has the checkbox instead.
pub const ANSWER_NEVER: i64 = 2;
/// Allow from the home network only (`allow_args` lan) and close the
/// router's port again.
pub const ANSWER_LAN: i64 = 3;
/// The question at host time (dec076): Windows' TaskDialog layout, the
/// verdict as the main instruction, the status rows as content, the long
/// text behind "Show details", a short footer.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Prompt {
    pub title: String,
    pub main: String,
    pub content: String,
    pub footer: String,
    /// the expanded information ("Show details")
    pub details: String,
    /// the checkbox ("Don't ask again when I host")
    pub verify: String,
    /// (answer id, "label\nnote") in display order (command links)
    pub buttons: Vec<(i64, String)>,
    /// the checkbox starts ticked (net_hostWindow 0: `hosting_status`
    /// shows the window anyway; untick it to have it back when you host)
    pub checked: bool,
    /// a warning icon (a step is left), else the information icon
    pub warn: bool,
}

/// `<a href="...">text</a>` to `text`.
pub fn strip_links(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find("<a href=\"") {
        out.push_str(&rest[..i]);
        let after = &rest[i..];
        let Some(j) = after.find("\">") else { out.push_str(after); return out };
        rest = &after[j + 2..];
    }
    out.push_str(rest);
    out.replace("</a>", "")
}

impl Prompt {
    /// The whole question as plain text, for Godot's own dialog (the fallback).
    pub fn plain(&self) -> String {
        let buttons: Vec<String> = self.buttons.iter().map(|b| match b.1.split_once('\n') {
            Some((l, n)) => format!("{l}: {n}"),
            None => b.1.clone(),
        }).collect();
        format!("{}\n\n{}\n\n{}\n\n{}", self.main, strip_links(&self.content), buttons.join("\n"), self.footer)
    }

    /// The fallback's buttons: the labels, and "Don't ask again" as one more.
    pub fn plain_buttons(&self) -> Vec<(i64, String)> {
        let mut b: Vec<(i64, String)> = self.buttons.iter().map(|b| (b.0, b.1.split('\n').next().unwrap_or("").to_string())).collect();
        if !self.verify.is_empty() {
            b.push((ANSWER_NEVER, self.verify.clone()));
        }
        b
    }
}

/// The networks a full allow rule is still missing for (what Allow covers).
pub fn missing(v: &Verdict) -> Vec<String> {
    match v {
        Verdict::NoRule(n) | Verdict::Blocked(n) | Verdict::LanOnly(n) => n.clone(),
        _ => Vec::new(),
    }
}

fn lacking(v: &Verdict) -> Option<&Vec<String>> {
    match v {
        Verdict::NoRule(n) | Verdict::Blocked(n) => Some(n),
        _ => None,
    }
}

/// Whether the host-time window is shown (dec076): only when the host
/// starts a network game (`host_time`, not the startup check), the player
/// did not say never (net_hostWindow 0, which also keeps what the
/// startup prompt's old "Don't ask again" saved), it was not shown already
/// for this server (`shown`: once per hosting, not per map change; the next
/// server asks again) and no administrator prompt is pending. Then on every
/// host where the window exists (`native`, Windows: the status and its
/// choices, with the firewall's buttons when a rule is lacking), elsewhere
/// only when the firewall lacks a rule.
pub fn should_ask(v: &Verdict, setting: i64, shown: bool, fixing: bool, host_time: bool, native: bool) -> bool {
    host_time && setting != 0 && !shown && !fixing && (native || lacking(v).is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_rule_on_private_network() {
        let (r, n) = parse("N|Private\r\n");
        assert_eq!(judge(&r, &n), Verdict::NoRule(vec!["Private".into()]));
    }

    #[test]
    fn allow_rule_for_private_covers_private_only() {
        let out = "R|Allow|True|Private\nN|Private\n";
        let (r, n) = parse(out);
        assert_eq!(judge(&r, &n), Verdict::Allowed(vec!["Private".into()]));
        let (r, n) = parse("R|Allow|True|Private\nN|Public\n");
        assert_eq!(judge(&r, &n), Verdict::NoRule(vec!["Public".into()]));
    }

    #[test]
    fn block_wins_and_disabled_rules_do_not_count() {
        let (r, n) = parse("R|Allow|True|Any\nR|Block|True|Public\nN|Public\n");
        assert_eq!(judge(&r, &n), Verdict::Blocked(vec!["Public".into()]));
        let (r, n) = parse("R|Allow|False|Private\nN|Private\n");
        assert_eq!(judge(&r, &n), Verdict::NoRule(vec!["Private".into()]));
        let (r, n) = parse("R|Allow|True|Any\nR|Block|False|Private\nN|DomainAuthenticated\nN|Private\n");
        assert_eq!(judge(&r, &n), Verdict::Allowed(vec!["Domain".into(), "Private".into()]));
    }

    #[test]
    fn multi_profile_field_and_unknown() {
        let (r, n) = parse("R|Allow|True|Private, Public\nN|Public\n");
        assert_eq!(judge(&r, &n), Verdict::Allowed(vec!["Public".into()]));
        assert!(matches!(judge(&[], &[]), Verdict::Unknown(_)));
    }

    #[test]
    fn scripts_quote_paths() {
        let s = check_script(r"C:\Users\O'Neil\Desktop\Tremulous Release\Tremulous.exe");
        assert!(s.starts_with(r"$e = 'C:\Users\O''Neil\Desktop\Tremulous Release\Tremulous.exe';"));
        let a = allow_args(r"C:\x y\T.exe", &["Private".into(), "Public".into()], false);
        assert!(a.ends_with("profile=private,public program=\"C:\\x y\\T.exe\""));
        assert!(a.contains("dir=in action=allow") && a.contains("protocol=udp"));
        assert!(allow_args("a", &[], false).contains("profile=private "));
        let f = fix_script(r"C:\O'Neil\T.exe", &["Public".into()], false);
        assert!(f.starts_with(r"$e = 'C:\O''Neil\T.exe';") && f.contains("-eq 'Block' } | Remove-NetFirewallRule;"));
        assert!(f.contains("netsh advfirewall firewall add rule") && f.contains("profile=public"));
    }

    #[test]
    fn rule_names_and_the_lan_rule() {
        let a = allow_args(r"C:\G\Tremulous.exe", &["Private".into()], false);
        assert!(a.starts_with("advfirewall firewall add rule name=\"Tremulous Revamped\" dir=in"), "{a}");
        assert!(!a.contains("remoteip"));
        let l = allow_args(r"C:\G\Tremulous.exe", &["Private".into()], true);
        assert_eq!(l, "advfirewall firewall add rule name=\"Tremulous Revamped (LAN)\" dir=in action=allow enable=yes protocol=udp profile=private remoteip=localsubnet program=\"C:\\G\\Tremulous.exe\"");
        let f = fix_script(r"C:\G\Tremulous.exe", &["Private".into()], true);
        assert!(f.contains("name=\"Tremulous Revamped (LAN)\"") && f.contains("remoteip=localsubnet"));
    }

    #[test]
    fn cleanup_touches_only_our_rules_for_this_or_a_missing_exe() {
        let c = cleanup_script();
        assert!(c.starts_with("Get-NetFirewallRule -DisplayName 'Tremulous Revamped*','Tremulous (Godot port)' "), "{c}");
        assert!(c.contains("-ieq $e") && c.contains("-not (Test-Path -LiteralPath"));
        assert!(c.ends_with("| Remove-NetFirewallRule"));
        // one elevated script: stale rules, then block rules, then the new rule
        let f = fix_script(r"C:\G\Tremulous.exe", &["Private".into()], false);
        let (i, j, k) = (f.find("-DisplayName").unwrap(), f.find("-eq 'Block'").unwrap(), f.find("netsh").unwrap());
        assert!(i < j && j < k);
        assert_eq!(f.matches("Remove-NetFirewallRule").count(), 2);
        // the check script reads each rule's remote addresses
        assert!(check_script("x").contains("Get-NetFirewallAddressFilter"));
    }

    #[test]
    fn a_lan_only_rule() {
        let (r, n) = parse("R|Allow|True|Private|LocalSubnet\nN|Private\n");
        assert!(r[0].lan);
        assert_eq!(judge(&r, &n), Verdict::LanOnly(vec!["Private".into()]));
        assert_eq!(missing(&judge(&r, &n)), vec!["Private".to_string()]);
        let (r, n) = parse("R|Allow|True|Private|LocalSubnet\nR|Allow|True|Any|Any\nN|Private\n");
        assert_eq!(judge(&r, &n), Verdict::Allowed(vec!["Private".into()]));
        let (r, n) = parse("R|Allow|True|Private|LocalSubnet\nN|Private\nN|Public\n");
        assert_eq!(judge(&r, &n), Verdict::NoRule(vec!["Private".into(), "Public".into()]));
        // the old four-field line: any address
        let (r, _) = parse("R|Allow|True|Private\n");
        assert!(!r[0].lan);
        // without the window (not Windows) a home-network-only rule is nothing to ask
        assert!(!should_ask(&Verdict::LanOnly(vec!["Private".into()]), 1, false, false, true, false));
        // with it, the window shows (and offers to let the internet in too)
        assert!(should_ask(&Verdict::LanOnly(vec!["Private".into()]), 1, false, false, true, true));
    }

    #[test]
    fn plain_text_and_fallback_buttons() {
        let p = Prompt {
            title: "t".into(),
            main: "m".into(),
            content: "a <a href=\"https://x.test/#h\">What does this mean?</a> b <a href=\"https://portforward.com\">How</a>".into(),
            footer: "f".into(),
            details: "d".into(),
            verify: "Don't ask again when I host".into(),
            buttons: vec![(ANSWER_ALLOW, "Allow players to join\nnote".into()), (ANSWER_HOST, "Host anyway".into())],
            ..Default::default()
        };
        assert_eq!(strip_links(&p.content), "a What does this mean? b How");
        assert!(p.plain().contains("Allow players to join: note\nHost anyway"));
        let b = p.plain_buttons();
        assert_eq!(b.iter().map(|b| b.1.as_str()).collect::<Vec<_>>(), ["Allow players to join", "Host anyway", "Don't ask again when I host"]);
        assert_eq!(b[2].0, ANSWER_NEVER);
    }

    #[test]
    fn encoded_command_is_utf16le_base64() {
        // PowerShell: [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes('dir'))
        assert_eq!(encode_command("dir"), "ZABpAHIA");
        assert_eq!(encode_command("ab"), "YQBiAA==");
        assert_eq!(encode_command(""), "");
    }

    #[test]
    fn host_time_decision() {
        let lack = Verdict::NoRule(vec!["Private".into()]);
        let blocked = Verdict::Blocked(vec!["Public".into()]);
        let allowed = Verdict::Allowed(vec!["Private".into()]);
        let unknown = Verdict::Unknown("x".into());
        // asks at host time, when lacking a rule
        assert!(should_ask(&lack, 1, false, false, true, true));
        assert!(should_ask(&blocked, 1, false, false, true, false));
        // never at startup (the silent check)
        assert!(!should_ask(&lack, 1, false, false, false, true));
        // "Don't ask when I host" (and the old startup opt-out: net_hostWindow 0)
        assert!(!should_ask(&lack, 0, false, false, true, true));
        assert!(!should_ask(&allowed, 0, false, false, true, true));
        // once per hosted server (not per map change), and not while the administrator prompt is pending
        assert!(!should_ask(&lack, 1, true, false, true, true));
        assert!(!should_ask(&lack, 1, false, true, true, true));
        // with the window (Windows) it shows on every host, also
        // once the rule exists or the check failed, so the choices can be changed again
        assert!(should_ask(&allowed, 1, false, false, true, true));
        assert!(should_ask(&unknown, 1, false, false, true, true));
        // without it (not Windows): nothing to ask when allowed or unknown
        assert!(!should_ask(&allowed, 1, false, false, true, false));
        assert!(!should_ask(&unknown, 1, false, false, true, false));
    }

    #[cfg(unix)]
    #[test]
    fn output_is_returned_when_the_process_ends_in_time() {
        let mut c = Command::new("sh");
        c.args(["-c", "echo R\\|Allow; exit 0"]);
        let o = output_timeout(c, std::time::Duration::from_secs(10)).unwrap();
        assert!(o.status.success());
        assert!(String::from_utf8_lossy(&o.stdout).contains("Allow"));
    }

    #[cfg(unix)]
    #[test]
    fn a_process_that_never_ends_is_killed_after_the_limit() {
        let mut c = Command::new("sh");
        c.args(["-c", "sleep 30"]);
        let t = std::time::Instant::now();
        let e = output_timeout(c, std::time::Duration::from_millis(300)).unwrap_err();
        assert!(e.contains("timed out"), "{e}");
        assert!(t.elapsed() < std::time::Duration::from_secs(10), "returned late: {:?}", t.elapsed());
    }

    #[cfg(unix)]
    #[test]
    fn a_missing_program_is_a_reason_not_a_panic() {
        let c = Command::new("/nonexistent/powershell");
        assert!(output_timeout(c, std::time::Duration::from_secs(1)).unwrap_err().contains("could not start"));
    }

    #[test]
    fn not_windows_check_is_unknown() {
        if !cfg!(windows) {
            assert!(matches!(check(), Verdict::Unknown(_)));
        }
        assert_eq!(CHECK_TIMEOUT_SECS, 60);
    }
}
