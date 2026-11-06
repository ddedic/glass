//! Windows Defender Firewall: whether it lets Glass accept connections, and the rule that
//! does. Without one, Windows asks the first time the host listens, on the computer's own
//! screen, and turning that down leaves Glass blocked.

use std::path::Path;

use windows::Win32::NetworkManagement::WindowsFirewall::{
    INetFwPolicy2, INetFwRule, NET_FW_ACTION_ALLOW, NET_FW_PROFILE2_PRIVATE, NET_FW_RULE_DIR_IN, NetFwPolicy2,
};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, IDispatch};
use windows::Win32::System::Environment::ExpandEnvironmentStringsW;
use windows::Win32::System::Ole::IEnumVARIANT;
use windows::Win32::System::Variant::{VARIANT, VT_DISPATCH, VariantClear};
use windows::core::{HSTRING, Interface};

pub use crate::firewall_bind::Remote;
use crate::firewall_bind::{self, TAILSCALE_V4, TAILSCALE_V6};

/// What the inbound rules say about a program on private networks.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Inbound {
    Allowed,
    /// A block rule (one is left behind when someone turns down Windows' prompt). Block
    /// rules win over allow rules.
    Blocked,
    /// No rule: Windows will ask.
    Missing,
}

/// One rule, as much of it as matters here.
#[derive(Debug, Clone)]
pub struct Rule {
    pub program: String,
    pub enabled: bool,
    pub inbound: bool,
    pub allow: bool,
    pub profiles: i32,
    pub name: String,
    pub remote: String,
}

/// Decides from the rules for `exe`; `same` says whether a rule's program is it.
pub fn decide(rules: &[Rule], same: impl Fn(&str) -> bool) -> Inbound {
    let (block, allow) = rules
        .iter()
        .filter(|r| r.enabled && r.inbound && r.profiles & NET_FW_PROFILE2_PRIVATE.0 != 0 && same(&r.program))
        .fold((false, false), |(b, a), r| (b || !r.allow, a || r.allow));
    if block {
        Inbound::Blocked
    } else if allow {
        Inbound::Allowed
    } else {
        Inbound::Missing
    }
}

/// The program paths in rules are often lower-cased (Windows' own prompt does that) or
/// written with variables like %ProgramFiles%.
fn same_program(rule: &str, exe: &Path) -> bool {
    expand(rule).to_lowercase() == crate::browse::shell_path(&exe.to_string_lossy()).to_lowercase()
}

fn expand(s: &str) -> String {
    if !s.contains('%') {
        return s.to_string();
    }
    let src = HSTRING::from(s);
    let mut buf = vec![0u16; 1024];
    // SAFETY: buf is a writable slice; the call reports the length it needed.
    let n = unsafe { ExpandEnvironmentStringsW(&src, Some(&mut buf)) } as usize;
    if n == 0 || n > buf.len() {
        return s.to_string();
    }
    String::from_utf16_lossy(&buf[..n - 1])
}

/// Every firewall rule's essentials. `None` if the firewall service can't be asked.
fn rules() -> Option<Vec<Rule>> {
    super::system::with_com(|| {
        // SAFETY: COM is initialised on this thread. Every VARIANT the enumerator fills is
        // cleared after its dispatch pointer is copied (AddRef'd) out.
        unsafe {
            let policy: INetFwPolicy2 = CoCreateInstance(&NetFwPolicy2, None, CLSCTX_INPROC_SERVER).ok()?;
            let all = policy.Rules().ok()?;
            let list: IEnumVARIANT = all._NewEnum().ok()?.cast().ok()?;
            let mut out = Vec::new();
            loop {
                let mut v = [VARIANT::default()];
                let mut got = 0u32;
                if list.Next(&mut v, &mut got).is_err() || got == 0 {
                    break;
                }
                let inner = &v[0].Anonymous.Anonymous;
                let disp: Option<IDispatch> = if inner.vt == VT_DISPATCH { (*inner.Anonymous.pdispVal).clone() } else { None };
                let _ = VariantClear(&mut v[0]);
                let Some(rule) = disp.and_then(|d| d.cast::<INetFwRule>().ok()) else { continue };
                out.push(Rule {
                    program: rule.ApplicationName().map(|s| s.to_string()).unwrap_or_default(),
                    enabled: rule.Enabled().is_ok_and(|e| e.as_bool()),
                    inbound: rule.Direction().is_ok_and(|d| d == NET_FW_RULE_DIR_IN),
                    allow: rule.Action().is_ok_and(|a| a == NET_FW_ACTION_ALLOW),
                    profiles: rule.Profiles().unwrap_or(0),
                    name: rule.Name().map(|s| s.to_string()).unwrap_or_default(),
                    remote: rule.RemoteAddresses().map(|s| s.to_string()).unwrap_or_default(),
                });
            }
            Some(out)
        }
    })
}

