//! Windows Defender Firewall. The inbound allow rule named "Glass" follows the saved
//! bind. Listing, creating, and deleting all go through `INetFwPolicy2`, so a path can
//! contain any character Windows allows in a file name.
//!
//! A narrowing reconcile deletes an owned wide allow before it adds the replacement.
//! If the replacement cannot be created, the wide allow stays gone and the error says
//! so. A block rule is never deleted and never skips that removal.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use windows::Win32::Foundation::{CloseHandle, ERROR_FILE_NOT_FOUND, ERROR_NO_MORE_ITEMS, HANDLE, HLOCAL, LocalFree, VARIANT_FALSE, VARIANT_TRUE, WAIT_ABANDONED, WAIT_OBJECT_0};
use windows::Win32::NetworkManagement::WindowsFirewall::{
    INetFwPolicy2, INetFwRule, INetFwRule2, INetFwRules, NET_FW_ACTION_ALLOW, NET_FW_ACTION_BLOCK, NET_FW_EDGE_TRAVERSAL_TYPE_ALLOW,
    NET_FW_EDGE_TRAVERSAL_TYPE_DEFER_TO_APP, NET_FW_EDGE_TRAVERSAL_TYPE_DEFER_TO_USER, NET_FW_EDGE_TRAVERSAL_TYPE_DENY,
    NET_FW_IP_PROTOCOL_ANY, NET_FW_RULE_DIR_IN, NetFwPolicy2, NetFwRule,
};
use windows::Win32::Security::Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW;
use windows::Win32::Security::{
    AdjustTokenPrivileges, LookupPrivilegeValueW, PSECURITY_DESCRIPTOR, SE_PRIVILEGE_ENABLED, SECURITY_ATTRIBUTES, TOKEN_ADJUST_PRIVILEGES,
    TOKEN_PRIVILEGES, TOKEN_QUERY,
};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize, IDispatch};
use windows::Win32::System::Environment::ExpandEnvironmentStringsW;
use windows::Win32::System::Ole::IEnumVARIANT;
use windows::Win32::System::Registry::{
    HKEY, HKEY_LOCAL_MACHINE, HKEY_USERS, KEY_READ, RRF_RT_REG_EXPAND_SZ, RRF_RT_REG_SZ, RegCloseKey, RegEnumKeyExW, RegGetValueW, RegLoadKeyW,
    RegOpenKeyExW, RegUnLoadKeyW,
};
use windows::Win32::System::Threading::{CreateMutexW, GetCurrentProcess, OpenProcessToken, ReleaseMutex, WaitForSingleObject};
use windows::Win32::System::Variant::{VARIANT, VT_DISPATCH, VariantClear};
use windows::core::{BSTR, HSTRING, Interface, w};

use super::firewall_scope::{
    AllowSpec, Coverage, Edge, FwRule, GROUPING, ListFail, PROFILE_PRIVATE, REMOTE_UNREADABLE, RULE_NAME, RemoteScope, ScopePlan, TAILSCALE_V4,
    TAILSCALE_V6, coverage, is_glass_name, manual_delete, manual_netsh, owned_allow, plan_from_binds, preview, programs_match,
    reconcile_stable, scope_for_saved, selected_programs,
};

/// Hidden argument the elevated helper runs. `glass setup` starts it, so changing bind
/// does not add an approval prompt of its own.
pub const SYNC_ARG: &str = "--glass-firewall-sync";

static RETIRE_SEQ: AtomicU64 = AtomicU64::new(1);

/// What the inbound rules say about a program on private networks.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Inbound {
    Allowed,
    /// A block rule wins over allow rules for the traffic it matches.
    Blocked,
    Missing,
}

/// Where one program's Glass rules stand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgramView {
    pub path: PathBuf,
    pub state: State,
    pub blocked: bool,
    pub foreign_wider: bool,
    pub detail: String,
    pub has_glass_allow: bool,
    pub scope: RemoteScope,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Ready,
    NeedsChange,
    Unreadable,
}

struct ComStore;

impl super::firewall_scope::FirewallStore for ComStore {
    fn list(&mut self) -> Result<Vec<FwRule>, ListFail> {
        read_rules()
    }

    fn add_allow(&mut self, program: &str, spec: AllowSpec) -> Result<(), String> {
        add_rule(program, spec)
    }

    fn remove_allow(&mut self, id: &str) -> Result<(), String> {
        retire(id, true)
    }
}

/// Whether `exe` may accept connections on private networks. `None` if the firewall
/// service can't be asked.
pub fn inbound(exe: &Path) -> Option<Inbound> {
    let rules = match read_rules() {
        Ok(rules) => rules,
        Err(ListFail::Incomplete(rules)) => rules,
        Err(ListFail::Unavailable(_)) => return None,
    };
    Some(match coverage(&rules, &exe.display().to_string()) {
        Coverage::Allowed => Inbound::Allowed,
        Coverage::Blocked => Inbound::Blocked,
        Coverage::Missing => Inbound::Missing,
    })
}

/// The per-user host, the all-users host, the secure-desktop copy, and this process
/// when it is one of those. `glassw.exe` does not listen. An elevated process also
/// picks up other users' per-user installs, which a setup started from the MSI copy
/// would otherwise miss.
pub fn programs() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        candidates.push(exe);
    }
    candidates.push(super::remove::host_exe());
    if let Some(dir) = super::agent::machine_dir() {
        candidates.push(dir.join(r"bin\glass.exe"));
    }
    if let Some(pf) = std::env::var_os("ProgramFiles") {
        candidates.push(PathBuf::from(pf).join(r"Glass\Service\glass.exe"));
    }
    if super::service::is_elevated() {
        candidates.extend(other_user_exes());
    }
    let flagged: Vec<(String, bool)> = candidates.iter().map(|path| (path.display().to_string(), path_present(path))).collect();
    let pairs: Vec<(&str, bool)> = flagged.iter().map(|(path, exists)| (path.as_str(), *exists)).collect();
    selected_programs(&pairs).into_iter().map(PathBuf::from).collect()
}

