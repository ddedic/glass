//! Which remote addresses Glass's Windows Firewall rule should allow, from `glass config bind`.
//!
//! The firewall sees the *peer's* address, not the address the socket is bound to. A
//! Tailscale listen address still receives connections from other tailnet devices, so the
//! rule allows Tailscale's ranges (`100.64.0.0/10` and `fd7a:115c:a1e0::/48`) rather than
//! this computer's own Tailscale address. No Win32 calls: the Windows module applies it.

use std::collections::BTreeSet;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// `NET_FW_PROFILE2_PRIVATE`. A canonical Glass rule is private networks only.
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
    /// Tailscale's CGNAT range and IPv6 ULA. Edge traversal is blocked.
    Tailscale,
    /// Every listener is loopback. The firewall does not filter loopback, so Glass
    /// keeps no inbound allow rule of its own.
    Loopback,
}

/// How a rule treats edge traversal (Teredo). `DeferToUser` is what `netsh` writes when
/// `edge` is omitted, and Windows refuses a remote-address filter on those rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeTraversal {
    Deny,
    Allow,
    DeferToApp,
    DeferToUser,
    /// The older boolean property was false, and the options property could not be read.
    /// That boolean is false for both deny and defer-to-user. A remote-address list other
    /// than "any" cannot be stored on defer-to-user, so it implies deny.
    DenyOrDeferUser,
    Unknown,
}

/// One rule that applies to the program being checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedRule {
    /// Display name is [`RULE_NAME`], ignoring case and surrounding space.
    pub named_glass: bool,
    pub enabled: bool,
    pub inbound: bool,
    pub allow: bool,
    /// `NET_FW_PROFILE2` bitmask.
    pub profiles: i32,
    /// Remote addresses as the firewall stores them (`*`, `Any`, CIDRs, masks, ranges).
    pub remote_addresses: String,
    pub edge: EdgeTraversal,
    pub protocol_any: bool,
    pub local_ports_any: bool,
    pub remote_ports_any: bool,
    pub local_addresses_any: bool,
}

/// What to do with the Glass rules for one program. Rules that are not named Glass are
/// never deleted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FirewallAction {
    /// Glass's own rules already match the scope.
    InSync,
    /// Add the missing allow rule, then delete Glass allow rules that aren't part of it.
    Converge,
    /// Delete Glass's inbound allow rules. Nothing replaces them.
    Remove,
    /// An enabled inbound block rule on the private profile covers this program.
    /// Block wins over allow, so Glass doesn't add or remove anything.
    Blocked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Decision {
    pub action: FirewallAction,
    /// An enabled inbound allow rule Glass did not create (a different name) still admits
    /// more remote addresses than the scope. Glass does not change it.
    pub foreign_wider: bool,
}

/// What `netsh advfirewall firewall add rule` should create.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AddSpec {
    /// The historical rule: private profile, any remote address, edge left at Windows' default.
    Any,
    /// `remoteip` is these canonical CIDRs and `edge=no`.
    Remote(Vec<String>),
}

/// For a Glass inbound allow rule: keep it, delete it, or it isn't one of ours to touch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disposition {
    /// Not a Glass inbound allow rule (another name, a block, outbound). Don't touch it.
    Leave,
    Keep,
    /// A Glass inbound allow rule that isn't part of the set this scope wants.
    Drop,
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
            // won't treat as a listen address.
            return RemoteScope::Any;
        };
        if ip.is_unspecified() {
            // `0.0.0.0` or `::` accepts every remote address of that family.
            open = true;
        } else if ip.is_loopback() {
            loopback = true;
        } else if crate::tailscale::is_tailscale_ip(ip) {
            tailscale = true;
        } else {
            // A LAN, VPN, or public address. Peers are whoever can route to it, not a
            // fixed range, and the private profile is the boundary Glass already uses.
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

pub fn is_glass_name(name: &str) -> bool {
    name.trim().eq_ignore_ascii_case(RULE_NAME)
}

/// Parsed `RemoteAddresses`. Order doesn't matter; a mask and a prefix for the same
/// network compare equal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedRemotes {
    pub any: bool,
    pub cidrs: BTreeSet<String>,
    pub other: BTreeSet<String>,
}

impl ParsedRemotes {
    pub fn is_unrestricted(&self) -> bool {
        self.any && self.cidrs.is_empty() && self.other.is_empty()
    }
}

