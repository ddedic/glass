//! Windows Defender Firewall: whether it lets Glass accept connections, and the rule that
//! does. Without one, Windows asks the first time the host listens, on the computer's own
//! screen, and turning that down leaves Glass blocked.
//!
//! The rule Glass creates is named "Glass" and follows `glass config bind` (see
//! `firewall_scope`). A rule left by `netsh` without `edge=` is edge-traversal
//! "defer to user", and Windows will not attach a remote-address filter to those: Glass
//! deletes that rule and creates a new one instead of editing it. Rules under any other
//! name are left alone.

use std::path::{Path, PathBuf};

use windows::Win32::NetworkManagement::WindowsFirewall::{
    INetFwPolicy2, INetFwRule, INetFwRule2, NET_FW_ACTION_ALLOW, NET_FW_EDGE_TRAVERSAL_TYPE_ALLOW,
    NET_FW_EDGE_TRAVERSAL_TYPE_DEFER_TO_APP, NET_FW_EDGE_TRAVERSAL_TYPE_DEFER_TO_USER,
    NET_FW_EDGE_TRAVERSAL_TYPE_DENY, NET_FW_IP_PROTOCOL_ANY, NET_FW_PROFILE2_PRIVATE,
    NET_FW_RULE_DIR_IN, NetFwPolicy2,
};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, IDispatch};
use windows::Win32::System::Environment::ExpandEnvironmentStringsW;
use windows::Win32::System::Ole::IEnumVARIANT;
use windows::Win32::System::Variant::{VARIANT, VT_DISPATCH, VariantClear};
use windows::core::{HSTRING, Interface};

use super::firewall_scope::{
    AddSpec, Decision, Disposition, EdgeTraversal, FirewallAction, ObservedRule, RULE_NAME,
    RemoteScope, TAILSCALE_V4, TAILSCALE_V6, decide_sync, dispositions, is_glass_name,
    netsh_add_rule, netsh_delete_inbound, parse_edge_text, parse_enabled, parse_profile,
    parse_remote_addresses, text_means_any,
};

/// Hidden argument the elevated helper runs. Not a user-facing command: `glass setup`
/// starts it, so changing bind does not add an approval prompt of its own.
pub const SYNC_ARG: &str = "--glass-firewall-sync";

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
    pub name: String,
    pub program: String,
    pub enabled: bool,
    pub inbound: bool,
    pub allow: bool,
    pub profiles: i32,
    pub remote_addresses: String,
    pub edge: EdgeTraversal,
    pub protocol_any: bool,
    pub local_ports_any: bool,
    pub remote_ports_any: bool,
    pub local_addresses_any: bool,
}

/// Where one program's Glass rules stand, before or after a sync.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgramView {
    pub path: PathBuf,
    pub state: State,
    pub foreign_wider: bool,
    /// Set when a sync was attempted and the rules still don't match.
    pub detail: String,
    pub has_glass_allow: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Ready,
    NeedsChange,
    Blocked,
    Unreadable,
}

/// Decides from the rules for `exe`; `same` says whether a rule's program is it.
pub fn decide(rules: &[Rule], same: impl Fn(&str) -> bool) -> Inbound {
    let (block, allow) = rules
        .iter()
        .filter(|r| {
            r.enabled
                && r.inbound
                && r.profiles & NET_FW_PROFILE2_PRIVATE.0 != 0
                && same(&r.program)
        })
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
    expand(rule).to_lowercase() == shell(&exe.to_string_lossy()).to_lowercase()
}