pub fn inspect() -> Vec<ProgramView> {
    let plan = scope_plan();
    let paths = programs();
    match read_rules() {
        Ok(rules) => views_from(&with_ruled(paths, &rules), &rules, plan, false),
        Err(ListFail::Incomplete(rules)) => views_from(&with_ruled(paths, &rules), &rules, plan, true),
        Err(ListFail::Unavailable(message)) => unreadable(&paths, plan.scope, &format!("Couldn't read Windows Firewall ({message}).")),
    }
}

/// Make every discovered program match the saved bind. Already elevated: no extra prompt.
/// One snapshot of the bind is used for the whole pass, and a named mutex keeps a second
/// setup from writing a wider rule over this one.
pub fn sync() -> Vec<ProgramView> {
    let Ok(_lock) = lock_firewall() else {
        let plan = scope_plan();
        return unreadable(&programs(), plan.scope, "Another Glass firewall update is still running. Run `glass setup` again.");
    };
    let paths = programs();
    // A program that was deleted can still have a Glass allow. Reconcile that path too.
    let paths = match read_rules() {
        Ok(rules) | Err(ListFail::Incomplete(rules)) => with_ruled(paths, &rules),
        Err(ListFail::Unavailable(_)) => paths,
    };
    let names: Vec<String> = paths.iter().map(|path| path.display().to_string()).collect();
    let mut store = ComStore;
    match reconcile_stable(&mut store, &names, scope_plan) {
        Ok(report) => {
            let plan = scope_plan();
            paths
                .iter()
                .map(|path| {
                    let found = report.programs.iter().find(|item| programs_match(&item.program, &path.display().to_string()));
                    ProgramView {
                        path: path.clone(),
                        state: State::Ready,
                        blocked: found.is_some_and(|item| item.blocked),
                        foreign_wider: found.is_some_and(|item| item.foreign_wider),
                        detail: String::new(),
                        has_glass_allow: found.is_some_and(|item| item.has_glass_allow),
                        scope: plan.scope,
                    }
                })
                .collect()
        }
        Err(error) => paths
            .into_iter()
            .map(|path| ProgramView {
                path,
                state: State::NeedsChange,
                blocked: false,
                foreign_wider: false,
                detail: error.message.clone(),
                has_glass_allow: false,
                scope: scope_plan().scope,
            })
            .collect(),
    }
}

/// One approval prompt (the same one setup already uses), then the rules are read back
/// here. The elevated child does not report success on its own stdout.
pub fn elevate_sync() -> Vec<ProgramView> {
    if !super::service::is_elevated() {
        let _ = launch_elevated();
        return inspect();
    }
    sync()
}

/// Entry point for [`SYNC_ARG`]. Refuses to run when not elevated, so it cannot loop
/// on the prompt.
pub fn run_sync() -> anyhow::Result<()> {
    anyhow::ensure!(super::service::is_elevated(), "administrator approval is required");
    let pending: Vec<_> = sync()
        .into_iter()
        .filter(|view| view.state != State::Ready)
        .map(|view| format!("{}: {}", view.path.display(), view.detail))
        .collect();
    anyhow::ensure!(pending.is_empty(), "{}", pending.join("\n"));
    Ok(())
}

/// What to tell the user after `glass config bind`. Applies the rule when this process
/// is already elevated; otherwise points at `glass setup` and does not ask for approval.
pub fn bind_change_note() -> Option<String> {
    let views = inspect();
    if views.is_empty() {
        return None;
    }
    if views.iter().all(|view| view.state == State::Unreadable) {
        return Some("Couldn't read Windows Firewall. Apply this listen address with: glass setup".into());
    }
    let needs = views.iter().any(|view| view.state == State::NeedsChange);
    let blocked = views.iter().any(|view| view.blocked);
    let foreign = views.iter().any(|view| view.foreign_wider);
    let wanted = views[0].scope;
    if !needs && blocked {
        return Some(blocked_hint().into());
    }
    if needs {
        let fresh_default = wanted == RemoteScope::Any && views.iter().all(|view| !view.has_glass_allow && view.detail.is_empty());
        if fresh_default && !foreign {
            return None;
        }
        if super::service::is_elevated() {
            return note_after(&sync());
        }
        return Some(change_hint(wanted, foreign));
    }
    foreign.then(|| foreign_warning(wanted).into())
}

pub fn ready_sentence(scope: RemoteScope) -> &'static str {
    match scope {
        RemoteScope::Any => "Glass can accept connections on private networks",
        RemoteScope::Tailscale => "Glass accepts connections from Tailscale only, on private networks",
        RemoteScope::Loopback | RemoteScope::Unknown => "Glass listens without a Glass inbound allow rule",
    }
}

pub fn blocked_hint() -> &'static str {
    "Windows Defender Firewall is blocking Glass (maybe its prompt was turned down). In\n\
     Windows Security › Firewall & network protection › Allow an app through firewall,\n\
     tick Private for Glass, then run `glass setup` again."
}

pub fn foreign_warning(scope: RemoteScope) -> &'static str {
    match scope {
        RemoteScope::Loopback | RemoteScope::Unknown => {
            "Another firewall rule for this program still allows inbound connections. Glass only changes rules it created."
        }
        RemoteScope::Tailscale => {
            "Another firewall rule for this program still allows more than Tailscale. Glass only changes rules it created."
        }
        RemoteScope::Any => "Another firewall rule for this program is still in place. Glass only changes rules it created.",
    }
}

/// Lines for `glass service install`. Quiet when the Glass rules already match.
pub fn install_lines(views: &[ProgramView]) -> Vec<String> {
    views
        .iter()
        .filter_map(|view| match view.state {
            State::NeedsChange | State::Unreadable => Some(format!(
                "Couldn't update the Windows Firewall rule for {}: {}",
                view.path.display(),
                view.detail
            )),
            State::Ready if view.blocked => Some(format!("{}: {}", view.path.display(), blocked_hint())),
            State::Ready if view.foreign_wider => Some(format!("{}: {}", view.path.display(), foreign_warning(view.scope))),
            State::Ready => None,
        })
        .collect()
}

