//! Device posture: what a device must be before it may connect, checked after it signs in
//! and before it sees anything. Set with `glass config posture-*`:
//!
//! - tailnet tags: a tailnet device that isn't the owner's own must carry one of them;
//! - operating systems: from Tailscale for tailnet devices, otherwise as the browser
//!   reports it (which a determined user can fake, so it says so);
//! - oldest browser version, from the browser's user agent;
//! - passkeys with user verification (enforced in passkey.rs).

#[cfg(test)]
use serde_json::Value;

/// OS names posture rules use.
pub const OS_NAMES: &[&str] = &["macos", "ios", "windows", "linux", "android", "chromeos"];

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Policy {
    pub tailnet_tags: Vec<String>,
    pub os: Vec<String>,
    pub min_browser: u32,
}

impl Policy {
    pub fn from_settings(s: &crate::settings::Settings) -> Self {
        Self {
            tailnet_tags: s.posture_tailnet_tags.clone(),
            os: s.posture_os.clone(),
            min_browser: s.posture_min_browser,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.tailnet_tags.is_empty() && self.os.is_empty() && self.min_browser == 0
    }
}

/// What's known about the device.
#[derive(Debug, Clone, Default)]
pub struct Facts {
    /// From `tailscale whois`, for tailnet devices.
    pub tailnet: Option<Tailnet>,
    /// The browser's user agent.
    pub ua: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct Tailnet {
    /// Owned by the computer's owner (their own devices skip the tag rule).
    pub owner: bool,
    pub tags: Vec<String>,
    /// Tailscale's OS name, lowercased (macos, ios, windows, linux, android).
    pub os: Option<String>,
}

/// Ok, or why the device is turned away (shown to it and logged).
pub fn evaluate(policy: &Policy, facts: &Facts) -> Result<(), String> {
    if !policy.tailnet_tags.is_empty()
        && let Some(t) = &facts.tailnet
        && !t.owner
        && !t.tags.iter().any(|tag| policy.tailnet_tags.contains(tag))
    {
        return Err(format!(
            "this computer only lets in tailnet devices tagged {}",
            policy.tailnet_tags.join(" or ")
        ));
    }
    if !policy.os.is_empty() {
        let (os, source) = match facts.tailnet.as_ref().and_then(|t| t.os.clone()) {
            Some(os) => (Some(os), "according to Tailscale"),
            None => (facts.ua.as_deref().and_then(os_from_ua).map(String::from), "reported by the browser"),
        };
        match os {
            Some(os) if policy.os.contains(&os) => {}
            Some(os) => {
                return Err(format!(
                    "this computer doesn't let in {os} devices ({source}); allowed: {}",
                    policy.os.join(", ")
                ));
            }
            None => return Err("this computer couldn't tell which system this device runs".into()),
        }
    }
    if policy.min_browser > 0 {
        match facts.ua.as_deref().and_then(browser_from_ua) {
            Some((_, v)) if v >= policy.min_browser => {}
            Some((name, v)) => {
                return Err(format!(
                    "this computer needs version {} or newer of your browser ({name} {v}); update it and try again",
                    policy.min_browser
                ));
            }
            None => return Err("this computer couldn't tell which browser version this is".into()),
        }
    }
    Ok(())
}

/// The OS a user agent claims, as an `OS_NAMES` entry.
pub fn os_from_ua(ua: &str) -> Option<&'static str> {
    Some(if ua.contains("iPhone") || ua.contains("iPad") {
        "ios"
    } else if ua.contains("Android") {
        "android"
    } else if ua.contains("CrOS") {
        "chromeos"
    } else if ua.contains("Windows") {
        "windows"
    } else if ua.contains("Macintosh") || ua.contains("Mac OS X") {
        "macos"
    } else if ua.contains("Linux") || ua.contains("X11") {
        "linux"
    } else {
        return None;
    })
}

/// (browser, major version) from a user agent: Edge, Firefox, Chrome or Safari.
pub fn browser_from_ua(ua: &str) -> Option<(&'static str, u32)> {
    let major = |marker: &str| -> Option<u32> {
        let rest = &ua[ua.find(marker)? + marker.len()..];
        rest.split(|c: char| !c.is_ascii_digit()).next()?.parse().ok()
    };
    for (marker, name) in [
        ("Edg/", "Edge"),
        ("EdgiOS/", "Edge"),
        ("Firefox/", "Firefox"),
        ("FxiOS/", "Firefox"),
        ("CriOS/", "Chrome"),
        ("Chrome/", "Chrome"),
    ] {
        if let Some(v) = major(marker) {
            return Some((name, v));
        }
    }
    if ua.contains("Safari/") {
        return major("Version/").map(|v| ("Safari", v));
    }
    None
}

/// Facts about a tailnet peer from `tailscale whois` output.
#[cfg(test)]
fn tailnet_facts(whois: &Value, owner: Option<&str>) -> Tailnet {
    Tailnet {
        owner: owner.is_some_and(|o| whois["UserProfile"]["LoginName"].as_str() == Some(o)),
        tags: whois["Node"]["Tags"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|t| t.as_str().map(String::from))
            .collect(),
        os: whois["Node"]["Hostinfo"]["OS"]
            .as_str()
            .map(|o| o.to_ascii_lowercase()),
    }
}

/// Checks a connecting device against the current settings.
pub fn check(
    _peer: std::net::IpAddr,
    relayed: bool,
    ua: Option<&str>,
    tailnet: Option<Tailnet>,
) -> Result<(), String> {
    let policy = Policy::from_settings(&crate::settings::load());
    if policy.is_empty() {
        return Ok(());
    }
    // Over WebRTC the address is public, never a tailnet device's. `tailnet` is the
    // same status snapshot the sign-in used.
    let tailnet = if relayed || crate::testmode::enabled() { None } else { tailnet };
    evaluate(
        &policy,
        &Facts {
            tailnet,
            ua: ua.map(String::from),
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    const SAFARI_IPAD: &str = "Mozilla/5.0 (iPad; CPU OS 18_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.0 Mobile/15E148 Safari/604.1";
    const CHROME_WIN: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36";
    const EDGE_MAC: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36 Edg/141.0.0.0";
    const FIREFOX_LINUX: &str = "Mozilla/5.0 (X11; Linux x86_64; rv:140.0) Gecko/20100101 Firefox/140.0";

    #[test]
    fn reads_user_agents() {
        assert_eq!(browser_from_ua(SAFARI_IPAD), Some(("Safari", 18)));
        assert_eq!(browser_from_ua(CHROME_WIN), Some(("Chrome", 141)));
        assert_eq!(browser_from_ua(EDGE_MAC), Some(("Edge", 141)));
        assert_eq!(browser_from_ua(FIREFOX_LINUX), Some(("Firefox", 140)));
        assert_eq!(os_from_ua(SAFARI_IPAD), Some("ios"));
        assert_eq!(os_from_ua(CHROME_WIN), Some("windows"));
        assert_eq!(os_from_ua(EDGE_MAC), Some("macos"));
        assert_eq!(os_from_ua(FIREFOX_LINUX), Some("linux"));
        assert_eq!(browser_from_ua("curl/8"), None);
    }

    #[test]
    fn no_policy_lets_everyone_in() {
        assert!(evaluate(&Policy::default(), &Facts::default()).is_ok());
    }

    #[test]
    fn browser_version() {
        let policy = Policy { min_browser: 140, ..Default::default() };
        let facts = |ua: &str| Facts { tailnet: None, ua: Some(ua.into()) };
        assert!(evaluate(&policy, &facts(CHROME_WIN)).is_ok());
        let err = evaluate(&policy, &facts(SAFARI_IPAD)).unwrap_err();
        assert!(err.contains("Safari 18"), "{err}");
        assert!(evaluate(&policy, &Facts::default()).is_err(), "unknown is refused");
    }

    #[test]
    fn operating_system_prefers_tailscale_over_the_browser() {
        let policy = Policy { os: vec!["macos".into(), "ios".into()], ..Default::default() };
        let browser_only = Facts { tailnet: None, ua: Some(CHROME_WIN.into()) };
        let err = evaluate(&policy, &browser_only).unwrap_err();
        assert!(err.contains("windows") && err.contains("reported by the browser"), "{err}");
        // Tailscale says it's a Mac, whatever the browser claims.
        let tailnet = Facts {
            tailnet: Some(Tailnet { os: Some("macos".into()), ..Default::default() }),
            ua: Some(CHROME_WIN.into()),
        };
        assert!(evaluate(&policy, &tailnet).is_ok());
    }

    #[test]
    fn tailnet_tags() {
        let policy = Policy { tailnet_tags: vec!["tag:trusted".into()], ..Default::default() };
        let device = |owner, tags: &[&str]| Facts {
            tailnet: Some(Tailnet { owner, tags: tags.iter().map(|t| t.to_string()).collect(), os: None }),
            ua: None,
        };
        assert!(evaluate(&policy, &device(true, &[])).is_ok(), "the owner's own");
        assert!(evaluate(&policy, &device(false, &["tag:trusted"])).is_ok());
        assert!(evaluate(&policy, &device(false, &["tag:other"])).is_err());
        // Off the tailnet the tag rule doesn't apply (links, tokens).
        assert!(evaluate(&policy, &Facts::default()).is_ok());
    }

    #[test]
    fn whois_facts() {
        let w: Value = serde_json::from_str(
            r#"{"UserProfile":{"LoginName":"me@x"},"Node":{"Tags":["tag:trusted"],"Hostinfo":{"OS":"iOS"}}}"#,
        )
        .unwrap();
        let t = tailnet_facts(&w, Some("me@x"));
        assert!(t.owner);
        assert_eq!(t.tags, vec!["tag:trusted"]);
        assert_eq!(t.os.as_deref(), Some("ios"));
        assert!(!tailnet_facts(&w, Some("other@x")).owner);
    }
}