pub fn parse_remote_addresses(raw: &str) -> ParsedRemotes {
    let raw = raw.trim();
    if raw.is_empty() || raw.eq_ignore_ascii_case("*") || raw.eq_ignore_ascii_case("any") {
        return ParsedRemotes {
            any: true,
            cidrs: BTreeSet::new(),
            other: BTreeSet::new(),
        };
    }
    let mut any = false;
    let mut cidrs = BTreeSet::new();
    let mut other = BTreeSet::new();
    for token in raw.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        if token.eq_ignore_ascii_case("*") || token.eq_ignore_ascii_case("any") {
            any = true;
            continue;
        }
        if let Some(cidr) = canonical_cidr(token) {
            cidrs.insert(cidr);
        } else {
            other.insert(token.to_ascii_lowercase());
        }
    }
    ParsedRemotes { any, cidrs, other }
}

pub fn decide_sync(rules: &[ObservedRule], scope: RemoteScope) -> Decision {
    let foreign_wider = foreign_wider(rules, scope);
    let blocked = rules
        .iter()
        .any(|r| r.enabled && r.inbound && !r.allow && r.profiles & PROFILE_PRIVATE != 0);
    // Loopback has nothing to add. A block rule can stay while Glass's allow rule goes.
    if blocked && scope != RemoteScope::Loopback {
        return Decision {
            action: FirewallAction::Blocked,
            foreign_wider,
        };
    }
    let disp = dispositions(rules, scope);
    let drops = disp.iter().any(|d| *d == Disposition::Drop);
    let action = match scope {
        RemoteScope::Loopback => {
            if drops {
                FirewallAction::Remove
            } else {
                FirewallAction::InSync
            }
        }
        RemoteScope::Any => {
            let kept = disp.iter().any(|d| *d == Disposition::Keep);
            let glass_allow = rules.iter().any(|r| r.named_glass && r.inbound && r.allow);
            let permitted = rules
                .iter()
                .any(|r| r.enabled && r.inbound && r.allow && r.profiles & PROFILE_PRIVATE != 0);
            // Already allowed by a rule we didn't create, and we have no Glass rule to
            // fix: don't add a second wide one.
            if !glass_allow && permitted {
                FirewallAction::InSync
            } else if kept && !drops {
                FirewallAction::InSync
            } else {
                FirewallAction::Converge
            }
        }
        RemoteScope::Tailscale => {
            if covered(rules, &disp) && !drops {
                FirewallAction::InSync
            } else {
                FirewallAction::Converge
            }
        }
    };
    Decision {
        action,
        foreign_wider,
    }
}

/// Parallel to `rules`. Only [`Disposition::Drop`] entries are Glass allow rules to delete.
pub fn dispositions(rules: &[ObservedRule], scope: RemoteScope) -> Vec<Disposition> {
    let mut out = vec![Disposition::Leave; rules.len()];
    let glass: Vec<usize> = rules
        .iter()
        .enumerate()
        .filter(|(_, r)| r.named_glass && r.inbound && r.allow)
        .map(|(i, _)| i)
        .collect();
    match scope {
        RemoteScope::Loopback => {
            for i in glass {
                out[i] = Disposition::Drop;
            }
        }
        RemoteScope::Any => {
            let mut kept = false;
            for i in glass {
                if !kept && canonical_any(&rules[i]) {
                    out[i] = Disposition::Keep;
                    kept = true;
                } else {
                    out[i] = Disposition::Drop;
                }
            }
        }
        RemoteScope::Tailscale => {
            if let Some(full) = glass
                .iter()
                .copied()
                .find(|&i| canonical_tailscale(&rules[i], &tailscale_set()))
            {
                for i in glass {
                    out[i] = if i == full {
                        Disposition::Keep
                    } else {
                        Disposition::Drop
                    };
                }
            } else {
                let mut have_v4 = false;
                let mut have_v6 = false;
                for i in glass {
                    match tailscale_part(&rules[i]) {
                        Some(Part::V4) if !have_v4 => {
                            out[i] = Disposition::Keep;
                            have_v4 = true;
                        }
                        Some(Part::V6) if !have_v6 => {
                            out[i] = Disposition::Keep;
                            have_v6 = true;
                        }
                        _ => out[i] = Disposition::Drop,
                    }
                }
            }
        }
    }
    out
}