/// Commands to print. A fresh default install only needs the add. `action=allow` keeps
/// the hint from deleting a block rule.
pub fn manual_command(path: &Path, scope: RemoteScope, has_glass_allow: bool) -> String {
    let program = path.to_string_lossy();
    let add = match scope {
        RemoteScope::Loopback | RemoteScope::Unknown => None,
        RemoteScope::Any => manual_netsh(&program, AllowSpec::Any),
        RemoteScope::Tailscale => manual_netsh(&program, AllowSpec::TailscaleBoth),
    };
    let delete = has_glass_allow.then(|| manual_delete(&program)).flatten();
    match (delete, add) {
        (Some(delete), Some(add)) => format!("netsh {delete}\n  netsh {add}"),
        (Some(delete), None) => format!("netsh {delete}"),
        (None, Some(add)) => format!("netsh {add}"),
        (None, None) => format!("Update the Glass firewall rule for {} in Windows Defender Firewall.", path.display()),
    }
}

/// Removes inbound rules named Glass for `exe`, allows and blocks. Other names stay.
/// Uninstall calls this. Synchronization uses [`ComStore::remove_allow`], which refuses
/// block rules.
pub fn remove_rule_quietly(exe: &Path) -> bool {
    delete_glass_rules(&exe.display().to_string()).is_ok()
}

/// Missing is absent. A permission error is still present: skipping it would leave its rule.
fn path_present(path: &Path) -> bool {
    match std::fs::metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(_) => true,
        Ok(meta) => meta.is_file(),
    }
}

/// Programs that already have a Glass-owned allow, even when the file is gone.
fn with_ruled(mut paths: Vec<PathBuf>, rules: &[FwRule]) -> Vec<PathBuf> {
    for rule in rules {
        if !owned_allow(rule) || rule.program.trim().is_empty() {
            continue;
        }
        if paths.iter().any(|have| programs_match(&have.display().to_string(), &rule.program)) {
            continue;
        }
        paths.push(PathBuf::from(&rule.program));
    }
    paths
}

fn views_from(paths: &[PathBuf], rules: &[FwRule], plan: ScopePlan, incomplete: bool) -> Vec<ProgramView> {
    let unknown = plan.scope == RemoteScope::Unknown;
    paths
        .iter()
        .map(|path| {
            let seen = preview(rules, &path.display().to_string(), plan);
            // Unknown is never Ready, even when nothing is installed yet.
            let needs = unknown || seen.needs_change || incomplete;
            ProgramView {
                path: path.clone(),
                state: if needs { State::NeedsChange } else { State::Ready },
                blocked: seen.blocked,
                foreign_wider: seen.foreign_wider,
                detail: if unknown {
                    "Couldn't read the saved bind. Run `glass setup` again so a wider Glass allow is not left behind.".into()
                } else if incomplete {
                    "Windows Firewall could not be listed completely. Run `glass setup` again.".into()
                } else {
                    String::new()
                },
                has_glass_allow: seen.has_glass_allow,
                scope: plan.scope,
            }
        })
        .collect()
}

fn unreadable(paths: &[PathBuf], scope: RemoteScope, detail: &str) -> Vec<ProgramView> {
    paths
        .iter()
        .map(|path| ProgramView {
            path: path.clone(),
            state: State::Unreadable,
            blocked: false,
            foreign_wider: false,
            detail: detail.into(),
            has_glass_allow: false,
            scope,
        })
        .collect()
}

fn note_after(views: &[ProgramView]) -> Option<String> {
    if views.iter().any(|view| view.state != State::Ready) {
        let detail = views
            .iter()
            .filter(|view| view.state != State::Ready)
            .map(|view| view.detail.clone())
            .collect::<Vec<_>>()
            .join(" ");
        return Some(format!("{detail}\nOr run: glass setup"));
    }
    let wanted = views.first().map(|view| view.scope).unwrap_or(RemoteScope::Any);
    if views.iter().any(|view| view.blocked) {
        return Some(blocked_hint().into());
    }
    let mut text = ready_sentence(wanted).to_string();
    if views.iter().any(|view| view.foreign_wider) {
        text.push(' ');
        text.push_str(foreign_warning(wanted));
    }
    Some(text)
}

fn change_hint(scope: RemoteScope, foreign: bool) -> String {
    let lead = match scope {
        RemoteScope::Tailscale => {
            "Windows Firewall still allows more than Tailscale. Apply the Tailscale-only rule with: glass setup"
        }
        RemoteScope::Loopback => "Windows Firewall still has an inbound rule for Glass. Remove it with: glass setup",
        RemoteScope::Unknown => "Couldn't read the saved listen address. Run `glass setup` again so a wider Glass allow is not left behind.",
        RemoteScope::Any => "Windows Firewall still has a different rule for Glass. Apply it with: glass setup",
    };
    if foreign { format!("{lead}\n{}", foreign_warning(scope)) } else { lead.into() }
}

/// Saved bind for this reconcile. An elevated process reads every registered profile's
/// real roaming AppData (including folder redirection) and the sign-in settings copy.
/// A file that is missing is the default bind. A file or profile that cannot be read
/// is [`RemoteScope::Unknown`]: owned allows are removed and nothing wide is created.
fn scope_plan() -> ScopePlan {
    let mut binds = Vec::new();
    let mut unreadable = false;
    consider_settings(&crate::settings::path(), &mut binds, &mut unreadable);
    if super::service::is_elevated() {
        consider_settings(&super::service::signin_dir().join("settings.json"), &mut binds, &mut unreadable);
        match accounts() {
            Ok(found) => {
                for account in found {
                    consider_settings(&account.roaming.join(r"Glass\settings.json"), &mut binds, &mut unreadable);
                }
            }
            Err(()) => unreadable = true,
        }
    }
    plan_from_binds(&binds, unreadable)
}

/// Missing is absent. Anything else, including a file that does not parse with
/// [`crate::listen::parse_bind`], is unreadable.
fn consider_settings(path: &Path, binds: &mut Vec<Vec<String>>, unreadable: &mut bool) {
    match std::fs::metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => *unreadable = true,
        Ok(meta) if !meta.is_file() => *unreadable = true,
        Ok(_) => match read_bind(path) {
            Ok(bind) => binds.push(bind),
            Err(()) => *unreadable = true,
        },
    }
}

fn read_bind(path: &Path) -> Result<Vec<String>, ()> {
    #[derive(serde::Deserialize)]
    struct File {
        #[serde(default)]
        bind: Vec<String>,
    }
    let bytes = std::fs::read(path).map_err(|_| ())?;
    let raw = serde_json::from_slice::<File>(&bytes).map(|file| file.bind).map_err(|_| ())?;
    // Same splitter the listener uses. A value it rejects is not a wide rule.
    scope_for_saved(&raw)?;
    crate::listen::parse_bind(&raw).map_err(|_| ())
}

