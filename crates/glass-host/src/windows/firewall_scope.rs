//! Which remote addresses Glass's Windows firewall rule should allow, and how a reconcile
//! turns the current rules into that set.
//!
//! The firewall sees the peer's address, not the address the socket is bound to, so a
//! Tailscale listen address still allows every Tailscale peer. No Win32 calls: the Windows
//! module applies the plan through one firewall backend.

use std::collections::BTreeSet;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// `NET_FW_PROFILE2_PRIVATE`. A rule Glass creates is private networks only.
pub const PROFILE_PRIVATE: i32 = 2;

/// Display name of every rule Glass creates. Anything else is left alone.
pub const RULE_NAME: &str = "Glass";

pub const TAILSCALE_V4: &str = "100.64.0.0/10";
pub const TAILSCALE_V6: &str = "fd7a:115c:a1e0::/48";

/// Remote-address scope of the inbound allow rule Glass maintains.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteScope {
    /// Any remote address, private profile only. The rule Glass has always created.
    Any,
    /// Tailscale's CGNAT range and IPv6 ULA, with edge traversal blocked.
    Tailscale,
    /// Every listener is loopback. The firewall does not filter loopback, so Glass
    /// keeps no inbound allow rule of its own.
    Loopback,
}

/// One snapshot of the saved bind. Passed through a reconcile so a concurrent edit
/// cannot make the decision and the write disagree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScopePlan {
    pub scope: RemoteScope,
    /// When false, do not create or restore a wide allow. An installer that could not
    /// read a profile uses this so an upgrade does not invent the old "any remote" rule.
    pub allow_wide: bool,
}

/// How a rule treats edge traversal. `DenyOrDefer` is the old boolean `false`, which
/// covers both deny and defer-to-user; a remote-address list cannot be stored on defer-to-user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    Deny,
    Allow,
    DeferToApp,
    DeferToUser,
    DenyOrDefer,
    Unknown,
}

/// What to add when the desired allow is missing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AllowSpec {
    /// Private profile, any remote address, edge left at Windows' default.
    Any,
    TailscaleBoth,
    TailscaleV4,
    TailscaleV6,
}

/// One firewall rule, already read as values. `id` is chosen by the backend and is
/// what [`FirewallStore::remove_allow`] deletes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FwRule {
    pub id: String,
    pub name: String,
    pub program: String,
    pub enabled: bool,
    pub inbound: bool,
    pub allow: bool,
    pub profiles: i32,
    pub remote_addresses: String,
    pub edge: Edge,
    pub protocol_any: bool,
    pub ports_any: bool,
    pub local_addresses_any: bool,
}

/// A listing that did not see every rule. Callers must not treat it as success.
#[derive(Debug)]
pub enum ListFail {
    Unavailable(String),
    Incomplete(Vec<FwRule>),
}