/// Rules still missing after [`dispositions`] keeps what it can. One combined Tailscale
/// rule when neither family is present; otherwise only the family that's missing.
pub fn adds_needed(rules: &[ObservedRule], scope: RemoteScope) -> Vec<AddSpec> {
    let disp = dispositions(rules, scope);
    match scope {
        RemoteScope::Loopback => Vec::new(),
        RemoteScope::Any => {
            if disp.iter().any(|d| *d == Disposition::Keep) {
                Vec::new()
            } else {
                vec![AddSpec::Any]
            }
        }
        RemoteScope::Tailscale => {
            let mut have_v4 = false;
            let mut have_v6 = false;
            for (rule, d) in rules.iter().zip(&disp) {
                if *d != Disposition::Keep {
                    continue;
                }
                let parsed = parse_remote_addresses(&rule.remote_addresses);
                have_v4 |= parsed.cidrs.contains(TAILSCALE_V4);
                have_v6 |= parsed.cidrs.contains(TAILSCALE_V6);
            }
            match (have_v4, have_v6) {
                (true, true) => Vec::new(),
                (false, false) => vec![AddSpec::Remote(vec![
                    TAILSCALE_V4.into(),
                    TAILSCALE_V6.into(),
                ])],
                (false, true) => vec![AddSpec::Remote(vec![TAILSCALE_V4.into()])],
                (true, false) => vec![AddSpec::Remote(vec![TAILSCALE_V6.into()])],
            }
        }
    }
}

/// `program` is the path `netsh` should store, already without a `\\?\` prefix.
/// `None` when the path contains characters that would break the command line.
pub fn netsh_add_rule(program: &str, spec: &AddSpec) -> Option<String> {
    if !netsh_safe(program) {
        return None;
    }
    let mut cmd = format!(
        "advfirewall firewall add rule name=\"{RULE_NAME}\" dir=in action=allow program=\"{program}\" enable=yes profile=private"
    );
    if let AddSpec::Remote(cidrs) = spec {
        cmd.push_str(" remoteip=\"");
        cmd.push_str(&cidrs.join(","));
        cmd.push_str("\" edge=no");
    }
    Some(cmd)
}

/// Deletes inbound rules named Glass for `program`. Outbound rules and other names stay.
pub fn netsh_delete_inbound(program: &str) -> Option<String> {
    netsh_safe(program).then(|| {
        format!(
            "advfirewall firewall delete rule name=\"{RULE_NAME}\" dir=in program=\"{program}\""
        )
    })
}

/// `*`, empty, or `Any`: the firewall's way of saying every address or port.
pub fn text_means_any(s: &str) -> bool {
    let s = s.trim();
    s.is_empty() || s.eq_ignore_ascii_case("*") || s.eq_ignore_ascii_case("any")
}

pub fn parse_enabled(s: &str) -> bool {
    matches!(
        s.trim().to_ascii_lowercase().as_str(),
        "true" | "1" | "yes" | "enabled"
    )
}

/// `EdgeTraversalPolicy` as PowerShell prints it (`Block`, `Allow`, `DeferToUser`, `DeferToApp`).
pub fn parse_edge_text(s: &str) -> EdgeTraversal {
    match s.trim().to_ascii_lowercase().as_str() {
        "block" | "deny" | "0" => EdgeTraversal::Deny,
        "allow" | "1" => EdgeTraversal::Allow,
        "defertoapp" | "2" => EdgeTraversal::DeferToApp,
        "defertouser" | "3" => EdgeTraversal::DeferToUser,
        _ => EdgeTraversal::Unknown,
    }
}

/// Profile flags as PowerShell prints them (`Private`, `Private, Domain`) or as a bitmask.
/// `None` when the text isn't one of those, so a localized value isn't mistaken for private.
pub fn parse_profile(s: &str) -> Option<i32> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if s.eq_ignore_ascii_case("any") || s.eq_ignore_ascii_case("all") {
        return Some(0x7fff_ffff);
    }
    if let Ok(n) = s.parse::<i32>() {
        return Some(n);
    }
    let mut bits = 0i32;
    let mut saw = false;
    for part in s.split(|c: char| c == ',' || c == '|' || c.is_whitespace()) {
        match part.trim().to_ascii_lowercase().as_str() {
            "" => {}
            "domain" => {
                bits |= 1;
                saw = true;
            }
            "private" => {
                bits |= PROFILE_PRIVATE;
                saw = true;
            }
            "public" => {
                bits |= 4;
                saw = true;
            }
            _ => return None,
        }
    }
    saw.then_some(bits)
}

fn netsh_safe(program: &str) -> bool {
    !program.is_empty()
        && program.chars().all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, ' ' | ':' | '\\' | '.' | '-' | '_' | '(' | ')')
        })
}

fn foreign_wider(rules: &[ObservedRule], scope: RemoteScope) -> bool {
    if scope == RemoteScope::Any {
        return false;
    }
    rules.iter().any(|r| {
        !r.named_glass
            && r.enabled
            && r.inbound
            && r.allow
            && r.profiles & PROFILE_PRIVATE != 0
            && !remote_within(&r.remote_addresses, scope)
    })
}