struct Account {
    roaming: PathBuf,
    local: PathBuf,
}

/// Registered user profiles (`S-1-5-21-*`), with roaming and local AppData from the
/// user's shell folders. That is the redirected path when folder redirection is set.
/// `Err` if a profile exists and its folders cannot be read: guessing `C:\Users` would
/// hide a Tailscale bind and authorize a wide rule.
fn accounts() -> Result<Vec<Account>, ()> {
    let key = open_key(HKEY_LOCAL_MACHINE, r"SOFTWARE\Microsoft\Windows NT\CurrentVersion\ProfileList")?;
    let mut out = Vec::new();
    let mut index = 0u32;
    loop {
        let mut name = [0u16; 256];
        let mut len = name.len() as u32;
        let err = unsafe { RegEnumKeyExW(key, index, Some(windows::core::PWSTR(name.as_mut_ptr())), &mut len, None, None, None, None) };
        if err == ERROR_NO_MORE_ITEMS {
            break;
        }
        if err.is_err() {
            unsafe { let _ = RegCloseKey(key); }
            return Err(());
        }
        index += 1;
        let sid = String::from_utf16_lossy(&name[..len as usize]);
        if !sid.starts_with("S-1-5-21-") {
            continue;
        }
        let image = match reg_string(key, &sid, "ProfileImagePath") {
            Ok(path) => PathBuf::from(expand_env(&path)),
            Err(Missing::Gone) => continue,
            Err(Missing::Failed) => {
                unsafe { let _ = RegCloseKey(key); }
                return Err(());
            }
        };
        match account_folders(&sid, &image) {
            Ok(Some(account)) => out.push(account),
            Ok(None) => {}
            Err(()) => {
                unsafe { let _ = RegCloseKey(key); }
                return Err(());
            }
        }
    }
    unsafe { let _ = RegCloseKey(key); }
    Ok(out)
}

enum Missing {
    Gone,
    Failed,
}

fn account_folders(sid: &str, profile: &Path) -> Result<Option<Account>, ()> {
    let shell = format!(r"{sid}\Software\Microsoft\Windows\CurrentVersion\Explorer\User Shell Folders");
    if let Some(account) = shell_folders(HKEY_USERS, &shell, profile)? {
        return Ok(Some(account));
    }
    let dat = profile.join("NTUSER.DAT");
    // `is_file` is false for a permission error too. That is not "no profile".
    match std::fs::metadata(&dat) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(()),
        Ok(meta) if !meta.is_file() => return Err(()),
        Ok(_) => {}
    }
    let _priv = enable_restore();
    let loaded = format!("GlassFw{}", sid.replace('-', ""));
    let file = HSTRING::from(dat.as_os_str());
    let sub = HSTRING::from(&loaded);
    if unsafe { RegLoadKeyW(HKEY_USERS, &sub, &file) }.is_err() {
        return Err(());
    }
    let hive = Hive(loaded);
    let nested = format!(r"{}\Software\Microsoft\Windows\CurrentVersion\Explorer\User Shell Folders", hive.0);
    let found = shell_folders(HKEY_USERS, &nested, profile)?;
    drop(hive);
    found.map(Some).ok_or(())
}

struct Hive(String);

impl Drop for Hive {
    fn drop(&mut self) {
        unsafe {
            let _ = RegUnLoadKeyW(HKEY_USERS, &HSTRING::from(&self.0));
        }
    }
}

/// `Ok(None)` when that hive is not loaded. A value that cannot be read is `Err`.
fn shell_folders(root: HKEY, subkey: &str, profile: &Path) -> Result<Option<Account>, ()> {
    let key = match open_key(root, subkey) {
        Ok(key) => key,
        Err(()) => return Ok(None),
    };
    let roaming = folder_value(key, "AppData", profile, r"AppData\Roaming");
    let local = folder_value(key, "Local AppData", profile, r"AppData\Local");
    unsafe { let _ = RegCloseKey(key); }
    match (roaming, local) {
        (Ok(roaming), Ok(local)) => Ok(Some(Account { roaming, local })),
        _ => Err(()),
    }
}

fn folder_value(key: HKEY, value: &str, profile: &Path, fallback: &str) -> Result<PathBuf, Missing> {
    match reg_string(key, "", value) {
        Ok(raw) => Ok(PathBuf::from(expand_user_path(&raw, profile))),
        Err(Missing::Gone) => Ok(profile.join(fallback)),
        Err(Missing::Failed) => Err(Missing::Failed),
    }
}

fn expand_user_path(raw: &str, profile: &Path) -> String {
    let profile = profile.display().to_string();
    let lower = raw.to_ascii_lowercase();
    let raw = if let Some(at) = lower.find("%userprofile%") {
        format!("{}{profile}{}", &raw[..at], &raw[at + "%userprofile%".len()..])
    } else {
        raw.to_string()
    };
    expand_env(&raw)
}

fn open_key(root: HKEY, subkey: &str) -> Result<HKEY, ()> {
    let mut key = HKEY::default();
    let err = unsafe { RegOpenKeyExW(root, &HSTRING::from(subkey), None, KEY_READ, &mut key) };
    if err == ERROR_FILE_NOT_FOUND {
        return Err(());
    }
    err.ok().map(|_| key).map_err(|_| ())
}

fn reg_string(key: HKEY, subkey: &str, value: &str) -> Result<String, Missing> {
    let mut buf = [0u16; 1024];
    let mut len = (buf.len() * 2) as u32;
    let err = unsafe {
        RegGetValueW(
            key,
            &HSTRING::from(subkey),
            &HSTRING::from(value),
            RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut len),
        )
    };
    if err == ERROR_FILE_NOT_FOUND {
        return Err(Missing::Gone);
    }
    if err.is_err() {
        return Err(Missing::Failed);
    }
    let chars = (len as usize / 2).saturating_sub(1);
    Ok(String::from_utf16_lossy(&buf[..chars.min(buf.len())]))
}