fn shell(path: &str) -> String {
    crate::browse::shell_path(path)
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
            let policy: INetFwPolicy2 =
                CoCreateInstance(&NetFwPolicy2, None, CLSCTX_INPROC_SERVER).ok()?;
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
                let disp: Option<IDispatch> = if inner.vt == VT_DISPATCH {
                    (*inner.Anonymous.pdispVal).clone()
                } else {
                    None
                };
                let _ = VariantClear(&mut v[0]);
                let Some(rule) = disp.and_then(|d| d.cast::<INetFwRule>().ok()) else {
                    continue;
                };
                let protocol = rule.Protocol();
                out.push(Rule {
                    name: bstr(rule.Name()),
                    program: bstr(rule.ApplicationName()),
                    enabled: rule.Enabled().is_ok_and(|e| e.as_bool()),
                    inbound: rule.Direction().is_ok_and(|d| d == NET_FW_RULE_DIR_IN),
                    allow: rule.Action().is_ok_and(|a| a == NET_FW_ACTION_ALLOW),
                    profiles: rule.Profiles().unwrap_or(0),
                    remote_addresses: bstr(rule.RemoteAddresses()),
                    edge: edge_of(&rule),
                    // A missing property is the default (any). A real restriction is a number
                    // other than "any", and that rule is not one of ours.
                    protocol_any: protocol.is_err()
                        || protocol
                            .as_ref()
                            .is_ok_and(|p| *p == NET_FW_IP_PROTOCOL_ANY.0),
                    local_ports_any: rule.LocalPorts().is_err()
                        || rule
                            .LocalPorts()
                            .is_ok_and(|s| text_means_any(&s.to_string())),
                    remote_ports_any: rule.RemotePorts().is_err()
                        || rule
                            .RemotePorts()
                            .is_ok_and(|s| text_means_any(&s.to_string())),
                    local_addresses_any: rule.LocalAddresses().is_err()
                        || rule
                            .LocalAddresses()
                            .is_ok_and(|s| text_means_any(&s.to_string())),
                });
            }
            Some(out)
        }
    })
}

fn bstr(value: windows::core::Result<windows::core::BSTR>) -> String {
    value.map(|s| s.to_string()).unwrap_or_default()
}

fn edge_of(rule: &INetFwRule) -> EdgeTraversal {
    // SAFETY: the rule pointer is live; these getters only read.
    if let Ok(rule2) = rule.cast::<INetFwRule2>()
        && let Ok(value) = unsafe { rule2.EdgeTraversalOptions() }
    {
        return if value == NET_FW_EDGE_TRAVERSAL_TYPE_DENY.0 {
            EdgeTraversal::Deny
        } else if value == NET_FW_EDGE_TRAVERSAL_TYPE_ALLOW.0 {
            EdgeTraversal::Allow
        } else if value == NET_FW_EDGE_TRAVERSAL_TYPE_DEFER_TO_APP.0 {
            EdgeTraversal::DeferToApp
        } else if value == NET_FW_EDGE_TRAVERSAL_TYPE_DEFER_TO_USER.0 {
            EdgeTraversal::DeferToUser
        } else {
            EdgeTraversal::Unknown
        };
    }
    match unsafe { rule.EdgeTraversal() } {
        Ok(value) if value.as_bool() => EdgeTraversal::Allow,
        Ok(_) => EdgeTraversal::DenyOrDeferUser,
        Err(_) => EdgeTraversal::Unknown,
    }
}

/// Whether `exe` may accept connections on private networks; `None` if unknown.
pub fn inbound(exe: &Path) -> Option<Inbound> {
    Some(decide(&rules()?, |p| same_program(p, exe)))
}

/// Saved `glass config bind`, not a one-off `--bind` (the firewall is per program, and the
/// sign-in host uses the saved settings).
pub fn scope() -> RemoteScope {
    super::firewall_scope::scope_for_bind(&crate::settings::load().bind)
}

/// The per-user host, the all-users host under Program Files, and the secure-desktop
/// copy. The sign-in host is that last copy, not a third program.
pub fn programs() -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut push = |path: PathBuf| {
        // glassw starts the host; it never listens, so it gets no rule.
        if path
            .file_name()
            .is_some_and(|name| name.eq_ignore_ascii_case("glassw.exe"))
        {
            return;
        }
        if out
            .iter()
            .any(|have: &PathBuf| same_program(&have.to_string_lossy(), &path))
        {
            return;
        }
        out.push(path);
    };
    if let Ok(exe) = std::env::current_exe() {
        push(exe);
    }
    if let Some(dir) = super::agent::machine_dir() {
        let host = dir.join(r"bin\glass.exe");
        if host.exists() {
            push(host);
        }
    }
    if let Some(pf) = std::env::var_os("ProgramFiles") {
        let svc = PathBuf::from(pf).join(r"Glass\Service\glass.exe");
        if svc.exists() {
            push(svc);
        }
    }
    out
}

pub fn inspect() -> Vec<ProgramView> {
    let paths = programs();
    let Some(all) = rules() else {
        return paths
            .into_iter()
            .map(|path| ProgramView {
                path,
                state: State::Unreadable,
                foreign_wider: false,
                detail: String::new(),
                has_glass_allow: false,
            })
            .collect();
    };
    paths
        .into_iter()
        .map(|path| view_of(&path, &all, ""))
        .collect()
}