pub trait FirewallStore {
    fn list(&mut self) -> Result<Vec<FwRule>, ListFail>;
    fn add_allow(&mut self, program: &str, spec: AllowSpec) -> Result<(), String>;
    /// Deletes one Glass-owned allow. Must not delete a block rule.
    fn remove_allow(&mut self, id: &str) -> Result<(), String>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgramReport {
    pub program: String,
    pub blocked: bool,
    pub foreign_wider: bool,
    pub has_glass_allow: bool,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub programs: Vec<ProgramReport>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconcileError {
    pub message: String,
}

/// What the inbound rules say about one program on private networks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Coverage {
    Allowed,
    Blocked,
    Missing,
}

/// `tokens` are canonical bind tokens (`listen::parse_bind`): `tailscale` or IP strings.
/// Empty is the default listen, which stays the wide private-network rule.
pub fn scope_for_bind<S: AsRef<str>>(tokens: &[S]) -> RemoteScope {
    if tokens.is_empty() {
        return RemoteScope::Any;
    }
    let mut tailscale = false;
    let mut loopback = false;
    let mut open = false;
    for token in tokens {
        let token = token.as_ref().trim();
        if token.is_empty() {
            continue;
        }
        if token.eq_ignore_ascii_case("tailscale") {
            tailscale = true;
            continue;
        }
        let Some(ip) = token.parse::<IpAddr>().ok().map(|ip| ip.to_canonical()) else {
            // Not a bind token. Don't invent a tighter rule from a value the host
            // will not treat as a listen address.
            return RemoteScope::Any;
        };
        if ip.is_unspecified() {
            open = true;
        } else if ip.is_loopback() {
            loopback = true;
        } else if crate::tailscale::is_tailscale_ip(ip) {
            tailscale = true;
        } else {
            // A LAN, VPN, or public address. Peers are whoever can route to it, not one
            // prefix, and the private profile is the boundary Glass already uses.
            open = true;
        }
    }
    if open || (!tailscale && !loopback) {
        RemoteScope::Any
    } else if tailscale {
        RemoteScope::Tailscale
    } else {
        RemoteScope::Loopback
    }
}

/// The narrowest of the saved binds. Loopback admits nothing inbound, then Tailscale's
/// two ranges, then any remote address. A shared program has one rule, so a narrower
/// saved bind wins: a wide rule must not outlive a Tailscale or loopback setting.
pub fn narrowest(scopes: impl IntoIterator<Item = RemoteScope>) -> RemoteScope {
    scopes
        .into_iter()
        .min_by_key(|scope| match scope {
            RemoteScope::Loopback => 0,
            RemoteScope::Tailscale => 1,
            RemoteScope::Any => 2,
        })
        .unwrap_or(RemoteScope::Any)
}

/// `binds` are the settings files that could be read. `unreadable` means another profile
/// might exist and could not be read, so a wide rule must not be created or restored.
pub fn plan_from_binds(binds: &[Vec<String>], unreadable: bool) -> ScopePlan {
    if binds.is_empty() {
        return ScopePlan { scope: RemoteScope::Any, allow_wide: !unreadable };
    }
    let scope = narrowest(binds.iter().map(|bind| scope_for_bind(bind)));
    ScopePlan { scope, allow_wide: scope == RemoteScope::Any && !unreadable }
}

pub fn is_glass_name(name: &str) -> bool {
    name.trim().eq_ignore_ascii_case(RULE_NAME)
}

/// Case-insensitive path compare. Strips the `\\?\` prefix Windows adds.
pub fn programs_match(rule_path: &str, exe: &str) -> bool {
    fn norm(path: &str) -> String {
        crate::browse::shell_path(path.trim()).replace('/', "\\").to_lowercase()
    }
    let rule_path = norm(rule_path);
    !rule_path.is_empty() && rule_path == norm(exe)
}

/// Candidates in the order they were discovered. Missing files and `glassw.exe` (it
/// does not listen) are dropped. The same path twice, differing only by case, is one.
pub fn selected_programs<S: AsRef<str>>(candidates: &[(S, bool)]) -> Vec<String> {
    let mut out = Vec::new();
    for (path, exists) in candidates {
        if !exists {
            continue;
        }
        let path = path.as_ref();
        // `Path` on Unix does not treat `\` as a separator, and these are Windows paths.
        let file_name = path.rsplit(['\\', '/']).next().unwrap_or(path);
        if file_name.eq_ignore_ascii_case("glassw.exe") {
            continue;
        }
        if out.iter().any(|have: &String| programs_match(have, path)) {
            continue;
        }
        out.push(path.to_string());
    }
    out
}

pub fn coverage(rules: &[FwRule], program: &str) -> Coverage {
    let mut allow = false;
    let mut block = false;
    for rule in rules {
        if !rule.enabled || !rule.inbound || !programs_match(&rule.program, program) {
            continue;
        }
        if rule.profiles & PROFILE_PRIVATE == 0 {
            continue;
        }
        if rule.allow {
            allow = true;
        } else {
            block = true;
        }
    }
    if block {
        Coverage::Blocked
    } else if allow {
        Coverage::Allowed
    } else {
        Coverage::Missing
    }
}

/// `netsh` text for a person to run by hand. The reconcile itself does not shell out.
/// A path containing a quote has no safe one-line form; the UI then says so.
pub fn manual_netsh(program: &str, spec: AllowSpec) -> Option<String> {
    if program.chars().any(|c| c == '"' || c == '\n' || c == '\r') {
        return None;
    }
    let program = crate::browse::shell_path(program);
    let base = format!(
        "advfirewall firewall add rule name=\"{RULE_NAME}\" dir=in action=allow program=\"{program}\" enable=yes profile=private"
    );
    Some(match spec {
        AllowSpec::Any => base,
        AllowSpec::TailscaleBoth => format!("{base} remoteip=\"{TAILSCALE_V4},{TAILSCALE_V6}\" edge=no"),
        AllowSpec::TailscaleV4 => format!("{base} remoteip=\"{TAILSCALE_V4}\" edge=no"),
        AllowSpec::TailscaleV6 => format!("{base} remoteip=\"{TAILSCALE_V6}\" edge=no"),
    })
}

pub fn manual_delete(program: &str) -> Option<String> {
    if program.chars().any(|c| c == '"' || c == '\n' || c == '\r') {
        return None;
    }
    let program = crate::browse::shell_path(program);
    // `action=allow` so the hint does not tell anyone to delete a block rule.
    Some(format!(
        "advfirewall firewall delete rule name=\"{RULE_NAME}\" dir=in action=allow program=\"{program}\""
    ))
}

/// What inspect reports without changing anything. `needs_change` is true when a
/// reconcile would add or remove an owned allow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Preview {
    pub needs_change: bool,
    pub blocked: bool,
    pub foreign_wider: bool,
    pub has_glass_allow: bool,
}

pub fn preview(rules: &[FwRule], program: &str, plan: ScopePlan) -> Preview {
    let ops = ops_for(rules, program, plan);
    Preview {
        needs_change: !ops.remove.is_empty() || !ops.add.is_empty(),
        blocked: ops.blocked,
        foreign_wider: ops.foreign_wider,
        has_glass_allow: ops.has_glass_allow,
    }
}