/// Whether `exe` may accept connections on private networks; `None` if unknown.
pub fn inbound(exe: &Path) -> Option<Inbound> {
    Some(decide(&rules()?, |p| same_program(p, exe)))
}

/// The saved listen address. A missing file is the default (any remote address).
/// Anything unreadable is `Err`: the caller must not add a wide rule.
pub fn saved_remote() -> Result<Remote, ()> {
    let bytes = match std::fs::read(crate::settings::path()) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Remote::Any),
        Err(_) => return Err(()),
    };
    let settings: crate::settings::Settings = serde_json::from_slice(&bytes).map_err(|_| ())?;
    firewall_bind::remote_for_bind(&settings.bind)
}

fn program(exe: &Path) -> String {
    crate::browse::shell_path(&exe.to_string_lossy())
}

/// netsh arguments that delete the rule named `name` for `exe`.
pub fn delete_named_args(exe: &Path, name: &str) -> String {
    format!("advfirewall firewall delete rule name=\"{name}\" program=\"{}\"", program(exe))
}

/// netsh arguments that delete the rule named Glass for `exe`.
pub fn delete_rule_args(exe: &Path) -> String {
    delete_named_args(exe, "Glass")
}

/// netsh arguments for Glass's allow, or `None` when loopback needs no allow.
pub fn add_rule_args(exe: &Path, remote: Remote) -> Option<String> {
    let base = format!(
        "advfirewall firewall add rule name=\"Glass\" dir=in action=allow program=\"{}\" enable=yes profile=private",
        program(exe)
    );
    match remote {
        Remote::Loopback => None,
        Remote::Any => Some(format!("{base} edge=no")),
        Remote::Tailscale => Some(format!("{base} remoteip=\"{TAILSCALE_V4},{TAILSCALE_V6}\" edge=no")),
    }
}

fn netsh(args: &str) -> bool {
    use std::os::windows::process::CommandExt;
    let mut cmd = std::process::Command::new("netsh");
    cmd.raw_arg(args).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    super::no_window(&mut cmd);
    cmd.status().is_ok_and(|s| s.success())
}

/// `set rule` for one existing Glass rule. A failure leaves that rule as it was.
/// Loopback has no allow to set.
fn set_rule_args(exe: &Path, remote: Remote) -> Option<String> {
    let remoteip = match remote {
        Remote::Loopback => return None,
        Remote::Any => "any".to_string(),
        Remote::Tailscale => format!("\"{TAILSCALE_V4},{TAILSCALE_V6}\""),
    };
    Some(format!(
        "advfirewall firewall set rule name=\"Glass\" dir=in program=\"{}\" new remoteip={remoteip} profile=private action=allow enable=yes edge=no",
        program(exe)
    ))
}

fn glass_name(name: &str) -> bool {
    name.trim().eq_ignore_ascii_case("Glass")
}

/// One `set` when a single Glass rule exists (a failed set leaves it untouched). An `add`
/// when there is none. More than one is deleted and replaced with a fresh rule, because
/// one `set` would change every match and still not be the single rule Glass checks for.
/// Loopback only deletes. Already admin. Success is [`in_sync`].
fn replace_rule(exe: &Path, remote: Remote) -> bool {
    let Some(listed) = program_rules(exe) else {
        log_scope("Couldn't list the firewall rules, so the Glass rule was left unchanged.");
        return false;
    };
    let cmds = replace_commands(exe, remote, &listed);
    if cmds.len() == 2 {
        let _ = netsh(&cmds[0]);
        if program_rules(exe).is_none_or(|rs| rs.iter().any(|r| glass_name(&r.name))) {
            log_scope("Couldn't remove the extra Glass firewall rules, so they were left unchanged.");
            return false;
        }
        let _ = netsh(&cmds[1]);
    } else if let Some(cmd) = cmds.first() {
        let _ = netsh(cmd);
    }
    if in_sync(exe, remote) == Some(true) {
        return true;
    }
    if cmds.len() == 2 {
        log_scope("Removed the extra Glass firewall rules, but couldn't add the new one.");
    }
    false
}