/// Make every program's Glass rules match [`scope`]. Already elevated: no extra prompt.
pub fn sync() -> Vec<ProgramView> {
    let Some(all) = rules() else {
        return programs()
            .into_iter()
            .map(|path| ProgramView {
                path,
                state: State::Unreadable,
                foreign_wider: false,
                detail: "couldn't read Windows Firewall".into(),
                has_glass_allow: false,
            })
            .collect();
    };
    let wanted = scope();
    programs()
        .into_iter()
        .map(|path| {
            let view = view_of(&path, &all, "");
            match view.state {
                State::Ready | State::Blocked | State::Unreadable => view,
                State::NeedsChange => match converge(&path, wanted, action_of(&view, &all)) {
                    Ok(()) => {
                        let Some(after) = rules() else {
                            return failed(
                                &path,
                                view.foreign_wider,
                                view.has_glass_allow,
                                "couldn't re-read Windows Firewall",
                            );
                        };
                        let mut done = view_of(&path, &after, "");
                        if done.state == State::NeedsChange {
                            done.detail = format!(
                                "the rule read back as {}\n{}",
                                describe(&path, &after),
                                manual_command(&path, wanted)
                            );
                        }
                        done
                    }
                    Err(detail) => failed(
                        &path,
                        view.foreign_wider,
                        view.has_glass_allow,
                        &format!("{detail}\n{}", manual_command(&path, wanted)),
                    ),
                },
            }
        })
        .collect()
}

/// One approval prompt (the same one setup already uses), then the rules are read back.
pub fn elevate_sync() -> Vec<ProgramView> {
    if !super::service::is_elevated() {
        let _ = launch_elevated();
        return inspect();
    }
    sync()
}

/// Entry point for [`SYNC_ARG`]. Fails when not elevated, so it can't loop on the prompt.
pub fn run_sync() -> anyhow::Result<()> {
    anyhow::ensure!(
        super::service::is_elevated(),
        "administrator approval is required"
    );
    let pending: Vec<_> = sync()
        .into_iter()
        .filter(|view| matches!(view.state, State::NeedsChange | State::Unreadable))
        .map(|view| format!("{}: {}", view.path.display(), view.detail))
        .collect();
    anyhow::ensure!(pending.is_empty(), "{}", pending.join("\n"));
    Ok(())
}

/// What to tell the user after `glass config bind`. Applies the rule when this process is
/// already elevated; otherwise points at `glass setup` and does not ask for approval.
pub fn bind_change_note() -> Option<String> {
    let views = inspect();
    if views.is_empty() {
        return None;
    }
    if views.iter().any(|view| view.state == State::Unreadable) {
        return Some(
            "Couldn't read Windows Firewall. Apply this listen address with: glass setup".into(),
        );
    }
    let needs = views.iter().any(|view| view.state == State::NeedsChange);
    let blocked = views.iter().any(|view| view.state == State::Blocked);
    let foreign = views.iter().any(|view| view.foreign_wider);
    let wanted = scope();
    if blocked && !needs {
        return Some(blocked_hint().into());
    }
    if needs {
        let fresh_default =
            wanted == RemoteScope::Any && views.iter().all(|view| !view.has_glass_allow);
        if fresh_default && !foreign {
            return None;
        }
        if super::service::is_elevated() {
            return note_after_sync(&sync(), wanted);
        }
        return Some(change_hint(wanted, foreign));
    }
    foreign.then(|| foreign_warning(wanted).into())
}

pub fn ready_sentence(scope: RemoteScope) -> &'static str {
    match scope {
        RemoteScope::Any => "Glass can accept connections on private networks",
        RemoteScope::Tailscale => {
            "Glass accepts connections from Tailscale only, on private networks"
        }
        RemoteScope::Loopback => {
            "Glass listens on this computer only, so it has no inbound firewall rule"
        }
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
        RemoteScope::Any => {
            "Another firewall rule for this program is still in place. Glass only changes rules named Glass."
        }
    }
}

