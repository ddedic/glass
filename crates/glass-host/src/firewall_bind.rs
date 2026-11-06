//! Remote addresses for Glass's own Windows firewall rule.
//!
//! The firewall sees the other computer, so a Tailscale listen address still means
//! every Tailscale peer.

use std::net::IpAddr;

pub const TAILSCALE_V4: &str = "100.64.0.0/10";
pub const TAILSCALE_V6: &str = "fd7a:115c:a1e0::/48";

/// Any remote address, Tailscale's two ranges, or no allow (loopback only).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Remote {
    Any,
    Tailscale,
    Loopback,
}

/// `values` are saved bind entries, split by [`crate::listen::parse_bind`].
/// A value the listener rejects is `Err`: the caller must not create a wide rule.
pub fn remote_for_bind<S: AsRef<str>>(values: &[S]) -> Result<Remote, ()> {
    Ok(classify(&crate::listen::parse_bind(values).map_err(|_| ())?))
}

fn classify(tokens: &[String]) -> Remote {
    if tokens.is_empty() {
        return Remote::Any;
    }
    let mut tailscale = false;
    let mut loopback = false;
    for token in tokens {
        if token == "tailscale" {
            tailscale = true;
            continue;
        }
        let Ok(ip) = token.parse::<IpAddr>() else { return Remote::Any };
        let ip = ip.to_canonical();
        if ip.is_unspecified() {
            return Remote::Any;
        } else if ip.is_loopback() {
            loopback = true;
        } else if crate::tailscale::is_tailscale_ip(ip) {
            tailscale = true;
        } else {
            return Remote::Any;
        }
    }
    if tailscale { Remote::Tailscale } else if loopback { Remote::Loopback } else { Remote::Any }
}

/// `None` means there is no Glass allow. Windows may store a prefix, a mask, or a range.
pub fn stored_matches(remote: Remote, stored: Option<&str>) -> bool {
    match remote {
        Remote::Loopback => stored.is_none(),
        Remote::Any => stored.is_some_and(is_any),
        Remote::Tailscale => stored.is_some_and(|s| normalized_remotes(s) == tailscale_set()),
    }
}

/// The stored list, each part written as Glass writes it, sorted.
pub fn normalized_remotes(stored: &str) -> Vec<String> {
    let mut got: Vec<String> = stored.split(',').map(canonical_part).filter(|s| !s.is_empty()).collect();
    got.sort();
    got.dedup();
    got
}

fn tailscale_set() -> Vec<String> {
    let mut want = vec![TAILSCALE_V4.to_string(), TAILSCALE_V6.to_string()];
    want.sort();
    want
}

fn is_any(stored: &str) -> bool {
    let t = stored.trim();
    t.is_empty() || t.eq_ignore_ascii_case("*") || t.eq_ignore_ascii_case("any")
}

fn canonical_part(part: &str) -> String {
    let p = part.trim().to_ascii_lowercase();
    if matches!(p.as_str(), "100.64.0.0/10" | "100.64.0.0/255.192.0.0" | "100.64.0.0-100.127.255.255") {
        return TAILSCALE_V4.to_string();
    }
    if let Some((addr, "48")) = p.split_once('/')
        && let Ok(IpAddr::V6(ip)) = addr.parse()
        && ip.segments()[..3] == [0xfd7a, 0x115c, 0xa1e0]
        && ip.segments()[3..].iter().all(|s| *s == 0)
    {
        return TAILSCALE_V6.to_string();
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bind_maps_to_the_firewall_addresses() {
        assert_eq!(remote_for_bind::<&str>(&[]), Ok(Remote::Any));
        assert_eq!(remote_for_bind(&["0.0.0.0"]), Ok(Remote::Any));
        assert_eq!(remote_for_bind(&["::"]), Ok(Remote::Any));
        assert_eq!(remote_for_bind(&["192.168.1.5"]), Ok(Remote::Any));
        assert_eq!(remote_for_bind(&["100.128.0.1"]), Ok(Remote::Any));
        assert_eq!(remote_for_bind(&["tailscale", "10.0.0.4"]), Ok(Remote::Any));
        assert_eq!(remote_for_bind(&["tailscale"]), Ok(Remote::Tailscale));
        assert_eq!(remote_for_bind(&["Tailscale"]), Ok(Remote::Tailscale));
        assert_eq!(remote_for_bind(&["tailscale,127.0.0.1"]), Ok(Remote::Tailscale));
        assert_eq!(remote_for_bind(&["100.64.0.7", "127.0.0.1"]), Ok(Remote::Tailscale));
        assert_eq!(remote_for_bind(&["::ffff:100.64.0.7"]), Ok(Remote::Tailscale));
        assert_eq!(remote_for_bind(&["fd7a:115c:a1e0::1"]), Ok(Remote::Tailscale));
        assert_eq!(remote_for_bind(&["127.0.0.1"]), Ok(Remote::Loopback));
        assert_eq!(remote_for_bind(&["127.0.0.1", "::1"]), Ok(Remote::Loopback));
        assert_eq!(remote_for_bind(&["not an address"]), Err(()));
        assert_eq!(remote_for_bind(&["127.0.0.1:8080"]), Err(()));
        let both = tailscale_set();
        assert_eq!(normalized_remotes("fd7a:115c:a1e0::/48,100.64.0.0/10"), both);
        assert_eq!(normalized_remotes("100.64.0.0/255.192.0.0, FD7A:115C:A1E0::/48"), both);
        assert_eq!(normalized_remotes("100.64.0.0-100.127.255.255,fd7a:115c:a1e0:0:0:0:0:0/48"), both);
        assert!(stored_matches(Remote::Any, Some("*")));
        assert!(stored_matches(Remote::Any, Some("")));
        assert!(stored_matches(Remote::Loopback, None));
        assert!(!stored_matches(Remote::Tailscale, Some("*")));
        assert!(!stored_matches(Remote::Tailscale, Some("100.64.0.0/10")));
        assert!(!stored_matches(Remote::Loopback, Some("*")));
    }
}