/// `set` for exactly one Glass rule, `add` for none, delete-then-add when there are more.
/// Loopback only deletes.
fn replace_commands(exe: &Path, remote: Remote, listed: &[Rule]) -> Vec<String> {
    if remote == Remote::Loopback {
        return vec![delete_rule_args(exe)];
    }
    let n = listed.iter().filter(|r| glass_name(&r.name)).count();
    let add = add_rule_args(exe, remote).expect("an allow remote has an add");
    match n {
        0 => vec![add],
        1 => vec![set_rule_args(exe, remote).expect("an allow remote has a set")],
        _ => vec![delete_rule_args(exe), add],
    }
}

/// Replaces Glass's rule from the saved bind. `false` leaves any existing rule as it is.
pub fn add_rule_quietly(exe: &Path) -> bool {
    match saved_remote() {
        Ok(remote) => apply_known(exe, remote),
        Err(()) => false,
    }
}

/// Replaces Glass's rule for `exe` with `remote` and stores that word for the installer.
/// Does not read settings. The store is HKLM, so only an elevated caller succeeds at it.
pub fn apply_known(exe: &Path, remote: Remote) -> bool {
    if !replace_rule(exe, remote) {
        log_scope("Couldn't update the Glass firewall rule.");
        return false;
    }
    if !store_scope(remote) {
        log_scope("The firewall rule was updated, but its scope was not saved for the next upgrade.");
    }
    true
}

/// Any rule named Glass for `exe`: any profile, enabled or not, allow or block.
/// `None` if the firewall can't be listed.
pub fn named_rule_present(exe: &Path) -> Option<bool> {
    Some(program_rules(exe)?.iter().any(|r| glass_name(&r.name)))
}

/// Removes Glass's rule for `exe`. `true` when no rule of that name remains,
/// including when there was nothing to delete.
pub fn remove_rule_quietly(exe: &Path) -> bool {
    let _ = netsh(&delete_rule_args(exe));
    program_rules(exe).is_some_and(|rs| !rs.iter().any(|r| glass_name(&r.name)))
}

/// Every rule named Glass for `exe`, whatever its profile. `None` if the firewall can't be listed.
fn glass_rules(exe: &Path) -> Option<Vec<Rule>> {
    Some(rules()?.into_iter().filter(|r| r.name.trim().eq_ignore_ascii_case("Glass") && same_program(&r.program, exe)).collect())
}

/// Hidden argument: relaunch this program elevated and run [`apply_from_arg`].
pub const APPLY_ARG: &str = "--apply-firewall-rule";

/// The only words [`parse_remote_arg`] accepts. The parent classified these; the child does not.
pub fn remote_arg(remote: Remote) -> &'static str {
    match remote {
        Remote::Tailscale => "tailscale",
        Remote::Loopback => "loopback",
        Remote::Any => "any",
    }
}

/// Read the machine scope instead of classifying again. The installer passes this.
pub const FROM_SAVED_SCOPE: &str = "--from-saved-scope";

/// What a machine-scope value means. Missing, blank, or unknown is the rule an older
/// MSI installed: private profile, any remote address. Never a settings-file read.
pub fn scope_from_saved(stored: Option<&str>) -> Remote {
    stored.map(str::trim).filter(|s| !s.is_empty()).and_then(parse_remote_arg).unwrap_or(Remote::Any)
}

/// The scope `--apply-firewall-rule` was given. `None` rejects the command.
pub fn scope_for_apply(rest: &[&str], saved: Option<&str>) -> Option<Remote> {
    match rest {
        [flag] if *flag == FROM_SAVED_SCOPE => Some(scope_from_saved(saved)),
        [word] => parse_remote_arg(word),
        _ => None,
    }
}