/// Make each program's Glass-owned inbound allows match `plan`.
///
/// Narrowing removes an owned wide allow before adding the replacement. If the add
/// fails, the wide allow stays gone and this returns an error: losing connectivity is
/// preferred to leaving the LAN in. A block rule is reported and never deleted, and it
/// does not stop that removal. An incomplete listing removes any too-wide owned allow
/// it did see, adds nothing, and returns an error.
pub fn reconcile(store: &mut impl FirewallStore, programs: &[impl AsRef<str>], plan: ScopePlan) -> Result<Report, ReconcileError> {
    let rules = match store.list() {
        Ok(rules) => rules,
        Err(ListFail::Unavailable(message)) => {
            return Err(ReconcileError {
                message: format!("Couldn't read Windows Firewall ({message}). No rule was changed."),
            });
        }
        Err(ListFail::Incomplete(seen)) => return recover_partial(store, programs, plan, &seen),
    };
    let mut reports = Vec::new();
    let mut errors = Vec::new();
    for program in programs {
        match reconcile_one(store, &rules, program.as_ref(), plan) {
            Ok(report) => reports.push(report),
            Err(message) => {
                errors.push(message.clone());
                reports.push(ProgramReport {
                    program: program.as_ref().to_string(),
                    blocked: false,
                    foreign_wider: false,
                    has_glass_allow: false,
                    detail: message,
                });
            }
        }
    }
    if errors.is_empty() {
        Ok(Report { programs: reports })
    } else {
        Err(ReconcileError { message: errors.join("\n") })
    }
}

/// Re-reads the plan after each pass. A bind edit during the write is applied again,
/// up to a few times; if it is still changing, the caller has to run once more.
pub fn reconcile_stable(
    store: &mut impl FirewallStore,
    programs: &[impl AsRef<str>],
    mut load: impl FnMut() -> ScopePlan,
) -> Result<Report, ReconcileError> {
    let mut plan = load();
    for _ in 0..4 {
        let report = reconcile(store, programs, plan)?;
        let now = load();
        if now == plan {
            return Ok(report);
        }
        plan = now;
    }
    Err(ReconcileError {
        message: "The saved bind changed while the firewall rule was updating. Run `glass setup` again.".into(),
    })
}

fn recover_partial(
    store: &mut impl FirewallStore,
    programs: &[impl AsRef<str>],
    plan: ScopePlan,
    seen: &[FwRule],
) -> Result<Report, ReconcileError> {
    // A partial list is not a reason to add. For a narrowing scope, still drop owned
    // allows that are already known to be too wide, so a missed row cannot keep the LAN in.
    let mut removed = 0;
    let mut failures = Vec::new();
    if plan.scope != RemoteScope::Any {
        for program in programs {
            for id in ops_for(seen, program.as_ref(), plan).remove {
                match store.remove_allow(&id) {
                    Ok(()) => removed += 1,
                    Err(error) => failures.push(error),
                }
            }
        }
    }
    let mut message = format!(
        "Windows Firewall could not be listed completely, so no allow rule was added. Removed {removed} Glass allow rule(s) that were wider than the saved bind. Run `glass setup` again."
    );
    if !failures.is_empty() {
        message.push_str(" Could not remove: ");
        message.push_str(&failures.join("; "));
    }
    Err(ReconcileError { message })
}

fn reconcile_one(store: &mut impl FirewallStore, rules: &[FwRule], program: &str, plan: ScopePlan) -> Result<ProgramReport, String> {
    let ops = ops_for(rules, program, plan);
    let mut remove_errors = Vec::new();
    for id in &ops.remove {
        if let Err(error) = store.remove_allow(id) {
            remove_errors.push(error);
        }
    }
    if !remove_errors.is_empty() {
        return Err(format!(
            "{program}: couldn't remove a Glass allow rule ({}). No narrower rule was added. A wider allow may still be in place; run `glass setup` again.",
            remove_errors.join("; ")
        ));
    }
    add_needed(store, program, &ops.add)?;
    Ok(ProgramReport {
        program: program.to_string(),
        blocked: ops.blocked,
        foreign_wider: ops.foreign_wider,
        has_glass_allow: ops.keeps_allow,
        detail: String::new(),
    })
}

fn add_needed(store: &mut impl FirewallStore, program: &str, specs: &[AllowSpec]) -> Result<(), String> {
    if specs == [AllowSpec::TailscaleBoth] {
        if store.add_allow(program, AllowSpec::TailscaleBoth).is_ok() {
            return Ok(());
        }
        // One rule with both families can be rejected. Each family is not wider than
        // the Tailscale ranges. The wide allow, if there was one, is already gone.
        let v4 = store.add_allow(program, AllowSpec::TailscaleV4);
        let v6 = store.add_allow(program, AllowSpec::TailscaleV6);
        return match (v4, v6) {
            (Ok(()), Ok(())) => Ok(()),
            (Ok(()), Err(error)) => Err(format!(
                "{program}: removed any wider Glass allow, but the Tailscale IPv6 rule was not added ({error}). Addresses outside Tailscale are not allowed. IPv6 Tailscale peers may not connect until `glass setup` is run again."
            )),
            (Err(error), Ok(())) => Err(format!(
                "{program}: removed any wider Glass allow, but the Tailscale IPv4 rule was not added ({error}). Addresses outside Tailscale are not allowed. IPv4 Tailscale peers may not connect until `glass setup` is run again."
            )),
            (Err(v4), Err(v6)) => Err(format!(
                "{program}: removed any wider Glass allow, but the Tailscale rule was not added ({v4}; {v6}). Glass may be unreachable from other computers until `glass setup` is run again."
            )),
        };
    }
    for spec in specs {
        if let Err(error) = store.add_allow(program, *spec) {
            return Err(format!(
                "{program}: removed any wider Glass allow, but the replacement was not added ({error}). Glass may be unreachable from other computers until `glass setup` is run again."
            ));
        }
    }
    Ok(())
}