/// Lines for `glass service install`: a rule that could not be updated, a block, or a
/// rule Glass did not create that is still wider than the saved bind. Quiet when the
/// Glass rules already match.
pub fn install_lines(views: &[ProgramView]) -> Vec<String> {
    let wanted = scope();
    views
        .iter()
        .filter_map(|view| match view.state {
            State::Blocked => Some(format!("{}: {}", view.path.display(), blocked_hint())),
            State::NeedsChange | State::Unreadable => Some(format!(
                "Couldn't update the Windows Firewall rule for {}: {}",
                view.path.display(),
                view.detail
            )),
            State::Ready if view.foreign_wider => Some(format!(
                "{}: {}",
                view.path.display(),
                foreign_warning(wanted)
            )),
            State::Ready => None,
        })
        .collect()
}

pub fn manual_command(path: &Path, scope: RemoteScope) -> String {
    let program = shell(&path.to_string_lossy());
    let delete = netsh_delete_inbound(&program);
    let add = match scope {
        RemoteScope::Loopback => None,
        RemoteScope::Any => netsh_add_rule(&program, &AddSpec::Any),
        RemoteScope::Tailscale => netsh_add_rule(
            &program,
            &AddSpec::Remote(vec![TAILSCALE_V4.into(), TAILSCALE_V6.into()]),
        ),
    };
    match (delete, add) {
        (Some(delete), Some(add)) => format!("netsh {delete}\n  netsh {add}"),
        (Some(delete), None) => format!("netsh {delete}"),
        (None, Some(add)) => format!("netsh {add}"),
        (None, None) => format!("the path {} can't be quoted for netsh", path.display()),
    }
}

/// netsh arguments for the rule `scope` wants. Loopback has no allow rule, so this is
/// the delete. A path that can't be quoted safely comes back as a short explanation.
pub fn add_rule_args(exe: &Path, scope: RemoteScope) -> String {
    let program = shell(&exe.to_string_lossy());
    match scope {
        RemoteScope::Tailscale => netsh_add_rule(
            &program,
            &AddSpec::Remote(vec![TAILSCALE_V4.into(), TAILSCALE_V6.into()]),
        ),
        RemoteScope::Any => netsh_add_rule(&program, &AddSpec::Any),
        RemoteScope::Loopback => netsh_delete_inbound(&program),
    }
    .unwrap_or_else(|| format!("(can't quote {program})"))
}

/// Removes rules named Glass for `exe`, inbound and outbound. Other names stay.
/// Uninstall calls this and ignores a missing rule.
pub fn remove_rule_quietly(exe: &Path) -> bool {
    let program = shell(&exe.to_string_lossy());
    // Same character check as the inbound delete: an odd path is not passed to netsh.
    if netsh_delete_inbound(&program).is_none() {
        return false;
    }
    netsh(&format!(
        "advfirewall firewall delete rule name=\"{RULE_NAME}\" program=\"{program}\""
    ))
    .is_ok()
}

fn note_after_sync(views: &[ProgramView], wanted: RemoteScope) -> Option<String> {
    if views
        .iter()
        .any(|view| matches!(view.state, State::NeedsChange | State::Unreadable))
    {
        let detail = views
            .iter()
            .filter(|view| matches!(view.state, State::NeedsChange | State::Unreadable))
            .map(|view| view.detail.clone())
            .collect::<Vec<_>>()
            .join(" ");
        return Some(format!("{detail}\nOr run: glass setup"));
    }
    if views.iter().any(|view| view.state == State::Blocked) {
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
        RemoteScope::Loopback => {
            "Windows Firewall still has an inbound rule for Glass. Remove it with: glass setup"
        }
        RemoteScope::Any => {
            "Windows Firewall still has a different rule for Glass. Apply it with: glass setup"
        }
    };
    if foreign {
        format!("{lead}\n{}", foreign_warning(scope))
    } else {
        lead.into()
    }
}

fn failed(path: &Path, foreign_wider: bool, has_glass_allow: bool, detail: &str) -> ProgramView {
    ProgramView {
        path: path.to_path_buf(),
        state: State::NeedsChange,
        foreign_wider,
        detail: detail.into(),
        has_glass_allow,
    }
}

fn view_of(path: &Path, all: &[Rule], detail: &str) -> ProgramView {
    let observed = observed_for(path, all);
    let Decision {
        action,
        foreign_wider,
    } = decide_sync(&observed, scope());
    let state = match action {
        FirewallAction::InSync => State::Ready,
        FirewallAction::Converge | FirewallAction::Remove => State::NeedsChange,
        FirewallAction::Blocked => State::Blocked,
    };
    ProgramView {
        path: path.to_path_buf(),
        state,
        foreign_wider,
        detail: detail.into(),
        has_glass_allow: observed
            .iter()
            .any(|rule| rule.named_glass && rule.inbound && rule.allow),
    }
}

