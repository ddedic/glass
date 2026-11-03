//! Host settings the owner changes with `glass config`, in `settings.json` next to the
//! token. Read fresh where they matter, so changes apply to the running host at once.

use std::path::PathBuf;

use anyhow::Result;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Settings {
    /// Ask the person at the computer before any guest (magic link) connects.
    pub ask_guests: bool,
    /// Ask before anyone connects, the owner's own devices included.
    pub ask_everyone: bool,
    /// Let anyone on this computer's tailnet in without the token (as a guest), not just
    /// the owner's own devices and the people the access rules name.
    pub tailnet_members: bool,
    /// Record every session to a video file in `recordings_dir`.
    pub record: bool,
    /// Where recordings go (default ~/Movies/Glass or ~/Videos/Glass).
    pub recordings_dir: Option<PathBuf>,
    /// Blank this computer's screen and block its keyboard and mouse while someone is
    /// connected (privacy mode).
    pub privacy: bool,
    /// Prefer HEVC over H.264 when every viewer can decode it.
    pub hevc: bool,
    /// Stay awake with the lid closed while on power, so a closed MacBook can still be
    /// unlocked and used from a viewer (see `lid`).
    pub lid_awake: bool,
    /// Local ports on this computer (or reachable from it) a signed-in owner device may
    /// open through Glass, e.g. [3000, 8888]. Empty: port forwarding off.
    pub forward_ports: Vec<u16>,
    /// Addresses to listen on (IPs, or `tailscale`). Empty: `0.0.0.0` for TCP, and
    /// WebTransport's dual-stack default. Applied the next time the host starts.
    pub bind: Vec<String>,
    /// Name shown in the viewer instead of the computer's (branding).
    pub name: Option<String>,
    /// Accent colour for the viewer, as #rrggbb (branding).
    pub accent: Option<String>,
    /// A self-hosted introducer for remote access (site/selfhost), instead of glassdesk.dev.
    pub relay: Option<String>,
    /// Single sign-on (OpenID Connect): the provider, e.g. https://accounts.google.com.
    pub oidc_issuer: Option<String>,
    pub oidc_client_id: Option<String>,
    /// Who may sign in: emails, or `@example.com` for a whole domain. Empty: nobody.
    pub oidc_allow: Vec<String>,
    /// What SSO users may do: full control, or view only.
    pub oidc_view_only: bool,
    /// Device posture: tailnet devices must carry one of these Tailscale tags (e.g.
    /// tag:trusted), unless they're the owner's own. Empty: no tag check.
    pub posture_tailnet_tags: Vec<String>,
    /// Device posture: allowed operating systems (macos, ios, windows, linux, android,
    /// chromeos). Empty: any.
    pub posture_os: Vec<String>,
    /// Device posture: oldest browser major version let in (Chrome, Safari, Firefox,
    /// Edge). 0: any.
    pub posture_min_browser: u32,
    /// Device posture: passkey links must use user verification (Face ID, Touch ID, a PIN).
    pub posture_passkey_uv: bool,
    /// Directory sign-in (LDAP): the server, `ldaps://…` or `ldap://…` (with StartTLS).
    pub ldap_url: Option<String>,
    /// Bind DN for a user, with `{user}` for the name typed, e.g.
    /// `uid={user},ou=people,dc=example,dc=com`. Without it: search under `ldap_base_dn`.
    pub ldap_user_dn: Option<String>,
    pub ldap_base_dn: Option<String>,
    /// Search filter for the user, with `{user}`; default `(|(uid={user})(mail={user}))`.
    pub ldap_user_filter: Option<String>,
    /// Account to search with (its password is `ldap-bind-password`); none: anonymous.
    pub ldap_bind_dn: Option<String>,
    /// Attribute listing the user's groups; default `memberOf`.
    pub ldap_group_attr: Option<String>,
}

/// The OIDC client secret lives in its own owner-only file, not in settings.json.
pub fn oidc_secret_path() -> PathBuf {
    crate::config_dir().join("oidc-secret")
}