fn enable_restore() -> bool {
    unsafe {
        let mut token = HANDLE::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY, &mut token).is_err() {
            return false;
        }
        let mut ok = true;
        for name in [w!("SeBackupPrivilege"), w!("SeRestorePrivilege")] {
            let mut luid = windows::Win32::Foundation::LUID::default();
            if LookupPrivilegeValueW(None, name, &mut luid).is_err() {
                ok = false;
                break;
            }
            let tp = TOKEN_PRIVILEGES {
                PrivilegeCount: 1,
                Privileges: [windows::Win32::Security::LUID_AND_ATTRIBUTES { Luid: luid, Attributes: SE_PRIVILEGE_ENABLED }],
            };
            if AdjustTokenPrivileges(token, false, Some(std::ptr::from_ref(&tp)), 0, None, None).is_err() {
                ok = false;
                break;
            }
        }
        let _ = CloseHandle(token);
        ok
    }
}

fn other_user_exes() -> Vec<PathBuf> {
    let Ok(found) = accounts() else { return Vec::new() };
    found.into_iter().map(|account| account.local.join(r"Programs\Glass\bin\glass.exe")).filter(|path| path_present(path)).collect()
}

struct FirewallLock(HANDLE);

impl Drop for FirewallLock {
    fn drop(&mut self) {
        unsafe {
            let _ = ReleaseMutex(self.0);
            let _ = CloseHandle(self.0);
        }
    }
}

fn lock_firewall() -> Result<FirewallLock, ()> {
    unsafe {
        // Administrators and SYSTEM only. A Local fallback would let an elevated
        // reconcile and this process run at the same time.
        let mut sd = PSECURITY_DESCRIPTOR::default();
        ConvertStringSecurityDescriptorToSecurityDescriptorW(w!("D:P(A;;GA;;;SY)(A;;GA;;;BA)"), 1, &mut sd, None).map_err(|_| ())?;
        let sa = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: sd.0,
            bInheritHandle: false.into(),
        };
        let handle = CreateMutexW(Some(std::ptr::from_ref(&sa)), false, w!("Global\\GlassFirewallReconcile"));
        let _ = LocalFree(Some(HLOCAL(sd.0)));
        let handle = handle.map_err(|_| ())?;
        let wait = WaitForSingleObject(handle, 60_000);
        if wait != WAIT_OBJECT_0 && wait != WAIT_ABANDONED {
            let _ = CloseHandle(handle);
            return Err(());
        }
        Ok(FirewallLock(handle))
    }
}

fn with_com<T>(body: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    // SAFETY: CoUninitialize runs on this thread only after a successful CoInitializeEx.
    // RPC_E_CHANGED_MODE means the thread already initialized COM, which is usable.
    let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    let out = body();
    if hr.is_ok() {
        unsafe { CoUninitialize() };
    }
    out
}

fn read_rules() -> Result<Vec<FwRule>, ListFail> {
    // COM init failure is reported as unavailable. A mid-enumeration failure is incomplete
    // and stays that way, so the caller can still drop a wide allow it did see.
    let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    let out = read_rules_com();
    if hr.is_ok() {
        unsafe { CoUninitialize() };
    }
    out
}

fn read_rules_com() -> Result<Vec<FwRule>, ListFail> {
    unsafe {
        let policy: INetFwPolicy2 = CoCreateInstance(&NetFwPolicy2, None, CLSCTX_INPROC_SERVER).map_err(|error| ListFail::Unavailable(error.to_string()))?;
        let all = policy.Rules().map_err(|error| ListFail::Unavailable(error.to_string()))?;
        let list: IEnumVARIANT = all
            ._NewEnum()
            .map_err(|error| ListFail::Unavailable(error.to_string()))?
            .cast()
            .map_err(|error| ListFail::Unavailable(error.to_string()))?;
        let mut out = Vec::new();
        let mut incomplete = false;
        loop {
            let mut values = [VARIANT::default()];
            let mut got = 0u32;
            if list.Next(&mut values, &mut got).is_err() {
                incomplete = true;
                break;
            }
            if got == 0 {
                break;
            }
            let inner = &values[0].Anonymous.Anonymous;
            let disp: Option<IDispatch> = if inner.vt == VT_DISPATCH { (*inner.Anonymous.pdispVal).clone() } else { None };
            let _ = VariantClear(&mut values[0]);
            let Some(rule) = disp.and_then(|disp| disp.cast::<INetFwRule>().ok()) else {
                incomplete = true;
                continue;
            };
            match read_one(&rule) {
                Read::Rule(rule) => out.push(rule),
                // Profiles or RemoteAddresses failed. Keep it so an owned allow is retired.
                Read::Partial(rule) => {
                    incomplete = true;
                    out.push(rule);
                }
                Read::Skip => {}
                Read::Failed => incomplete = true,
            }
        }
        if incomplete { Err(ListFail::Incomplete(out)) } else { Ok(out) }
    }
}

enum Read {
    Rule(FwRule),
    /// Identifiable, but Profiles or RemoteAddresses could not be read.
    Partial(FwRule),
    /// Not an allow or a block. It is not one of ours and not a hole in the list.
    Skip,
    Failed,
}

fn read_one(rule: &INetFwRule) -> Read {
    unsafe {
        let Some(name) = bstr(rule.Name()) else { return Read::Failed };
        let Some(program) = bstr(rule.ApplicationName()) else { return Read::Failed };
        let program = expand_env(&program);
        let Some(enabled) = rule.Enabled().ok() else { return Read::Failed };
        let enabled = enabled.as_bool();
        let Some(direction) = rule.Direction().ok() else { return Read::Failed };
        let inbound = direction == NET_FW_RULE_DIR_IN;
        let Some(action) = rule.Action().ok() else { return Read::Failed };
        let allow = if action == NET_FW_ACTION_ALLOW {
            true
        } else if action == NET_FW_ACTION_BLOCK {
            false
        } else {
            return Read::Skip;
        };
        let mut partial = false;
        let profiles = match rule.Profiles() {
            Ok(profiles) => profiles,
            Err(_) => {
                partial = true;
                -1
            }
        };
        let remote_addresses = match bstr(rule.RemoteAddresses()) {
            Some(remote) => remote,
            None => {
                partial = true;
                REMOTE_UNREADABLE.to_string()
            }
        };
        // A missing grouping is a historical rule. Failing the whole list on that
        // read would refuse every reconcile, because other programs' rules are in it.
        let grouping = bstr(rule.Grouping()).unwrap_or_default();
        let edge = edge_of(rule);
        let protocol = rule.Protocol();
        let protocol_any = protocol.is_err() || protocol.as_ref().is_ok_and(|protocol| *protocol == NET_FW_IP_PROTOCOL_ANY.0);
        let ports_any = text_any(rule.LocalPorts()) && text_any(rule.RemotePorts());
        let local_addresses_any = text_any(rule.LocalAddresses());
        let mut seen = FwRule {
            id: String::new(),
            name,
            program,
            enabled,
            inbound,
            allow,
            profiles,
            remote_addresses,
            edge,
            protocol_any,
            ports_any,
            local_addresses_any,
            grouping,
        };
        seen.id = fingerprint(&seen);
        if partial { Read::Partial(seen) } else { Read::Rule(seen) }
    }
}