fn action_of(view: &ProgramView, all: &[Rule]) -> FirewallAction {
    decide_sync(&observed_for(&view.path, all), scope()).action
}

fn observed_for(path: &Path, all: &[Rule]) -> Vec<ObservedRule> {
    all.iter()
        .filter(|rule| same_program(&rule.program, path))
        .map(to_observed)
        .collect()
}

fn to_observed(rule: &Rule) -> ObservedRule {
    ObservedRule {
        named_glass: is_glass_name(&rule.name),
        enabled: rule.enabled,
        inbound: rule.inbound,
        allow: rule.allow,
        profiles: rule.profiles,
        remote_addresses: rule.remote_addresses.clone(),
        edge: rule.edge,
        protocol_any: rule.protocol_any,
        local_ports_any: rule.local_ports_any,
        remote_ports_any: rule.remote_ports_any,
        local_addresses_any: rule.local_addresses_any,
    }
}

fn converge(path: &Path, scope: RemoteScope, action: FirewallAction) -> Result<(), String> {
    if action == FirewallAction::Converge {
        add_missing(path, scope)?;
    }
    remove_nonkeepers(path, scope, true)?;
    if in_sync(path, scope) {
        return Ok(());
    }
    // One `remoteip` with both families can be rejected or stored as a single family.
    // Add each family on its own. This second delete refuses to run the name-wide
    // fallback, so a failed listing can't remove the rules just added.
    if scope == RemoteScope::Tailscale {
        let _ = add_spec(path, &AddSpec::Remote(vec![TAILSCALE_V4.into()]));
        let _ = add_spec(path, &AddSpec::Remote(vec![TAILSCALE_V6.into()]));
        remove_nonkeepers(path, scope, false)?;
        if in_sync(path, scope) {
            return Ok(());
        }
    }
    let detail = rules()
        .map(|all| describe(path, &all))
        .unwrap_or_else(|| "the firewall rules couldn't be read".into());
    Err(format!("the rule read back as {detail}"))
}

fn add_missing(path: &Path, scope: RemoteScope) -> Result<(), String> {
    let Some(all) = rules() else {
        return Err("couldn't read Windows Firewall".into());
    };
    let specs = super::firewall_scope::adds_needed(&observed_for(path, &all), scope);
    for spec in &specs {
        if let Err(error) = add_spec(path, spec) {
            let AddSpec::Remote(cidrs) = spec else {
                return Err(error);
            };
            if cidrs.len() < 2 {
                return Err(error);
            }
            add_spec(path, &AddSpec::Remote(vec![TAILSCALE_V4.into()]))?;
            add_spec(path, &AddSpec::Remote(vec![TAILSCALE_V6.into()]))?;
        }
    }
    Ok(())
}

fn add_spec(path: &Path, spec: &AddSpec) -> Result<(), String> {
    let program = shell(&path.to_string_lossy());
    let command =
        netsh_add_rule(&program, spec).ok_or_else(|| format!("can't quote {}", path.display()))?;
    netsh(&command)
}

/// Drops Glass inbound allow rules that aren't part of `scope`.
///
/// PowerShell is only how one rule is named for deletion (`netsh` deletes every rule
/// that shares the display name, including the one just added). Which rules to keep is
/// decided from COM. A rule the two views can't be paired for is left in place.
/// `fallback` may delete every inbound Glass rule for the program and add the desired
/// one; the Tailscale retry passes false so a failed listing can't remove what it added.
fn remove_nonkeepers(path: &Path, scope: RemoteScope, fallback: bool) -> Result<(), String> {
    let listed = match list_glass() {
        Ok(listed) => listed,
        Err(error) => {
            if fallback {
                return fallback_reset(path, scope);
            }
            return Err(error);
        }
    };
    let Some(all) = rules() else {
        if fallback {
            return fallback_reset(path, scope);
        }
        return Err("couldn't read Windows Firewall".into());
    };
    let com: Vec<ObservedRule> = observed_for(path, &all)
        .into_iter()
        .filter(|rule| rule.named_glass && rule.inbound)
        .collect();
    let mine: Vec<&Listed> = listed
        .iter()
        .filter(|rule| same_program(&rule.program, path))
        .collect();
    let mut used = vec![false; mine.len()];
    let mut paired: Vec<(ObservedRule, String)> = Vec::new();
    for rule in com {
        let found = mine
            .iter()
            .enumerate()
            .find(|(i, listed)| !used[*i] && ps_matches(listed, &rule));
        if let Some((i, listed)) = found {
            used[i] = true;
            paired.push((rule, listed.id.clone()));
        }
    }
    let observed: Vec<ObservedRule> = paired.iter().map(|(rule, _)| rule.clone()).collect();
    let disp = dispositions(&observed, scope);
    // No canonical allow rule to keep, and this scope still wants one: deleting the
    // others would leave nothing letting the program through. Leave them; the caller
    // reads the rules back and reports that.
    if scope != RemoteScope::Loopback && !disp.iter().any(|d| *d == Disposition::Keep) {
        return Ok(());
    }
    let ids: Vec<String> = disp
        .iter()
        .enumerate()
        .filter(|(_, disposition)| **disposition == Disposition::Drop)
        .map(|(i, _)| paired[i].1.clone())
        .collect();
    remove_ids(&ids)
}