pub fn oidc_secret() -> Option<String> {
    std::fs::read_to_string(oidc_secret_path())
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn save_oidc_secret(secret: &str) -> Result<()> {
    save_secret(&oidc_secret_path(), secret)
}

/// The LDAP search account's password (owner-only file).
pub fn ldap_bind_password_path() -> PathBuf {
    crate::config_dir().join("ldap-bind-password")
}

pub fn ldap_bind_password() -> Option<String> {
    read_secret(&ldap_bind_password_path())
}

/// The bearer token SCIM provisioning (an identity provider) must present.
pub fn scim_token_path() -> PathBuf {
    crate::config_dir().join("scim-token")
}

pub fn scim_token() -> Option<String> {
    read_secret(&scim_token_path())
}

fn read_secret(p: &std::path::Path) -> Option<String> {
    std::fs::read_to_string(p)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Writes an owner-only secret file; an empty value removes it.
fn save_secret(p: &std::path::Path, secret: &str) -> Result<()> {
    if secret.is_empty() {
        let _ = std::fs::remove_file(p);
        return Ok(());
    }
    write_private(p, secret.as_bytes())
}

/// Writes an owner-only file (mode 600), replacing it whole.
pub fn write_private(p: &std::path::Path, bytes: &[u8]) -> Result<()> {
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = p.with_extension("tmp");
    let _ = std::fs::remove_file(&tmp);
    use std::io::Write;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
    opts.open(&tmp)?.write_all(bytes)?;
    std::fs::rename(&tmp, p)?;
    Ok(())
}

pub fn path() -> PathBuf {
    crate::config_dir().join("settings.json")
}

/// The settings in force: the owner's, with any policies (`policy`) over them.
pub fn load() -> Settings {
    with_policy(load_file(), &crate::policy::load())
}

fn with_policy(mut s: Settings, policy: &crate::policy::Policy) -> Settings {
    for (key, value) in policy.settings() {
        let _ = s.assign(key, value);
    }
    s
}

/// What to store: `s`, but with the owner's own values (`file`) for what `policy` sets.
fn without_policy(mut s: Settings, file: &Settings, policy: &crate::policy::Policy) -> Result<Settings> {
    for (key, _) in policy.settings() {
        s.assign(key, &file.get(key)?)?;
    }
    Ok(s)
}

/// The owner's settings, as stored.
fn load_file() -> Settings {
    std::fs::read(path())
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

/// Stores `s`, keeping the owner's own value of anything a policy sets, for when the
/// policy goes.
pub fn save(s: &Settings) -> Result<()> {
    let policy = crate::policy::load();
    let s = if policy.is_empty() { s.clone() } else { without_policy(s.clone(), &load_file(), &policy)? };
    let p = path();
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = p.with_extension("tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(&s)?)?;
    std::fs::rename(&tmp, &p)?;
    Ok(())
}

/// The keys `glass config` knows, with what they mean.
pub const KEYS: &[(&str, &str)] = &[
    ("ask-guests", "on/off: ask before a guest (magic link) connects"),
    ("ask-everyone", "on/off: ask before anyone connects"),
    ("tailnet-members", "on/off: anyone on this tailnet connects without the token, as a guest"),
    ("record", "on/off: record every session"),
    ("recordings-dir", "folder for recordings"),
    ("privacy", "on/off: blank this screen and block local input while viewed"),
    ("hevc", "on/off: use HEVC when every viewer supports it"),
    (
        "lid-awake",
        if cfg!(windows) {
            "on/off: stay awake with the lid closed while on power (laptops: closing the lid does nothing while plugged in)"
        } else {
            "on/off: stay awake with the lid closed while on power (laptops; on a Mac, asks for your password once)"
        },
    ),
    ("agents", "on/off: AI agents on other devices may ask to use this computer (glass agents)"),
    ("forward-ports", "comma-separated ports owner devices may open, e.g. 3000,8888"),
    (
        "bind",
        if cfg!(windows) {
            "listen addresses, comma-separated (or tailscale); default 0.0.0.0; applies on restart. On Windows, `glass setup` updates the firewall rule to match"
        } else {
            "listen addresses, comma-separated (or tailscale); default 0.0.0.0; on macOS, port 443 needs the default or 0.0.0.0; applies on restart"
        },
    ),
    ("name", "name shown in the viewer"),
    ("accent", "viewer accent colour, e.g. #ff8800"),
    ("relay", "self-hosted remote-access server, e.g. https://glass.example.com"),
    ("oidc-issuer", "single sign-on provider, e.g. https://accounts.google.com"),
    ("oidc-client-id", "the OAuth client id registered with that provider"),
    ("oidc-client-secret", "its client secret (stored separately, owner-only)"),
    ("oidc-allow", "who may sign in: emails or @domain, comma-separated"),
    ("posture-tailnet-tags", "tailnet devices must carry one of these tags, e.g. tag:trusted"),
    ("posture-os", "allowed operating systems, e.g. macos,ios,windows"),
    ("posture-min-browser", "oldest browser major version let in, e.g. 120 (0: any)"),
    ("posture-require-passkey-uv", "on/off: passkey links need Face ID, Touch ID or a PIN"),
    ("oidc-scope", "full or view-only: what SSO users may do (glass acl for more)"),
    ("ldap-url", "directory sign-in server, ldaps://… or ldap://… (StartTLS)"),
    ("ldap-user-dn", "bind DN with {user}, e.g. uid={user},ou=people,dc=example,dc=com"),
    ("ldap-base-dn", "or: where to search for the user, e.g. dc=example,dc=com"),
    ("ldap-user-filter", "search filter with {user}, default (|(uid={user})(mail={user}))"),
    ("ldap-bind-dn", "account to search with (default anonymous)"),
    ("ldap-bind-password", "its password (stored separately, owner-only)"),
    ("ldap-group-attr", "attribute listing a user's groups (default memberOf)"),
    ("scim-token", "bearer token for SCIM provisioning; new makes one"),
];

/// A relay must be HTTPS (browsers need a secure page for WebRTC and passkeys), except
/// on localhost for testing. Returned without a trailing slash.
pub fn validate_relay(url: &str) -> Result<String> {
    let url = url.trim().trim_end_matches('/');
    let rest = url
        .strip_prefix("https://")
        .or_else(|| {
            url.strip_prefix("http://")
                .filter(|r| r.starts_with("localhost") || r.starts_with("127.0.0.1"))
        })
        .ok_or_else(|| anyhow::anyhow!("expected an https:// URL (or http://localhost)"))?;
    let host = rest.split(['/', ':']).next().unwrap_or("");
    anyhow::ensure!(
        !host.is_empty()
            && host
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b".-".contains(&b))
            && !rest.contains(['?', '#', '@', ' ']),
        "expected a URL like https://glass.example.com"
    );
    Ok(url.to_string())
}

fn on_off(v: &str) -> Result<bool> {
    match v.to_ascii_lowercase().as_str() {
        "on" | "true" | "yes" | "1" => Ok(true),
        "off" | "false" | "no" | "0" => Ok(false),
        _ => anyhow::bail!("expected on or off"),
    }
}

/// "a, b,c" → ["a", "b", "c"], lowercased.
fn list(value: &str) -> Vec<String> {
    value
        .split([',', ' '])
        .map(|v| v.trim().to_ascii_lowercase())
        .filter(|v| !v.is_empty())
        .collect()
}

fn show_bool(b: bool) -> String {
    if b { "on" } else { "off" }.into()
}

impl Settings {
    pub fn get(&self, key: &str) -> Result<String> {
        Ok(match key {
            "ask-guests" => show_bool(self.ask_guests),
            "ask-everyone" => show_bool(self.ask_everyone),
            "tailnet-members" => show_bool(self.tailnet_members),
            "record" => show_bool(self.record),
            "recordings-dir" => recordings_dir(self).display().to_string(),
            "privacy" => show_bool(self.privacy),
            "hevc" => show_bool(self.hevc),
            "lid-awake" => show_bool(self.lid_awake),
            // Kept with the agents' grants, not here.
            "agents" => show_bool(crate::grants::load().enabled),
            "forward-ports" => self
                .forward_ports
                .iter()
                .map(|p| p.to_string())
                .collect::<Vec<_>>()
                .join(","),
            "bind" => {
                if self.bind.is_empty() {
                    "0.0.0.0".into()
                } else {
                    self.bind.join(",")
                }
            }
            "name" => self.name.clone().unwrap_or_default(),
            "accent" => self.accent.clone().unwrap_or_default(),
            "relay" => self.relay.clone().unwrap_or_default(),
            "oidc-issuer" => self.oidc_issuer.clone().unwrap_or_default(),
            "oidc-client-id" => self.oidc_client_id.clone().unwrap_or_default(),
            "oidc-client-secret" => if oidc_secret().is_some() { "(set)" } else { "" }.into(),
            "oidc-allow" => self.oidc_allow.join(","),
            "oidc-scope" => if self.oidc_view_only { "view-only" } else { "full" }.into(),
            "posture-tailnet-tags" => self.posture_tailnet_tags.join(","),
            "posture-os" => self.posture_os.join(","),
            "posture-min-browser" => self.posture_min_browser.to_string(),
            "posture-require-passkey-uv" => show_bool(self.posture_passkey_uv),
            "ldap-url" => self.ldap_url.clone().unwrap_or_default(),
            "ldap-user-dn" => self.ldap_user_dn.clone().unwrap_or_default(),
            "ldap-base-dn" => self.ldap_base_dn.clone().unwrap_or_default(),
            "ldap-user-filter" => self.ldap_user_filter.clone().unwrap_or_default(),
            "ldap-bind-dn" => self.ldap_bind_dn.clone().unwrap_or_default(),
            "ldap-bind-password" => if ldap_bind_password().is_some() { "(set)" } else { "" }.into(),
            "ldap-group-attr" => self.ldap_group_attr.clone().unwrap_or_default(),
            "scim-token" => if scim_token().is_some() { "(set)" } else { "" }.into(),
            _ => anyhow::bail!("unknown setting {key}; see glass config"),
        })
    }

    /// Sets `key` from its text form; an empty value resets optional ones. A setting a
    /// policy manages can't be changed.
    pub fn set(&mut self, key: &str, value: &str) -> Result<()> {
        crate::policy::load().check(key, value)?;
        self.assign(key, value)
    }

    fn assign(&mut self, key: &str, value: &str) -> Result<()> {
        let value = value.trim();
        match key {
            "ask-guests" => self.ask_guests = on_off(value)?,
            "ask-everyone" => self.ask_everyone = on_off(value)?,
            "tailnet-members" => self.tailnet_members = on_off(value)?,
            "record" => self.record = on_off(value)?,
            "recordings-dir" => {
                self.recordings_dir = (!value.is_empty()).then(|| PathBuf::from(value))
            }
            "privacy" => self.privacy = on_off(value)?,
            "hevc" => self.hevc = on_off(value)?,
            "lid-awake" => {
                let on = on_off(value)?;
                if on && !self.lid_awake {
                    crate::lid::allow()?;
                } else if !on && self.lid_awake {
                    crate::lid::release()?;
                }
                self.lid_awake = on;
            }
            "agents" => crate::grants::set_enabled(on_off(value)?)?,
            "forward-ports" => {
                self.forward_ports = value
                    .split([',', ' '])
                    .filter(|p| !p.is_empty())
                    .map(|p| {
                        p.parse::<u16>()
                            .ok()
                            .filter(|&n| n != 0)
                            .ok_or_else(|| anyhow::anyhow!("{p} isn't a port"))
                    })
                    .collect::<Result<_>>()?
            }
            "bind" => {
                if value.is_empty() {
                    self.bind.clear();
                } else {
                    let parsed = crate::listen::parse_bind([value])?;
                    anyhow::ensure!(!parsed.is_empty(), "expected an IP address or tailscale");
                    self.bind = parsed;
                }
            }
            "name" => {
                let name: String = value.chars().filter(|c| !c.is_control()).take(40).collect();
                self.name = (!name.is_empty()).then_some(name);
            }
            "accent" => {
                anyhow::ensure!(
                    value.is_empty()
                        || (value.len() == 7
                            && value.starts_with('#')
                            && value[1..].bytes().all(|b| b.is_ascii_hexdigit())),
                    "expected a colour like #ff8800"
                );
                self.accent = (!value.is_empty()).then(|| value.to_ascii_lowercase());
            }
            "relay" => {
                self.relay = if value.is_empty() {
                    None
                } else {
                    Some(validate_relay(value)?)
                }
            }
            "oidc-issuer" => {
                self.oidc_issuer = if value.is_empty() {
                    None
                } else {
                    Some(validate_relay(value)?)
                }
            }
            "oidc-client-id" => {
                anyhow::ensure!(
                    value.len() <= 512 && !value.chars().any(|c| c.is_control() || c == ' '),
                    "that doesn't look like a client id"
                );
                self.oidc_client_id = (!value.is_empty()).then(|| value.to_string());
            }
            "oidc-client-secret" => save_oidc_secret(value)?,
            "oidc-allow" => {
                self.oidc_allow = value
                    .split([',', ' '])
                    .filter(|e| !e.is_empty())
                    .map(|e| {
                        let e = e.to_ascii_lowercase();
                        let ok = match e.strip_prefix('@') {
                            Some(domain) => domain.contains('.') && !domain.contains('@'),
                            None => e.split_once('@').is_some_and(|(u, d)| !u.is_empty() && d.contains('.')),
                        };
                        anyhow::ensure!(ok, "{e} isn't an email or @domain");
                        Ok(e)
                    })
                    .collect::<Result<_>>()?
            }
            "oidc-scope" => {
                self.oidc_view_only = match value {
                    "full" | "" => false,
                    "view-only" => true,
                    _ => anyhow::bail!("expected full or view-only"),
                }
            }
            "posture-tailnet-tags" => {
                self.posture_tailnet_tags = list(value)
                    .into_iter()
                    .map(|t| if t.starts_with("tag:") { t } else { format!("tag:{t}") })
                    .collect()
            }
            "posture-os" => {
                let os = list(value);
                for o in &os {
                    anyhow::ensure!(
                        crate::posture::OS_NAMES.contains(&o.as_str()),
                        "unknown OS {o}; use {}",
                        crate::posture::OS_NAMES.join(", ")
                    );
                }
                self.posture_os = os;
            }
            "posture-min-browser" => {
                self.posture_min_browser = if value.is_empty() {
                    0
                } else {
                    value.parse().map_err(|_| anyhow::anyhow!("expected a version number like 120"))?
                }
            }
            "posture-require-passkey-uv" => self.posture_passkey_uv = on_off(value)?,
            "ldap-url" => {
                anyhow::ensure!(
                    value.is_empty()
                        || ((value.starts_with("ldaps://") || value.starts_with("ldap://"))
                            && !value.contains(' ')),
                    "expected ldaps://host or ldap://host"
                );
                self.ldap_url = (!value.is_empty()).then(|| value.trim_end_matches('/').to_string());
            }
            "ldap-user-dn" => {
                anyhow::ensure!(
                    value.is_empty() || value.contains("{user}"),
                    "the DN needs {{user}} where the name goes"
                );
                self.ldap_user_dn = (!value.is_empty()).then(|| value.to_string());
            }
            "ldap-base-dn" => self.ldap_base_dn = (!value.is_empty()).then(|| value.to_string()),
            "ldap-user-filter" => {
                anyhow::ensure!(
                    value.is_empty() || (value.starts_with('(') && value.contains("{user}")),
                    "expected a filter like (uid={{user}})"
                );
                self.ldap_user_filter = (!value.is_empty()).then(|| value.to_string());
            }
            "ldap-bind-dn" => self.ldap_bind_dn = (!value.is_empty()).then(|| value.to_string()),
            "ldap-bind-password" => save_secret(&ldap_bind_password_path(), value)?,
            "ldap-group-attr" => {
                anyhow::ensure!(
                    value.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'),
                    "expected an attribute name like memberOf"
                );
                self.ldap_group_attr = (!value.is_empty()).then(|| value.to_string());
            }
            "scim-token" => {
                let token = if value == "new" {
                    let mut bytes = [0u8; 24];
                    crate::control::getrandom(&mut bytes)?;
                    bytes.iter().map(|b| format!("{b:02x}")).collect()
                } else {
                    anyhow::ensure!(
                        value.is_empty() || (value.len() >= 16 && !value.contains(' ')),
                        "use at least 16 characters, or new"
                    );
                    value.to_string()
                };
                save_secret(&scim_token_path(), &token)?;
            }
            _ => anyhow::bail!("unknown setting {key}; see glass config"),
        }
        Ok(())
    }
}

pub fn recordings_dir(s: &Settings) -> PathBuf {
    s.recordings_dir.clone().unwrap_or_else(|| {
        let movies = if cfg!(target_os = "macos") {
            "Movies"
        } else {
            "Videos"
        };
        crate::home().join(movies).join("Glass")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_and_get_round_trip() {
        let mut s = Settings::default();
        s.set("ask-guests", "on").unwrap();
        s.set("forward-ports", "3000, 8888").unwrap();
        s.set("accent", "#FF8800").unwrap();
        s.set("name", "Studio Mac").unwrap();
        assert!(s.ask_guests);
        assert_eq!(s.forward_ports, vec![3000, 8888]);
        assert_eq!(s.get("forward-ports").unwrap(), "3000,8888");
        assert_eq!(s.get("accent").unwrap(), "#ff8800");
        assert!(s.set("accent", "orange").is_err());
        assert!(s.set("forward-ports", "0").is_err());
        assert!(s.set("record", "maybe").is_err());
        assert!(s.set("nope", "on").is_err());
        s.set("name", "").unwrap();
        assert_eq!(s.name, None);
        assert_eq!(s.get("bind").unwrap(), "0.0.0.0");
        s.set("bind", "Tailscale, 127.0.0.1").unwrap();
        assert_eq!(s.bind, vec!["tailscale".to_string(), "127.0.0.1".into()]);
        assert_eq!(s.get("bind").unwrap(), "tailscale,127.0.0.1");
        assert!(s.set("bind", "nope").is_err());
        assert!(s.set("bind", "127.0.0.1:8080").is_err());
        s.set("bind", "").unwrap();
        assert!(s.bind.is_empty());
        // Unknown and missing fields are fine.
        let old: Settings = serde_json::from_str(r#"{"ask_guests":true,"future":1}"#).unwrap();
        assert!(old.ask_guests && !old.record);
    }

    #[test]
    fn policies_show_over_the_owners_settings_but_are_not_stored() {
        use crate::policy::{Policy, Raw};
        let machine = |n: &str| match n {
            "AskGuests" => Some(Raw::Number(1)),
            "Relay" => Some(Raw::Text("https://glass.example.com".into())),
            _ => None,
        };
        let policy = Policy::merge(&[&machine]);
        let file = Settings { name: Some("Studio".into()), ..Default::default() };
        let mut shown = with_policy(file.clone(), &policy);
        assert!(shown.ask_guests);
        assert_eq!(shown.relay.as_deref(), Some("https://glass.example.com"));
        // The owner changes something else; the policy's values aren't written.
        shown.assign("privacy", "on").unwrap();
        let stored = without_policy(shown, &file, &policy).unwrap();
        assert!(stored.privacy && !stored.ask_guests && stored.relay.is_none());
        assert_eq!(stored.name.as_deref(), Some("Studio"));
    }

    #[test]
    fn relay_urls() {
        assert_eq!(
            validate_relay("https://glass.example.com/").unwrap(),
            "https://glass.example.com"
        );
        assert_eq!(
            validate_relay("http://localhost:8787").unwrap(),
            "http://localhost:8787"
        );
        assert!(validate_relay("http://glass.example.com").is_err(), "plain http");
        assert!(validate_relay("ftp://x").is_err());
        assert!(validate_relay("https://").is_err());
        assert!(validate_relay("https://a b.com").is_err());
        assert!(validate_relay("https://user@evil.com").is_err());
        let mut s = Settings::default();
        s.set("relay", "https://glass.example.com").unwrap();
        assert_eq!(s.get("relay").unwrap(), "https://glass.example.com");
        s.set("relay", "").unwrap();
        assert_eq!(s.relay, None);
    }
}