fn fingerprint(rule: &FwRule) -> String {
    format!(
        "{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}",
        rule.name,
        rule.program,
        rule.enabled as u8,
        rule.allow as u8,
        rule.inbound as u8,
        rule.profiles,
        rule.remote_addresses,
        edge_code(rule.edge),
        rule.protocol_any as u8,
        rule.ports_any as u8,
        rule.local_addresses_any as u8,
        rule.grouping
    )
}

fn edge_code(edge: Edge) -> i32 {
    match edge {
        Edge::Deny => 0,
        Edge::Allow => 1,
        Edge::DeferToApp => 2,
        Edge::DeferToUser => 3,
        Edge::DenyOrDefer => 4,
        Edge::Unknown => 5,
    }
}

fn edge_of(rule: &INetFwRule) -> Edge {
    unsafe {
        if let Ok(rule2) = rule.cast::<INetFwRule2>()
            && let Ok(value) = rule2.EdgeTraversalOptions()
        {
            return if value == NET_FW_EDGE_TRAVERSAL_TYPE_DENY.0 {
                Edge::Deny
            } else if value == NET_FW_EDGE_TRAVERSAL_TYPE_ALLOW.0 {
                Edge::Allow
            } else if value == NET_FW_EDGE_TRAVERSAL_TYPE_DEFER_TO_APP.0 {
                Edge::DeferToApp
            } else if value == NET_FW_EDGE_TRAVERSAL_TYPE_DEFER_TO_USER.0 {
                Edge::DeferToUser
            } else {
                Edge::Unknown
            };
        }
        match rule.EdgeTraversal() {
            Ok(value) if value.as_bool() => Edge::Allow,
            Ok(_) => Edge::DenyOrDefer,
            Err(_) => Edge::Unknown,
        }
    }
}

fn bstr(value: windows::core::Result<BSTR>) -> Option<String> {
    value.ok().map(|text| text.to_string())
}

fn text_any(value: windows::core::Result<BSTR>) -> bool {
    match value {
        Err(_) => true,
        Ok(text) => {
            let text = text.to_string();
            let text = text.trim();
            text.is_empty() || text == "*" || text.eq_ignore_ascii_case("any")
        }
    }
}

fn expand_env(text: &str) -> String {
    if !text.contains('%') {
        return text.to_string();
    }
    let src = HSTRING::from(text);
    let mut buf = vec![0u16; 1024];
    // SAFETY: buf is writable. A path that does not fit is returned unchanged.
    let n = unsafe { ExpandEnvironmentStringsW(&src, Some(&mut buf)) } as usize;
    if n == 0 || n > buf.len() {
        return text.to_string();
    }
    String::from_utf16_lossy(&buf[..n - 1])
}

fn add_rule(program: &str, spec: AllowSpec) -> Result<(), String> {
    add_rule_grouped(program, spec, true)
}

fn add_rule_grouped(program: &str, spec: AllowSpec, grouping: bool) -> Result<(), String> {
    with_com(|| unsafe {
        let policy: INetFwPolicy2 = CoCreateInstance(&NetFwPolicy2, None, CLSCTX_INPROC_SERVER).map_err(|error| error.to_string())?;
        let rule: INetFwRule2 = CoCreateInstance(&NetFwRule, None, CLSCTX_INPROC_SERVER).map_err(|error| error.to_string())?;
        rule.SetName(&BSTR::from(RULE_NAME)).map_err(|error| format!("name: {error}"))?;
        if grouping {
            rule.SetGrouping(&BSTR::from(GROUPING)).map_err(|error| format!("grouping: {error}"))?;
        }
        rule.SetApplicationName(&BSTR::from(program)).map_err(|error| format!("application: {error}"))?;
        rule.SetEnabled(VARIANT_TRUE).map_err(|error| format!("enabled: {error}"))?;
        rule.SetDirection(NET_FW_RULE_DIR_IN).map_err(|error| format!("direction: {error}"))?;
        rule.SetAction(NET_FW_ACTION_ALLOW).map_err(|error| format!("action: {error}"))?;
        rule.SetProfiles(PROFILE_PRIVATE).map_err(|error| format!("profiles: {error}"))?;
        rule.SetProtocol(NET_FW_IP_PROTOCOL_ANY.0).map_err(|error| format!("protocol: {error}"))?;
        let remote = match spec {
            AllowSpec::Any => None,
            AllowSpec::TailscaleBoth => Some(format!("{TAILSCALE_V4},{TAILSCALE_V6}")),
            AllowSpec::TailscaleV4 => Some(TAILSCALE_V4.to_string()),
            AllowSpec::TailscaleV6 => Some(TAILSCALE_V6.to_string()),
        };
        if let Some(remote) = remote {
            rule.SetRemoteAddresses(&BSTR::from(remote)).map_err(|error| format!("remote: {error}"))?;
            rule.SetEdgeTraversalOptions(NET_FW_EDGE_TRAVERSAL_TYPE_DENY.0).map_err(|error| format!("edge: {error}"))?;
        }
        let rules = policy.Rules().map_err(|error| format!("rules: {error}"))?;
        let as_rule: INetFwRule = rule.cast().map_err(|error| format!("rule interface: {error}"))?;
        rules.Add(&as_rule).map_err(|error| format!("add: {error}"))
    })
}