/// Whether a PowerShell row is the same rule COM returned. Profile and edge come from
/// COM for the keep/drop decision; they only have to agree here so two rules that share
/// a remote-address list aren't paired with each other's ids.
fn ps_matches(listed: &Listed, rule: &ObservedRule) -> bool {
    parse_remote_addresses(&listed.remote) == parse_remote_addresses(&rule.remote_addresses)
        && listed.action.trim().eq_ignore_ascii_case("allow") == rule.allow
        && parse_enabled(&listed.enabled) == rule.enabled
        && edge_matches(&listed.edge, rule.edge)
        && parse_profile(&listed.profile).is_some_and(|bits| bits == rule.profiles)
        && protocol_matches(&listed.protocol, rule.protocol_any)
        && text_means_any(&listed.local_port) == rule.local_ports_any
        && text_means_any(&listed.remote_port) == rule.remote_ports_any
        && text_means_any(&listed.local) == rule.local_addresses_any
}

fn protocol_matches(text: &str, any: bool) -> bool {
    (text_means_any(text) || text.trim() == "256") == any
}

fn edge_matches(text: &str, edge: EdgeTraversal) -> bool {
    match (parse_edge_text(text), edge) {
        (parsed, edge) if parsed == edge => true,
        // The older boolean property is false for both deny and defer-to-user.
        (EdgeTraversal::Deny | EdgeTraversal::DeferToUser, EdgeTraversal::DenyOrDeferUser) => true,
        _ => false,
    }
}

fn in_sync(path: &Path, scope: RemoteScope) -> bool {
    rules().is_some_and(|all| {
        decide_sync(&observed_for(path, &all), scope).action == FirewallAction::InSync
    })
}

fn describe(path: &Path, all: &[Rule]) -> String {
    let bits: Vec<_> = observed_for(path, all)
        .iter()
        .filter(|rule| rule.named_glass && rule.inbound && rule.allow)
        .map(|rule| {
            format!(
                "remote \"{}\" (edge {:?}, profile {})",
                rule.remote_addresses, rule.edge, rule.profiles
            )
        })
        .collect();
    if bits.is_empty() {
        "no Glass allow rule".into()
    } else {
        bits.join("; ")
    }
}

fn launch_elevated() -> bool {
    let Ok(exe) = std::env::current_exe() else {
        return false;
    };
    let script = format!(
        "$ErrorActionPreference='Stop'; $p = Start-Process -FilePath {} -ArgumentList '{}' -Verb RunAs -WindowStyle Hidden -Wait -PassThru; exit $p.ExitCode",
        super::ps_string(&exe.display().to_string()),
        SYNC_ARG
    );
    super::powershell(&script)
        .status()
        .is_ok_and(|s| s.success())
}

