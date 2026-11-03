//! Windows Defender Firewall. The inbound allow rule named "Glass" follows the saved
//! bind. Listing, creating, and deleting all go through `INetFwPolicy2`, so a path can
//! contain any character Windows allows in a file name.
//!
//! A narrowing reconcile deletes an owned wide allow before it adds the replacement.
//! If the replacement cannot be created, the wide allow stays gone and the error says
//! so. A block rule is never deleted and never skips that removal.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use windows::Win32::Foundation::{CloseHandle, HANDLE, VARIANT_FALSE, VARIANT_TRUE, WAIT_ABANDONED, WAIT_OBJECT_0};
use windows::Win32::NetworkManagement::WindowsFirewall::{
    INetFwPolicy2, INetFwRule, INetFwRule2, INetFwRules, NET_FW_ACTION_ALLOW, NET_FW_ACTION_BLOCK, NET_FW_EDGE_TRAVERSAL_TYPE_ALLOW,
    NET_FW_EDGE_TRAVERSAL_TYPE_DEFER_TO_APP, NET_FW_EDGE_TRAVERSAL_TYPE_DEFER_TO_USER, NET_FW_EDGE_TRAVERSAL_TYPE_DENY,
    NET_FW_IP_PROTOCOL_ANY, NET_FW_RULE_DIR_IN, NetFwPolicy2, NetFwRule,
};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize, IDispatch};
use windows::Win32::System::Environment::ExpandEnvironmentStringsW;
use windows::Win32::System::Ole::IEnumVARIANT;
use windows::Win32::System::Threading::{CreateMutexW, ReleaseMutex, WaitForSingleObject};
use windows::Win32::System::Variant::{VARIANT, VT_DISPATCH, VariantClear};
use windows::core::{BSTR, HSTRING, Interface, w};

use super::firewall_scope::{
    AllowSpec, Coverage, Edge, FwRule, ListFail, PROFILE_PRIVATE, RULE_NAME, RemoteScope, ScopePlan, TAILSCALE_V4, TAILSCALE_V6,
    coverage, is_glass_name, manual_delete, manual_netsh, plan_from_binds, preview, programs_match, reconcile_stable, selected_programs,
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
    let flagged: Vec<(String, bool)> = candidates.iter().map(|path| (path.display().to_string(), path.is_file())).collect();
    let pairs: Vec<(&str, bool)> = flagged.iter().map(|(path, exists)| (path.as_str(), *exists)).collect();
    selected_programs(&pairs).into_iter().map(PathBuf::from).collect()
}

pub fn inspect() -> Vec<ProgramView> {
    let plan = scope_plan();
    let paths = programs();
    match read_rules() {
        Ok(rules) => views_from(&paths, &rules, plan, false),
        Err(ListFail::Incomplete(rules)) => views_from(&paths, &rules, plan, true),
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
        RemoteScope::Loopback => "Glass listens on this computer only, so it has no inbound firewall rule",
    }
}

pub fn blocked_hint() -> &'static str {
    "Windows Defender Firewall is blocking Glass (maybe its prompt was turned down). In\n\
     Windows Security › Firewall & network protection › Allow an app through firewall,\n\
     tick Private for Glass, then run `glass setup` again."
}