/// Whether the installer should write FirewallScope. The any-address fallback (nothing
/// usable was saved) is applied but not stored, so a later `glass service install` with
/// no scope still leaves that copy's rule alone.
pub fn persist_scope(from_saved: bool, stored: Option<&str>) -> bool {
    !from_saved || stored.map(str::trim).filter(|s| !s.is_empty()).and_then(parse_remote_arg).is_some()
}

/// Scope for an elevated `glass service install`. A passed word wins. Otherwise a valid
/// machine-scope word. Missing or unknown leaves the rule alone (it does not become any).
pub fn scope_for_service(passed: Option<&str>, persisted: Option<&str>) -> Option<Remote> {
    passed.and_then(|s| parse_remote_arg(s.trim())).or_else(|| persisted.and_then(|s| parse_remote_arg(s.trim())))
}

/// `Some` only for [`remote_arg`]. Anything else, including a different case, is refused.
pub fn parse_remote_arg(arg: &str) -> Option<Remote> {
    match arg {
        "tailscale" => Some(Remote::Tailscale),
        "loopback" => Some(Remote::Loopback),
        "any" => Some(Remote::Any),
        _ => None,
    }
}

/// `Start-Process -Verb RunAs` of `exe` with [`APPLY_ARG`] and [`remote_arg`]. The path is
/// one PowerShell literal, so spaces, `$` and `%` stay in the path. The child runs [`replace_rule`].
fn elevate_launch(exe: &Path, remote: Remote) -> String {
    let list = [APPLY_ARG, remote_arg(remote)].map(super::ps_string).join(",");
    format!(
        "$ErrorActionPreference='Stop'; $p = Start-Process -FilePath {} -ArgumentList {list} -Verb RunAs -WindowStyle Hidden -Wait -PassThru; exit $p.ExitCode",
        super::ps_string(&exe.display().to_string()),
    )
}

/// Replaces the rule as administrator. Windows asks once. The child is told `remote`;
/// it does not read settings. Already elevated: do it here.
pub fn add_rule_elevated(exe: &Path) -> bool {
    let Ok(remote) = saved_remote() else { return false };
    if super::service::is_elevated() {
        return apply_known(exe, remote);
    }
    super::powershell(&elevate_launch(exe, remote)).status().is_ok_and(|s| s.success())
}

/// Entry for [`APPLY_ARG`] plus one word, or [`FROM_SAVED_SCOPE`]. The program is this
/// executable. Never reads settings.
pub fn apply_from_arg(rest: &[String]) -> bool {
    if !super::service::is_elevated() {
        return false;
    }
    let saved = machine_scope();
    let words: Vec<&str> = rest.iter().map(String::as_str).collect();
    let Some(remote) = scope_for_apply(&words, saved.as_deref()) else { return false };
    let from_saved = words == [FROM_SAVED_SCOPE];
    let store = persist_scope(from_saved, saved.as_deref());
    if from_saved && !store {
        let detail = saved.as_deref().map(str::trim).filter(|s| !s.is_empty());
        log_scope(&match detail {
            None => "No saved firewall scope; restoring the private-network rule the older installer added.".into(),
            Some(word) => format!("Saved firewall scope {word:?} is not usable; restoring the private-network rule the older installer added."),
        });
    }
    let Ok(exe) = std::env::current_exe() else {
        log_scope("Couldn't find this program to update its firewall rule.");
        return false;
    };
    let updated = if store { apply_known(&exe, remote) } else { replace_rule(&exe, remote) };
    if !updated {
        if !store {
            log_scope("Couldn't update the Glass firewall rule.");
        }
        return false;
    }
    if remote == Remote::Loopback {
        log_scope("Saved firewall scope is loopback, so no inbound firewall rule was added.");
    }
    true
}

const SCOPE_KEY: &str = r"SOFTWARE\Glass";
const SCOPE_VALUE: &str = "FirewallScope";

pub fn machine_scope() -> Option<String> {
    let value = super::registry_string(SCOPE_KEY, SCOPE_VALUE)?;
    let value = value.trim().to_string();
    (!value.is_empty()).then_some(value)
}