struct Ops {
    remove: Vec<String>,
    add: Vec<AllowSpec>,
    blocked: bool,
    foreign_wider: bool,
    has_glass_allow: bool,
    keeps_allow: bool,
}

fn ops_for(rules: &[FwRule], program: &str, plan: ScopePlan) -> Ops {
    let mine: Vec<&FwRule> = rules.iter().filter(|rule| programs_match(&rule.program, program)).collect();
    let blocked = mine.iter().any(|rule| rule.enabled && rule.inbound && !rule.allow && affects_private(rule));
    let foreign_wider = mine.iter().any(|rule| {
        rule.enabled
            && rule.inbound
            && rule.allow
            && !is_glass_name(&rule.name)
            && affects_private(rule)
            && match plan.scope {
                RemoteScope::Any => false,
                RemoteScope::Loopback => true,
                RemoteScope::Tailscale => !remote_within_tailscale(&rule.remote_addresses),
            }
    });
    let allows: Vec<&FwRule> = mine.iter().copied().filter(|rule| owned_allow(rule)).collect();
    let has_glass_allow = allows.iter().any(|rule| rule.enabled);
    let (remove, add) = match plan.scope {
        RemoteScope::Loopback => (allows.iter().map(|rule| rule.id.clone()).collect(), Vec::new()),
        RemoteScope::Tailscale => tailscale_ops(&allows),
        RemoteScope::Any => any_ops(&allows, plan.allow_wide),
    };
    let removed: BTreeSet<&str> = remove.iter().map(String::as_str).collect();
    let kept_existing = allows.iter().any(|rule| rule.enabled && !removed.contains(rule.id.as_str()));
    let keeps_allow = plan.scope != RemoteScope::Loopback && (kept_existing || !add.is_empty());
    Ops { remove, add, blocked, foreign_wider, has_glass_allow, keeps_allow }
}

fn owned_allow(rule: &FwRule) -> bool {
    rule.inbound && rule.allow && is_glass_name(&rule.name)
}

fn affects_private(rule: &FwRule) -> bool {
    rule.profiles & PROFILE_PRIVATE != 0
}

fn shape_ok(rule: &FwRule) -> bool {
    rule.enabled && rule.profiles == PROFILE_PRIVATE && rule.protocol_any && rule.ports_any && rule.local_addresses_any
}

fn edge_blocks(rule: &FwRule) -> bool {
    matches!(rule.edge, Edge::Deny | Edge::DenyOrDefer)
}

fn tailscale_ops(allows: &[&FwRule]) -> (Vec<String>, Vec<AllowSpec>) {
    let both = tailscale_set();
    let v4 = BTreeSet::from([TAILSCALE_V4.to_string()]);
    let v6 = BTreeSet::from([TAILSCALE_V6.to_string()]);
    let matching = |expect: &BTreeSet<String>| {
        allows.iter().filter(|rule| shape_ok(rule) && edge_blocks(rule) && remote_exact(rule) == Some(expect.clone())).copied().collect::<Vec<_>>()
    };
    let full = matching(&both);
    let only_v4 = matching(&v4);
    let only_v6 = matching(&v6);
    let (keep, add): (Vec<&FwRule>, Vec<AllowSpec>) = if !full.is_empty() {
        (vec![full[0]], Vec::new())
    } else if !only_v4.is_empty() && !only_v6.is_empty() {
        (vec![only_v4[0], only_v6[0]], Vec::new())
    } else if !only_v4.is_empty() {
        (vec![only_v4[0]], vec![AllowSpec::TailscaleV6])
    } else if !only_v6.is_empty() {
        (vec![only_v6[0]], vec![AllowSpec::TailscaleV4])
    } else {
        (Vec::new(), vec![AllowSpec::TailscaleBoth])
    };
    let keep_ids: BTreeSet<&str> = keep.iter().map(|rule| rule.id.as_str()).collect();
    let remove = allows.iter().filter(|rule| !keep_ids.contains(rule.id.as_str())).map(|rule| rule.id.clone()).collect();
    (remove, add)
}