pub fn foreign_warning(scope: RemoteScope) -> &'static str {
    match scope {
        RemoteScope::Loopback => {
            "Another firewall rule for this program still allows inbound connections. Glass only changes rules named Glass."
        }
        RemoteScope::Tailscale => {
            "Another firewall rule for this program still allows more than Tailscale. Glass only changes rules named Glass."
        }
        RemoteScope::Any => "Another firewall rule for this program is still in place. Glass only changes rules named Glass.",
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
        RemoteScope::Loopback => None,
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

fn views_from(paths: &[PathBuf], rules: &[FwRule], plan: ScopePlan, incomplete: bool) -> Vec<ProgramView> {
    paths
        .iter()
        .map(|path| {
            let seen = preview(rules, &path.display().to_string(), plan);
            let needs = seen.needs_change || incomplete;
            ProgramView {
                path: path.clone(),
                state: if needs { State::NeedsChange } else { State::Ready },
                blocked: seen.blocked,
                foreign_wider: seen.foreign_wider,
                detail: if incomplete {
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
        RemoteScope::Any => "Windows Firewall still has a different rule for Glass. Apply it with: glass setup",
    };
    if foreign { format!("{lead}\n{}", foreign_warning(scope)) } else { lead.into() }
}

/// Saved bind for this reconcile. An elevated process also reads other users' settings,
/// and the narrowest of those wins, because the Program Files copy has one rule shared
/// by every account. A profile that cannot be read suppresses a new wide rule.
fn scope_plan() -> ScopePlan {
    if !super::service::is_elevated() {
        let path = crate::settings::path();
        if !path.is_file() {
            return plan_from_binds(&[], false);
        }
        return match read_bind(&path) {
            Ok(bind) => plan_from_binds(&[bind], false),
            Err(()) => plan_from_binds(&[], true),
        };
    }
    let mut binds = Vec::new();
    let mut unreadable = false;
    let own = crate::settings::path();
    if own.is_file() {
        match read_bind(&own) {
            Ok(bind) => binds.push(bind),
            Err(()) => unreadable = true,
        }
    }
    let (more, failed) = other_user_binds(&own);
    binds.extend(more);
    unreadable |= failed;
    plan_from_binds(&binds, unreadable)
}

fn read_bind(path: &Path) -> Result<Vec<String>, ()> {
    #[derive(serde::Deserialize)]
    struct File {
        #[serde(default)]
        bind: Vec<String>,
    }
    let bytes = std::fs::read(path).map_err(|_| ())?;
    serde_json::from_slice::<File>(&bytes).map(|file| file.bind).map_err(|_| ())
}

/// Binds that could be read, and whether any profile was unreadable. A readable
/// Tailscale setting is kept even when another profile cannot be parsed.
fn other_user_binds(skip: &Path) -> (Vec<Vec<String>>, bool) {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(users_dir().unwrap_or_default()) else { return (out, true) };
    let mut unreadable = false;
    for entry in entries.flatten() {
        if skip_profile(&entry.file_name().to_string_lossy()) {
            continue;
        }
        let file = entry.path().join(r"AppData\Roaming\Glass\settings.json");
        if !file.is_file() || programs_match(&file.display().to_string(), &skip.display().to_string()) {
            continue;
        }
        match read_bind(&file) {
            Ok(bind) => out.push(bind),
            Err(()) => unreadable = true,
        }
    }
    (out, unreadable)
}

fn other_user_exes() -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(users_dir().unwrap_or_default()) else { return Vec::new() };
    entries
        .flatten()
        .filter(|entry| !skip_profile(&entry.file_name().to_string_lossy()))
        .map(|entry| entry.path().join(r"AppData\Local\Programs\Glass\bin\glass.exe"))
        .filter(|path| path.is_file())
        .collect()
}

fn users_dir() -> Option<PathBuf> {
    let drive = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into());
    Some(PathBuf::from(format!(r"{drive}\Users")))
}

fn skip_profile(name: &str) -> bool {
    matches!(name, "Public" | "Default" | "Default User" | "All Users")
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
        // Global is visible to the elevated helper and to SYSTEM during an MSI install.
        // A non-elevated token cannot create it; Local still serializes this session.
        let handle = CreateMutexW(None, false, w!("Global\\GlassFirewallReconcile"))
            .or_else(|_| CreateMutexW(None, false, w!("Local\\GlassFirewallReconcile")))
            .map_err(|_| ())?;
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
                Read::Skip => {}
                Read::Failed => incomplete = true,
            }
        }
        if incomplete { Err(ListFail::Incomplete(out)) } else { Ok(out) }
    }
}

enum Read {
    Rule(FwRule),
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
        let Some(profiles) = rule.Profiles().ok() else { return Read::Failed };
        let Some(remote_addresses) = bstr(rule.RemoteAddresses()) else { return Read::Failed };
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
        };
        seen.id = fingerprint(&seen);
        Read::Rule(seen)
    }
}