fn netsh(args: &str) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    let mut cmd = std::process::Command::new("netsh");
    cmd.raw_arg(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    super::no_window(&mut cmd);
    let out = cmd
        .output()
        .map_err(|error| format!("couldn't run netsh ({error})"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(command_output(&out))
    }
}

/// Glass's inbound rules, with the instance id PowerShell uses to delete one of them.
/// `netsh delete` by name would also delete the rule just added.
fn list_glass() -> Result<Vec<Listed>, String> {
    let script = r#"
$ProgressPreference = 'SilentlyContinue'
$WarningPreference = 'SilentlyContinue'
$InformationPreference = 'SilentlyContinue'
$ErrorActionPreference = 'Stop'
$rules = @(Get-NetFirewallRule -DisplayName 'Glass' -ErrorAction SilentlyContinue | Where-Object { $_.Direction -eq 'Inbound' -and $_.DisplayName -eq 'Glass' })
$items = foreach ($rule in $rules) {
  try {
    $app = $rule | Get-NetFirewallApplicationFilter
    $addr = $rule | Get-NetFirewallAddressFilter
    $port = $rule | Get-NetFirewallPortFilter
    [pscustomobject]@{
      Id = [string]$rule.Name
      Action = [string]$rule.Action
      Enabled = [string]$rule.Enabled
      Profile = [string]$rule.Profile
      Edge = [string]$rule.EdgeTraversalPolicy
      Remote = ((@($addr.RemoteAddress) | ForEach-Object { "$_" }) -join ',')
      Local = ((@($addr.LocalAddress) | ForEach-Object { "$_" }) -join ',')
      Protocol = [string]$port.Protocol
      LocalPort = [string]$port.LocalPort
      RemotePort = [string]$port.RemotePort
      Program = [string]$app.Program
    }
  } catch { continue }
}
if (-not $items) { $json = '[]' } else { $json = ConvertTo-Json -InputObject @($items) -Compress }
[Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($json))
"#;
    let out = ps_output(script)?;
    if !out.status.success() {
        return Err(command_output(&out));
    }
    decode_list(&String::from_utf8_lossy(&out.stdout))
}

fn decode_list(stdout: &str) -> Result<Vec<Listed>, String> {
    let line = stdout
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("")
        .trim();
    let bytes = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, line)
        .map_err(|_| "couldn't read the firewall rule list".to_string())?;
    let text =
        String::from_utf8(bytes).map_err(|_| "couldn't read the firewall rule list".to_string())?;
    let value: serde_json::Value = serde_json::from_str(text.trim())
        .map_err(|_| "couldn't read the firewall rule list".to_string())?;
    let items = match value {
        serde_json::Value::Array(items) => items,
        other => vec![other],
    };
    items
        .into_iter()
        .map(|item| {
            serde_json::from_value(item)
                .map_err(|_| "couldn't read the firewall rule list".to_string())
        })
        .collect()
}

#[derive(Debug, serde::Deserialize)]
struct Listed {
    #[serde(rename = "Id")]
    id: String,
    #[serde(rename = "Action")]
    action: String,
    #[serde(rename = "Enabled")]
    enabled: String,
    #[serde(rename = "Profile")]
    profile: String,
    #[serde(rename = "Edge")]
    edge: String,
    #[serde(rename = "Remote")]
    remote: String,
    #[serde(rename = "Local")]
    local: String,
    #[serde(rename = "Protocol")]
    protocol: String,
    #[serde(rename = "LocalPort")]
    local_port: String,
    #[serde(rename = "RemotePort")]
    remote_port: String,
    #[serde(rename = "Program")]
    program: String,
}

fn remove_ids(ids: &[String]) -> Result<(), String> {
    if ids.is_empty() {
        return Ok(());
    }
    if ids.iter().any(|id| !safe_rule_id(id)) {
        return Err("a Glass rule has an unexpected id, so it was left in place".into());
    }
    let list = ids
        .iter()
        .map(|id| format!("'{id}'"))
        .collect::<Vec<_>>()
        .join(",");
    ps_ok(&format!(
        "$ProgressPreference='SilentlyContinue'; $ErrorActionPreference='Stop'; Remove-NetFirewallRule -Name {list}"
    ))
}

fn safe_rule_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() < 80
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '{' | '}' | '-' | '_'))
}

/// Last resort when the per-rule ids can't be listed. `netsh` can only delete by name,
/// which also removes a rule just added, so this deletes first and then adds the rule
/// the scope wants. Callers use it only when selective deletion isn't available.
fn fallback_reset(path: &Path, scope: RemoteScope) -> Result<(), String> {
    let program = shell(&path.to_string_lossy());
    if let Some(delete) = netsh_delete_inbound(&program) {
        let _ = netsh(&delete);
    }
    match scope {
        RemoteScope::Loopback => Ok(()),
        RemoteScope::Any => add_spec(path, &AddSpec::Any),
        RemoteScope::Tailscale => add_spec(
            path,
            &AddSpec::Remote(vec![TAILSCALE_V4.into(), TAILSCALE_V6.into()]),
        )
        .or_else(|_| {
            add_spec(path, &AddSpec::Remote(vec![TAILSCALE_V4.into()]))?;
            add_spec(path, &AddSpec::Remote(vec![TAILSCALE_V6.into()]))?;
            Ok(())
        }),
    }
}