fn any_ops(allows: &[&FwRule], allow_wide: bool) -> (Vec<String>, Vec<AllowSpec>) {
    let wide: Vec<&FwRule> = allows.iter().copied().filter(|rule| shape_ok(rule) && remote_is_unrestricted(&rule.remote_addresses)).collect();
    if !allow_wide {
        // Don't restore the wide rule. If a narrower Glass allow is already there,
        // a wide sibling is still exposure, so that sibling goes.
        let restricted = allows.iter().any(|rule| rule.enabled && !remote_is_unrestricted(&rule.remote_addresses));
        if !restricted {
            return (Vec::new(), Vec::new());
        }
        let remove = allows.iter().filter(|rule| remote_is_unrestricted(&rule.remote_addresses)).map(|rule| rule.id.clone()).collect();
        return (remove, Vec::new());
    }
    let keep = wide.first().map(|rule| rule.id.as_str());
    let remove = allows.iter().filter(|rule| keep != Some(rule.id.as_str())).map(|rule| rule.id.clone()).collect();
    let add = if wide.is_empty() { vec![AllowSpec::Any] } else { Vec::new() };
    (remove, add)
}

fn tailscale_set() -> BTreeSet<String> {
    BTreeSet::from([TAILSCALE_V4.to_string(), TAILSCALE_V6.to_string()])
}

fn remote_exact(rule: &FwRule) -> Option<BTreeSet<String>> {
    match remote_class(&rule.remote_addresses) {
        RemoteClass::Exact(cidrs) => Some(cidrs),
        RemoteClass::Unrestricted | RemoteClass::Other => None,
    }
}

fn remote_is_unrestricted(raw: &str) -> bool {
    matches!(remote_class(raw), RemoteClass::Unrestricted)
}

fn remote_within_tailscale(raw: &str) -> bool {
    match remote_class(raw) {
        RemoteClass::Exact(cidrs) => !cidrs.is_empty() && cidrs.iter().all(|cidr| cidr_within_tailscale(cidr)),
        RemoteClass::Unrestricted | RemoteClass::Other => false,
    }
}

enum RemoteClass {
    Unrestricted,
    Exact(BTreeSet<String>),
    Other,
}

fn remote_class(raw: &str) -> RemoteClass {
    let raw = raw.trim();
    if raw.is_empty() || raw.eq_ignore_ascii_case("*") || raw.eq_ignore_ascii_case("any") {
        return RemoteClass::Unrestricted;
    }
    let mut cidrs = BTreeSet::new();
    for part in raw.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if part == "*" || part.eq_ignore_ascii_case("any") {
            return RemoteClass::Other;
        }
        match parse_remote_token(part) {
            Some(cidr) => {
                cidrs.insert(cidr);
            }
            None => return RemoteClass::Other,
        }
    }
    if cidrs.is_empty() { RemoteClass::Unrestricted } else { RemoteClass::Exact(cidrs) }
}

fn parse_remote_token(part: &str) -> Option<String> {
    if !part.contains('/')
        && let Some((start, end)) = part.split_once('-')
        && let (Ok(start), Ok(end)) = (start.trim().parse::<IpAddr>(), end.trim().parse::<IpAddr>())
    {
        return range_cidr(start.to_canonical(), end.to_canonical());
    }
    let (addr, prefix) = part.rsplit_once('/')?;
    let ip = addr.trim().parse::<IpAddr>().ok()?.to_canonical();
    let prefix = prefix.trim();
    let bits: u8 = if let Ok(bits) = prefix.parse() {
        bits
    } else if let Ok(mask) = prefix.parse::<Ipv4Addr>() {
        mask_prefix(mask)?
    } else {
        return None;
    };
    network_cidr(ip, bits)
}

fn mask_prefix(mask: Ipv4Addr) -> Option<u8> {
    let bits = u32::from(mask);
    let host = !bits;
    (host & host.wrapping_add(1) == 0).then_some(bits.count_ones() as u8)
}

fn network_cidr(ip: IpAddr, prefix: u8) -> Option<String> {
    match ip {
        IpAddr::V4(addr) if prefix <= 32 => Some(format!("{}/{prefix}", network_v4(addr, prefix))),
        IpAddr::V6(addr) if prefix <= 128 => Some(format!("{}/{prefix}", network_v6(addr, prefix))),
        _ => None,
    }
}

fn network_v4(addr: Ipv4Addr, prefix: u8) -> Ipv4Addr {
    if prefix == 0 {
        return Ipv4Addr::UNSPECIFIED;
    }
    let mask = u32::MAX << (32 - prefix);
    Ipv4Addr::from(u32::from(addr) & mask)
}

fn network_v6(addr: Ipv6Addr, prefix: u8) -> Ipv6Addr {
    if prefix == 0 {
        return Ipv6Addr::UNSPECIFIED;
    }
    let mask = u128::MAX << (128 - prefix);
    Ipv6Addr::from(u128::from(addr) & mask)
}

fn range_cidr(start: IpAddr, end: IpAddr) -> Option<String> {
    match (start, end) {
        (IpAddr::V4(start), IpAddr::V4(end)) => {
            let start = u32::from(start);
            let end = u32::from(end);
            let size = end.checked_sub(start)?.checked_add(1)?;
            if !size.is_power_of_two() || start & (size - 1) != 0 {
                return None;
            }
            network_cidr(IpAddr::V4(Ipv4Addr::from(start)), 32 - size.trailing_zeros() as u8)
        }
        (IpAddr::V6(start), IpAddr::V6(end)) => {
            let start = u128::from(start);
            let end = u128::from(end);
            let size = end.checked_sub(start)?.checked_add(1)?;
            if !size.is_power_of_two() || start & (size - 1) != 0 {
                return None;
            }
            network_cidr(IpAddr::V6(Ipv6Addr::from(start)), 128 - size.trailing_zeros() as u8)
        }
        _ => None,
    }
}