fn fingerprint(rule: &FwRule) -> String {
    format!(
        "{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}",
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
        rule.local_addresses_any as u8
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
    with_com(|| unsafe {
        let policy: INetFwPolicy2 = CoCreateInstance(&NetFwPolicy2, None, CLSCTX_INPROC_SERVER).map_err(|error| error.to_string())?;
        let rule: INetFwRule2 = CoCreateInstance(&NetFwRule, None, CLSCTX_INPROC_SERVER).map_err(|error| error.to_string())?;
        rule.SetName(&BSTR::from(RULE_NAME)).map_err(|error| format!("name: {error}"))?;
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
        // Disable first. A crash between this and Remove leaves no wide allow in force.
        let _ = rule.SetEnabled(VARIANT_FALSE);
        let unique = BSTR::from(format!("Glass-{}", RETIRE_SEQ.fetch_add(1, Ordering::Relaxed)));
        rule.SetName(&unique).map_err(|error| format!("couldn't rename the Glass rule ({error})"))?;
        rules.Remove(&unique).map_err(|error| {
            format!("couldn't delete the Glass rule ({error}). It was disabled, so it no longer allows traffic.")
        })
    })
}

fn find_rule(rules: &INetFwRules, id: &str) -> Result<INetFwRule, String> {
    unsafe {
        let list: IEnumVARIANT = rules._NewEnum().map_err(|error| error.to_string())?.cast().map_err(|error| error.to_string())?;
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
            if let Read::Rule(seen) = read_one(&rule)
                && seen.id == id
            {
                return Ok(rule);
            }
        }
        Err("the Glass rule was no longer there".into())
    }
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

    /// Creates a wide Glass allow for a dummy executable, reconciles it to Tailscale,
    /// then loopback, then wide again, and checks the firewall service itself. Gated so
    /// a developer running the unit tests does not need to be an administrator.
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
        struct Guard(String);
        impl Drop for Guard {
            fn drop(&mut self) {
                let _ = delete_glass_rules(&self.0);
            }
        }
        let _guard = Guard(program.clone());
        let _ = delete_glass_rules(&program);

        let mut store = ComStore;
        store.add_allow(&program, AllowSpec::Any).expect("create the wide rule");
        firewall_scope::reconcile(&mut store, &[program.as_str()], ScopePlan { scope: RemoteScope::Tailscale, allow_wide: false })
            .expect("tailscale reconcile");
        assert_only_tailscale(&mut store, &program);

        firewall_scope::reconcile(&mut store, &[program.as_str()], ScopePlan { scope: RemoteScope::Loopback, allow_wide: false })
            .expect("loopback reconcile");
        assert!(glass_allows(&mut store, &program).is_empty(), "loopback keeps no Glass allow");

        firewall_scope::reconcile(&mut store, &[program.as_str()], ScopePlan { scope: RemoteScope::Any, allow_wide: true }).expect("wide reconcile");
        let allows = glass_allows(&mut store, &program);
        assert_eq!(allows.len(), 1, "one wide Glass allow");
        assert!(is_unrestricted(&allows[0].remote_addresses), "remote={}", allows[0].remote_addresses);
    }

    fn glass_allows(store: &mut ComStore, program: &str) -> Vec<FwRule> {
        store
            .list()
            .expect("list rules")
            .into_iter()
            .filter(|rule| rule.inbound && rule.allow && is_glass_name(&rule.name) && programs_match(&rule.program, program))
            .collect()
    }

    fn assert_only_tailscale(store: &mut ComStore, program: &str) {
        let allows = glass_allows(store, program);
        assert!(!allows.is_empty(), "a Tailscale allow should exist");
        for rule in &allows {
            assert!(!is_unrestricted(&rule.remote_addresses), "wide remote {}", rule.remote_addresses);
            let remote = rule.remote_addresses.to_lowercase();
            assert!(
                remote.contains("100.64.0.0") || remote.contains("fd7a:115c:a1e0"),
                "remote {}",
                rule.remote_addresses
            );
            assert_eq!(rule.edge, Edge::Deny, "edge {:?}", rule.edge);
            assert_eq!(rule.profiles, PROFILE_PRIVATE);
        }
    }

    fn is_unrestricted(raw: &str) -> bool {
        let raw = raw.trim();
        raw.is_empty() || raw == "*" || raw.eq_ignore_ascii_case("any")
    }
}