/// Removes only the FirewallScope value. A missing key or value is success.
pub fn clear_machine_scope() -> Result<(), String> {
    use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND, ERROR_SUCCESS};
    // SAFETY: deletes one value. A missing key or value is success.
    let err = unsafe {
        windows::Win32::System::Registry::RegDeleteKeyValueW(
            windows::Win32::System::Registry::HKEY_LOCAL_MACHINE,
            &HSTRING::from(SCOPE_KEY),
            &HSTRING::from(SCOPE_VALUE),
        )
    };
    if err == ERROR_SUCCESS || err == ERROR_FILE_NOT_FOUND || err == ERROR_PATH_NOT_FOUND {
        Ok(())
    } else {
        Err(format!("Couldn't remove HKLM\\SOFTWARE\\Glass FirewallScope ({}).", err.0))
    }
}

fn store_scope(remote: Remote) -> bool {
    let wide: Vec<u16> = remote_arg(remote).encode_utf16().chain([0]).collect();
    // SAFETY: a NUL-terminated UTF-16 word and its size in bytes. Creates the key.
    unsafe {
        windows::Win32::System::Registry::RegSetKeyValueW(
            windows::Win32::System::Registry::HKEY_LOCAL_MACHINE,
            &windows::core::HSTRING::from(SCOPE_KEY),
            &windows::core::HSTRING::from(SCOPE_VALUE),
            windows::Win32::System::Registry::REG_SZ.0,
            Some(wide.as_ptr().cast()),
            (wide.len() * 2) as u32,
        )
        .is_ok()
    }
}

fn log_scope(msg: &str) {
    println!("{msg}");
    let dir = std::env::var_os("ProgramData").map(std::path::PathBuf::from).unwrap_or_else(|| std::path::PathBuf::from(r"C:\ProgramData"));
    let path = dir.join(r"Glass\firewall.log");
    let _ = std::fs::create_dir_all(path.parent().unwrap());
    let mut line = msg.to_string();
    line.push_str("\r\n");
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        use std::io::Write;
        let _ = file.write_all(line.as_bytes());
    }
}

/// Whether the Glass rules for one program are exactly what `remote` asks for.
/// Loopback means no rule of that name. Otherwise exactly one enabled inbound allow,
/// on the private profile only, with that address set.
fn rules_match(found: &[Rule], remote: Remote) -> bool {
    if remote == Remote::Loopback {
        return found.is_empty();
    }
    let [rule] = found else { return false };
    rule.enabled
        && rule.inbound
        && rule.allow
        && rule.profiles == NET_FW_PROFILE2_PRIVATE.0
        && firewall_bind::stored_matches(remote, Some(rule.remote.as_str()))
}

/// Whether Glass's own rule for `exe` already matches `remote`.
pub fn in_sync(exe: &Path, remote: Remote) -> Option<bool> {
    Some(glass_matches(&program_rules(exe)?, remote))
}

/// Every rule for `exe`. `None` if the firewall can't be listed.
pub fn program_rules(exe: &Path) -> Option<Vec<Rule>> {
    Some(rules()?.into_iter().filter(|r| same_program(&r.program, exe)).collect())
}

/// Whether the rules named Glass in `rules` are exactly what `remote` asks for.
pub fn glass_matches(rules: &[Rule], remote: Remote) -> bool {
    let glass: Vec<Rule> = rules.iter().filter(|r| glass_name(&r.name)).cloned().collect();
    rules_match(&glass, remote)
}

/// Enabled inbound allows for this program other than the rule named Glass.
pub fn other_allow_names(rules: &[Rule]) -> Vec<String> {
    let mut names: Vec<String> = rules
        .iter()
        .filter(|r| r.enabled && r.inbound && r.allow && !r.name.trim().eq_ignore_ascii_case("Glass"))
        .map(|r| {
            let name = r.name.trim();
            if name.is_empty() { "(unnamed)".into() } else { name.to_string() }
        })
        .collect();
    names.sort();
    names.dedup();
    names
}