fn cidr_within_tailscale(cidr: &str) -> bool {
    let Some((addr, prefix)) = cidr.split_once('/') else { return false };
    let Ok(prefix) = prefix.parse::<u8>() else { return false };
    let Ok(ip) = addr.parse::<IpAddr>() else { return false };
    match ip {
        IpAddr::V4(addr) => (10..=32).contains(&prefix) && network_v4(addr, 10) == Ipv4Addr::new(100, 64, 0, 0),
        IpAddr::V6(addr) => {
            (48..=128).contains(&prefix) && network_v6(addr, 48) == "fd7a:115c:a1e0::".parse::<Ipv6Addr>().unwrap_or(Ipv6Addr::UNSPECIFIED)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PROGRAM: &str = r"C:\Users\Иван\AppData\Local\Programs\Glass\bin\glass.exe";

    fn allow(id: &str, remote: &str, edge: Edge) -> FwRule {
        FwRule {
            id: id.into(),
            name: RULE_NAME.into(),
            program: PROGRAM.into(),
            enabled: true,
            inbound: true,
            allow: true,
            profiles: PROFILE_PRIVATE,
            remote_addresses: remote.into(),
            edge,
            protocol_any: true,
            ports_any: true,
            local_addresses_any: true,
        }
    }

    struct Mock {
        rules: Vec<FwRule>,
        incomplete: bool,
        unavailable: bool,
        fail_add: Vec<AllowSpec>,
        fail_remove: bool,
        next_id: u32,
    }

    impl Mock {
        fn new(rules: Vec<FwRule>) -> Self {
            Self { rules, incomplete: false, unavailable: false, fail_add: Vec::new(), fail_remove: false, next_id: 100 }
        }

        fn glass_allows(&self) -> Vec<&FwRule> {
            self.rules.iter().filter(|rule| owned_allow(rule) && programs_match(&rule.program, PROGRAM)).collect()
        }
    }

    impl FirewallStore for Mock {
        fn list(&mut self) -> Result<Vec<FwRule>, ListFail> {
            if self.unavailable {
                return Err(ListFail::Unavailable("down".into()));
            }
            if self.incomplete {
                return Err(ListFail::Incomplete(self.rules.clone()));
            }
            Ok(self.rules.clone())
        }

        fn add_allow(&mut self, program: &str, spec: AllowSpec) -> Result<(), String> {
            if self.fail_add.contains(&spec) {
                return Err(format!("add {spec:?} failed"));
            }
            let (remote, edge) = match spec {
                AllowSpec::Any => ("*", Edge::DeferToUser),
                AllowSpec::TailscaleBoth => ("100.64.0.0/10,fd7a:115c:a1e0::/48", Edge::Deny),
                AllowSpec::TailscaleV4 => (TAILSCALE_V4, Edge::Deny),
                AllowSpec::TailscaleV6 => (TAILSCALE_V6, Edge::Deny),
            };
            self.next_id += 1;
            let mut rule = allow(&format!("added-{}", self.next_id), remote, edge);
            rule.program = program.to_string();
            self.rules.push(rule);
            Ok(())
        }

        fn remove_allow(&mut self, id: &str) -> Result<(), String> {
            if self.fail_remove {
                return Err(format!("remove {id} failed"));
            }
            let before = self.rules.len();
            self.rules.retain(|rule| rule.id != id || !rule.allow);
            if self.rules.len() == before {
                return Err(format!("no allow {id}"));
            }
            Ok(())
        }
    }

    fn tailscale_plan() -> ScopePlan {
        ScopePlan { scope: RemoteScope::Tailscale, allow_wide: false }
    }

    fn wide_plan(allow_wide: bool) -> ScopePlan {
        ScopePlan { scope: RemoteScope::Any, allow_wide }
    }

    #[test]
    fn scope_follows_the_bind_tokens() {
        assert_eq!(scope_for_bind::<&str>(&[]), RemoteScope::Any);
        assert_eq!(scope_for_bind(&["0.0.0.0"]), RemoteScope::Any);
        assert_eq!(scope_for_bind(&["::"]), RemoteScope::Any);
        assert_eq!(scope_for_bind(&["192.168.1.5"]), RemoteScope::Any);
        assert_eq!(scope_for_bind(&["tailscale", "10.0.0.4"]), RemoteScope::Any);
        assert_eq!(scope_for_bind(&["not-an-address"]), RemoteScope::Any);
        assert_eq!(scope_for_bind(&["Tailscale"]), RemoteScope::Tailscale);
        assert_eq!(scope_for_bind(&["100.64.0.7", "127.0.0.1"]), RemoteScope::Tailscale);
        assert_eq!(scope_for_bind(&["::ffff:100.64.0.7"]), RemoteScope::Tailscale);
        assert_eq!(scope_for_bind(&["fd7a:115c:a1e0::1"]), RemoteScope::Tailscale);
        assert_eq!(scope_for_bind(&["127.0.0.1", "::1"]), RemoteScope::Loopback);
        let mixed = plan_from_binds(&[vec!["tailscale".into()], vec!["0.0.0.0".into()]], false);
        assert_eq!(mixed.scope, RemoteScope::Tailscale);
        assert!(!mixed.allow_wide);
        assert_eq!(plan_from_binds(&[], false), wide_plan(true));
        assert_eq!(plan_from_binds(&[], true), wide_plan(false));
        assert_eq!(plan_from_binds(&[vec!["127.0.0.1".into()], vec!["tailscale".into()]], false).scope, RemoteScope::Loopback);
    }

    #[test]
    fn remote_address_text_canonicalises_to_the_tailscale_ranges() {
        let store_both = "100.64.0.0/255.192.0.0,FD7A:115C:A1E0:0:0:0:0:0/48";
        assert!(remote_within_tailscale(store_both));
        assert!(remote_within_tailscale("100.64.0.0-100.127.255.255"));
        assert!(remote_within_tailscale("100.64.9.9/32"));
        assert!(!remote_within_tailscale("100.64.0.0/8"));
        assert!(!remote_within_tailscale("*"));
        assert!(!remote_within_tailscale("LocalSubnet"));
        assert!(!remote_within_tailscale("192.168.0.0/16"));
        let program = r"\\?\C:\Program Files\Glass\glass.exe";
        let historical = manual_netsh(program, AllowSpec::Any).unwrap();
        assert_eq!(
            historical,
            r#"advfirewall firewall add rule name="Glass" dir=in action=allow program="C:\Program Files\Glass\glass.exe" enable=yes profile=private"#
        );
        assert!(manual_netsh(PROGRAM, AllowSpec::TailscaleBoth).unwrap().contains("remoteip=\"100.64.0.0/10,fd7a:115c:a1e0::/48\" edge=no"));
        assert!(manual_delete(PROGRAM).unwrap().contains("action=allow"));
    }

    #[test]
    fn programs_skip_glassw_and_duplicates() {
        let chosen = selected_programs(&[
            (r"C:\Program Files\Glass\bin\glass.exe", true),
            (r"C:\Program Files\Glass\bin\GLASS.EXE", true),
            (r"C:\Program Files\Glass\bin\glassw.exe", true),
            (r"C:\Users\Иван\AppData\Local\Programs\Glass\bin\glass.exe", true),
            (r"C:\Program Files\Glass\Service\glass.exe", false),
        ]);
        assert_eq!(
            chosen,
            vec![
                r"C:\Program Files\Glass\bin\glass.exe".to_string(),
                r"C:\Users\Иван\AppData\Local\Programs\Glass\bin\glass.exe".to_string(),
            ]
        );
    }

    #[test]
    fn a_wide_rule_is_replaced_by_tailscale_and_a_second_pass_changes_nothing() {
        let mut store = Mock::new(vec![allow("wide", "*", Edge::DeferToUser)]);
        let report = reconcile(&mut store, &[PROGRAM], tailscale_plan()).unwrap();
        assert!(report.programs[0].has_glass_allow);
        assert_eq!(store.glass_allows().len(), 1);
        assert!(remote_within_tailscale(&store.glass_allows()[0].remote_addresses));
        assert_eq!(store.glass_allows()[0].edge, Edge::Deny);
        let before = store.rules.len();
        reconcile(&mut store, &[PROGRAM], tailscale_plan()).unwrap();
        assert_eq!(store.rules.len(), before);
    }

    #[test]
    fn add_failure_still_removes_the_wide_allow() {
        let mut store = Mock::new(vec![allow("wide", "*", Edge::DeferToUser)]);
        store.fail_add = vec![AllowSpec::TailscaleBoth, AllowSpec::TailscaleV4, AllowSpec::TailscaleV6];
        let error = reconcile(&mut store, &[PROGRAM], tailscale_plan()).unwrap_err();
        assert!(error.message.contains("may be unreachable"));
        assert!(store.glass_allows().is_empty());
    }

    #[test]
    fn ipv6_add_failure_after_ipv4_leaves_no_wide_allow() {
        let mut store = Mock::new(vec![allow("wide", "*", Edge::DeferToUser)]);
        store.fail_add = vec![AllowSpec::TailscaleBoth, AllowSpec::TailscaleV6];
        let error = reconcile(&mut store, &[PROGRAM], tailscale_plan()).unwrap_err();
        assert!(error.message.contains("IPv6"));
        let left: Vec<_> = store.glass_allows().iter().map(|rule| rule.remote_addresses.as_str()).collect();
        assert_eq!(left, vec![TAILSCALE_V4]);
    }

    #[test]
    fn delete_failure_does_not_add_and_reports_the_wide_rule() {
        let mut store = Mock::new(vec![allow("wide", "*", Edge::DeferToUser)]);
        store.fail_remove = true;
        let error = reconcile(&mut store, &[PROGRAM], tailscale_plan()).unwrap_err();
        assert!(error.message.contains("wider allow may still be in place"));
        assert_eq!(store.glass_allows().len(), 1);
        assert!(remote_is_unrestricted(&store.glass_allows()[0].remote_addresses));
    }

    #[test]
    fn a_partial_listing_removes_a_seen_wide_allow_and_adds_nothing() {
        let mut store = Mock::new(vec![allow("wide", "*", Edge::DeferToUser)]);
        store.incomplete = true;
        let error = reconcile(&mut store, &[PROGRAM], tailscale_plan()).unwrap_err();
        assert!(error.message.contains("listed completely"));
        assert!(store.glass_allows().is_empty());
        store.unavailable = true;
        store.incomplete = false;
        let error = reconcile(&mut store, &[PROGRAM], tailscale_plan()).unwrap_err();
        assert!(error.message.contains("No rule was changed"));
    }

    #[test]
    fn a_block_rule_does_not_keep_a_wide_allow_and_is_not_deleted() {
        let mut block = allow("block", "192.168.1.9/32", Edge::Deny);
        block.allow = false;
        block.name = "Glass".into();
        let mut foreign = allow("foreign", "192.168.0.0/16", Edge::DeferToUser);
        foreign.name = "Glass remote desktop".into();
        let mut store = Mock::new(vec![allow("wide", "*", Edge::DeferToUser), block, foreign]);
        let report = reconcile(&mut store, &[PROGRAM], tailscale_plan()).unwrap();
        assert!(report.programs[0].blocked);
        assert!(report.programs[0].foreign_wider);
        assert!(store.rules.iter().any(|rule| rule.id == "block" && !rule.allow));
        assert!(store.rules.iter().any(|rule| rule.id == "foreign"));
        assert!(store.glass_allows().iter().all(|rule| remote_within_tailscale(&rule.remote_addresses)));
        assert!(store.glass_allows().iter().all(|rule| !remote_is_unrestricted(&rule.remote_addresses)));
    }

    #[test]
    fn loopback_removes_allows_and_leaves_blocks() {
        let mut block = allow("block", "*", Edge::Deny);
        block.allow = false;
        let mut store = Mock::new(vec![allow("wide", "*", Edge::DeferToUser), block]);
        let report = reconcile(&mut store, &[PROGRAM], ScopePlan { scope: RemoteScope::Loopback, allow_wide: false }).unwrap();
        assert!(!report.programs[0].has_glass_allow);
        assert!(report.programs[0].blocked);
        assert!(store.glass_allows().is_empty());
        assert!(store.rules.iter().any(|rule| rule.id == "block"));
    }

    #[test]
    fn an_unreadable_profile_does_not_restore_a_wide_rule() {
        let mut store = Mock::new(vec![allow("tight", "100.64.0.0/10,fd7a:115c:a1e0::/48", Edge::Deny), allow("wide", "*", Edge::DeferToUser)]);
        reconcile(&mut store, &[PROGRAM], wide_plan(false)).unwrap();
        let remotes: Vec<_> = store.glass_allows().iter().map(|rule| rule.remote_addresses.as_str()).collect();
        assert_eq!(remotes, vec!["100.64.0.0/10,fd7a:115c:a1e0::/48"]);
        let mut lone = Mock::new(vec![allow("wide", "*", Edge::DeferToUser)]);
        reconcile(&mut lone, &[PROGRAM], wide_plan(false)).unwrap();
        assert_eq!(lone.glass_allows().len(), 1);
        let mut empty = Mock::new(vec![]);
        reconcile(&mut empty, &[PROGRAM], wide_plan(true)).unwrap();
        assert!(remote_is_unrestricted(&empty.glass_allows()[0].remote_addresses));
        reconcile(&mut store, &[PROGRAM], wide_plan(true)).unwrap();
        assert!(store.glass_allows().iter().any(|rule| remote_is_unrestricted(&rule.remote_addresses)));
        assert!(store.glass_allows().iter().all(|rule| remote_is_unrestricted(&rule.remote_addresses)));
    }

    #[test]
    fn a_later_tailscale_bind_wins_over_an_overlapping_wide_pass() {
        let mut store = Mock::new(vec![allow("wide", "*", Edge::DeferToUser)]);
        let mut reads = 0;
        reconcile_stable(&mut store, &[PROGRAM], || {
            reads += 1;
            if reads == 1 { wide_plan(true) } else { tailscale_plan() }
        })
        .unwrap();
        assert!(store.glass_allows().iter().all(|rule| remote_within_tailscale(&rule.remote_addresses)));
        assert!(!store.glass_allows().iter().any(|rule| remote_is_unrestricted(&rule.remote_addresses)));
    }

    #[test]
    fn a_narrower_foreign_address_inside_tailscale_is_not_wider() {
        let mut foreign = allow("foreign", "100.64.9.9/32", Edge::Deny);
        foreign.name = "Someone else".into();
        let mut store = Mock::new(vec![foreign]);
        let report = reconcile(&mut store, &[PROGRAM], tailscale_plan()).unwrap();
        assert!(!report.programs[0].foreign_wider);
        assert!(store.rules.iter().any(|rule| rule.id == "foreign"));
    }
}