fn remote_within(raw: &str, scope: RemoteScope) -> bool {
    let parsed = parse_remote_addresses(raw);
    match scope {
        RemoteScope::Any => true,
        // Loopback accepts no remote peer, so any inbound allow is more than the scope.
        RemoteScope::Loopback => false,
        RemoteScope::Tailscale => {
            // "Any", LocalSubnet, and anything we can't place are treated as wider.
            // A longer prefix inside a Tailscale range is not: it admits fewer peers.
            !parsed.any
                && parsed.other.is_empty()
                && parsed.cidrs.iter().all(|cidr| cidr_within_tailscale(cidr))
        }
    }
}

/// `cidr` is canonical (`address/prefix`, network address already masked).
fn cidr_within_tailscale(cidr: &str) -> bool {
    let Some((addr, prefix)) = cidr.split_once('/') else {
        return false;
    };
    let Ok(prefix) = prefix.parse::<u8>() else {
        return false;
    };
    match addr.parse::<IpAddr>() {
        Ok(IpAddr::V4(ip)) => {
            prefix >= 10 && prefix <= 32 && apply_v4(ip, 10) == Ipv4Addr::new(100, 64, 0, 0)
        }
        Ok(IpAddr::V6(ip)) => {
            prefix >= 48
                && prefix <= 128
                && apply_v6(ip, 48) == Ipv6Addr::new(0xfd7a, 0x115c, 0xa1e0, 0, 0, 0, 0, 0)
        }
        Err(_) => false,
    }
}

fn covered(rules: &[ObservedRule], disp: &[Disposition]) -> bool {
    let mut seen = BTreeSet::new();
    for (rule, d) in rules.iter().zip(disp) {
        if *d != Disposition::Keep {
            continue;
        }
        let parsed = parse_remote_addresses(&rule.remote_addresses);
        seen.extend(parsed.cidrs);
    }
    seen == tailscale_set()
}

fn tailscale_set() -> BTreeSet<String> {
    BTreeSet::from([TAILSCALE_V4.into(), TAILSCALE_V6.into()])
}

enum Part {
    V4,
    V6,
}

fn tailscale_part(rule: &ObservedRule) -> Option<Part> {
    if !canonical_shape(rule) || !edge_blocks(rule) {
        return None;
    }
    let parsed = parse_remote_addresses(&rule.remote_addresses);
    if parsed.any || !parsed.other.is_empty() {
        return None;
    }
    if parsed.cidrs.len() == 1 && parsed.cidrs.contains(TAILSCALE_V4) {
        Some(Part::V4)
    } else if parsed.cidrs.len() == 1 && parsed.cidrs.contains(TAILSCALE_V6) {
        Some(Part::V6)
    } else {
        None
    }
}

fn canonical_tailscale(rule: &ObservedRule, expect: &BTreeSet<String>) -> bool {
    if !canonical_shape(rule) || !edge_blocks(rule) {
        return false;
    }
    let parsed = parse_remote_addresses(&rule.remote_addresses);
    !parsed.any && parsed.other.is_empty() && parsed.cidrs == *expect
}

fn canonical_any(rule: &ObservedRule) -> bool {
    canonical_shape(rule) && parse_remote_addresses(&rule.remote_addresses).is_unrestricted()
}

fn canonical_shape(rule: &ObservedRule) -> bool {
    rule.named_glass
        && rule.enabled
        && rule.inbound
        && rule.allow
        && rule.profiles == PROFILE_PRIVATE
        && rule.protocol_any
        && rule.local_ports_any
        && rule.remote_ports_any
        && rule.local_addresses_any
}

fn edge_blocks(rule: &ObservedRule) -> bool {
    match rule.edge {
        EdgeTraversal::Deny => true,
        EdgeTraversal::DenyOrDeferUser => {
            !parse_remote_addresses(&rule.remote_addresses).is_unrestricted()
        }
        _ => false,
    }
}

fn canonical_cidr(token: &str) -> Option<String> {
    let token = token.trim();
    if let Some((start, end)) = split_range(token) {
        return range_cidr(start, end);
    }
    if let Some((addr, prefix)) = token.split_once('/') {
        let ip: IpAddr = strip_brackets(addr.trim()).parse().ok()?;
        return cidr_from(ip, prefix.trim());
    }
    let ip: IpAddr = strip_brackets(token).parse().ok()?;
    let ip = ip.to_canonical();
    Some(match ip {
        IpAddr::V4(v4) => format!("{v4}/32"),
        IpAddr::V6(v6) => format!("{v6}/128"),
    })
}