/// `allows_only` is the sync path: a block rule must not be deleted. Uninstall passes
/// false and removes Glass-named inbound rules of either action for this program.
fn retire(id: &str, allows_only: bool) -> Result<(), String> {
    with_com(|| unsafe {
        let policy: INetFwPolicy2 = CoCreateInstance(&NetFwPolicy2, None, CLSCTX_INPROC_SERVER).map_err(|error| error.to_string())?;
        let rules = policy.Rules().map_err(|error| error.to_string())?;
        let rule = find_rule(&rules, id)?;
        if allows_only && rule.Action().map_err(|error| error.to_string())? != NET_FW_ACTION_ALLOW {
            return Err("refusing to delete a block rule".into());
        }
        // The marker has to stick before the name changes, or a crash after the rename
        // leaves an enabled rule that the next pass cannot recognize.
        rule.SetGrouping(&BSTR::from(GROUPING)).map_err(|error| {
            format!("couldn't mark the Glass rule ({error}). It was left named {RULE_NAME}.")
        })?;
        rule.SetEnabled(VARIANT_FALSE).map_err(|error| {
            format!("couldn't disable the Glass rule ({error}). It was left named {RULE_NAME}.")
        })?;
        let enabled = rule.Enabled().map_err(|error| {
            format!("couldn't confirm the Glass rule was disabled ({error}). It was left named {RULE_NAME}.")
        })?;
        if enabled.as_bool() {
            return Err(format!("couldn't disable the Glass rule (it stayed enabled). It was left named {RULE_NAME}."));
        }
        let unique = retirement_name();
        let unique_bstr = BSTR::from(&unique);
        rule.SetName(&unique_bstr).map_err(|error| {
            format!("couldn't rename the Glass rule ({error}). It was disabled, so it no longer allows traffic.")
        })?;
        rules.Remove(&unique_bstr).map_err(|error| {
            format!("couldn't delete the Glass rule ({error}). It was disabled, so it no longer allows traffic.")
        })
    })
}

fn retirement_name() -> String {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let seq = RETIRE_SEQ.fetch_add(1, Ordering::Relaxed);
    format!("Glass-{}-{nanos}-{seq}", std::process::id())
}

fn find_rule(rules: &INetFwRules, id: &str) -> Result<INetFwRule, String> {
    unsafe {
        let list: IEnumVARIANT = rules._NewEnum().map_err(|error| error.to_string())?.cast().map_err(|error| error.to_string())?;
        let mut fallback = Vec::new();
        loop {
            let mut values = [VARIANT::default()];
            let mut got = 0u32;
            if list.Next(&mut values, &mut got).is_err() {
                return Err("Windows Firewall could not be listed completely, so the rule was left in place".into());
            }
            if got == 0 {
                break;
            }
            let inner = &values[0].Anonymous.Anonymous;
            let disp: Option<IDispatch> = if inner.vt == VT_DISPATCH { (*inner.Anonymous.pdispVal).clone() } else { None };
            let _ = VariantClear(&mut values[0]);
            let Some(rule) = disp.and_then(|disp| disp.cast::<INetFwRule>().ok()) else { continue };
            let seen = match read_one(&rule) {
                Read::Rule(seen) | Read::Partial(seen) => seen,
                Read::Skip | Read::Failed => continue,
            };
            if seen.id == id {
                return Ok(rule);
            }
            // The first read stored a sentinel. The second read may now have the real
            // remote addresses, so the fingerprint no longer matches.
            if same_partial(id, &seen) {
                fallback.push((rule, seen));
            }
        }
        pick_fallback(fallback).ok_or_else(|| "the Glass rule was no longer there".into())
    }
}

/// The sought id came from a partial read. Match another owned allow for that program
/// whose other fields still agree, ignoring the fields that were unreadable.
fn same_partial(sought: &str, seen: &FwRule) -> bool {
    if !owned_allow(seen) {
        return false;
    }
    let sought_lines: Vec<&str> = sought.split('\n').collect();
    let live: Vec<&str> = seen.id.split('\n').collect();
    if sought_lines.len() != 12 || live.len() != 12 {
        return false;
    }
    let remote_partial = sought_lines[6] == REMOTE_UNREADABLE;
    let profiles_partial = sought_lines[5] == "-1";
    if !remote_partial && !profiles_partial {
        return false;
    }
    for i in 0..12 {
        if i == 2 {
            continue;
        }
        if i == 5 && profiles_partial {
            continue;
        }
        if i == 6 && remote_partial {
            continue;
        }
        if i == 11 && sought_lines[11].is_empty() {
            continue;
        }
        if sought_lines[i] != live[i] {
            return false;
        }
    }
    true
}

/// Prefer a rule that is still only partly readable. Otherwise prefer one that is not
/// an exact Tailscale allow, so a keeper is not the one removed when several match.
fn pick_fallback(mut candidates: Vec<(INetFwRule, FwRule)>) -> Option<INetFwRule> {
    if let Some(index) = candidates.iter().position(|(_, seen)| seen.remote_addresses == REMOTE_UNREADABLE || seen.profiles < 0) {
        return Some(candidates.swap_remove(index).0);
    }
    if let Some(index) = candidates.iter().position(|(_, seen)| !remote_looks_tailscale(&seen.remote_addresses)) {
        return Some(candidates.swap_remove(index).0);
    }
    candidates.into_iter().next().map(|(rule, _)| rule)
}

fn remote_looks_tailscale(raw: &str) -> bool {
    let raw = raw.trim().to_ascii_lowercase();
    !raw.is_empty()
        && !raw.contains('*')
        && !raw.contains("unreadable")
        && raw.split(',').all(|part| {
            let part = part.trim();
            part.is_empty() || part.contains("100.64.") || part.contains("fd7a:115c:a1e0")
        })
        && (raw.contains("100.64.") || raw.contains("fd7a:115c:a1e0"))
}

fn delete_glass_rules(program: &str) -> Result<(), String> {
    let rules = match read_rules() {
        Ok(rules) => rules,
        Err(ListFail::Incomplete(rules)) => rules,
        Err(ListFail::Unavailable(message)) => return Err(message),
    };
    for rule in rules {
        if rule.inbound && is_glass_name(&rule.name) && programs_match(&rule.program, program) {
            retire(&rule.id, false)?;
        }
    }
    Ok(())
}