/// What to say after `bind` changes. Nothing when the rule already matches and we are not elevated.
pub fn bind_change_note() -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    let remote = match saved_remote() {
        Ok(remote) => remote,
        Err(()) => return Some("Couldn't read the saved listen address, so the firewall rule was left unchanged.".into()),
    };
    if super::service::is_elevated() {
        return Some(if apply_known(&exe, remote) {
            match remote {
                Remote::Tailscale => format!("The firewall rule now allows only {TAILSCALE_V4} and {TAILSCALE_V6}."),
                Remote::Loopback => "The inbound firewall rule was removed; Glass only listens on this computer.".into(),
                Remote::Any => "The firewall rule allows connections on private networks.".into(),
            }
        } else {
            "Couldn't update the Windows Firewall rule.".into()
        });
    }
    if in_sync(&exe, remote) == Some(true) {
        return None;
    }
    Some(match remote {
        Remote::Tailscale => format!("The firewall rule was not changed. Run `glass setup` to allow only {TAILSCALE_V4} and {TAILSCALE_V6}."),
        Remote::Loopback => "The firewall rule was not changed. Run `glass setup` to remove the inbound allow.".into(),
        Remote::Any => "The firewall rule was not changed. Run `glass setup` to allow Glass on private networks.".into(),
    })
}