/// A range is `start-end` with no extra hyphen. IPv6 addresses don't contain `-`.
fn split_range(token: &str) -> Option<(IpAddr, IpAddr)> {
    let (start, end) = token.split_once('-')?;
    let start = strip_brackets(start.trim()).parse().ok()?;
    let end = strip_brackets(end.trim()).parse().ok()?;
    Some((start, end))
}

fn strip_brackets(addr: &str) -> &str {
    addr.strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .unwrap_or(addr)
}

fn cidr_from(ip: IpAddr, prefix: &str) -> Option<String> {
    match ip.to_canonical() {
        IpAddr::V4(v4) => {
            let bits = if let Ok(n) = prefix.parse::<u8>() {
                if n > 32 {
                    return None;
                }
                n
            } else {
                mask_prefix(prefix.parse().ok()?)?
            };
            Some(format!("{}/{bits}", apply_v4(v4, bits)))
        }
        IpAddr::V6(v6) => {
            let bits: u8 = prefix.parse().ok()?;
            if bits > 128 {
                return None;
            }
            Some(format!("{}/{bits}", apply_v6(v6, bits)))
        }
    }
}

fn mask_prefix(mask: Ipv4Addr) -> Option<u8> {
    let bits = u32::from(mask);
    if bits != 0 {
        let inv = !bits;
        if inv & inv.wrapping_add(1) != 0 {
            return None;
        }
    }
    Some(bits.leading_ones() as u8)
}

fn apply_v4(ip: Ipv4Addr, prefix: u8) -> Ipv4Addr {
    let masked = if prefix == 0 {
        0
    } else {
        u32::from(ip) & (u32::MAX << (32 - prefix))
    };
    Ipv4Addr::from(masked)
}

fn apply_v6(ip: Ipv6Addr, prefix: u8) -> Ipv6Addr {
    let masked = if prefix == 0 {
        0
    } else {
        u128::from(ip) & (u128::MAX << (128 - prefix))
    };
    Ipv6Addr::from(masked)
}

fn range_cidr(start: IpAddr, end: IpAddr) -> Option<String> {
    match (start.to_canonical(), end.to_canonical()) {
        (IpAddr::V4(s), IpAddr::V4(e)) => {
            let s = u32::from(s);
            let e = u32::from(e);
            let span = e.checked_sub(s)?.checked_add(1)?;
            let prefix = prefix_of(span as u128, s as u128, 32)?;
            Some(format!("{}/{prefix}", Ipv4Addr::from(s)))
        }
        (IpAddr::V6(s), IpAddr::V6(e)) => {
            let s = u128::from(s);
            let e = u128::from(e);
            let span = e.checked_sub(s)?.checked_add(1)?;
            let prefix = prefix_of(span, s, 128)?;
            Some(format!("{}/{prefix}", Ipv6Addr::from(s)))
        }
        _ => None,
    }
}