fn launch_elevated() -> bool {
    let Ok(exe) = std::env::current_exe() else { return false };
    let script = format!(
        "$ErrorActionPreference='Stop'; $p = Start-Process -FilePath {} -ArgumentList '{}' -Verb RunAs -WindowStyle Hidden -Wait -PassThru; exit $p.ExitCode",
        super::ps_string(&exe.display().to_string()),
        SYNC_ARG
    );
    super::powershell(&script).status().is_ok_and(|status| status.success())
}

#[cfg(test)]
mod tests {
    use super::super::firewall_scope::{self, FirewallStore};
    use super::super::service;
    use super::*;
    use windows::Win32::NetworkManagement::WindowsFirewall::NET_FW_PROFILE2_PRIVATE;

    #[test]
    fn scope_constants_match_the_windows_headers() {
        assert_eq!(PROFILE_PRIVATE, NET_FW_PROFILE2_PRIVATE.0);
        assert_eq!(NET_FW_EDGE_TRAVERSAL_TYPE_DENY.0, 0);
        assert_eq!(NET_FW_EDGE_TRAVERSAL_TYPE_ALLOW.0, 1);
        assert_eq!(NET_FW_EDGE_TRAVERSAL_TYPE_DEFER_TO_APP.0, 2);
        assert_eq!(NET_FW_EDGE_TRAVERSAL_TYPE_DEFER_TO_USER.0, 3);
        assert_eq!(NET_FW_IP_PROTOCOL_ANY.0, 256);
    }

    /// Creates wide Glass allows for this test executable and a second copy, reconciles
    /// them, and checks the firewall service itself. Gated so a developer running the
    /// unit tests does not need to be an administrator.
    #[test]
    fn windows_reconcile_matches_the_firewall_service() {
        if std::env::var("GLASS_FIREWALL_INTEGRATION").ok().as_deref() != Some("1") {
            return;
        }
        assert!(service::is_elevated(), "GLASS_FIREWALL_INTEGRATION requires an elevated process");
        // The firewall service checks the image with GetBinaryType. A file that is
        // only the letters MZ is rejected with "The parameter is incorrect".
        let exe = std::env::current_exe().expect("test executable");
        let program = crate::browse::shell_path(&exe.display().to_string());
        // The firewall service reads the image itself. A copy under the user temp
        // directory is rejected with "The parameter is incorrect"; a sibling of this
        // executable is a path it can already open.
        let copy = exe.with_file_name(format!("glass-fw-{}-copy.exe", std::process::id()));
        std::fs::copy(&exe, &copy).expect("copy the test executable");
        let second = crate::browse::shell_path(&copy.display().to_string());
        struct Guard {
            programs: Vec<String>,
            copy: std::path::PathBuf,
        }
        impl Drop for Guard {
            fn drop(&mut self) {
                for program in &self.programs {
                    let _ = delete_glass_rules(program);
                }
                let _ = std::fs::remove_file(&self.copy);
            }
        }
        let _guard = Guard { programs: vec![program.clone(), second.clone()], copy };
        let _ = delete_glass_rules(&program);
        let _ = delete_glass_rules(&second);

        // A historical rule has the display name and no Grouping. The second program
        // gets a rule this build would create.
        add_rule_grouped(&program, AllowSpec::Any, false).expect("create the historical wide rule");
        let historical = glass_allows(&mut ComStore, &program);
        assert_eq!(historical.len(), 1);
        assert!(historical[0].grouping.is_empty(), "historical rules have no grouping");

        let mut store = ComStore;
        store
            .add_allow(&second, AllowSpec::Any)
            .unwrap_or_else(|error| panic!("create the second wide rule for {second}: {error}"));
        firewall_scope::reconcile(&mut store, &[program.as_str(), second.as_str()], ScopePlan { scope: RemoteScope::Tailscale })
            .expect("tailscale reconcile");
        assert_exact_tailscale(&mut store, &program);
        assert_exact_tailscale(&mut store, &second);

        firewall_scope::reconcile(&mut store, &[program.as_str(), second.as_str()], ScopePlan { scope: RemoteScope::Loopback })
            .expect("loopback reconcile");
        assert!(glass_allows(&mut store, &program).is_empty(), "loopback keeps no Glass allow");
        assert!(glass_allows(&mut store, &second).is_empty(), "loopback keeps no Glass allow");

        firewall_scope::reconcile(&mut store, &[program.as_str()], ScopePlan { scope: RemoteScope::Any }).expect("wide reconcile");
        let allows = glass_allows(&mut store, &program);
        assert_eq!(allows.len(), 1, "one wide Glass allow");
        assert!(allows[0].enabled && allows[0].inbound && allows[0].allow);
        assert!(programs_match(&allows[0].program, &program));
        assert_eq!(allows[0].profiles, PROFILE_PRIVATE);
        assert!(is_unrestricted(&allows[0].remote_addresses), "remote={}", allows[0].remote_addresses);
    }

    fn glass_allows(store: &mut ComStore, program: &str) -> Vec<FwRule> {
        store
            .list()
            .expect("list rules")
            .into_iter()
            .filter(|rule| owned_allow(rule) && programs_match(&rule.program, program))
            .collect()
    }

    fn assert_exact_tailscale(store: &mut ComStore, program: &str) {
        let allows = glass_allows(store, program);
        assert!(!allows.is_empty(), "a Tailscale allow should exist for {program}");
        for rule in &allows {
            assert!(rule.enabled, "enabled");
            assert!(rule.inbound, "inbound");
            assert!(rule.allow, "allow");
            assert!(programs_match(&rule.program, program), "program {}", rule.program);
            assert_eq!(rule.profiles, PROFILE_PRIVATE, "profile");
            assert_eq!(rule.edge, Edge::Deny, "edge {:?}", rule.edge);
            assert_eq!(rule.grouping, GROUPING);
        }
        let remotes: Vec<&str> = allows.iter().map(|rule| rule.remote_addresses.as_str()).collect();
        assert!(firewall_scope::covers_tailscale_exactly(&remotes), "remotes {remotes:?}");
    }

    fn is_unrestricted(raw: &str) -> bool {
        let raw = raw.trim();
        raw.is_empty() || raw == "*" || raw.eq_ignore_ascii_case("any")
    }
}