fn ps_output(script: &str) -> Result<std::process::Output, String> {
    let mut cmd = super::powershell(script);
    cmd.stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    cmd.output()
        .map_err(|error| format!("couldn't run PowerShell ({error})"))
}

fn ps_ok(script: &str) -> Result<(), String> {
    let out = ps_output(script)?;
    if out.status.success() {
        Ok(())
    } else {
        Err(command_output(&out))
    }
}

fn command_output(out: &std::process::Output) -> String {
    let text = format!(
        "{} {}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    );
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.is_empty() {
        format!("the command failed ({})", out.status)
    } else {
        text.chars().take(400).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(program: &str, allow: bool, profiles: i32) -> Rule {
        Rule {
            name: RULE_NAME.into(),
            program: program.into(),
            enabled: true,
            inbound: true,
            allow,
            profiles,
            remote_addresses: "*".into(),
            edge: EdgeTraversal::DeferToUser,
            protocol_any: true,
            local_ports_any: true,
            remote_ports_any: true,
            local_addresses_any: true,
        }
    }

    #[test]
    fn block_rules_win_and_only_private_ones_count() {
        let same = |p: &str| p.eq_ignore_ascii_case(r"C:\Glass\glass.exe");
        assert_eq!(decide(&[], same), Inbound::Missing);
        assert_eq!(
            decide(&[rule(r"c:\glass\glass.exe", true, 2)], same),
            Inbound::Allowed
        );
        assert_eq!(
            decide(&[rule(r"c:\glass\glass.exe", true, 0x7fff_ffff)], same),
            Inbound::Allowed
        );
        assert_eq!(
            decide(&[rule(r"c:\glass\glass.exe", true, 4)], same),
            Inbound::Missing
        );
        assert_eq!(
            decide(&[rule(r"c:\other.exe", true, 2)], same),
            Inbound::Missing
        );
        assert_eq!(
            decide(
                &[
                    rule(r"c:\glass\glass.exe", true, 2),
                    rule(r"C:\Glass\glass.exe", false, 2)
                ],
                same
            ),
            Inbound::Blocked
        );
        let mut off = rule(r"c:\glass\glass.exe", false, 2);
        off.enabled = false;
        assert_eq!(
            decide(&[off, rule(r"c:\glass\glass.exe", true, 3)], same),
            Inbound::Allowed
        );
    }

    #[test]
    fn the_rule_names_the_program_plainly() {
        assert_eq!(
            add_rule_args(
                Path::new(r"\\?\C:\Program Files\Glass\glass.exe"),
                RemoteScope::Any
            ),
            "advfirewall firewall add rule name=\"Glass\" dir=in action=allow program=\"C:\\Program Files\\Glass\\glass.exe\" enable=yes profile=private"
        );
        let tailscale = add_rule_args(Path::new(r"C:\Glass\glass.exe"), RemoteScope::Tailscale);
        assert!(tailscale.contains("remoteip=\"100.64.0.0/10,fd7a:115c:a1e0::/48\" edge=no"));
        assert!(
            !add_rule_args(Path::new(r"C:\Glass\glass.exe"), RemoteScope::Any).contains("edge=")
        );
        assert!(
            add_rule_args(Path::new(r"C:\Glass\glass.exe"), RemoteScope::Loopback)
                .contains("delete rule")
        );
    }

    #[test]
    fn scope_constants_match_the_windows_headers() {
        assert_eq!(
            super::super::firewall_scope::PROFILE_PRIVATE,
            NET_FW_PROFILE2_PRIVATE.0
        );
        assert_eq!(NET_FW_EDGE_TRAVERSAL_TYPE_DENY.0, 0);
        assert_eq!(NET_FW_EDGE_TRAVERSAL_TYPE_ALLOW.0, 1);
        assert_eq!(NET_FW_EDGE_TRAVERSAL_TYPE_DEFER_TO_APP.0, 2);
        assert_eq!(NET_FW_EDGE_TRAVERSAL_TYPE_DEFER_TO_USER.0, 3);
        assert_eq!(NET_FW_IP_PROTOCOL_ANY.0, 256);
    }
}