fn prefix_of(span: u128, start: u128, width: u8) -> Option<u8> {
    if span == 0 || !span.is_power_of_two() {
        return None;
    }
    let host = span.trailing_zeros();
    if host > u32::from(width) {
        return None;
    }
    let prefix = width - host as u8;
    let align = if host == 0 { 0 } else { (1u128 << host) - 1 };
    (start & align == 0).then_some(prefix)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn allow(remote: &str, edge: EdgeTraversal) -> ObservedRule {
        ObservedRule {
            named_glass: true,
            enabled: true,
            inbound: true,
            allow: true,
            profiles: PROFILE_PRIVATE,
            remote_addresses: remote.into(),
            edge,
            protocol_any: true,
            local_ports_any: true,
            remote_ports_any: true,
            local_addresses_any: true,
        }
    }

    fn foreign(remote: &str) -> ObservedRule {
        let mut rule = allow(remote, EdgeTraversal::Deny);
        rule.named_glass = false;
        rule
    }

    #[test]
    fn scope_follows_the_bind_tokens() {
        assert_eq!(scope_for_bind::<&str>(&[]), RemoteScope::Any);
        assert_eq!(scope_for_bind(&["0.0.0.0"]), RemoteScope::Any);
        assert_eq!(scope_for_bind(&["::"]), RemoteScope::Any);
        assert_eq!(scope_for_bind(&["0.0.0.0", "::"]), RemoteScope::Any);
        assert_eq!(scope_for_bind(&["tailscale"]), RemoteScope::Tailscale);
        assert_eq!(scope_for_bind(&["Tailscale"]), RemoteScope::Tailscale);
        assert_eq!(
            scope_for_bind(&["tailscale", "127.0.0.1"]),
            RemoteScope::Tailscale
        );
        assert_eq!(scope_for_bind(&["100.64.0.2"]), RemoteScope::Tailscale);
        assert_eq!(scope_for_bind(&["100.127.255.255"]), RemoteScope::Tailscale);
        assert_eq!(scope_for_bind(&["100.128.0.1"]), RemoteScope::Any);
        assert_eq!(scope_for_bind(&["100.63.255.255"]), RemoteScope::Any);
        assert_eq!(
            scope_for_bind(&["fd7a:115c:a1e0::1234"]),
            RemoteScope::Tailscale
        );
        assert_eq!(scope_for_bind(&["fd7a:115c:a1e1::1"]), RemoteScope::Any);
        assert_eq!(
            scope_for_bind(&["::ffff:100.64.0.7"]),
            RemoteScope::Tailscale
        );
        assert_eq!(scope_for_bind(&["192.168.1.10"]), RemoteScope::Any);
        assert_eq!(scope_for_bind(&["8.8.8.8"]), RemoteScope::Any);
        assert_eq!(
            scope_for_bind(&["10.1.2.3", "100.64.0.2"]),
            RemoteScope::Any
        );
        assert_eq!(
            scope_for_bind(&["tailscale", "192.168.1.5"]),
            RemoteScope::Any
        );
        assert_eq!(scope_for_bind(&["0.0.0.0", "::1"]), RemoteScope::Any);
        assert_eq!(scope_for_bind(&["::", "100.64.0.2"]), RemoteScope::Any);
        assert_eq!(scope_for_bind(&["127.0.0.1"]), RemoteScope::Loopback);
        assert_eq!(scope_for_bind(&["::1"]), RemoteScope::Loopback);
        assert_eq!(scope_for_bind(&["127.0.0.1", "::1"]), RemoteScope::Loopback);
        assert_eq!(scope_for_bind(&["not-an-ip"]), RemoteScope::Any);
    }

    #[test]
    fn remote_address_text_canonicalises_to_the_tailscale_ranges() {
        let both = parse_remote_addresses("FD7A:115C:A1E0::/48, 100.64.0.0/255.192.0.0");
        assert!(!both.any);
        assert!(both.other.is_empty());
        assert_eq!(both.cidrs, tailscale_set());
        assert_eq!(
            parse_remote_addresses("100.64.0.0-100.127.255.255").cidrs,
            BTreeSet::from([TAILSCALE_V4.into()])
        );
        assert_eq!(
            parse_remote_addresses("fd7a:115c:a1e0:0:0:0:0:0/48").cidrs,
            BTreeSet::from([TAILSCALE_V6.into()])
        );
        assert_eq!(
            parse_remote_addresses("100.64.0.5/10").cidrs,
            BTreeSet::from([TAILSCALE_V4.into()])
        );
        assert!(parse_remote_addresses("*").is_unrestricted());
        assert!(parse_remote_addresses("").is_unrestricted());
        assert!(parse_remote_addresses("Any").is_unrestricted());
        let local = parse_remote_addresses("LocalSubnet");
        assert!(!local.any && local.cidrs.is_empty() && local.other.contains("localsubnet"));
        assert_eq!(
            parse_remote_addresses("100.64.0.2").cidrs,
            BTreeSet::from(["100.64.0.2/32".into()])
        );
    }

    #[test]
    fn default_netsh_command_is_unchanged_and_tailscale_sets_edge_and_remoteip() {
        let program = r"C:\Program Files\Glass\glass.exe";
        assert_eq!(
            netsh_add_rule(program, &AddSpec::Any).unwrap(),
            "advfirewall firewall add rule name=\"Glass\" dir=in action=allow program=\"C:\\Program Files\\Glass\\glass.exe\" enable=yes profile=private"
        );
        assert_eq!(
            netsh_add_rule(
                program,
                &AddSpec::Remote(vec![TAILSCALE_V4.into(), TAILSCALE_V6.into()])
            )
            .unwrap(),
            "advfirewall firewall add rule name=\"Glass\" dir=in action=allow program=\"C:\\Program Files\\Glass\\glass.exe\" enable=yes profile=private remoteip=\"100.64.0.0/10,fd7a:115c:a1e0::/48\" edge=no"
        );
        assert!(
            !netsh_add_rule(r"C:\Glass\glass.exe", &AddSpec::Any)
                .unwrap()
                .contains("edge=")
        );
        assert_eq!(
            netsh_delete_inbound(program).unwrap(),
            "advfirewall firewall delete rule name=\"Glass\" dir=in program=\"C:\\Program Files\\Glass\\glass.exe\""
        );
        assert!(netsh_add_rule(r"C:\Glass\bad&name.exe", &AddSpec::Any).is_none());
    }

    #[test]
    fn historical_wide_rule_matches_only_the_any_scope() {
        let wide = allow("*", EdgeTraversal::DeferToUser);
        assert_eq!(
            decide_sync(&[wide.clone()], RemoteScope::Any).action,
            FirewallAction::InSync
        );
        assert_eq!(
            decide_sync(&[wide.clone()], RemoteScope::Tailscale).action,
            FirewallAction::Converge
        );
        assert_eq!(
            decide_sync(&[allow("Any", EdgeTraversal::Deny)], RemoteScope::Any).action,
            FirewallAction::InSync
        );
        assert_eq!(
            decide_sync(&[], RemoteScope::Any).action,
            FirewallAction::Converge
        );
        assert_eq!(
            decide_sync(&[foreign("*")], RemoteScope::Any).action,
            FirewallAction::InSync
        );
    }

    #[test]
    fn tailscale_scope_accepts_one_combined_rule_or_one_per_family() {
        let combined = allow(
            "100.64.0.0/255.192.0.0,fd7a:115c:a1e0::/48",
            EdgeTraversal::Deny,
        );
        assert_eq!(
            decide_sync(&[combined.clone()], RemoteScope::Tailscale).action,
            FirewallAction::InSync
        );
        assert!(adds_needed(&[combined.clone()], RemoteScope::Tailscale).is_empty());

        let ambiguous = allow(
            "100.64.0.0/10,fd7a:115c:a1e0::/48",
            EdgeTraversal::DenyOrDeferUser,
        );
        assert_eq!(
            decide_sync(&[ambiguous], RemoteScope::Tailscale).action,
            FirewallAction::InSync
        );

        let v4 = allow("100.64.0.0/10", EdgeTraversal::Deny);
        let v6 = allow(TAILSCALE_V6, EdgeTraversal::Deny);
        assert_eq!(
            decide_sync(&[v6.clone(), v4.clone()], RemoteScope::Tailscale).action,
            FirewallAction::InSync
        );
        assert_eq!(
            adds_needed(&[v4.clone()], RemoteScope::Tailscale),
            vec![AddSpec::Remote(vec![TAILSCALE_V6.into()])]
        );
        assert_eq!(
            adds_needed(&[], RemoteScope::Tailscale),
            vec![AddSpec::Remote(vec![
                TAILSCALE_V4.into(),
                TAILSCALE_V6.into()
            ])]
        );

        let defer = allow(
            "100.64.0.0/10,fd7a:115c:a1e0::/48",
            EdgeTraversal::DeferToUser,
        );
        assert_eq!(
            decide_sync(&[defer], RemoteScope::Tailscale).action,
            FirewallAction::Converge
        );
        let allowed_edge = allow("100.64.0.0/10,fd7a:115c:a1e0::/48", EdgeTraversal::Allow);
        assert_eq!(
            decide_sync(&[allowed_edge], RemoteScope::Tailscale).action,
            FirewallAction::Converge
        );
        let only_v4 = allow(TAILSCALE_V4, EdgeTraversal::Deny);
        assert_eq!(
            decide_sync(&[only_v4], RemoteScope::Tailscale).action,
            FirewallAction::Converge
        );
        let dup = vec![combined.clone(), combined];
        assert_eq!(
            decide_sync(&dup, RemoteScope::Tailscale).action,
            FirewallAction::Converge
        );
    }

    #[test]
    fn powershell_profile_and_edge_text_parse() {
        assert_eq!(parse_profile("Private"), Some(PROFILE_PRIVATE));
        assert_eq!(parse_profile("Private, Domain"), Some(PROFILE_PRIVATE | 1));
        assert_eq!(parse_profile("2"), Some(PROFILE_PRIVATE));
        assert_eq!(parse_profile("Any"), Some(0x7fff_ffff));
        assert_eq!(parse_profile("Privat"), None);
        assert_eq!(parse_edge_text("Block"), EdgeTraversal::Deny);
        assert_eq!(parse_edge_text("DeferToUser"), EdgeTraversal::DeferToUser);
        assert!(parse_enabled("True"));
        assert!(parse_enabled("Enabled"));
        assert!(!parse_enabled("False"));
        assert!(!parse_enabled("Disabled"));
        assert!(text_means_any("Any"));
        assert!(text_means_any(""));
    }

    #[test]
    fn duplicates_and_stale_rules_are_dropped_and_foreign_rules_are_not() {
        let wide = allow("*", EdgeTraversal::DeferToUser);
        let tight = allow("100.64.0.0/10,fd7a:115c:a1e0::/48", EdgeTraversal::Deny);
        let mut other = foreign("192.168.0.0/16");
        let mut block = allow("*", EdgeTraversal::Deny);
        block.allow = false;
        block.named_glass = true;
        let mut outbound = tight.clone();
        outbound.inbound = false;
        let rules = vec![wide, tight.clone(), other.clone(), block, outbound];
        let disp = dispositions(&rules, RemoteScope::Tailscale);
        assert_eq!(
            disp,
            vec![
                Disposition::Drop,
                Disposition::Keep,
                Disposition::Leave,
                Disposition::Leave,
                Disposition::Leave,
            ]
        );
        // 192.168.0.0/16 admits peers Tailscale does not. The block rule still wins,
        // so nothing is added or removed until that block is gone.
        let decision = decide_sync(&rules, RemoteScope::Tailscale);
        assert!(decision.foreign_wider);
        assert_eq!(decision.action, FirewallAction::Blocked);
        other.remote_addresses = "*".into();
        let decision = decide_sync(&[tight, other], RemoteScope::Tailscale);
        assert_eq!(decision.action, FirewallAction::InSync);
        assert!(decision.foreign_wider);
        assert!(!decide_sync(&[foreign(TAILSCALE_V4)], RemoteScope::Tailscale).foreign_wider);
        assert!(!decide_sync(&[foreign("100.64.9.9/32")], RemoteScope::Tailscale).foreign_wider);
        assert!(decide_sync(&[foreign("100.64.0.0/8")], RemoteScope::Tailscale).foreign_wider);
    }

    #[test]
    fn a_block_rule_stops_an_allow_change_except_when_nothing_is_added() {
        let mut block = allow("*", EdgeTraversal::Deny);
        block.allow = false;
        block.named_glass = false;
        let wide = allow("*", EdgeTraversal::DeferToUser);
        assert_eq!(
            decide_sync(&[block.clone(), wide.clone()], RemoteScope::Tailscale).action,
            FirewallAction::Blocked
        );
        assert_eq!(
            decide_sync(&[block.clone(), wide.clone()], RemoteScope::Any).action,
            FirewallAction::Blocked
        );
        assert_eq!(
            decide_sync(&[block, wide], RemoteScope::Loopback).action,
            FirewallAction::Remove
        );
        assert_eq!(
            decide_sync(&[], RemoteScope::Loopback).action,
            FirewallAction::InSync
        );
        assert_eq!(
            decide_sync(
                &[allow("*", EdgeTraversal::DeferToUser)],
                RemoteScope::Loopback
            )
            .action,
            FirewallAction::Remove
        );
    }

    #[test]
    fn a_rule_that_is_not_the_canonical_shape_is_replaced() {
        let mut public = allow("100.64.0.0/10,fd7a:115c:a1e0::/48", EdgeTraversal::Deny);
        public.profiles = PROFILE_PRIVATE | 4;
        assert_eq!(
            decide_sync(&[public], RemoteScope::Tailscale).action,
            FirewallAction::Converge
        );
        let mut ports = allow("*", EdgeTraversal::DeferToUser);
        ports.local_ports_any = false;
        assert_eq!(
            decide_sync(&[ports], RemoteScope::Any).action,
            FirewallAction::Converge
        );
        let mut off = allow("*", EdgeTraversal::DeferToUser);
        off.enabled = false;
        assert_eq!(
            decide_sync(&[off], RemoteScope::Any).action,
            FirewallAction::Converge
        );
        let lan = allow("192.168.1.0/24", EdgeTraversal::Deny);
        assert_eq!(
            decide_sync(&[lan.clone()], RemoteScope::Any).action,
            FirewallAction::Converge
        );
        assert_eq!(adds_needed(&[lan], RemoteScope::Any), vec![AddSpec::Any]);
    }

    #[test]
    fn switching_back_to_any_replaces_a_tailscale_rule() {
        let tight = allow("100.64.0.0/10,fd7a:115c:a1e0::/48", EdgeTraversal::Deny);
        assert_eq!(
            decide_sync(&[tight.clone()], RemoteScope::Any).action,
            FirewallAction::Converge
        );
        assert_eq!(adds_needed(&[tight], RemoteScope::Any), vec![AddSpec::Any]);
    }
}