/// The setup line once the rule matches `remote`.
pub fn ready_sentence(remote: Remote) -> &'static str {
    match remote {
        Remote::Any => "Glass can accept connections on private networks",
        Remote::Tailscale => "Glass's firewall rule allows only Tailscale (100.64.0.0/10, fd7a:115c:a1e0::/48)",
        Remote::Loopback => "Glass only listens on this computer, so it has no inbound firewall rule",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(program: &str, allow: bool, profiles: i32) -> Rule {
        Rule { program: program.into(), enabled: true, inbound: true, allow, profiles, name: String::new(), remote: String::new() }
    }

    #[test]
    fn block_rules_win_and_only_private_ones_count() {
        let same = |p: &str| p.eq_ignore_ascii_case(r"C:\Glass\glass.exe");
        assert_eq!(decide(&[], same), Inbound::Missing);
        assert_eq!(decide(&[rule(r"c:\glass\glass.exe", true, 2)], same), Inbound::Allowed);
        assert_eq!(decide(&[rule(r"c:\glass\glass.exe", true, 0x7fff_ffff)], same), Inbound::Allowed);
        assert_eq!(decide(&[rule(r"c:\glass\glass.exe", true, 4)], same), Inbound::Missing);
        assert_eq!(decide(&[rule(r"c:\other.exe", true, 2)], same), Inbound::Missing);
        assert_eq!(decide(&[rule(r"c:\glass\glass.exe", true, 2), rule(r"C:\Glass\glass.exe", false, 2)], same), Inbound::Blocked);
        let mut off = rule(r"c:\glass\glass.exe", false, 2);
        off.enabled = false;
        assert_eq!(decide(&[off, rule(r"c:\glass\glass.exe", true, 3)], same), Inbound::Allowed);
    }

    #[test]
    fn the_rule_names_the_program_plainly() {
        let exe = Path::new(r"\\?\C:\Program Files\Glass\glass.exe");
        assert_eq!(
            add_rule_args(exe, Remote::Any).as_deref(),
            Some("advfirewall firewall add rule name=\"Glass\" dir=in action=allow program=\"C:\\Program Files\\Glass\\glass.exe\" enable=yes profile=private edge=no")
        );
        let tailscale = add_rule_args(exe, Remote::Tailscale).unwrap();
        assert!(tailscale.contains("remoteip=\"100.64.0.0/10,fd7a:115c:a1e0::/48\""));
        assert!(tailscale.ends_with("edge=no"));
        assert!(add_rule_args(exe, Remote::Loopback).is_none());
    }

    #[test]
    fn replace_sets_an_existing_rule_and_adds_only_when_missing() {
        let exe = Path::new(r"C:\Program Files\Glass\glass.exe");
        let prog = r"C:\Program Files\Glass\glass.exe";
        assert_eq!(
            set_rule_args(exe, Remote::Any).as_deref(),
            Some("advfirewall firewall set rule name=\"Glass\" dir=in program=\"C:\\Program Files\\Glass\\glass.exe\" new remoteip=any profile=private action=allow enable=yes edge=no")
        );
        assert_eq!(
            set_rule_args(exe, Remote::Tailscale).as_deref(),
            Some("advfirewall firewall set rule name=\"Glass\" dir=in program=\"C:\\Program Files\\Glass\\glass.exe\" new remoteip=\"100.64.0.0/10,fd7a:115c:a1e0::/48\" profile=private action=allow enable=yes edge=no")
        );
        let mut old = rule(prog, false, 4);
        old.enabled = false;
        old.name = "Glass".into();
        let set = replace_commands(exe, Remote::Tailscale, &[old.clone()]);
        assert_eq!(set, vec![set_rule_args(exe, Remote::Tailscale).unwrap()]);
        let any = replace_commands(exe, Remote::Any, &[old.clone()]);
        assert_eq!(any.len(), 1, "{any:?}");
        assert!(any[0].contains("remoteip=any") && any[0].contains("edge=no") && !any[0].contains("add rule"), "{any:?}");
        let added = replace_commands(exe, Remote::Any, &[]);
        assert_eq!(added, vec![add_rule_args(exe, Remote::Any).unwrap()]);
        let mut extra = old.clone();
        extra.profiles = NET_FW_PROFILE2_PRIVATE.0;
        let repair = replace_commands(exe, Remote::Any, &[old, extra]);
        assert_eq!(repair, vec![delete_rule_args(exe), add_rule_args(exe, Remote::Any).unwrap()], "{repair:?}");
        assert_eq!(replace_commands(exe, Remote::Loopback, &[]), vec![delete_rule_args(exe)]);
    }

    #[test]
    fn other_allows_are_named_and_are_not_the_glass_rule() {
        let mut glass = rule(r"C:\Glass\glass.exe", true, 2);
        glass.name = "Glass".into();
        let mut prompt = rule(r"C:\Glass\glass.exe", true, 4);
        prompt.name = "glass.exe".into();
        assert_eq!(other_allow_names(&[glass.clone(), prompt]), vec!["glass.exe".to_string()]);
        assert!(glass_matches(&[glass], Remote::Any));
    }

    #[test]
    fn elevate_relaunches_this_program_with_the_path_literal() {
        let spaces = elevate_launch(Path::new(r"C:\Program Files\Glass\glass.exe"), Remote::Tailscale);
        assert!(spaces.contains(r"-FilePath 'C:\Program Files\Glass\glass.exe'"), "{spaces}");
        assert!(spaces.contains("-ArgumentList '--apply-firewall-rule','tailscale'"), "{spaces}");
        let odd = elevate_launch(Path::new(r"C:\Glass$Bin\%ProgramFiles%\glass.exe"), Remote::Any);
        assert!(odd.contains(r"-FilePath 'C:\Glass$Bin\%ProgramFiles%\glass.exe'"), "{odd}");
        assert!(odd.contains("-ArgumentList '--apply-firewall-rule','any'"), "{odd}");
        assert!(!odd.contains("cmd.exe") && !odd.contains("netsh") && !odd.contains("APPDATA"), "{odd}");
    }

    #[test]
    fn remote_argument_rejects_unknown_values() {
        assert_eq!(parse_remote_arg("tailscale"), Some(Remote::Tailscale));
        assert_eq!(parse_remote_arg("loopback"), Some(Remote::Loopback));
        assert_eq!(parse_remote_arg("any"), Some(Remote::Any));
        for bad in ["", "Any", "Tailscale", "LOOPBACK", "all", "*", "any ", "tailscale,any"] {
            assert_eq!(parse_remote_arg(bad), None, "{bad}");
            assert!(!apply_from_arg(&[bad.to_string()]));
        }
        assert!(!apply_from_arg(&[]));
        assert!(!apply_from_arg(&["any".into(), "tailscale".into()]));
    }

    #[test]
    fn saved_scope_and_service_args_do_not_read_a_profile() {
        assert_eq!(scope_from_saved(None), Remote::Any);
        assert_eq!(scope_from_saved(Some("")), Remote::Any);
        assert_eq!(scope_from_saved(Some("  ")), Remote::Any);
        assert_eq!(scope_from_saved(Some("Any")), Remote::Any);
        assert_eq!(scope_from_saved(Some("wider")), Remote::Any);
        assert_eq!(scope_from_saved(Some(" tailscale ")), Remote::Tailscale);
        assert_eq!(scope_from_saved(Some("loopback")), Remote::Loopback);
        assert_eq!(scope_for_apply(&["--from-saved-scope"], None), Some(Remote::Any));
        assert_eq!(scope_for_apply(&["--from-saved-scope"], Some("nope")), Some(Remote::Any));
        assert_eq!(scope_for_apply(&["--from-saved-scope"], Some("tailscale")), Some(Remote::Tailscale));
        assert_eq!(scope_for_apply(&["tailscale"], Some("any")), Some(Remote::Tailscale));
        assert_eq!(scope_for_apply(&["nope"], None), None);
        assert_eq!(scope_for_apply(&["any", "tailscale"], None), None);
        assert_eq!(scope_for_apply(&[], Some("tailscale")), None);
        assert_eq!(scope_for_service(Some("tailscale"), Some("any")), Some(Remote::Tailscale));
        assert_eq!(scope_for_service(None, Some("loopback")), Some(Remote::Loopback));
        assert_eq!(scope_for_service(None, None), None);
        assert_eq!(scope_for_service(None, Some("missing-file")), None);
        assert_eq!(scope_for_service(Some("ANY"), Some("tailscale")), Some(Remote::Tailscale));
        assert!(!persist_scope(true, None));
        assert!(!persist_scope(true, Some("")));
        assert!(!persist_scope(true, Some("nope")));
        assert!(persist_scope(true, Some("any")));
        assert!(persist_scope(true, Some("loopback")));
        assert!(persist_scope(false, None));
    }

    #[test]
    fn in_sync_rejects_extra_rules_and_wider_profiles() {
        let private = NET_FW_PROFILE2_PRIVATE.0;
        let public = windows::Win32::NetworkManagement::WindowsFirewall::NET_FW_PROFILE2_PUBLIC.0;
        let mut good = rule(r"C:\Glass\glass.exe", true, private);
        good.name = "Glass".into();
        good.remote = "*".into();
        assert!(rules_match(&[good.clone()], Remote::Any));
        let mut public_only = good.clone();
        public_only.profiles = public;
        assert!(!rules_match(&[good.clone(), public_only.clone()], Remote::Any), "an extra public rule still counts");
        let mut wider = good.clone();
        wider.profiles = private | public;
        assert!(!rules_match(&[wider], Remote::Any), "private plus public is not private");
        assert!(!rules_match(&[public_only], Remote::Loopback), "a public rule is still a Glass rule");
        assert!(rules_match(&[], Remote::Loopback));
        let mut off = good.clone();
        off.enabled = false;
        assert!(!rules_match(&[off], Remote::Any));
        good.remote = format!("{TAILSCALE_V4},{TAILSCALE_V6}");
        assert!(rules_match(&[good.clone()], Remote::Tailscale));
        good.remote = TAILSCALE_V4.into();
        assert!(!rules_match(&[good], Remote::Tailscale));
    }

    /// Creates the Tailscale rule for this executable and reads back exactly both ranges.
    /// Skipped unless Windows CI sets GLASS_FIREWALL_INTEGRATION=1.
    #[test]
    fn windows_tailscale_rule_is_exactly_both_ranges() {
        if std::env::var("GLASS_FIREWALL_INTEGRATION").ok().as_deref() != Some("1") {
            return;
        }
        assert!(super::super::service::is_elevated(), "Windows CI must run elevated");
        let exe = std::env::current_exe().unwrap();
        struct Cleanup(std::path::PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = replace_rule(&self.0, Remote::Loopback);
            }
        }
        let _cleanup = Cleanup(exe.clone());
        assert!(replace_rule(&exe, Remote::Any), "create the wide rule");
        let wide = glass_rules(&exe).expect("list the wide rule");
        assert_eq!(wide.len(), 1, "wide rules: {wide:?}");
        assert!(firewall_bind::stored_matches(Remote::Any, wide.first().map(|r| r.remote.as_str())), "wide remote was {}", wide[0].remote);
        assert!(replace_rule(&exe, Remote::Tailscale), "replace the wide rule");
        let found = glass_rules(&exe).expect("list firewall rules");
        assert_eq!(found.len(), 1, "rules: {found:?}");
        assert_eq!(found[0].profiles, NET_FW_PROFILE2_PRIVATE.0, "profile was {}", found[0].profiles);
        assert_eq!(firewall_bind::normalized_remotes(&found[0].remote), vec![TAILSCALE_V4.to_string(), TAILSCALE_V6.to_string()], "remote addresses were {}", found[0].remote);
        assert!(replace_rule(&exe, Remote::Loopback), "remove the rule");
        assert!(glass_rules(&exe).expect("list firewall rules").is_empty());
    }
}
